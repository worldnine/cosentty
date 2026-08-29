// Live check of the headless-Chrome web renderer against a real Cosense page.
// NOT part of `cargo test`: it launches a browser and hits the network.
//
//   cargo run --bin web_smoke -- help-jp Mermaid
//   COSENSE_SID=s:xxx cargo run --bin web_smoke -- my-sandbox テスト
//
// Prints, per Mermaid block: the line id, the selector, the PNG size and the
// wall time; PNGs land in $TMPDIR/cosense-web-smoke/.
use cosense::api::{AuthStore, Client, Config};
use cosense::chrome::ChromeBackend;
use cosense::render::{render_lines, Block};
use cosense::webrender::{hash_code, WebBackend, WebRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project = args.first().cloned().unwrap_or_else(|| "help-jp".into());
    let title = args.get(1).cloned().unwrap_or_else(|| "Mermaid".into());

    let auth = AuthStore::load(std::env::var("COSENSE_SID").ok());
    let sid = auth.sid().map(str::to_string);
    let client = Client::new(Config {
        project: project.clone(),
        auth,
        api_domain: "scrapbox.io".into(),
    })?;
    let page = client.get_page_in(&project, &title)?;
    println!(
        "page {}/{} id={} commit={} lines={} sid={}",
        project,
        title,
        page.id,
        page.commit_id,
        page.lines.len(),
        if sid.is_some() { "yes" } else { "no" }
    );

    let texts: Vec<String> = page.lines.iter().map(|l| l.text.clone()).collect();
    let out = render_lines(&texts);
    let mut reqs: Vec<WebRequest> = Vec::new();
    for b in &out.blocks {
        let Block::WebRender { kind, code, last_src, .. } = b else { continue };
        let line_id = page.lines[*last_src].id.clone();
        println!(
            "  block last_src={last_src} line_id={line_id} selector={}",
            kind.selector(&line_id)
        );
        reqs.push(WebRequest {
            kind: *kind,
            project: project.clone(),
            title: title.clone(),
            page_id: page.id.clone(),
            line_id,
            code_hash: hash_code(code),
            width_px: 880,
            dark: true,
        });
    }
    if reqs.is_empty() {
        println!("no mermaid blocks on this page");
        return Ok(());
    }

    let backend = ChromeBackend::detect(sid).ok_or("no Chrome found (set COSENSE_CHROME)")?;
    let dir = std::env::temp_dir().join("cosense-web-smoke");
    std::fs::create_dir_all(&dir)?;
    let t0 = std::time::Instant::now();
    let results = backend.render_batch(&reqs);
    let elapsed = t0.elapsed();
    for (req, res) in reqs.iter().zip(results) {
        match res {
            Ok(png) => {
                let path = dir.join(format!("{}.png", req.line_id));
                std::fs::write(&path, &png)?;
                let dim = image::load_from_memory(&png)
                    .map(|i| format!("{}x{}", i.width(), i.height()))
                    .unwrap_or_else(|e| format!("undecodable: {e}"));
                println!("  OK  {} -> {} {dim} ({} bytes)", req.line_id, path.display(), png.len());
            }
            Err(e) => println!("  ERR {} -> {e}", req.line_id),
        }
    }
    println!("batch of {} in {:?}", reqs.len(), elapsed);
    backend.shutdown();
    Ok(())
}
