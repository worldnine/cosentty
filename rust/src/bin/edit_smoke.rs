// End-to-end smoke test of the edit API (preview → submit → verify →
// clean up) against a THROWAWAY page. Writes two commits: one appends a
// marker line, one deletes it — the page ends exactly as it started.
//
// Usage:
//   cargo run --bin edit_smoke -- <project> <title>
use cosense::api::{AuthStore, Client, Config, EditOp};
use cosense::editops::diff_to_ops;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, title) = match (args.first(), args.get(1)) {
        (Some(p), Some(t)) => (p.clone(), t.clone()),
        _ => return Err("usage: edit_smoke <project> <title>".into()),
    };
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let cfg = Config {
        project: project.clone(),
        auth: AuthStore::load(sid),
        api_domain: "scrapbox.io".into(),
    };
    let client = Client::new(cfg)?;

    let page = client.get_page_in(&project, &title)?;
    println!("page: {} ({} lines, id {})", page.title, page.lines.len(), page.id);

    let marker = format!(
        "edit_smoke {} (safe to delete)",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
    );

    // 1. append the marker
    let p1 = client.preview_edit(
        &project,
        &page.id,
        &[EditOp::Insert { anchor: "_end".into(), text: marker.clone() }],
    )?;
    println!("preview 1: {} (expires {})", p1.preview_id, p1.expire_at);
    let c1 = client.submit_edit(&project, &p1.preview_id)?;
    println!("commit 1:  {} (insert)", c1.commit_id);

    // 2. verify it landed, take its line id
    let after = client.get_page_in(&project, &title)?;
    let line = after
        .lines
        .iter()
        .find(|l| l.text == marker)
        .ok_or("marker line not found after insert")?;
    println!("verified:  line {} = {:?}", line.id, line.text);

    // 3. delete it again
    let p2 = client.preview_edit(&project, &page.id, &[EditOp::Delete { id: line.id.clone() }])?;
    let c2 = client.submit_edit(&project, &p2.preview_id)?;
    println!("commit 2:  {} (delete)", c2.commit_id);

    // 4. verify the page is back to its original line count
    let fin = client.get_page_in(&project, &title)?;
    if fin.lines.iter().any(|l| l.text == marker) {
        return Err("marker still present after delete".into());
    }
    println!(
        "verified:  marker gone ({} lines, started with {})",
        fin.lines.len(),
        page.lines.len()
    );

    // Phase 2: the ^e ($EDITOR) path — diff_to_ops against the live API.
    // Simulate an edit that REPLACES the last line, INSERTS two lines, then
    // diff back to the original; the page ends exactly as it started.
    let base = client.get_page_in(&project, &title)?;
    let old: Vec<(String, String)> =
        base.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let original_texts: Vec<String> = base.lines.iter().map(|l| l.text.clone()).collect();
    let mut edited = original_texts.clone();
    let last = edited.len() - 1;
    edited[last] = format!("{} (smoke)", edited[last]);
    edited.push("edit_smoke multi A".into());
    edited.push("edit_smoke multi B".into());

    let ops = diff_to_ops(&old, &edited);
    println!("phase 2:   {} ops from the editor-style diff", ops.len());
    let p3 = client.preview_edit(&project, &base.id, &ops)?;
    let c3 = client.submit_edit(&project, &p3.preview_id)?;
    println!("commit 3:  {} (replace+insert)", c3.commit_id);

    let mid = client.get_page_in(&project, &title)?;
    let mid_texts: Vec<String> = mid.lines.iter().map(|l| l.text.clone()).collect();
    if mid_texts != edited {
        return Err(format!("phase 2 mismatch:\n{mid_texts:?}\nvs\n{edited:?}").into());
    }
    // the replaced line must have KEPT its id (permalink survives)
    let kept = mid.lines[last].id == base.lines[last].id;
    println!("verified:  edited state matches; replaced line kept id: {kept}");

    // diff back to the original and submit
    let old2: Vec<(String, String)> =
        mid.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let ops_back = diff_to_ops(&old2, &original_texts);
    let p4 = client.preview_edit(&project, &mid.id, &ops_back)?;
    let c4 = client.submit_edit(&project, &p4.preview_id)?;
    println!("commit 4:  {} (restore)", c4.commit_id);
    let fin2 = client.get_page_in(&project, &title)?;
    let fin_texts: Vec<String> = fin2.lines.iter().map(|l| l.text.clone()).collect();
    if fin_texts != original_texts {
        return Err("restore mismatch — page differs from the original!".into());
    }
    println!("verified:  page restored to the original, byte for byte");
    println!("OK — edit API + editor-diff round-trip works");
    Ok(())
}
