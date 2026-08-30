//! Disposable, explicitly invoked probe for Cosense's outline-editing wire
//! protocol. It performs one real browser gesture and therefore changes the
//! named page. It is never called by the TUI or the test suite.
//!
//! Usage:
//!   COSENSE_SID=<sid> cargo run --bin outline_probe -- \
//!     --execute <project> <title> <line-id> \
//!     <ctrl-up|ctrl-down|ctrl-left|ctrl-right|alt-up|alt-down|alt-left|alt-right>

use cosense::api::{AuthStore, Client, Config, EditOp, Page};
use cosense::chrome::{ChromeBackend, OutlineProbeKey};
use cosense::webrender::WebBackend;
use cosense::ws::{self, IoPacket, Recv};
use serde_json::{json, Value};
use std::str::FromStr;
use std::time::{Duration, Instant};

const COMMIT_TIMEOUT: Duration = Duration::from_secs(20);
const USAGE: &str = "Usage:
  COSENSE_SID=<sid> cargo run --bin outline_probe -- \\
    --execute <project> <title> <line-id> \\
    <ctrl-up|ctrl-down|ctrl-left|ctrl-right|alt-up|alt-down|alt-left|alt-right>

This diagnostic performs exactly one real outline-editing key chord and changes
that page. It does not restore the page. Quote a title containing spaces.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() != 5 || args.first().map(String::as_str) != Some("--execute") {
        return Err(
            format!("{USAGE}\n\nRefusing to edit without the explicit --execute guard.").into(),
        );
    }

    let project = &args[1];
    let title = &args[2];
    let line_id = &args[3];
    let key_text = &args[4];
    let key = OutlineProbeKey::from_str(key_text)?;
    let sid = std::env::var("COSENSE_SID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("outline_probe needs a non-empty COSENSE_SID")?;

    let auth = AuthStore::load(Some(sid.clone()));
    let client = Client::new(Config {
        project: project.clone(),
        auth,
        api_domain: "scrapbox.io".into(),
    })?;
    let before = client.get_page_in(project, title)?;
    if before.id.is_empty() || !before.persistent {
        return Err("the requested page does not exist".into());
    }
    if !before.lines.iter().any(|line| line.id == *line_id) {
        return Err(format!("line id {line_id:?} is not present on the requested page").into());
    }

    // Detection is side-effect free. Chrome is launched only after the
    // websocket room is joined, so the first resulting commit cannot race
    // ahead of our observer.
    let backend = ChromeBackend::detect(Some(sid.clone()))
        .ok_or("no Chrome found (set COSENSE_CHROME to its executable)")?;
    let project_id = client.get_project_id(project)?;
    let mut link = ws::RoomLink::connect("scrapbox.io", &sid)
        .map_err(|error| format!("websocket connect failed: {error}"))?;
    link.join(&project_id, &before.id)
        .map_err(|error| format!("websocket room join failed: {error}"))?;

    print_page("before", &before);
    println!("\naction:");
    println!("  line_id={line_id:?}");
    println!("  key={key_text}");
    println!("  websocket room joined; dispatching one chord through CDP");

    backend
        .dispatch_outline_probe_key(project, title, line_id, key)
        .map_err(|error| format!("browser dispatch failed: {error}"))?;
    let commit = wait_for_next_page_commit(&mut link, &before.id, COMMIT_TIMEOUT)?;
    print_commit(&commit);

    let after = client.get_page_in(project, title)?;
    print_page("after", &after);

    backend.idle();
    backend.shutdown();
    Ok(())
}

/// Wait for the next commit for this page. Cursor and other socket.io events
/// are ignored; no attempt is made to choose a commit by author because a
/// browser and this probe use the same account.
fn wait_for_next_page_commit(
    link: &mut ws::RoomLink,
    page_id: &str,
    timeout: Duration,
) -> Result<Value, Box<dyn std::error::Error>> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        match link.recv() {
            Recv::Packet(IoPacket::Event { name, data }) if name == "commit" => {
                if data.get("pageId").and_then(Value::as_str) == Some(page_id) {
                    return Ok(data);
                }
            }
            Recv::Packet(_) | Recv::Tick => {}
            Recv::Lost => {
                return Err("websocket connection was lost before a commit arrived".into())
            }
        }
    }
    Err(format!("no page commit arrived within {timeout:?}").into())
}

fn print_commit(data: &Value) {
    let changes = data.get("changes").cloned().unwrap_or(Value::Null);
    let ops = changes
        .as_array()
        .map(|entries| ws::parse_changes(entries))
        .unwrap_or_default();
    let parsed_ops: Vec<Value> = ops.iter().map(edit_op_json).collect();

    println!("\nwebsocket commit:");
    for field in ["id", "parentId", "pageId", "projectId", "userId", "kind"] {
        println!(
            "  {field}={:?}",
            data.get(field).and_then(Value::as_str).unwrap_or("")
        );
    }
    println!("  raw changes:");
    println!("{}", serde_json::to_string_pretty(&changes).unwrap());
    println!("  parsed ops:");
    println!("{}", serde_json::to_string_pretty(&parsed_ops).unwrap());
}

fn edit_op_json(op: &EditOp) -> Value {
    match op {
        EditOp::Insert { anchor, lines } => json!({
            "op": "insert",
            "anchor": anchor,
            "lines": lines.iter().map(|(id, text)| json!({"id": id, "text": text})).collect::<Vec<_>>(),
        }),
        EditOp::Replace { id, text } => json!({
            "op": "replace",
            "id": id,
            "text": text,
        }),
        EditOp::Delete { id } => json!({
            "op": "delete",
            "id": id,
        }),
    }
}

fn print_page(label: &str, page: &Page) {
    println!("\n{label} page:");
    println!("  title={:?}", page.title);
    println!("  page_id={:?}", page.id);
    println!("  commit_id={:?}", page.commit_id);
    println!("  lines={}", page.lines.len());
    for (index, line) in page.lines.iter().enumerate() {
        println!(
            "  [{index}] id={:?} text={:?} created={} updated={}",
            line.id, line.text, line.created, line.updated
        );
    }
}
