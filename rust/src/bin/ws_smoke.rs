// End-to-end smoke test of the websocket push sync (NOTE-websocket-sync.md):
// connect with `connect.sid`, join the page room, make a REAL edit through
// the REST edit API (PAT — the edit endpoint refuses sid), and observe the
// commit event arrive over the websocket as `42["commit", …]`. The page is
// left byte-for-byte as it was.
//
// Usage:
//   COSENSE_SID=<sid> cargo run --bin ws_smoke -- <project> <title>
use cosense::api::{AuthStore, Client, Config, EditOp};
use cosense::ws::{self, IoPacket, Recv};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, title) = match (args.first(), args.get(1)) {
        (Some(p), Some(t)) => (p.clone(), t.clone()),
        _ => return Err("usage: ws_smoke <project> <title>".into()),
    };
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let Some(sid) = sid else {
        return Err("ws_smoke needs COSENSE_SID (websocket auth is the sid cookie only)".into());
    };
    let auth = AuthStore::load(Some(sid.clone()));
    let cfg = Config { project: project.clone(), auth: auth.clone(), api_domain: "scrapbox.io".into() };
    let client = Client::new(cfg)?;

    let page = client.get_page_in(&project, &title)?;
    let project_id = client.get_project_id(&project)?;
    println!("page: {} ({} lines, id {}) projectId {}", page.title, page.lines.len(), page.id, project_id);

    let mut link = ws::RoomLink::connect("scrapbox.io", &sid).map_err(|e| format!("connect: {e}"))?;
    link.join(&project_id, &page.id).map_err(|e| format!("join: {e}"))?;
    println!("joined room");

    let marker = format!(
        "ws_smoke {} (safe to delete)",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs()
    );

    // 1. insert the marker through the REST edit API and wait for the push
    let p1 = client.preview_edit(&project, &page.id, &[EditOp::insert("_end", &marker)])?;
    let c1 = client.submit_edit(&project, &p1.preview_id)?;
    println!("committed insert ({}) — waiting for the push…", c1.commit_id);

    let (insert_seen, done) = wait_for_commit(
        &mut link,
        &c1.commit_id,
        &marker,
        insert_matches,
        std::time::Duration::from_secs(15),
    )?;
    println!(
        "push: insert event seen in {:.1}s ({insert_seen})",
        done.as_secs_f64()
    );

    // 2. delete it again and wait for that push too
    let after = client.get_page_in(&project, &title)?;
    let line = after
        .lines
        .iter()
        .find(|l| l.text == marker)
        .ok_or("marker line not found after insert")?;
    let p2 = client.preview_edit(&project, &page.id, &[EditOp::Delete { id: line.id.clone() }])?;
    let c2 = client.submit_edit(&project, &p2.preview_id)?;
    println!("committed delete ({}) — waiting for the push…", c2.commit_id);

    let (delete_seen, _) = wait_for_commit(
        &mut link,
        &c2.commit_id,
        &line.id,
        delete_matches,
        std::time::Duration::from_secs(15),
    )?;
    println!("push: delete event seen (op {delete_seen})");

    // 3. verify the page is back to the original line count
    let fin = client.get_page_in(&project, &title)?;
    if fin.lines.iter().any(|l| l.text == marker) {
        return Err("marker still present after delete".into());
    }
    println!(
        "verified:  marker gone ({} lines, started with {})",
        fin.lines.len(),
        page.lines.len()
    );
    println!("OK — ws push round-trip works (insert + delete observed live)");
    Ok(())
}

/// Drain the room until the commit `commit_id` shows up (15 s cap), then
/// run `verify` over its ops. Returns the ops summary and the elapsed time.
fn wait_for_commit(
    link: &mut ws::RoomLink,
    commit_id: &str,
    expected: &str,
    verify: fn(&EditOp, &str) -> bool,
    timeout: std::time::Duration,
) -> Result<(String, std::time::Duration), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match link.recv() {
            Recv::Packet(IoPacket::Event { name, data }) if name == "commit" => {
                if let Some(c) = ws::parse_commit(&data) {
                    let summary = summarize(&c);
                    println!(
                        "  event: commit {} (parent {}) user {}: {}",
                        c.commit_id,
                        c.parent_id,
                        if c.user_id.is_empty() { "<none>" } else { &c.user_id },
                        summary
                    );
                    if c.commit_id == commit_id {
                        let ok = c.ops.iter().any(|op| verify(op, expected));
                        println!("  → {}", if ok { "MATCHES" } else { "MISMATCH" });
                        return Ok((summary, start.elapsed()));
                    }
                }
            }
            Recv::Packet(_) => {}
            Recv::Lost => return Err("connection lost while waiting for the push".into()),
            Recv::Tick => {}
        }
    }
    Err(format!("no commit {commit_id} within {timeout:?}").into())
}

/// A one-line shape of the commit's ops (insert / update / delete …).
fn summarize(c: &ws::RemoteCommit) -> String {
    c.ops
        .iter()
        .map(|op| match op {
            EditOp::Insert { anchor, lines } => {
                format!("insert {}×(anchor {anchor})", lines.len())
            }
            EditOp::Replace { id, .. } => format!("update {id}"),
            EditOp::Delete { id } => format!("delete {id}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Did this op carry the marker text (the insert we made)?
fn insert_matches(op: &EditOp, marker: &str) -> bool {
    matches!(op, EditOp::Insert { lines, .. } if lines.iter().any(|(_, t)| t.contains(marker)))
}

/// Did this op delete the exact line id we measured after the insert?
fn delete_matches(op: &EditOp, line_id: &str) -> bool {
    matches!(op, EditOp::Delete { id } if id == line_id)
}