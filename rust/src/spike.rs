// Phase 0 spike: prove inline images in a scrolling document work in ghostty.
// Risks under test:
//   1. ratatui-image detects the kitty protocol in ghostty and renders full-res.
//   2. Scrolling the image partially/fully off-screen does not leave ghost pixels.
//
// Controls: j/k or arrows = scroll, g = top, q = quit.

use std::error::Error;
use std::time::Duration;

use image::DynamicImage;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

// A vertical document made of stacked blocks.
enum Block {
    Text(String),
    Image { cells_w: u16, cells_h: u16 },
}

struct Doc {
    blocks: Vec<Block>,
}

impl Doc {
    // Height in rows of each block given a viewport width.
    fn block_height(&self, b: &Block) -> u16 {
        match b {
            Block::Text(_) => 1,
            Block::Image { cells_h, .. } => *cells_h,
        }
    }
    fn total_height(&self) -> u16 {
        self.blocks.iter().map(|b| self.block_height(b)).sum()
    }
}

struct App {
    doc: Doc,
    image: StatefulProtocol,
    scroll: u16, // rows scrolled from top of the virtual document
}

fn download_image(url: &str) -> Result<DynamicImage, Box<dyn Error>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("cosense-tui-spike")
        .build()?;
    let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
    let img = image::load_from_memory(&bytes)?;
    Ok(img)
}

fn main() -> Result<(), Box<dyn Error>> {
    // A known public gyazo image (PNG). Override with argv[1].
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://i.gyazo.com/da78df293f9e83a74b5402411e2f2e01.png".to_string());

    eprintln!("Downloading {url} ...");
    let dyn_img = download_image(&url)?;
    let (px_w, px_h) = (dyn_img.width(), dyn_img.height());

    let mut terminal = ratatui::init();
    // Must be called after entering the alternate screen (ratatui::init did that).
    let picker = Picker::from_query_stdio()?;
    let font = picker.font_size();

    // Compute the image's natural size in terminal cells, capped to a max width.
    let max_cols: u32 = 60;
    let nat_cols = (px_w as f32 / font.width as f32).ceil() as u32;
    let nat_rows = (px_h as f32 / font.height as f32).ceil() as u32;
    let (cells_w, cells_h) = if nat_cols > max_cols {
        let scale = max_cols as f32 / nat_cols as f32;
        (max_cols, ((nat_rows as f32) * scale).ceil() as u32)
    } else {
        (nat_cols, nat_rows)
    };

    let protocol_type = picker.protocol_type();
    let image = picker.new_resize_protocol(dyn_img);

    // Build a document: header text, the image, then lots of numbered lines,
    // so we can scroll the image off the top and check for ghosting.
    let mut blocks: Vec<Block> = Vec::new();
    blocks.push(Block::Text(format!(
        "SPIKE  protocol={:?}  font={}x{}  img={}x{}px -> {}x{} cells",
        protocol_type, font.width, font.height, px_w, px_h, cells_w, cells_h
    )));
    blocks.push(Block::Text("scroll: j/k or arrows   g: top   q: quit".into()));
    blocks.push(Block::Text("".into()));
    blocks.push(Block::Text("--- text above the image ---".into()));
    for i in 1..=4 {
        blocks.push(Block::Text(format!("above line {i}")));
    }
    blocks.push(Block::Image {
        cells_w: cells_w as u16,
        cells_h: cells_h as u16,
    });
    blocks.push(Block::Text("--- text below the image ---".into()));
    for i in 1..=40 {
        blocks.push(Block::Text(format!(
            "below line {i:02}  日本語混じりの行 (CJK width test) — scroll me"
        )));
    }

    let mut app = App {
        doc: Doc { blocks },
        image,
        scroll: 0,
    };

    let res = run(&mut terminal, &mut app);
    ratatui::restore();
    res
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
) -> Result<(), Box<dyn Error>> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let max_scroll = app.doc.total_height().saturating_sub(1);
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.scroll = (app.scroll + 1).min(max_scroll)
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.scroll = app.scroll.saturating_sub(1)
                    }
                    KeyCode::Char('g') => app.scroll = 0,
                    KeyCode::Char(' ') => {
                        app.scroll = (app.scroll + 10).min(max_scroll)
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // Clear the whole frame each draw so stale image graphics are removed.
    f.render_widget(Clear, area);

    let view_top = app.scroll as i32;
    let view_bottom = view_top + area.height as i32;

    let mut y_cursor: i32 = 0; // virtual y of the current block's top
    for b in &app.doc.blocks {
        let h = app.doc.block_height(b) as i32;
        let block_top = y_cursor;
        let block_bottom = y_cursor + h;
        y_cursor = block_bottom;

        // Skip blocks entirely outside the viewport.
        if block_bottom <= view_top || block_top >= view_bottom {
            continue;
        }

        // Screen y where this block's top lands.
        let screen_y = block_top - view_top;

        match b {
            Block::Text(s) => {
                if screen_y >= 0 && screen_y < area.height as i32 {
                    let rect = Rect::new(area.x, area.y + screen_y as u16, area.width, 1);
                    let style = if s.starts_with("SPIKE") {
                        Style::default().fg(Color::Black).bg(Color::Green)
                    } else if s.starts_with("---") {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    f.render_widget(Paragraph::new(Line::from(s.clone())).style(style), rect);
                }
            }
            Block::Image { cells_w, cells_h } => {
                // Only render the image when it is FULLY visible. This is the
                // spike's key check: when partially scrolled, we intentionally
                // hide it and verify no ghost remains (Clear above handles it).
                let fully_visible = screen_y >= 0 && (screen_y + *cells_h as i32) <= area.height as i32;
                if fully_visible {
                    let rect = Rect::new(
                        area.x,
                        area.y + screen_y as u16,
                        (*cells_w).min(area.width),
                        *cells_h,
                    );
                    let widget = StatefulImage::default().resize(Resize::Fit(None));
                    f.render_stateful_widget(widget, rect, &mut app.image);
                } else {
                    // Placeholder marker so the layout height is still felt.
                    let vis_top = screen_y.max(0);
                    let vis_bot = (screen_y + *cells_h as i32).min(area.height as i32);
                    if vis_bot > vis_top {
                        let rect = Rect::new(
                            area.x,
                            area.y + vis_top as u16,
                            area.width,
                            (vis_bot - vis_top) as u16,
                        );
                        f.render_widget(
                            Paragraph::new("[image hidden while partially scrolled]")
                                .style(Style::default().fg(Color::DarkGray)),
                            rect,
                        );
                    }
                }
            }
        }
    }
}
