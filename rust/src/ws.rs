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
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::protocol::Message;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{ClientRequestBuilder, WebSocket};

/// How often the ws thread wakes while idle: navigation, resync requests
/// and kernel read timeouts surface here. The server pings every 25 s, so
/// this is far shorter than any real traffic gap — and short enough that a
/// gap-driven resync request is served within ~a quarter second.
const READ_TICK: Duration = Duration::from_millis(250);
/// How long individual reads may block during the TLS/HTTP handshake
/// (round trips); tightened to [`READ_TICK`] once the socket is up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
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
    Open {
        ping_interval: u64,
        ping_timeout: u64,
    },
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
    Event {
        name: String,
        data: serde_json::Value,
    },
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
                    let name = a.remove(0).as_str().map(str::to_string).unwrap_or_default();
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
        IoPacket::Open {
            ping_interval: n("pingInterval"),
            ping_timeout: n("pingTimeout"),
        }
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

/// Parse one `42["commit",{…}]` event. Malformed / non-page commits return
/// `None`. A meta-only commit (linesCount / charsCount / a title change —
/// no line ops) parses to `ops: []`: there is nothing to APPLY, but the
/// commit still advances the page's chain, and dropping it here is what
/// used to break contiguity — the next text commit's parent pointed at the
/// meta commit nobody remembered, and every such gap cost a full-page
/// resync over HTTP.
pub fn parse_commit(data: &serde_json::Value) -> Option<RemoteCommit> {
    let commit_id = data.get("id")?.as_str()?.to_string();
    let parent_id = data.get("parentId")?.as_str()?.to_string();
    let page_id = data.get("pageId")?.as_str()?.to_string();
    let user_id = data
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let ops = parse_changes(data.get("changes")?.as_array()?);
    Some(RemoteCommit {
        commit_id,
        parent_id,
        page_id,
        user_id,
        ops,
    })
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
                ops.push(EditOp::Insert {
                    anchor: anchor.to_string(),
                    lines,
                });
            }
        } else if let Some(id) = c.get("_update").and_then(|v| v.as_str()) {
            if let Some(text) = c
                .get("lines")
                .and_then(|l| l.get("text"))
                .and_then(|v| v.as_str())
            {
                ops.push(EditOp::Replace {
                    id: id.to_string(),
                    text: text.to_string(),
                });
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
                if id.is_empty() {
                    None
                } else {
                    Some((id.to_string(), text.to_string()))
                }
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
/// already present is skipped (a replayed event or our own echo cannot
/// duplicate a line), and replaces / deletes for ids we do not have are
/// no-ops (a stale event cannot resurrect or clobber anything). Inserted
/// and replaced lines are stamped with the COMMITTER's `user_id` so line
/// blame is right immediately, not only after the next full resync.
pub fn apply_remote_ops(lines: &mut Vec<PageLine>, ops: &[EditOp], user_id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for op in ops {
        match op {
            EditOp::Insert {
                anchor,
                lines: newl,
            } => {
                let mut at = if anchor == "_end" {
                    lines.len()
                } else {
                    lines
                        .iter()
                        .position(|l| l.id == *anchor)
                        .unwrap_or(lines.len())
                };
                for (id, text) in newl {
                    if lines.iter().any(|l| l.id == *id) {
                        continue; // already applied (replay / own echo) — never duplicate
                    }
                    lines.insert(
                        at,
                        PageLine {
                            id: id.clone(),
                            text: text.clone(),
                            user_id: user_id.to_string(),
                            created: now,
                            updated: now,
                        },
                    );
                    at += 1;
                }
            }
            EditOp::Replace { id, text } => {
                if let Some(l) = lines.iter_mut().find(|l| l.id == *id) {
                    if l.text != *text {
                        // a same-text replace is an echo / replay — skip it
                        // outright (nothing to do; keeps blame intact)
                        l.text = text.clone();
                        l.user_id = user_id.to_string();
                        l.updated = now;
                    }
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

/// How long to wait for the TCP handshake itself.
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `TcpStream::connect` with a deadline. Resolves the name, then tries the
/// addresses in turn — an IPv6 address that black-holes must not eat the
/// whole budget when an IPv4 one would answer.
fn connect_within(addr: (&str, u16), timeout: Duration) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("resolve: no addresses for {}", addr.0));
    }
    let each = timeout / addrs.len().min(4) as u32;
    let mut last = String::from("no address answered");
    for a in &addrs {
        match TcpStream::connect_timeout(a, each.max(Duration::from_secs(2))) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("tcp: {e}"),
        }
    }
    Err(last)
}

impl RoomLink {
    /// Connect to `api_domain`, speak the engine.io open + namespace
    /// connect (`40`), and verify the server accepted us. `sid` is the
    /// `connect.sid` cookie value — the ONLY credential the server takes
    /// on the websocket (PAT is refused, verified live).
    pub fn connect(api_domain: &str, sid: &str) -> Result<Self, String> {
        let uri: tungstenite::http::Uri =
            format!("wss://{api_domain}/socket.io/?EIO=4&transport=websocket")
                .parse()
                .map_err(|e| format!("bad ws uri: {e}"))?;
        let request = ClientRequestBuilder::new(uri)
            .with_header("Origin", format!("https://{api_domain}"))
            .with_header("Cookie", format!("connect.sid={sid}"))
            .with_header("User-Agent", "cosentty");
        // A bounded connect: the OS default can hang for minutes on a
        // black-holed route, and every second of that is a second the
        // reader is on the slow poll believing a push channel is coming.
        let stream = connect_within((api_domain, 443), TCP_CONNECT_TIMEOUT)?;
        stream.set_nodelay(true).ok();
        // The TLS + HTTP handshake needs time for its round trips — a short
        // read timeout here would abort it with WouldBlock. Use a generous
        // one for the handshake and switch to the fast tick AFTER it.
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        let (mut ws, _) = tungstenite::client_tls_with_config(request, stream, None, None)
            .map_err(|e| format!("ws handshake: {e}"))?;
        // From here on, reads return within READ_TICK so navigation and
        // resync requests are noticed promptly.
        set_inner_read_timeout(&mut ws, Some(READ_TICK));
        // engine.io open (`0{…}`), within a deadline; tick timeouts just
        // keep the wait going rather than aborting the connection.
        let deadline = Instant::now() + JOIN_ACK_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("no engine.io open frame".into());
            }
            match RoomLink::read_once(&mut ws) {
                Ok(Some(Message::Text(t))) if t.starts_with('0') => break,
                Ok(Some(Message::Text(t))) => {
                    return Err(format!("unexpected open frame: {t}"));
                }
                Ok(Some(Message::Close(_))) => return Err("closed before open".into()),
                Err(e) => return Err(format!("no engine.io open: {e}")),
                _ => {} // tick / non-text — keep waiting
            }
        }
        // socket.io namespace connect
        ws.send(Message::Text(frame_connect().into()))
            .map_err(|e| format!("send 40: {e}"))?;
        let deadline = Instant::now() + JOIN_ACK_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("no connect ack".into());
            }
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
                _ => {} // tick — keep waiting
            }
        }
        Ok(RoomLink {
            ws,
            last_rx: Instant::now(),
        })
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
    fn read_once(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<Option<Message>, String> {
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

/// Reach through rustls/tungstenite to the underlying [`TcpStream`] (its
/// `sock` field is public) and set the read timeout — the recv loop ticks
/// at [`READ_TICK`] while the handshake got to keep [`HANDSHAKE_TIMEOUT`].
fn set_inner_read_timeout(
    ws: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) {
    let tcp = match ws.get_mut() {
        MaybeTlsStream::Plain(tcp) => Some(tcp),
        MaybeTlsStream::Rustls(stream) => Some(&mut stream.sock),
        _ => None,
    };
    if let Some(tcp) = tcp {
        let _ = tcp.set_read_timeout(timeout);
    }
}

// ---------------------------------------------------------------------------
// The sync thread
// ---------------------------------------------------------------------------

/// A full-page sync result: the page as fetched, plus the newest commit
/// the thread had forwarded when the fetch was made — the local model is
/// then known to be AT `head`, so the next event with `parentId == head`
/// applies as a contiguous diff (no mistrust-reload after every resync).
#[derive(Debug)]
pub struct ResyncPage {
    pub page: crate::api::Page,
    /// Newest commit the thread had seen when the page was fetched
    /// (`None` on a brand-new room: the first event triggers one catch-up).
    pub head: Option<String>,
    /// Server-state epoch when the FETCH STARTED. If the viewer has moved
    /// past it since (a commit of ours landed, a commit was applied), this
    /// page is a photograph of the past and installing it would take the
    /// reader backwards — the same guard `PolledPage` carries.
    pub epoch: u64,
}

/// Messages the websocket thread ships to the event loop.
pub enum WsEvent {
    /// A page commit is here — apply it through the same gate as polling.
    Commit(RemoteCommit),
    /// The connection (re)joined a room or a resync was served: install the
    /// page as the fresh truth (fills any commits missed while away) and
    /// resume the commit chain at `head`.
    Resynced(ResyncPage),
    /// One-shot status text (throttled by the thread).
    Status(String),
    /// The push channel's state changed, for the room named by
    /// `(project, title)`. NEVER throttled and never derived from the
    /// status text: the poller's interval hangs off this, and a swallowed
    /// `Reconnecting` would leave it at 60 s — the exact silence this whole
    /// mechanism exists to prevent.
    ///
    /// The room is named because a `Live` for the page the reader has
    /// already left must not hold the NEW page on the 60 s poll. That
    /// message can still be in the channel when the navigation happens.
    State {
        project: String,
        title: String,
        state: crate::capability::SyncState,
    },
}

/// Requests the event loop sends to the sync thread. Only the thread talks
/// to the network; the app parks a request in this channel and applies the
/// Resynced result when it comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsRequest {
    /// Refetch the room's page and ship it as a `WsEvent::Resynced`.
    Resync,
}

/// Which live-update plan to start with: a push channel is attempted when a
/// `connect.sid` exists, but it has proven NOTHING yet, so the poll starts
/// fast either way.
///
/// This is the fix for the stale-sid hole. Choosing 60 s from the mere
/// PRESENCE of a cookie meant an expired one delayed every web edit by up to
/// a minute, silently. The interval is now relaxed only by a
/// [`SyncState::Live`](crate::capability::SyncState::Live) event, i.e. after
/// a room join AND its catch-up fetch have both succeeded.
pub fn initial_plan(sid_available: bool) -> (bool, crate::capability::SyncState) {
    (sid_available, crate::capability::SyncState::Polling)
}

/// An edge-triggered resync request that is served until SUCCESS: a failed
/// fetch does not consume the request (the gap event's effect would
/// otherwise vanish until the periodic resync), and retries are spaced by
/// a capped exponential backoff so a failing endpoint is not hammered.
struct ResyncRequest {
    due: bool,
    next_at: Instant,
    backoff: Duration,
}

impl ResyncRequest {
    fn new() -> Self {
        Self {
            due: false,
            next_at: Instant::now(),
            backoff: Duration::from_secs(1),
        }
    }

    /// (Re)arm the request (arrival of a `WsRequest::Resync`).
    fn request(&mut self) {
        self.due = true;
    }

    /// May a fetch attempt go now? Arms the containment window for the
    /// next attempt (so a slow/failing server is not hammered per tick).
    fn attempt(&mut self, now: Instant) -> bool {
        if self.due && now >= self.next_at {
            self.next_at = now + self.backoff;
            true
        } else {
            false
        }
    }

    /// The fetch succeeded: the request is consumed.
    fn ok(&mut self) {
        self.due = false;
        self.next_at = Instant::now();
        self.backoff = Duration::from_secs(1);
    }

    /// The fetch failed: the request stays due, the retry gap grows.
    fn fail(&mut self) {
        self.backoff = (self.backoff * 2).min(Duration::from_secs(15));
    }
}

/// The page id the viewer has in hand for the page it is showing, as
/// `(project, title, page id)`. Every page install refreshes it; the room
/// joiner consumes it instead of asking the server for a fact it was just
/// told (see `take_page_id_hint`).
pub type PageIdHint = Arc<Mutex<Option<(String, String, String)>>>;

/// Watch `target` (project, title — the same mutex the poller uses), keep a
/// websocket room joined on it, forward commit events to `tx`, and serve
/// `WsRequest::Resync` requests from the event loop.
pub fn spawn_ws_sync(
    client: Client,
    sid: String,
    target: Arc<Mutex<(String, String)>>,
    req_rx: mpsc::Receiver<WsRequest>,
    tx: Sender<WsEvent>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
) {
    spawn_ws_sync_with_page_id_hint(
        client,
        sid,
        target,
        req_rx,
        tx,
        epoch,
        Arc::new(Mutex::new(None)),
    );
}

/// As [`spawn_ws_sync`], reusing the page id the viewer already has for
/// the page it is showing, rather than fetching the page again just to
/// read its id. The catch-up fetch after joining remains mandatory.
pub fn spawn_ws_sync_with_page_id_hint(
    client: Client,
    sid: String,
    target: Arc<Mutex<(String, String)>>,
    req_rx: mpsc::Receiver<WsRequest>,
    tx: Sender<WsEvent>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    page_id_hint: PageIdHint,
) {
    std::thread::spawn(move || {
        use crate::capability::SyncState;
        let api_domain = client.config().api_domain.clone();
        let mut backoff = Duration::from_secs(1);
        let mut last_status = Instant::now() - STATUS_THROTTLE;
        // The room we last joined, and the newest commit we forwarded for it.
        // The head survives reconnects (the room's history does too) and is
        // cleared when the target moves to a different page.
        let mut joined: Option<(String, String)> = None;
        let mut last_commit_id: Option<String> = None;
        loop {
            let (project, title) = match target.lock() {
                Ok(t) => t.clone(),
                Err(_) => return,
            };
            if project.is_empty() || title.is_empty() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            if joined
                .as_ref()
                .map_or(true, |j| *j != (project.clone(), title.clone()))
            {
                joined = Some((project.clone(), title.clone()));
                last_commit_id = None; // new room: commit lineage is unknown
            }
            // Resolve the room's ids over REST. The page install that moved
            // the target already read the page, and the page id is
            // immutable, so the hint answers this without a request — for
            // the first page AND for every navigation after it. A reconnect
            // with no hint left still performs this pre-join fetch; its DATA
            // is never published, because the post-join catch-up below is
            // the authoritative snapshot.
            let page_id = match take_page_id_hint(&page_id_hint, &project, &title) {
                Some(id) => id,
                None => match client.get_page_in(&project, &title) {
                    Ok(page) => page.id,
                    Err(e) => {
                        throttled_status(
                            &tx,
                            &mut last_status,
                            &format!("ws: page lookup failed ({e})"),
                        );
                        state(&tx, &project, &title, SyncState::Reconnecting);
                        std::thread::sleep(backoff);
                        backoff = grow(backoff);
                        continue;
                    }
                },
            };
            let project_id = match client.get_project_id(&project) {
                Ok(p) => p,
                Err(e) => {
                    throttled_status(
                        &tx,
                        &mut last_status,
                        &format!("ws: project lookup failed ({e})"),
                    );
                    state(&tx, &project, &title, SyncState::Reconnecting);
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            };

            let mut link = match RoomLink::connect(&api_domain, &sid) {
                Ok(l) => l,
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: reconnect… ({e})"));
                    state(&tx, &project, &title, SyncState::Reconnecting);
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            };
            backoff = Duration::from_secs(1);
            match link.join(&project_id, &page_id) {
                Ok(()) => {}
                Err(e) => {
                    throttled_status(&tx, &mut last_status, &format!("ws: join failed ({e})"));
                    state(&tx, &project, &title, SyncState::Reconnecting);
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue;
                }
            }
            // POST-JOIN catch-up: refetch the page so the published snapshot
            // covers everything up to the join — commits between the pre-join
            // fetch and the join are in neither the room stream we had nor
            // the old snapshot. A failed catch-up fetch means the join does
            // NOT count as live: reconnect and retry the whole cycle.
            let at = epoch.load(std::sync::atomic::Ordering::SeqCst);
            match client.get_page_in(&project, &title) {
                Ok(page) => {
                    let _ = tx.send(WsEvent::Resynced(ResyncPage {
                        page,
                        head: last_commit_id.clone(),
                        epoch: at,
                    }));
                    // The join AND its catch-up both landed: this is the only place
                    // the push channel counts as live, so the only place the
                    // insurance poll is allowed to relax.
                    state(&tx, &project, &title, SyncState::Live);
                    throttled_status(
                        &tx,
                        &mut last_status,
                        &crate::t!("ws: 接続済み (push sync)", "ws: connected (push sync)"),
                    );
                }
                Err(e) => {
                    throttled_status(
                        &tx,
                        &mut last_status,
                        &format!("ws: catch-up fetch failed ({e}) — reconnecting"),
                    );
                    state(&tx, &project, &title, SyncState::Reconnecting);
                    std::thread::sleep(backoff);
                    backoff = grow(backoff);
                    continue; // re-read the target, reconnect, rejoin, refetch
                }
            }

            // Serve the joined room until it dies or the target moves on.
            let mut last_sync = Instant::now();
            let changed = room_loop(
                &mut link,
                &client,
                &project,
                &title,
                &target,
                &req_rx,
                &tx,
                &mut last_commit_id,
                &mut last_sync,
                &epoch,
            );
            if changed {
                // Navigation: loop re-reads the target and rejoins there.
            } else {
                state(&tx, &project, &title, SyncState::Reconnecting);
                throttled_status(
                    &tx,
                    &mut last_status,
                    &crate::t!("ws: 切断 — 再接続します", "ws: disconnected — reconnecting"),
                );
                std::thread::sleep(backoff);
                backoff = grow(backoff);
            }
        }
    });
}

/// Serve one joined room until it dies (`false`) or the target moves on
/// (`true`). Resync requests and navigation are served on ticks; commit
/// events are forwarded with the newest id tracked as the room head.
fn room_loop(
    link: &mut RoomLink,
    client: &Client,
    project: &str,
    title: &str,
    target: &Arc<Mutex<(String, String)>>,
    req_rx: &mpsc::Receiver<WsRequest>,
    tx: &Sender<WsEvent>,
    last_commit_id: &mut Option<String>,
    last_sync: &mut Instant,
    epoch: &std::sync::atomic::AtomicU64,
) -> bool {
    // A requested resync is served until it SUCCEEDS: a failed fetch does
    // not consume the request (the gap event's effect must not vanish until
    // the periodic resync), and retries are spaced by a capped backoff.
    let mut resync = ResyncRequest::new();
    let mut last_resync_status = Instant::now() - STATUS_THROTTLE;
    loop {
        if link.idle() > SILENCE_LIMIT {
            return false; // dead half-open socket
        }
        // The event loop asked for a full-page resync (commit-chain gap):
        // fetch on THIS thread — the UI thread never does network I/O.
        match req_rx.try_recv() {
            Ok(WsRequest::Resync) => resync.request(),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return false,
        }
        if resync.attempt(Instant::now()) {
            let at = epoch.load(std::sync::atomic::Ordering::SeqCst);
            match client.get_page_in(project, title) {
                Ok(page) => {
                    let _ = tx.send(WsEvent::Resynced(ResyncPage {
                        page,
                        head: last_commit_id.clone(),
                        epoch: at,
                    }));
                    *last_sync = Instant::now();
                    resync.ok();
                }
                Err(e) => {
                    resync.fail(); // request stays due; retry after backoff
                    throttled_status(
                        tx,
                        &mut last_resync_status,
                        &crate::t!(
                            "ws: 再同期失敗 — 再試行します ({e})",
                            "ws: resync failed — retrying ({e})"
                        ),
                    );
                }
            }
        }
        match link.recv() {
            Recv::Packet(IoPacket::Event { name, data }) if name == "commit" => {
                if let Some(c) = parse_commit(&data) {
                    *last_commit_id = Some(c.commit_id.clone());
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
                    let at = epoch.load(std::sync::atomic::Ordering::SeqCst);
                    if let Ok(page) = client.get_page_in(project, title) {
                        let _ = tx.send(WsEvent::Resynced(ResyncPage {
                            page,
                            head: last_commit_id.clone(),
                            epoch: at,
                        }));
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

/// Take the page id the viewer already knows for this target, if it is
/// about this target. Consumed either way: a hint that names another page
/// is stale, and one that named this page has now been used.
fn take_page_id_hint(hint: &PageIdHint, project: &str, title: &str) -> Option<String> {
    let mut hint = hint.lock().unwrap_or_else(|p| p.into_inner());
    let (hint_project, hint_title, page_id) = hint.take()?;
    (hint_project == project && hint_title == title && !page_id.is_empty()).then_some(page_id)
}

/// Exponential backoff growth, capped at [`MAX_BACKOFF`].
fn grow(b: Duration) -> Duration {
    (b * 2).min(MAX_BACKOFF)
}

/// Publish a push-channel state. Deliberately NOT throttled — see
/// [`WsEvent::State`].
fn state(tx: &Sender<WsEvent>, project: &str, title: &str, s: crate::capability::SyncState) {
    let _ = tx.send(WsEvent::State {
        project: project.to_string(),
        title: title.to_string(),
        state: s,
    });
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
        PageLine {
            id: id.into(),
            text: text.into(),
            user_id: String::new(),
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn the_page_id_hint_is_consumed_only_for_its_own_target() {
        let hint: PageIdHint = Arc::new(Mutex::new(Some((
            "proj".into(),
            "page".into(),
            "P1".into(),
        ))));
        assert_eq!(take_page_id_hint(&hint, "proj", "page"), Some("P1".into()));
        assert!(hint.lock().unwrap().is_none(), "used once");
        assert_eq!(
            take_page_id_hint(&hint, "proj", "page"),
            None,
            "a reconnect with nothing left asks the server"
        );

        let stale: PageIdHint = Arc::new(Mutex::new(Some((
            "proj".into(),
            "page".into(),
            "P1".into(),
        ))));
        assert_eq!(take_page_id_hint(&stale, "proj", "other"), None);
        assert!(
            stale.lock().unwrap().is_none(),
            "a stale hint must not leak into a later room"
        );
    }

    #[test]
    fn a_sid_alone_does_not_earn_the_slow_poll() {
        use crate::capability::SyncState;
        // Push sync is ATTEMPTED when a sid exists — but it has proven
        // nothing yet, so both sessions start at the fast poll. A stale sid
        // used to cost up to 60 s of silence here.
        assert_eq!(initial_plan(true), (true, SyncState::Polling));
        assert_eq!(initial_plan(false), (false, SyncState::Polling));
        assert_eq!(initial_plan(true).1.poll_interval(), Duration::from_secs(3));
    }

    #[test]
    fn resync_request_survives_failures_until_success() {
        let mut r = ResyncRequest::new();
        let t0 = Instant::now(); // after construction, so the first attempt may run
                                 // quietly idle until armed
        assert!(!r.attempt(t0));
        r.request();
        // first attempt is allowed immediately
        assert!(r.attempt(t0));
        // …and a FAILED fetch does not consume the request
        r.fail();
        assert!(r.due, "a failed attempt must not drop the gap resync");
        // hammering right after a failure is rejected (bounded cadence)
        assert!(!r.attempt(t0 + Duration::from_millis(1)));
        // after the backoff a retry is allowed (exponential, capped)
        assert!(r.attempt(t0 + Duration::from_secs(2)));
        r.fail();
        assert!(r.due);
        assert!(!r.attempt(t0 + Duration::from_secs(2) + Duration::from_millis(1)));
        assert!(r.attempt(t0 + Duration::from_secs(4)));
        r.ok(); // success consumes it
        assert!(!r.due);
        assert!(
            !r.attempt(t0 + Duration::from_secs(5)),
            "cleared request stays quiet"
        );
        // …and the backoff reset for the next request
        r.request();
        assert!(r.attempt(Instant::now()));
    }

    #[test]
    fn resync_backoff_is_capped() {
        let mut r = ResyncRequest::new();
        for _ in 0..12 {
            r.fail();
        }
        let t0 = Instant::now();
        r.request();
        assert!(r.attempt(t0));
        assert_eq!(
            r.backoff,
            Duration::from_secs(15),
            "retry gap never exceeds 15s"
        );
    }

    #[test]
    #[ignore = "requires a live Cosense project and COSENSE_SID; run explicitly with --ignored"]
    fn ws_sync_publishes_a_post_join_catch_up_snapshot() {
        // Explicit integration against the real server. Ordinary tests must
        // not acquire a network dependency from the developer's credentials.
        // After joining, the worker must publish a fresh catch-up snapshot.
        let sid = std::env::var("COSENSE_SID").expect("COSENSE_SID is required");
        assert!(!sid.is_empty(), "COSENSE_SID must not be empty");
        let project =
            std::env::var("COSENSE_PROJECT_NAME").expect("COSENSE_PROJECT_NAME is required");
        let cfg = crate::api::Config {
            project: project.clone(),
            auth: crate::api::AuthStore::load(Some(sid.clone())),
            api_domain: "scrapbox.io".into(),
        };
        let client = crate::api::Client::new(cfg).expect("client");
        let (_, pages) = client
            .list_pages_in(&project, 1, 0, "updated")
            .expect("latest page");
        let title = pages.first().expect("non-empty project").title.clone();

        let (req_tx, req_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let target = Arc::new(Mutex::new((project.clone(), title.clone())));
        spawn_ws_sync(
            client.clone(),
            sid.clone(),
            target,
            req_rx,
            tx,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut catch_up: Option<crate::api::Page> = None;
        let mut connected = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(WsEvent::Resynced(res)) => {
                    // the published snapshot is the CURRENT page — as read
                    // post-join, not some pre-join fetch
                    let now_page = client
                        .get_page_in(&project, &title)
                        .expect("current page read");
                    assert_eq!(res.page.id, now_page.id, "catch-up is the live page");
                    assert_eq!(res.page.title, now_page.title);
                    catch_up = Some(now_page);
                }
                Ok(WsEvent::State {
                    state: crate::capability::SyncState::Live,
                    ..
                }) => connected = true,
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => panic!("ws channel died: {e}"),
            }
            if catch_up.is_some() && connected {
                break;
            }
        }
        assert!(connected, "the thread reported itself connected");
        assert!(
            catch_up.is_some(),
            "a post-join catch-up snapshot was published"
        );
        // keep the request half alive so the thread exits its room cycle
        // cleanly when the channel closes (test process ends next anyway)
        drop(req_tx);
        let _ = rx;
    }

    #[test]
    fn parses_engine_io_open_ping_pong_and_close() {
        assert!(matches!(
            parse_packet(r#"0{"sid":"x","upgrades":[],"pingInterval":25000,"pingTimeout":20000}"#),
            IoPacket::Open {
                ping_interval: 25000,
                ping_timeout: 20000
            }
        ));
        assert_eq!(parse_packet("2"), IoPacket::Ping);
        assert_eq!(parse_packet("3"), IoPacket::Pong);
        assert_eq!(parse_packet("1"), IoPacket::Close);
    }

    #[test]
    fn parses_namespace_connect_and_errors() {
        assert_eq!(parse_packet("40"), IoPacket::Connect);
        assert!(matches!(
            parse_packet(r#"40{"sid":"s"}"#),
            IoPacket::Connected(_)
        ));
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
                EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("i1".into(), "new line".into())]
                },
                EditOp::Replace {
                    id: "u1".into(),
                    text: "edited".into()
                },
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
        assert_eq!(
            c.ops,
            vec![EditOp::Replace {
                id: "u1".into(),
                text: "edited".into()
            }]
        );
    }

    #[test]
    fn meta_only_commit_parses_with_empty_ops_and_keeps_the_chain() {
        let data = serde_json::json!({
            "kind": "page",
            "parentId": "p0",
            "changes": [{"linesCount": 31}, {"charsCount": 385}],
            "pageId": "pg",
            "userId": "me",
            "id": "c1",
        });
        let c = parse_commit(&data).expect("meta commits still carry the chain");
        assert_eq!(c.commit_id, "c1");
        assert_eq!(c.parent_id, "p0");
        assert!(c.ops.is_empty(), "nothing to apply");
    }

    #[test]
    fn malformed_commit_is_none() {
        assert!(parse_commit(&serde_json::json!({"kind": "page"})).is_none());
        assert!(parse_commit(&serde_json::json!({})).is_none());
    }

    #[test]
    fn partial_insert_replay_skips_existing_ids_without_advancing_the_insertion_slot() {
        for anchor in ["_end", "tail", "missing"] {
            let mut lines = vec![pl("a", "title"), pl("x", "already received")];
            if anchor == "tail" {
                lines.push(pl("tail", "last"));
            }
            let ops = vec![EditOp::Insert {
                anchor: anchor.into(),
                lines: vec![
                    ("x".into(), "already received".into()),
                    ("y".into(), "new".into()),
                    ("z".into(), "newest".into()),
                ],
            }];
            apply_remote_ops(&mut lines, &ops, "alice");
            let ids: Vec<_> = lines.iter().map(|l| l.id.as_str()).collect();
            let expected = if anchor == "tail" {
                vec!["a", "x", "y", "z", "tail"]
            } else {
                vec!["a", "x", "y", "z"]
            };
            assert_eq!(ids, expected);
            apply_remote_ops(&mut lines, &ops, "bob");
            assert_eq!(
                lines.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(lines[2].user_id, "alice");
        }
    }

    #[test]
    fn remote_ops_apply_and_are_idempotent_on_replay() {
        let mut lines = vec![pl("a", "title"), pl("b", "one")];
        // fresh insert + replace + delete, all at once (a commit)
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Insert {
                    anchor: "b".into(),
                    lines: vec![("x".into(), "mid".into())],
                },
                EditOp::Replace {
                    id: "a".into(),
                    text: "TITLE".into(),
                },
                EditOp::Delete { id: "b".into() },
            ],
            "alice",
        );
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["TITLE", "mid"]);
        // remote lines are stamped with the committer (blame is right now)
        assert_eq!(lines[0].user_id, "alice");
        assert_eq!(lines[1].user_id, "alice");
        // re-applying the same ops (a reconnect replay) changes nothing
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Insert {
                    anchor: "b".into(),
                    lines: vec![("x".into(), "mid".into())],
                },
                EditOp::Replace {
                    id: "a".into(),
                    text: "TITLE".into(),
                },
                EditOp::Delete { id: "b".into() },
            ],
            "bob",
        );
        let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["TITLE".to_string(), "mid".to_string()]);
        // …and the echo did not clobber the recorded committer
        assert_eq!(lines[0].user_id, "alice");
        // stale ops for unknown ids are no-ops
        apply_remote_ops(
            &mut lines,
            &[
                EditOp::Replace {
                    id: "ghost".into(),
                    text: "??".into(),
                },
                EditOp::Delete { id: "ghost".into() },
                EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("y".into(), "end".into())],
                },
            ],
            "carol",
        );
        let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(
            texts,
            vec!["TITLE".to_string(), "mid".to_string(), "end".to_string()]
        );
        // a fresh insert stamps its committer
        assert_eq!(lines[2].user_id, "carol");
    }
}
