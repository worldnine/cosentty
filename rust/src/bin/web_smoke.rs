// Live check of the headless-Chrome web renderer against a real Cosense page.
// NOT part of `cargo test`: it launches a browser and hits the network.
//
//   cargo run --bin web_smoke -- help-jp Mermaid
//   COSENSE_SID=s:xxx cargo run --bin web_smoke -- <sandbox> テスト
//
// Prints, per Mermaid block: the line id, the selector, the PNG size and the
// wall time; PNGs land in $TMPDIR/cosense-web-smoke/.
use cosense::api::{AuthStore, Client, Config};
use cosense::capability::RenderCapability;
use cosense::chrome::ChromeBackend;
use cosense::render::{render_lines, Block};
use cosense::webrender::{hash_code, ArtifactCache, WebBackend, WebRequest};

/// Is a pid still alive? `kill(pid, 0)` probes without signalling it.
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--twice` runs the batch a second time through the SAME backend, which
    // is how the warm-browser path gets exercised: the second number is what
    // a re-render (an edited diagram, a pane resize) actually costs.
    let twice = args.iter().any(|a| a == "--twice");
    args.retain(|a| a != "--twice");
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
        let Block::Artifact {
            kind,
            code,
            last_src,
            ..
        } = b
        else {
            continue;
        };
        // Only the kinds the browser draws. Math is text-or-source, so it
        // has no selector to smoke-test.
        let Some(kind) = kind.web() else { continue };
        let line_id = page.lines[*last_src].id.clone();
        println!(
            "  block last_src={last_src} line_id={line_id} selector={}",
            kind.selector(&line_id)
        );
        reqs.push(WebRequest {
            kind,
            project: project.clone(),
            title: title.clone(),
            page_id: page.id.clone(),
            line_id,
            code_hash: hash_code(code),
            dark: true,
        });
    }
    if reqs.is_empty() {
        println!("no mermaid blocks on this page");
        return Ok(());
    }

    // The smoke renders as the session actually would: with the cookie when
    // one is configured, anonymously when not — which is exactly the public
    // fallback path.
    let auth = if sid.is_some() {
        RenderCapability::Authenticated
    } else {
        RenderCapability::Anonymous
    };
    println!(
        "browser auth: {}",
        if sid.is_some() {
            "session cookie"
        } else {
            "anonymous"
        }
    );
    let backend = ChromeBackend::detect(sid).ok_or("no Chrome found (set COSENSE_CHROME)")?;
    let dir = std::env::temp_dir().join("cosense-web-smoke");
    std::fs::create_dir_all(&dir)?;
    let t0 = std::time::Instant::now();
    let results = backend.render_batch(&reqs, auth);
    let elapsed = t0.elapsed();
    if twice {
        let t1 = std::time::Instant::now();
        let again = backend.render_batch(&reqs, auth);
        let ok = again.iter().filter(|r| r.is_ok()).count();
        println!(
            "second batch (warm browser): {ok}/{} ok in {:?}",
            again.len(),
            t1.elapsed()
        );
    }
    // Route successes through the real artifact cache, exactly as the
    // viewer's render worker does, so a run also exercises (and lets the
    // operator inspect) the on-disk cache and its permissions.
    let cache = ArtifactCache::new();
    println!("artifact cache: {}", cache.dir().display());
    let mut failed: Vec<String> = Vec::new();
    for (req, res) in reqs.iter().zip(results) {
        match res {
            Ok(png) => {
                cache.put(&req.cache_key(), &png);
                let path = dir.join(format!("{}.png", req.line_id));
                std::fs::write(&path, &png)?;
                let dim = image::load_from_memory(&png)
                    .map(|i| format!("{}x{}", i.width(), i.height()))
                    .unwrap_or_else(|e| format!("undecodable: {e}"));
                println!(
                    "  OK  {} -> {} {dim} ({} bytes)",
                    req.line_id,
                    path.display(),
                    png.len()
                );
            }
            Err(e) => {
                println!("  ERR {} -> {e}", req.line_id);
                failed.push(format!("{}: {e}", req.line_id));
            }
        }
    }
    println!("batch of {} in {:?}", reqs.len(), elapsed);

    // Lifecycle check against THIS backend's own browser — not every Chrome
    // on the machine, which says nothing about whether we leaked.
    let owned = backend.last_owned();
    match &owned {
        Some((pid, dir)) => println!("owned browser: pid {pid}, profile {}", dir.display()),
        None => println!("owned browser: none was started"),
    }
    backend.idle();
    backend.shutdown();
    let mut leaked: Vec<String> = Vec::new();
    if let Some((pid, dir)) = owned {
        // Chrome's helper processes outlive the SIGKILL on their parent by a
        // moment, and hold files open in the profile until they go.
        for _ in 0..60 {
            if !alive(pid) && !dir.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if alive(pid) {
            leaked.push(format!("browser pid {pid} still running"));
        }
        if dir.exists() {
            leaked.push(format!("profile {} still present", dir.display()));
        }
    }
    println!("after shutdown: owned browser gone = {}", leaked.is_empty());

    if !failed.is_empty() || !leaked.is_empty() {
        for f in &failed {
            eprintln!("FAILED diagram {f}");
        }
        for l in &leaked {
            eprintln!("LEAKED {l}");
        }
        std::process::exit(1);
    }
    Ok(())
}
