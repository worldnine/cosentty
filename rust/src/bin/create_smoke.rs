// End-to-end smoke test of PAGE CREATION against a throwaway title:
// the same request the viewer sends when a page does not exist yet
// (preview with NO pageId, then submit), followed by a read-back.
//
// The page is left behind on purpose — deleting it needs previewDelete,
// and seeing the result is the point. Use a disposable title.
//
// Usage:
//   cargo run --bin create_smoke -- <project> <title>
use cosense::api::{AuthStore, Client, Config, EditOp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, title) = match (args.first(), args.get(1)) {
        (Some(p), Some(t)) => (p.clone(), t.clone()),
        _ => return Err("usage: create_smoke <project> <title>".into()),
    };
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let cfg = Config {
        project: project.clone(),
        auth: AuthStore::load(sid),
        api_domain: "scrapbox.io".into(),
    };
    let client = Client::new(cfg)?;

    let before = client.get_page_in(&project, &title)?;
    println!(
        "before: persistent={} id={:?} lines={}",
        before.persistent,
        before.id,
        before.lines.len()
    );
    if before.persistent {
        return Err("that page already exists — use a disposable title".into());
    }

    // Exactly what `dispatch_create` builds: the whole page as one insert
    // at `_end`, with client-generated line ids and no pageId.
    let lines: Vec<(String, String)> = vec![
        (new_id(0), title.clone()),
        (new_id(1), String::new()),
        (new_id(2), "create_smoke body 1".into()),
        (new_id(3), "create_smoke body 2".into()),
    ];
    let ops = vec![EditOp::Insert {
        anchor: "_end".into(),
        lines: lines.clone(),
    }];

    let preview = client.preview_edit(&project, "", &ops)?;
    println!(
        "preview: {} (expires {})",
        preview.preview_id, preview.expire_at
    );
    println!("preview title: {:?}", preview.title);
    for l in &preview.lines {
        println!("  preview line: {:?}", l);
    }
    let commit = client.submit_edit(&project, &preview.preview_id)?;
    println!("commit:  {} title={:?}", commit.commit_id, commit.title);

    let after = client.get_page_in(&project, &title)?;
    println!(
        "after:  persistent={} id={:?} lines={}",
        after.persistent,
        after.id,
        after.lines.len()
    );
    for l in &after.lines {
        println!("  {} {:?}", l.id, l.text);
    }
    Ok(())
}

/// A 24-hex client line id, shaped like the viewer's `new_line_id`.
fn new_id(n: u8) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{:016x}{:06x}{:02x}",
        now as u64,
        std::process::id() & 0xff_ffff,
        n
    )
}
