// Headless verification of api.rs + render.rs against a real project.
//
// Usage:
//   cargo run --bin probe                      # defaults to help-jp
//   cargo run --bin probe -- <project>         # list + first page
//   cargo run --bin probe -- <project> "ページタイトル"   # render that page
//   COSENSE_SID=s:xxx cargo run --bin probe -- <project> "..."   # private
use cosense::api::{AuthStore, Client, Config};
use cosense::render::{render_lines, Block};

fn block_to_plain(b: &Block) -> String {
    match b {
        Block::Blank => "[BLANK]".to_string(),
        Block::Image { url, .. } => format!("[IMAGE {url}]"),
        Block::Inline { .. } => "[INLINE]".to_string(),
        Block::Text(line) => line.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
        Block::WebRender { kind, rows, last_src, .. } => format!(
            "[WEB {kind:?} last_src={last_src}]\n{}",
            rows.iter()
                .map(|(_, l)| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        ),
        Block::Table(t) => {
            // lay out at a generous width for probing
            t.layout(100)
                .iter()
                .map(|(l, _)| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project = args.get(0).cloned().unwrap_or_else(|| "help-jp".into());
    let page_title = args.get(1).cloned();
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());

    let cfg = Config { project: project.clone(), auth: AuthStore::load(sid), api_domain: "scrapbox.io".into() };
    let client = Client::new(cfg)?;

    println!("=== project: {project} ===");
    let (count, pages) = client.list_pages(20, 0, "updated")?;
    println!("LIST count={count}");
    for p in pages.iter().take(15) {
        println!("  - {}", p.title);
    }

    let title = match page_title {
        Some(t) => t,
        None => match pages.first() {
            Some(p) => {
                println!("\n(no title given; rendering most-recently-updated page)");
                p.title.clone()
            }
            None => return Ok(()),
        },
    };

    println!("\n=== RENDER: {title} ===");
    let page = client.get_page(&title)?;
    let out = render_lines(&page.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
    for b in &out.blocks {
        println!("{}", block_to_plain(b));
    }
    println!("\n--- links ({}) ---", out.extracted.links.len());
    println!("{:?}", out.extracted.links.iter().take(20).collect::<Vec<_>>());
    println!("--- images ({}) ---", out.extracted.images.len());
    for u in out.extracted.images.iter().take(10) {
        println!("  {u}");
    }
    Ok(())
}
