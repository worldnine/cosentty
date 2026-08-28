//! Websocket push sync for live web edits.
//!
//! The real-time channel Cosense uses (scrapbox.io, socket.io v4 over a raw
//! websocket) was scouted live on 2026-08-28 — see `NOTE-websocket-sync.md`
//! for the full protocol. Talking it needs only a websocket: authenticate
//! with the `connect.sid` cookie, join the page room, and listen for
//! `42["commit", …]` frames whose `changes` are the same lineId-based
//! [`EditOp`]s the edit API accepts. `_insert` / `_update` / `_delete`
//! entries map straight onto [`EditOp`]; meta entries (`linesCount`,
//! `charsCount`, …) and unknown events (`cursor`, `infobox:reload`) are
//! ignored. No socket.io client crate is involved.

use crate::api::{Client, EditOp, PageLine};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::protocol::Message;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{ClientRequestBuilder, WebSocket};

/// How often the ws thread wakes while idle: navigation is noticed here,
/// and kernel read timeouts surface as ticks. The server pings every 25 s,
/// so this is far shorter than any real traffic gap.
const READ_TICK: Duration = Duration::from_secs(2);
/// No traffic at all for this long means the socket is dead even though no
/// error surfaced (server pings every 25 s — 60 s of silence is fatal).
const SILENCE_LIMIT: Duration = Duration::from_secs(60);
/// Reconnect backoff: 1s → 2s → 4s … capped here.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Periodic full-page resync while connected (the "insurance poll": unknown
/// changes — renames, meta-only commits — are picked up here).
const PERIODIC_SYNC: Duration = Duration::from_secs(60);
/// How long to wait for the `430[…]` join ack.
const JOIN_ACK_TIMEOUT: Duration = Duration::from_secs(15);
/// Status lines are throttled to one per this interval.
const STATUS_THROTTLE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Packet layer (pure, unit-tested)
// ---------------------------------------------------------------------------

/// One engine.io / socket.io frame after the websocket text is split off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoPacket {
    /// engine.io open: `0{"sid":…,"pingInterval":…,"pingTimeout":…}`.
    Open { ping_interval: u64, ping_timeout: u64 },
    /// engine.io ping (`2`) — reply with a pong (`3`).
    Ping,
    /// engine.io pong (`3`).
    Pong,
    /// engine.io close (`1`).
    Close,
    /// socket.io namespace connect (`40` — our own, echo of the ack `40{…}`).
    Connect,
    /// socket.io namespace connect ack with payload (`40{…}`).
    Connected(serde_json::Value),
    /// socket.io disconnect (`41`).
    Disconnect,
    /// `42[name, data]` (a trailing packet id between `42` and `[` is
    /// accepted and dropped — the server's join ack uses one).
    Event { name: String, data: serde_json::Value },
    /// `43<packet-id>[…]` — acknowledgement (the join reply).
    Ack(serde_json::Value),
    /// `44{…}` — connect error ("You are not logged in yet." and friends).
    Error(String),
    /// Anything else (ignored, occasionally surfaced as a status line).
    Other(String),
}

/// Classify one websocket text payload.
pub fn parse_packet(text: &str) -> IoPacket {
    if let Some(rest) = text.strip_prefix('0') {
        return parse_open(rest);
    }
    match text {
        "2" => return IoPacket::Ping,
        "3" => return IoPacket::Pong,
        "1" => return IoPacket::Close,
        _ => {}
    }
    let Some(body) = text.strip_prefix('4') else {
        return IoPacket::Other(text.to_string());
    };
    match &body[..1] {
        "0" => {
            let payload = body[1..].trim();
            if payload.is_empty() {
                IoPacket::Connect
            } else {
                serde_json::from_str(payload)
                    .map(IoPacket::Connected)
                    .unwrap_or(IoPacket::Other(text.to_string()))
            }
        }
        "1" => IoPacket::Disconnect,
        "2" => {
            // "42" + [optional packet id] + json array
            let rest = &body[1..];
            let json = match rest.strip_prefix(|c: char| c.is_ascii_digit()) {
                Some(r) => r,
                None => rest,
            };
            match serde_json::from_str::<serde_json::Value>(json) {
                Ok(serde_json::Value::Array(mut a)) if !a.is_empty() => {
                    let name = a
                        .remove(0)
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_default();
                    let data = a.into_iter().next().unwrap_or(serde_json::Value::Null);
                    IoPacket::Event { name, data }
                }
                _ => IoPacket::Other(text.to_string()),
            }
        }
        "3" => {
            // "43" + [packet id digits] + json (the join ack: `430[…]`)
            let rest = &body[1..];
            let json = match rest.strip_prefix(|c: char| c.is_ascii_digit()) {
                Some(r) => r,
                None => rest,
            };
            serde_json::from_str(json)
                .map(IoPacket::Ack)
                .unwrap_or(IoPacket::Other(text.to_string()))
        }
        "4" => IoPacket::Error(body[1..].to_string()),
        _ => IoPacket::Other(text.to_string()),
    }
}

/// `0{…}` open frame → the peer's ping cadence.
fn parse_open(json: &str) -> IoPacket {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
        let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        IoPacket::Open { ping_interval: n("pingInterval"), ping_timeout: n("pingTimeout") }
    } else {
        IoPacket::Other(format!("0{json}"))
    }
}

/// The socket.io packet the client sends to connect its namespace: `40`.
pub fn frame_connect() -> &'static str {
    "40"
}

/// The engine.io pong answering the server's `2`: `3`.
pub fn frame_pong() -> &'static str {
    "3"
}

/// The `420[…]` room:join request (packet id 0, the only outstanding ack).
pub fn frame_join(project_id: &str, page_id: &str) -> String {
    let msg = serde_json::json!([
        "socket.io-request",
        {
            "method": "room:join",
            "data": {
                "projectId": project_id,
                "pageId": page_id,
                "projectUpdatesStream": false,
            },
        },
    ]);
    format!("420{msg}")
}

// ---------------------------------------------------------------------------
// Commit events → EditOps
// ---------------------------------------------------------------------------

/// A parsed `42["commit", …]` event, shaped for the apply gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommit {
    /// Own id of this commit.
    pub commit_id: String,
    /// The commit this one extends (contiguity = our local model kept up).
    pub parent_id: String,
    /// The page the commit is for.
    pub page_id: String,
    /// Who made it — events from `me` are self-echoes and skipped.
    pub user_id: String,
    pub ops: Vec<EditOp>,
}

/// Parse one `42["commit",{…}]` event. Meta-only commits (no line ops) or
/// non-page commits return `None` — there is nothing to apply.
pub fn parse_commit(data: &serde_json::Value) -> Option<RemoteCommit> {
    let commit_id = data.get("id")?.as_str()?.to_string();
    let parent_id = data.get("parentId")?.as_str()?.to_string();
    let page_id = data.get("pageId")?.as_str()?.to_string();
    let user_id = data.get("userId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ops = parse_changes(data.get("changes")?.as_array()?);
    if ops.is_empty() {
        return None;
    }
    Some(RemoteCommit { commit_id, parent_id, page_id, user_id, ops })
}

/// `changes` entries → [`EditOp`]. `_insert`/`_update`/`_delete` map
/// 1:1 (a multi-line `lines` array becomes one insert); everything else —
/// `linesCount`, `charsCount`, a title-change entry, … — is skipped.
pub fn parse_changes(changes: &[serde_json::Value]) -> Vec<EditOp> {
    let mut ops = Vec::new();
    for c in changes {
        if let Some(anchor) = c.get("_insert").and_then(|v| v.as_str()) {
            let lines = insert_lines(c.get("lines"));
            if !lines.is_empty() {
                ops.push(EditOp::Insert { anchor: anchor.to_string(), lines });
            }
        } else if let Some(id) = c.get("_update").and_then(|v| v.as_str()) {
            if let Some(text) = c
                .get("lines")
                .and_then(|l| l.get("text"))
                .and_then(|v| v.as_str())
            {
                ops.push(EditOp::Replace { id: id.to_string(), text: text.to_string() });
            }
        } else if let Some(id) = c.get("_delete").and_then(|v| v.as_str()) {
            ops.push(EditOp::Delete { id: id.to_string() });
        }
    }
    ops
}

/// The `lines` of an insert: the browser sends `{id,text}` for one line and
/// `[{id,text},…]` for a multi-line paste.
fn insert_lines(v: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let Some(v) = v else { return Vec::new() };
    match v {
        serde_json::Value::Object(_) => {
            let id = v.get("id").and_then(|s| s.as_str()).unwrap_or_default();
            if id.is_empty() {
                Vec::new()
            } else {
                let text = v.get("text").and_then(|s| s.as_str()).unwrap_or_default();
                vec![(id.to_string(), text.to_string())]
            }
        }
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|l| {
                let id = l.get("id").and_then(|s| s.as_str())?;
                let text = l.get("text").and_then(|s| s.as_str()).unwrap_or_default();
                if id.is_empty() { None } else { Some((id.to_string(), text.to_string())) }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Apply REMOTE commit ops to the local page model.
///
/// Unlike [`crate::editops::apply_ops`] (which mirrors the server for our
/// OWN edits and may assume exact application), remote ops must be safe to
/// re-apply or apply slightly out of order: an insert whose line id is
/// already present is skipped (a replayed event cannot duplicate a line),
/// and replaces / deletes for ids we do not have are no-ops (a stale event
/// cannot resurrect or clobber anything).
pub fn apply_remote_ops(lines: &mut Vec<PageLine>, ops: &[EditOp]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for op in ops {
        match op {
            EditOp::Insert { anchor, lines: newl } => {
                let at = if anchor == "_end" {
                    lines.len()
                } else {
                    lines.iter().position(|l| l.id == *anchor).unwrap_or(lines.len())
                };
                for (k, (id, text)) in newl.iter().enumerate() {
                    if lines.iter().any(|l| l.id == *id) {
                        continue; // already applied (replay) — never duplicate
                    }
                    lines.insert(
                        at + k,
                        PageLine {
                            id: id.clone(),
                            text: text.clone(),
                            user_id: String::new(),
                            created: now,
                            updated: now,
                        },
                    );
                }
            }
            EditOp::Replace { id, text } => {
                if let Some(l) = lines.iter_mut().find(|l| l.id == *id) {
                    l.text = text.clone();
                    l.updated = now;
                }
            }
            EditOp::Delete { id } => {
                lines.retain(|l| l.id != *id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// One connection (shared by the sync thread and the smoke binary)
// ---------------------------------------------------------------------------

/// A single engine.io + socket.io session on top of one websocket.
pub struct RoomLink {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    last_rx: Instant,
}

/// What a read tick produced.
pub enum Recv {
    /// The parsed frame (pings already answered with a pong).
    Packet(IoPacket),
    /// Nothing arrived inside `READ_TICK` — check navigation / liveness.
    Tick,
    /// The connection is gone; the caller should reconnect.
    Lost,
}

impl RoomLink {
    /// Connect to `api_domain`, speak the engine.io open + namespace
    /// connect (`40`), and verify the server accepted us. `sid` is the
    /// `connect.sid` cookie value — the ONLY credential the server takes
    /// on the websocket (PAT is refused, verified live).
    pub fn connect(api_domain: &str, sid: &str) -> Result<Self, String> {
        let uri: tungstenite::http::Uri = format!("wss://{api_domain}/socket.io/?EIO=4&transport=websocket")
            .parse()
            .map_err(|e| format!("bad ws uri: {e}"))?;
        let request = ClientRequestBuilder::new(uri)
            .with_header("Origin", format!("https://{api_domain}"))
            .with_header("Cookie", format!("connect.sid={sid}"))
            .with_header("User-Agent", "cosense-tui");
        let stream = TcpStream::connect((api_domain, 443)).map_err(|e| format!("tcp: {e}"))?;
        stream.set_nodelay(true).ok();
        stream
            .set_read_timeout(Some(READ_TICK))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        let (mut ws, _) = tungstenite::client_tls_with_config(request, stream, None, None)
            .map_err(|e| format!("ws handshake: {e}"))?;
        // engine.io open
        match RoomLink::read_once(&mut ws) {
            Ok(Some(Message::Text(t))) if t.starts_with('0') => {}
            Ok(Some(Message::Text(t))) => return Err(format!("unexpected open frame: {t}")),
            other => return Err(format!("no engine.io open: {other:?}")),
        }
        // socket.io namespace connect
        ws.send(Message::Text(frame_connect().into()))
            .map_err(|e| format!("send 40: {e}"))?;
        loop {
            match RoomLink::read_once(&mut ws) {
                Ok(Some(Message::Text(t))) => {
                    match parse_packet(t.as_str()) {
                        IoPacket::Connected(_) => break,
                        IoPacket::Error(m) => return Err(format!("namespace refused: {m}")),
                        IoPacket::Ping => {
                            ws.send(Message::Text(frame_pong().into())).ok();
                        }
                        _ => {} // stray frames before the ack
                    }
                }
                Ok(Some(Message::Close(_))) | Err(_) => {
                    return Err("namespace connect lost".into());
                }
                Ok(_) => {}
            }
        }
        Ok(RoomLink { ws, last_rx: Instant::now() })
    }

    /// Join the page room (`420[…]`) and wait for the `430[…]` ack.
    pub fn join(&mut self, project_id: &str, page_id: &str) -> Result<(), String> {
        let frame = frame_join(project_id, page_id);
        self.ws
            .send(Message::Text(frame.into()))
            .map_err(|e| format!("send join: {e}"))?;
        let deadline = Instant::now() + JOIN_ACK_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("join ack timeout".into());
            }
            match RoomLink::read_once(&mut self.ws) {
                Ok(Some(Message::Text(t))) => {
                    self.last_rx = Instant::now();
                    match parse_packet(t.as_str()) {
                        IoPacket::Ack(_) => return Ok(()),
                        IoPacket::Error(m) => return Err(format!("join refused: {m}")),
                        IoPacket::Ping => {
                            self.ws.send(Message::Text(frame_pong().into())).ok();
                        }
                        _ => {} // cursor / peek frames before the ack
                    }
                }
                Ok(Some(Message::Close(_))) | Err(_) => return Err("join lost".into()),
                _ => {} // timeout tick — keep waiting until the deadline
            }
        }
    }

    /// One read: text frames are parsed (pings auto-answered). Timeouts and
    /// empty reads become [`Recv::Tick`], real failures [`Recv::Lost`].
    pub fn recv(&mut self) -> Recv {
        match RoomLink::read_once(&mut self.ws) {
            Ok(Some(Message::Text(t))) => {
                self.last_rx = Instant::now();
                match parse_packet(t.as_str()) {
                    IoPacket::Ping => {
                        self.ws.send(Message::Text(frame_pong().into())).ok();
                        Recv::Packet(IoPacket::Pong)
                    }
                    p => Recv::Packet(p),
                }
            }
            Ok(Some(Message::Close(_))) | Err(_) => Recv::Lost,
            _ => Recv::Tick,
        }
    }

    /// Since the last successful read.
    pub fn idle(&self) -> Duration {
        self.last_rx.elapsed()
    }

    /// Raw tick-able read: `Err` on kernel timeout, `Ok(None)` on nothing.
    fn read_once(
        ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    ) -> Result<Option<Message>, String> {
        match ws.read() {
            Ok(m) => Ok(Some(m)),
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// The sync thread
// ---------------------------------------------------------------------------

/// Messages the websocket thread ships to the event loop.
pub enum WsEvent {
    /// A page commit is here — apply it through the same gate as polling.
    Commit(RemoteCommit),
    /// The connection (re)joined a room and the page was re-fetched: the
    /// event loop should install it as the fresh truth (fills any commits
    /// missed while away) and reset its remote-head tracking.
    Resynced { page: crate::api::Page },
    /// One-shot status text (throttled by the thread).
    Status(String),
}

/// Watch `target` (project, title — the same mutex the poller uses), keep a
/// websocket room joined on it, and forward commit events to `tx`.
pub fn spawn_ws_sync(
    client: Client,
    sid: String,
    target: Arc<Mutex<(String, String)>>,
    tx: Sender<WsEvent>,
) {
    std::thread::spawn(move || {
        let api_domain = client.config().api_domain.clone();
        let mut backoff = Duration::from_secs(1);
        let mut last_status = Instant::now() - STATUS_THROTTLE;
        loop {
            let (project, title) = match target.lock() {
                Ok(t) => t.clone(),
                Err(_) => return,
            };
            if project.is_empty() || title.is_empty() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            // Resolve the room's ids over REST (the page API also needs a
            // full fetch — that IS the rejoin catch-up payload).
            let page = match client.get_page_in(&project, &title) {
                Ok(p) => p,
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: page lookup failed ({e})"));
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            };
            let project_id = match client.get_project_id(&project) {
                Ok(p) => p,
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: project lookup failed ({e})"));
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            };

            let mut link = match RoomLink::connect(&api_domain, &sid) {
                Ok(l) => l,
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: reconnect… ({e})"));
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            };
            backoff = Duration::from_secs(1);
            match link.join(&project_id, &page.id) {
                Ok(()) => {
                    let _ = tx.send(WsEvent::Resynced { page });
                    throttled_status(&tx, &mut last_status, "ws: 接続済み (push sync)");
                }
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: join failed ({e})"));
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            }

            // Serve the joined room until it dies or the target moves on.
            let mut last_sync = Instant::now();
            let changed = room_loop(&mut link, &client, &project, &title, &target, &tx, &mut last_sync);
            if changed {
                // Navigation: loop re-reads the target and rejoins there.
            } else {
                throttled_status(&tx, &mut last_status, "ws: 切断 — 再接続します");
                std::thread::sleep(backoff);
                backoff = grow(backoff);
            }
        }
    });
}

/// Serve one joined room until it dies (`false`) or the target moves on
/// (`true`).
fn room_loop(
    link: &mut RoomLink,
    client: &Client,
    project: &str,
    title: &str,
    target: &Arc<Mutex<(String, String)>>,
    tx: &Sender<WsEvent>,
    last_sync: &mut Instant,
) -> bool {
    loop {
        if link.idle() > SILENCE_LIMIT {
            return false; // dead half-open socket
        }
        match link.recv() {
            Recv::Packet(IoPacket::Event { name, data }) if name == "commit" => {
                if let Some(c) = parse_commit(&data) {
                    let _ = tx.send(WsEvent::Commit(c));
                }
            }
            Recv::Packet(IoPacket::Close | IoPacket::Disconnect | IoPacket::Error(_)) => {
                return false;
            }
            Recv::Packet(_) => {} // cursor, infobox:reload, pong, …
            Recv::Lost => return false,
            Recv::Tick => {
                // Navigation: re-read the shared target and rejoin there.
                let moved = match target.lock() {
                    Ok(t) => t.0 != project || t.1 != title,
                    Err(_) => return false,
                };
                // Periodic full-page resync (the insurance poll).
                if last_sync.elapsed() >= PERIODIC_SYNC && !moved {
                    if let Ok(page) = client.get_page_in(project, title) {
                        let _ = tx.send(WsEvent::Resynced { page });
                        *last_sync = Instant::now();
                    }
                }
                if moved {
                    return true;
                }
            }
        }
    }
}

/// Exponential backoff growth, capped at [`MAX_BACKOFF`].
fn grow(b: Duration) -> Duration {
    (b * 2).min(MAX_BACKOFF)
}

/// Status text, at most once per [`STATUS_THROTTLE`] (the status line is
/// transient — the app overwrites it constantly).
fn throttled_status(tx: &Sender<WsEvent>, last: &mut Instant, text: &str) {
    if last.elapsed() >= STATUS_THROTTLE {
        *last = Instant::now();
        let _ = tx.send(WsEvent::Status(text.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::PageLine;

    fn pl(id: &str, text: &str) -> PageLine {
        PageLine { id: id.into(), text: text.into(), user_id: String::new(), created: 0, updated: 0 }
    }

    #[test]
    fn parses_engine_io_open_ping_pong_and_close() {
        assert!(matches!(
            parse_packet(r#"0{"sid":"x","upgrades":[],"pingInterval":25000,"pingTimeout":20000}"#),
            IoPacket::Open { ping_interval: 25000, ping_timeout: 20000 }
        ));
        assert_eq!(parse_packet("2"), IoPacket::Ping);
        assert_eq!(parse_packet("3"), IoPacket::Pong);
        assert_eq!(parse_packet("1"), IoPacket::Close);
    }

    #[test]
    fn parses_namespace_connect_and_errors() {
        assert_eq!(parse_packet("40"), IoPacket::Connect);
        assert!(matches!(parse_packet(r#"40{"sid":"s"}"#), IoPacket::Connected(_)));
        // the observed failure when no sid cookie is sent
        assert!(matches!(
            parse_packet(r#"44{"message":"You are not logged in yet."}"#),
            IoPacket::Error(m) if m.contains("not logged in")
        ));
    }

    #[test]
    fn parses_events_with_and_without_packet_id() {
        match parse_packet(r#"42["cursor",{"visible":true}]"#) {
            IoPacket::Event { name, data } => {
                assert_eq!(name, "cursor");
                assert_eq!(data["visible"], true);
            }
            other => panic!("{other:?}"),
        }
        // the join ack's sibling: an event with a packet id carries it
        match parse_packet(r#"42["commit",{"kind":"page"}]"#) {
            IoPacket::Event { name, .. } => assert_eq!(name, "commit"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_the_join_ack() {
        match parse_packet(r#"430[{"data":{"success":true}}]"#) {
            IoPacket::Ack(v) => assert_eq!(v[0]["data"]["success"], true),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_frames_stay_other() {
        assert!(matches!(parse_packet(""), IoPacket::Other(_)));
        assert!(matches!(parse_packet("hello"), IoPacket::Other(_)));
    }

    #[test]
    fn join_frame_is_the_scouted_wire_format() {
        let f = frame_join("PROJ", "PAGE");
        assert!(f.starts_with("420["));
        let v: serde_json::Value = serde_json::from_str(&f[3..]).unwrap();
        assert_eq!(v[0], "socket.io-request");
        assert_eq!(v[1]["method"], "room:join");
        assert_eq!(v[1]["data"]["projectId"], "PROJ");
        assert_eq!(v[1]["data"]["pageId"], "PAGE");
        assert_eq!(v[1]["data"]["projectUpdatesStream"], false);
    }

    #[test]
    fn changes_parse_into_ops_ignoring_meta() {
        let changes = serde_json::json!([
            { "_insert": "_end", "lines": { "id": "i1", "text": "new line" } },
            { "_update": "u1", "lines": { "text": "edited" } },
            { "_delete": "d1", "lines": { "origText": "was here" } },
            { "linesCount": 31 },
            { "charsCount": 385 },
            { "title": "renamed!" },
        ]);
        let ops = parse_changes(changes.as_array().unwrap());
        assert_eq!(
            ops,
            vec![
                EditOp::Insert { anchor: "_end".into(), lines: vec![("i1".into(), "new line".into())] },
                EditOp::Replace { id: "u1".into(), text: "edited".into() },
                EditOp::Delete { id: "d1".into() },
            ]
        );
    }

    #[test]
    fn multi_line_insert_lines_array() {
        let changes = serde_json::json!([
            { "_insert": "_end", "lines": [{"id": "a", "text": "1"}, {"id": "b", "text": "2"}] }
        ]);
        let ops = parse_changes(changes.as_array().unwrap());
        assert_eq!(
            ops,
            vec![EditOp::Insert {
                anchor: "_end".into(),
                lines: vec![("a".into(), "1".into()), ("b".into(), "2".into())],
            }]
        );
    }

    #[test]
    fn commit_event_parses_fully() {
        let data = serde_json::json!({
            "kind": "page",
            "parentId": "p0",
            "changes": [{"_update": "u1", "lines": {"text": "edited"}}],
            "pageId": "pg",
            "userId": "me",
            "projectId": "pr",
            "id": "c1",
        });
        let c = parse_commit(&data).unwrap();
        assert_eq!(c.commit_id, "c1");
        assert_eq!(c.parent_id, "p0");
        assert_eq!(c.page_id, "pg");
        assert_eq!(c.user_id, "me");
        assert_eq!(c.ops, vec![EditOp::Replace { id: "u1".into(), text: "edited".into() }]);
    }

    #[test]
    fn meta_only_commit_parses_to_none() {
        let data = serde_json::json!({
            "kind": "page",
            "parentId": "p0",
            "changes": [{"linesCount": 31}, {"charsCount": 385}],
            "pageId": "pg",
            "userId": "me",
            "id": "c1",
        });
        assert!(parse_commit(&data).is_none());
    }

    #[test]
    fn malformed_commit_is_none() {
        assert!(parse_commit(&serde_json::json!({"kind": "page"})).is_none());
        assert!(parse_commit(&serde_json::json!({})).is_none());
    }

    #[test]
    fn remote_ops_apply_and_are_idempotent_on_replay() {
        let mut lines = vec![pl("a", "title"), pl("b", "one")];
        // fresh insert + replace + delete, all at once (a commit)
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Insert { anchor: "b".into(), lines: vec![("x".into(), "mid".into())] },
                EditOp::Replace { id: "a".into(), text: "TITLE".into() },
                EditOp::Delete { id: "b".into() },
            ],
        );
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["TITLE", "mid"]);
        // re-applying the same ops (a reconnect replay) changes nothing
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Insert { anchor: "b".into(), lines: vec![("x".into(), "mid".into())] },
                EditOp::Replace { id: "a".into(), text: "TITLE".into() },
                EditOp::Delete { id: "b".into() },
            ],
        );
        let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["TITLE".to_string(), "mid".to_string()]);
        // stale ops for unknown ids are no-ops
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Replace { id: "ghost".into(), text: "??".into() },
                EditOp::Delete { id: "ghost".into() },
                EditOp::Insert { anchor: "_end".into(), lines: vec![("y".into(), "end".into())] },
            ],
        );
        let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["TITLE".to_string(), "mid".to_string(), "end".to_string()]);
    }
}