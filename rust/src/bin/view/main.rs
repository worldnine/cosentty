// Single-page viewer (akapen-style): richly rendered read view with a
// persistent line cursor, in-place range selection, inline comment cards,
// and agent-actionable comment export.
//
// Message language: prose the reader is meant to READ — status line, key
// hints, help, errors — is Japanese. What stays as written are the things
// that are not words in a language but labels of something: key names
// (Enter, Esc, ^z, j/k), flags and env vars (--preview, COSENSE_SID),
// notation (`code:mmd`), protocol and product names (websocket, OSC 52,
// Cosense, Gyazo, Chrome). The rule exists because the footer shows hints
// and status in the SAME row: with two languages sharing that row, and
// sometimes one string (`⌫@行頭 join`), the seam showed on every frame.
//
// Keys:
//   j/k ↑/↓  move line cursor        Space/b  page down/up
//   g/G       top/bottom             v        start/stop range selection
//   c         write comment on cursor/selection
//   y         copy all comments (clipboard)   D  delete all comments
//   q         quit (comments also printed to stdout)
//   Enter/f   follow the line's link: a page navigates, an uploaded file
//             is saved to the download dir and opened, an http(s) URL (↗)
//             opens in the browser (a gyazo image → its gyazo page)
//   wheel     scroll the viewport only (the cursor keeps its line)
//   click     open a link under the pointer, otherwise move the cursor;
//             a double-click enters EDIT at the character clicked, and
//             only there (no selection): in EDIT a double-click takes a
//             word, a triple the line
//   drag      select a range (READ: whole lines; EDIT: characters that
//             may cross lines — the caret follows the pointer, the
//             anchor holds where the button came down)
//   scrollbar click the track to jump, drag the thumb to scrub
//
// The cursor addresses SOURCE lines (akapen's ViewState model: `cursor` is a
// source line number, and the rows it occupies are looked up per frame via
// render's per-block source attribution). Comments carry the exact line
// text + Cosense line-ID deep link, so an agent can edit_lines verbatim.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cosense::api::{new_line_id, AuthStore, Client, Config, EditError, EditOp, PageLine};
use cosense::{t, ts};
use cosense::capability::{self, RenderCapability, SyncState};
use cosense::editops::{apply_ops, diff_to_ops, invert_ops};
use cosense::ws::{self, RemoteCommit, WsEvent};
use cosense::comment::{format_all, Comment, Selection};
use cosense::image_fetch::ImageFetcher;
use cosense::outline::{Destination, Direction as OutlineDirection, LineRange, Plan as OutlinePlan, PlanError, Scope as OutlineScope};
use cosense::webrender::{ArtifactCache, WebBackend, WebError, WebRequest};
use cosense::highlight::Highlighter;
use cosense::render::{
    bullet_indent_width, file_name_of_url, gyazo_permalink, is_scrapbox_file_url,
    render_lines_with, Block, CodeSpan, LinkTruth,
};
use cosense::wrap::{hanging_prefix, wrap_line, wrap_line_parts};

use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

const SEL_BG: Color = Color::DarkGray;
/// The list marker, in the one place both the drawing and its tests read.
const BULLET: &str = "\u{2022}";
/// What a TAB looks like on the caret line: one column, so the cells it
/// separates stay apart and the caret has somewhere to be.
const TAB_MARK: &str = "|";
/// Source mode's line-number gutter: `"  12 "` (4 digits + space).
const SOURCE_NUM_W: usize = 5;

/// How many pages the index asks for in one request. Cosense's own list
/// pages in hundreds; beyond this the index says how many it is not
/// showing rather than pretending to be complete.
const INDEX_PAGE_LIMIT: u32 = 500;

/// How long the index's scrollbar stays up after the last scroll.
const INDEX_BAR_LINGER: Duration = Duration::from_millis(900);

/// How often the page is scanned for links whose fate is unknown.
const LINK_SCAN_EVERY: Duration = Duration::from_millis(400);

/// How many of this viewer's own commit ids are remembered, waiting for
/// their websocket echo (see `App::own_commits`).
const OWN_COMMIT_MEMORY: usize = 256;
/// Cursor-row highlight inside the page body.
const CURSOR_BG: Color = Color::DarkGray;
const CARD_BG: Color = Color::Black;

// Non-content chrome uses ANSI palette entries, never fixed RGB. Terminal
// themes own the actual values behind these role colors.
const CHROME_ACCENT: Color = Color::Cyan;
const CHROME_ACTIVE: Color = Color::Green;
const CHROME_CARET: Color = Color::LightBlue;
const CHROME_DIM: Color = Color::DarkGray;
const CHROME_SCROLL: Color = Color::Gray;

struct ImageInfo {
    /// Sliced protocol: renders row-by-row so partial vertical scroll clips
    /// cleanly (SignedPosition.y may be negative) instead of vanishing.
    sliced: SlicedProtocol,
    /// Display height in cells (from the sliced size), used for layout.
    cells_h: u16,
    /// Display width in cells, and the column cap this was encoded for. A
    /// diagram is re-encoded from its cached PNG when the pane crosses that
    /// cap, so it never paints past the text column (the draw clips to the
    /// pane, so an over-wide image simply loses its right-hand side).
    cells_w: u16,
    built_for: u16,
}

/// Widest an inline image may be drawn, in cells. Images are capped at a
/// fixed 64 columns regardless of the pane — that is long-standing
/// behaviour and is left alone. A DIAGRAM additionally has to fit the pane:
/// a clipped photo is still a photo, a clipped flowchart has lost its right
/// half.
const IMAGE_MAX_COLS: u16 = 64;

/// Column cap for a diagram in a pane whose text area is `text_w` wide.
fn diagram_max_cols(text_w: u16) -> u16 {
    text_w.min(IMAGE_MAX_COLS).max(1)
}

/// Rows reserved for an image that is still downloading. The real height
/// replaces it (and the layout is rebuilt) once the image arrives.
const IMAGE_PLACEHOLDER_H: u16 = 8;
/// The tallest a picture may be drawn, in rows. Beyond this the reader is
/// scrolling through one image instead of reading a page; the picture is
/// scaled down (keeping its shape) so the text around it stays reachable.
const MAX_IMAGE_ROWS: u16 = 20;
/// …and how wide, while a mixed line is being laid out without it.
const IMAGE_PLACEHOLDER_W: u16 = 24;

/// A finished background image load: the image already resized and encoded
/// for the terminal (the expensive part — done on the worker so the UI
/// never stalls while a page's images arrive), or why it failed.
type ImageMsg = (String, Result<ImageInfo, String>);

/// A finished background file download: the label shown to the user and
/// where it landed, or why it failed.
type FileMsg = (String, Result<std::path::PathBuf, String>);

/// One batch of web renders: everything on ONE page, so the worker can serve
/// them with a single browser navigation. `gen` is the App's page generation
/// at the time of the request.
enum WebJob {
    /// Draw these in a browser (or serve them from the disk cache), encoded
    /// for a pane whose text area is `max_cols` wide.
    Render {
        gen: u64,
        reqs: Vec<WebRequest>,
        max_cols: u16,
        /// The source epoch this batch was built from. Re-checked on the
        /// worker before the browser is asked and again before anything is
        /// written to the cache.
        src_epoch: u64,
        /// `Some` lets the batch reach a browser, in that credential state.
        /// `None` is cache-only: serve what is on disk and answer every
        /// miss with `Missing`. That is the default page-load pass, and it
        /// is also what a project we may not render gets.
        auth: Option<RenderCapability>,
    },
    /// Re-encode an artifact already on disk at a new column cap, because
    /// the pane was resized. No browser is involved: this is a decode plus
    /// a resize, done on the worker so the UI thread never stalls on it.
    Rescale { gen: u64, key: String, max_cols: u16 },
    /// Quit. Sent once, on the way out, so the worker can be joined.
    Stop,
}

impl WebJob {
    /// The page generation this job belongs to. `Stop` belongs to none.
    fn gen(&self) -> Option<u64> {
        match self {
            WebJob::Render { gen, .. } | WebJob::Rescale { gen, .. } => Some(*gen),
            WebJob::Stop => None,
        }
    }
}

/// A finished web render, already decoded and protocol-encoded on the
/// worker. `gen` and `key` together decide whether it is still wanted.
struct WebMsg {
    gen: u64,
    key: String,
    /// True when this answers a `Rescale`. A failed rescale is not a failed
    /// diagram: the artifact already on screen stays, and the reader is
    /// told nothing.
    rescale: bool,
    /// What this reply was produced with. A refusal has to be attributed to
    /// a credential state, not guessed from session flags: one browser
    /// batch refuses EVERY key in it at once, and the whole batch has to be
    /// judged as the single event it was.
    attempted: Option<RenderCapability>,
    res: WebOutcome,
}

/// What became of one request. Typed because the three failures are NOT
/// interchangeable: a cache miss must leave the artifact renderable later,
/// a refusal must retune the session's capabilities, and only a genuine
/// failure is worth telling the reader about.
enum WebOutcome {
    Drawn(ImageInfo),
    /// Nothing on disk, and this pass was not allowed to draw. Not an
    /// error: it must not land in `web_errors`, or `R` could never render it.
    Missing,
    /// The browser was refused (login wall). The session decides what to do
    /// with that — it says nothing about the REST credential.
    Denied,
    Failed(String),
    /// The page source moved while this was in flight, so the picture (if
    /// there is one) describes different text than the key naming it. The
    /// block simply goes back to source: it is NOT a miss and NOT a failure,
    /// and the pass that follows the new source picks it up.
    Stale,
}

impl WebOutcome {
    /// Did this produce a picture?
    #[cfg(test)]
    fn is_drawn(&self) -> bool {
        matches!(self, WebOutcome::Drawn(_))
    }
}

/// The web-render worker: the ONLY thread that talks to a browser. It takes
/// whole batches, answers the on-disk cache without launching anything, and
/// hands back terminal-ready images. The UI thread never waits on it — it
/// only sends and later drains.
fn spawn_web_worker(
    jobs: mpsc::Receiver<WebJob>,
    out: mpsc::Sender<WebMsg>,
    backend: Arc<dyn WebBackend>,
    picker: Picker,
    // `current_gen` is the App's page generation: work queued for a page the
    // reader has already left is dropped BEFORE it costs a decode or a
    // browser navigation.
    cache: ArtifactCache,
    current_gen: Arc<std::sync::atomic::AtomicU64>,
    // The App's source epoch: a render whose source has moved is discarded
    // rather than written to the cache under a key it no longer matches.
    current_src: Arc<std::sync::atomic::AtomicU64>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Idle window. The backend keeps a browser warm between batches (a
        // re-render in a warm browser is roughly twice as fast), but a
        // viewer nobody is editing should not hold a browser process.
        // 15 s is short enough that a reader who has finished with a page
        // is not paying for a resident Chrome, and long enough to cover the
        // pause between "the diagram appeared" and "I edited it again".
        let idle = idle_window();
        let reap_at_once = idle.is_zero();
        let live = |gen: u64| gen == current_gen.load(std::sync::atomic::Ordering::SeqCst);
        loop {
            let first = match jobs.recv_timeout(if reap_at_once {
                // A zero window still needs a real block here; the browser
                // is already gone (reaped below), so waking is free.
                Duration::from_secs(3600)
            } else {
                idle
            }) {
                Ok(job) => job,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    backend.idle();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            // Take everything already queued behind it. A burst of jobs for
            // the page the reader has moved on from must not make the
            // current page wait through several browser navigations.
            let mut batch = vec![first];
            let mut stop = false;
            while let Ok(job) = jobs.try_recv() {
                batch.push(job);
            }
            // Stale-generation jobs are never ACCEPTED, so they owe no
            // reply: `set_page` clears `web_pending`/`web_rescaling` for the
            // page being left, which is what keeps this from leaking. See
            // `set_page_clears_the_state_a_dropped_job_would_have_answered`.
            batch.retain(|job| match job.gen() {
                None => {
                    stop = true;
                    false
                }
                Some(g) => live(g),
            });

            // Every same-generation render is merged into ONE batch, so a
            // page's diagrams still cost a single navigation however many
            // passes queued them.
            let mut merged: Vec<WebRequest> = Vec::new();
            let mut merged_cols: Option<u16> = None;
            let mut merged_auth: Option<RenderCapability> = None;
            let mut merged_src = 0u64;
            let mut merged_gen = 0u64;
            for job in batch {
                match job {
                    WebJob::Stop => {}
                    WebJob::Rescale { gen, key, max_cols } => {
                        // A rescale is answered from the disk cache. If the
                        // PNG is gone, the answer is an error so the viewer
                        // stops waiting — it keeps the size it has.
                        let res = match cache.get(&key) {
                            Some(png) => match decode_web_png(&picker, &png, max_cols) {
                                Ok(info) => WebOutcome::Drawn(info),
                                Err(e) => WebOutcome::Failed(e),
                            },
                            None => WebOutcome::Failed(t!("作り直せる図はありません", "no cached artifact to resize")),
                        };
                        let _ = out.send(WebMsg { gen, key, rescale: true, attempted: None, res });
                    }
                    WebJob::Render { gen, src_epoch, reqs, max_cols, auth } => {
                        // Freshness is judged PER JOB, before merging. The
                        // checks around the browser below are per batch, so
                        // a job built against the old source that merged
                        // with a fresh one would ride in on its freshness —
                        // and be filed under a hash it no longer matches.
                        if src_epoch
                            != current_src.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            for req in &reqs {
                                let _ = out.send(WebMsg {
                                    gen,
                                    key: req.cache_key(),
                                    rescale: false,
                                    attempted: None,
                                    res: WebOutcome::Stale,
                                });
                            }
                            continue;
                        }
                        merged_gen = gen;
                        merged_src = src_epoch;
                        merged_cols = Some(max_cols);
                        // Coalescing several passes: the most permissive
                        // one wins, so an explicit `R` arriving behind a
                        // page-load probe is not silently downgraded to
                        // cache-only.
                        merged_auth = merged_auth.or(auth);
                        for req in reqs {
                            if !merged.iter().any(|r| r.cache_key() == req.cache_key()) {
                                merged.push(req);
                            }
                        }
                    }
                }
            }

            if let Some(max_cols) = merged_cols {
                run_render_batch(
                    &*backend,
                    &picker,
                    &cache,
                    &out,
                    merged_gen,
                    merged_src,
                    &current_src,
                    merged,
                    max_cols,
                    merged_auth,
                );
            }
            if reap_at_once {
                // `COSENSE_WEB_IDLE_SECS=0`: hold no browser between
                // batches at all. Every page then pays the cold-start cost.
                backend.idle();
            }
            if stop {
                break;
            }
        }
    })
}

/// How long a browser is kept warm between batches.
/// `COSENSE_WEB_IDLE_SECS`, clamped to 0..=300; 0 reaps after every batch.
fn idle_window() -> Duration {
    let secs = std::env::var("COSENSE_WEB_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s <= 300)
        .unwrap_or(15);
    Duration::from_secs(secs)
}

/// Serve one page's worth of requests: the disk cache first, the browser for
/// whatever is left. EVERY request gets exactly one reply, including cache
/// misses and decode failures — a request with no reply would leave the
/// viewer pulsing a diagram forever.
fn run_render_batch(
    backend: &dyn WebBackend,
    picker: &Picker,
    cache: &ArtifactCache,
    out: &mpsc::Sender<WebMsg>,
    gen: u64,
    // The source this batch was built from. A render captures what the
    // SERVER shows when the browser looks, so if the source has moved since,
    // the picture belongs to different text than the key naming it.
    src_epoch: u64,
    current_src: &std::sync::atomic::AtomicU64,
    reqs: Vec<WebRequest>,
    max_cols: u16,
    // `None` means this pass may not launch a browser: hits are served,
    // misses are answered `Missing`.
    auth: Option<RenderCapability>,
) {
    let mut to_render: Vec<WebRequest> = Vec::new();
    for req in reqs {
        let key = req.cache_key();
        match cache.get(&key).map(|png| decode_web_png(picker, &png, max_cols)) {
            Some(Ok(info)) => {
                let _ = out.send(WebMsg {
                    gen,
                    key,
                    rescale: false,
                    attempted: None, // served from disk; no credential was used
                    res: WebOutcome::Drawn(info),
                });
            }
            // On disk but unreadable: truncated by a crash, or corrupted
            // underneath us. That must not become a permanent failure for
            // this artifact — drop the entry and let the browser produce
            // it again.
            Some(Err(_)) => {
                cache.remove(&key);
                to_render.push(req);
            }
            None => to_render.push(req),
        }
    }
    if to_render.is_empty() {
        return;
    }
    let fresh = || src_epoch == current_src.load(std::sync::atomic::Ordering::SeqCst);
    let Some(auth) = auth else {
        // Cache-only pass. Every miss still owes exactly one reply, or the
        // viewer would pulse those blocks forever.
        for req in &to_render {
            let key = req.cache_key();
            let _ = out.send(WebMsg {
                gen,
                key,
                rescale: false,
                attempted: None,
                res: WebOutcome::Missing,
            });
        }
        return;
    };
    // Checked here, before the browser is asked: the source may have moved
    // while this job sat in the queue behind another batch.
    if !fresh() {
        for req in &to_render {
            let key = req.cache_key();
            let _ = out.send(WebMsg {
                gen,
                key,
                rescale: false,
                attempted: None,
                res: WebOutcome::Stale,
            });
        }
        return;
    }
    let results = backend.render_batch(&to_render, auth);
    // ...and again here. A page of diagrams takes seconds to draw, which is
    // exactly long enough for a commit to land underneath it. Writing that
    // PNG under this key would poison the cache for a week.
    let still_fresh = fresh();
    for (req, res) in to_render.iter().zip(results) {
        let key = req.cache_key();
        let res = match res {
            _ if !still_fresh => WebOutcome::Stale,
            Ok(png) => match decode_web_png(picker, &png, max_cols) {
                Ok(info) => {
                    cache.put(&key, &png);
                    WebOutcome::Drawn(info)
                }
                Err(e) => WebOutcome::Failed(e),
            },
            Err(WebError::NotAuthorized) => WebOutcome::Denied,
            Err(e) => WebOutcome::Failed(e.to_string()),
        };
        let _ = out.send(WebMsg { gen, key, rescale: false, attempted: Some(auth), res });
    }
}

/// PNG bytes -> terminal image. Only image decoding happens here: no SVG or
/// HTML from the browser is ever interpreted in this process.
fn decode_web_png(picker: &Picker, png: &[u8], max_cols: u16) -> Result<ImageInfo, String> {
    let img = image::load_from_memory(png).map_err(|e| e.to_string())?;
    build_image(picker, img, max_cols)
}

/// Something Enter/f can act on from the cursor line.
#[derive(Clone, Debug, PartialEq)]
enum LinkItem {
    /// An internal page link (`[Page]`, `#tag`) in the current project.
    Page(String),
    /// A cross-project link (`[/project/Page]`): the viewer moves into that
    /// project, as the browser does.
    ProjectPage { project: String, title: String },
    /// A link to a whole project (`[/project]`), which on Cosense is its
    /// home screen. Here that is the project's index.
    ProjectIndex { project: String },
    /// An uploaded file (`[name https://scrapbox.io/files/…pdf]`).
    File { label: String, url: String },
    /// Any other http(s) URL (`[title https://…]`, bare URL) — the browser's.
    Url { label: String, url: String },
    /// A `code:` or `table:` block, saved from the page in hand.
    ///
    /// Cosense does serve both as files, but only to a session cookie:
    /// `/api/code/…` answers 401 to the token this viewer authenticates
    /// with. The page is already here, so the file is written from it — no
    /// second credential, no round trip, and what lands is exactly what is
    /// on screen.
    Export { label: String, src: usize, csv: bool },
}

impl LinkItem {
    fn label(&self) -> String {
        match self {
            LinkItem::Page(t) => t.clone(),
            LinkItem::ProjectPage { project, title } => format!("/{project}/{title}"),
            LinkItem::ProjectIndex { project } => format!("/{project}"),
            LinkItem::File { label, .. } => format!("↓ {label}"),
            LinkItem::Export { label, .. } => format!("↓ {label}"),
            LinkItem::Url { label, .. } => format!("↗ {label}"),
        }
    }

}

/// One section of the related-pages list rendered below the page body
/// (scrapbox.io's "Links" / per-hub 2-hop groups / "External links").
struct RelSection {
    heading: String,
    entries: Vec<RelEntry>,
}

/// One related page: where Enter goes, and what the row shows.
struct RelEntry {
    item: LinkItem,
    title: String,
    /// First description line (the page's own first body line), dimmed.
    desc: String,
    /// `updated` epoch seconds (0 = unknown, no age shown).
    age: i64,
    /// Two-state telomere for related rows. RelatedPages has no per-user
    /// lastAccessed, so this is based on this TUI's persisted visit record.
    unread: bool,
}

/// A laid-out visual row. Content rows carry their source line; card rows
/// are synthesized (not selectable, no source). `FrameEnd` is the explicit
/// boundary between the boxed page body and the unboxed related sections.
enum Row {
    /// A rendered row. `start`/`hang` say where it sits in the line it was
    /// wrapped out of: `start` is the display column of its first
    /// character along the unwrapped line, `hang` the indent it carries in
    /// front of that. A row that is nobody's continuation has both zero.
    /// This is what turns a click back into a position in the source
    /// line's rendering — see `App::link_at_screen_position`.
    Line { line: Line<'static>, src: usize, start: usize, hang: usize },
    Blank { src: usize },
    /// A picture on a line of its own. `indent`: the display column it
    /// starts at, so it lines up with the text of its level. `item`: the
    /// picture IS the list item (it wears the bullet) rather than hanging
    /// under a line of text.
    Image { url: String, height: u16, src: usize, indent: usize, item: bool },
    /// A line of text and pictures laid out together (see `layout_inline`):
    /// every part carries its own row and column inside the block.
    Inline {
        src: usize,
        height: u16,
        /// The column its level's text starts at, and whether that level
        /// makes it a list item (it then wears a bullet at its top-left,
        /// like every other item).
        indent: usize,
        item: bool,
        images: Vec<(u16, u16, String)>,
        texts: Vec<(u16, u16, Line<'static>)>,
    },
    /// Space reserved for an image still downloading in the background.
    ImageLoading { src: usize, indent: usize, item: bool },
    ImageError { msg: String, src: usize, indent: usize, item: bool },
    Card { line: Line<'static> },
    FrameEnd,
}

impl Row {
    fn height(&self) -> u16 {
        match self {
            Row::Image { height, .. } | Row::Inline { height, .. } => *height,
            Row::ImageLoading { .. } => IMAGE_PLACEHOLDER_H,
            _ => 1,
        }
    }
    fn src(&self) -> Option<usize> {
        match self {
            Row::Line { src, .. }
            | Row::Blank { src }
            | Row::Image { src, .. }
            | Row::Inline { src, .. }
            | Row::ImageLoading { src, .. }
            | Row::ImageError { src, .. } => Some(*src),
            Row::Card { .. } | Row::FrameEnd => None,
        }
    }
}

/// View shows rendered Scrapbox notation; Source shows the raw lines with
/// numbers. Both address the same source lines, so the cursor, comments, and
/// telomere carry across the toggle (akapen's `Tab view⇄source`).
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    View,
    Source,
}

/// Somewhere the reader has been. An index is not merely its project name:
/// its filter, cursor, scroll and focused excerpt are the choice the reader
/// was making. Keeping the model makes browser-style back instant and exact
/// — no refetch and no jump back to the first row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Page { project: String, title: String },
    Index { project: String, state: Box<cosense::index::Index> },
}

impl Place {
    fn label(&self) -> String {
        match self {
            Place::Page { title, .. } => title.clone(),
            Place::Index { project, .. } => format!("/{project}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeaderColors {
    fg: Color,
    bg: Color,
}

impl HeaderColors {
    fn fallback() -> Self {
        Self { fg: Color::Black, bg: CHROME_ACCENT }
    }
}

/// The pre-action model one structural action can be rolled back to.
#[derive(Clone)]
struct OutlineSnapshot {
    project: String,
    page_id: String,
    /// Page-install generation, not the commit-queue generation. Navigating
    /// away and back to the same immutable page id is still a different
    /// installation whose lines may predate this commit.
    install_gen: u64,
    lines: Vec<PageLine>,
    cursor: usize,
    selection: Option<Selection>,
    undo_stack: Vec<(String, Vec<EditOp>)>,
    redo_stack: Vec<(String, Vec<EditOp>)>,
    history_dropped: bool,
}

/// The one outstanding structural action: which commit job decides its
/// fate, and what to put back if that job does not land. The job id is what
/// identifies the outcome — ordering and in-flight counts cannot, because
/// an ordinary commit may still be on its way when the action starts.
#[derive(Clone)]
struct OutlinePending {
    job: CommitJobId,
    snapshot: OutlineSnapshot,
}

/// The sticky outline move mode (NOTE-outline-editing.md §移動モード): the
/// reader grabs one block with `m` and keeps dragging it with plain keys.
/// Every step is LOCAL — the existing line values are reordered and
/// re-indented in place, ids and all, nothing is sent and nothing is
/// pushed onto the undo stack — and leaving sends ONE commit for the whole
/// drag. Five presses are one commit, one new set of ids, one undo step.
#[derive(Clone)]
struct MoveMode {
    /// Line ids of the grabbed block, frozen at entry. Outdenting inside
    /// the mode makes the lines below read as children; the reader still
    /// picked up THESE lines, so the set never grows.
    block: Vec<String>,
    /// The page as it stood before the grab. The one exit commit is the
    /// diff from here to wherever the block ended up.
    before: Vec<PageLine>,
    cursor: usize,
    selection: Option<Selection>,
}

struct App {
    mode: Mode,
    project: String,
    title: String,
    /// Cosense project's navbar colors, adapted over the terminal background.
    header_colors: HeaderColors,
    /// Immutable page id (the edit API and the commit log key on it).
    page_id: String,
    /// Raw page lines with per-line author/time metadata. Note `user_id` is
    /// the line's LAST UPDATER (verified against the commit log), not its
    /// original author — the page API does not carry the creator.
    lines: Vec<PageLine>,
    blocks: Vec<Block>,
    /// What can be followed on each block (parallel to `blocks`), as the
    /// renderer reported it. The click path reads this instead of looking
    /// at colours.
    hits: Vec<Vec<cosense::render::Hit>>,
    srcs: Vec<usize>,
    images: HashMap<String, ImageInfo>,
    image_errors: HashMap<String, String>,
    /// Images currently downloading in the background.
    pending: HashSet<String>,
    image_tx: mpsc::Sender<ImageMsg>,
    image_rx: mpsc::Receiver<ImageMsg>,
    /// File downloads in flight (Enter/f on a 📎 link).
    file_tx: mpsc::Sender<FileMsg>,
    file_rx: mpsc::Receiver<FileMsg>,

    // --- web renderer (Mermaid today; see cosense::webrender) -------------
    /// Bumped on every page install. Shared with the render worker, which
    /// drops queued work for a page the reader has left before spending a
    /// decode or a browser navigation on it. A result that comes back for
    /// an older generation is dropped even if its key were to collide.
    web_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Terminal background is dark (picks the browser's color scheme).
    web_dark: bool,
    /// Render keys currently in flight.
    web_pending: HashSet<String>,
    /// Why a key failed, for the status line. Its block falls back to code.
    web_errors: HashMap<String, String>,
    /// Source lines whose code block is being rendered right now, each with
    /// `(row's position in the block, block row count)`. The draw pass runs
    /// a band of brightness down them so the reader can see the renderer
    /// working — rebuilt with the layout, empty whenever nothing is pending.
    web_shimmer: HashMap<usize, (u16, u16)>,
    /// Clock the shimmer animates against.
    web_anim: std::time::Instant,
    /// Column cap the on-screen diagrams are encoded for — `text_w` capped
    /// at `IMAGE_MAX_COLS`. Updated by the layout; a change re-encodes the
    /// artifacts from their cached PNGs (no browser).
    web_cols: u16,
    /// Rescales in flight, so a resize does not queue the same key twice.
    web_rescaling: HashSet<String>,
    /// The local page may no longer match the server: a commit was refused
    /// or never left the machine. The browser can only ever show what the
    /// SERVER has, so while this is set no diagram is rendered — a render
    /// would capture the server's version of a block and file it under the
    /// LOCAL text's hash, quietly attaching the wrong picture to the source
    /// the reader is looking at. Cleared only when an authoritative page
    /// install proves the two agree again (see `mark_desynced`).
    web_unsynced: bool,
    /// A diagram-failure note, and when it stops being worth showing.
    ///
    /// It lives in its OWN slot rather than in `app.status`, and ranks
    /// BELOW it: a commit failure, an auth error or a resync notice must
    /// never be overwritten — or worse, wiped when the diagram note times
    /// out — by something as minor as a picture that did not draw. It ranks
    /// above the key hints, and gives the line back after a few seconds so
    /// the hints return.
    web_notice: Option<(String, std::time::Instant)>,
    /// Jobs into the render worker, results back. Artifacts land in
    /// `images` (they are images), so drawing needs no special case.
    web_job_tx: mpsc::Sender<WebJob>,
    /// Held until `main` spawns the worker; tests keep it and inspect jobs.
    web_jobs_rx: Option<mpsc::Receiver<WebJob>>,
    web_tx: mpsc::Sender<WebMsg>,
    web_rx: mpsc::Receiver<WebMsg>,

    rows: Vec<Row>,
    laid_width: u16,
    /// Viewport height (text rows) as of the last frame; movement keys use
    /// it to keep the cursor in view without waiting for the next draw.
    view_h: u16,
    /// Screen geometry as of the last frame, for mouse hit-testing: the
    /// text area and the scrollbar track.
    text_rect: Rect,
    bar_rect: Rect,
    /// Source line where a left-button drag started (selection anchor),
    /// with the character anchor of the same press when the drag runs
    /// inside an edit session. A READ drag has no character range, so its
    /// anchor keeps `None` there.
    drag_anchor: Option<(usize, Option<usize>)>,
    /// Scrollbar thumb grab: (track row, scroll offset) at the press.
    scrollbar_drag: Option<(u16, u16)>,
    scroll: u16, // row-height units from top

    /// SOURCE line under the cursor (0-based). Display rows are derived —
    /// see `cursor_rows`. akapen's `ViewState.cursor`.
    cursor: usize,
    /// Range selection over SOURCE lines — READ's unit. EDIT selects
    /// characters instead (`EditSession::sel_from`); the two never coexist.
    selection: Option<Selection>,
    /// What is known about the pages this page's links lead to. Seeded by
    /// each page load and filled in by background lookups; kept for the
    /// whole project so walking back and forth does not re-ask.
    links: LinkTruth,
    /// Titles (in `title_lc` form) already asked about. A title stays here
    /// even if the lookup failed: one question per title per session, so a
    /// project this credential cannot read costs one request, not one
    /// every scan. The cost of that is a link left uncoloured, which is
    /// the harmless direction.
    link_pending: HashSet<String>,
    /// When the page was last scanned for links nobody has asked about,
    /// and what the page looked like then: the caret's line and the source
    /// epoch. A change to either is a reason to scan now rather than on
    /// the next beat.
    link_scan_at: Instant,
    link_scan_key: (Option<usize>, u64),
    /// To the lookup worker (`spawn_link_prober`); `None` in tests, which
    /// makes `probe_unknown_links` a no-op.
    link_probe_tx: Option<mpsc::Sender<LinkProbe>>,
    link_probe_rx: mpsc::Receiver<(LinkProbe, bool)>,
    link_probe_res_tx: mpsc::Sender<(LinkProbe, bool)>,
    /// Scroll the viewport to the cursor on the next frame. Set by keyboard
    /// navigation; wheel scrolling leaves it clear so the viewport can move
    /// away from the cursor (akapen's herdr-review style).
    follow: bool,
    /// Comment composer (the bottom input; `c`).
    composing: Option<Input>,
    comments: Vec<Comment>,
    status: String,
    /// One-shot portable fallback for terminals that do not deliver
    /// modified arrow keys. The next key is always consumed.
    outline_prefix: bool,
    /// The grabbed block, while the sticky move mode is up.
    move_mode: Option<MoveMode>,
    /// Snapshot held while the one structural commit is outstanding. READ
    /// remains navigable, but no second mutation may be based on its
    /// optimistic line order until the server accepts or rejects it.
    outline_pending: Option<OutlinePending>,
    /// A structural commit succeeded after this installation was fetched,
    /// but its authoritative refresh failed. The displayed line IDs are
    /// unsafe edit targets until another page install replaces them.
    outline_refresh_needed: bool,

    /// Back stack of visited places (`[`), and forward stack (`]`).
    history: Vec<Place>,
    forward: Vec<Place>,
    /// Open overlay (comments list / link picker / help), if any.
    overlay: Option<Overlay>,
    /// The project index (full-width list + excerpt dock). Open = it owns
    /// the screen and the keys; the page it came from remains underneath.
    index: Option<cosense::index::Index>,
    /// The list and excerpt rectangles from the last index frame, so mouse
    /// input uses exactly the geometry that was drawn.
    index_list_rect: Rect,
    index_preview_rect: Rect,
    /// The project the open index is listing. Usually the page's own, but
    /// a `[/other-project]` link opens that project's index while the page
    /// underneath is still this one's.
    index_project: String,
    /// When the index was last scrolled. Its scrollbar appears briefly at
    /// the screen edge while the list moves, then gets out of the way.
    index_scrolled_at: Option<Instant>,

    /// When this page was last seen by the user before this visit — the
    /// later of Cosense's `lastAccessed` (browser) and the local visit
    /// record (this viewer). `None` = first visit anywhere: every line is
    /// unread, as on scrapbox.io.
    read_at: Option<i64>,

    /// Per-project member tables (userId -> display name) for line blame,
    /// fetched lazily: see `ensure_members` for when they refresh.
    members: HashMap<String, MembersCache>,

    /// Some while the reader is viewing a historical snapshot (←/→).
    /// The page is read-only in that state.
    time: Option<TimeMachine>,
    /// Whether the current credential may edit THIS project. Public pages
    /// remain readable with a PAT for another project, but must not enter
    /// EDIT unless that user is actually a member.
    editable: bool,
    /// Session input-source control: command mode runs in ASCII, the
    /// original source is restored when the app drops (Japanese-first
    /// model, see `cosense::ime`).
    session_ime: cosense::ime::SessionIme,
    /// True once the session's force-ascii took hold (the helper may still
    /// be compiling on first run; retried each tick until then).
    ime_ready: bool,
    /// Held while the composer or edit session is open: Japanese in,
    /// ASCII out.
    ime_guard: Option<cosense::ime::ImeGuard>,

    /// The modeless edit session, when open.
    session: Option<EditSession>,
    /// Undo stack: (label, ops that revert the corresponding commit).
    undo_stack: Vec<(String, Vec<EditOp>)>,
    redo_stack: Vec<(String, Vec<EditOp>)>,
    /// A server refresh threw away history that could no longer be
    /// replayed. Only used to explain an empty stack instead of shrugging.
    history_dropped: bool,
    /// Where an uncreated page stands with the server (see `CreateState`).
    create_state: CreateState,
    /// Commit queue into the serial worker, and its outcomes back.
    commit_tx: mpsc::Sender<CommitJob>,
    commit_res_rx: mpsc::Receiver<CommitOutcome>,
    /// The worker's job receiver, held until `main` spawns the worker
    /// (tests keep it here and inspect queued jobs directly).
    commit_jobs_rx: Option<mpsc::Receiver<CommitJob>>,
    /// Result sender handed to the worker (kept for the spawn call).
    commit_res_tx: mpsc::Sender<CommitOutcome>,
    /// Jobs sent but not yet answered (quit flushes until 0).
    inflight: usize,
    /// Next commit-job id. Monotonic for the life of the process, so an
    /// outcome can always be traced back to the job that produced it.
    next_job_id: CommitJobId,
    /// Commit ids this viewer wrote, newest last. Their websocket echoes
    /// carry ops we have already applied locally — and may arrive AFTER we
    /// have edited past them, in which case applying the ops again would
    /// put an older version of a line back on screen. Recognised by id, so
    /// they only move the chain head along.
    ///
    /// Bounded: an echo that has not arrived within `OWN_COMMIT_MEMORY`
    /// commits is not going to, and the page has moved so far past it that
    /// the chain check would send us to a resync anyway.
    own_commits: std::collections::VecDeque<String>,
    /// Conflict generation: bumping it invalidates all queued jobs.
    gen: Arc<std::sync::atomic::AtomicU64>,
    /// What the web poller should watch (project, title); updated on every
    /// page install and rename.
    poll_target: Arc<std::sync::Mutex<(String, String)>>,
    /// Pages the poller fetched (applied by `apply_remote`).
    poll_rx: mpsc::Receiver<PolledPage>,
    /// The poller's sender (kept for the spawn call in `main`).
    poll_tx: mpsc::Sender<PolledPage>,
    /// Retunes the poller's interval while it sleeps. The push channel's
    /// state is what drives it (see `sync_state`).
    poll_ctrl_tx: mpsc::Sender<Duration>,
    poll_ctrl_rx: Option<mpsc::Receiver<Duration>>,
    /// What this session may do for the current project: sid presence,
    /// project visibility, and whatever the renderer has been refused.
    /// Never a single "authenticated" flag — see `cosense::capability`.
    caps: capability::Capabilities,
    /// When diagrams may be drawn (`COSENSE_WEB_RENDER`).
    render_policy: capability::RenderPolicy,
    /// Visibility answers from the background probe (project, verdict).
    vis_rx: mpsc::Receiver<(String, capability::Visibility)>,
    vis_tx: mpsc::Sender<(String, capability::Visibility)>,
    /// The project the probe was last asked about, so a page move inside
    /// the same project does not re-ask.
    vis_asked: Option<String>,
    /// Artifacts a cache-only pass looked for and did not find. They are
    /// NOT failures: they stay as source, stop shimmering, and `R` can
    /// still render them.
    web_missing: HashSet<String>,
    /// `R` was pressed while the page-load cache probe was still out. The
    /// probe owns those keys until it answers, so the keypress cannot queue
    /// anything yet — it is remembered here and served the moment the
    /// misses come back. Cleared per page.
    web_manual_wanted: bool,
    /// The "you need a cookie" notice has been shown for THIS page already.
    /// Reset by `set_page`, so it is said once per page and not per block.
    web_notice_shown: bool,

    /// Monotonic counter of the page SOURCE as the app holds it. Deliberately
    /// separate from `server_epoch`: that one guards poll responses against
    /// local progress, this one guards a render in flight against the source
    /// moving underneath it. Folding them together would either throw away
    /// good poll snapshots on every keystroke or good renders on every poll.
    ///
    /// A render captures whatever the SERVER is showing at the moment the
    /// browser looks. If the source has moved since the job was queued, that
    /// screenshot belongs to a different text than the key it would be filed
    /// under — and the artifact cache keeps it for a week.
    src_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// Monotonic counter of everything that makes the server's state newer
    /// than a poll already in flight: a local commit landing, a websocket
    /// commit applied, a page installed. A poller stamps this when it
    /// starts a GET; if it has moved by the time the response arrives, that
    /// response is a photograph of the past and is dropped.
    server_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// The push channel's state, TYPED. The status line renders this; the
    /// poller's interval follows it. Nothing reads the status text.
    sync_state: capability::SyncState,
    /// Whether a push channel is being attempted at all (a sid exists).
    /// A session without one shows `poll`, not `reconnecting`.
    ws_attempted: bool,

    /// Websocket push events (commit payloads, resyncs, status notes).
    ws_rx: mpsc::Receiver<ws::WsEvent>,
    /// The sync thread's sender (kept for the spawn call in `main`).
    ws_tx: mpsc::Sender<ws::WsEvent>,
    /// Resync requests toward the sync thread: only it does network I/O —
    /// a gap asks for a full-page refetch here and applies the result when
    /// it comes back. Kept for tests to inspect the requests.
    ws_req_tx: mpsc::Sender<ws::WsRequest>,
    ws_req_rx: Option<mpsc::Receiver<ws::WsRequest>>,
    /// A gap was detected (a commit whose `parentId` broke the chain): a
    /// full-page resync was requested and will arrive on `ws_rx`.
    ws_resync_pending: bool,
    /// A `Resynced` that arrived while an apply gate was up, plus how many
    /// buffered commits preceded it (those are superseded by the page).
    /// Only the LATEST is kept; applied when the gate drops.
    ws_held_resync: Option<(ws::ResyncPage, usize)>,
    /// Remote commits that arrived while an apply gate was up; applied in
    /// order once the gates drop (see `ws_flush_pending`).
    ws_pending: std::collections::VecDeque<RemoteCommit>,
    /// Commit id the local page model is known to be at. A commit whose
    /// `parentId` does not match it means we missed something → a
    /// background resync (see `ws_resync_pending`).
    ws_head: Option<String>,
    /// Last left-click (for double- and triple-click detection): time,
    /// column, row, and how many clicks this spot has counted.
    last_click: Option<(Instant, u16, u16, u8)>,
    /// Link pressed with the left button. Activation waits for button-up on
    /// the same target, so dragging can still select text/lines.
    pressed_link: Option<(usize, LinkItem)>,
    /// Related-pages sections below the body (view mode only; hidden while
    /// the edit session is open).
    related: Vec<RelSection>,
    /// Flattened related entries in render order. Entry `i` renders with
    /// the VIRTUAL source index `lines.len() + i`, so the cursor, Enter and
    /// mouse clicks address related rows exactly like body lines.
    virtual_items: Vec<LinkItem>,
    /// Terminal background is light (for related-row colors; set from Ctx).
    light: bool,
}

/// One project's member table and when it was fetched.
struct MembersCache {
    names: HashMap<String, String>,
    fetched_at: Instant,
}

/// A member table older than this is refetched on the next use: joins,
/// departures and display-name changes are rare, so a long window is fine.
const MEMBERS_TTL: Duration = Duration::from_secs(10 * 60);
/// An unknown userId (a member who joined after the fetch) triggers an
/// early refetch — but at most this often, so a departed member's id (which
/// will never resolve) cannot make every `t` press hit the API.
const MEMBERS_MISS_COOLDOWN: Duration = Duration::from_secs(60);

/// Time-machine state: the page's server-side snapshot history, and where
/// the reader currently stands in it. akapen's document timeline, backed by
/// Cosense's page-snapshots API instead of Git — the server already keeps
/// every version, so nothing is captured locally.
struct TimeMachine {
    /// Snapshot stamps, oldest → newest.
    points: Vec<cosense::api::SnapshotStamp>,
    /// Index into `points` currently shown. Stepping past the newest
    /// point exits the machine back to NOW (the live page).
    pos: usize,
    /// Fetched snapshots, so scrubbing back and forth is instant.
    cache: HashMap<String, cosense::api::Snapshot>,
}

/// A single-line text input with a movable cursor (byte index, always on
/// a char boundary). The composer used to be append-only; Japanese text
/// especially needs mid-line correction without retyping everything.
struct Input {
    buf: String,
    cur: usize,
}

impl Input {
    fn new(buf: String) -> Self {
        let cur = buf.len();
        Input { buf, cur }
    }
    fn parts(&self) -> (&str, &str) {
        self.buf.split_at(self.cur)
    }
    fn insert_char(&mut self, ch: char) {
        self.buf.insert(self.cur, ch);
        self.cur += ch.len_utf8();
    }
    fn insert_str(&mut self, s: &str) {
        self.buf.insert_str(self.cur, s);
        self.cur += s.len();
    }
    fn left(&mut self) {
        if let Some(ch) = self.buf[..self.cur].chars().next_back() {
            self.cur -= ch.len_utf8();
        }
    }
    fn right(&mut self) {
        if let Some(ch) = self.buf[self.cur..].chars().next() {
            self.cur += ch.len_utf8();
        }
    }
    fn home(&mut self) {
        self.cur = 0;
    }
    fn end(&mut self) {
        self.cur = self.buf.len();
    }
    fn backspace(&mut self) {
        if let Some(ch) = self.buf[..self.cur].chars().next_back() {
            self.cur -= ch.len_utf8();
            self.buf.remove(self.cur);
        }
    }
    fn delete(&mut self) {
        if self.cur < self.buf.len() {
            self.buf.remove(self.cur);
        }
    }
    /// ^w: delete the word (or whitespace run) left of the cursor.
    fn delete_word(&mut self) {
        let head = &self.buf[..self.cur];
        let trimmed = head.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        self.buf.replace_range(cut..self.cur, "");
        self.cur = cut;
    }
    /// ^u: kill to the start of the line.
    fn kill_to_start(&mut self) {
        self.buf.replace_range(..self.cur, "");
        self.cur = 0;
    }
    /// ^k: kill to the end of the line.
    fn kill_to_end(&mut self) {
        self.buf.truncate(self.cur);
    }
}

/// The modeless edit session (SPEC-edit-session.md): the caret line shows
/// raw source and takes every printable key; ↑/↓ walk lines, Enter splits
/// or creates lines, BOL-Backspace joins — the session spans any number of
/// lines. The only dirty state is THIS line's working text; everything
/// structural commits immediately, text commits when the caret leaves.
struct EditSession {
    /// Caret line (index into `app.lines`; mirrored to `app.cursor`).
    line: usize,
    /// Working text + caret of the caret line.
    input: Input,
    /// Last committed text of this line (`dirty = input.buf != orig`).
    orig: String,
    /// Sticky display column for ↑/↓ over lines of differing length.
    want_col: Option<usize>,
    /// Character selection: the fixed end of the range as a
    /// (line, byte) position, with the caret as the moving one. EDIT has
    /// no line-range concept at all — a range may cross lines, and every
    /// operation on it (typing, ⌫, Enter, paste, copy) is text-unit: the
    /// lines it spans merge or split the way a textarea's would. `None`,
    /// or an anchor sitting exactly on the caret, selects nothing.
    sel_from: Option<(usize, usize)>,
}

impl EditSession {
    /// The selection as two (line, byte) ends in DOCUMENT order (top
    /// first), `None` when nothing is selected. The caret is always one of
    /// the two ends — the head a drag or a Shift-move last reached.
    fn sel_ends(&self) -> Option<((usize, usize), (usize, usize))> {
        let from = self.sel_from?;
        let head = (self.line, self.input.cur);
        (from != head).then(|| if from <= head { (from, head) } else { (head, from) })
    }

    /// The selected byte range WHEN IT FITS ON THE CARET LINE, normalised
    /// — the unit the local, uncommitted edits (cut, wrap, bracket math)
    /// work in. A range that crossed into another line answers `None`:
    /// touching it is structural, and structural edits commit.
    fn sel_span(&self) -> Option<(usize, usize)> {
        let ((a, x), (b, y)) = self.sel_ends()?;
        (a == b).then_some((x, y))
    }

    /// The covered byte range OF THE CARET LINE for whatever selection
    /// exists: the caret line is always one end of the range, so a range
    /// off this line reaches from the caret to that line's near edge.
    /// This is what draws reversed on the raw caret line; the other lines
    /// the range covers are painted as whole-line bands.
    fn caret_sel_bytes(&self) -> Option<(usize, usize)> {
        let ((tl, tb), (bl, bb)) = self.sel_ends()?;
        if tl == bl {
            Some((tb, bb))
        } else if self.line == tl {
            Some((tb, self.input.buf.len()))
        } else {
            Some((0, bb))
        }
    }
}

/// Identifies one queued commit for its whole life, outcome included.
type CommitJobId = u64;

/// One queued commit for the serial background worker. `id` names this job
/// so its outcome can be recognised; `gen` invalidates jobs queued before a
/// conflict reload (their base state is gone).
struct CommitJob {
    id: CommitJobId,
    gen: u64,
    project: String,
    page_id: String,
    label: String,
    ops: Vec<EditOp>,
}

/// What a commit attempt came back with. Every variant carries the id of
/// the job it answers: outcomes arrive one at a time but jobs are queued
/// freely, so nothing about arrival order says which job an outcome is for.
enum CommitOutcome {
    /// `commit_id` is the id the server gave this commit — the name our
    /// own edit will come back under on the websocket (see
    /// `App::own_commits`).
    Done { job: CommitJobId, label: String, title: String, commit_id: String },
    Conflict { job: CommitJobId },
    Skipped { job: CommitJobId },
    Failed { job: CommitJobId, label: String, msg: String },
}

impl CommitOutcome {
    /// The job this outcome answers.
    fn job(&self) -> CommitJobId {
        match self {
            CommitOutcome::Done { job, .. }
            | CommitOutcome::Conflict { job }
            | CommitOutcome::Skipped { job }
            | CommitOutcome::Failed { job, .. } => *job,
        }
    }
}

/// A freshly polled page: how web-side edits reach the screen (~3 s).
struct PolledPage {
    project: String,
    title: String,
    page: cosense::api::Page,
    /// The server-state epoch when this fetch was STARTED. If anything has
    /// advanced the epoch since — a local commit, an applied websocket
    /// commit, a page install — then this snapshot predates it and would
    /// roll the reader back. `commitId` cannot be used for this: it is not
    /// safely ordered from the client's side.
    epoch: u64,
}

/// The web-edit poller: refetches the CURRENT page every few seconds on
/// its own thread and ships it to the event loop, which applies it only
/// when nothing local is in flight (see `apply_remote`). Cosense proper
/// uses a websocket; polling one small JSON keeps this dependency-free
/// and is plenty "live" for a wiki.
fn spawn_web_poller(
    client: Client,
    target: Arc<std::sync::Mutex<(String, String)>>,
    tx: mpsc::Sender<PolledPage>,
    ctrl: mpsc::Receiver<Duration>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    interval: Duration,
) {
    std::thread::spawn(move || {
        let mut interval = interval;
        loop {
            // The sleep IS the control channel: a push channel that dies
            // 2 s into a 60 s nap must not leave the reader waiting out the
            // other 58. `recv_timeout` wakes on either.
            let fetch_now = match absorb_interval(&ctrl, interval) {
                Some((next, urgent)) => {
                    interval = next;
                    urgent
                }
                None => return, // app gone
            };
            if !fetch_now {
                continue;
            }
            let (project, title) = match target.lock() {
                Ok(t) => t.clone(),
                Err(_) => return,
            };
            if project.is_empty() || title.is_empty() {
                continue;
            }
            // Stamped BEFORE the request goes out: everything that happens
            // while it is in flight makes the answer stale.
            let started_at = epoch.load(std::sync::atomic::Ordering::SeqCst);
            if let Ok(page) = client.get_page_in(&project, &title) {
                if tx.send(PolledPage { project, title, page, epoch: started_at }).is_err() {
                    return; // app gone
                }
            }
        }
    });
}

/// Ask, off the UI thread, whether a project is readable with no credential
/// at all. One anonymous request per project, and only when the answer could
/// change a decision.
fn spawn_visibility_probe(
    client: Client,
    project: String,
    tx: mpsc::Sender<(String, capability::Visibility)>,
) {
    std::thread::spawn(move || {
        let verdict = client.probe_visibility(&project);
        let _ = tx.send((project, verdict));
    });
}

/// Wait out `current`, or wake early on a new interval from the control
/// channel. Returns the interval to use next and whether to fetch right now;
/// `None` means the channel is gone and the poller should stop.
///
/// Speeding up fetches immediately — that transition means "the push channel
/// just stopped being trustworthy", and the point of the switch is to close
/// the gap, not to open a fresh one. Slowing down does not: the push channel
/// went live and just delivered a catch-up, so there is nothing to catch.
fn absorb_interval(
    ctrl: &mpsc::Receiver<Duration>,
    current: Duration,
) -> Option<(Duration, bool)> {
    match ctrl.recv_timeout(current) {
        // Timed out: an ordinary tick.
        Err(mpsc::RecvTimeoutError::Timeout) => Some((current, true)),
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
        Ok(first) => {
            // Several state changes can pile up while we slept; only the
            // last one describes the world.
            let mut next = first;
            loop {
                match ctrl.try_recv() {
                    Ok(d) => next = d,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return None,
                }
            }
            Some((next, next < current))
        }
    }
}

/// The serial commit worker: one job in flight at a time, in queue order —
/// ordering is what keeps every op valid against the server's state.
fn spawn_commit_worker(
    client: Client,
    jobs: mpsc::Receiver<CommitJob>,
    out: mpsc::Sender<CommitOutcome>,
    gen: Arc<std::sync::atomic::AtomicU64>,
) {
    std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            let id = job.id;
            if job.gen < gen.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = out.send(CommitOutcome::Skipped { job: id });
                continue;
            }
            let res = client
                .preview_edit(&job.project, &job.page_id, &job.ops)
                .and_then(|p| client.submit_edit(&job.project, &p.preview_id));
            let outcome = match res {
                Ok(c) => CommitOutcome::Done {
                    job: id,
                    label: job.label,
                    title: c.title,
                    commit_id: c.commit_id,
                },
                Err(EditError::NotFastForward) => CommitOutcome::Conflict { job: id },
                Err(e) => CommitOutcome::Failed { job: id, label: job.label, msg: e.to_string() },
            };
            let _ = out.send(outcome);
        }
    });
}

/// A modal list/panel layered over the page.
enum Overlay {
    /// All comments across pages; Enter jumps to the page+line.
    Comments { cursor: usize },
    /// Link picker for the cursor line when it has several links.
    Links { items: Vec<LinkItem>, cursor: usize },
    /// Details for the cursor's line: who last edited it and when
    /// (akapen's `t` detail slot).
    LineInfo,
    /// Key reference.
    Help,
}

impl Overlay {
}

/// Current wall-clock time in epoch seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "3m" / "2h" / "5d" style age from an epoch-seconds timestamp.
fn relative_age(updated: i64) -> String {
    let now = now_secs();
    let secs = (now - updated).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 86_400 * 365 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}y", secs / (86_400 * 365))
    }
}

impl App {
    fn new(project: String) -> Self {
        let (image_tx, image_rx) = mpsc::channel();
        let (file_tx, file_rx) = mpsc::channel();
        let (web_job_tx, web_jobs_rx) = mpsc::channel();
        let (web_tx, web_rx) = mpsc::channel();
        let (commit_tx, commit_jobs_rx) = mpsc::channel();
        let (commit_res_tx, commit_res_rx) = mpsc::channel();
        let (poll_tx, poll_rx) = mpsc::channel();
        let (poll_ctrl_tx, poll_ctrl_rx) = mpsc::channel();
        let (vis_tx, vis_rx) = mpsc::channel();
        let (ws_tx, ws_rx) = mpsc::channel();
        let (ws_req_tx, ws_req_rx) = mpsc::channel();
        let (link_probe_res_tx, link_probe_rx) = mpsc::channel();
        App {
            mode: Mode::View,
            project,
            title: String::new(),
            header_colors: HeaderColors::fallback(),
            page_id: String::new(),
            lines: Vec::new(),
            blocks: Vec::new(),
            srcs: Vec::new(),
            images: HashMap::new(),
            image_errors: HashMap::new(),
            pending: HashSet::new(),
            image_tx,
            image_rx,
            file_tx,
            file_rx,
            rows: Vec::new(),
            laid_width: 0,
            view_h: 0,
            text_rect: Rect::default(),
            bar_rect: Rect::default(),
            drag_anchor: None,
            scrollbar_drag: None,
            scroll: 0,
            cursor: 0,
            selection: None,
            hits: Vec::new(),
            links: LinkTruth::default(),
            link_pending: HashSet::new(),
            link_scan_at: Instant::now(),
            link_scan_key: (None, 0),
            link_probe_tx: None,
            link_probe_rx,
            link_probe_res_tx,
            follow: true,
            composing: None,
            comments: Vec::new(),
            status: String::new(),
            outline_prefix: false,
            move_mode: None,
            outline_pending: None,
            outline_refresh_needed: false,
            history: Vec::new(),
            forward: Vec::new(),
            overlay: None,
            index: None,
            index_list_rect: Rect::default(),
            index_preview_rect: Rect::default(),
            index_project: String::new(),
            index_scrolled_at: None,
            web_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            web_dark: true,
            web_pending: HashSet::new(),
            web_errors: HashMap::new(),
            web_shimmer: HashMap::new(),
            web_anim: std::time::Instant::now(),
            web_cols: IMAGE_MAX_COLS,
            web_rescaling: HashSet::new(),
            web_unsynced: false,
            web_notice: None,
            web_job_tx,
            web_jobs_rx: Some(web_jobs_rx),
            web_tx,
            web_rx,
            read_at: None,
            members: HashMap::new(),
            time: None,
            editable: false,
            session_ime: cosense::ime::SessionIme::new(cosense::ime::ImeMode::Off),
            ime_ready: false,
            ime_guard: None,
            session: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_dropped: false,
            create_state: CreateState::Idle,
            commit_tx,
            commit_res_rx,
            commit_jobs_rx: Some(commit_jobs_rx),
            commit_res_tx,
            inflight: 0,
            next_job_id: 1,
            own_commits: std::collections::VecDeque::new(),
            gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            poll_target: Arc::new(std::sync::Mutex::new((String::new(), String::new()))),
            poll_rx,
            poll_tx,
            poll_ctrl_tx,
            poll_ctrl_rx: Some(poll_ctrl_rx),
            caps: capability::Capabilities::default(),
            render_policy: capability::RenderPolicy::from_env(),
            vis_rx,
            vis_tx,
            vis_asked: None,
            web_missing: HashSet::new(),
            web_manual_wanted: false,
            web_notice_shown: false,
            src_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            server_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sync_state: capability::SyncState::Polling,
            ws_attempted: false,
            ws_rx,
            ws_tx,
            ws_req_tx,
            ws_req_rx: Some(ws_req_rx),
            ws_resync_pending: false,
            ws_held_resync: None,
            ws_pending: std::collections::VecDeque::new(),
            ws_head: None,
            last_click: None,
            pressed_link: None,
            related: Vec::new(),
            virtual_items: Vec::new(),
            light: false,
        }
    }

    /// Number of cursor-addressable source indices: body lines, plus the
    /// related entries in view mode (virtual lines after the body). The
    /// edit session hides the related section, so its rows are not
    /// addressable then either — exactly like source mode.
    /// Where the reader is right now, for the back stack.
    fn here(&self) -> Place {
        match self.index.as_ref() {
            Some(index) => Place::Index {
                project: self.index_project.clone(),
                state: Box::new(index.clone()),
            },
            None => Place::Page { project: self.project.clone(), title: self.title.clone() },
        }
    }

    fn src_count(&self) -> usize {
        self.lines.len()
            + if self.mode == Mode::View && self.session.is_none() {
                self.virtual_items.len()
            } else {
                0
            }
    }

    /// Read state for a cursor-addressable related row. The flattening order
    /// is exactly the same as `virtual_items`.
    fn related_unread(&self, src: usize) -> Option<bool> {
        let index = src.checked_sub(self.lines.len())?;
        self.related
            .iter()
            .flat_map(|section| section.entries.iter())
            .nth(index)
            .map(|entry| entry.unread)
    }

    /// Install a freshly loaded page, resetting view state (keeps comments).
    /// Install a freshly loaded page and immediately kick off its image
    /// downloads. Loading is folded in here so no navigation path can forget
    /// it (back/forward included).
    fn set_page(&mut self, l: Loaded, ctx: &Ctx) {
        self.bump_server_epoch();
        self.time = None; // installing a live page always exits history
        self.outline_prefix = false;
        self.move_mode = None;
        self.outline_refresh_needed = false;
        // A page install ends any edit session and cuts the undo lineage:
        // undo ops reference THIS page's line ids.
        self.session = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        // Echoes of the last page's commits say nothing about this one.
        self.own_commits.clear();
        self.history_dropped = false;
        // Whatever the last page was waiting to become, it is not this one.
        self.create_state = CreateState::Idle;
        // A different project is a different set of capabilities: what we
        // learned about the old one (visibility, a refused browser) says
        // nothing here. The sid is a property of the SESSION and survives.
        if self.project != l.project {
            self.caps = self.caps.for_new_project();
            self.vis_asked = None;
        }
        self.project = l.project;
        self.title = l.title;
        self.header_colors = l.header_colors;
        self.page_id = l.page_id;
        self.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Navigating leaves the joined room. Until the thread has re-joined
        // the new one AND caught it up, there is no push channel here, so
        // the fast poll covers the gap — and `absorb_interval` fetches at
        // once on the way down rather than waiting out a 60 s nap.
        self.set_sync_state(SyncState::Polling);
        self.lines = l.lines;
        self.blocks = l.blocks;
        self.srcs = l.srcs;
        self.hits = l.hits;
        self.read_at = l.read_at;
        self.editable = l.editable;
        // Answers that were only true of the page we are leaving go now;
        // the page arriving may be the second one writing that word.
        self.links.forget_dead();
        self.links.absorb(l.links);
        // ...so those titles must be askable again.
        self.link_pending.clear();
        if let Ok(mut t) = self.poll_target.lock() {
            *t = (self.project.clone(), self.title.clone());
        }
        // A fresh page install severs the websocket commit lineage: the
        // room re-joins on the new target, and any remote head we tracked,
        // events buffered, or resync held for the OLD page are meaningless.
        self.ws_head = None;
        self.ws_pending.clear();
        self.ws_resync_pending = false;
        self.ws_held_resync = None;
        self.related = l.related;
        self.virtual_items = self
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
        self.images.clear();
        self.image_errors.clear();
        self.pending.clear();
        self.web_pending.clear();
        self.web_errors.clear();
        self.web_rescaling.clear();
        self.web_missing.clear();
        self.web_manual_wanted = false;
        // Once per page, not once per diagram.
        self.web_notice_shown = false;
        // A freshly fetched page IS the server's state.
        self.mark_synced();
        self.laid_width = 0; // force rebuild
        self.scroll = 0;
        self.cursor = 0;
        self.selection = None;
        self.follow = true;
        self.web_dark = !ctx.light;
        self.start_image_loads(ctx);
        self.start_web_renders(capability::Trigger::Auto);
    }

    /// Kick off background downloads for every image on the page. Each
    /// finishes independently and is installed by the event loop, so the page
    /// is readable immediately and images fill in as they arrive.
    fn start_image_loads(&mut self, ctx: &Ctx) {
        // Every picture on the page, INCLUDING the ones inside a mixed
        // text-and-picture line. Reading only `Block::Image` meant a
        // picture written beside text was laid out (its box reserved) and
        // then never fetched — it simply never appeared.
        let urls: Vec<String> = self
            .blocks
            .iter()
            .flat_map(|b| match b {
                Block::Image { url, .. } => vec![url.clone()],
                Block::Inline { parts, .. } => parts
                    .iter()
                    .filter_map(|p| match p {
                        cosense::render::InlinePart::Image(url) => Some(url.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        for url in urls {
            if self.images.contains_key(&url)
                || self.image_errors.contains_key(&url)
                || self.pending.contains(&url)
            {
                continue;
            }
            self.pending.insert(url.clone());
            let tx = self.image_tx.clone();
            let fetcher = Arc::clone(&ctx.fetcher);
            let picker = ctx.picker.clone();
            std::thread::spawn(move || {
                // download → decode → resize → protocol-encode, all here
                let res = fetcher
                    .fetch(&url)
                    .map_err(|e| e.to_string())
                    .and_then(|img| build_image(&picker, img, IMAGE_MAX_COLS));
                let _ = tx.send((url, res));
            });
        }
    }

    /// The render order for one Mermaid block, or `None` when the block
    /// cannot be rendered from the live web page: a historical snapshot (the
    /// browser only ever shows the current page), a line Cosense has not
    /// given an id yet, or a layout that has not happened.
    fn web_request(
        &self,
        kind: cosense::webrender::WebKind,
        code: &str,
        last_src: usize,
    ) -> Option<WebRequest> {
        if self.time.is_some() {
            return None;
        }
        let line_id = self.lines.get(last_src)?.id.clone();
        if line_id.is_empty() {
            return None;
        }
        Some(WebRequest {
            kind,
            project: self.project.clone(),
            title: self.title.clone(),
            page_id: self.page_id.clone(),
            line_id,
            code_hash: cosense::webrender::hash_code(code),
            dark: self.web_dark,
        })
    }

    /// Queue every not-yet-rendered diagram on the page as ONE batch. Returns
    /// immediately: the worker owns the browser, and the code block stays on
    /// screen until an artifact arrives.
    fn start_web_renders(&mut self, trigger: capability::Trigger) -> bool {
        // The browser can only ever show what the SERVER has, so nothing is
        // requested while a commit is still on its way there, or while a
        // commit is known to have failed. Both are whole-page gates: any
        // queued or lost commit could be the one that changes a diagram,
        // and `inflight == 0` alone only means "nothing in flight", not
        // "everything landed".
        if self.inflight > 0 || self.web_unsynced {
            return false;
        }
        // A historical snapshot is not what the server is showing now, so a
        // screenshot of the live page would be filed under the snapshot's
        // source hash — the same poisoning `src_epoch` guards against.
        if self.time.is_some() {
            return false;
        }
        // What this session may do, right now, for this project.
        let decision = capability::decide(&self.caps, self.render_policy, trigger);
        let auth = match decision {
            capability::Decision::Nothing => return false,
            capability::Decision::Render(cap) => Some(cap),
            capability::Decision::CacheOnly { notice } => {
                // Said once per page: repeating it per diagram, per pass,
                // would bury every other status the reader needs.
                if let Some(text) = notice {
                    if !self.web_notice_shown {
                        self.web_notice_shown = true;
                        self.note_web_failure(text.to_string());
                    }
                }
                None
            }
        };
        // The one anonymous attempt on an Unknown project is spent HERE,
        // when it is dispatched — not when it answers, or a second `R`
        // pressed while the first is in flight would spend it twice.
        if auth == Some(RenderCapability::Anonymous)
            && self.caps.visibility != capability::Visibility::Public
        {
            self.caps.anonymous_spent = true;
        }
        let mut reqs: Vec<WebRequest> = Vec::new();
        for b in &self.blocks {
            let Block::WebRender { kind, code, rows, last_src } = b else { continue };
            // An open session does NOT hold up the rest of the page: only
            // the block under the caret waits, because that one line's
            // buffer has not been committed yet (every caret MOVE commits
            // the dirty line first, so leaving a block releases it).
            if self.caret_is_inside(rows) {
                continue;
            }
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            let key = req.cache_key();
            if self.images.contains_key(&key)
                || self.web_errors.contains_key(&key)
                || self.web_pending.contains(&key)
                || reqs.iter().any(|r| r.cache_key() == key)
            {
                continue;
            }
            // A cache-only pass already looked at this one and found
            // nothing. Asking again would just re-answer `Missing`; only a
            // pass that may actually draw is worth sending.
            if self.web_missing.contains(&key) && auth.is_none() {
                continue;
            }
            reqs.push(req);
        }
        if reqs.is_empty() {
            return false;
        }
        let queued = auth.is_some();
        let keys: Vec<String> = reqs.iter().map(|r| r.cache_key()).collect();
        for k in &keys {
            self.web_pending.insert(k.clone());
            self.web_missing.remove(k);
        }
        // Those blocks now shimmer; the map is rebuilt with the layout.
        self.laid_width = 0;
        self.web_anim = std::time::Instant::now();
        if self
            .web_job_tx
            .send(WebJob::Render {
                gen: self.gen_now(),
                src_epoch: self.src_epoch_now(),
                reqs,
                max_cols: self.web_cols,
                auth,
            })
            .is_err()
        {
            // The worker is gone (shutting down). Nothing will ever answer,
            // so un-mark them: the code block stays, and it stops pulsing.
            for k in &keys {
                self.web_pending.remove(k);
            }
            return false;
        }
        queued
    }

    /// The browser hit a login wall. This says something about the COOKIE,
    /// never about the PAT that is reading and committing this page — so
    /// nothing here touches `editable`, the credential, or the status.
    fn note_denied(&mut self, denied: Vec<(String, Option<RenderCapability>)>) {
        // Every key in the batch goes back to being drawable: whatever we
        // decide below, none of these is a FAILURE of the diagram.
        for (key, _) in &denied {
            self.web_missing.insert(key.clone());
        }
        // The batch was one request with one credential state. If any key
        // in it names the state, that is the state that was refused.
        let attempted = denied
            .iter()
            .find_map(|(_, a)| *a)
            .unwrap_or(RenderCapability::Anonymous);
        // A refused cookie is a refused cookie whatever we do next: from
        // here it counts as absent, so no later pass presents it again.
        // (Without this, a private page would retry the same dead cookie on
        // every `R`, forever.)
        if attempted == RenderCapability::Authenticated {
            self.caps.cookie_rejected = true;
        }
        match capability::on_not_authorized(&self.caps, attempted) {
            // A public page that refused a cookie refused a STALE cookie.
            // Anonymous is a different request, and usually works — and it
            // retries EVERY key the batch lost, in one more batch.
            capability::Denial::RetryAnonymous => {
                // `cookie_rejected` above is what makes this retry genuinely
                // cookie-free, and it retries EVERY key the batch lost in
                // one more batch.
                self.start_web_renders(capability::Trigger::Manual);
            }
            capability::Denial::GiveUp => {
                // Only an attempt that carried no cookie can prove the
                // browser has nothing left to offer.
                if attempted == RenderCapability::Anonymous {
                    self.caps.browser_denied = true;
                }
                if !self.web_notice_shown {
                    self.web_notice_shown = true;
                    let msg = if self.caps.sid {
                        capability::sid_rejected()
                    } else {
                        capability::needs_sid()
                    };
                    self.note_web_failure(msg.to_string());
                }
            }
        }
    }

    /// Start the anonymous visibility probe for the current project, once.
    ///
    /// Free evidence first: if this session resolved NO credential for the
    /// project and still read the page, it is public by demonstration and
    /// no request is needed.
    fn ensure_visibility(&mut self, ctx: &Ctx) {
        if self.project.is_empty() || self.vis_asked.as_deref() == Some(&self.project) {
            return;
        }
        self.vis_asked = Some(self.project.clone());
        if ctx.client.credential_for(&self.project).is_none() {
            self.caps.visibility = capability::Visibility::Public;
            return;
        }
        spawn_visibility_probe(ctx.client.clone(), self.project.clone(), self.vis_tx.clone());
    }

    /// Take whatever the visibility probe learned. Pure bookkeeping: the
    /// verdict only ever widens or narrows what a LATER render pass may do.
    fn drain_visibility(&mut self) {
        while let Ok((project, verdict)) = self.vis_rx.try_recv() {
            if project == self.project {
                self.caps.visibility = verdict;
            }
        }
    }

    /// The poller's end of the control channel, for tests: `main` normally
    /// takes it when the poller is spawned.
    #[cfg(test)]
    fn poll_ctrl_rx_for_test(&mut self) -> &mpsc::Receiver<Duration> {
        self.poll_ctrl_rx.as_ref().expect("still held in tests")
    }

    /// Adopt the push channel's new state: retune the poller and, when the
    /// status line is showing the session summary, refresh its `sync:` tag.
    ///
    /// The retune is what makes a stale sid cheap. The old code picked 60 s
    /// from the mere existence of a cookie; now the interval is a function
    /// of a state the websocket thread has actually demonstrated.
    fn set_sync_state(&mut self, st: SyncState) {
        if self.sync_state == st {
            return;
        }
        self.sync_state = st;
        // A dead channel just means the poller is gone (shutdown).
        let _ = self.poll_ctrl_tx.send(st.poll_interval());
        self.refresh_sync_tag();
    }

    /// The `sync:` word inside the session summary, kept truthful as the
    /// state moves. Only that summary is rewritten — a live status message
    /// (a commit result, an auth note) is left alone.
    fn refresh_sync_tag(&mut self) {
        // Either wording may be on screen: the status was written in the
        // reader's language, and a test may have seeded the other one.
        let Some((at, tag)) = ["同期: ", "sync: "]
            .iter()
            .find_map(|p| self.status.find(p).map(|i| (i, *p)))
        else {
            return;
        };
        let Some(rest) = self.status.get(at + tag.len()..) else { return };
        let end = rest.find(' ').map(|i| at + tag.len() + i).unwrap_or(self.status.len());
        self.status.replace_range(at + tag.len()..end, self.sync_label());
    }

    /// What to call the live-update channel. A session with no sid is not
    /// "reconnecting" — it never had a push channel to lose.
    fn sync_label(&self) -> &'static str {
        if self.ws_attempted {
            self.sync_state.label()
        } else {
            SyncState::Polling.label()
        }
    }

    /// Is the edit session's caret on one of these source lines? Such a
    /// line is being typed into, so `app.lines` still holds the committed
    /// text while the session's buffer holds the reader's — the block is not
    /// renderable until the caret leaves and the commit lands.
    fn caret_is_inside(&self, rows: &[(usize, Line<'static>)]) -> bool {
        let Some(s) = &self.session else { return false };
        rows.iter().any(|(src, _)| *src == s.line)
    }

    /// Source lines belonging to a diagram that is being rendered right now,
    /// mapped to their position in the block. Derived from the blocks rather
    /// than the laid-out rows, so wrapped continuation rows of one source
    /// line pulse together.
    fn web_shimmer_rows(&self) -> HashMap<usize, (u16, u16)> {
        let mut out = HashMap::new();
        // Source mode shows the raw notation and never a picture, so there
        // is nothing there for the pulse to be about.
        if self.web_pending.is_empty() || self.mode == Mode::Source {
            return out;
        }
        for b in &self.blocks {
            let Block::WebRender { kind, code, rows, last_src } = b else { continue };
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            if !self.web_pending.contains(&req.cache_key()) {
                continue;
            }
            let len = rows.len() as u16;
            for (i, (src, _)) in rows.iter().enumerate() {
                out.insert(*src, (i as u16, len));
            }
        }
        out
    }

    /// Re-encode any diagram that was built for a different column cap than
    /// the pane now has. The picture keeps its place on screen — the old,
    /// wrongly-sized one stays up until the new encoding lands, which beats
    /// flickering back to source. Costs a PNG decode on the worker; the
    /// browser is not involved.
    fn rescale_diagrams(&mut self) {
        let want = self.web_cols;
        let mut keys: Vec<String> = Vec::new();
        for b in &self.blocks {
            let Block::WebRender { kind, code, last_src, .. } = b else { continue };
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            let key = req.cache_key();
            let Some(info) = self.images.get(&key) else { continue };
            if info.built_for != want && !self.web_rescaling.contains(&key) {
                keys.push(key);
            }
        }
        for key in keys {
            self.web_rescaling.insert(key.clone());
            if self
                .web_job_tx
                .send(WebJob::Rescale { gen: self.gen_now(), key: key.clone(), max_cols: want })
                .is_err()
            {
                self.web_rescaling.remove(&key);
            }
        }
    }

    /// Note that a diagram did not draw. This never touches `app.status`,
    /// so it cannot overwrite — or, on expiry, erase — a commit, auth or
    /// resync message.
    fn note_web_failure(&mut self, msg: String) {
        const SHOWN_FOR: std::time::Duration = std::time::Duration::from_secs(6);
        self.web_notice = Some((msg, std::time::Instant::now() + SHOWN_FOR));
    }

    /// Drop the diagram note once it has had its seconds, so the key hints
    /// come back. Returns whether anything changed.
    fn expire_web_notice(&mut self) -> bool {
        let Some((_, until)) = self.web_notice.as_ref() else { return false };
        if std::time::Instant::now() < *until {
            return false;
        }
        self.web_notice = None;
        true
    }

    /// What the bottom row should say, in priority order: the edit
    /// session's keys, then the links on the cursor line, then whatever
    /// wrote to `status` (commits, auth, resync — the things a reader must
    /// not miss), then a diagram note, and finally the key hints.
    /// Why the page might not be showing the newest state, in a few
    /// characters — or nothing at all when it is.
    ///
    /// Live sync with nothing held is the quiet, normal case. Everything
    /// else is worth a word: `poll` means web-side edits take up to three
    /// seconds, and `held` means they have ARRIVED and are waiting for the
    /// caret line to be committed (remote state is never installed over an
    /// unsaved line). Without this, both look like "the viewer got slow".
    fn sync_notice(&self) -> Option<String> {
        if !self.ws_pending.is_empty() {
            let dirty = self
                .session
                .as_ref()
                .map(|s| s.input.buf != s.orig)
                .unwrap_or(false);
            let why = if self.move_mode.is_some() {
                // A drag holds the whole page's order, so remote edits wait
                // for it exactly as they wait for an unsaved line.
                ts!("つかんでいるブロックを待っています", "waiting on the grabbed block")
            } else if dirty {
                ts!("編集中の行を待っています", "waiting on the line being edited")
            } else {
                ts!("適用待ち", "waiting to apply")
            };
            return Some(t!("⟳ {} 件{}", "⟳ {} update(s) — {}", self.ws_pending.len(), why));
        }
        match self.sync_state {
            capability::SyncState::Live => None,
            capability::SyncState::Polling => Some(t!("同期: poll", "sync: poll")),
            capability::SyncState::Reconnecting => Some(t!("同期: 再接続中", "sync: reconnecting")),
        }
    }

    fn hint_text(&self, cursor_links: &[LinkItem]) -> String {
        match self.sync_notice() {
            Some(tag) => format!("{tag} · {}", self.hint_body(cursor_links)),
            None => self.hint_body(cursor_links),
        }
    }

    fn hint_body(&self, cursor_links: &[LinkItem]) -> String {
        // The move mode says its own name and its own keys, in words: the
        // grabbed block is highlighted, but a highlight is a colour and a
        // colour alone must not be what tells the reader where they are.
        // A refusal ("no sibling that way") rides along behind them.
        if self.move_mode.is_some() {
            let keys = t!(
                "MOVE — j/k 1行 · J/K 兄弟 · h/l 字下げ · Esc 確定",
                "MOVE — j/k line · J/K sibling · h/l indent · Esc commit"
            );
            return match self.status.is_empty() {
                true => keys,
                false => format!("{keys} · {}", self.status),
            };
        }
        if self.outline_prefix {
            return t!(
                "OUTLINE h/j/k/l 行 左/下/上/右 · H/J/K/L ブロック · Esc 取消",
                "OUTLINE h/j/k/l line left/down/up/right · H/J/K/L block · Esc cancel"
            );
        }
        if let Some(s) = self.session.as_ref() {
            let dirty = s.input.buf != s.orig;
            return t!("{}↑↓ 移動 · Enter 改行 · ⌫@行頭 前の行と結合 · Tab 字下げ · Esc 終了", "{}↑↓ move · Enter new line · ⌫@BOL join · Tab indent · Esc done",
                if dirty { "● " } else { "" }
            );
        }
        if !cursor_links.is_empty() {
            let listed: Vec<String> = cursor_links
                .iter()
                .take(9)
                .enumerate()
                .map(|(i, l)| {
                    // A link with nothing behind it does not open: Enter
                    // starts the page. Better said before it is pressed.
                    let mark = match l {
                        LinkItem::Page(t) if self.links.missing(t) => ts!("(未作成)", "(uncreated)"),
                        _ => "",
                    };
                    format!("{}:{}{mark}", i + 1, l.label())
                })
                .collect();
            return t!("Enter/f で開く → {}", "Enter/f open → {}", listed.join("  "));
        }
        if !self.status.is_empty() {
            return self.status.clone();
        }
        if let Some((msg, _)) = self.web_notice.as_ref() {
            return msg.clone();
        }
        if self.editable {
            t!("j/k 移動  Enter リンク  e 編集  o 行追加  u 取り消し  w ブラウザ  ? ヘルプ  q 終了", "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit")
        } else {
            t!("j/k 移動  Enter リンク  w ブラウザ  ? ヘルプ  q 終了  · 読み取り専用", "j/k move  Enter link  w browser  ? help  q quit  · read-only")
        }
    }

    /// The local model may have drifted from the server. Deliberately
    /// sticky: a later commit succeeding says nothing about the edit that
    /// did not, so only an authoritative page install clears it.
    fn mark_desynced(&mut self) {
        self.web_unsynced = true;
    }

    /// A whole page arrived from the server and replaced the local lines,
    /// so whatever had drifted is gone. This is the ONLY way the flag
    /// clears; it must never be called on a partial or local-only update,
    /// which would re-open the window it exists to close.
    fn mark_synced(&mut self) {
        self.web_unsynced = false;
    }

    /// The page source has changed: any render queued before this moment
    /// would be filed under a hash it no longer matches.
    fn bump_src_epoch(&self) {
        self.src_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn src_epoch_now(&self) -> u64 {
        self.src_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The server's state has moved on: any poll response fetched before
    /// this moment is stale.
    fn bump_server_epoch(&self) {
        self.server_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn server_epoch_now(&self) -> u64 {
        self.server_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn gen_now(&self) -> u64 {
        self.web_gen.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Install finished web renders. A result from an older page generation
    /// is dropped WITHOUT touching any state: the reader has moved on, and
    /// the same key can legitimately be pending again for the current page
    /// (navigate away and back), so clearing by key alone would cancel the
    /// live request and leave the diagram pulsing forever.
    fn drain_web_renders(&mut self) -> bool {
        let mut changed = false;
        // Refusals from this drain, judged together once the loop ends.
        let mut denied: Vec<(String, Option<RenderCapability>)> = Vec::new();
        while let Ok(msg) = self.web_rx.try_recv() {
            if msg.gen != self.gen_now() {
                continue;
            }
            let WebMsg { key, rescale, attempted, res, .. } = msg;
            if rescale {
                // A rescale only ever changes the SIZE of a picture that is
                // already on screen. If it failed, the reader keeps the
                // size they had: no error, no notice, no lost diagram.
                self.web_rescaling.remove(&key);
                if let WebOutcome::Drawn(info) = res {
                    self.images.insert(key, info);
                    changed = true;
                }
                continue;
            }
            self.web_pending.remove(&key);
            match res {
                WebOutcome::Drawn(info) => {
                    self.images.insert(key, info);
                }
                // Not on disk, and this pass could not draw. The block goes
                // back to being source, silently — `R` will pick it up.
                WebOutcome::Missing => {
                    self.web_missing.insert(key);
                }
                // The BROWSER was refused. The REST credential is untouched:
                // edits and reads carry on exactly as before. Collected
                // rather than handled here — one batch refuses every key in
                // it at once, and handling them one at a time made the
                // second key see the first key's bookkeeping and give up.
                WebOutcome::Denied => {
                    denied.push((key, attempted));
                }
                // Nothing is recorded: not an image, not a miss, not an
                // error. The next pass sees the new source and asks again.
                WebOutcome::Stale => {}
                WebOutcome::Failed(e) => {
                    self.note_web_failure(t!("diagram: {e}（ソースを表示します）", "diagram: {e} (showing source)"));
                    self.web_errors.insert(key, e);
                }
            }
            changed = true;
        }
        if !denied.is_empty() {
            self.note_denied(denied);
        }
        // An `R` that arrived while the cache probe was out: now that the
        // misses are known, serve it. Once — the flag is cleared whether or
        // not there turned out to be anything to draw.
        if self.web_manual_wanted && self.web_pending.is_empty() {
            self.web_manual_wanted = false;
            self.start_web_renders(capability::Trigger::Manual);
        }
        changed
    }

    /// Install images that finished loading. Returns true if anything
    /// changed (the caller rebuilds the layout, since heights shift). Cheap:
    /// the worker already did the decoding and encoding.
    /// Ask about every page link on the page whose fate is not known yet.
    ///
    /// Two things are deliberately left out:
    ///
    /// * **the caret line**, while a session is open. That line is being
    ///   written; `[新` on the way to `[新しいページ]` is not a question
    ///   worth asking, and the line is drawn as raw source anyway, so no
    ///   answer could show. Moving off the line is what submits it.
    /// * **cross-project links**, whose existence this reading of
    ///   `relatedPages` never covered.
    fn probe_unknown_links(&mut self) {
        let Some(tx) = self.link_probe_tx.as_ref() else { return };
        // Scan at once when something could have changed the answer — the
        // text was edited, or the caret left the line it was writing on,
        // which is the moment a new link becomes a question worth asking.
        // Otherwise keep a slow beat: with nothing to ask (the usual case)
        // the scan itself is the only cost, and it should not run every
        // frame.
        let key = (self.session.as_ref().map(|s| s.line), self.src_epoch_now());
        if key == self.link_scan_key && self.link_scan_at.elapsed() < LINK_SCAN_EVERY {
            return;
        }
        self.link_scan_key = key;
        self.link_scan_at = Instant::now();
        let editing = self.session.as_ref().map(|s| s.line);
        for (i, line) in self.lines.iter().enumerate() {
            if Some(i) == editing {
                continue;
            }
            for item in links_on_line(&mask_inline_code(&line.text)) {
                let LinkItem::Page(title) = item else { continue };
                if self.links.exists(&title).is_some() {
                    continue;
                }
                let key = cosense::render::title_lc(&title);
                if !self.link_pending.insert(key) {
                    continue;
                }
                let _ = tx.send(LinkProbe {
                    project: self.project.clone(),
                    title,
                    asked_by: self.page_id.clone(),
                });
            }
        }
    }

    /// Take the answers that came back. `true` = something changed, so the
    /// page has to be rendered again with them.
    fn drain_link_probes(&mut self) -> bool {
        let mut changed = false;
        while let Ok((probe, live)) = self.link_probe_rx.try_recv() {
            self.link_pending.remove(&cosense::render::title_lc(&probe.title));
            // An answer about a project we have since left says nothing
            // about the page on screen.
            if probe.project != self.project {
                continue;
            }
            // "Nobody but the asking page writes this" is only an answer
            // for the page that asked (see `LinkTruth::forget_dead`); if
            // the reader has moved on, this page may be the second one
            // writing it, and that is a different question.
            if !live && probe.asked_by != self.page_id {
                continue;
            }
            self.links.learn(&probe.title, live);
            changed = true;
        }
        changed
    }

    fn drain_images(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, res)) = self.image_rx.try_recv() {
            self.pending.remove(&url);
            match res {
                Ok(info) => {
                    self.images.insert(url, info);
                }
                Err(e) => {
                    self.image_errors
                        .insert(url.clone(), format!("[image failed: {} — {e}]", short(&url)));
                }
            }
            changed = true;
        }
        changed
    }

    /// Everything Enter/f or a mouse click can follow at one source index.
    /// On a related row (virtual line below the body) this is its one target.
    fn links_at_src(&self, src: usize) -> Vec<LinkItem> {
        if src >= self.lines.len() {
            if self.mode == Mode::View {
                if let Some(item) = self.virtual_items.get(src - self.lines.len()) {
                    return vec![item.clone()];
                }
            }
            return Vec::new();
        }
        let Some(text) = self.lines.get(src).map(|l| l.text.as_str()) else {
            return Vec::new();
        };
        let text = &mask_inline_code(text);
        let mut items: Vec<LinkItem> = Vec::new();
        // A code block or a table is a file Cosense will hand over.
        if let Some(item) = block_export_link(text, src) {
            items.push(item);
        }
        items.extend(links_on_line(text));
        items.extend(labelled_urls(text).into_iter().map(|(label, url)| {
            link_item_for_url(label, url)
        }));
        items
    }

    fn cursor_line_links(&self) -> Vec<LinkItem> {
        self.links_at_src(self.cursor)
    }

    /// Save an uploaded file to the download directory in the background;
    /// the result lands in `file_rx` and is reported (and opened) by the
    /// event loop.
    fn start_download(&mut self, ctx: &Ctx, label: String, url: String) {
        let dest = download_path_in(&ctx.download_dir, &label, &url);
        let tx = self.file_tx.clone();
        let fetcher = Arc::clone(&ctx.fetcher);
        self.status = t!("ダウンロード中 {label}…", "downloading {label}…");
        std::thread::spawn(move || {
            let res = fetcher.download_to(&url, &dest).map(|_| dest).map_err(|e| e.to_string());
            let _ = tx.send((label, res));
        });
    }

    /// Report finished downloads: open the file and say where it went.
    fn drain_downloads(&mut self) {
        while let Ok((label, res)) = self.file_rx.try_recv() {
            match res {
                Ok(path) => {
                    let shown = path.display().to_string();
                    let opened = open_in_browser(&shown);
                    self.status = if opened {
                        t!("保存しました {shown} · 開きました", "saved {shown} · opened")
                    } else {
                        t!("保存しました {shown}", "saved {shown}")
                    };
                }
                Err(e) => self.status = t!("ダウンロードに失敗しました: {label} — {e}", "download failed: {label} — {e}"),
            }
        }
    }

    /// The source line index under the cursor, if the page has any lines.
    fn cursor_src(&self) -> Option<usize> {
        if self.cursor < self.lines.len() {
            Some(self.cursor)
        } else {
            None
        }
    }

    /// Index into `comments` of the comment covering the cursor line.
    fn comment_at_cursor(&self) -> Option<usize> {
        let src = self.cursor_src()?;
        self.comments.iter().position(|c| {
            c.project == self.project && c.title == self.title && c.covers(src)
        })
    }

    /// Browser URL for the current page, deep-linked to the cursor line.
    fn cursor_url(&self) -> String {
        let title = urlencode_component(&self.title);
        match self.cursor_src().and_then(|s| self.lines.get(s)) {
            Some(l) if !l.id.is_empty() => {
                format!("https://scrapbox.io/{}/{}#{}", self.project, title, l.id)
            }
            _ => format!("https://scrapbox.io/{}/{}", self.project, title),
        }
    }
}

/// A Cosense page URL from the command line, as `(project, title, lineId)`:
/// `https://scrapbox.io/<project>/<title>#<lineId>` (title and fragment
/// optional; the title is percent-encoded as the browser shows it). Also
/// accepts the `cosen.se` short host and a scheme-less `scrapbox.io/…`.
/// `None` for anything that is not such a URL (a plain project name).
fn parse_page_url(arg: &str) -> Option<(String, Option<String>, Option<String>)> {
    let rest = arg
        .strip_prefix("https://")
        .or_else(|| arg.strip_prefix("http://"))
        .unwrap_or(arg);
    let rest = ["scrapbox.io/", "www.scrapbox.io/", "cosen.se/"]
        .iter()
        .find_map(|h| rest.strip_prefix(h))?;
    // the fragment is the line id; a query string is not part of the title
    let (path, fragment) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    let mut segs = path.splitn(2, '/');
    let project = segs.next().filter(|p| !p.is_empty())?.to_string();
    let title = segs
        .next()
        .map(|t| t.trim_end_matches('/'))
        .filter(|t| !t.is_empty())
        .map(percent_decode);
    let line_id = fragment.filter(|f| !f.is_empty()).map(str::to_string);
    Some((project, title, line_id))
}

/// Decode `%XX` escapes (UTF-8) as browsers encode page titles; a stray
/// `%` that is not an escape is kept as is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn urlencode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

/// Open a URL in the default browser (macOS `open`, Linux `xdg-open`).
fn open_in_browser(url: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(cmd)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
            true
        })
        .unwrap_or(false)
}

impl App {
    /// Make sure the CURRENT project's member table is usable for resolving
    /// `want` (a userId), fetching or refetching when:
    /// - the project has no table yet (first `t` in this project), or
    /// - the table is older than `MEMBERS_TTL`, or
    /// - `want` is not in the table and the table is older than
    ///   `MEMBERS_MISS_COOLDOWN` (a newly joined member; the cooldown
    ///   stops an id that will never resolve from refetching every time).
    /// A failed fetch stores an empty table so the same rules pace retries.
    /// Called on demand (the `t` overlay), never per frame.
    fn ensure_members(&mut self, ctx: &Ctx, want: Option<&str>) {
        let stale = match self.members.get(&self.project) {
            None => true,
            Some(m) => {
                let age = m.fetched_at.elapsed();
                let missing = want.map_or(false, |id| !id.is_empty() && !m.names.contains_key(id));
                age > MEMBERS_TTL || (missing && age > MEMBERS_MISS_COOLDOWN)
            }
        };
        if !stale {
            return;
        }
        let names: HashMap<String, String> = match ctx.client.list_members_in(&self.project) {
            Ok(ms) => ms
                .into_iter()
                .map(|m| {
                    let name = if m.display_name.is_empty() { m.name } else { m.display_name };
                    (m.id, name)
                })
                .collect(),
            Err(e) => {
                // Say so: a silent empty table would look like "unknown
                // member" and hide a permission or network problem.
                self.status = t!("メンバー一覧を取得できません: {} — {e}", "member list failed for {}: {e}", self.project);
                HashMap::new()
            }
        };
        self.members
            .insert(self.project.clone(), MembersCache { names, fetched_at: Instant::now() });
    }

    /// Display name for a userId in the current project, from the cached
    /// table; an unresolved id shows its first 8 characters.
    fn member_name(&self, id: &str) -> String {
        if id.is_empty() {
            return "(unknown)".into();
        }
        self.members
            .get(&self.project)
            .and_then(|m| m.names.get(id))
            .cloned()
            .unwrap_or_else(|| format!("({}…)", &id[..id.len().min(8)]))
    }

    /// True if the line was edited after the user last saw this page (or
    /// the page was never seen). Drives the telomere tint.
    fn line_unread(&self, l: &PageLine) -> bool {
        unread_since(l.updated, self.read_at)
    }

    /// Number of unread lines on this page.
    fn unread_count(&self) -> usize {
        self.lines.iter().filter(|l| self.line_unread(l)).count()
    }

    /// True if a source line is covered by any saved comment.
    fn src_has_comment(&self, src: usize) -> bool {
        self.comments
            .iter()
            .any(|c| c.project == self.project && c.title == self.title && c.covers(src))
    }

    /// Columns available to the text for a `width`-wide body in `mode`:
    /// frame/caret + telomere + one blank column on the left, and a blank,
    /// scrollbar, and frame column on the right. Source mode also spends
    /// the line-number label.
    fn text_width(mode: Mode, width: u16) -> usize {
        let w = width as usize;
        match mode {
            Mode::View => w.saturating_sub(6).max(1),
            Mode::Source => w.saturating_sub(6 + SOURCE_NUM_W).max(1),
        }
    }

    /// The page as the screen sees it: the committed lines, except the
    /// caret line, which is the session's working text. Structure has to
    /// be judged on what the eye is reading, not on what the server last
    /// heard — otherwise typing `code:` only becomes a code block one
    /// commit later.
    fn source_texts(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.lines.iter().map(|l| l.text.as_str()).collect();
        if let Some(s) = self.session.as_ref() {
            if let Some(slot) = v.get_mut(s.line) {
                *slot = s.input.buf.as_str();
            }
        }
        v
    }

    /// Is `line` inside a `code:` block? Bullet display and caret math both
    /// need this: in code the leading whitespace is content, so it is not
    /// drawn as a bullet and the caret does not step over one.
    fn code_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        (line < self.lines.len())
            .then(|| cosense::render::code_span_at(&self.source_texts(), line))
            .flatten()
    }

    /// The `table:` block the line belongs to, if any. A table row is not
    /// an outline row: its indent is structure and its TABs separate
    /// cells, so the session has to know which one it is standing on.
    fn table_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        (line < self.lines.len())
            .then(|| cosense::render::table_span_at(&self.source_texts(), line))
            .flatten()
    }

    /// A block whose lines are RAW: a `code:` block or a `table:` block.
    /// Both hold structure in their indent rather than an outline level,
    /// so the caret line is drawn without a bullet and the caret arithmetic
    /// hangs from the block's own gutter.
    fn raw_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        self.code_span_at_line(line).or_else(|| self.table_span_at_line(line))
    }

    /// Put the caret on an edit's own location (see `edit_focus`). The
    /// session moves with it, so undo/redo show what changed instead of
    /// leaving the caret wherever it happened to be. Returns whether the
    /// line was still there to stand on.
    fn focus_edit(&mut self, focus: Option<(String, usize)>) -> bool {
        let Some((id, caret)) = focus else { return false };
        let Some(line) = self.lines.iter().position(|l| l.id == id) else { return false };
        let text = self.lines[line].text.clone();
        self.cursor = line;
        self.follow = true;
        self.laid_width = 0;
        if let Some(s) = self.session.as_mut() {
            s.line = line;
            s.input = Input { buf: text.clone(), cur: caret.min(text.len()) };
            s.orig = text;
            s.want_col = None;
            s.sel_from = None;
        }
        true
    }

    /// The width the body rows are wrapped at. After an edit the layout is
    /// stale (`laid_width = 0`) until the next draw; the last text rect is
    /// the honest answer in between.
    fn session_wrap_width(&self) -> usize {
        if self.laid_width > 0 {
            return Self::text_width(self.mode, self.laid_width);
        }
        if self.text_rect.width > 0 {
            return self.text_rect.width as usize;
        }
        // No geometry at all (nothing has been drawn yet): treat lines as
        // unwrapped rather than as one column wide, which would turn every
        // line into a stack of single characters.
        usize::MAX
    }

    /// Convenience for the places that only care whether it is code.
    #[cfg(test)]
    fn line_in_code(&self, line: usize) -> bool {
        self.code_span_at_line(line).is_some()
    }

    /// View mode: rendered blocks wrapped to the pane, each tagged with the
    /// source line it came from. The edit session's caret line renders as
    /// RAW SOURCE (cosense web: the line with the caret reveals its
    /// notation; everything else stays rendered).
    fn content_view(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::View, width);
        let edit: Option<(usize, &str)> =
            self.session.as_ref().map(|s| (s.line, s.input.buf.as_str()));
        // Inside a `code:` block the caret line is code, not an outline
        // row: its leading whitespace is content and must not be drawn as
        // a bullet. Judged on the WORKING text, so typing `code:` turns
        // the bullets off as you type it, not one commit later.
        let edit_code = edit.and_then(|(line, _)| self.raw_span_at_line(line));
        // The character selection, in DISPLAY byte offsets — the caret line
        // is drawn through `session_display`, so the span has to be mapped
        // the same way the caret is.
        // The character selection, in DISPLAY byte offsets — the caret
        // line is drawn through `session_display`, so the span has to be
        // mapped the same way the caret is. A range across lines reaches
        // the caret line to its near edge; that is what `caret_sel_bytes`
        // answers even when the range does not fit on the line.
        let sel_disp: Option<(usize, usize)> = self.session.as_ref().and_then(|s| {
            let (a, b) = s.caret_sel_bytes()?;
            Some((
                display_caret(&s.input.buf, a, edit_code),
                display_caret(&s.input.buf, b, edit_code),
            ))
        });
        let raw_rows = |content: &mut Vec<Row>, buf: &str, src: usize| {
            // The indent renders as its bullet (dim) — same shape as the
            // view — while the underlying data stays whitespace.
            let disp = session_display(buf, edit_code);
            let prefix = if edit_code.is_some() { 0 } else { display_prefix_bytes(buf) };
            let wrapped = SessionWrap::new(&disp, text_w, session_hang(buf, edit_code));
            let mut at = 0usize; // byte offset of this segment within `disp`
            for (k, seg) in wrapped.segs.iter().cloned().enumerate() {
                let seg_start = at;
                at += seg.len();
                let mut spans: Vec<Span<'static>> = Vec::new();
                if wrapped.indent_of(k) > 0 {
                    // The continuation hangs under the text, not under the
                    // bullet — the same shape READ has always had.
                    spans.push(Span::raw(" ".repeat(wrapped.indent_of(k))));
                }
                let mut push = |text: &str, selected: bool, dim: bool| {
                    if text.is_empty() {
                        return;
                    }
                    let mut st = Style::default();
                    if dim {
                        st = st.fg(Color::DarkGray);
                    }
                    if selected {
                        // REVERSED, not a background colour: the caret line
                        // is already painted with the cursor band, and
                        // `SEL_BG` is that same grey — a selection drawn
                        // with it is invisible exactly where it always is.
                        // Swapping fg/bg contrasts against any background,
                        // on any terminal, without inventing a colour.
                        st = st.add_modifier(Modifier::REVERSED);
                    }
                    spans.push(Span::styled(text.to_string(), st));
                };
                // Cut this segment into [before | selected | after], with
                // the bullet prefix (first row only) staying dim.
                let (lo, hi) = match sel_disp {
                    Some((a, b)) => (
                        a.saturating_sub(seg_start).min(seg.len()),
                        b.saturating_sub(seg_start).min(seg.len()),
                    ),
                    None => (seg.len(), seg.len()),
                };
                let (lo, hi) = (floor_boundary(&seg, lo), floor_boundary(&seg, hi));
                let dim_to = if k == 0 { prefix.min(seg.len()) } else { 0 };
                for (from, to, selected) in
                    [(0, lo, false), (lo, hi, true), (hi, seg.len(), false)]
                {
                    if from >= to {
                        continue;
                    }
                    // The dim prefix may end inside this piece.
                    let cut = dim_to.clamp(from, to);
                    push(&seg[from..cut], selected, true);
                    push(&seg[cut..to], selected, false);
                }
                content.push(Row::Line { line: Line::from(spans), src, start: 0, hang: 0 });
            }
        };
        let mut content: Vec<Row> = Vec::new();
        for (b, &src) in self.blocks.iter().zip(self.srcs.iter()) {
            if let Some((eline, ebuf)) = edit {
                if src == eline
                    && !matches!(b, Block::Table(_) | Block::WebRender { .. })
                {
                    raw_rows(&mut content, ebuf, src);
                    continue;
                }
            }
            match b {
                Block::Text(line) => {
                    // bullets / code hang their continuation rows under the
                    // text; quotes repeat their bar on every row
                    for w in wrap_line_parts(line, text_w, &hanging_prefix(line)) {
                        content.push(Row::Line {
                            line: w.line,
                            src,
                            start: w.start,
                            hang: w.hang,
                        });
                    }
                }
                Block::Blank => content.push(Row::Blank { src }),
                Block::Table(t) => {
                    // Table rows carry their OWN source lines (one Scrapbox
                    // line per row), so the cursor addresses rows directly.
                    // The session's caret row swaps to raw source once.
                    let mut emitted_raw = false;
                    for (line, row_src) in t.layout(text_w) {
                        if let Some((eline, ebuf)) = edit {
                            if row_src == eline {
                                if !emitted_raw {
                                    raw_rows(&mut content, ebuf, row_src);
                                    emitted_raw = true;
                                }
                                continue;
                            }
                        }
                        content.push(Row::Line { line, src: row_src, start: 0, hang: 0 });
                    }
                }
                Block::WebRender { kind, code, rows, last_src } => {
                    let key = self
                        .web_request(*kind, code, *last_src)
                        .map(|r| r.cache_key());
                    // While the edit session is inside this block the reader
                    // is working on the raw source, so the picture steps
                    // aside — the existing source-editing contract wins.
                    let editing_here = edit
                        .map(|(eline, _)| rows.iter().any(|(rsrc, _)| *rsrc == eline))
                        .unwrap_or(false);
                    let art = if editing_here {
                        None
                    } else {
                        key.as_ref().and_then(|k| self.images.get(k).map(|i| (k, i)))
                    };
                    if let Some((k, info)) = art {
                        content.push(Row::Image {
                            url: k.clone(),
                            height: info.cells_h.max(1),
                            src: *last_src,
                            indent: 0,
                            item: false,
                        });
                        continue;
                    }
                    // No artifact (yet, or ever): the plain code block, with
                    // the session's caret row swapped to raw source.
                    for (rsrc, line) in rows {
                        if let Some((eline, ebuf)) = edit {
                            if *rsrc == eline {
                                raw_rows(&mut content, ebuf, *rsrc);
                                continue;
                            }
                        }
                        for w in wrap_line_parts(line, text_w, &hanging_prefix(line)) {
                            content.push(Row::Line {
                                line: w.line,
                                src: *rsrc,
                                start: w.start,
                                hang: w.hang,
                            });
                        }
                    }
                }
                Block::Inline { indent, item, parts } => {
                    let indent = (*indent).min(text_w.saturating_sub(4));
                    content.push(self.inline_row(parts, indent, *item, text_w, src));
                }
                Block::Image { url, indent, item } => {
                    let indent = (*indent).min(text_w.saturating_sub(4));
                    if let Some(info) = self.images.get(url) {
                        content.push(Row::Image {
                            url: url.clone(),
                            height: info.cells_h.max(1),
                            src,
                            indent,
                            item: *item,
                        });
                    } else if let Some(msg) = self.image_errors.get(url) {
                        content.push(Row::ImageError {
                            msg: msg.clone(),
                            src,
                            indent,
                            item: *item,
                        });
                    } else {
                        // still downloading — reserve space so the page is
                        // readable now and the image slots in when it lands
                        content.push(Row::ImageLoading { src, indent, item: *item });
                    }
                }
            }
        }
        content
    }

    /// Lay a mixed text-and-picture line out for this pane. A picture the
    /// viewer has not decoded yet still takes part: it reserves a
    /// placeholder-sized box so the line does not jump when it lands.
    fn inline_row(
        &self,
        parts: &[cosense::render::InlinePart],
        indent: usize,
        item: bool,
        text_w: usize,
        src: usize,
    ) -> Row {
        use cosense::render::InlinePart;
        let items: Vec<Inline> = parts
            .iter()
            .map(|p| match p {
                InlinePart::Text(line) => Inline::Text(line.clone()),
                InlinePart::Image(url) => {
                    let (w, h) = match self.images.get(url) {
                        Some(info) => (info.cells_w, info.cells_h.max(1)),
                        None => (IMAGE_PLACEHOLDER_W, IMAGE_PLACEHOLDER_H),
                    };
                    Inline::Image { url: url.clone(), w, h }
                }
            })
            .collect();
        let (images, texts, height) = layout_inline(&items, indent, text_w);
        Row::Inline {
            src,
            height,
            indent,
            item,
            images: images.into_iter().map(|p| (p.row, p.col, p.what)).collect(),
            texts: texts.into_iter().map(|p| (p.row, p.col, p.what)).collect(),
        }
    }

    /// Build the related-pages sections that live BELOW and OUTSIDE the page    /// Build the related-pages sections that live BELOW and OUTSIDE the page
    /// frame. Rows remain cursor-addressable virtual lines; only the visual
    /// containment changes.
    fn related_rows(&self, text_w: usize) -> Vec<Row> {
        if self.related.is_empty() {
            return Vec::new();
        }
        let dim = Style::default().fg(CHROME_DIM);
        // Neither underlined nor coloured for being links. That rule is
        // for finding the pressable thing IN A LINE OF PROSE; these rows
        // are nothing but links, one per line, under a heading that says
        // so. With link-ness needing no mark, the colour is free to carry
        // the one thing that differs between rows — whether the page has
        // been seen since it changed — in the same blue the telomere uses
        // for it. Same reading as the index's list.
        let unread_style = Style::default().fg(CHROME_CARET);
        let read_style = Style::default();
        let mut vsrc = self.lines.len();
        let mut rows = vec![Row::Card { line: Line::from("") }];
        for sec in &self.related {
            let head = format!("── {} ", sec.heading);
            let used = str_width(&head);
            let fill = "─".repeat(text_w.saturating_sub(used));
            rows.push(Row::Card {
                line: Line::from(Span::styled(format!("{head}{fill}"), dim)),
            });
            for e in &sec.entries {
                let style = if e.unread { unread_style } else { read_style };
                rows.push(Row::Line { line: related_row(e, text_w, style), src: vsrc, start: 0, hang: 0 });
                vsrc += 1;
            }
            rows.push(Row::Card { line: Line::from("") });
        }
        rows
    }

    /// Source mode: the raw Scrapbox notation, one numbered row per source
    /// line (wrapped). Same `src` tagging as view mode, so the cursor,
    /// comments and telomere all line up across the toggle.
    fn content_source(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::Source, width);
        let num_style = Style::default().fg(Color::DarkGray);
        let mut content: Vec<Row> = Vec::new();
        for (src, l) in self.lines.iter().enumerate() {
            let raw = Line::from(l.text.clone());
            for (k, wrapped) in wrap_line(&raw, text_w).into_iter().enumerate() {
                // number only on a line's first display row
                let label = if k == 0 {
                    format!("{:>4} ", src + 1)
                } else {
                    " ".repeat(SOURCE_NUM_W)
                };
                let mut spans: Vec<Span<'static>> = vec![Span::styled(label, num_style)];
                spans.extend(wrapped.spans);
                content.push(Row::Line { line: Line::from(spans), src, start: 0, hang: 0 });
            }
        }
        content
    }

    /// Rebuild the visual rows for `width`: content rows (with source
    /// attribution) plus inline comment cards inserted after each comment's
    /// anchor block. Called on resize and whenever comments change.
    fn rebuild(&mut self, width: u16) {
        // Diagrams must fit the pane: the draw clips to the text column, so
        // an image encoded wider than the pane loses its right-hand side.
        // Recorded here (the layout is the only place the width is known)
        // and acted on by `rescale_diagrams`.
        self.web_cols = diagram_max_cols(Self::text_width(self.mode, width) as u16);
        // 1. Page rows and related rows are separate visual regions. The
        // page is boxed; related sections are appended after its FrameEnd.
        // The edit session (and source mode) hides the related sections:
        // the caret line is raw source and the region below the frame is
        // pure page content.
        let (content, related): (Vec<Row>, Vec<Row>) = match self.mode {
            Mode::View => {
                let text_w = Self::text_width(Mode::View, width);
                let related = if self.session.is_none() {
                    self.related_rows(text_w)
                } else {
                    Vec::new()
                };
                (self.content_view(width), related)
            }
            Mode::Source => (self.content_source(width), Vec::new()),
        };

        // 2. compute each comment's anchor: the last content index whose src
        //    lies in the comment's range (so the card sits under its block).
        let mut cards_after: HashMap<usize, Vec<usize>> = HashMap::new(); // content idx -> comment idxs
        for (ci, c) in self.comments.iter().enumerate() {
            // Only weave cards for comments that belong to THIS page.
            if c.project != self.project || c.title != self.title {
                continue;
            }
            let mut anchor: Option<usize> = None;
            for (idx, row) in content.iter().enumerate() {
                if let Some(s) = row.src() {
                    if c.start <= s && s <= c.end {
                        anchor = Some(idx);
                    }
                }
            }
            let key = anchor.unwrap_or(content.len().saturating_sub(1));
            cards_after.entry(key).or_default().push(ci);
        }

        // 3. weave cards in (cards span the text column, not the gutters)
        let mut rows: Vec<Row> = Vec::new();
        let width_cols = match self.mode {
            Mode::View => Self::text_width(Mode::View, width),
            // source rows carry their number label inline, so the card
            // spans label + text
            Mode::Source => Self::text_width(Mode::Source, width) + SOURCE_NUM_W,
        };
        for (idx, row) in content.into_iter().enumerate() {
            rows.push(row);
            if let Some(cidxs) = cards_after.get(&idx) {
                for &ci in cidxs {
                    for line in card_lines(&self.comments[ci], width_cols) {
                        rows.push(Row::Card { line });
                    }
                }
            }
        }
        // Painted as └───┘ by `ui`; related rows begin after it.
        rows.push(Row::FrameEnd);
        rows.extend(related);
        self.rows = rows;
        self.web_shimmer = self.web_shimmer_rows();
        self.laid_width = width;
        self.clamp_cursor();
    }

    /// Snap the cursor onto a source line that actually has display rows.
    /// Table body lines, for instance, render into the table block of their
    /// header line and so own no rows of their own; a cursor left there
    /// (e.g. after a Tab from source mode) moves to the nearest line that
    /// does — searching UP first, since the block that visibly contains the
    /// line belongs to an earlier source line, then down. akapen's
    /// `max_cursor` clamp, plus that guard.
    fn clamp_cursor(&mut self) {
        let n = self.src_count();
        if n == 0 || self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = self.cursor.min(n - 1);
        if self.cursor_rows().is_some() {
            return;
        }
        // A drawn diagram collapses its whole source span into ONE image
        // row, owned by the block's last line. A cursor anywhere else in
        // that span owns no row — and the generic search below goes UP,
        // landing the reader on the line BEFORE the diagram rather than on
        // the diagram they were just editing. Send it to the row the block
        // actually has, which is also where it visually already is.
        if let Some(owner) = self.diagram_row_owner(self.cursor) {
            self.cursor = owner;
            return;
        }
        let up = (0..self.cursor).rev().find(|&s| self.src_rows(s).is_some());
        let down = (self.cursor + 1..n).find(|&s| self.src_rows(s).is_some());
        if let Some(s) = up.or(down) {
            self.cursor = s;
        }
    }

    /// If `src` lies inside a diagram block that is currently showing its
    /// picture, the source line that owns that picture's row.
    fn diagram_row_owner(&self, src: usize) -> Option<usize> {
        self.blocks.iter().find_map(|b| {
            let Block::WebRender { rows, last_src, .. } = b else { return None };
            if !rows.iter().any(|(rsrc, _)| *rsrc == src) {
                return None;
            }
            // Only when the block really is a picture right now: an undrawn
            // one renders its source lines normally, and they own rows.
            self.src_rows(*last_src).map(|_| *last_src)
        })
    }

    /// The display rows occupied by source line `src`, as an inclusive
    /// `(first, last)` row-index range — `None` if it renders to nothing.
    /// Card rows woven in between count toward the range (like akapen's
    /// `source_starts[i]..source_starts[i+1]` span including comment cards),
    /// so following the cursor keeps the line's card on screen too.
    fn src_rows(&self, src: usize) -> Option<(usize, usize)> {
        let first = self.rows.iter().position(|r| r.src() == Some(src))?;
        let last = self.rows.iter().rposition(|r| r.src() == Some(src))?;
        Some((first, last))
    }

    /// The display rows of the cursor's source line (see `src_rows`).
    fn cursor_rows(&self) -> Option<(usize, usize)> {
        self.src_rows(self.cursor)
    }

    /// Move the cursor by one display row (j/k).
    fn move_cursor(&mut self, forward: bool) {
        self.move_cursor_display(if forward { 1 } else { -1 });
    }

    /// Move the cursor by `delta` display rows and snap to a source line
    /// (akapen's `move_cursor_display`): leaving downward counts from the
    /// line's LAST row and upward from its FIRST row, so wrapped
    /// continuations are jumped past. If the target row has no source
    /// (a comment card), the nearest sourced row onward in the direction
    /// of travel is taken — falling back to the other direction at the
    /// document's ends so the cursor always lands somewhere.
    fn move_cursor_display(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        let Some((first, last)) = self.cursor_rows() else { return };
        let n = self.rows.len() as isize;
        let cur = if delta > 0 { last } else { first } as isize;
        let target = (cur + delta).clamp(0, n - 1) as usize;
        let found = if delta > 0 {
            (target..n as usize)
                .find_map(|i| self.rows[i].src())
                .or_else(|| (0..target).rev().find_map(|i| self.rows[i].src()))
        } else {
            (0..=target)
                .rev()
                .find_map(|i| self.rows[i].src())
                .or_else(|| (target + 1..n as usize).find_map(|i| self.rows[i].src()))
        };
        if let Some(s) = found {
            self.cursor = s;
        }
        self.after_cursor_move();
    }

    /// Visual row at `screen_row` (0-based inside the text area), accounting
    /// for scrolling and multi-row images.
    fn row_at_screen_row(&self, screen_row: i32) -> Option<&Row> {
        // `screen_row == 0` is the normal content anchor one row below the
        // top rule. Once scrolled, `-1` is valid: content reclaims the screen
        // row where that rule used to be.
        let want = self.scroll as i32 + screen_row;
        if want < 0 {
            return None;
        }
        let want = want as u32;
        let mut y = 0u32;
        for row in &self.rows {
            let h = row.height() as u32;
            if want < y + h {
                return Some(row);
            }
            y += h;
        }
        None
    }

    /// The source line rendered at `screen_row`; `None` on a card row or
    /// past the end.
    fn src_at_screen_row(&self, screen_row: i32) -> Option<usize> {
        self.row_at_screen_row(screen_row).and_then(Row::src)
    }

    /// What a mouse cell leads to, in VIEW mode.
    ///
    /// The answer comes from what the renderer said it drew (`Hit`: a
    /// span index per followable thing), not from how the row looks. The
    /// click is turned into a column of the unwrapped rendered line, and
    /// the hit whose span covers that column wins.
    ///
    /// It used to be done by comparing the clicked span's colour with the
    /// palette and counting equal labels. That made the click path depend
    /// on the styling: giving uncreated links their own colour quietly
    /// made them the one thing on a page that could not be followed.
    fn link_at_screen_position(&self, screen_row: i32, col: usize) -> Option<(usize, LinkItem)> {
        if self.mode != Mode::View || self.session.is_some() {
            return None;
        }
        let Row::Line { line, src, start, hang } = self.row_at_screen_row(screen_row)? else {
            return None;
        };
        // A related-page row (a virtual line below the page) has exactly
        // one target and no notation to speak of.
        if *src >= self.lines.len() {
            // One target per row, and the whole row is it — there is no
            // prose here to click past.
            if line.spans.iter().all(|sp| sp.content.trim().is_empty()) {
                return None;
            }
            return self.links_at_src(*src).into_iter().next().map(|item| (*src, item));
        }
        // A table is laid out against the pane at draw time, so its
        // `table:` header is not a Text block and has no hits of its own.
        // The header is still followable (Enter saves the CSV), and its
        // label is the row's first span — so the column decides, as it
        // does everywhere else here.
        if self.text_block_at(*src).is_none() {
            let label_w = line.spans.first().map(|s| str_width(s.content.as_ref()))?;
            if col >= label_w {
                return None;
            }
            let text = mask_inline_code(&self.lines.get(*src)?.text);
            return block_export_link(&text, *src).map(|item| (*src, item));
        }
        // Screen column → column of the line as it was rendered before
        // wrapping. A continuation row carries the hanging indent in
        // front, which belongs to no span of the original.
        if col < *hang {
            return None;
        }
        let want = start + (col - hang);
        // The hit's span is an index into the UNWRAPPED line, so measure
        // there: this row only holds a slice of it.
        let (full, hits) = self.text_block_at(*src)?;
        let mut at = 0usize;
        let mut spans = Vec::with_capacity(full.spans.len());
        for sp in &full.spans {
            let w = str_width(sp.content.as_ref());
            spans.push((at, w));
            at += w;
        }
        let hit = hits.iter().find(|h| {
            spans.get(h.span).is_some_and(|(from, w)| *from <= want && want < from + w)
        })?;
        self.item_for_hit(*src, &hit.target).map(|item| (*src, item))
    }

    /// The rendered (unwrapped) line for a source line and what can be
    /// followed on it. `Hit::span` counts spans of THIS line, which is the
    /// coordinate system the renderer reported in.
    fn text_block_at(&self, src: usize) -> Option<(&Line<'static>, &[cosense::render::Hit])> {
        let i = self
            .srcs
            .iter()
            .position(|s| *s == src)
            .filter(|i| matches!(self.blocks.get(*i), Some(Block::Text(_))))?;
        let Some(Block::Text(line)) = self.blocks.get(i) else { return None };
        Some((line, self.hits.get(i).map(|v| v.as_slice()).unwrap_or(&[])))
    }

    /// Turn what the renderer drew into what this viewer does with it: a
    /// page is opened, an upload is downloaded, a `code:`/`table:` header
    /// is saved as a file.
    fn item_for_hit(&self, src: usize, target: &cosense::render::HitTarget) -> Option<LinkItem> {
        use cosense::render::HitTarget;
        match target {
            HitTarget::Page(text) => Some(page_link_item(text)),
            HitTarget::Url { label, url } => {
                Some(link_item_for_url(label.clone(), url.clone()))
            }
            HitTarget::BlockLabel => {
                block_export_link(&mask_inline_code(&self.lines.get(src)?.text), src)
            }
        }
    }

    /// Put the cursor on source line `src` (clamped / snapped to a rendered
    /// line), extending any selection and scheduling a viewport follow.
    fn goto_src(&mut self, src: usize) {
        self.cursor = src;
        self.clamp_cursor();
        self.after_cursor_move();
    }

    /// First / last cursor-addressable source line. Related rows are source
    /// lines too for j/k navigation, but `G` deliberately targets the last
    /// BODY line because the related sections live outside the page frame.
    fn first_src(&self) -> Option<usize> {
        self.rows.iter().find_map(Row::src)
    }
    fn last_src(&self) -> Option<usize> {
        self.rows.iter().rev().find_map(Row::src)
    }
    fn last_body_src(&self) -> Option<usize> {
        self.rows
            .iter()
            .filter_map(Row::src)
            .filter(|&src| src < self.lines.len())
            .last()
    }

    /// Height offset of the page frame's bottom rule (`FrameEnd`).
    fn frame_end_top(&self) -> u16 {
        self.rows
            .iter()
            .take_while(|r| !matches!(r, Row::FrameEnd))
            .map(Row::height)
            .sum()
    }

    fn after_cursor_move(&mut self) {
        if let Some(s) = self.selection.as_mut() {
            s.cursor = self.cursor;
        }
        self.follow = true;
    }

    /// Y offset (in height units) of row index `i` from the top.
    fn row_top(&self, i: usize) -> u16 {
        self.rows[..i].iter().map(Row::height).sum()
    }

    /// Total content height in height units.
    fn total_height(&self) -> u16 {
        self.rows.iter().map(Row::height).sum()
    }

    /// Largest scroll offset. The top rule is one row above `rows`; the
    /// bottom page rule is an explicit FrameEnd inside `rows`, followed by
    /// any unboxed related rows. Thus the whole extent is total + 1.
    fn max_scroll(&self, band_h: u16) -> u16 {
        self.total_height().saturating_add(1).saturating_sub(band_h)
    }

    /// Scroll that pins the PAGE frame's bottom rule to the viewport bottom.
    /// Unlike max_scroll, this intentionally ignores related rows.
    fn frame_end_scroll(&self, band_h: u16) -> u16 {
        self.frame_end_top().saturating_add(2).saturating_sub(band_h)
    }

    /// Y offset of the cursor line's first display row, if it has any.
    fn cursor_top(&self) -> Option<u16> {
        self.cursor_rows().map(|(first, _)| self.row_top(first))
    }

    /// Keep the cursor's source line within the viewport of `body_h` rows,
    /// always leaving ONE row below the cursor's last display row for the
    /// frame's bottom rule. Scrolling up aligns the FIRST display row to
    /// the top; scrolling down keeps the LAST display row + rule row on
    /// screen. When the cursor is on the very LAST source line, the bottom
    /// rule is what matters, so the viewport goes all the way to max
    /// scroll (G does the same). akapen's `keep_cursor_visible`.
    fn follow_cursor(&mut self, body_h: u16) {
        let body_h = body_h.max(2); // cursor row + at least one rule row
        let Some((first, last)) = self.cursor_rows() else { return };
        let top = self.row_top(first);
        let bottom = self.row_top(last) + self.rows[last].height();
        if top < self.scroll {
            self.scroll = top;
        } else if bottom + 1 > self.scroll + body_h {
            // Reserve the row below the cursor for the bottom rule.
            self.scroll = bottom + 1 - body_h;
        }
        self.scroll = self.scroll.min(self.max_scroll(body_h));
        // On the very last source line, keep the bottom rule visible by
        // letting the viewport reach the document's end.
        if let Some(last_src) = self.last_src() {
            if self.cursor == last_src {
                self.scroll = self.max_scroll(body_h);
            }
        }
    }

    /// Wheel scroll: move the viewport by `delta` height units and leave
    /// the cursor at its absolute line — scrolling back finds it where it
    /// was (akapen's `wheel_scroll`).
    fn wheel_scroll(&mut self, delta: i32, body_h: u16) {
        let max = self.max_scroll(body_h) as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max.max(0)) as u16;
        self.follow = false;
    }

    /// Re-lay out for `width` while keeping the cursor's source line on the
    /// SAME physical screen row (akapen's `replace_view_preserving_cursor`).
    /// Used by the Tab mode toggle and by width changes, so the text under
    /// the eye does not jump when wrapping or the mode changes the row
    /// count above the cursor.
    fn relayout_preserving_screen_row(&mut self, width: u16, body_h: u16) {
        let screen = self.cursor_top().map(|t| t as i32 - self.scroll as i32);
        self.rebuild(width);
        if let (Some(screen), Some(top)) = (screen, self.cursor_top()) {
            let max = self.max_scroll(body_h) as i32;
            self.scroll = (top as i32 - screen).clamp(0, max.max(0)) as u16;
        }
        self.follow = true;
    }

    /// The cursor's position as a fraction of the source lines (0..=1),
    /// akapen's `cursor_fraction`. Because the cursor already IS a source
    /// line, it survives re-layouts unchanged; this is only needed when
    /// the line list itself is replaced by one of a different length.
    #[cfg(test)]
    fn cursor_fraction(&self) -> f64 {
        let max = self.lines.len().saturating_sub(1);
        if max == 0 {
            0.0
        } else {
            self.cursor as f64 / max as f64
        }
    }

    /// Build a Comment from the current selection (or cursor) + body. The
    /// selection is a SOURCE-line range, so it maps straight onto lines.
    fn make_comment(&self, body: String) -> Option<Comment> {
        if self.lines.is_empty() {
            return None;
        }
        let (a, b) = self.selection.map(|s| s.range()).unwrap_or((self.cursor, self.cursor));
        let last = self.lines.len() - 1;
        // A related row (virtual line below the body) cannot anchor a
        // comment; a drag that merely overshoots into that area clamps.
        if a > last {
            return None;
        }
        let (a, b) = (a.min(last), b.min(last));
        let line_texts: Vec<String> = (a..=b).map(|i| self.lines[i].text.clone()).collect();
        let line_ids: Vec<String> = (a..=b).map(|i| self.lines[i].id.clone()).collect();
        Some(Comment {
            project: self.project.clone(),
            title: self.title.clone(),
            start: a,
            end: b,
            line_texts,
            line_ids,
            text: body,
        })
    }
}

/// Display width of a string in terminal columns.
fn str_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

/// Hard-wrap `s` into segments of at most `w` display columns, preserving
/// every char in order (no trimming). Both the session line's display and
/// its caret math use THIS function, so the hardware cursor can never
/// disagree with the text it sits on. Always returns ≥ 1 segment.
/// The nearest char boundary at or below `i` — slicing a display segment
/// at a byte that lands mid-character would panic.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}


/// The caret line, wrapped the way it is drawn: the first row uses the
/// whole width, continuation rows are pushed in by `hang` so the text
/// lines up under its own first character instead of under the bullet.
///
/// READ has always wrapped like this; EDIT wrapped flat, so a long
/// bullet's continuation jumped back to the margin the moment you started
/// editing it. Every piece of caret arithmetic — the hardware cursor, a
/// click, ↑/↓, the selection highlight — has to agree with the drawing,
/// so they all read this one structure.
struct SessionWrap {
    segs: Vec<String>,
    hang: usize,
}

impl SessionWrap {
    fn new(disp: &str, width: usize, hang: usize) -> Self {
        let width = width.max(1);
        // A continuation narrower than this is worse than no hanging at
        // all (`wrap.rs` draws the same line).
        let hang = if hang > 0 && width.saturating_sub(hang) >= 8 { hang } else { 0 };
        let mut segs: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut used = 0usize;
        for ch in disp.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            let limit = if segs.is_empty() { width } else { width - hang };
            if cw > 0 && used + cw > limit && !cur.is_empty() {
                segs.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(ch);
            used += cw;
        }
        segs.push(cur);
        Self { segs, hang }
    }

    /// The indent drawn before row `i`.
    fn indent_of(&self, row: usize) -> usize {
        if row == 0 { 0 } else { self.hang }
    }

    /// (row, column ON SCREEN) of a display byte offset.
    fn row_col(&self, disp_byte: usize) -> (usize, usize) {
        let mut off = 0usize;
        for (i, seg) in self.segs.iter().enumerate() {
            let end = off + seg.len();
            if disp_byte < end || (disp_byte == end && i + 1 == self.segs.len()) {
                let within = str_width(&seg[..floor_boundary(seg, disp_byte - off)]);
                return (i, self.indent_of(i) + within);
            }
            off = end;
        }
        let last = self.segs.len().saturating_sub(1);
        let w = str_width(self.segs.last().map(String::as_str).unwrap_or(""));
        (last, self.indent_of(last) + w)
    }

    /// Display byte offset at a row and an on-screen column.
    fn offset_at(&self, row: usize, col: usize) -> usize {
        let row = row.min(self.segs.len().saturating_sub(1));
        let before: usize = self.segs.iter().take(row).map(String::len).sum();
        let col = col.saturating_sub(self.indent_of(row));
        before + self.segs.get(row).map(|s| byte_at_col(s, col)).unwrap_or(0)
    }
}

/// How far the caret line's continuation rows hang: the column its text
/// starts at, which is the bullet's width for an outline row and the code
/// gutter inside a `code:` block.
fn session_hang(buf: &str, code: Option<CodeSpan>) -> usize {
    match code {
        Some(span) => span.gutter_cols(),
        None => {
            let n = indent_of(buf).chars().count();
            if n == 0 { 0 } else { bullet_indent_width(n) + 2 }
        }
    }
}



/// Byte offset in `s` whose display column is closest to `col` (used by/// Byte offset in `s` whose display column is closest to `col` (used by
/// sticky-column ↑/↓ and by mouse clicks).
fn byte_at_col(s: &str, col: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > col {
            return i;
        }
        used += cw;
    }
    s.len()
}

/// Leading whitespace (the Scrapbox indent) of a line.
fn indent_of(s: &str) -> &str {
    let end = s.find(|c: char| !c.is_whitespace()).unwrap_or(s.len());
    &s[..end]
}

/// Display form of the session's raw line. One source whitespace character
/// remains one logical level, while each step after level 1 occupies two
/// terminal cells: level 1 `•`, level 2 `  •`, level 3 `    •`.
/// The DATA stays unchanged and the caret conversion below maps between the
/// compact source indent and its expanded display form.
/// How many of the first `n` characters of `buf` are really leading
/// whitespace — what `strip_leading_ws` would take off.
fn leading_ws_taken(buf: &str, n: usize) -> usize {
    buf.chars()
        .take(n)
        .take_while(|c| *c == ' ' || *c == '\t' || *c == '\u{3000}')
        .count()
}

fn session_display(buf: &str, code: Option<CodeSpan>) -> String {
    // A TAB inside the line is the cell separator of a table, and
    // invisible everywhere else: at width zero the cells on either side
    // run together and the caret sits in a gap nobody can see. Show it as
    // one column — one character in, one out, so every caret offset still
    // maps. A tab in the INDENT is a level, not a cell, and is left to the
    // indent handling below.
    let mark = |text: &str| text.replace('\t', TAB_MARK);
    if let Some(span) = code {
        // Wear the renderer's code gutter, not the raw indent: the block's
        // base indent comes off and the same columns go back on, so the
        // caret line sits in the same column as the code around it.
        let strip = leading_ws_taken(buf, span.strip_chars());
        let rest: String = buf.chars().skip(strip).collect();
        return format!("{}{}", " ".repeat(span.gutter_cols()), mark(&rest));
    }
    let ind = indent_of(buf);
    let n = ind.chars().count();
    if n == 0 {
        return mark(buf);
    }
    let mut out = String::with_capacity(buf.len() + n + 4);
    out.push_str(&" ".repeat(bullet_indent_width(n)));
    out.push('\u{2022}');
    out.push(' ');
    out.push_str(&mark(&buf[ind.len()..]));
    out
}

/// Byte length of the `…• ` prefix in `session_display(buf)` (0 when the
/// line has no indent).
fn display_prefix_bytes(buf: &str) -> usize {
    let n = indent_of(buf).chars().count();
    if n == 0 { 0 } else { bullet_indent_width(n) + '•'.len_utf8() + 1 }
}

/// Byte offset in `session_display(buf, in_code)` for byte offset `caret`.

fn display_caret(buf: &str, caret: usize, code: Option<CodeSpan>) -> usize {
    // A caret that strayed mid-character — a mouse anchor computed against
    // another line's text is the classic way — is floored to the boundary:
    // the slice below would panic on it.
    let caret = floor_boundary(buf, caret);
    let ci = buf[..caret].chars().count();
    if let Some(span) = code {
        let strip = leading_ws_taken(buf, span.strip_chars());
        let di = if ci < strip { ci } else { span.gutter_cols() + ci - strip };
        let disp = session_display(buf, code);
        return disp.char_indices().nth(di).map(|(b, _)| b).unwrap_or_else(|| disp.len());
    }
    let n = indent_of(buf).chars().count();
    let di = if n == 0 {
        ci
    } else if ci < n {
        ci * 2
    } else {
        // The display prefix has 2n chars; the source prefix has n.
        ci + n
    };
    let disp = session_display(buf, None);
    disp.char_indices().nth(di).map(|(b, _)| b).unwrap_or_else(|| disp.len())
}

/// Inverse: a byte offset in `session_display(buf)` back to `buf`. A click
/// in either cell of one visual indent step maps to the nearest logical
/// source boundary; the injected space after `•` maps to the text start.
fn raw_caret_from_display(buf: &str, disp_byte: usize, code: Option<CodeSpan>) -> usize {
    let disp = session_display(buf, code);
    let di = disp[..floor_boundary(&disp, disp_byte)].chars().count();
    if let Some(span) = code {
        let strip = leading_ws_taken(buf, span.strip_chars());
        let gutter = span.gutter_cols();
        let ci = if di < gutter { di.min(strip) } else { strip + di - gutter };
        return buf.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(buf.len());
    }
    let n = indent_of(buf).chars().count();
    let ci = if n == 0 {
        di
    } else if di < n * 2 {
        (di + 1) / 2
    } else {
        di - n
    };
    buf.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(buf.len())
}

/// Truncate `s` to at most `w` columns, appending `…` when cut.
fn truncate_width(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if str_width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// One related-pages row: `title · age  description`, the title in
/// `title_style` (see `related_rows`: blue while unread, plain once seen)
/// and the rest dim, truncated to the pane width — related rows do not
/// wrap, being a scannable list rather than body text.
fn related_row(e: &RelEntry, text_w: usize, title_style: Style) -> Line<'static> {
    let dim = Style::default().fg(CHROME_DIM);
    let title = truncate_width(&e.title, text_w);
    let mut spans = vec![Span::styled(title.clone(), title_style)];
    let mut used = str_width(&title);
    if e.age > 0 {
        let meta = format!(" · {}", relative_age(e.age));
        if used + str_width(&meta) <= text_w {
            used += str_width(&meta);
            spans.push(Span::styled(meta, dim));
        }
    }
    if !e.desc.is_empty() && used + 2 < text_w {
        let desc = truncate_width(&e.desc, text_w - used - 2);
        spans.push(Span::styled(format!("  {desc}"), dim));
    }
    Line::from(spans)
}

/// Render a comment as inline card lines (indented, distinct background).
fn card_lines(c: &Comment, width: usize) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4).max(10);
    let bar = "─".repeat(inner);
    let cs = Style::default().bg(CARD_BG).fg(Color::Gray);
    let accent = Style::default().bg(CARD_BG).fg(Color::LightBlue);
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(format!("  ╭{bar}╮"), accent)));
    let head = t!("  💬 {} 行目", "  💬 lines {}", c.range_label());
    out.push(Line::from(vec![
        Span::styled("  │ ", accent),
        Span::styled(
            pad(&head, inner.saturating_sub(2)),
            cs.add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │", accent),
    ]));
    for bl in c.text.lines() {
        for chunk in wrap_plain(bl, inner.saturating_sub(2)) {
            out.push(Line::from(vec![
                Span::styled("  │ ", accent),
                Span::styled(pad(&chunk, inner.saturating_sub(2)), cs),
                Span::styled(" │", accent),
            ]));
        }
    }
    out.push(Line::from(Span::styled(format!("  ╰{bar}╯"), accent)));
    out
}

fn pad(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

fn wrap_plain(s: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn main() -> Result<(), Box<dyn Error>> {
    // Parse positional (project, title) + flags (--theme NAME, --light, --dark).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut theme: Option<String> = None;
    let mut force_light: Option<bool> = None;
    let mut ime_mode = cosense::ime::ImeMode::Jp; // Japanese-first default
    let mut preview = cosense::index::PreviewMode::Auto;
    let mut download_dir: Option<String> = None;
    let mut lang: Option<String> = None;
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--theme" => theme = it.next(),
            "--light" => force_light = Some(true),
            "--dark" => force_light = Some(false),
            "--ime" => {
                ime_mode = cosense::ime::ImeMode::parse(&it.next().unwrap_or_default());
            }
            s if s.starts_with("--ime=") => {
                ime_mode = cosense::ime::ImeMode::parse(&s["--ime=".len()..]);
            }
            s if s.starts_with("--theme=") => theme = Some(s["--theme=".len()..].to_string()),
            // The index's excerpt dock keeps ashiato's flag and threshold:
            // `auto` (default) shows it from 80 columns up.
            "--preview" => {
                preview = it
                    .next()
                    .as_deref()
                    .and_then(cosense::index::PreviewMode::parse)
                    .unwrap_or_default();
            }
            s if s.starts_with("--preview=") => {
                preview = cosense::index::PreviewMode::parse(&s["--preview=".len()..])
                    .unwrap_or_default();
            }
            "--lang" => lang = it.next(),
            s if s.starts_with("--lang=") => lang = Some(s["--lang=".len()..].to_string()),
            "--download-dir" => download_dir = it.next(),
            s if s.starts_with("--download-dir=") => {
                download_dir = Some(s["--download-dir=".len()..].to_string())
            }
            _ => positional.push(a),
        }
    }
    // Before any message is built: the whole UI asks `lang` for its words.
    cosense::lang::set(cosense::lang::Lang::detect(lang.as_deref(), &|k| {
        std::env::var(k).ok()
    }));

    // `view <project> [title]`, or `view https://scrapbox.io/<project>/<title>#<lineId>`
    // — a page URL pasted from the browser opens that project's page, with
    // the cursor on the deep-linked line.
    let (project, title, line_id) = match positional.first().and_then(|a| parse_page_url(a)) {
        Some(target) => target,
        None => (
            positional.first().cloned().unwrap_or_else(|| "help-jp".into()),
            positional.get(1).cloned(),
            None,
        ),
    };
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let gyazo_token = std::env::var("GYAZO_TEAMS_ACCESS_TOKEN")
        .ok()
        .or_else(|| std::env::var("GYAZO_ACCESS_TOKEN").ok())
        .filter(|s| !s.is_empty());

    // Auth: the official CLI's `cosense login` store (PAT / service
    // account), then the sid cookie fallback — see `AuthStore`.
    let auth = AuthStore::load(sid);
    let api_domain = "scrapbox.io".to_string();
    let user_cred = auth.resolve_user(&format!("https://{api_domain}"));
    let cfg = Config { project: project.clone(), auth, api_domain };
    let client = Client::new(cfg)?;

    // No title on the command line = "show me the project". The most
    // recently updated page is loaded underneath (Esc lands there), and
    // the index opens on top of it: the site top.
    let start_at_index = title.is_none();
    let title = match title {
        Some(t) => t,
        None => {
            let (_, pages) = client.list_pages(1, 0, "updated")?;
            pages.first().map(|p| p.title.clone()).ok_or("empty project")?
        }
    };

    let fetcher = Arc::new(ImageFetcher::new(gyazo_token, user_cred)?);

    let mut terminal = ratatui::init();
    // Wheel scrolling moves the viewport (akapen parity); failure to enable
    // mouse reporting only loses that, so it is not fatal. Bracketed paste
    // lets the composer take multi-line pastes as ONE event.
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    // Compile the macOS IME helper in the background so the first composer
    // open never blocks on swiftc.
    cosense::ime::start_background_build();
    // Detect the terminal's actual background after entering the alt screen.
    // A forced mode uses a representative base so translucent Cosense navbar
    // colors still compose predictably without a second OSC query.
    let detected_bg = if force_light.is_none() {
        cosense::theme::detect_background()
    } else {
        None
    };
    let light = force_light.unwrap_or_else(|| {
        detected_bg
            .map(cosense::theme::background_is_light)
            .unwrap_or(false)
    });
    let terminal_bg = detected_bg.unwrap_or(if light { (250, 250, 250) } else { (24, 24, 24) });
    let hl = Highlighter::new(theme.as_deref(), light);
    // One color scheme for the whole page: headings, links, quotes and
    // code labels take the theme's markdown colors (akapen parity).
    let palette = cosense::theme::Palette::from_theme(&hl, light);
    let picker = image_picker()?;

    // Where saved files land. A directory the user NAMED is created if it
    // is missing; if that cannot be done, say so once at startup and fall
    // back to the search, rather than failing at each download with a
    // message about a path nobody remembers choosing.
    let home = std::env::var("HOME").ok();
    let env_download = std::env::var("COSENSE_DOWNLOAD_DIR").ok();
    let xdg_download = std::env::var("XDG_DOWNLOAD_DIR").ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (mut download_dir, named) = pick_download_dir(
        download_dir.as_deref(),
        env_download.as_deref(),
        xdg_download.as_deref(),
        home.as_deref(),
        cwd.clone(),
    );
    let mut download_note = None;
    if named {
        if let Err(e) = std::fs::create_dir_all(&download_dir) {
            let (fallback, _) =
                pick_download_dir(None, None, xdg_download.as_deref(), home.as_deref(), cwd);
            download_note = Some(t!(
                "{} は使えません（{e}）。{} に保存します",
                "{} is unusable ({e}); saving to {} instead",
                download_dir.display(),
                fallback.display()
            ));
            download_dir = fallback;
        }
    }

    let ctx = Ctx {
        client,
        hl,
        palette,
        picker,
        fetcher,
        light,
        terminal_bg,
        ime_mode,
        preview,
        download_dir,
        editability: std::sync::Mutex::new(HashMap::new()),
        project_themes: std::sync::Mutex::new(HashMap::new()),
    };
    let loaded = load_page(&ctx, &project, &title)?;

    let mut app = App::new(project.clone());
    app.light = ctx.light;
    app.session_ime = cosense::ime::SessionIme::new(ime_mode);
    // The serial commit worker: owns its own Client clone and answers on
    // the outcome channel drained by the event loop.
    if let Some(jobs_rx) = app.commit_jobs_rx.take() {
        spawn_commit_worker(
            ctx.client.clone(),
            jobs_rx,
            app.commit_res_tx.clone(),
            Arc::clone(&app.gen),
        );
    }
    // The web renderer: headless Chrome when one can be found, otherwise a
    // backend that fails every request so diagrams simply stay code blocks.
    // The session's `connect.sid` (never the PAT) is what a browser can use,
    // and it is handed to the backend here and nowhere else.
    // `COSENSE_WEB_RENDER=off` means OFF: no backend, no worker thread, and
    // no cache directory is created or swept. Diagrams are code blocks, and
    // nothing on disk is touched.
    let renderer_off = app.render_policy == capability::RenderPolicy::Off;
    let web_backend: Arc<dyn WebBackend> = if renderer_off {
        Arc::new(cosense::webrender::UnavailableBackend(WebError::NoBrowser))
    } else {
        match cosense::chrome::ChromeBackend::detect(ctx.client.sid().map(str::to_string)) {
            Some(b) => Arc::new(b),
            None => Arc::new(cosense::webrender::UnavailableBackend(WebError::NoBrowser)),
        }
    };
    let web_worker = app.web_jobs_rx.take().filter(|_| !renderer_off).map(|jobs_rx| {
        spawn_web_worker(
            jobs_rx,
            app.web_tx.clone(),
            Arc::clone(&web_backend),
            ctx.picker.clone(),
            ArtifactCache::new(),
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        )
    });
    // Live web edits: websocket push when the session has a `connect.sid`
    // (regardless of the project credential — REST may well resolve to a
    // PAT while the push channel only accepts the sid), polling otherwise.
    // A slow INSURANCE poll keeps running even in ws mode: a stale or
    // invalid sid (push stuck reconnecting forever) must degrade to
    // "web edits still land, just at the poll interval" — never silence.
    let sid = ctx.client.sid().map(str::to_string);
    // A sid is one capability among several: it enables push sync and lets
    // the browser see private pages. It is NOT what makes the app usable —
    // a PAT session without one still reads, edits and commits.
    app.caps.sid = sid.is_some();
    let (ws_active, initial_state) = cosense::ws::initial_plan(sid.is_some());
    app.ws_attempted = ws_active;
    app.sync_state = initial_state;
    let poll_ctrl_rx = app
        .poll_ctrl_rx
        .take()
        .expect("poller control receiver is only handed out once");
    spawn_web_poller(
        ctx.client.clone(),
        Arc::clone(&app.poll_target),
        app.poll_tx.clone(),
        poll_ctrl_rx,
        Arc::clone(&app.server_epoch),
        initial_state.poll_interval(),
    );
    // Answers "was this link ever written?" for the links a page arrives
    // without an answer for — the ones typed since it was loaded.
    let (probe_tx, probe_rx) = mpsc::channel();
    app.link_probe_tx = Some(probe_tx);
    spawn_link_prober(ctx.client.clone(), probe_rx, app.link_probe_res_tx.clone());
    if let Some(sid) = &sid {
        let ws_req_rx = app
            .ws_req_rx
            .take()
            .expect("ws request receiver is only handed out once");
        cosense::ws::spawn_ws_sync(
            ctx.client.clone(),
            sid.clone(),
            Arc::clone(&app.poll_target),
            ws_req_rx,
            app.ws_tx.clone(),
            Arc::clone(&app.server_epoch),
        );
    }
    app.set_page(loaded, &ctx);
    if start_at_index {
        let project = app.project.clone();
        open_index(&mut app, &ctx, &project, String::new());
    }
    // Say how we are authenticated (or that we are not): edits and private
    // reads depend on it, and `cosense login` is the fix when missing.
    // `sync:` shows which live-update path is active (ws = websocket push,
    // poll = 3 s polling).
    // `image:` says which picture protocol answered the terminal query.
    // Only `kitty` (unicode placeholders) and `halfblocks` are anchored to
    // cells, so this is the first thing to look at when pictures float
    // over a neighbouring pane.
    let img = image_protocol_name(&ctx.picker);
    app.status = match ctx.client.credential_for(&project) {
        Some(c) if app.editable => t!("認証: {} · 編集可 · 同期: {} · 画像: {img} · ? ヘルプ", "auth: {} · edit enabled · sync: {} · image: {img} · ? help",
            c.kind(),
            app.sync_label()
        ),
        Some(c) => t!("認証: {} · 読み取り専用（プロジェクトのメンバーではありません）· 同期: {} · 画像: {img} · ? ヘルプ", "auth: {} · read-only (not a project member) · sync: {} · image: {img} · ? help",
            c.kind(),
            app.sync_label()
        ),
        None => t!("認証なし — 公開ページの読み取りのみ（編集するには `cosense login`）· 画像: {img}", "no auth — public read-only (`cosense login` to enable edits) · image: {img}"
        ),
    };
    if let Some(note) = download_note {
        app.status = note;
    }
    // `#<lineId>` from the URL: start on that line (the first frame's layout
    // clamps it onto a rendered line and scrolls it into view).
    if let Some(id) = line_id {
        match app.lines.iter().position(|l| l.id == id) {
            Some(i) => {
                app.cursor = i;
                app.follow = true;
            }
            None => app.status = t!("このページに行 {id} はありません", "line {id} not found on this page"),
        }
    }

    let res = run(&mut terminal, &mut app, &ctx);
    // Shutdown contract, in this order:
    //   1. `shutdown()` refuses further work and SIGKILLs any browser in
    //      flight. That is also what unblocks the worker: its DevTools
    //      socket dies, so a batch mid-navigation errors out promptly
    //      instead of waiting out its budget.
    //   2. give the terminal back, so a slow reap is never a black screen.
    //   3. `Stop` ends the worker loop, and the join makes "no browser and
    //      no worker outlive this process" a fact rather than a hope.
    web_backend.shutdown();
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    if let Some(worker) = web_worker {
        let _ = app.web_job_tx.send(WebJob::Stop);
        let _ = worker.join();
    }
    drop(app.ime_guard.take());

    if !app.comments.is_empty() {
        println!("{}", format_all(&app.comments));
    }
    res
}

/// Shared, page-independent resources used to load pages on navigation.
struct Ctx {
    client: Client,
    hl: Highlighter,
    palette: cosense::theme::Palette,
    picker: Picker,
    /// Shared with background download threads.
    fetcher: Arc<ImageFetcher>,
    /// Terminal background is light (drives telomere/palette shades).
    light: bool,
    /// Actual OSC 11 background when available, otherwise a light/dark
    /// estimate. Cosense's translucent navbar colors are composed over it.
    terminal_bg: (u8, u8, u8),
    /// Input-source policy around the composer (`--ime`, default jp).
    ime_mode: cosense::ime::ImeMode,
    /// Whether the index's excerpt dock is drawn (`--preview`).
    preview: cosense::index::PreviewMode,
    /// Where saved files land (`--download-dir`, see `pick_download_dir`).
    download_dir: std::path::PathBuf,
    /// Per-project edit permission. Membership changes are rare; navigation
    /// should not refetch `/users/me` + the member table on every page.
    editability: std::sync::Mutex<HashMap<String, bool>>,
    /// Selected Cosense site-theme id per project. `None` is cached too: a
    /// PAT-only private project falls back without retrying on every page.
    project_themes: std::sync::Mutex<HashMap<String, Option<String>>>,
}

impl Ctx {
    fn project_theme(&self, project: &str) -> Option<String> {
        if let Ok(cache) = self.project_themes.lock() {
            if let Some(theme) = cache.get(project) {
                return theme.clone();
            }
        }
        let theme = self.client.get_project_theme(project).ok();
        if let Ok(mut cache) = self.project_themes.lock() {
            cache.insert(project.to_string(), theme.clone());
        }
        theme
    }

    fn can_edit_in(&self, project: &str) -> bool {
        if let Ok(cache) = self.editability.lock() {
            if let Some(&editable) = cache.get(project) {
                return editable;
            }
        }
        let probed: Result<bool, Box<dyn Error>> = match self.client.credential_for(project) {
            None => Ok(false),
            // A project-scoped service account exists specifically to act in
            // that project; unlike a user it has no `/users/me` membership.
            Some(cosense::api::Credential::ServiceAccount(_)) => Ok(true),
            Some(_) => self.client.get_me().and_then(|me| {
                self.client
                    .list_members_in(project)
                    .map(|members| members.iter().any(|m| m.id == me))
            }),
        };
        let Ok(editable) = probed else { return false };
        if let Ok(mut cache) = self.editability.lock() {
            cache.insert(project.to_string(), editable);
        }
        editable
    }
}

/// Everything produced by loading one page.
struct Loaded {
    project: String,
    title: String,
    /// Site-theme-derived header colors for this project.
    header_colors: HeaderColors,
    /// Immutable page id (edit API / commit log).
    page_id: String,
    lines: Vec<PageLine>,
    blocks: Vec<Block>,
    srcs: Vec<usize>,
    /// See `App::hits`.
    hits: Vec<Vec<cosense::render::Hit>>,
    /// See `App::read_at`.
    read_at: Option<i64>,
    /// Whether this credential may edit the loaded project.
    editable: bool,
    /// Related-pages sections (see `build_related`).
    related: Vec<RelSection>,
    /// What this page's response said about the pages it links to.
    links: LinkTruth,
}

/// Build the related-pages sections the way scrapbox.io presents them:
///
///   Links            — 1-hop: pages this page links to + pages linking here
///                      (existing pages only; the API precomputes this).
///   <hub> (×N)       — 2-hop: pages sharing the link <hub> with this page,
///                      one group per link of this page, in page order. An
///                      entry appears only under its first hub. A hub may be
///                      a page that does not exist — 2-hop links work through
///                      empty pages, which is exactly what makes a purely
///                      auto-linked wiki hang together.
///   External links   — outgoing `[/project/title]` links (the API exposes
///                      no incoming cross-project list).
fn related_is_unread(
    visits: &HashMap<String, i64>,
    project: &str,
    title: &str,
    updated: i64,
) -> bool {
    visits
        .get(&format!("{project}/{title}"))
        .map_or(true, |&seen_at| updated > 0 && updated > seen_at)
}

fn build_related(page: &cosense::api::Page, project: &str) -> Vec<RelSection> {
    let mut secs: Vec<RelSection> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let visits = load_visits();
    let unread = |p: &str, title: &str, updated: i64| {
        related_is_unread(&visits, p, title, updated)
    };
    let key_of = |p: &cosense::api::RelatedPage| {
        if p.title_lc.is_empty() { p.title.to_lowercase() } else { p.title_lc.clone() }
    };
    let entry_of = |p: &cosense::api::RelatedPage| RelEntry {
        item: LinkItem::Page(p.title.clone()),
        title: p.title.clone(),
        desc: p.descriptions.first().cloned().unwrap_or_default(),
        age: p.updated,
        unread: unread(project, &p.title, p.updated),
    };
    if let Some(rel) = &page.related {
        if !rel.links1hop.is_empty() {
            for p in &rel.links1hop {
                seen.insert(key_of(p));
            }
            secs.push(RelSection {
                heading: format!("Links ({})", rel.links1hop.len()),
                entries: rel.links1hop.iter().map(&entry_of).collect(),
            });
        }
        // 2-hop groups, one per link of this page (page order). `links_lc`
        // of an entry lists which links it shares.
        for hub in &page.links {
            let hub_lc = hub.to_lowercase();
            let group: Vec<&cosense::api::RelatedPage> = rel
                .links2hop
                .iter()
                .filter(|p| !seen.contains(&key_of(p)) && p.links_lc.iter().any(|l| *l == hub_lc))
                .collect();
            if group.is_empty() {
                continue;
            }
            for p in &group {
                seen.insert(key_of(p));
            }
            secs.push(RelSection {
                heading: format!("{hub} ({})", group.len()),
                entries: group.into_iter().map(&entry_of).collect(),
            });
        }
        // 2-hop entries whose hubs did not match any current link (rename
        // races and the like) still deserve a place.
        let rest: Vec<&cosense::api::RelatedPage> =
            rel.links2hop.iter().filter(|p| !seen.contains(&key_of(p))).collect();
        if !rest.is_empty() {
            secs.push(RelSection {
                heading: format!("2 hop links ({})", rest.len()),
                entries: rest.into_iter().map(&entry_of).collect(),
            });
        }
    }
    if !page.project_links.is_empty() {
        let entries: Vec<RelEntry> = page
            .project_links
            .iter()
            .filter_map(|pl| {
                let rest = pl.strip_prefix('/')?;
                let (project, title) = rest.split_once('/')?;
                if project.is_empty() || title.is_empty() {
                    return None;
                }
                Some(RelEntry {
                    item: LinkItem::ProjectPage {
                        project: project.to_string(),
                        title: title.to_string(),
                    },
                    title: pl.clone(),
                    desc: String::new(),
                    age: 0,
                    unread: unread(project, title, 0),
                })
            })
            .collect();
        if !entries.is_empty() {
            secs.push(RelSection {
                heading: format!("External links ({})", entries.len()),
                entries,
            });
        }
    }
    secs
}

/// A line is unread if it was edited after `read_at`, or the page was never
/// seen at all (`None`).
fn unread_since(updated: i64, read_at: Option<i64>) -> bool {
    read_at.map_or(true, |t| updated > t)
}

/// Local record of when this viewer last opened each page, keyed by
/// `project/title` (epoch seconds). Cosense only learns about browser
/// visits, so without this a page read here would stay "unread" forever.
/// Lives in `$XDG_STATE_HOME/cosense-tui/visits.json` (default
/// `~/.local/state`).
fn visits_path() -> Option<std::path::PathBuf> {
    let base = match std::env::var("XDG_STATE_HOME") {
        Ok(x) if !x.is_empty() => std::path::PathBuf::from(x),
        _ => std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".local").join("state"),
    };
    Some(base.join("cosense-tui").join("visits.json"))
}

fn load_visits() -> HashMap<String, i64> {
    visits_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record a visit to `project/title` at `now`, returning the PREVIOUS
/// local visit time (if any). Failures to persist are ignored: the worst
/// case is a page that stays "unread" on the next visit.
fn record_visit(project: &str, title: &str, now: i64) -> Option<i64> {
    let mut visits = load_visits();
    let prev = visits.insert(format!("{project}/{title}"), now);
    if let Some(p) = visits_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string(&visits) {
            let _ = std::fs::write(p, s);
        }
    }
    prev
}

/// Leave the index for one of its rows. The index goes on the back stack
/// (that is what makes `[` come back to it), and a row that is not a page
/// yet lands in EDIT on a fresh line — nothing is sent until something is
/// written (see `dispatch_create`).
fn open_from_index(app: &mut App, ctx: &Ctx, target: Option<(String, bool)>) {
    let from = app.here();
    let project = app.index_project.clone();
    // Held, not dropped: a page that fails to load is not a reason to lose
    // the list you were choosing from.
    let saved = app.index.take();
    let Some((title, create)) = target else { return };
    if !navigate_from(app, ctx, &project, &title, from) {
        app.index = saved;
        return;
    }
    if create {
        app.cursor = 0;
        open_line(app, ctx, false);
    }
}

/// `[` / `]`: step back or forward through the places visited.
///
/// A place is a page OR a project's index, so walking back out of a page
/// you reached from the index lands in the index — where you were — rather
/// than in whatever page happened to precede it.
fn go_history(app: &mut App, ctx: &Ctx, back: bool) {
    let place = if back { app.history.pop() } else { app.forward.pop() };
    let Some(place) = place else {
        app.status = if back { t!("戻る先の履歴はありません", "no history") } else { t!("進む先の履歴はありません", "no forward history") };
        return;
    };
    let here = app.here();
    let label = place.label();
    let arrow = if back { "←" } else { "→" };
    let mut arrived = false;
    match place {
        Place::Page { project, title } => {
            // Every index keeps the page it was opened over loaded. If that
            // page is the destination, closing the index is both exact and
            // instant: cursor, scroll and images do not have to be rebuilt.
            if app.index.is_some() && app.project == project && app.title == title {
                app.index = None;
                app.status = format!("{arrow} {title}");
                arrived = true;
            } else {
                match load_page(ctx, &project, &title) {
                    Ok(loaded) => {
                        app.index = None;
                        app.set_page(loaded, ctx);
                        app.status = format!("{arrow} {title}");
                        arrived = true;
                    }
                    Err(e) => {
                        // A failed back must not eat the destination. The
                        // same key can retry after the connection recovers.
                        let place = Place::Page { project, title };
                        if back {
                            app.history.push(place);
                        } else {
                            app.forward.push(place);
                        }
                        app.status = t!("履歴の移動に失敗しました: {e}", "history failed: {e}");
                    }
                }
            }
        }
        Place::Index { project, state } => {
            app.index = Some(*state);
            app.index_project = project;
            app.overlay = None;
            app.status = format!("{arrow} {label}");
            arrived = true;
        }
    }
    if arrived {
        if back {
            app.forward.push(here);
        } else {
            app.history.push(here);
        }
    }
}

/// Open the project index: every page, newest first, with a short excerpt
/// from the one under the cursor docked below the list.
///
/// The list is one request (`/api/pages/<project>?limit=500&sort=updated`)
/// and the excerpt costs nothing on top of it: the same response carries
/// each page's first lines, which is what the reader is choosing between.
fn open_index(app: &mut App, ctx: &Ctx, project: &str, filter: String) {
    use cosense::index::{Entry, Index};
    let (count, pages) = match ctx.client.list_pages_in(project, INDEX_PAGE_LIMIT, 0, "updated") {
        Ok(v) => v,
        Err(e) => {
            app.status = t!("ページ一覧を取得できません: {e}", "page list failed: {e}");
            return;
        }
    };
    let visits = load_visits();
    let mut entries: Vec<Entry> = pages
        .into_iter()
        .map(|p| {
            let seen = visits.get(&format!("{project}/{}", p.title)).copied();
            Entry::from_summary(p, seen)
        })
        .collect();
    // The API floats pinned pages to the top even under sort=updated, so a
    // pinned two-year-old page would head a "recently updated" list.
    entries.sort_by(|a, b| b.updated.cmp(&a.updated));
    let mut ix = Index::new(entries, count.max(0) as usize);
    if !filter.is_empty() {
        ix.set_filter(filter);
    }
    app.index = Some(ix);
    app.index_project = project.to_string();
    app.overlay = None;
}

/// Fetch and render one page. Images are NOT downloaded here: they are
/// fetched on background threads (see `App::start_image_loads`) so a page
/// with many images still appears immediately.
fn load_page(ctx: &Ctx, project: &str, title: &str) -> Result<Loaded, Box<dyn Error>> {
    let page = ctx.client.get_page_in(project, title)?;
    let mut lines: Vec<PageLine> = page.lines.clone();
    if !page.persistent {
        // An uncreated page comes back as a template: a title line with no
        // id. Give every line an id here, so editing works exactly as on a
        // real page and the create request can name its own lines.
        for l in lines.iter_mut() {
            if l.id.is_empty() {
                l.id = new_line_id();
            }
        }
    }
    let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
    let links = link_truth(&page);
    let rendered = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &links);
    // Last seen = later of the browser's and this viewer's previous visit;
    // then stamp this visit so the next open treats today's lines as read.
    let local_prev = record_visit(project, title, now_secs());
    let read_at = match (page.last_accessed, local_prev) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let related = build_related(&page, project);
    let editable = ctx.can_edit_in(project);
    let site_theme = ctx.project_theme(project);
    let (header_fg, header_bg) =
        cosense::theme::project_header_colors(site_theme.as_deref(), ctx.terminal_bg);
    Ok(Loaded {
        project: project.to_string(),
        title: title.to_string(),
        header_colors: HeaderColors { fg: header_fg, bg: header_bg },
        page_id: live_page_id(&page),
        lines,
        blocks: rendered.blocks,
        srcs: rendered.srcs,
        hits: rendered.hits,
        read_at,
        editable,
        related,
        links,
    })
}

/// What this page's own response says about the pages it links to — by
/// Cosense's rule, which is not "does the page exist".
///
/// Read out of its client (`compileRelatedPages` in the related-page
/// store, and `isPageExists` where a link is drawn), a link is drawn as
/// live when ANY of these hold:
///
/// * it is a 1-hop or 2-hop neighbour — a page that exists;
/// * it is the page being read;
/// * **another page links to the same title**, written or not.
///
/// That last rule is not an accident: the index the web client builds
/// walks every page's links and marks each target live *unless the only
/// page writing it is the one on screen*
/// (`P !== p.Page.id && n.set(te, !0)`). So the red does not mean "no
/// page here"; it means "nobody but this page has ever said this word".
/// A tag two pages share is already a hub, and Cosense stops calling it
/// empty the moment the second page uses it.
///
/// The same answer is computable from the response already in hand: a
/// page that links to one of OUR targets shares that target with us, so
/// it is in our own 1-hop or 2-hop list with the target in its `linksLc`.
/// Checked against scrapbox.io: on villagepump's `井戸端` the naive "not
/// in links1hop" reading calls four titles empty, Cosense calls three,
/// and the one it spares is `実況` — unwritten, but linked from other
/// pages. This reproduces that exactly.
///
/// It costs no extra request, and it answers every link the page was
/// SAVED with. Links written since — the ones a reader is most likely to
/// be looking at — are not in it at all, and `probe_unknown_links` goes
/// and asks about those one at a time.
fn link_truth(page: &cosense::api::Page) -> LinkTruth {
    // No related-pages block, no reading: every link keeps its ordinary
    // colour rather than all of them turning red at once.
    let Some(r) = page.related.as_ref() else { return LinkTruth::default() };
    let neighbours = || r.links1hop.iter().chain(r.links2hop.iter());
    let mut existing: Vec<&str> = neighbours().map(|p| p.title.as_str()).collect();
    // Titles this page links to that a neighbour ALSO links to: shared
    // words, live whether or not anyone wrote the page.
    let ours: HashSet<String> =
        page.links.iter().map(|l| cosense::render::title_lc(l)).collect();
    existing.extend(
        neighbours()
            .flat_map(|p| p.links_lc.iter())
            .filter(|c| ours.contains(c.as_str()))
            .map(|c| c.as_str()),
    );
    let mut truth = LinkTruth::seed(page.links.iter().map(|s| s.as_str()), existing);
    // And this page itself, which is not in its own 1-hop list. Reading a
    // page is the most direct answer there is about it — including the
    // uncreated one opened through a link, which is exactly the title the
    // page we came from is drawing.
    truth.learn(&page.title, page.persistent);
    truth
}

/// The lookup worker: one title in, one answer out.
///
/// The question is Cosense's, not "does the page exist" (see
/// `link_truth`): a title is live when somebody wrote it, OR when some
/// page other than the one asking links to it. The cheap half is asked
/// first — a HEAD that says "written" ends it — so only a title nobody
/// wrote costs the second request, which is also the small one (an
/// unwritten page's response is its back links, not a body).
///
/// Serial on purpose. Unknown links are rare (a page arrives with all of
/// its links already answered), so this is a trickle — and a trickle down
/// one thread cannot turn a page full of new links into a burst of
/// requests.
fn spawn_link_prober(
    client: Client,
    rx: mpsc::Receiver<LinkProbe>,
    tx: mpsc::Sender<(LinkProbe, bool)>,
) {
    std::thread::spawn(move || {
        while let Ok(probe) = rx.recv() {
            let LinkProbe { project, title, asked_by } = &probe;
            // A failed lookup answers nothing, and is not retried (see
            // `App::link_pending`): the title stays unknown and keeps its
            // ordinary colour, which is the safe direction.
            let Ok(written) = client.page_exists(project, title) else { continue };
            let live = if written {
                true
            } else {
                match client.backlink_ids(project, title) {
                    // The asking page's own link does not make a word
                    // shared — that is the whole point of the rule.
                    Ok(ids) => ids.iter().any(|id| id != asked_by),
                    Err(_) => continue,
                }
            };
            if tx.send((probe, live)).is_err() {
                return; // app gone
            }
        }
    });
}

/// One question for the lookup worker: is this title live? Asked from a
/// page whose own links must not count towards the answer.
#[derive(Debug, PartialEq, Eq)]
struct LinkProbe {
    project: String,
    title: String,
    asked_by: String,
}

/// How pictures get onto the screen.
///
/// Quality first: whatever the terminal answers to the capability query.
/// Half-blocks are a fallback the reader can ask for, not a default —
/// they cost most of the picture (two pixels per cell) and that is a worse
/// trade than the problem they solve.
///
/// The problem they solve: a pixel protocol is painted by the terminal
/// EMULATOR, which knows nothing about a multiplexer's panes, so an image
/// can float over whatever is drawn next to it. kitty's graphics go
/// through UNICODE PLACEHOLDERS here (ratatui-image draws them that way),
/// which are anchored to text cells and therefore clip and scroll like
/// text; sixel and iTerm2 placements do not. So the fallback is worth
/// having, and worth being explicit about:
///
/// `COSENSE_IMAGE=halfblocks` — draw pictures as text cells
/// `COSENSE_IMAGE=auto` (default) — the terminal's own protocol
fn image_picker() -> Result<Picker, Box<dyn Error>> {
    use ratatui_image::picker::ProtocolType;
    let mut picker = Picker::from_query_stdio()?;
    let want = std::env::var("COSENSE_IMAGE").unwrap_or_default();
    let forced = match want.as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None, // "auto" or unset: whatever the terminal answered
    };
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    Ok(picker)
}

/// The name of the picture protocol in use, for the status line. Which one
/// is live decides whether pictures clip with the panes around them, so it
/// is worth being able to see without a debugger.
fn image_protocol_name(picker: &Picker) -> &'static str {
    use ratatui_image::picker::ProtocolType;
    match picker.protocol_type() {
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Halfblocks => "halfblocks",
    }
}

/// Turn a decoded image into a sliced protocol at a width-capped cell size./// Turn a decoded image into a sliced protocol at a width-capped cell size.
/// Runs on a worker thread (`Picker` is a plain clone of the terminal's
/// capabilities), never on the UI thread.
fn build_image(
    picker: &Picker,
    dyn_img: image::DynamicImage,
    max_cols: u16,
) -> Result<ImageInfo, String> {
    let font = picker.font_size();
    let (px_w, px_h) = (dyn_img.width(), dyn_img.height());
    let nat_cols = (px_w as f32 / font.width as f32).ceil() as u32;
    // Rows are FLOORED, not rounded up: a picture whose last row is only
    // half-covered ends mid-cell, and text placed on that row — its
    // baseline — then reads as sitting below the picture instead of level
    // with it. Losing a few pixels off the bottom is invisible; the
    // misalignment is not.
    let nat_rows = (px_h as f32 / font.height as f32).floor().max(1.0) as u32;
    let cap = max_cols.max(1) as u32;
    let (cw, ch) = if nat_cols > cap {
        let scale = cap as f32 / nat_cols as f32;
        (cap, ((nat_rows as f32) * scale).floor().max(1.0) as u32)
    } else {
        (nat_cols.max(1), nat_rows.max(1))
    };
    // A picture taller than this owns the screen: the reader scrolls
    // through one image instead of reading a page. Cosense pages are full
    // of tall screenshots, and in a browser they simply take the width
    // they are given — a terminal has to cap the HEIGHT instead, because
    // rows are the scarce direction.
    let (cw, ch) = if ch > MAX_IMAGE_ROWS as u32 {
        let scale = MAX_IMAGE_ROWS as f32 / ch as f32;
        (((cw as f32) * scale).floor().max(1.0) as u32, MAX_IMAGE_ROWS as u32)
    } else {
        (cw, ch)
    };
    let size = Size::new(cw as u16, ch as u16);
    SlicedProtocol::new(picker, dyn_img, Some(size))
        .map(|sliced| {
            let s = sliced.size();
            ImageInfo {
                sliced,
                cells_h: s.height.max(1),
                // The protocol may round down; never report more than asked
                // for, since the cap is what keeps the diagram inside the
                // pane.
                cells_w: s.width.min(max_cols.max(1)),
                built_for: max_cols.max(1),
            }
        })
        .map_err(|e| e.to_string())
}

/// Page links on a raw Scrapbox line, left to right: `[PageName]` (not a
/// URL, decoration, or icon) and `#hashtag` in the current project, and
/// `[/project/PageName]` into another project. A bare `[/project]` (the
/// project's top page) is skipped: it is not a page.
/// The line with every inline-code span blanked out, byte offsets intact.
///
/// Backticks quote notation: a line that WRITES about `[a link]` is not a
/// line that HAS one. The renderer already knew this; link extraction did
/// not, so a page documenting the notation was covered in links to pages
/// nobody meant to name.
fn mask_inline_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for ch in text.chars() {
        if ch == '`' {
            in_code = !in_code;
            out.push(' ');
            continue;
        }
        if in_code {
            for _ in 0..ch.len_utf8() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn positioned_links_on_line(text: &str) -> Vec<(usize, LinkItem)> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel_start) = text[from..].find('[') {
        let start = from + rel_start;
        if let Some(rel_end) = text[start + 1..].find(']') {
            let end = start + 1 + rel_end;
            let inner = &text[start + 1..end];
            let is_deco = inner
                .find(' ')
                .map(|sp| inner[..sp].chars().all(|c| matches!(c, '*' | '/' | '_' | '-')))
                .unwrap_or(false)
                && inner.starts_with(|c| matches!(c, '*' | '/' | '_' | '-'));
            let is_url = inner.contains("http://") || inner.contains("https://");
            let is_icon = inner.contains(".icon");
            if !is_deco && !is_url && !is_icon && !inner.is_empty() {
                // One reading of what a bracket points at, shared with the
                // renderer's hits (`page_link_item`): a page here, a page
                // over there, or a whole project.
                out.push((start, page_link_item(inner)));
            }
            from = end + 1;
        } else {
            break;
        }
    }
    // hashtags
    let mut from = 0usize;
    while let Some(rel_pos) = text[from..].find('#') {
        let pos = from + rel_pos;
        let ok = pos == 0
            || text[..pos].chars().next_back().map(|c| c.is_whitespace()).unwrap_or(false);
        let tag: String = text[pos + 1..].chars().take_while(|c| !c.is_whitespace()).collect();
        if ok && !tag.is_empty() {
            out.push((pos, LinkItem::Page(tag)));
        }
        from = pos + 1;
    }
    out
}

/// What `[...]` (or a `#tag`) leads to: a page here, or a page in another
/// project when it is written `/project/title`.
fn page_link_item(text: &str) -> LinkItem {
    let Some(rest) = text.strip_prefix('/') else {
        return LinkItem::Page(text.to_string());
    };
    let (project, title) = match rest.split_once('/') {
        Some((p, t)) => (p, t.trim()),
        None => (rest, ""),
    };
    if project.is_empty() {
        return LinkItem::Page(text.to_string());
    }
    if title.is_empty() {
        // `[/project]` (or `[/project/]`): the project itself, whose home
        // screen here is its index.
        return LinkItem::ProjectIndex { project: project.to_string() };
    }
    LinkItem::ProjectPage { project: project.to_string(), title: title.to_string() }
}

fn links_on_line(text: &str) -> Vec<LinkItem> {
    positioned_links_on_line(text).into_iter().map(|(_, item)| item).collect()
}

/// Every http(s) URL on a raw Scrapbox line, as `(label, url)`, left to
/// right. The bracket form `[title https://…]` (either order) labels the
/// URL with `title`; a bare URL is labelled by its file name for uploads
/// and by the URL itself otherwise.
fn positioned_labelled_urls(text: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("http") {
        let start = from + rel;
        let url_end = text[start..]
            .find(|c: char| c.is_whitespace() || c == ']')
            .map(|e| start + e)
            .unwrap_or(text.len());
        let url = &text[start..url_end];
        from = url_end.max(start + 1);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        // The bracket around this URL, if any: `[label url]`.
        let bracket = text[..start]
            .rfind('[')
            .filter(|&lb| !text[lb..start].contains(']'))
            .and_then(|lb| text[url_end..].find(']').map(|rb| (lb, url_end + rb)));
        // A LINKED IMAGE (`[href imageUrl]`) is one link, not two: the
        // picture is the label and the href is where it goes. Offering
        // both, with each labelled by the other, was the reason a linked
        // image asked which link you meant — and then opened the other one.
        if let Some((lb, rb)) = bracket {
            let inner = &text[lb + 1..rb];
            if cosense::render::looks_like_image_url(url) && has_other_url(inner, url) {
                continue; // the href carries this bracket
            }
        }
        let label = bracket
            .map(|(lb, rb)| text[lb + 1..rb].replace(url, "").trim().to_string())
            .filter(|l| !l.is_empty())
            .map(|l| {
                // The other side of a linked image is the picture: say so
                // rather than printing its URL.
                if gyazo_permalink(&l).is_some() {
                    "\u{1f5bc} gyazo".to_string()
                } else if cosense::render::looks_like_image_url(&l) {
                    format!("\u{1f5bc} {}", file_name_of_url(&l))
                } else {
                    l
                }
            })
            .unwrap_or_else(|| {
                if is_scrapbox_file_url(url) {
                    file_name_of_url(url).to_string()
                } else if gyazo_permalink(url).is_some() {
                    // `link_item_for_url` names these "gyazo" — leave it to it.
                    url.to_string()
                } else if cosense::render::looks_like_image_url(url) {
                    format!("\u{1f5bc} {}", file_name_of_url(url))
                } else {
                    url.to_string()
                }
            });
        out.push((start, label, url.to_string()));
    }
    out
}

/// Does `inner` hold a URL other than `url`?
fn has_other_url(inner: &str, url: &str) -> bool {
    inner
        .split_whitespace()
        .any(|t| t != url && (t.starts_with("http://") || t.starts_with("https://")))
}

fn labelled_urls(text: &str) -> Vec<(String, String)> {
    positioned_labelled_urls(text)
        .into_iter()
        .map(|(_, label, url)| (label, url))
        .collect()
}

/// A `code:` or `table:` header line, as something to FOLLOW.
///
/// Cosense serves both as files — `/api/code/<project>/<title>/<name>` and
/// `/api/table/<project>/<title>/<name>.csv` — so the header line can be a
/// link like any other, and `Enter` saves it the way it saves an
/// attachment. No new key, no invented download path.
fn block_export_link(text: &str, src: usize) -> Option<LinkItem> {
    let body = text.trim_start();
    let (csv, name) = if let Some(n) = body.strip_prefix("code:") {
        (false, n.trim())
    } else if let Some(n) = body.strip_prefix("table:") {
        (true, n.trim())
    } else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let label = if csv { format!("{name}.csv") } else { name.to_string() };
    Some(LinkItem::Export { label, src, csv })
}

/// The text of the block whose header is at `src`: the code as it is
/// written, or the table as CSV.
///
/// A Cosense table is tab-separated cells, so the conversion is
/// mechanical; only a cell holding a comma, a quote or a newline needs
/// quoting (RFC 4180), and that is decided per cell rather than guessed.
fn block_export_body(lines: &[PageLine], src: usize, csv: bool) -> String {
    let Some(header) = lines.get(src) else { return String::new() };
    let indent = indent_of(&header.text).chars().count();
    let mut out: Vec<String> = Vec::new();
    for line in lines.iter().skip(src + 1) {
        let depth = line.text.chars().take_while(|c| c.is_whitespace()).count();
        if depth <= indent && !line.text.trim().is_empty() {
            break;
        }
        if depth <= indent {
            if csv {
                break; // a blank line ends a table
            }
            out.push(String::new());
            continue;
        }
        let body: String = line.text.chars().skip(indent + 1).collect();
        out.push(if csv { csv_row(&body) } else { body });
    }
    // Trailing blank lines belong to the page, not to the block.
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// One table row as CSV (RFC 4180 quoting).
fn csv_row(row: &str) -> String {
    row.split('\t')
        .map(|cell| {
            let cell = cell.trim_end();
            if cell.contains([',', '"', '\n']) {
                format!("\"{}\"", cell.replace('"', "\"\""))
            } else {
                cell.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn link_item_for_url(label: String, url: String) -> LinkItem {
    if is_scrapbox_file_url(&url) {
        LinkItem::File { label, url }
    } else if let Some(page) = gyazo_permalink(&url) {
        // An inline image opens its Gyazo page (comments, original, Teams
        // org), not the raw pixel URL.
        let label = if label == url { "gyazo".to_string() } else { label };
        LinkItem::Url { label, url: page }
    } else {
        LinkItem::Url { label, url }
    }
}

/// Uploaded-file links on a raw Scrapbox line (see `labelled_urls`).
#[cfg(test)]
fn files_on_line(text: &str) -> Vec<(String, String)> {
    labelled_urls(text).into_iter().filter(|(_, u)| is_scrapbox_file_url(u)).collect()
}

/// Where downloads go, asked in the order a reader would expect to be
/// asked: `--download-dir`, `COSENSE_DOWNLOAD_DIR`, the desktop's own
/// `XDG_DOWNLOAD_DIR`, `~/Downloads`, and finally the working directory.
///
/// `~/Downloads` was the whole rule, which is a machine-shaped assumption:
/// a server account or a stripped-down box has no such directory, and the
/// file then landed in whatever directory the viewer happened to start in
/// without anyone having chosen it.
///
/// The bool is "the user named this place". A named directory is created if
/// it is missing — naming it is the request. A directory merely FOUND has
/// to exist already, or the search moves on.
fn pick_download_dir(
    explicit: Option<&str>,
    env_dir: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
    cwd: std::path::PathBuf,
) -> (std::path::PathBuf, bool) {
    let named = explicit
        .into_iter()
        .chain(env_dir)
        .map(str::trim)
        .find(|s| !s.is_empty());
    if let Some(dir) = named {
        return (expand_home(dir, home), true);
    }
    let found = xdg
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_home(s, home))
        .filter(|d| d.is_dir())
        .or_else(|| {
            home.map(|h| std::path::PathBuf::from(h).join("Downloads")).filter(|d| d.is_dir())
        });
    (found.unwrap_or(cwd), false)
}

/// `~` and `~/…` against a known home. Config files and shells both write
/// paths that way, and the env var arrives unexpanded when it was set by
/// hand rather than by the shell.
fn expand_home(path: &str, home: Option<&str>) -> std::path::PathBuf {
    let Some(home) = home else { return std::path::PathBuf::from(path) };
    match path {
        "~" => std::path::PathBuf::from(home),
        p if p.starts_with("~/") => std::path::PathBuf::from(home).join(&p[2..]),
        p => std::path::PathBuf::from(p),
    }
}

/// `<dir>/<name>`, where `<name>` is the link's label when it carries an
/// extension, else the URL's file name; an existing file is not overwritten
/// (`name (2).pdf`, `name (3).pdf`, …).
fn download_path_in(dir: &std::path::Path, label: &str, url: &str) -> std::path::PathBuf {
    let raw = if label.contains('.') { label } else { file_name_of_url(url) };
    let name: String = raw.chars().map(|c| if c == '/' || c == '\0' { '_' } else { c }).collect();
    let mut path = dir.join(&name);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Follow one link: pages navigate, files download, URLs go to the browser.
fn activate_link(app: &mut App, ctx: &Ctx, item: LinkItem) {
    match item {
        LinkItem::Page(title) => {
            let project = app.project.clone();
            navigate_to(app, ctx, &project, &title)
        }
        LinkItem::ProjectPage { project, title } => navigate_to(app, ctx, &project, &title),
        // A whole project: its index, which is what Cosense's home screen
        // is for. The place being left goes on the back stack, as it does
        // for a page.
        LinkItem::ProjectIndex { project } => {
            let from = app.here();
            open_index(app, ctx, &project, String::new());
            if app.index.is_some() {
                app.history.push(from);
                app.forward.clear();
                app.status = format!("→ /{project}");
            }
        }
        LinkItem::File { label, url } => app.start_download(ctx, label, url),
        LinkItem::Export { label, src, csv } => {
            let body = block_export_body(&app.lines, src, csv);
            let dest = download_path_in(&ctx.download_dir, &label, "");
            app.status = match std::fs::write(&dest, body) {
                Ok(()) => {
                    let shown = dest.display().to_string();
                    if open_in_browser(&shown) {
                        t!("保存しました {shown} · 開きました", "saved {shown} · opened")
                    } else {
                        t!("保存しました {shown}", "saved {shown}")
                    }
                }
                Err(e) => t!("保存に失敗しました: {label} — {e}", "save failed: {label} — {e}"),
            };
        }
        LinkItem::Url { url, .. } => {
            app.status = if open_in_browser(&url) {
                t!("開きました {url}", "opened {url}")
            } else {
                t!("ブラウザを開けません", "failed to open browser")
            };
        }
    }
}

fn short(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

/// What `y` copies, and what to call it in the status line.
///
/// Always the **Cosense source**: indentation, `[]` links, `code:` blocks,
/// exactly as the page stores them. That is what survives a round trip
/// into another Cosense page, an editor, or a commit message. (A rendered
/// copy would lose the notation and the structure with it.)
///
/// The caret line comes from the session buffer, so what you copy is what
/// you see, not the last version the server heard.
fn copy_payload(app: &App, whole_page: bool) -> Option<(String, String)> {
    if app.lines.is_empty() {
        return None;
    }
    let text_of = |i: usize| -> String {
        match app.session.as_ref() {
            Some(s) if s.line == i => s.input.buf.clone(),
            _ => app.lines[i].text.clone(),
        }
    };
    if whole_page {
        let body: Vec<String> = (0..app.lines.len()).map(text_of).collect();
        return Some((body.join("\n"), format!("page ({} lines)", body.len())));
    }
    if let Some(s) = app.session.as_ref() {
        if let Some((a, b)) = s.sel_span() {
            return Some((s.input.buf[a..b].to_string(), "selection".into()));
        }
        // A range across lines copies as text too: the anchor line's
        // tail, the lines between whole, the far line's head — what the
        // range really holds, joinable back into one paste.
        if let Some(((tl, tb), (bl, bb))) = s.sel_ends() {
            if tl != bl && bl < app.lines.len() {
                let top = text_of(tl);
                let mut parts = vec![top[floor_boundary(&top, tb)..].to_string()];
                parts.extend((tl + 1..bl).map(&text_of));
                let bot = text_of(bl);
                parts.push(bot[..floor_boundary(&bot, bb)].to_string());
                let label = format!("selection ({} lines)", parts.len());
                return Some((parts.join("\n"), label));
            }
        }
    }
    let (a, b) = match app.selection {
        Some(sel) => sel.range(),
        None => {
            let line = app.session.as_ref().map(|s| s.line).unwrap_or(app.cursor);
            (line, line)
        }
    };
    let b = b.min(app.lines.len() - 1);
    if a > b {
        return None;
    }
    let body: Vec<String> = (a..=b).map(text_of).collect();
    let label = if body.len() == 1 { "line".into() } else { format!("{} lines", body.len()) };
    Some((body.join("\n"), label))
}

/// Copy `payload` and report it on the status line — including the ways it
/// can fail, which are silent otherwise (no clipboard tool, a terminal
/// that will not take OSC 52, a copy too large to send that way).
fn copy_and_report(app: &mut App, payload: Option<(String, String)>) {
    let Some((text, label)) = payload else {
        app.status = t!("コピーするものがありません", "nothing to copy");
        return;
    };
    app.status = if copy_to_clipboard(&text) {
        format!("✓ copied {label}")
    } else {
        t!("コピーできません — クリップボードのコマンドが無く、端末も OSC 52 を拒否しました", "copy failed — no clipboard tool and the terminal refused OSC 52")
    };
}

fn copy_to_clipboard(text: &str) -> bool {
    // Over SSH, `pbcopy` and friends would write into the clipboard of the
    // machine the viewer runs on — not the one the hands are on. OSC 52
    // hands the text to the terminal EMULATOR, which is always local, so
    // it is the only thing that works remotely.
    let remote =
        std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
    if !remote {
        for (bin, args) in [
            ("pbcopy", &[][..]),
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ] {
            if pipe_to(bin, args, text) {
                return true;
            }
        }
    }
    osc52_copy(text)
}

/// Feed `text` to a clipboard helper on stdin. False when it is not
/// installed or refuses.
fn pipe_to(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin).args(args).stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Hand the text to the terminal emulator itself (OSC 52). Works through
/// SSH, and through tmux when the sequence is wrapped in its passthrough.
///
/// Terminals cap what they will take (commonly ~74 KB of base64); a copy
/// too big to send is refused here rather than silently truncated.
fn osc52_copy(text: &str) -> bool {
    const MAX_BYTES: usize = 48 * 1024;
    if text.len() > MAX_BYTES {
        return false;
    }
    let payload = format!("\x1b]52;c;{}\x07", b64_encode(text.as_bytes()));
    let seq = if std::env::var_os("TMUX").is_some() {
        // tmux only forwards what it is told to forward.
        format!("\x1bPtmux;{}\x1b\\", payload.replace('\x1b', "\x1b\x1b"))
    } else {
        payload
    };
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok()
}

/// Standard base64, for OSC 52. (The decoder lives in `chrome`; this is
/// the only place that needs to encode.)
fn b64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Load `project/title` and install it, pushing the current page onto
/// history. `project` may differ from the current one (`[/project/title]`).
fn navigate_to(app: &mut App, ctx: &Ctx, project: &str, title: &str) {
    let from = app.here();
    // The caller has nothing to undo: it was not holding anything the way
    // the index holds its list.
    let _ = navigate_from(app, ctx, project, title, from);
}

/// `navigate_to`, saying explicitly where it is being left from — the
/// index closes before the page loads, so it has to name itself while it
/// still can.
#[must_use = "a failed navigation leaves the reader where they were"]
fn navigate_from(app: &mut App, ctx: &Ctx, project: &str, title: &str, from: Place) -> bool {
    let same_project = app.project == project;
    match load_page(ctx, project, title) {
        Ok(loaded) => {
            app.history.push(from);
            app.forward.clear();
            app.set_page(loaded, ctx);
            app.status = if page_is_uncreated(app) {
                // Following a link to a page nobody has written yet is how
                // a wiki grows. Say what it is, and what makes it real.
                t!("未作成のページ — e / o で書き始めると作成されます（{title}）", "an uncreated page — e / o starts writing it ({title})")
            } else if same_project {
                format!("→ {title}")
            } else {
                format!("→ /{project}/{title}")
            };
            true
        }
        Err(e) => {
            app.status = t!("開けません: /{project}/{title} — {e}", "open failed: /{project}/{title} — {e}");
            false
        }
    }
}

/// Events processed per frame at most, so an input flood can't starve the
/// draw (the rest is handled on the next frame). akapen's constant.
const MAX_EVENTS_PER_FRAME: usize = 64;

/// What one key press asks the event loop to do. `Editor` needs the loop
/// itself (it suspends the terminal), so it travels up instead of being
/// handled in place.
enum Action {
    Continue,
    Quit,
    Editor,
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    loop {
        // Keep command mode in ASCII (retried while the IME helper builds).
        if !app.ime_ready && app.composing.is_none() && app.session.is_none() {
            app.ime_ready = app.session_ime.force_ascii();
        }
        // Commit outcomes from the serial worker (✓ / conflict recovery).
        while let Ok(outcome) = app.commit_res_rx.try_recv() {
            handle_commit_outcome(app, ctx, outcome);
        }
        // A commit-chain gap (or the first event after a fresh join) asks
        // the ws thread to refetch the page in the background — no network
        // I/O on the UI thread; the result arrives as a Resynced event.
        ws_send_resync_request(app);
        // A Resynced that arrived while a gate was up applies now that it
        // is down (the latest one is kept; buffered commits that preceded
        // it are superseded by the fresh page).
        ws_apply_held_resync(app, ctx);
        // Remote commits that were buffered while a gate was up (inflight,
        // dirty line, composer, history) apply now that it is down.
        ws_flush_pending(app, ctx);
        // Websocket push: commit events (sub-second diffs) and resyncs.
        while let Ok(ev) = app.ws_rx.try_recv() {
            handle_ws_event(app, ctx, ev);
        }
        // Web-side edits, freshly polled (applied only when safe).
        while let Ok(polled) = app.poll_rx.try_recv() {
            apply_remote(app, ctx, polled);
        }
        // A page typed into existence goes up as soon as the queue is free.
        dispatch_create(app);
        // Install any images that finished downloading, then draw.
        if app.drain_images() | app.drain_web_renders() {
            app.laid_width = 0; // heights changed — rebuild layout
        }
        // A link's fate came back: the page has to be coloured again.
        if app.drain_link_probes() {
            rerender(app, ctx);
        }
        app.probe_unknown_links();
        // Both are no-ops on most frames: a diagram is only requested when
        // its own source changed, and only re-encoded when the pane crossed
        // the column cap it was built for.
        app.ensure_visibility(ctx);
        app.drain_visibility();
        app.start_web_renders(capability::Trigger::Auto);
        app.rescale_diagrams();
        app.expire_web_notice();
        app.drain_downloads();
        terminal.draw(|f| ui(f, app, ctx))?;
        // Wait up to one tick for input (short, so arriving images refresh
        // promptly), then drain everything that queued up into ONE frame.
        // A wheel flick — or herdr, which delivers bursts at once — queues
        // dozens of mouse events; a redraw per event (images included)
        // made scrolling crawl. akapen's event_loop does the same.
        // Idle: one wake-up per 120ms is enough to slot in arriving images.
        // While a diagram renders, the shimmer wants smoother frames — but
        // only then, so an idle viewer still costs ~8 wake-ups a second.
        let tick = if app.web_shimmer.is_empty() { 120 } else { 60 };
        if !event::poll(Duration::from_millis(tick))? {
            continue;
        }
        for _ in 0..MAX_EVENTS_PER_FRAME {
            if !event::poll(Duration::ZERO)? {
                break;
            }
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    match handle_key(app, ctx, k) {
                        Action::Quit => {
                            flush_commits(terminal, app, ctx);
                            return Ok(());
                        }
                        Action::Editor => {
                            editor_roundtrip(terminal, app, ctx);
                            break; // geometry may have changed — redraw first
                        }
                        Action::Continue => {}
                    }
                }
                Event::Mouse(m) => handle_mouse(app, ctx, m),
                Event::Paste(data) => handle_paste(app, ctx, &data),
                _ => {}
            }
        }
    }
}

/// Quit: close the session (committing its dirty line) and wait for the
/// serial worker to drain — nothing typed is left behind. Bounded at 15 s;
/// a hung network reports instead of trapping the user in a dead TUI.
fn flush_commits(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) {
    // A block still in hand is work the reader did: let go of it (which
    // sends the drag) before the queue is drained, exactly as a dirty caret
    // line is committed here.
    if app.move_mode.is_some() {
        leave_move_mode(app, ctx);
    }
    if app.session.is_some() {
        leave_session(app, ctx);
    }
    // Leaving the session may have been the edit that owes the server a
    // page. Nothing dispatches it after this point, so quitting straight
    // after writing a new page would have thrown the whole page away.
    dispatch_create(app);
    let deadline = Instant::now() + Duration::from_secs(15);
    while app.inflight > 0 && Instant::now() < deadline {
        app.status = t!("…送信中の編集 {} 件", "…flushing {} edit(s)", app.inflight);
        let _ = terminal.draw(|f| ui(f, app, ctx));
        match app.commit_res_rx.recv_timeout(Duration::from_millis(300)) {
            Ok(outcome) => handle_commit_outcome(app, ctx, outcome),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if app.inflight > 0 {
        // Leave a trace on the real terminal after restore.
        eprintln!("warning: {} edit(s) may not have reached the server", app.inflight);
    }
}

/// A bracketed paste: the comment composer takes it verbatim; the edit
/// session takes it structurally (a multi-line paste becomes real lines,
/// see `session_paste`).
fn handle_paste(app: &mut App, ctx: &Ctx, data: &str) {
    // Pasting is not a move key, so it lets go of the block first for the
    // same reason every other key does: the page must not change under a
    // block still being carried.
    if app.move_mode.is_some() {
        leave_move_mode(app, ctx);
    }
    let clean = data.replace("\r\n", "\n").replace('\r', "\n");
    // A single trailing newline is an artefact of how the text was copied
    // (a whole line from a file, a terminal selection, an IME committing a
    // phrase), not a request for an empty line after it. Keeping it left a
    // blank line behind every such paste — and with the caret carried onto
    // it, the caret looked like it had jumped one line too far.
    let clean = clean.strip_suffix('\n').unwrap_or(&clean).to_string();
    if clean.is_empty() {
        return;
    }
    if let Some(input) = app.composing.as_mut() {
        input.insert_str(&clean);
        return;
    }
    // The index's filter is a text field too: pasting a title into it is
    // the fastest way to reach a page someone sent you. Only the first
    // line — a filter is one line by definition.
    if let Some(ix) = app.index.as_mut() {
        if let Some(first) = clean.lines().next() {
            let mut f = ix.filter.clone();
            f.push_str(first);
            ix.set_filter(f);
        }
        return;
    }
    if app.session.is_some() {
        session_paste(app, ctx, &clean);
        return;
    }
    // READ has nowhere to put it. Silence here reads as "paste is broken",
    // so say where it does go.
    if !clean.trim().is_empty() {
        app.status = t!("貼り付けは編集中に — e / i / o で入ってから", "paste while editing — enter with e / i / o first");
    }
}

/// Keys while the index owns the screen.
///
/// A picker's keys: move, type to narrow, Enter to go. Typing goes to the
/// filter rather than to commands, so there is no mode to remember. The
/// exceptions are navigation (`[`/`]`, Esc, Enter, Tab) and movement keys;
/// back must remain back on every screen rather than becoming filter text.
fn handle_index_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    use cosense::index::{Pane, Row};
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let page_rows = app.index_list_rect.height.max(1) as i32;
    let preview_on = app.index_preview_rect.width > 0 && app.index_preview_rect.height > 0;
    let Some(ix) = app.index.as_mut() else { return Action::Continue };
    match (k.code, ctrl) {
        (KeyCode::Char('['), false) => go_history(app, ctx, true),
        (KeyCode::Char(']'), false) => go_history(app, ctx, false),
        (KeyCode::Esc, _) => {
            if app.history.is_empty() {
                // A title-less launch starts at the site top and therefore
                // has no earlier route. The latest page is already loaded
                // underneath, which remains a useful fallback.
                app.index = None;
                app.status = String::new();
            } else {
                go_history(app, ctx, true);
            }
        }
        // The excerpt is a second place to be, so Tab moves between it and
        // the list — but only when there IS an excerpt dock.
        (KeyCode::Tab, _) if preview_on => {
            ix.focus = match ix.focus {
                Pane::List => Pane::Preview,
                Pane::Preview => Pane::List,
            };
        }
        (KeyCode::Enter, _) => {
            let target = match ix.rows().get(ix.cursor) {
                Some(Row::Page(e)) => Some((e.title.clone(), false)),
                Some(Row::Create(name)) => Some((name.to_string(), true)),
                None => None,
            };
            open_from_index(app, ctx, target);
        }
        // Scrolling the preview when it has the focus; moving the list
        // otherwise. Same fingers either way.
        (KeyCode::Down, _) | (KeyCode::Char('j'), false) if ix.focus == Pane::Preview => {
            ix.preview_scroll = ix.preview_scroll.saturating_add(1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), false) if ix.focus == Pane::Preview => {
            ix.preview_scroll = ix.preview_scroll.saturating_sub(1);
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
            ix.move_cursor(1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), true) => {
            ix.move_cursor(-1);
        }
        (KeyCode::Char('j'), false) if ix.filter.is_empty() => {
            ix.move_cursor(1);
        }
        (KeyCode::Char('k'), false) if ix.filter.is_empty() => {
            ix.move_cursor(-1);
        }
        (KeyCode::Char('g'), false) if ix.filter.is_empty() => {
            ix.move_cursor(i32::MIN / 2);
        }
        (KeyCode::Char('G'), false) if ix.filter.is_empty() => {
            ix.move_cursor(i32::MAX / 2);
        }
        (KeyCode::PageDown, _) | (KeyCode::Char('d'), true) => {
            ix.move_cursor(page_rows);
        }
        (KeyCode::PageUp, _) => {
            ix.move_cursor(-page_rows);
        }
        (KeyCode::Backspace, _) => {
            let mut f = ix.filter.clone();
            f.pop();
            ix.set_filter(f);
        }
        // `^u` clears the filter, as it clears a line everywhere else.
        (KeyCode::Char('u'), true) => ix.set_filter(String::new()),
        // Anything printable narrows the list. A filter is one line, so
        // there is nothing else it could be.
        (KeyCode::Char(c), false) => {
            let mut f = ix.filter.clone();
            f.push(c);
            ix.set_filter(f);
        }
        _ => {}
    }
    Action::Continue
}

/// Structural arrows use exact modifiers. This deliberately excludes
/// Shift+Alt, Ctrl+Alt, and every other combination.
fn outline_arrow(k: event::KeyEvent) -> Option<(bool, OutlineDirection)> {
    let block = match k.modifiers {
        KeyModifiers::CONTROL => false,
        KeyModifiers::ALT => true,
        _ => return None,
    };
    let direction = match k.code {
        KeyCode::Left => OutlineDirection::Left,
        KeyCode::Right => OutlineDirection::Right,
        KeyCode::Up => OutlineDirection::Up,
        KeyCode::Down => OutlineDirection::Down,
        _ => return None,
    };
    Some((block, direction))
}

/// The portable ^g follow-up. Uppercase characters may arrive with or
/// without an explicit SHIFT flag depending on the terminal protocol.
fn outline_prefix_command(k: event::KeyEvent) -> Option<(bool, OutlineDirection)> {
    let plain = k.modifiers == KeyModifiers::NONE;
    let upper = plain || k.modifiers == KeyModifiers::SHIFT;
    match k.code {
        KeyCode::Char('h') if plain => Some((false, OutlineDirection::Left)),
        KeyCode::Char('j') if plain => Some((false, OutlineDirection::Down)),
        KeyCode::Char('k') if plain => Some((false, OutlineDirection::Up)),
        KeyCode::Char('l') if plain => Some((false, OutlineDirection::Right)),
        KeyCode::Char('H') if upper => Some((true, OutlineDirection::Left)),
        KeyCode::Char('J') if upper => Some((true, OutlineDirection::Down)),
        KeyCode::Char('K') if upper => Some((true, OutlineDirection::Up)),
        KeyCode::Char('L') if upper => Some((true, OutlineDirection::Right)),
        _ => None,
    }
}

fn outline_mutation_blocked(app: &mut App) -> bool {
    if app.move_mode.is_some() {
        app.status = t!(
            "移動モード中 — Esc/Enter で確定してから",
            "in move mode — commit it first with Esc/Enter"
        );
        return true;
    }
    if app.outline_refresh_needed {
        app.status = t!(
            "アウトライン移動後の再読み込み待ち — ページを開き直してください",
            "outline refresh required — reopen the page before editing"
        );
        return true;
    }
    if app.outline_pending.is_some() {
        app.status = t!(
            "アウトライン操作の完了待ち — 編集・取り消し・やり直しは待ってください",
            "waiting for outline action — edit, undo, and redo are temporarily blocked"
        );
        return true;
    }
    false
}

fn outline_snapshot(app: &App) -> OutlineSnapshot {
    OutlineSnapshot {
        project: app.project.clone(),
        page_id: app.page_id.clone(),
        install_gen: app.gen_now(),
        lines: app.lines.clone(),
        cursor: app.cursor,
        selection: app.selection,
        undo_stack: app.undo_stack.clone(),
        redo_stack: app.redo_stack.clone(),
        history_dropped: app.history_dropped,
    }
}

fn restore_outline_snapshot(app: &mut App, snapshot: &OutlineSnapshot) {
    app.lines = snapshot.lines.clone();
    app.cursor = snapshot.cursor;
    app.selection = snapshot.selection;
    app.undo_stack = snapshot.undo_stack.clone();
    app.redo_stack = snapshot.redo_stack.clone();
    app.history_dropped = snapshot.history_dropped;
}

fn outline_error(app: &mut App, error: PlanError) {
    app.status = match error {
        PlanError::InvalidTarget => t!("カーソルの下にソース行がありません", "no source line under the cursor"),
        PlanError::TitleProtected => t!("タイトル行はアウトライン操作できません", "the title line is protected"),
        PlanError::CannotOutdent => t!(
            "字下げを戻せません — 対象の全行に字下げが必要です",
            "cannot outdent — every target line must be indented"
        ),
        PlanError::Boundary => t!("これ以上移動できません", "cannot move any farther"),
        PlanError::NotSibling => t!(
            "同じ親を持つ兄弟ブロックがありません",
            "no sibling block with the same parent"
        ),
    };
}

/// The refusals every outline action shares, in the order the reader
/// meets them. `false` means the reason is already on the status line.
fn outline_action_allowed(app: &mut App) -> bool {
    // An ordinary commit may still be in flight: the gate waits for its own
    // job id, and the serial worker keeps the ops in order. Only a second
    // structural action has to wait, because there is one snapshot to roll
    // back to and one gate to release.
    if outline_mutation_blocked(app) {
        return false;
    }
    if app.web_unsynced {
        app.status = t!(
            "サーバーの内容を読み直すまでアウトライン操作できません",
            "outline actions require a fresh server copy"
        );
        return false;
    }
    if app.time.is_some() {
        app.status = t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)");
        return false;
    }
    if !ensure_editable(app) {
        return false;
    }
    if app.cursor >= app.lines.len() {
        outline_error(app, PlanError::InvalidTarget);
        return false;
    }
    if page_is_uncreated(app) {
        app.status = t!(
            "未作成ページではアウトライン操作できません",
            "outline actions are unavailable until the page is created"
        );
        return false;
    }
    true
}

/// Hand one structural op list to the single write path, under the gate
/// and the rollback every outline action shares: the snapshot is the page
/// as it stands NOW, so a job the worker never took leaves no screen-only
/// fact behind.
fn queue_outline_action(app: &mut App, ctx: &Ctx, label: &str, done: String, ops: Vec<EditOp>) {
    let snapshot = outline_snapshot(app);
    if let Some(job) = do_edit(app, ctx, label, ops) {
        app.outline_pending = Some(OutlinePending { job, snapshot });
        app.follow = true;
        app.status = done;
    } else {
        // The worker never took the structural job. Put the clean
        // pre-action model back immediately; an optimistic move must never
        // become a screen-only fact.
        restore_outline_snapshot(app, &snapshot);
        rerender(app, ctx);
    }
}

/// Lower one pure outline plan into the existing edit queue. Replacements
/// keep IDs. Moves delete and recreate only the user's target.
fn edit_outline(app: &mut App, ctx: &Ctx, block: bool, direction: OutlineDirection) {
    if !outline_action_allowed(app) {
        return;
    }
    if block && app.selection.is_some() {
        app.status = t!(
            "選択中はブロック操作できません — Esc で選択を解除",
            "block actions are unavailable with a selection — Esc clears it"
        );
        return;
    }

    let scope = if block {
        OutlineScope::Block(app.cursor)
    } else {
        let (start, end) = app.selection.map(|selection| selection.range()).unwrap_or((app.cursor, app.cursor));
        OutlineScope::Lines(LineRange::new(start, end))
    };
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    let plan = match cosense::outline::plan(&source, scope, direction) {
        Ok(plan) => plan,
        Err(error) => {
            outline_error(app, error);
            return;
        }
    };

    let (label, done, ops) = match plan {
        OutlinePlan::Replace { range, texts } => {
            let ops = (range.start..=range.end)
                .zip(texts)
                .map(|(line, text)| EditOp::Replace { id: app.lines[line].id.clone(), text })
                .collect();
            (
                t!("アウトラインの字下げ", "outline indent"),
                t!("…字下げを保存中", "…saving indentation"),
                ops,
            )
        }
        OutlinePlan::Move { range, destination, destination_start: _ } => {
            let anchor = match destination {
                Destination::Before(line) => app.lines[line].id.clone(),
                Destination::End => "_end".to_string(),
            };
            let inserted: Vec<(String, String)> = app.lines[range.start..=range.end]
                .iter()
                .map(|line| (new_line_id(), line.text.clone()))
                .collect();
            let mut ops: Vec<EditOp> = app.lines[range.start..=range.end]
                .iter()
                .map(|line| EditOp::Delete { id: line.id.clone() })
                .collect();
            ops.push(EditOp::Insert { anchor, lines: inserted });
            (
                t!("アウトラインの移動", "outline move"),
                t!("…移動を保存中", "…saving move"),
                ops,
            )
        }
    };

    queue_outline_action(app, ctx, &label, done, ops);
}

/// Where the grabbed block sits right now, found by its frozen ids. `None`
/// means the run is no longer there, which the mode itself cannot cause.
fn move_block_range(app: &App) -> Option<(usize, usize)> {
    let block = &app.move_mode.as_ref()?.block;
    let first = block.first()?;
    let start = app.lines.iter().position(|line| line.id == *first)?;
    let end = start + block.len() - 1;
    let run = app.lines.get(start..=end)?;
    run.iter()
        .map(|line| line.id.as_str())
        .eq(block.iter().map(|id| id.as_str()))
        .then_some((start, end))
}

/// `m`: grab the block under the cursor and stay in the mode until
/// `Esc`/`Enter`. Nothing is sent yet — the grab is a promise to send one
/// commit later.
fn enter_move_mode(app: &mut App, ctx: &Ctx) {
    if !outline_action_allowed(app) {
        return;
    }
    // A block operation with a selection open has no single answer — the
    // same refusal the Alt and ^g block bindings give. Refusing is better
    // than carrying a selection that the drag would silently drop.
    if app.selection.is_some() {
        app.status = t!(
            "選択中はブロック操作できません — Esc で選択を解除",
            "block actions are unavailable with a selection — Esc clears it"
        );
        return;
    }
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    let range = match cosense::outline::grab(&source, app.cursor) {
        Ok(range) => range,
        Err(error) => {
            outline_error(app, error);
            return;
        }
    };
    app.move_mode = Some(MoveMode {
        block: app.lines[range.start..=range.end]
            .iter()
            .map(|line| line.id.clone())
            .collect(),
        before: app.lines.clone(),
        cursor: app.cursor,
        selection: app.selection,
    });
    app.follow = true;
    app.status.clear();
    rerender(app, ctx);
}

/// One press inside the mode. Purely local: the same `PageLine` values are
/// reordered or re-indented, so every id — and every permalink, telomere
/// and comment hanging off it — stays exactly where it was. Nothing goes
/// to the server, nothing goes onto the undo stack.
fn move_mode_step(app: &mut App, ctx: &Ctx, direction: OutlineDirection, whole_sibling: bool) {
    let Some((start, end)) = move_block_range(app) else {
        // The grabbed lines are gone. Nothing in the mode can do that, and
        // remote application is held while it is up, so this is the
        // unexpected case — which is exactly why it must not keep the
        // half-dragged arrangement. The page goes back to what it was when
        // the block was picked up, and the disagreement is recorded.
        let Some(mode) = app.move_mode.take() else { return };
        app.lines = mode.before;
        app.cursor = mode.cursor.min(app.lines.len().saturating_sub(1));
        app.selection = mode.selection;
        app.mark_desynced();
        app.follow = true;
        rerender(app, ctx);
        app.status = t!(
            "つかんでいたブロックが見つからないので、つかむ前の並びに戻しました",
            "the grabbed block is gone — restored the arrangement from before the grab"
        );
        return;
    };
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    // Two vertical steps, and the reader picks which one. `Lines` moves the
    // block past ONE source line, keeping its depth: it never refuses (bar
    // the title), and Up and Down are exact inverses. `Grabbed` steps over
    // a whole sibling subtree at once, which is the fast way to reorder
    // sections but has nowhere to go when there is no sibling.
    //
    // Requiring the sibling step was the mistake this replaces: it refused
    // moves that were reachable anyway by going out a level, stepping, and
    // coming back in — so the rule cost keystrokes without protecting any
    // arrangement.
    let range = LineRange::new(start, end);
    let scope = if whole_sibling && matches!(direction, OutlineDirection::Up | OutlineDirection::Down)
    {
        OutlineScope::Grabbed(range)
    } else {
        OutlineScope::Lines(range)
    };
    let plan = match cosense::outline::plan(&source, scope, direction) {
        Ok(plan) => plan,
        Err(error) => {
            // Only the whole-sibling step can refuse, and the answer is
            // one press away: `j` steps a single line and never refuses.
            outline_error(app, error);
            return;
        }
    };
    // Ids survive every step, so the cursor and the selection travel by id.
    let cursor_id = app.lines.get(app.cursor).map(|line| line.id.clone());
    let selection_ids = app.selection.and_then(|selection| {
        let anchor = app.lines.get(selection.anchor)?.id.clone();
        let cursor = app.lines.get(selection.cursor)?.id.clone();
        Some((anchor, cursor))
    });
    match plan {
        OutlinePlan::Replace { range, texts } => {
            for (line, text) in (range.start..=range.end).zip(texts) {
                app.lines[line].text = text;
            }
        }
        OutlinePlan::Move { range, destination: _, destination_start } => {
            let block: Vec<PageLine> = app.lines.drain(range.start..=range.end).collect();
            let at = destination_start.min(app.lines.len());
            app.lines.splice(at..at, block);
        }
    }
    if let Some(id) = cursor_id {
        if let Some(line) = app.lines.iter().position(|line| line.id == id) {
            app.cursor = line;
        }
    }
    app.selection = selection_ids.and_then(|(anchor, cursor)| {
        let anchor = app.lines.iter().position(|line| line.id == anchor)?;
        let cursor = app.lines.iter().position(|line| line.id == cursor)?;
        Some(Selection { anchor, cursor })
    });
    app.follow = true;
    app.status.clear();
    rerender(app, ctx);
}

/// The ONE edit from the pre-mode page to the final arrangement: nothing
/// when the block came home, `Replace` (ids kept) when only its
/// indentation changed, and — exactly as cosense itself does it — delete
/// plus insert of the block ALONE when its position changed.
/// Returns `(commit label, status while saving, ops)`.
fn move_mode_edit(
    before: &[PageLine],
    after: &[PageLine],
    block: &[String],
) -> Option<(String, String, Vec<EditOp>)> {
    let moved = before
        .iter()
        .map(|line| line.id.as_str())
        .ne(after.iter().map(|line| line.id.as_str()));
    if !moved {
        let ops: Vec<EditOp> = before
            .iter()
            .zip(after)
            .filter(|(was, now)| was.text != now.text)
            .map(|(_, now)| EditOp::Replace { id: now.id.clone(), text: now.text.clone() })
            .collect();
        return (!ops.is_empty()).then(|| {
            (
                t!("アウトラインの字下げ", "outline indent"),
                t!("…字下げを保存中", "…saving indentation"),
                ops,
            )
        });
    }
    let first = block.first()?;
    let start = after.iter().position(|line| line.id == *first)?;
    let final_block = after.get(start..start + block.len())?;
    // The block is contiguous wherever it landed, so the line after it is
    // never one of its own — and it still exists in the pre-mode page.
    let anchor = after
        .get(start + block.len())
        .map(|line| line.id.clone())
        .unwrap_or_else(|| "_end".to_string());
    let mut ops: Vec<EditOp> =
        block.iter().map(|id| EditOp::Delete { id: id.clone() }).collect();
    ops.push(EditOp::Insert {
        anchor,
        lines: final_block
            .iter()
            .map(|line| (new_line_id(), line.text.clone()))
            .collect(),
    });
    Some((
        t!("アウトラインの移動", "outline move"),
        t!("…移動を保存中", "…saving move"),
        ops,
    ))
}

/// `Esc`/`Enter` (and anything else the reader asks for): let go of the
/// block and send the whole drag as one commit. The pre-mode arrangement
/// goes back first, so the ops are applied exactly once — by `do_edit`,
/// the same write path every other edit uses, which is also what makes it
/// one undo step and what puts the cursor back on the moved block.
fn leave_move_mode(app: &mut App, ctx: &Ctx) {
    let Some(mode) = app.move_mode.take() else { return };
    let after = std::mem::replace(&mut app.lines, mode.before);
    app.cursor = mode.cursor;
    app.selection = mode.selection;
    let Some((label, done, ops)) = move_mode_edit(&app.lines, &after, &mode.block) else {
        // The block came home: position and depth are what they were, so
        // there is nothing to tell the server.
        rerender(app, ctx);
        app.follow = true;
        app.status = t!(
            "移動モードを抜けました（変更なし）",
            "left move mode (nothing changed)"
        );
        return;
    };
    queue_outline_action(app, ctx, &label, done, ops);
}

/// One key press.
fn handle_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    if app.index.is_some() {
        return handle_index_key(app, ctx, k);
    }
    // Comment composer captures all keys. Emacs/readline-style line
    // editing, so a Japanese sentence can be corrected mid-line.
    if app.composing.is_some() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (k.code, ctrl) {
            (KeyCode::Esc, _) => {
                app.composing = None;
                app.ime_guard = None; // back to ASCII for command mode
                app.status = t!("キャンセルしました", "cancelled");
            }
            (KeyCode::Enter, _) => {
                let input = app.composing.take().unwrap();
                app.ime_guard = None; // back to ASCII for command mode
                finish_composer(app, input);
            }
            (KeyCode::Backspace, _) => in_input(app, Input::backspace),
            (KeyCode::Delete, _) | (KeyCode::Char('d'), true) => in_input(app, Input::delete),
            (KeyCode::Left, _) | (KeyCode::Char('b'), true) => in_input(app, Input::left),
            (KeyCode::Right, _) | (KeyCode::Char('f'), true) => in_input(app, Input::right),
            (KeyCode::Home, _) | (KeyCode::Char('a'), true) => in_input(app, Input::home),
            (KeyCode::End, _) | (KeyCode::Char('e'), true) => in_input(app, Input::end),
            (KeyCode::Char('w'), true) => in_input(app, Input::delete_word),
            (KeyCode::Char('u'), true) => in_input(app, Input::kill_to_start),
            (KeyCode::Char(ch), false) => {
                if let Some(c) = app.composing.as_mut() {
                    c.insert_char(ch);
                }
            }
            _ => {}
        }
        return Action::Continue;
    }

    // The modeless edit session owns everything next (SPEC §3).
    if app.session.is_some() {
        handle_session_key(app, ctx, k);
        return Action::Continue;
    }

    // Overlays capture keys while open.
    if app.overlay.is_some() {
        handle_overlay_key(app, ctx, k.code, k.modifiers);
        return Action::Continue;
    }

    // The sticky move mode owns the plain keys while it is up: h/j/k/l and
    // the bare arrows drag the grabbed block, Esc/Enter/m let go. Every
    // OTHER key lets go first — the drag's one commit goes out — and then
    // does its ordinary job, so navigating away can never leave the block
    // half moved.
    //
    // The arrows are bound because they are what a reader reaches for when
    // a block is visibly held: having them silently drop it and move the
    // cursor instead would be the one surprise this mode must not have.
    if app.move_mode.is_some() {
        // SHIFT counts as plain here: a terminal reports `H` as Shift+`h`,
        // and caps lock or the habit of the `^g H/J/K/L` block bindings
        // must not commit a half-finished drag.
        let plain = (k.modifiers - KeyModifiers::SHIFT) == KeyModifiers::NONE;
        // Lowercase and the bare arrows step one line; uppercase steps over
        // a whole sibling. Caps lock therefore gives a bigger jump, never a
        // commit.
        let drag = match k.code {
            KeyCode::Char('j') if plain => Some((OutlineDirection::Down, false)),
            KeyCode::Char('k') if plain => Some((OutlineDirection::Up, false)),
            KeyCode::Char('J') if plain => Some((OutlineDirection::Down, true)),
            KeyCode::Char('K') if plain => Some((OutlineDirection::Up, true)),
            KeyCode::Char('h' | 'H') if plain => Some((OutlineDirection::Left, false)),
            KeyCode::Char('l' | 'L') if plain => Some((OutlineDirection::Right, false)),
            KeyCode::Down if plain => Some((OutlineDirection::Down, false)),
            KeyCode::Up if plain => Some((OutlineDirection::Up, false)),
            KeyCode::Left if plain => Some((OutlineDirection::Left, false)),
            KeyCode::Right if plain => Some((OutlineDirection::Right, false)),
            _ => None,
        };
        if let Some((direction, whole_sibling)) = drag {
            move_mode_step(app, ctx, direction, whole_sibling);
            return Action::Continue;
        }
        match k.code {
            // `m` grabbed the block, so `m` puts it down again.
            KeyCode::Esc | KeyCode::Enter => {
                leave_move_mode(app, ctx);
                return Action::Continue;
            }
            KeyCode::Char('m' | 'M') if plain => {
                leave_move_mode(app, ctx);
                return Action::Continue;
            }
            _ => leave_move_mode(app, ctx),
        }
    }

    // ^g is a one-shot prefix: whatever follows is consumed, including Esc
    // and unknown keys, so the second key never leaks into another command.
    if app.outline_prefix {
        app.outline_prefix = false;
        if let Some((block, direction)) = outline_prefix_command(k) {
            edit_outline(app, ctx, block, direction);
        } else {
            app.status = t!("アウトライン操作を取り消しました", "outline action cancelled");
        }
        return Action::Continue;
    }
    if k.code == KeyCode::Char('g') && k.modifiers == KeyModifiers::CONTROL {
        app.outline_prefix = true;
        app.status.clear();
        return Action::Continue;
    }
    if let Some((block, direction)) = outline_arrow(k) {
        edit_outline(app, ctx, block, direction);
        return Action::Continue;
    }

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match (k.code, ctrl) {
        // ---- quit ---- (akapen default: q quits, Esc only cancels)
        (KeyCode::Char('q'), false) => return Action::Quit,
        (KeyCode::Esc, false) => {
            if app.time.is_some() {
                // Leaving history means the live page replaces the snapshot.
                // If that fetch fails there is no live page to show, so we
                // stay in history: `set_page` (which clears `time`) runs
                // only on success. Clearing `time` first would relabel the
                // SNAPSHOT's lines as NOW and editable, and let the renderer
                // screenshot today's page under a historical source's hash.
                if reload_page(app, ctx) {
                    app.status = t!("最新", "NOW");
                }
            } else if app.selection.is_some() {
                app.selection = None;
                app.status = t!("選択を解除しました", "selection cleared");
            } else {
                app.status.clear();
            }
        }

        // ---- time machine (←/→, akapen's timeline; server snapshots) ----
        (KeyCode::Left, false) if k.modifiers == KeyModifiers::NONE => travel(app, ctx, -1),
        (KeyCode::Right, false) if k.modifiers == KeyModifiers::NONE => travel(app, ctx, 1),

        // ---- move (akapen parity) ----
        // Shift+↑/↓ selects, in READ as in EDIT: the same fingers, the same
        // result. `v` then j/k (akapen) still works — this is the version
        // people try first.
        (KeyCode::Down, false) if k.modifiers == KeyModifiers::SHIFT => read_select_line(app, true),
        (KeyCode::Up, false) if k.modifiers == KeyModifiers::SHIFT => read_select_line(app, false),
        (KeyCode::Char('j'), false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(true),
        (KeyCode::Down, false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(true),
        (KeyCode::Char('k'), false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(false),
        (KeyCode::Up, false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(false),
        (KeyCode::Char('g'), false) => {
            if let Some(s) = app.first_src() {
                app.goto_src(s);
            }
        }
        (KeyCode::Char('G'), false) => {
            if let Some(s) = app.last_body_src() {
                app.goto_src(s);
            }
            // G means "bottom of the boxed page", not the related sections
            // below it. j/↓ can still continue from there into Links.
            if app.view_h > 0 {
                app.scroll = app.frame_end_scroll(app.view_h);
                app.follow = false;
            }
        }
        // Paging moves the CURSOR by a screenful / half (akapen): the
        // viewport follows it, so the eye keeps its place on the line.
        (KeyCode::PageDown, _) => app.move_cursor_display(app.view_h.max(1) as isize),
        (KeyCode::PageUp, _) => app.move_cursor_display(-(app.view_h.max(1) as isize)),
        (KeyCode::Char('d'), true) => app.move_cursor_display((app.view_h / 2).max(1) as isize),
        (KeyCode::Char('u'), true) => app.move_cursor_display(-((app.view_h / 2).max(1) as isize)),

        // ---- links & history (akapen's file ]/[ slot) ----
        (KeyCode::Enter, false) | (KeyCode::Char('f'), false) => {
            let mut links = app.cursor_line_links();
            match links.len() {
                0 => app.status = t!("この行にリンクはありません", "no link on this line"),
                1 => activate_link(app, ctx, links.remove(0)),
                _ => app.overlay = Some(Overlay::Links { items: links, cursor: 0 }),
            }
        }
        (KeyCode::Char('['), false) => go_history(app, ctx, true),
        (KeyCode::Char(']'), false) => go_history(app, ctx, false),

        // ---- mode toggle (akapen's `Tab view⇄source`) ----
        (KeyCode::Tab, _) => {
            // The cursor is a source line, so it carries over as is; the
            // re-layout additionally keeps that line on the same physical
            // screen row (akapen's `replace_view_preserving_cursor`).
            app.mode = match app.mode {
                Mode::View => Mode::Source,
                Mode::Source => Mode::View,
            };
            let (w, h) = (app.laid_width.max(1), app.view_h);
            app.relayout_preserving_screen_row(w, h);
            app.selection = None;
            app.status = match app.mode {
                Mode::View => "view".into(),
                Mode::Source => "source".into(),
            };
        }

        // ---- edit (akapen's `e edit` slot, back to its true meaning:
        //      the modeless session, SPEC-edit-session.md) ----
        (KeyCode::Char('e'), false) => {
            // caret at the END of the cursor line (続きを書く)
            let caret = app
                .cursor_src()
                .and_then(|s| app.lines.get(s))
                .map(|l| l.text.len())
                .unwrap_or(0);
            enter_session(app, ctx, app.cursor, caret);
        }
        (KeyCode::Char('i'), false) => {
            // caret at the start of the text (right after the indent)
            let caret = app
                .cursor_src()
                .and_then(|s| app.lines.get(s))
                .map(|l| indent_of(&l.text).len())
                .unwrap_or(0);
            enter_session(app, ctx, app.cursor, caret);
        }

        // ---- the sticky outline move mode (**m**ove) ----
        // One block, dragged with plain keys for as long as it takes, then
        // one commit. The modified arrows still do single steps from here
        // and from EDIT; this is the route that needs no modifier at all.
        (KeyCode::Char('m'), false) if k.modifiers == KeyModifiers::NONE => {
            enter_move_mode(app, ctx)
        }

        // ---- render this page's missing web artifacts ----
        // Rendering is manual by default: a page load only shows artifacts
        // it already has on disk, because launching a browser is by far the
        // most expensive thing this viewer does. Uppercase `R` is generic:
        // Mermaid today, and TeX / icons / ProjectCSS-backed views later.
        (KeyCode::Char('R'), false) => {
            if app.start_web_renders(capability::Trigger::Manual) {
                // Drawing; the blocks pulse and say the rest themselves.
            } else if app.web_pending.iter().any(|k| !app.images.contains_key(k)) {
                // The page-load cache probe still owns these keys, so the
                // request cannot be queued yet. Remember it and serve it the
                // moment the probe reports its misses — pressing `R` twice
                // should not be part of the interface.
                app.web_manual_wanted = true;
            } else if app.web_notice.is_none() {
                app.note_web_failure(match app.render_policy {
                    capability::RenderPolicy::Off => {
                        t!("diagram: レンダラは off です (COSENSE_WEB_RENDER)", "diagram: renderer is off (COSENSE_WEB_RENDER)")
                    }
                    _ => t!("diagram: 描画するものはありません", "diagram: nothing to draw"),
                });
            }
        }

        // ---- open in browser (moved from `e`: **w**eb) ----
        (KeyCode::Char('w'), false) => {
            let url = app.cursor_url();
            app.status = if open_in_browser(&url) {
                t!("開きました {url}", "opened {url}")
            } else {
                t!("ブラウザを開けません", "failed to open browser")
            };
        }

        // ---- undo / redo (the safety net; no confirmation gates) ----
        // `u` is the akapen/vim seat; `^z` is the same key the session
        // uses, so the reflex works whichever mode you happen to be in.
        (KeyCode::Char('u'), false) | (KeyCode::Char('z'), true) => {
            undo(app, ctx);
        }
        (KeyCode::Char('r'), true) => {
            redo(app, ctx);
        }

        // ---- comments (akapen parity) ----
        (KeyCode::Char('v'), false) => {
            if app.selection.is_some() {
                app.selection = None;
                app.status = t!("選択を解除しました", "selection cleared");
            } else if app.cursor >= app.lines.len() {
                app.status = t!("関連ページの行は選択できません", "related rows cannot be selected");
            } else {
                app.selection = Some(Selection::new(app.cursor));
                app.status = t!("選択中 — j/k で広げる · c でコメント", "selecting — j/k extend · c comment");
            }
        }
        (KeyCode::Char('c'), false) => {
            if app.time.is_some() {
                app.status = t!("履歴を表示中 — コメントは最新でのみ書けます（Esc で戻る）", "viewing history — comments need NOW (Esc)");
                return Action::Continue;
            }
            app.composing = Some(Input::new(String::new()));
            app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
            app.status = t!("コメントを入力 · Enter 保存 · Esc 取消", "type comment · Enter save · Esc cancel");
        }

        // ---- new lines (vim's o/O; the session opens on the new line) ----
        (KeyCode::Char('o'), false) => open_line(app, ctx, false),
        (KeyCode::Char('O'), false) => open_line(app, ctx, true),
        (KeyCode::Char('e'), true) => {
            // Whole-page edit in $EDITOR (the “big edit” path: multi-line,
            // IME-free — the editor is the user's own environment).
            if outline_mutation_blocked(app) || !ensure_editable(app) {
                return Action::Continue;
            }
            if app.time.is_some() {
                app.status = t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)");
                return Action::Continue;
            }
            return Action::Editor;
        }
        // `x` used to delete the cursor line (or the selection) right
        // here, in the mode meant for reading. Deletion now lives where
        // editing does: `^k` for a line, Shift+↑↓ then ⌫ for a range. The
        // key is kept only to say so — a key that silently stops working
        // is worse than one that explains itself.
        (KeyCode::Char('x'), false) => {
            app.status = t!("削除は編集中に — e で入って ^k（行）/ Shift+↑↓ と ⌫（範囲）", "delete while editing — e, then ^k (line) or Shift+↑↓ and ⌫ (range)");
        }
        (KeyCode::Char('d'), false) => {
            if let Some(i) = app.comment_at_cursor() {
                app.comments.remove(i);
                app.laid_width = 0;
                app.status = t!("コメントを削除しました", "comment deleted");
            } else {
                app.status = t!("この行にコメントはありません", "no comment on this line");
            }
        }
        // jump between comments on this page
        (KeyCode::Char('n'), true) => jump_comment(app, true),
        (KeyCode::Char('p'), true) => jump_comment(app, false),

        // ---- output ----
        // `y` copies what is under the cursor (or the selection); `Y` the
        // whole page. Copying the COMMENTS moved to the comments overlay
        // (`l` then `y`), where the comments are.
        (KeyCode::Char('y'), false) => {
            let payload = copy_payload(app, false);
            copy_and_report(app, payload);
        }
        (KeyCode::Char('Y'), false) => {
            let payload = copy_payload(app, true);
            copy_and_report(app, payload);
        }

        // ---- lists / help ----
        (KeyCode::Char('l'), false) => {
            app.overlay = Some(Overlay::Comments { cursor: 0 });
        }
        // The project index (akapen's `^o files` slot): a full-width list
        // with a short excerpt from the selected page docked below. Typing
        // filters it, so the old picker's fingers still work — they now
        // have a screen to work in.
        (KeyCode::Char('o'), true) => {
            let from = app.here();
            let project = app.project.clone();
            open_index(app, ctx, &project, String::new());
            if app.index.is_some() {
                // Opening the index is going somewhere, so it goes on the
                // stack: `[` from here returns to the page it was opened
                // from, and `[` from a page opened out of it returns here.
                app.history.push(from);
                app.forward.clear();
            }
        }
        // line detail: who edited the cursor line and when (toggle)
        (KeyCode::Char('t'), false) => {
            app.overlay = if matches!(app.overlay, Some(Overlay::LineInfo)) {
                None
            } else {
                // resolve the updater's name for THIS project before showing
                let want = app.cursor_src().and_then(|s| app.lines.get(s)).map(|l| l.user_id.clone());
                app.ensure_members(ctx, want.as_deref());
                Some(Overlay::LineInfo)
            };
        }
        (KeyCode::Char('?'), false) => app.overlay = Some(Overlay::Help),

        // Unbound: show what arrived. A NON-ASCII char here almost always
        // means the IME is on — say so in Japanese instead of a cryptic
        // "unbound key" (the ime helper normally prevents this state).
        _ => {
            if let KeyCode::Char(c) = k.code {
                if !c.is_ascii() {
                    // Reading is done in ASCII — j/k/e/q are the language
                    // here — so put the input source back rather than only
                    // complaining about it. (Typing Japanese belongs to the
                    // edit session, which turns the IME on by itself.)
                    let fixed = app.session_ime.force_ascii();
                    app.status = if fixed {
                        t!("英数に戻しました（日本語は e / i で編集に入ってから）", "switched back to ASCII (Japanese needs an edit session: e / i)")
                    } else {
                        t!("IMEがONのようです — 英数に切り替えてください（編集中は自動で日本語になります）", "the IME looks ON — switch to ASCII (an edit session turns it on for you)")
                    };
                    return Action::Continue;
                }
            }
            app.status = t!("割り当てのないキー: {:?} {:?}", "unbound key: {:?} {:?}", k.code, k.modifiers);
        }
    }
    Action::Continue
}

/// Apply one `Input` editing method to the open composer.
fn in_input(app: &mut App, f: fn(&mut Input)) {
    if let Some(c) = app.composing.as_mut() {
        f(c);
    }
}

/// Enter in the comment composer.
fn finish_composer(app: &mut App, input: Input) {
    let buf = input.buf;
    if buf.trim().is_empty() {
        app.status = t!("空のコメントは破棄しました", "empty comment discarded");
    } else if let Some(c) = app.make_comment(buf) {
        app.comments.push(c);
        app.selection = None;
        app.status = t!("コメントを保存しました（全 {} 件）", "comment saved ({} total)", app.comments.len());
        app.laid_width = 0; // force rebuild to weave the card
    } else {
        app.status = t!("コメントを行に結び付けられません", "could not anchor comment");
    }
}

// ---------------------------------------------------------------------------
// The edit engine (SPEC-edit-session.md §4–§6): the LOCAL model is
// authoritative — every edit applies to `app.lines` immediately (insert ids
// are client-generated, so no reload is ever needed) and a serial worker
// commits the same ops in order in the background. Undo is the diff back.
// ---------------------------------------------------------------------------

/// Re-render the page body from the (just mutated) local model.
fn rerender(app: &mut App, ctx: &Ctx) {
    // The source is about to change shape. Anything already queued against
    // the old text must not come back and be filed under it.
    app.bump_src_epoch();
    let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
    let r = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &app.links);
    app.blocks = r.blocks;
    app.srcs = r.srcs;
    app.hits = r.hits;
    app.laid_width = 0;
    app.start_image_loads(ctx);
    app.start_web_renders(capability::Trigger::Auto);
}

/// Can these ops still be applied to the page as it now stands? Every id
/// they name has to be there — `_end` always is.
fn ops_replayable(ops: &[EditOp], live: &std::collections::HashSet<&str>) -> bool {
    ops.iter().all(|op| match op {
        EditOp::Insert { anchor, .. } => anchor == "_end" || live.contains(anchor.as_str()),
        EditOp::Replace { id, .. } | EditOp::Delete { id } => live.contains(id.as_str()),
    })
}

/// Keep every history entry that can be reached in the order the user will
/// pop it. Validity is sequential: a newer undo can recreate an id required
/// by the older undo below it, so checking every entry against only the
/// initial live page discards perfectly usable lineage.
fn retain_replayable_history(
    stack: &mut Vec<(String, Vec<EditOp>)>,
    live_lines: &[PageLine],
) -> usize {
    let mut simulated = live_lines.to_vec();
    let mut keep = vec![false; stack.len()];
    for index in (0..stack.len()).rev() {
        let live: std::collections::HashSet<&str> =
            simulated.iter().map(|line| line.id.as_str()).collect();
        if ops_replayable(&stack[index].1, &live) {
            apply_ops(&mut simulated, &stack[index].1);
            keep[index] = true;
        }
    }
    let before = stack.len();
    let mut index = 0usize;
    stack.retain(|_| {
        let kept = keep[index];
        index += 1;
        kept
    });
    before - stack.len()
}

fn ensure_editable(app: &mut App) -> bool {
    if app.editable {
        true
    } else {
        app.status = t!("このプロジェクトでは編集権限がありません", "no edit permission in this project");
        false
    }
}

#[derive(Clone, Copy)]
struct MoveShape {
    start: usize,
    len: usize,
}

#[derive(Clone, Copy)]
struct MoveRebase {
    cursor: usize,
    selection: Option<(usize, usize)>,
}

/// Recognize the move lowering shared by outline actions and their history:
/// delete one contiguous target, then recreate the same-size run with fresh IDs.
/// Replace-only indentation history deliberately does not pass this gate.
fn move_shape(app: &App, ops: &[EditOp]) -> Option<MoveShape> {
    let (last, deletes) = ops.split_last()?;
    let EditOp::Insert { lines: inserted, .. } = last else { return None };
    if deletes.is_empty() || deletes.len() != inserted.len() {
        return None;
    }
    let deleted: Vec<&str> = deletes
        .iter()
        .map(|op| match op {
            EditOp::Delete { id } => Some(id.as_str()),
            EditOp::Insert { .. } | EditOp::Replace { .. } => None,
        })
        .collect::<Option<_>>()?;
    let start = app.lines.iter().position(|line| line.id == deleted[0])?;
    let source = app.lines.get(start..start + deleted.len())?;
    if source.iter().map(|line| line.id.as_str()).ne(deleted.iter().copied())
        || inserted.iter().any(|(id, _)| app.lines.iter().any(|line| line.id == *id))
    {
        return None;
    }
    Some(MoveShape { start, len: deleted.len() })
}

/// If these ops are a source move, remember offsets inside the logical
/// target before its old IDs disappear. Duplicate line text is irrelevant:
/// rebasing uses the fresh IDs already carried by the insert op.
fn prepare_move_rebase(app: &App, ops: &[EditOp]) -> Option<MoveRebase> {
    let shape = move_shape(app, ops)?;
    let cursor = app.cursor.checked_sub(shape.start)?.min(shape.len - 1);
    let selection = app.selection.and_then(|selection| {
        let (a, b) = selection.range();
        (a == shape.start && b + 1 == shape.start + shape.len)
            .then(|| (selection.anchor - shape.start, selection.cursor - shape.start))
    });
    Some(MoveRebase { cursor, selection })
}

fn apply_move_rebase(app: &mut App, ops: &[EditOp], rebase: MoveRebase) -> bool {
    let Some(EditOp::Insert { lines: inserted, .. }) = ops.last() else { return false };
    let locate = |offset: usize| app.lines.iter().position(|line| line.id == inserted[offset].0);
    let Some(cursor) = locate(rebase.cursor) else { return false };
    app.cursor = cursor;
    app.selection = rebase.selection.and_then(|(anchor, cursor)| {
        locate(anchor).zip(locate(cursor)).map(|(anchor, cursor)| Selection { anchor, cursor })
    });
    app.follow = true;
    true
}

/// Apply `ops` locally, push their inverse onto the undo stack, and queue
/// the background commit. The single write path for every edit.
/// Returns the commit job the edit was queued under, when it reached the
/// worker at all (an uncreated page commits nothing yet; see below).
fn do_edit(app: &mut App, ctx: &Ctx, label: &str, ops: Vec<EditOp>) -> Option<CommitJobId> {
    if outline_mutation_blocked(app) || !ensure_editable(app) || ops.is_empty() {
        return None;
    }
    let inverse = invert_ops(&app.lines, &ops);
    let move_rebase = prepare_move_rebase(app, &ops);
    apply_ops(&mut app.lines, &ops);
    if let Some(rebase) = move_rebase {
        apply_move_rebase(app, &ops, rebase);
    }
    app.undo_stack.push((label.to_string(), inverse));
    if app.undo_stack.len() > 200 {
        app.undo_stack.remove(0);
    }
    app.redo_stack.clear();
    // A fresh edit starts a fresh lineage: an older drop no longer
    // explains anything.
    app.history_dropped = false;
    let job = if page_is_uncreated(app) {
        // Nothing to commit against yet. The edit lives locally and the
        // whole page goes up as one create instead — sending these ops
        // would need a pageId that does not exist. Once the create is out,
        // later edits wait for the page to come back rather than sending a
        // second create, which would make a second page.
        if app.create_state != CreateState::Sent {
            app.create_state = CreateState::Needed;
        }
        None
    } else {
        queue_commit(app, label, ops)
    };
    rerender(app, ctx);
    job
}


/// Byte offset just past the text an edit changed: everything before the
/// common prefix and after the common suffix is what moved, so the caret
/// belongs at the end of the new middle. Typing lands after what you
/// typed; taking it away lands where it was.
fn changed_end(old: &str, new: &str) -> usize {
    let mut prefix = 0usize;
    for ((i, a), b) in old.char_indices().zip(new.chars()) {
        if a != b {
            break;
        }
        prefix = i + a.len_utf8();
    }
    let mut suffix = 0usize;
    let mut o = old.char_indices().rev();
    let mut n = new.char_indices().rev();
    while let (Some((oi, a)), Some((ni, b))) = (o.next(), n.next()) {
        if a != b || oi < prefix || ni < prefix {
            break;
        }
        suffix += a.len_utf8();
    }
    new.len().saturating_sub(suffix).max(prefix)
}

/// Where an applied edit leaves the caret: on the line it changed, just
/// after the change — which is the whole point of undo/redo. Being
/// returned to where the caret happened to be says nothing about what
/// moved; being taken to the change shows it.
///
/// `before` is the page as it stood before the ops were applied.
fn edit_focus(
    lines: &[PageLine],
    ops: &[EditOp],
    before: &[(String, String)],
) -> Option<(String, usize)> {
    let old_text = |id: &str| -> &str {
        before.iter().find(|(i, _)| i == id).map(|(_, t)| t.as_str()).unwrap_or("")
    };
    // A line that still exists is the better place to stand.
    for op in ops {
        match op {
            EditOp::Insert { lines: ins, .. } => {
                if let Some((id, text)) = ins.last() {
                    return Some((id.clone(), text.len()));
                }
            }
            EditOp::Replace { id, text } => {
                return Some((id.clone(), changed_end(old_text(id), text)));
            }
            EditOp::Delete { .. } => continue,
        }
    }
    // Nothing but deletions: stand where the first deleted line was — the
    // line that slid up into its place, at its start. When nothing slid up
    // (the page ends there), the deletion happened at the END of the line
    // above, so that is where the caret belongs.
    let first = ops.iter().find_map(|op| match op {
        EditOp::Delete { id } => before.iter().position(|(i, _)| i == id),
        _ => None,
    })?;
    match lines.get(first) {
        Some(l) => Some((l.id.clone(), 0)),
        None => lines.last().map(|l| (l.id.clone(), l.text.len())),
    }
}

/// The page id to edit against, or empty when there is nothing to edit
/// against yet.
///
/// Cosense answers 200 for a title that does not exist AND hands out an
/// id with it — a provisional one, for a page it has not made. Committing
/// against that id writes into nothing: the viewer looks like it saved and
/// the web shows an empty page. `persistent` is the only field that says
/// whether the page is real, so it is the one to read.
fn live_page_id(page: &cosense::api::Page) -> String {
    if page.persistent {
        page.id.clone()
    } else {
        String::new()
    }
}

/// How far an uncreated page has got toward existing.
///
/// The distinction that matters is `Sent`: the create carries the whole
/// page, so once it is out, a second one must never follow. The server
/// answers a second create with a SECOND page (same title, auto-suffixed),
/// and the writing splits between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CreateState {
    /// The page exists, or nothing has been typed into it yet.
    Idle,
    /// Local edits are waiting to bring the page into being.
    Needed,
    /// The create is out. Later edits stay local until the page comes back
    /// with an id, and `adopt_created_page` commits them as a diff.
    Sent,
}

/// Does this page exist on the server yet? Cosense answers 200 for any
/// title, so a link to an uncreated page opens as a template with just its
/// title line; it becomes real on the first commit.
fn page_is_uncreated(app: &App) -> bool {
    app.page_id.is_empty()
}

/// Bring an uncreated page into being: one insert of the whole local page,
/// with no `pageId` — the API reads the first inserted line as the title.
/// Sent once, when nothing else is in flight, so a burst of typing cannot
/// race two creates (which the server would answer with two pages, the
/// second auto-suffixed).
fn dispatch_create(app: &mut App) {
    if app.create_state != CreateState::Needed || !page_is_uncreated(app) || app.inflight > 0 {
        return;
    }
    // A title and nothing else is not a page. Opening a name from the
    // picker lands you in EDIT on an empty line, and renaming that title
    // while you think about it is still just thinking: walking away must
    // leave no trace either way. Content is what makes a page.
    if !app.lines.iter().skip(1).any(|l| !l.text.trim().is_empty()) {
        return;
    }
    app.create_state = CreateState::Sent;
    let lines: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    queue_commit(app, &t!("ページの作成", "create page"), vec![EditOp::Insert { anchor: "_end".into(), lines }]);
    app.status = t!("ページを作成しています…", "creating the page…");
}

/// Send one commit job to the serial worker. Returns the id the job was
/// queued under, so a caller that has to recognise its own outcome (an
/// outline action) can wait for that id and nothing else. `None` means the
/// worker is gone and the edit never left the machine.
fn queue_commit(app: &mut App, label: &str, ops: Vec<EditOp>) -> Option<CommitJobId> {
    let id = app.next_job_id;
    app.next_job_id += 1;
    let job = CommitJob {
        id,
        gen: app.gen.load(std::sync::atomic::Ordering::SeqCst),
        project: app.project.clone(),
        page_id: app.page_id.clone(),
        label: label.to_string(),
        ops,
    };
    if app.commit_tx.send(job).is_ok() {
        app.inflight += 1;
        Some(id)
    } else {
        app.status = t!("コミット処理が停止しました — 編集はこの画面にしか残りません", "commit worker gone — edits are LOCAL ONLY");
        // The edit never left the machine: local lines and the server have
        // parted ways, and every diagram on the page must stay as source.
        app.mark_desynced();
        None
    }
}

/// `u`: revert the newest commit (locally at once, on the server via the
/// queue). Ids of replaced lines survive; re-inserted lines get fresh ids.
fn undo(app: &mut App, ctx: &Ctx) -> bool {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return false;
    }
    let Some((_, next_ops)) = app.undo_stack.last() else {
        app.status = empty_history_reason(app, t!("取り消せる編集がありません", "nothing to undo"));
        return false;
    };
    let structural = move_shape(app, next_ops).is_some();
    // Capture the pre-action model before either the page or its history
    // changes. An ordinary commit in flight is no obstacle: the gate below
    // waits for this action's own job id.
    let snapshot = structural.then(|| outline_snapshot(app));

    let (label, ops) = app.undo_stack.pop().expect("history was checked above");
    let redo = invert_ops(&app.lines, &ops);
    let before: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let move_rebase = prepare_move_rebase(app, &ops);
    let replace_selection = app.selection.filter(|_| {
        move_rebase.is_none() && ops.iter().all(|op| matches!(op, EditOp::Replace { .. }))
    });
    apply_ops(&mut app.lines, &ops);
    app.redo_stack.push((label.clone(), redo));
    let focus = edit_focus(&app.lines, &ops, &before);
    let job = queue_commit(app, &format!("undo {label}"), ops.clone());
    if let Some(snapshot) = snapshot {
        match job {
            Some(job) => app.outline_pending = Some(OutlinePending { job, snapshot }),
            None => {
                // The worker never took the structural job — nothing will
                // ever answer for it, so undo the optimistic change now.
                restore_outline_snapshot(app, &snapshot);
                rerender(app, ctx);
                return false;
            }
        }
    }
    rerender(app, ctx);
    let seated = if let Some(selection) = replace_selection {
        // Horizontal outline changes keep every line and every endpoint.
        // The selection's cursor is the active end; focusing the first
        // Replace would make an upward selection visibly jump to its anchor.
        app.selection = Some(selection);
        app.cursor = selection.cursor;
        app.follow = true;
        true
    } else {
        move_rebase
            .map(|rebase| apply_move_rebase(app, &ops, rebase))
            .unwrap_or_else(|| app.focus_edit(focus))
    };
    app.status = t!("{label} を取り消しました（あと {} 件）", "undid {label} ({} more)", app.undo_stack.len());
    seated
}

/// Why is there nothing to undo/redo? "Never had any" and "the server
/// moved and took the lineage with it" are different answers, and silence
/// makes the second look like a broken key.
fn empty_history_reason(app: &App, empty: String) -> String {
    if app.history_dropped {
        // Which of the two stacks is empty does not matter here: the web
        // edit dropped both, and that is the whole answer.
        t!("履歴は web 側の更新で失効しました", "history was dropped by a web edit")
    } else {
        empty
    }
}

/// `^r`: re-apply the newest undone commit.
fn redo(app: &mut App, ctx: &Ctx) -> bool {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return false;
    }
    let Some((_, next_ops)) = app.redo_stack.last() else {
        app.status = empty_history_reason(app, t!("やり直せる編集がありません", "nothing to redo"));
        return false;
    };
    let structural = move_shape(app, next_ops).is_some();
    // As in `undo`: snapshot first, gate on this action's own job id.
    let snapshot = structural.then(|| outline_snapshot(app));

    let (label, ops) = app.redo_stack.pop().expect("history was checked above");
    let undo_ops = invert_ops(&app.lines, &ops);
    let before: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let move_rebase = prepare_move_rebase(app, &ops);
    let replace_selection = app.selection.filter(|_| {
        move_rebase.is_none() && ops.iter().all(|op| matches!(op, EditOp::Replace { .. }))
    });
    apply_ops(&mut app.lines, &ops);
    app.undo_stack.push((label.clone(), undo_ops));
    let focus = edit_focus(&app.lines, &ops, &before);
    let job = queue_commit(app, &format!("redo {label}"), ops.clone());
    if let Some(snapshot) = snapshot {
        match job {
            Some(job) => app.outline_pending = Some(OutlinePending { job, snapshot }),
            None => {
                restore_outline_snapshot(app, &snapshot);
                rerender(app, ctx);
                return false;
            }
        }
    }
    rerender(app, ctx);
    let seated = if let Some(selection) = replace_selection {
        app.selection = Some(selection);
        app.cursor = selection.cursor;
        app.follow = true;
        true
    } else {
        move_rebase
            .map(|rebase| apply_move_rebase(app, &ops, rebase))
            .unwrap_or_else(|| app.focus_edit(focus))
    };
    app.status = t!("{label} をやり直しました", "redid {label}");
    seated
}

// ---------------------------------------------------------------------------
// The modeless edit session (SPEC §1–§3).
// ---------------------------------------------------------------------------

/// Open the session on body line `line` with the caret at byte `caret`.
fn enter_session(app: &mut App, ctx: &Ctx, line: usize, caret: usize) {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.status = t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)");
        return;
    }
    if line >= app.lines.len() {
        app.status = t!("関連ページの行は編集できません（o でページ末尾に行を足せます）", "related rows cannot be edited (o adds a line at the end)");
        return;
    }
    let text = app.lines[line].text.clone();
    let caret = caret.min(text.len());
    // snap to a char boundary
    let caret = (0..=caret).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    app.session = Some(EditSession {
        line,
        input: Input { buf: text.clone(), cur: caret },
        orig: text,
        want_col: None,
        sel_from: None,
    });
    app.cursor = line;
    app.selection = None;
    app.follow = true;
    app.laid_width = 0;
    app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
    app.status = t!("EDIT — ↑↓ 移動 · Enter 改行 · ^z 取り消し · Esc 終了", "EDIT — ↑↓ move · Enter new line · ^z undo · Esc done");
}

/// Commit the caret line's text if it changed (called whenever the caret
/// leaves the line, and on Esc).
fn session_commit_dirty(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    if s.input.buf == s.orig {
        return;
    }
    let (line, buf) = (s.line, s.input.buf.clone());
    let id = app.lines[line].id.clone();
    let label = format!("line {}", line + 1);
    do_edit(app, ctx, &label, vec![EditOp::Replace { id, text: buf.clone() }]);
    if let Some(s) = app.session.as_mut() {
        s.orig = buf;
    }
}

/// Drop the session state and go back to READ + ASCII. Commits nothing —
/// callers decide what to do with the dirty line first.
fn close_session(app: &mut App) {
    app.session = None;
    app.ime_guard = None;
    app.laid_width = 0;
}

/// Leave the session (Esc): commit the dirty line, back to READ + ASCII.
fn leave_session(app: &mut App, ctx: &Ctx) {
    session_commit_dirty(app, ctx);
    close_session(app);
    app.status = t!("✓ 完了", "✓ done");
}

/// `^z` (undo) / `^r` (redo) inside the session — SPEC §6 says the safety
/// net works in EDIT too, not only in READ.
///
/// The dirty line commits first, so undo means "take back what I just
/// typed" instead of skipping over it to the commit before.
///
/// Re-seating afterwards is the whole difficulty: an undo can move the
/// caret line, rewrite it, or take it away entirely. The line **id** is
/// what survives an undo (indices do not), so that is what we follow. When
/// the line itself is undone away — which is exactly what happens when you
/// keep pressing undo past the point where you created it — the caret
/// falls back to the line above, at its end, the way any editor does.
/// Pressing undo N times must never eject you from EDIT.
fn session_history(app: &mut App, ctx: &Ctx, back: bool) {
    session_commit_dirty(app, ctx);
    let Some(s) = app.session.as_ref() else { return };
    let id = app.lines[s.line].id.clone();
    // Where to land if this very line is undone away.
    let fallback = s.line.checked_sub(1).map(|i| app.lines[i].id.clone());
    let col = s.want_col.unwrap_or_else(|| str_width(&s.input.buf[..s.input.cur]));
    let stack_was_empty = if back { app.undo_stack.is_empty() } else { app.redo_stack.is_empty() };
    let seated = if back { undo(app, ctx) } else { redo(app, ctx) };
    if stack_was_empty {
        return; // nothing happened; the status line already says so
    }
    // undo/redo put the caret on what they changed. Only when that line is
    // gone (nothing to stand on) does the session need placing by hand.
    if seated {
        return;
    }
    let (line, col) = match app.lines.iter().position(|l| l.id == id) {
        Some(line) => (line, col),
        None => {
            let line = fallback
                .and_then(|prev| app.lines.iter().position(|l| l.id == prev))
                .unwrap_or_else(|| app.cursor.min(app.lines.len().saturating_sub(1)));
            (line, str_width(&app.lines[line].text)) // caret at end of line
        }
    };
    let text = app.lines[line].text.clone();
    let caret = byte_at_col(&text, col);
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = Some(col);
    }
    app.cursor = line;
    app.follow = true;
    app.laid_width = 0;
}

/// Move the session's caret to `line`, committing the line it leaves —
/// what ↑/↓ do, addressed by line number instead of by direction.
fn session_move_to_line(app: &mut App, ctx: &Ctx, line: usize) {
    let Some(s) = app.session.as_ref() else { return };
    if s.line == line || line >= app.lines.len() {
        return;
    }
    session_commit_dirty(app, ctx);
    let text = app.lines[line].text.clone();
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: text.len() };
        s.orig = text;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = line;
}

/// What a single-line paste should actually insert.
///
/// * a URL pasted over a selection → `[selected text URL]`, the link form
/// * an image URL pasted on its own → `[URL]`, which is what draws it
/// * anything else → exactly what was pasted
fn paste_shape(app: &App, pasted: &str) -> String {
    let url = pasted.trim();
    let is_url = url.starts_with("http://") || url.starts_with("https://");
    if !is_url || url.split_whitespace().count() != 1 {
        return pasted.to_string();
    }
    if let Some(s) = app.session.as_ref() {
        if let Some((a, b)) = s.sel_span() {
            let label = s.input.buf[a..b].trim();
            if !label.is_empty() && !label.contains('[') && !label.contains(']') {
                return format!("[{label} {url}]");
            }
        }
    }
    let draws = cosense::render::gyazo_permalink(url).is_some()
        || cosense::render::looks_like_image_url(url);
    if draws {
        format!("[{url}]")
    } else {
        pasted.to_string()
    }
}

/// `session_cut_span` for a session already borrowed mutably.
fn session_cut_span_in(s: &mut EditSession) {
    if let Some((a, b)) = s.sel_span() {
        s.input.buf.replace_range(a..b, "");
        s.input.cur = a;
        s.sel_from = None;
    }
}

/// Drop the character selection's text from the caret line. Stays local
/// (the line is simply dirty afterwards), like any other typing: the line
/// commits when the caret leaves it.
fn session_cut_span(app: &mut App) -> bool {
    let Some(s) = app.session.as_mut() else { return false };
    if s.sel_span().is_none() {
        return false;
    }
    session_cut_span_in(s);
    s.want_col = None;
    app.laid_width = 0;
    app.follow = true;
    true
}

/// The one answer to "type over a selection": the range disappears and
/// `text` stands in its place, the caret after it. A range inside one
/// line is just the buffer editing it was made of — local, dirty, it
/// commits when the caret leaves. A range ACROSS lines is structural: it
/// is the anchor line's head, `text`, and the far line's tail, with every
/// line wholly between them deleted — one commit, one undo step, exactly
/// as every split and join already is. (Cosense web's textarea does the
/// same; here a page's lines ARE the units, so "merge" is the honest
/// spelling of "replace".) Returns whether a selection was consumed.
fn session_replace_selection(app: &mut App, ctx: &Ctx, text: &str) -> bool {
    let Some(((tl, tb), (bl, bb))) =
        app.session.as_ref().and_then(|s| s.sel_ends())
    else {
        return false;
    };
    if tl == bl {
        let Some(s) = app.session.as_mut() else { return false };
        let Some((a, b)) = s.sel_span() else { return false };
        s.input.buf.replace_range(a..b, text);
        s.input.cur = a + text.len();
        s.sel_from = None;
        s.want_col = None;
        app.laid_width = 0;
        app.follow = true;
        return true;
    }
    // The endpoints' lines: the caret line's working buffer is the truth
    // for its own end (nothing is typed while a selection lives — every
    // key that makes one commits on the way — but the seat itself can
    // carry an uncommitted split). Commit anyway: it is a no-op unless so.
    if bl >= app.lines.len() {
        // Stale far end (a remote edit reshaped the page mid-selection):
        // the range dies here, unseen, and the keystroke goes unfed.
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
        return false;
    }
    session_commit_dirty(app, ctx);
    let (top_text, bot_text) = (
        app.lines[tl].text.clone(),
        app.lines[bl].text.clone(),
    );
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let merged = format!("{prefix}{text}{suffix}");
    let mut ops = vec![EditOp::Replace {
        id: app.lines[tl].id.clone(),
        text: merged.clone(),
    }];
    ops.extend((tl + 1..=bl).map(|i| EditOp::Delete { id: app.lines[i].id.clone() }));
    do_edit(app, ctx, &t!("選択範囲の置換", "replace selection"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = tl;
        s.input = Input { buf: merged.clone(), cur: prefix.len() + text.len() };
        s.orig = merged;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = tl;
    app.follow = true;
    true
}

/// Enter with a selection across lines: the range is replaced by the
/// line break itself — the anchor line keeps its head, the far line its
/// tail, the lines wholly between are deleted, and the caret opens the
/// tail. (A same-line range answers false: cutting it and splitting at
/// the junction is the very same edit, and `session_split` does that on
/// its way through.)
fn session_enter_selection(app: &mut App, ctx: &Ctx) -> bool {
    let Some(((tl, tb), (bl, bb))) =
        app.session.as_ref().and_then(|s| s.sel_ends())
    else {
        return false;
    };
    if tl == bl {
        return false;
    }
    // A remote edit that reshaped the page can leave the far end standing
    // past its last line. The range was about to die at the next draw
    // anyway; it dies here, unseen.
    if bl >= app.lines.len() {
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
        return false;
    }
    session_commit_dirty(app, ctx);
    let (top_text, bot_text) = (
        app.lines[tl].text.clone(),
        app.lines[bl].text.clone(),
    );
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let mut ops = vec![
        EditOp::Replace { id: app.lines[tl].id.clone(), text: prefix },
        EditOp::Replace { id: app.lines[bl].id.clone(), text: suffix.clone() },
    ];
    ops.extend((tl + 1..bl).map(|i| EditOp::Delete { id: app.lines[i].id.clone() }));
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    // The middle lines are gone: the far line now sits one below the
    // anchor, and it is where the caret goes on.
    let seat = tl + 1;
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input { buf: suffix.clone(), cur: 0 };
        s.orig = suffix;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = seat;
    app.follow = true;
    true
}

/// Shift+↑/↓ in READ: grow a line range from wherever the cursor is.
/// Anchors on the first press; after that the ordinary cursor move carries
/// the far end (`after_cursor_move`).
fn read_select_line(app: &mut App, down: bool) {
    if app.selection.is_none() {
        app.selection = Some(Selection::new(app.cursor));
    }
    app.move_cursor(down);
    if let Some((a, b)) = app.selection.map(|s| s.range()) {
        app.status = t!("{} 行を選択 · y コピー · c コメント · Esc 解除", "selected {} line(s) · y copy · c comment · Esc clear", b - a + 1);
    }
}

/// `^t`: step the caret line's heading up a level, and off the top back
/// to plain text.
///
/// Cosense has no official shortcut for this — the community UserScript
/// uses `Ctrl+8` (`*` is Shift+8), which a terminal cannot offer: Ctrl+8
/// arrives as 0x7F, indistinguishable from Backspace. One key that CYCLES
/// gives both directions without a modifier the terminal mangles, and
/// matches how the levels are used in practice: keep pressing until it
/// looks right.
fn session_cycle_heading(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (buf, cur) = (s.input.buf.clone(), s.input.cur);
    let indent = indent_of(&buf).to_string();
    let body = &buf[indent.len()..];
    let (stars, text) = match heading_body(body) {
        Some((n, t)) => (Some(n), t.to_string()),
        None => (None, body.to_string()),
    };
    // 1 → 2 → 3 → 4 → plain → 1 …
    let next = match stars {
        None => Some(1),
        Some(n) if n < 4 => Some(n + 1),
        _ => None,
    };
    let new_body = match next {
        Some(n) => format!("[{} {text}]", "*".repeat(n)),
        None => text.clone(),
    };
    // Keep the caret on the same character of the TEXT, wherever the
    // notation around it moved to.
    let old_text_at = indent.len() + stars.map(|n| n + 2).unwrap_or(0);
    let new_text_at = indent.len() + next.map(|n| n + 2).unwrap_or(0);
    let within = cur.saturating_sub(old_text_at).min(text.len());
    let new_cur = new_text_at + within;
    if let Some(s) = app.session.as_mut() {
        s.input = Input { buf: format!("{indent}{new_body}"), cur: new_cur };
        s.want_col = None;
        s.sel_from = None;
    }
    app.laid_width = 0;
    app.follow = true;
    let _ = ctx;
    app.status = match next {
        Some(n) => t!("見出し レベル{n}（^t でさらに）", "heading level {n} (^t for more)"),
        None => t!("見出しを解除（^t で再び）", "heading cleared (^t to start again)"),
    };
}

/// `[** text]` → `(2, "text")`. Only a WHOLE line counts: a heading is the
/// line, not a run inside it.
fn heading_body(body: &str) -> Option<(usize, &str)> {
    let inner = body.strip_prefix('[')?.strip_suffix(']')?;
    let stars = inner.chars().take_while(|c| *c == '*').count();
    if stars == 0 {
        return None;
    }
    inner[stars..].strip_prefix(' ').map(|t| (stars, t))
}

/// Is the caret line inside a `code:` block?/// Is the caret line inside a `code:` block?
fn session_in_code(app: &App) -> bool {
    app.session
        .as_ref()
        .map(|s| s.line)
        .map(|line| app.code_span_at_line(line).is_some())
        .unwrap_or(false)
}

/// Is the caret sitting inside an empty `[]`?/// Is the caret sitting inside an empty `[]`?
fn empty_pair_at_caret(app: &App) -> bool {
    app.session
        .as_ref()
        .map(|s| {
            s.input.buf[..s.input.cur].ends_with('[') && s.input.buf[s.input.cur..].starts_with(']')
        })
        .unwrap_or(false)
}

/// One printable key in the session, with the bracket habits cosense web
/// has: `[` opens a pair (or wraps the selection), and `]` typed where one
/// is already waiting steps over it instead of doubling.
///
/// Brackets are how everything is written in Cosense — links, images,
/// decoration — so typing the closing half by hand every time is the
/// single most repeated keystroke there is.
fn session_type_char(app: &mut App, ctx: &Ctx, ch: char) {
    // Not inside a `code:` block. There, notation is off by contract —
    // links, images and quotes all stop working — and brackets are just
    // characters someone is typing on purpose (`[0]`, `[\n`). Helping
    // would be the one place the block leaks.
    let in_code = session_in_code(app);
    // A typed character replaces the selection — all of it, wherever the
    // range ends. The bracket habits below only make sense inside one
    // line; a range across lines just takes the character the plain way.
    let cross = app
        .session
        .as_ref()
        .and_then(|s| s.sel_ends())
        .is_some_and(|((a, _), (b, _))| a != b);
    match ch {
        '[' if !in_code && !cross => {
            // With a selection, the brackets go AROUND it: selecting a word
            // and pressing `[` is how a link gets made.
            let selected = app.session.as_ref().and_then(|s| {
                let (a, b) = s.sel_span()?;
                Some(s.input.buf[a..b].to_string())
            });
            session_cut_span(app);
            if let Some(s) = app.session.as_mut() {
                match selected {
                    Some(text) => {
                        let wrapped = format!("[{text}]");
                        s.input.insert_str(&wrapped);
                    }
                    None => {
                        s.input.insert_str("[]");
                        s.input.cur -= 1; // between the pair
                    }
                }
                s.want_col = None;
            }
        }
        ']' if !in_code
            && !cross
            && app
                .session
                .as_ref()
                .map(|s| s.input.buf[s.input.cur..].starts_with(']'))
                .unwrap_or(false)
            && app.session.as_ref().and_then(|s| s.sel_span()).is_none() =>
        {
            if let Some(s) = app.session.as_mut() {
                s.input.right();
                s.want_col = None;
            }
        }
        _ => {
            let mut tmp = [0u8; 4];
            if !session_replace_selection(app, ctx, ch.encode_utf8(&mut tmp)) {
                if let Some(s) = app.session.as_mut() {
                    s.input.insert_char(ch);
                    s.want_col = None;
                }
            }
        }
    }
    app.laid_width = 0;
    app.follow = true;
}

/// Shift+←/→: grow (or start) the character selection as the caret moves.
fn session_select_char(app: &mut App, right: bool) {
    if let Some(s) = app.session.as_mut() {
        if s.sel_from.is_none() {
            s.sel_from = Some((s.line, s.input.cur));
        }
        if right {
            s.input.right();
        } else {
            s.input.left();
        }
        s.want_col = None;
        // A selection collapsed back onto its anchor is no selection.
        if s.sel_from == Some((s.line, s.input.cur)) {
            s.sel_from = None;
        }
    }
    app.selection = None;
    app.laid_width = 0;
    app.follow = true;
}

/// Shift+↑/↓ inside the session: carry the caret one line and grow the
/// CHARACTER range with it — the very selection the mouse drags, ended
/// in (line, byte) positions and covering the lines between. The caret
/// line commits on the way, exactly as a plain ↑/↓ does — selecting must
/// never be the thing that loses a keystroke.
fn session_select_line(app: &mut App, ctx: &Ctx, delta: i32) {
    let Some(s) = app.session.as_ref() else { return };
    let anchor = s.sel_from.unwrap_or((s.line, s.input.cur));
    session_move_line(app, ctx, delta);
    let Some(head) = app.session.as_ref().map(|s| (s.line, s.input.cur)) else {
        return;
    };
    if head == anchor {
        // The caret could not move (the page's edge): the press anchors
        // nothing, and whatever the move said about being at the end
        // stands.
        return;
    }
    if let Some(s) = app.session.as_mut() {
        s.sel_from = Some(anchor);
    }
    app.selection = None;
    let n = anchor.0.abs_diff(head.0) + 1;
    app.status = t!("{} 行を選択 · ⌫ 削除 · Esc 解除", "selected {} line(s) · ⌫ delete · Esc clear", n);
}

/// `^k` inside the session — the emacs contract, one line at a time:
/// kill to the end of the line, and when there is nothing left to kill,
/// kill the line itself. So `^k^k` deletes a line without leaving EDIT,
/// which is what makes deletion an editing gesture rather than a READ-mode
/// keystroke.
///
/// Deleting is structural, so it commits at once (as joins and splits do).
/// The dirty text commits first: undo has to give back the line you were
/// looking at, not the last version the server happened to hold.
fn session_kill(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, at_eol) = (s.line, s.input.cur == s.input.buf.len());
    if !at_eol {
        if let Some(s) = app.session.as_mut() {
            s.input.kill_to_end();
            s.want_col = None;
            s.sel_from = None;
        }
        app.laid_width = 0;
        app.follow = true;
        return;
    }
    // Line 0 is the page title: deleting it renames the page (same reason
    // `x` refuses). Emptying it is still allowed — that is a rename you
    // typed on purpose.
    if line == 0 {
        app.status = t!("タイトル行は削除できません", "the title line cannot be deleted");
        return;
    }
    session_commit_dirty(app, ctx);
    let id = app.lines[line].id.clone();
    do_edit(app, ctx, &t!("行の削除", "delete line"), vec![EditOp::Delete { id }]);
    // The caret takes the place the line left behind: the line that slid
    // up into this index, or the one above when we killed the last line.
    let seat = line.min(app.lines.len().saturating_sub(1));
    let text = app.lines[seat].text.clone();
    let caret = if seat < line { text.len() } else { 0 };
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = None;
    }
    app.cursor = seat;
    app.follow = true;
    app.laid_width = 0;
    app.status = t!("✓ 1行削除 · ^z で戻せます", "✓ deleted line · ^z to undo");
}

/// ↑/↓ inside the session: commit the dirty line, carry the caret to the
/// next/previous BODY line, keeping the display column (sticky).
fn session_move_line(app: &mut App, ctx: &Ctx, delta: i32) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf, cur) = (s.line, s.input.buf.clone(), s.input.cur);
    let code = app.raw_span_at_line(line);
    let width = app.session_wrap_width();
    let disp = session_display(&buf, code);
    let wrapped = SessionWrap::new(&disp, width, session_hang(&buf, code));
    let (row, col_now) = wrapped.row_col(display_caret(&buf, cur, code));
    let want = s.want_col.unwrap_or(col_now);

    // A wrapped line owns several display rows, and ↑/↓ mean "the row
    // above/below" — the same movement the eye makes. Only at the top or
    // bottom row of a line does the caret cross into the next SOURCE line.
    let target_row = row as i32 + delta;
    if target_row >= 0 && (target_row as usize) < wrapped.segs.len() {
        let off = wrapped.offset_at(target_row as usize, want);
        if let Some(s) = app.session.as_mut() {
            s.input.cur = raw_caret_from_display(&buf, off, code);
            s.want_col = Some(want);
            // A plain move collapses a selection to the caret — the
            // range was the shift-key's, and the shift is gone.
            s.sel_from = None;
        }
        app.follow = true;
        app.laid_width = 0;
        return;
    }

    // Crossing into another line: commit what is here first, as before.
    session_commit_dirty(app, ctx);
    let cur_line = line as i32;
    let last = app.lines.len().saturating_sub(1) as i32;
    let target = (cur_line + delta).clamp(0, last);
    if target == cur_line {
        app.status = if delta < 0 { t!("ページの先頭です", "top of page") } else { t!("ページの末尾です — Enter で行を足せます", "end of page — Enter adds a line") };
        return;
    }
    let line = target as usize;
    let text = app.lines[line].text.clone();
    // Land on the row nearest where the caret came from: the FIRST row of
    // the line below, the LAST row of the line above.
    let code = app.raw_span_at_line(line);
    let disp = session_display(&text, code);
    let wrapped = SessionWrap::new(&disp, width, session_hang(&text, code));
    let landing = if delta < 0 { wrapped.segs.len().saturating_sub(1) } else { 0 };
    let caret = raw_caret_from_display(&text, wrapped.offset_at(landing, want), code);
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = Some(want);
        s.sel_from = None;
    }
    app.cursor = line;
    app.follow = true;
    app.laid_width = 0;
}

/// Enter inside the session: split at the caret (at EOL this creates a
/// fresh line). The new line inherits the indent; the caret lands after
/// it; the session CONTINUES there — keep typing.
///
/// Exception — the standard list-escape: Enter on an EMPTY bullet (a
/// whitespace-only line) does not chain another empty bullet. The empty
/// bullet dissolves into a true blank line (its spaces are removed) and
/// the fresh line starts flush, with no indent.
fn session_split(app: &mut App, ctx: &Ctx) {
    // Enter replaces the selection the way every typed character does —
    // with the line break this time. Across lines that is the range
    // collapsing into exactly one break; on one line, cutting the range
    // and splitting at the junction left behind is the identical edit,
    // which the ordinary path below then performs.
    if session_enter_selection(app, ctx) {
        return;
    }
    if let Some(s) = app.session.as_mut() {
        if s.sel_span().is_some() {
            session_cut_span_in(s);
        }
    }
    let Some(s) = app.session.as_ref() else { return };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
    // In a `code:` block the indent is content, not list structure. Enter
    // on the HEADER opens the block's first body line (without this there
    // is no way to type a block at all: the new line would start flush and
    // the renderer would not read it as code). Enter on ONE blank line
    // inside keeps the indent — blank code exists — but a second blank in
    // a row is the writer asking out (the same bargain the empty bullet
    // makes, delayed one line for the blank code's sake).
    let texts = app.source_texts();
    let code = cosense::render::code_span_at(&texts, line);
    let table = cosense::render::table_span_at(&texts, line);
    drop(texts);
    if let Some(span) = code.or(table) {
        if line == span.header {
            session_open_below(app, ctx, line, span.body_indent(), String::new());
            return;
        }
        // What continues onto the new line. In code it is the line's OWN
        // leading whitespace — code sits at depths of its own past the
        // block's base (`  if x:` under a 1-space body), and resetting to
        // the base flattened every deeper line on Enter. A table row has
        // no inner depth; it keeps the base.
        let indent = if code.is_some() {
            indent_of(&buf).to_string()
        } else {
            span.body_indent()
        };
        let empty = buf.chars().all(char::is_whitespace);
        // A table ends the way a list does: Enter on a row with nothing
        // in it leaves. In code one blank line is blank CODE, so leaving
        // takes two: Enter on a blank line directly under another blank
        // dissolves both into true blanks — the first ends the block, the
        // second is where writing continues, flush.
        if empty && code.is_some() {
            let prev_blank = app
                .lines
                .get(line.wrapping_sub(1))
                .filter(|_| line > span.header + 1)
                .map(|l| !l.text.is_empty() && l.text.chars().all(char::is_whitespace))
                .unwrap_or(false);
            if prev_blank {
                let ops = vec![
                    EditOp::Replace { id: app.lines[line - 1].id.clone(), text: String::new() },
                    EditOp::Replace { id: app.lines[line].id.clone(), text: String::new() },
                ];
                do_edit(app, ctx, &t!("コードブロックの終了", "leave code block"), ops);
                if let Some(s) = app.session.as_mut() {
                    s.input = Input { buf: String::new(), cur: 0 };
                    s.orig = String::new();
                    s.want_col = None;
                    s.sel_from = None;
                }
                app.follow = true;
                return;
            }
            let tail = format!("{indent}{}", buf[caret.min(buf.len())..].trim_start());
            session_open_below(app, ctx, line, indent, tail);
            return;
        }
        if !empty {
            let tail = format!("{indent}{}", buf[caret.min(buf.len())..].trim_start());
            if caret >= buf.trim_end().len() {
                session_open_below(app, ctx, line, indent, tail);
                return;
            }
        }
    }
    if !buf.is_empty() && buf.chars().all(char::is_whitespace) {
        let id = app.lines[line].id.clone();
        let anchor = app
            .lines
            .get(line + 1)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "_end".into());
        let ops = vec![
            EditOp::Replace { id, text: String::new() },
            EditOp::Insert { anchor, lines: vec![(new_line_id(), String::new())] },
        ];
        do_edit(app, ctx, &t!("改行", "new line"), ops);
        if let Some(s) = app.session.as_mut() {
            s.line = line + 1;
            s.input = Input { buf: String::new(), cur: 0 };
            s.orig = String::new();
            s.want_col = None;
            s.sel_from = None;
        }
        app.cursor = line + 1;
        app.follow = true;
        return;
    }
    let head = buf[..caret].to_string();
    let indent = indent_of(&buf);
    let indent = if caret < indent.len() { &buf[..caret] } else { indent };
    let tail = format!("{indent}{}", &buf[caret..]);
    let caret_new = indent.len();
    let id = app.lines[line].id.clone();
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let ops = vec![
        EditOp::Replace { id, text: head },
        EditOp::Insert { anchor, lines: vec![(new_line_id(), tail.clone())] },
    ];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input { buf: tail.clone(), cur: caret_new };
        s.orig = tail;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
}

/// Add a line right below `line`, seat the session on it with the caret
/// after `indent`, and leave the current line as it was. Used where Enter
/// must place a line at a specific indent rather than split the text.
fn session_open_below(app: &mut App, ctx: &Ctx, line: usize, indent: String, text: String) {
    let text = if text.is_empty() { indent.clone() } else { text };
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let mut ops = Vec::new();
    // The header's own text may still be uncommitted (you just typed
    // `code:py`); commit it with the same edit that opens the body line.
    if let Some(s) = app.session.as_ref() {
        if s.input.buf != s.orig {
            ops.push(EditOp::Replace { id: app.lines[line].id.clone(), text: s.input.buf.clone() });
        }
    }
    ops.push(EditOp::Insert { anchor, lines: vec![(new_line_id(), text.clone())] });
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input { buf: text.clone(), cur: indent.len().min(text.len()) };
        s.orig = text;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
    app.laid_width = 0;
}

/// Backspace at BOL: join this line into the previous one; the caret
/// lands on the junction. Joining line 1 into line 0 edits the title.
fn session_join_up(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line == 0 {
        app.status = t!("ページの先頭です", "top of page");
        return;
    }
    let prev_text = app.lines[line - 1].text.clone();
    let merged = format!("{prev_text}{buf}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line - 1].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line].id.clone() },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line - 1;
        s.input = Input { buf: merged.clone(), cur: prev_text.len() };
        s.orig = merged;
        s.want_col = None;
    }
    app.cursor = line - 1;
    app.follow = true;
}

/// Delete at EOL: join the NEXT line into this one (forward join).
fn session_join_down(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line + 1 >= app.lines.len() {
        app.status = t!("ページの末尾です", "end of page");
        return;
    }
    let next_text = app.lines[line + 1].text.clone();
    let merged = format!("{buf}{next_text}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line + 1].id.clone() },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.input = Input { buf: merged.clone(), cur: buf.len() };
        s.orig = merged;
        s.want_col = None;
    }
}

/// Tab / Shift+Tab: indent or outdent by one logical level (one source
/// space, rendered as a two-cell nesting step).
fn session_indent(app: &mut App, delta: i32) {
    // Indenting is a whole-line act; a live selection would only end up
    // pointing at bytes that moved, so it is dropped first.
    if let Some(s) = app.session.as_mut() {
        s.sel_from = None;
    }
    // In a table a TAB is what separates one cell from the next, so that
    // is what Tab types there. (Shift+Tab takes the separator back, and
    // once there is none left it outdents — which is how a row leaves the
    // table, the same way a line leaves a code block.)
    let table = app.session.as_ref().and_then(|s| app.table_span_at_line(s.line));
    if let Some(span) = table {
        let in_body = app.session.as_ref().map(|s| s.line > span.header).unwrap_or(false);
        if in_body {
            if let Some(s) = app.session.as_mut() {
                if delta > 0 {
                    s.input.insert_char('\t');
                    s.want_col = None;
                    app.laid_width = 0;
                    return;
                }
                if s.input.buf[..s.input.cur].ends_with('\t') {
                    s.input.cur -= 1;
                    s.input.buf.remove(s.input.cur);
                    s.want_col = None;
                    app.laid_width = 0;
                    return;
                }
            }
        }
    }
    let Some(s) = app.session.as_mut() else { return };
    if delta > 0 {
        s.input.buf.insert(0, ' ');
        s.input.cur += 1;
    } else if let Some(first) = s.input.buf.chars().next() {
        if first.is_whitespace() {
            let w = first.len_utf8();
            s.input.buf.replace_range(..w, "");
            s.input.cur = s.input.cur.saturating_sub(w);
        }
    }
    s.want_col = None;
    app.laid_width = 0;
}

/// `o` / `O` in READ: create a fresh line below/above the cursor (indent
/// inherited) and open the session on it. On a related row (or an empty
/// spot) `o` appends at the page end.
fn open_line(app: &mut App, ctx: &Ctx, above: bool) {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.status = t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)");
        return;
    }
    let cur = app.cursor_src();
    let indent: String = cur
        .and_then(|s| app.lines.get(s))
        .map(|l| indent_of(&l.text).to_string())
        .unwrap_or_default();
    let anchor = match (cur, above) {
        (Some(s), true) => app.lines[s].id.clone(),
        (Some(s), false) => app
            .lines
            .get(s + 1)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "_end".into()),
        (None, _) => "_end".into(),
    };
    let new_id = new_line_id();
    let ops = vec![EditOp::Insert { anchor, lines: vec![(new_id.clone(), indent.clone())] }];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
        enter_session(app, ctx, idx, indent.len());
    }
}

/// One key while the session is open — the modeless core: printable keys
/// type, arrows move the caret, Enter makes lines, Esc leaves.
fn handle_session_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) {
    // A modified arrow is cosense's outline chord. This viewer answers it
    // in READ, not here, and the arms below match arrows regardless of
    // their modifiers — so without this the chord would quietly move the
    // caret instead. Doing nothing and saying where the block keys live is
    // the honest answer; the selection and the caret are left untouched
    // because nothing happened.
    if matches!(
        k.code,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
    ) && matches!(k.modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        app.status = t!(
            "ブロックを動かすには Esc で編集を抜けて m（移動モード）",
            "to move a block: Esc to leave EDIT, then m (move mode)"
        );
        return;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    // Anything that is not extending or acting on the selection drops it:
    // a range that outlives the keystroke it was made for is a trap.
    let keeps_span = matches!(
        (k.code, shift, ctrl),
        (KeyCode::Left, true, _)
            | (KeyCode::Right, true, _)
            | (KeyCode::Up, true, _)
            | (KeyCode::Down, true, _)
            | (KeyCode::Esc, _, _)
            | (KeyCode::Enter, _, _)
            | (KeyCode::Char('y'), _, true)
            | (KeyCode::Backspace, _, _)
            | (KeyCode::Delete, _, _)
    ) || matches!(k.code, KeyCode::Char(_)) && !ctrl;
    if !keeps_span {
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
    }
    if app.selection.is_some()
        && !matches!(
            (k.code, shift),
            (KeyCode::Up, true)
                | (KeyCode::Down, true)
                | (KeyCode::Backspace, _)
                | (KeyCode::Delete, _)
                | (KeyCode::Esc, _)
                | (KeyCode::Char('y'), _)
        )
    {
        app.selection = None;
    }
    // Any horizontal edit resets the sticky ↑/↓ column.
    let reset_col = |app: &mut App| {
        if let Some(s) = app.session.as_mut() {
            s.want_col = None;
            s.sel_from = None;
        }
    };
    let edit_input = |app: &mut App, f: fn(&mut Input)| {
        if let Some(s) = app.session.as_mut() {
            f(&mut s.input);
            s.want_col = None;
            s.sel_from = None;
        }
        app.laid_width = 0;
        app.follow = true;
    };
    match (k.code, ctrl) {
        (KeyCode::Esc, _) => {
            // One Esc at a time: a selection is dropped first, so Esc never
            // both un-selects and leaves.
            if app.selection.is_some() {
                app.selection = None;
                app.status = t!("選択を解除しました", "selection cleared");
            } else if app.session.as_ref().and_then(|s| s.sel_ends()).is_some() {
                if let Some(s) = app.session.as_mut() {
                    s.sel_from = None;
                }
                app.status = t!("選択を解除しました", "selection cleared");
            } else {
                leave_session(app, ctx);
            }
        }
        (KeyCode::Enter, _) => session_split(app, ctx),
        // Shift+↑/↓ grows the CHARACTER range from the caret — the same
        // selection the mouse drags, reachable from the keyboard too.
        (KeyCode::Up, _) if shift => session_select_line(app, ctx, -1),
        (KeyCode::Down, _) if shift => session_select_line(app, ctx, 1),
        (KeyCode::Up, _) => session_move_line(app, ctx, -1),
        (KeyCode::Down, _) => session_move_line(app, ctx, 1),
        (KeyCode::Left, _) if shift => session_select_char(app, false),
        (KeyCode::Right, _) if shift => session_select_char(app, true),
        (KeyCode::Left, _) | (KeyCode::Char('b'), true) => edit_input(app, Input::left),
        (KeyCode::Right, _) | (KeyCode::Char('f'), true) => edit_input(app, Input::right),
        (KeyCode::Home, _) | (KeyCode::Char('a'), true) => edit_input(app, Input::home),
        (KeyCode::End, _) | (KeyCode::Char('e'), true) => edit_input(app, Input::end),
        (KeyCode::Char('w'), true) => edit_input(app, Input::delete_word),
        (KeyCode::Char('u'), true) => edit_input(app, Input::kill_to_start),
        (KeyCode::Char('k'), true) => session_kill(app, ctx),
        (KeyCode::Char('t'), true) => session_cycle_heading(app, ctx),
        // `y` types a letter in a modeless session, and `^c` belongs to the
        // terminal, so copying takes `^y`.
        (KeyCode::Char('y'), true) => {
            let payload = copy_payload(app, false);
            copy_and_report(app, payload);
        }
        // `u` is a printable key in a modeless session, so undo takes the
        // key everyone already presses to undo text: `^z`. Raw mode turns
        // ISIG off, so it never reached the terminal as SIGTSTP anyway,
        // and this viewer has no suspend of its own to lose.
        (KeyCode::Char('z'), true) => session_history(app, ctx, true),
        (KeyCode::Char('r'), true) => session_history(app, ctx, false),
        (KeyCode::Backspace, _) | (KeyCode::Delete, _)
            if app.session.as_ref().and_then(|s| s.sel_ends()).is_some() =>
        {
            session_replace_selection(app, ctx, "");
        }
        (KeyCode::Backspace, _) => {
            let at_bol = app.session.as_ref().map(|s| s.input.cur == 0).unwrap_or(false);
            if at_bol {
                session_join_up(app, ctx);
            } else if empty_pair_at_caret(app) && !session_in_code(app) {
                // Backspacing out of `[|]` takes the bracket that was put
                // there for you, not just the one you typed.
                edit_input(app, Input::delete);
                edit_input(app, Input::backspace);
            } else {
                edit_input(app, Input::backspace);
            }
        }
        (KeyCode::Delete, _) | (KeyCode::Char('d'), true) => {
            let at_eol = app
                .session
                .as_ref()
                .map(|s| s.input.cur == s.input.buf.len())
                .unwrap_or(false);
            if at_eol {
                session_join_down(app, ctx);
            } else {
                edit_input(app, Input::delete);
            }
        }
        (KeyCode::Tab, _) => session_indent(app, 1),
        (KeyCode::BackTab, _) => session_indent(app, -1),
        (KeyCode::Char(ch), false) => session_type_char(app, ctx, ch),
        _ => {
            reset_col(app);
        }
    }
}

/// A multi-line paste while the session is open: the first fragment goes
/// into the caret line; the rest become real lines (one insert op), and
/// the caret lands at the end of the last pasted fragment.
fn session_paste(app: &mut App, ctx: &Ctx, clean: &str) {
    if !clean.contains('\n') {
        // A pasted URL usually wants brackets around it — that is what
        // makes an image a picture, and a selection a labelled link. Doing
        // it by hand after every paste is the kind of chore cosense web
        // already spares you.
        let text = paste_shape(app, clean);
        if !session_replace_selection(app, ctx, &text) {
            if let Some(s) = app.session.as_mut() {
                s.input.insert_str(&text);
                s.want_col = None;
                s.sel_from = None;
            }
        }
        app.laid_width = 0;
        return;
    }
    // Multi-line: a live selection goes first — the paste replaces it,
    // as any typed character's would — then the fragments land plain.
    session_replace_selection(app, ctx, "");
    let Some(s) = app.session.as_ref() else { return };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
    let mut parts = clean.split('\n');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let head = format!("{}{}", &buf[..caret], first);
    let tail_of_line = &buf[caret..];
    let last_part = rest.last().copied().unwrap_or("");
    let mut inserted: Vec<(String, String)> = Vec::new();
    for (i, p) in rest.iter().enumerate() {
        let text = if i + 1 == rest.len() { format!("{p}{tail_of_line}") } else { (*p).to_string() };
        inserted.push((new_line_id(), text));
    }
    let last_id = inserted.last().map(|(id, _)| id.clone());
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let ops = vec![
        EditOp::Replace { id: app.lines[line].id.clone(), text: head },
        EditOp::Insert { anchor, lines: inserted },
    ];
    do_edit(app, ctx, &t!("貼り付け", "paste"), ops);
    if let (Some(s), Some(last_id)) = (app.session.as_mut(), last_id) {
        if let Some(idx) = app.lines.iter().position(|l| l.id == last_id) {
            s.line = idx;
            let text = app.lines[idx].text.clone();
            s.input = Input { buf: text.clone(), cur: last_part.len().min(text.len()) };
            s.orig = text;
            s.want_col = None;
            app.cursor = idx;
            app.follow = true;
        }
    }
}

/// `^e`: suspend the TUI, open the WHOLE page in `$EDITOR`, diff the
/// result into minimal lineId ops (unchanged lines keep their ids), and
/// commit — undo (`u`) is the safety net. The editor is the user's own
/// environment — multi-line editing, their keybindings, their IME
/// settings — which makes this the most natural way to write a lot.
fn editor_roundtrip(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) {
    let original: String =
        app.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    let path = std::env::temp_dir().join(format!(
        "cosense-{}-{}.txt",
        std::process::id(),
        now_secs()
    ));
    if let Err(e) = std::fs::write(&path, format!("{original}\n")) {
        app.status = t!("一時ファイルを作れません: {e}", "temp file failed: {e}");
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());

    // Suspend the TUI for the editor, restore it after — whatever happens.
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} '{}'", path.display()))
        .status();
    *terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    app.laid_width = 0; // re-lay out on the next frame

    let ok = matches!(status, Ok(s) if s.success());
    let edited = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !ok {
        app.status = t!("エディタが中断しました（{editor}）— 何も書き込んでいません", "editor aborted ({editor}) — nothing written");
        return;
    }
    let edited = edited.strip_suffix('\n').unwrap_or(&edited).to_string();
    if edited == original {
        app.status = t!("変更はありません", "no changes");
        return;
    }
    let new_lines: Vec<String> = edited.split('\n').map(str::to_string).collect();
    if new_lines.iter().all(|l| l.trim().is_empty()) {
        app.status = t!("ページが空になるため中止しました（ページの削除はブラウザで）", "page emptied — refusing (delete pages in the browser)");
        return;
    }
    let old: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let ops = diff_to_ops(&old, &new_lines);
    if ops.is_empty() {
        app.status = t!("変更はありません", "no changes");
    } else {
        let n = ops.len();
        do_edit(app, ctx, &t!("エディタ", "editor"), ops);
        app.status = t!("✓ エディタの変更を {n} 件コミットしました · u で戻せます", "✓ editor: {n} op(s) committed · u to undo");
    }
}

/// Reject one optimistic structural action, invalidate anything still
/// queued against it, and replace it with server truth. The pre-action
/// snapshot is restored first, so even a failed reload cannot leave the
/// outline move as a screen-only success.
fn recover_outline_action(app: &mut App, ctx: &Ctx, pending: OutlineSnapshot, reason: String) {
    recover_outline_action_with(app, ctx, pending, reason, reload_page);
}

/// Recovery is parameterized at this narrow fetch seam so the successful
/// reload path can be tested without a live Cosense server.
fn recover_outline_action_with(
    app: &mut App,
    ctx: &Ctx,
    pending: OutlineSnapshot,
    reason: String,
    reload: impl FnOnce(&mut App, &Ctx) -> bool,
) {
    app.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let same_page = app.project == pending.project && app.page_id == pending.page_id;
    if !same_page {
        app.status = t!(
            "前のページのアウトライン操作に失敗しました（{reason}）",
            "outline action on the previous page failed ({reason})"
        );
        return;
    }

    let cursor_id = pending.lines.get(pending.cursor).map(|line| line.id.clone());
    let selection_ids = pending.selection.and_then(|selection| {
        pending
            .lines
            .get(selection.anchor)
            .zip(pending.lines.get(selection.cursor))
            .map(|(anchor, cursor)| (anchor.id.clone(), cursor.id.clone()))
    });

    app.mark_desynced();
    restore_outline_snapshot(app, &pending);
    rerender(app, ctx);

    if reload(app, ctx) {
        // A reload normally clears page-local history in `set_page`. Put the
        // pre-action lineage back only if the response is still for the page
        // that owned the rejected action, and discard entries whose IDs no
        // longer exist in the authoritative response.
        if app.project == pending.project && app.page_id == pending.page_id {
            app.undo_stack = pending.undo_stack;
            app.redo_stack = pending.redo_stack;
            let dropped = retain_replayable_history(&mut app.undo_stack, &app.lines)
                + retain_replayable_history(&mut app.redo_stack, &app.lines);
            app.history_dropped = pending.history_dropped || dropped != 0;

            if let Some(id) = cursor_id {
                if let Some(cursor) = app.lines.iter().position(|line| line.id == id) {
                    app.cursor = cursor;
                }
            }
            app.selection = selection_ids.and_then(|(anchor_id, cursor_id)| {
                app.lines
                    .iter()
                    .position(|line| line.id == anchor_id)
                    .zip(app.lines.iter().position(|line| line.id == cursor_id))
                    .map(|(anchor, cursor)| Selection { anchor, cursor })
            });
            app.follow = true;
        }
        app.status = t!(
            "アウトライン操作に失敗しました（{reason}）— サーバーの内容を読み直しました",
            "outline action failed ({reason}) — reloaded server state"
        );
    } else {
        let reload_reason = app.status.clone();
        app.status = t!(
            "アウトライン操作に失敗しました（{reason}）— 変更を戻しました。{reload_reason}",
            "outline action failed ({reason}) — reverted the change. {reload_reason}"
        );
    }
}

/// A commit came back from the worker (drained per frame).
fn handle_commit_outcome(app: &mut App, ctx: &Ctx, outcome: CommitOutcome) {
    app.inflight = app.inflight.saturating_sub(1);
    // Take the structural gate only for the job it is actually waiting on.
    // An ordinary commit may have been in flight when the action started, or
    // been queued behind it, and either could come back first — arrival
    // order says nothing about ownership, the job id does.
    let outline = match app.outline_pending.as_ref() {
        Some(pending) if pending.job == outcome.job() => {
            app.outline_pending.take().map(|pending| pending.snapshot)
        }
        _ => None,
    };
    match outcome {
        CommitOutcome::Done { job: _, label, title, commit_id } => {
            // Navigation may still happen while the gate is up. In that case
            // this result belongs wholly to the old page: clearing the gate
            // is the only current-app state it may touch.
            if let Some(pending) = outline.as_ref() {
                let same_page = app.project == pending.project && app.page_id == pending.page_id;
                let same_install = same_page && app.gen_now() == pending.install_gen;
                if !same_install {
                    // Navigation is allowed while saving. Even an away/back
                    // trip to the same page installed a snapshot from before
                    // this commit, so the old outcome must not rename or
                    // otherwise mutate that installation. If it is the same
                    // page, reload once to reveal the successful move.
                    if same_page {
                        // The commit succeeded after this installation was
                        // fetched. Until an authoritative reload lands, its
                        // line IDs are stale and neither edits nor web
                        // rendering may trust them.
                        app.bump_server_epoch();
                        app.mark_desynced();
                        app.outline_refresh_needed = true;
                        if reload_page(app, ctx) {
                            if !commit_id.is_empty() {
                                app.own_commits.push_back(commit_id);
                                while app.own_commits.len() > OWN_COMMIT_MEMORY {
                                    app.own_commits.pop_front();
                                }
                            }
                            app.status = format!("✓ {label}");
                        }
                    }
                    return;
                }
            }
            let outline_done = outline.is_some();
            if !commit_id.is_empty() {
                app.own_commits.push_back(commit_id);
                while app.own_commits.len() > OWN_COMMIT_MEMORY {
                    app.own_commits.pop_front();
                }
            }
            // The server now holds something a poll started before this may
            // not know about.
            app.bump_server_epoch();
            // Title-line edits rename the page (auto-suffix included).
            if !title.is_empty() && title != app.title {
                app.title = title;
                if let Ok(mut t) = app.poll_target.lock() {
                    *t = (app.project.clone(), app.title.clone());
                }
            }
            if outline_done
                || app.status.is_empty()
                || app.status.starts_with('✓')
                || app.status.starts_with("EDIT")
            {
                app.status = format!("✓ {label}");
            }
            // A page that just came into being has an id we do not know
            // yet, and every edit until we do has to wait. Waiting for the
            // next poll means up to 3 s (60 s on websocket sync) of held
            // edits that a quit would take with it — so fetch it now.
            if page_is_uncreated(app) && app.create_state == CreateState::Sent {
                adopt_after_create(app, ctx);
            }
        }
        CommitOutcome::Skipped { job: _ } => {
            if let Some(pending) = outline {
                recover_outline_action(app, ctx, pending, t!("処理が失効", "invalidated"));
            }
        }
        CommitOutcome::Failed { job: _, label, msg } => {
            if let Some(pending) = outline {
                recover_outline_action(
                    app,
                    ctx,
                    pending,
                    t!("送信失敗: {label} — {msg}", "commit failed: {label} — {msg}"),
                );
            } else {
                app.status = t!("コミットに失敗しました: {label} — {msg}", "commit failed: {label} — {msg}");
                app.mark_desynced();
                if app.create_state == CreateState::Sent && page_is_uncreated(app) {
                    // The page was never made. Let the next edit try again
                    // rather than retrying in a loop against a dead network.
                    app.create_state = CreateState::Idle;
                }
            }
        }
        CommitOutcome::Conflict { job: _ } => {
            if let Some(pending) = outline {
                recover_outline_action(app, ctx, pending, t!("競合", "conflict"));
            } else {
                recover_conflict(app, ctx);
            }
        }
    }
}

/// Fetch the page we just created and take its id. One blocking request,
/// once per created page: the alternative is holding every later edit
/// until a poll happens to notice, and losing them if the viewer quits
/// first. A failure is not fatal — the poller adopts it later.
fn adopt_after_create(app: &mut App, ctx: &Ctx) {
    if let Ok(page) = ctx.client.get_page_in(&app.project, &app.title) {
        adopt_created_page(app, ctx, &page);
    }
}

/// The page we typed now exists on the server. Take its id and its line
/// ids as the base, then commit whatever was typed after the create left —
/// those keystrokes were never part of it.
///
/// The lines come back with the ids WE generated (the create names its own
/// lines), so the cursor, the session and the telomere all survive this.
fn adopt_created_page(app: &mut App, ctx: &Ctx, page: &cosense::api::Page) {
    if !page.persistent {
        return; // a provisional id is not a page (see `live_page_id`)
    }
    let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
    let session_id =
        app.session.as_ref().and_then(|s| app.lines.get(s.line)).map(|l| l.id.clone());
    let local: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
    let server: Vec<(String, String)> =
        page.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    app.page_id = page.id.clone();
    app.lines = page.lines.clone();
    app.create_state = CreateState::Idle;
    // It exists now. Pages that link here are drawing it as uncreated on
    // the strength of a reading that is one commit out of date.
    app.links.learn(&app.title.clone(), true);
    app.bump_server_epoch();
    app.mark_synced();
    let ops = cosense::editops::diff_to_ops(&server, &local);
    if ops.is_empty() {
        rerender(app, ctx);
        app.status = t!("✓ ページを作成しました", "✓ page created");
    } else {
        // `do_edit` applies locally, stacks the undo and queues the commit
        // — the same path any other edit takes, now that there is a page.
        do_edit(app, ctx, &t!("新規ページの同期", "sync new page"), ops);
        app.status = t!("✓ ページを作成しました", "✓ page created");
    }
    reanchor_cursor_session(app, cursor_id, session_id);
}

/// The shared apply gate: never install remote state while the local
/// model is at stake — viewing history, a commit in flight, a composer
/// open, or a dirty caret line. Polling drops the page when gated (it
/// refetches soon anyway); websocket commits are BUFFERED instead and
/// flushed the moment the gate drops (`ws_flush_pending`).
fn remote_gate_clear(app: &App) -> bool {
    app.time.is_none()
        && app.inflight == 0
        && app.composing.is_none()
        // A grabbed block is uncommitted local work exactly like a dirty
        // caret line: installing someone else's page under it would drag
        // the block against lines the reader never saw.
        && app.move_mode.is_none()
        && !app.session.as_ref().map(|s| s.input.buf != s.orig).unwrap_or(false)
}

/// Replace the local page model with `page`'s lines, keeping the cursor and
/// a clean session on THEIR line ids (a vanished line closes the session),
/// clearing the local undo lineage (its anchors came from the old state),
/// and re-rendering. Polled pages and websocket resyncs both land here.
fn install_remote_lines(app: &mut App, ctx: &Ctx, page: &cosense::api::Page, status: &str) {
    // A full page from the server replaces the local lines wholesale, so
    // whatever a failed commit had left diverging is resolved here. This
    // and `set_page` are the only places the desync flag clears.
    app.bump_server_epoch();
    app.mark_synced();
    let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
    let session_id = app
        .session
        .as_ref()
        .and_then(|s| app.lines.get(s.line))
        .map(|l| l.id.clone());

    app.related = build_related(page, &app.project);
    app.virtual_items = app
        .related
        .iter()
        .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
        .collect();
    app.lines = page.lines.clone();
    // Remote lineage: an entry whose anchor line the server no longer has
    // cannot be replayed, but the rest still can. Dropping the WHOLE
    // history here is what made `^r` look dead: a single web-side edit (or
    // one 3 s poll that differed) silently took the redo stack with it.
    let live: std::collections::HashSet<&str> =
        app.lines.iter().map(|l| l.id.as_str()).collect();
    let before = app.undo_stack.len() + app.redo_stack.len();
    app.undo_stack.retain(|(_, ops)| ops_replayable(ops, &live));
    app.redo_stack.retain(|(_, ops)| ops_replayable(ops, &live));
    if app.undo_stack.len() + app.redo_stack.len() < before {
        app.history_dropped = true;
    }
    rerender(app, ctx);
    reanchor_cursor_session(app, cursor_id, session_id);
    app.follow = true;
    app.status = status.into();
}

/// Re-anchor the cursor and a clean session onto their line ids after the
/// page model changed (full install or remote diff); a vanished session
/// line closes the session.
fn reanchor_cursor_session(app: &mut App, cursor_id: Option<String>, session_id: Option<String>) {
    // Keep the cursor on ITS line (by id), not its number.
    if let Some(id) = cursor_id {
        if let Some(i) = app.lines.iter().position(|l| l.id == id) {
            app.cursor = i;
        }
    }
    // Re-anchor the session the same way; a vanished line closes it.
    if let Some(sid) = session_id {
        match app.lines.iter().position(|l| l.id == sid) {
            Some(i) => {
                let text = app.lines[i].text.clone();
                if let Some(s) = app.session.as_mut() {
                    s.line = i;
                    let cur = s.input.cur.min(text.len());
                    let cur = (0..=cur).rev().find(|&b| text.is_char_boundary(b)).unwrap_or(0);
                    s.input = Input { buf: text.clone(), cur };
                    s.orig = text;
                    s.want_col = None;
                }
                app.cursor = i;
            }
            None => {
                app.session = None;
                app.ime_guard = None;
            }
        }
    }
}

/// Reflect a polled page: WEB EDITS APPEAR ON SCREEN within seconds.
/// Applied only when nothing local is at stake — the page matches, no
/// commit is in flight, no line is dirty, no composer is open — so the
/// local-authoritative model is never overwritten mid-thought. Remote
/// lines keep their per-line `updated`, so the telomere shows the new
/// lines as unread, exactly like a browser revisit.
fn apply_remote(app: &mut App, ctx: &Ctx, polled: PolledPage) {
    if polled.project != app.project || polled.title != app.title {
        return;
    }
    if page_is_uncreated(app) {
        // Until the create lands, every poll is the same empty template.
        // Installing it would wipe the page being typed.
        adopt_created_page(app, ctx, &polled.page);
        return;
    }
    // The snapshot was taken before something newer landed — a commit of
    // ours, a websocket commit, a page install. Installing it now would
    // undo that on screen. It is not an error; the next poll is seconds
    // away and will carry the newer state.
    if polled.epoch != app.server_epoch_now() {
        return;
    }
    if !remote_gate_clear(app) {
        return;
    }
    let same = polled.page.lines.len() == app.lines.len()
        && polled
            .page
            .lines
            .iter()
            .zip(&app.lines)
            .all(|(a, b)| a.id == b.id && a.text == b.text);
    if same {
        // Identical — nothing to install. But this IS a fresh, authoritative
        // full snapshot (the epoch guard above proved it is not stale), and
        // it agrees with the screen line for line. So if a failed commit had
        // left us believing the page had drifted, that belief is now
        // demonstrably wrong: an undo, a retry or a web-side revert brought
        // the two back together. Clearing it here is what lets diagrams
        // render again — without it the flag is sticky for the session.
        //
        // Only the flag is touched: no lines, no cursor, no undo lineage.
        if app.web_unsynced {
            app.mark_synced();
        }
        return;
    }
    install_remote_lines(app, ctx, &polled.page, &t!("⟳ web側の編集を反映", "⟳ applying a web edit"));
}

// -------------------------------------------------------------------------
// Websocket push sync (NOTE-websocket-sync.md)
// -------------------------------------------------------------------------

/// One event from the ws thread → the event loop.
fn handle_ws_event(app: &mut App, ctx: &Ctx, ev: WsEvent) {
    match ev {
        // A state for the room the reader has already left says nothing
        // about the one they are on — and a late `Live` from the old room
        // would hold the NEW page on the 60 s poll.
        WsEvent::State { project, title, state } => {
            if project == app.project && title == app.title {
                app.set_sync_state(state);
            }
        }
        WsEvent::Status(s) => {
            // Never clobber the session hint (EDIT — …): connection notes
            // are transient and can wait.
            if !app.status.starts_with("EDIT") {
                app.status = s;
            }
        }
        WsEvent::Resynced(res) => ws_on_resync(app, ctx, res),
        WsEvent::Commit(c) => ws_on_commit(app, ctx, c),
    }
}

/// The ws thread (re)joined the room, served a resync request, or hit its
/// periodic catch-up: install the fetched page (fills any commits missed
/// while away), resume the commit chain at `head`, and — when gated — hold
/// the LATEST page for the moment the gate drops instead of dropping it.
fn ws_on_resync(app: &mut App, ctx: &Ctx, res: ws::ResyncPage) {
    if res.page.id != app.page_id {
        return; // stale room
    }
    if resync_is_stale(app, &res) {
        return;
    }
    if !remote_gate_clear(app) {
        // Buffered commits so far precede this full page (their effects are
        // inside it); commits received after it are in the channel still.
        app.ws_held_resync = Some((res, app.ws_pending.len()));
        return;
    }
    app.ws_pending.clear();
    app.ws_head = res.head;
    install_remote_lines(app, ctx, &res.page, &t!("⟳ websocket 全同期", "⟳ full websocket resync"));
}

/// A held resync (arrived while a gate was up) applies now that the gate is
/// down. Buffered commits that PRECEDED it are superseded by the page and
/// dropped; commits after it stay buffered and apply against the new state.
fn ws_apply_held_resync(app: &mut App, ctx: &Ctx) {
    let Some((res, pre)) = app.ws_held_resync.take() else { return };
    if !remote_gate_clear(app) {
        app.ws_held_resync = Some((res, pre)); // still gated — keep holding
        return;
    }
    // Waiting for the gate is exactly when the page moves on underneath.
    if resync_is_stale(app, &res) {
        return;
    }
    for _ in 0..pre.min(app.ws_pending.len()) {
        app.ws_pending.pop_front();
    }
    app.ws_head = res.head;
    install_remote_lines(app, ctx, &res.page, &t!("⟳ websocket 全同期", "⟳ full websocket resync"));
}

/// A remote commit arrived — from ANY user, including ourselves. Commit
/// events are applied through the same gate as polling: our own echoes are
/// just idempotent no-ops (the local model already has them), and a commit
/// from the same account edited in the browser applies like any other.
fn ws_on_commit(app: &mut App, ctx: &Ctx, c: RemoteCommit) {
    if c.page_id != app.page_id {
        return; // stale room (navigation is one step ahead of the events)
    }
    if !remote_gate_clear(app) {
        app.ws_pending.push_back(c);
        return;
    }
    ws_flush_pending(app, ctx);
    ws_apply_one(app, ctx, c);
}

/// Apply buffered commits now that the gates are clear, oldest first.
fn ws_flush_pending(app: &mut App, ctx: &Ctx) {
    while !app.ws_pending.is_empty() {
        let next = app.ws_pending.pop_front().expect("non-empty");
        if !ws_apply_one(app, ctx, next) {
            break; // a gate came up mid-flush — the rest stay buffered
        }
    }
}

/// Apply ONE remote commit. Returns false if it had to be re-buffered (a
/// gate is up); true if it was applied, dropped, or queued a resync.
fn ws_apply_one(app: &mut App, ctx: &Ctx, c: RemoteCommit) -> bool {
    if c.page_id != app.page_id {
        return true; // stale room
    }
    if !remote_gate_clear(app) {
        app.ws_pending.push_front(c);
        return false;
    }
    // Our own commit, coming back to us. Its ops are already in the local
    // model — that is where they came from — and re-applying them is not
    // harmless: an echo can arrive AFTER we have edited past it, and a
    // `Replace` then puts the older text back. (Type, press Enter to
    // split, and let the first commit's echo land afterwards: the line
    // grows its old tail back and the caret ends up a line below the
    // text. Reported from the IME, where confirming and splitting happen
    // a keystroke apart.) So it only moves the head along.
    if let Some(i) = app.own_commits.iter().position(|id| *id == c.commit_id) {
        app.own_commits.remove(i);
        app.ws_head = Some(c.commit_id);
        return true;
    }
    if app.ws_head.as_deref() == Some(c.parent_id.as_str()) {
        // Contiguous: this commit extends the state we are known to be at.
        // Apply its ops as a diff (idempotent: our own echo changes
        // nothing) and mark the new head.
        let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
        let session_id = app
            .session
            .as_ref()
            .and_then(|s| app.lines.get(s.line))
            .map(|l| l.id.clone());
        cosense::ws::apply_remote_ops(&mut app.lines, &c.ops, &c.user_id);
        // The screen now holds a state newer than any poll already out.
        app.bump_server_epoch();
        app.ws_head = Some(c.commit_id.clone());
        rerender(app, ctx);
        reanchor_cursor_session(app, cursor_id, session_id);
        app.follow = true;
        app.status = t!("⟳ websocket で更新を反映", "⟳ applying a websocket update");
        return true;
    }
    // Chain broke (reconnect gap, join replay, meta-only commits in
    // between): the event is NOT applied and NOT silently dropped — a
    // background full-page resync is requested, and the fetched page will
    // carry this commit's effect.
    app.ws_resync_pending = true;
    app.status = t!("⟳ websocket 差分に欠落 — 再同期します", "⟳ a websocket diff was missing — resyncing");
    true
}

/// Was this full page fetched before something newer landed here?
///
/// A resync replaces the local lines wholesale, so a page fetched before
/// our last commit would delete the line we are typing on — and deleting
/// the session's line closes the session, dropping the reader out of EDIT
/// mid-word. (Reported as \"pressing Enter twice quickly throws me back to
/// view mode\".) Polls have carried this guard from the start; the
/// websocket's resync had not.
///
/// A stale page is not an error and not a loss: another resync is asked
/// for at once, and the ws thread's own backoff keeps that from becoming
/// a storm while someone is typing.
fn resync_is_stale(app: &mut App, res: &ws::ResyncPage) -> bool {
    if res.epoch == app.server_epoch_now() {
        return false;
    }
    app.ws_resync_pending = true;
    true
}

/// Ship one outstanding resync request to the ws thread (which is the only
/// side that talks to the network). The flag is one-shot per frame; the
/// result arrives back as a `WsEvent::Resynced`.
fn ws_send_resync_request(app: &mut App) {
    if app.ws_resync_pending {
        app.ws_resync_pending = false;
        let _ = app.ws_req_tx.send(ws::WsRequest::Resync);
    }
}

/// 409 NotFastForward: someone else moved the page. Invalidate the queue,
/// reload the server truth, and re-anchor the session by line id — the
/// caret text is NEVER lost: if its line is gone, it becomes a fresh line
/// at the end and the session continues there.
fn recover_conflict(app: &mut App, ctx: &Ctx) {
    app.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // 409 means the server moved and our ops did not land: local and server
    // disagree from this moment. Marked BEFORE the reload, because if the
    // reload fails we are still divergent and must not render — a browser
    // would screenshot the server's text and file it under ours.
    app.mark_desynced();
    let stash = app.session.as_ref().map(|s| {
        (app.lines.get(s.line).map(|l| l.id.clone()).unwrap_or_default(), s.input.buf.clone(), s.input.cur)
    });
    if !reload_page(app, ctx) {
        // `reload_page` already said why on the status line; saying anything
        // else here would bury it. The reader keeps their text, the flag
        // stays set, and the next successful install clears it.
        return;
    }
    // set_page cleared session + undo lineage and marked us synced again.
    match stash {
        None => {
            app.status = t!("他の人がページを更新しました — 読み直しました", "page changed by someone else — reloaded");
        }
        Some((id, buf, caret)) => {
            if let Some(idx) = app.lines.iter().position(|l| l.id == id) {
                enter_session(app, ctx, idx, 0);
                if let Some(s) = app.session.as_mut() {
                    s.input = Input { buf: buf.clone(), cur: caret.min(buf.len()) };
                }
                app.status = t!("他の人がページを更新しました — 読み直し、編集中の行はそのままです", "page changed by someone else — reloaded, your line kept");
            } else if !buf.trim().is_empty() {
                // The line is gone: rescue the text as a fresh last line.
                let new_id = new_line_id();
                let ops = vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![(new_id.clone(), buf.clone())],
                }];
                do_edit(app, ctx, "退避", ops);
                if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
                    enter_session(app, ctx, idx, buf.len());
                }
                app.status = t!("編集中の行が他の人に削除されました — 内容はページ末尾に退避しました", "your line was deleted by someone else — text rescued at the end");
            } else {
                app.status = t!("他の人がページを更新しました — 読み直しました", "page changed by someone else — reloaded");
            }
        }
    }
}

/// ←/→: travel the page's snapshot history (dir = -1 older, +1 newer).
/// First ← enters the machine at the newest snapshot; → past the newest
/// exits back to NOW (live page, refetched). The timeline is fetched once
/// per visit and snapshots are cached, so scrubbing is instant.
fn travel(app: &mut App, ctx: &Ctx, dir: i32) {
    if app.time.is_none() {
        if dir > 0 {
            app.status = t!("すでに最新です", "already at NOW");
            return;
        }
        match ctx.client.list_snapshots(&app.project, &app.page_id) {
            Ok(points) if !points.is_empty() => {
                let last = points.len() - 1;
                app.time = Some(TimeMachine { points, pos: last, cache: HashMap::new() });
                show_snapshot(app, ctx, last);
            }
            Ok(_) => app.status = t!("このページに履歴はありません", "no snapshots for this page"),
            Err(e) => app.status = t!("履歴一覧を取得できません: {e}", "snapshot list failed: {e}"),
        }
        return;
    }
    let (pos, len) = {
        let tm = app.time.as_ref().unwrap();
        (tm.pos, tm.points.len())
    };
    if dir < 0 {
        if pos == 0 {
            app.status = t!("最も古い履歴です", "oldest snapshot");
        } else {
            show_snapshot(app, ctx, pos - 1);
        }
    } else if pos + 1 >= len {
        // Past the newest snapshot is NOW — but only if NOW can be fetched.
        // See the Esc path: a failed reload keeps the snapshot, read-only.
        if reload_page(app, ctx) {
            app.status = t!("最新", "NOW");
        }
    } else {
        show_snapshot(app, ctx, pos + 1);
    }
}

/// Install snapshot `idx` of the time machine: swap the page body for the
/// historical lines (rendered normally — the UI always consumes complete
/// documents, akapen's history model), drop the related list (it describes
/// the PRESENT graph), and keep the cursor's line number.
fn show_snapshot(app: &mut App, ctx: &Ctx, idx: usize) {
    let (ts_id, created, len) = {
        let tm = app.time.as_ref().unwrap();
        (tm.points[idx].id.clone(), tm.points[idx].created, tm.points.len())
    };
    let cached = app.time.as_ref().unwrap().cache.get(&ts_id).cloned();
    let snap = match cached {
        Some(s) => s,
        None => match ctx.client.get_snapshot(&app.project, &app.page_id, &ts_id) {
            Ok(s) => {
                app.time.as_mut().unwrap().cache.insert(ts_id.clone(), s.clone());
                s
            }
            Err(e) => {
                app.status = t!("履歴を取得できません: {e}", "snapshot fetch failed: {e}");
                return;
            }
        },
    };
    let texts: Vec<String> = snap.lines.iter().map(|l| l.text.clone()).collect();
    // A snapshot is the page as it was; which of its links exist is only
    // known for NOW, so an old revision says nothing about it.
    let rendered =
        render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &LinkTruth::default());
    app.lines = snap.lines;
    app.blocks = rendered.blocks;
    app.srcs = rendered.srcs;
    app.hits = rendered.hits;
    app.related = Vec::new();
    app.virtual_items = Vec::new();
    app.images.clear();
    app.image_errors.clear();
    app.pending.clear();
    app.web_pending.clear();
    app.web_errors.clear();
    app.selection = None;
    app.laid_width = 0; // rebuild (clamps the cursor)
    app.follow = true;
    app.start_image_loads(ctx);
    app.time.as_mut().unwrap().pos = idx;
    app.status = t!("⏪ {}/{} · {}（{}前）· ← 古い · → 新しい · Esc 最新", "⏪ {}/{} · {} ({} ago) · ← older · → newer · Esc NOW",
        idx + 1,
        len,
        cosense::theme::format_local(created),
        relative_age(created),
    );
}

/// Refetch the current page in place, keeping the cursor's line number and
/// following it (used after edits, edit conflicts, and ws re-sync).
/// Returns whether the refetch succeeded.
fn reload_page(app: &mut App, ctx: &Ctx) -> bool {
    let cur = app.cursor;
    match load_page(ctx, &app.project.clone(), &app.title.clone()) {
        Ok(l) => {
            app.set_page(l, ctx);
            app.cursor = cur; // clamped on the next rebuild
            app.follow = true;
            true
        }
        Err(e) => {
            app.status = t!("読み直しに失敗しました: {e}", "reload failed: {e}");
            false
        }
    }
}

/// Mouse handling (akapen parity).
///
/// - Wheel: the viewport alone moves, one row per event; the cursor keeps
///   its line (scrolling back finds it where it was). Over an overlay the
///   wheel moves the overlay's cursor instead.
/// - Left click on an underlined link: activate it on button-up. Elsewhere,
///   move the cursor and drop selection. Dragging selects through lines.
/// - Scrollbar: pressing the track jumps the viewport so the thumb starts
///   at that row and grabs it; dragging scrubs (the pointer may leave the
///   column). Viewport-only, like the wheel.
/// - While the composer is open only the wheel works: the commented range
///   is pinned and a click must not rewrite it mid-typing.
fn handle_mouse(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    // Clicking and dragging move the cursor and the selection, which are
    // the move mode's own hands: let go of the block (committing the drag)
    // before they mean something else.
    //
    // Only a BUTTON does that. Mouse capture asks for any-event tracking,
    // so bare pointer motion arrives here too, and letting that go would
    // mean a trackpad brushed in passing commits a half-finished drag and
    // spends a set of line ids on it. The wheel only moves the viewport.
    if app.move_mode.is_some()
        && matches!(
            m.kind,
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_)
        )
    {
        leave_move_mode(app, ctx);
    }
    if app.index.is_some() {
        handle_mouse_index(app, ctx, m);
        return;
    }
    if app.overlay.is_some() {
        match m.kind {
            MouseEventKind::ScrollDown => handle_overlay_key(app, ctx, KeyCode::Down, KeyModifiers::NONE),
            MouseEventKind::ScrollUp => handle_overlay_key(app, ctx, KeyCode::Up, KeyModifiers::NONE),
            _ => {}
        }
        return;
    }
    handle_mouse_content(app, ctx, m);
}

/// Mouse in the index: the wheel moves the list (or scrolls the excerpt
/// when the pointer is over it), and a click on a row OPENS that page.
///
/// One click, not two. The list is a picker — every row is a link, and a
/// row is a big target — so the second click of a select-then-open dance
/// would only be a way of asking "are you sure" about a page that can be
/// left again with `[`.
fn handle_mouse_index(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    use cosense::index::{Pane, Row};
    let list = app.index_list_rect;
    let preview = app.index_preview_rect;
    let inside = |r: Rect| {
        m.column >= r.x
            && m.column < r.x + r.width
            && m.row >= r.y
            && m.row < r.y + r.height
    };
    let over_list = inside(list);
    let over_preview = inside(preview);
    let row_at = |m: &MouseEvent, ix: &cosense::index::Index| -> Option<usize> {
        if m.row < list.y || m.row >= list.y + list.height || !over_list {
            return None;
        }
        let i = ix.scroll + (m.row - list.y) as usize;
        (i < ix.len()).then_some(i)
    };
    match m.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let down = matches!(m.kind, MouseEventKind::ScrollDown);
            let height = list.height as usize;
            let Some(ix) = app.index.as_mut() else { return };
            // The wheel belongs to whatever it is pointing at, whichever
            // pane has the keys. Over the list it moves the WINDOW and
            // leaves the selection alone — the same bargain the page body
            // makes, where the wheel never carries the cursor off its line.
            if over_list {
                if ix.scroll_by(if down { 1 } else { -1 }, height) {
                    app.index_scrolled_at = Some(Instant::now());
                }
            } else if over_preview && down {
                ix.preview_scroll = ix.preview_scroll.saturating_add(1);
            } else if over_preview {
                ix.preview_scroll = ix.preview_scroll.saturating_sub(1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(ix) = app.index.as_mut() else { return };
            if over_preview {
                // Clicking the excerpt is how you get to scroll it with
                // the keys, the way Tab does.
                ix.focus = Pane::Preview;
                return;
            }
            if !over_list {
                return;
            }
            ix.focus = Pane::List;
            let Some(i) = row_at(&m, ix) else { return };
            ix.cursor = i;
            let target = match ix.rows().get(i) {
                Some(Row::Page(e)) => Some((e.title.clone(), false)),
                Some(Row::Create(name)) => Some((name.to_string(), true)),
                None => None,
            };
            open_from_index(app, ctx, target);
        }
        _ => {}
    }
}

/// Caret byte for a click at display column `col` of body line `line`:
/// exact on the session's caret line (it shows raw source), best-effort on
/// rendered lines (raw and rendered columns differ where notation hides).
fn click_caret(app: &App, line: usize, col: usize, screen_row: i32) -> usize {
    let text = app
        .session
        .as_ref()
        .filter(|s| s.line == line)
        .map(|s| s.input.buf.clone())
        .or_else(|| app.lines.get(line).map(|l| l.text.clone()))
        .unwrap_or_default();
    // Map through the bullet display: what the eye clicked is the display
    // column, which the view (bullets at indent) also approximates.
    let code = app.raw_span_at_line(line);
    let disp = session_display(&text, code);
    // A wrapped line owns SEVERAL display rows. The column alone cannot say
    // where in the text the click landed — without the row, every
    // continuation row maps onto the first one, and the caret jumps to the
    // wrong place (and a drag selects the wrong run).
    let width = app.session_wrap_width();
    let wrapped = SessionWrap::new(&disp, width, session_hang(&text, code));
    let seg_index = app
        .src_rows(line)
        .map(|(first, _)| {
            let clicked = (app.scroll as i32 + screen_row).max(0) as usize;
            clicked.saturating_sub(first).min(wrapped.segs.len().saturating_sub(1))
        })
        .unwrap_or(0);
    raw_caret_from_display(&text, wrapped.offset_at(seg_index, col), code)
}

/// One button-press, counted: a press within the double-click window of
/// the previous one at (almost) the same cell continues its gesture —
/// word on the second click, line on the third — and the count restarts
/// after. Records the press in `last_click`, answers with this press's
/// place in the gesture: 1, 2 or 3.
fn register_click(
    last: &mut Option<(Instant, u16, u16, u8)>,
    now: Instant,
    column: u16,
    row: u16,
) -> u8 {
    let count = match *last {
        Some((t, cx, cy, c))
            if now.duration_since(t) < Duration::from_millis(450)
                && cx.abs_diff(column) <= 1
                && cy.abs_diff(row) <= 1 =>
        {
            if c >= 3 {
                1
            } else {
                c + 1
            }
        }
        _ => 1,
    };
    *last = Some((now, column, row, count));
    count
}

/// The byte range of the word `caret` sits in: a run of letters and
/// digits — CJK counts as letters, so a click inside `こんにちは` takes
/// the whole run up to the first space or punctuation mark. A caret
/// exactly at a word's edge still takes that word; a click in the
/// whitespace or punctuation between two words takes nothing (`None`).
fn word_span(buf: &str, caret: usize) -> Option<(usize, usize)> {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '#';
    let caret = floor_boundary(buf, caret);
    let mut start: Option<usize> = None;
    let mut found: Option<(usize, usize)> = None;
    for (i, c) in buf.char_indices() {
        match (start, word(c)) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                if found.is_none() && s <= caret && caret <= i {
                    found = Some((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    if found.is_none() {
        if let Some(s) = start.filter(|s| caret >= *s) {
            found = Some((s, buf.len()));
        }
    }
    found.filter(|(a, b)| a != b)
}

/// Mouse over the page body (no overlay open): wheel, click, drag,
/// scrollbar. Uses the geometry the last frame recorded in
/// `App::text_rect` / `App::bar_rect`.
fn handle_mouse_content(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => {
            app.drag_anchor = None;
            app.pressed_link = None;
            app.wheel_scroll(1, app.view_h);
            return;
        }
        MouseEventKind::ScrollUp => {
            app.drag_anchor = None;
            app.pressed_link = None;
            app.wheel_scroll(-1, app.view_h);
            return;
        }
        _ => {}
    }
    if app.composing.is_some() {
        return;
    }

    let text = app.text_rect;
    let bar = app.bar_rect;
    // `text.y` is the unscrolled content anchor below the top rule. The
    // visible band also includes one row above it: that row is the rule at
    // scroll=0 and becomes content as soon as the rule scrolls away.
    let viewport_top = text.y.saturating_sub(1);
    let viewport_bottom = text.y.saturating_add(text.height);
    let in_rows = m.row >= viewport_top && m.row <= viewport_bottom;
    let in_text_col = m.column >= text.x && m.column < text.x + text.width;
    let screen_row = m.row as i32 - text.y as i32;
    let clamped_row = m.row.clamp(viewport_top, viewport_bottom) as i32 - text.y as i32;
    let in_track = m.row >= bar.y && m.row < bar.y + bar.height;
    let track_row = m.row.saturating_sub(bar.y);
    // The pointer may leave the scrollbar while dragging: clamp it back.
    let clamped_track_row =
        m.row.clamp(bar.y, (bar.y + bar.height).saturating_sub(1)) - bar.y;
    // The scrollable extent includes the external top rule; FrameEnd is
    // already part of the rows. The track deliberately starts below the
    // frame's top rule, so the top-position thumb cannot overwrite it.
    let total = app.total_height().saturating_add(1) as usize;
    let viewport = app.view_h as usize;
    let track_len = bar.height as usize;
    let on_track = m.column == bar.x
        && in_track
        && cosense::theme::scroll_thumb_in_track(
            total,
            viewport,
            track_len,
            app.scroll as usize,
        )
        .is_some();

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if on_track => {
            if let Some(off) = cosense::theme::scroll_offset_at_in_track(
                total,
                viewport,
                track_len,
                track_row as usize,
            ) {
                app.scroll = off as u16;
                app.follow = false;
                app.scrollbar_drag = Some((track_row, off as u16));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            let (start_row, start_off) = app.scrollbar_drag.unwrap();
            if let Some(off) = cosense::theme::scroll_offset_drag_in_track(
                total,
                viewport,
                track_len,
                start_row as usize,
                start_off as usize,
                clamped_track_row as usize,
            ) {
                app.scroll = off as u16;
                app.follow = false;
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            app.scrollbar_drag = None;
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.pressed_link = None;
            // Any column left of the scrollbar counts (gutter included):
            // the row is what selects the line. Only cells inside the text
            // column can activate an underlined link.
            if in_rows && m.column < bar.x {
                if let Some(src) = app.src_at_screen_row(screen_row) {
                    let col = (m.column.saturating_sub(text.x)) as usize;
                    // Session open: a click moves the caret and arms a
                    // selection for a drag to grow; a double-click takes
                    // the word under the pointer, a triple-click the whole
                    // line — cosense web's gestures, in the unit EDIT
                    // selects in (a character range on the caret line).
                    if app.session.is_some() {
                        if src < app.lines.len() {
                            session_commit_dirty(app, ctx);
                            let count = register_click(
                                &mut app.last_click,
                                Instant::now(),
                                m.column,
                                m.row,
                            );
                            let caret = click_caret(app, src, col, screen_row);
                            app.selection = None;
                            let mut anchor_caret = None;
                            if let Some(s) = app.session.as_mut() {
                                if s.line != src {
                                    let t = app.lines[src].text.clone();
                                    s.line = src;
                                    s.orig = t.clone();
                                    s.input = Input { buf: t, cur: 0 };
                                }
                                s.input.cur = caret.min(s.input.buf.len());
                                s.want_col = None;
                                // A plain click only arms the anchor: a
                                // click that does not drag selects nothing
                                // (see the Up arm). The gestures select.
                                s.sel_from = match count {
                                    2 => match word_span(&s.input.buf, s.input.cur) {
                                        Some((a, b)) => {
                                            s.input.cur = b;
                                            Some((src, a))
                                        }
                                        None => None,
                                    },
                                    3 => {
                                        s.input.cur = s.input.buf.len();
                                        Some((src, 0))
                                    }
                                    _ => Some((src, s.input.cur)),
                                };
                                if s.sel_ends().is_none() {
                                    s.sel_from = None;
                                }
                                anchor_caret = s.sel_from.map(|(_, b)| b).or(Some(s.input.cur));
                            }
                            app.drag_anchor = Some((src, anchor_caret));
                            app.cursor = src;
                            app.follow = true;
                            app.laid_width = 0;
                        }
                        return;
                    }
                    // A link activates on button-up over the same target.
                    // Waiting for release preserves click-drag selection.
                    if in_text_col {
                        if let Some((link_src, item)) =
                            app.link_at_screen_position(screen_row, col)
                        {
                            app.selection = None;
                            app.drag_anchor = Some((link_src, None));
                            app.goto_src(link_src);
                            app.pressed_link = Some((link_src, item));
                            app.last_click = None;
                            return;
                        }
                    }
                    // READ: a double-click ENTERS the session at the
                    // clicked character — the view→edit transition — and
                    // nothing more: it just parks the caret where the
                    // pointer was, with no selection. (Taking the word on
                    // the gesture that only meant to open EDIT surprised
                    // the reader; selecting is a gesture for once you are
                    // already IN edit — see the session branch above, where
                    // a double-click takes the word and a triple the line.)
                    // A single click moves the line cursor as before.
                    let count =
                        register_click(&mut app.last_click, Instant::now(), m.column, m.row);
                    if count >= 2 && src < app.lines.len() && app.time.is_none() {
                        let caret = click_caret(app, src, col, screen_row);
                        enter_session(app, ctx, src, caret);
                        return;
                    }
                    app.selection = None;
                    app.drag_anchor = Some((src, None));
                    app.goto_src(src);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.pressed_link = None;
            // Dragging past the edge scrolls, so a selection can reach
            // beyond one screenful.
            if in_rows {
                if m.row <= viewport_top {
                    app.wheel_scroll(-1, app.view_h);
                } else if m.row >= viewport_bottom {
                    app.wheel_scroll(1, app.view_h);
                }
            }
            let Some((anchor, anchor_caret)) = app.drag_anchor else { return };
            let Some(end) = app.src_at_screen_row(clamped_row) else { return };
            if app.session.is_some() {
                let col = m.column.saturating_sub(text.x) as usize;
                if end < app.lines.len() {
                    // Characters, across lines: the caret — the range's
                    // moving end — follows the pointer wherever it
                    // wanders, and the anchor stays exactly where the
                    // button came down. Drifting back onto the anchor
                    // line collapses the range right back into the
                    // character selection it started as: there is no line
                    // mode to get stuck in, because there is no line
                    // mode. Seat the session on the pointer's line FIRST:
                    // the caret below belongs to THAT line's text, and
                    // writing a foreign buffer's offset is the
                    // mid-character panic the draws used to die of. (A
                    // drag types nothing, so the seat's commit is a
                    // no-op.)
                    session_move_to_line(app, ctx, end);
                    let caret = click_caret(app, end, col, clamped_row);
                    if let Some(s) = app.session.as_mut() {
                        s.input.cur = caret.min(s.input.buf.len());
                        s.sel_from = anchor_caret.map(|b| (anchor, b));
                        if s.sel_ends().is_none() {
                            s.sel_from = None;
                        }
                        s.want_col = None;
                    }
                }
                app.selection = None;
                app.laid_width = 0;
                app.follow = true;
                return;
            }
            app.selection = Some(Selection { anchor, cursor: end });
            app.goto_src(end);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some((src, target)) = app.pressed_link.take() {
                let same_target = in_rows
                    && in_text_col
                    && app
                        .link_at_screen_position(
                            screen_row,
                            m.column.saturating_sub(text.x) as usize,
                        )
                        .map(|(_, item)| item == target)
                        .unwrap_or(false);
                app.drag_anchor = None;
                if same_target {
                    app.selection = None;
                    app.goto_src(src);
                    activate_link(app, ctx, target);
                    return;
                }
            }
            app.drag_anchor = None;
            // A plain click arms an anchor but selects nothing.
            if let Some(s) = app.session.as_mut() {
                if s.sel_ends().is_none() {
                    s.sel_from = None;
                }
            }
        }
        _ => {}
    }
}

/// Move the cursor to the next/previous commented line on this page.
fn jump_comment(app: &mut App, forward: bool) {
    let cur_src = app.cursor_src().unwrap_or(0);
    let mut targets: Vec<usize> = app
        .comments
        .iter()
        .filter(|c| c.project == app.project && c.title == app.title)
        .map(|c| c.start)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let target = if forward {
        targets.into_iter().find(|&s| s > cur_src)
    } else {
        targets.into_iter().filter(|&s| s < cur_src).next_back()
    };
    match target {
        Some(src) => {
            app.goto_src(src);
            app.status = t!("{} 行目のコメント", "comment at line {}", src + 1);
        }
        None => app.status = t!("これ以上コメントはありません", "no more comments"),
    }
}

/// Key handling while an overlay is open.
///
/// The Pages picker is a filter box: printable keys type into the filter, so
/// movement there uses arrows / ^n / ^p. The other overlays are plain lists
/// and keep j/k.
fn handle_overlay_key(app: &mut App, ctx: &Ctx, code: KeyCode, mods: KeyModifiers) {
    enum Act {
        None,
        Close,
        Up,
        Down,
        Activate,
    }
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let is_line_info = matches!(app.overlay, Some(Overlay::LineInfo));
    let act = match code {
        KeyCode::Esc => Act::Close,
        // `t` toggles the line-detail overlay back off.
        KeyCode::Char('t') if is_line_info => Act::Close,
        KeyCode::Enter => Act::Activate,
        KeyCode::Down => Act::Down,
        KeyCode::Up => Act::Up,
        KeyCode::Char('n') if ctrl => Act::Down,
        KeyCode::Char('p') if ctrl => Act::Up,
        // The comments list is where copying every comment belongs.
        KeyCode::Char('y') if matches!(app.overlay, Some(Overlay::Comments { .. })) => {
            let text = format_all(&app.comments);
            app.status = if app.comments.is_empty() {
                t!("コピーするコメントがありません", "no comments to copy")
            } else if copy_to_clipboard(&text) {
                t!("✓ コメント {} 件をコピーしました", "✓ copied {} comment(s)", app.comments.len())
            } else {
                t!("コピーできません — クリップボードのコマンドが無く、端末も OSC 52 を拒否しました", "copy failed — no clipboard tool and the terminal refused OSC 52")
            };
            Act::None
        }
        KeyCode::Char('q') => Act::Close,
        KeyCode::Char('j') => Act::Down,
        KeyCode::Char('k') => Act::Up,
        _ => Act::None,
    };

    let len = match &app.overlay {
        Some(Overlay::Comments { .. }) => app.comments.len(),
        Some(Overlay::Links { items, .. }) => items.len(),
        _ => 0,
    };

    match act {
        Act::Close => app.overlay = None,
        Act::Down => {
            if let Some(
                Overlay::Comments { cursor } | Overlay::Links { cursor, .. },
            ) = app.overlay.as_mut()
            {
                if len > 0 {
                    *cursor = (*cursor + 1).min(len - 1);
                }
            }
        }
        Act::Up => {
            if let Some(
                Overlay::Comments { cursor } | Overlay::Links { cursor, .. },
            ) = app.overlay.as_mut()
            {
                *cursor = cursor.saturating_sub(1);
            }
        }
        Act::Activate => {
            // The link picker mixes pages and files: hand the item over.
            if let Some(Overlay::Links { items, cursor }) = &app.overlay {
                let item = items.get(*cursor).cloned();
                app.overlay = None;
                if let Some(item) = item {
                    activate_link(app, ctx, item);
                }
                return;
            }
            let create = false;
            let target: Option<(String, String, Option<usize>)> = match &app.overlay {
                Some(Overlay::Comments { cursor }) => app
                    .comments
                    .get(*cursor)
                    .map(|c| (c.project.clone(), c.title.clone(), Some(c.start))),
                _ => None,
            };
            app.overlay = None;
            if let Some((project, title, line)) = target {
                if project != app.project || title != app.title {
                    navigate_to(app, ctx, &project, &title);
                }
                if create {
                    // Land in EDIT on a fresh body line. Nothing is sent
                    // yet: an empty page is not created until it has
                    // something in it (see `dispatch_create`).
                    app.cursor = 0;
                    open_line(app, ctx, false);
                }
                if let Some(src) = line {
                    // The page may have just been loaded and not laid out
                    // yet; `rebuild` clamps the cursor onto a rendered line.
                    app.cursor = src;
                    app.follow = true;
                }
            }
        }
        Act::None => {}
    }
}

/// Draw the project index: a full-width list with a shallow excerpt from
/// the selected page docked below.
///
/// The excerpt is the first lines the list API already returned. That is
/// deliberate: it costs no request, so moving through a thousand pages
/// never waits for the network. Its small height makes that limited content
/// read as an intentional peek rather than an incomplete page.
fn draw_index(f: &mut Frame, app: &mut App, ctx: &Ctx, area: Rect) {
    use cosense::index::{Pane, Row};
    let Some(ix) = app.index.as_mut() else { return };
    let body_h = area.height.saturating_sub(2); // header + footer
    let layout = cosense::index::layout(ctx.preview, area.width, body_h);
    if layout.preview.is_none() {
        // A resize may remove the excerpt while it owns focus. Hand the
        // keys back to the visible list rather than leaving them attached
        // to a region that no longer exists.
        ix.focus = Pane::List;
    }
    let rows_h = layout.list as usize;
    let scroll = ix.follow(rows_h);
    let rows = ix.rows();
    let focus = ix.focus;

    // ---- header ------------------------------------------------------
    let shown = rows.len();
    let head = if ix.filter.is_empty() {
        let more = if ix.total > ix.entries.len() {
            format!(" of {}", ix.total)
        } else {
            String::new()
        };
        format!(" {} — {} pages{}", app.project, ix.entries.len(), more)
    } else {
        format!(" {} — {}_ ({} match)", app.project, ix.filter, shown)
    };
    f.render_widget(
        Paragraph::new(head).style(
            Style::default().fg(app.header_colors.fg).bg(app.header_colors.bg),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // ---- list --------------------------------------------------------
    //
    // Full width: the short description supplied by the pages API is an
    // excerpt below, not a half-empty reading pane beside the list. A caret
    // column sits at the left edge; the transient scrollbar uses the far
    // right edge, where it cannot look like a divider between content.
    let caret_x = area.x;
    let text_x = area.x + 2;
    let text_w = area.width.saturating_sub(3);
    let list_area = Rect::new(text_x, area.y + 1, text_w, layout.list);
    // The whole row is the click target, not just its text: hitting the
    // telomere or age means that row.
    app.index_list_rect = Rect::new(area.x, area.y + 1, area.width, layout.list);
    let dim_when_away = |st: Style| if focus == Pane::List { st } else { st.fg(CHROME_DIM) };
    let app_light = app.light;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut caret_rows: Vec<u16> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(rows_h) {
        let selected = i == ix.cursor;
        let mut style = Style::default();
        if selected {
            style = style.bg(CURSOR_BG);
            caret_rows.push(area.y + 1 + (i - scroll) as u16);
        }
        let line = match row {
            Row::Page(e) => {
                // The same mark the page's own gutter wears: THICKNESS is
                // how recently it changed, COLOUR is whether it has been
                // seen. A project's list then reads the way its lines do.
                let (glyph, tel) =
                    cosense::theme::telomere(now_secs() - e.updated, e.unread, app_light);
                let title_style = if e.unread {
                    dim_when_away(style.fg(CHROME_CARET))
                } else {
                    style
                };
                Line::from(vec![
                    Span::styled(glyph.to_string(), style.fg(tel)),
                    Span::styled(format!("{:>4} ", relative_age(e.updated)), style.fg(CHROME_DIM)),
                    Span::styled(
                        truncate_width(&e.title, text_w.saturating_sub(6) as usize),
                        title_style,
                    ),
                ])
            }
            Row::Create(name) => Line::from(Span::styled(
                format!("  ＋ 「{name}」を作成"),
                dim_when_away(style.fg(CHROME_ACTIVE)),
            )),
        };
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), list_area);

    // The scrollbar, for as long as the scroll is still in the reader's
    // hand: thumb only, in the column just inside the list's right edge.
    let bar = app
        .index_scrolled_at
        .filter(|t| t.elapsed() < INDEX_BAR_LINGER)
        .and_then(|_| {
            cosense::theme::scroll_thumb_in_track(rows.len(), rows_h, rows_h, scroll)
        });

    // Everything the rest of the frame needs from the borrowed list, so
    // the preview can read the app again.
    let row_count = rows.len();
    let cursor = ix.cursor;
    drop(rows);

    // The cursor rides the left edge, as it does on the page.
    let buf = f.buffer_mut();
    if let Some((start, len)) = bar {
        let bar_x = area.x + area.width.saturating_sub(1);
        for k in start..start + len {
            if let Some(c) = buf.cell_mut((bar_x, area.y + 1 + k as u16)) {
                c.set_symbol("▐");
                c.set_style(Style::default().fg(CHROME_SCROLL));
            }
        }
    }
    for y in caret_rows {
        if let Some(c) = buf.cell_mut((caret_x, y)) {
            c.set_symbol(">");
            c.set_style(Style::default().fg(if focus == Pane::List {
                CHROME_CARET
            } else {
                CHROME_DIM
            }));
        }
    }

    // ---- excerpt -----------------------------------------------------
    app.index_preview_rect = Rect::default();
    if let Some(height) = layout.preview {
        // One blank row after the list, then a shallow, full-width peek.
        // Nothing frames it: its fixed height is what says "excerpt".
        let prev_area = Rect::new(
            text_x,
            area.y + 1 + layout.list + layout.gap,
            text_w,
            height,
        );
        app.index_preview_rect = Rect::new(
            area.x,
            prev_area.y,
            area.width,
            height,
        );
        let lines = index_preview_lines(app, ctx, text_w as usize);
        let ix = app.index.as_ref().expect("open");
        let skip = ix.preview_scroll as usize;
        let shown: Vec<Line<'static>> =
            lines.into_iter().skip(skip).take(height as usize).collect();
        f.render_widget(Paragraph::new(shown), prev_area);
    }

    // ---- footer ------------------------------------------------------
    let ix = app.index.as_ref().expect("open");
    let hint = match (layout.preview.is_some(), ix.focus) {
        (true, Pane::List) => ts!(
            "j/k · 文字で絞り込み · Enter 開く · Tab 抜粋 · Esc/[ 戻る · ] 進む",
            "j/k · type to filter · Enter open · Tab excerpt · Esc/[ back · ] forward"
        ),
        (true, Pane::Preview) => ts!(
            "j/k 抜粋をスクロール · Tab 一覧 · Enter 開く · Esc/[ 戻る · ] 進む",
            "j/k scroll excerpt · Tab list · Enter open · Esc/[ back · ] forward"
        ),
        (false, _) => ts!(
            "j/k · 文字で絞り込み · Enter 開く · Esc/[ 戻る · ] 進む",
            "j/k · type to filter · Enter open · Esc/[ back · ] forward"
        ),
    };
    // Where in the list the reader is — the footer's job here as on the
    // page (`L12/205`), which is why the list needs no scrollbar.
    let pos = if row_count == 0 {
        "0/0".to_string()
    } else {
        format!("{}/{}", cursor + 1, row_count)
    };
    f.render_widget(
        Paragraph::new(format!(" {} {pos} · {hint}", ts!("一覧", "index"))).style(Style::default().fg(CHROME_DIM)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

/// The excerpt dock for the page under the cursor: one compact heading and
/// the first lines supplied by the pages API, rendered as the page would.
fn index_preview_lines(app: &App, ctx: &Ctx, width: usize) -> Vec<Line<'static>> {
    let Some(ix) = app.index.as_ref() else { return Vec::new() };
    let Some(entry) = ix.selected() else {
        return vec![Line::from(Span::styled(
            t!("（まだ無いページ — Enter で書きはじめる）", "(an uncreated page — Enter starts writing it)"),
            Style::default().fg(CHROME_DIM),
        ))];
    };
    // Compact on purpose: title and metadata share one row, leaving almost
    // all of this shallow region to the excerpt itself. A rule or box would
    // make it look like a second full view and amplify the empty space.
    let dim = Style::default().fg(CHROME_DIM);
    let meta = format!(
        " · {} ago{}",
        relative_age(entry.updated),
        if entry.unread { " · 未読" } else { "" }
    );
    let show_meta = width > str_width(&meta) + 8;
    let title_width = if show_meta { width - str_width(&meta) } else { width };
    let title = truncate_width(&entry.title, title_width);
    let mut heading = vec![Span::styled(
        title,
        Style::default().fg(ctx.palette.title).add_modifier(Modifier::BOLD),
    )];
    if show_meta {
        heading.push(Span::styled(meta, dim));
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(heading)];
    // The renderer reads line 0 as the page's title (it wears the title
    // style and carries no notation), so the title has to be there — and
    // its block dropped, since the heading above already says it.
    let mut texts: Vec<String> = vec![entry.title.clone()];
    texts.extend(entry.descriptions.iter().cloned());
    let out = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &LinkTruth::default());
    for block in out.blocks.iter().skip(1) {
        match block {
            Block::Text(line) => {
                for w in wrap_line_parts(line, width.saturating_sub(1), &hanging_prefix(line)) {
                    lines.push(w.line);
                }
            }
            Block::Blank => lines.push(Line::from("")),
            // A picture, a table or a diagram in the first lines: the
            // preview says it is there rather than drawing it, which would
            // cost a download per cursor move.
            _ => lines.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(CHROME_DIM),
            ))),
        }
    }
    lines
}

fn page_frame_visible(editing: bool) -> bool {
    !editing
}

/// Page chrome uses an ANSI role color; the terminal theme supplies its RGB.
fn page_frame_style() -> Style {
    Style::default().fg(CHROME_DIM)
}

fn ui(f: &mut Frame, app: &mut App, ctx: &Ctx) {
    let area = f.area();
    f.render_widget(Clear, area);

    // The index owns the whole screen while it is open: it is a place to
    // be, not something laid over the page (the page is still loaded and
    // Esc goes back to it).
    if app.index.is_some() {
        draw_index(f, app, ctx, area);
        return;
    }

    // header
    let sel_info = app
        .selection
        .map(|s| {
            let (a, b) = s.range();
            t!("  [選択 {}-{}]", "  [sel {}-{}]", a + 1, b + 1)
        })
        .unwrap_or_default();
    // Unread badge: first visit anywhere, or how many lines changed since.
    let unread = match (app.read_at, app.unread_count()) {
        (None, _) => t!("  · 初回", "  · first visit"),
        (Some(_), 0) => String::new(),
        (Some(_), n) => t!("  · 未読 {n}", "  · {n} new"),
    };
    // `project/title`, the same shape as the page's URL — so a cross-project
    // hop changes both the label and the Cosense-site-derived header color.
    let time_badge = app
        .time
        .as_ref()
        .map(|tm| {
            let p = &tm.points[tm.pos];
            format!(
                "  ⏪ {}/{} · {}",
                tm.pos + 1,
                tm.points.len(),
                cosense::theme::format_local(p.created)
            )
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(Line::from(t!(
            " {}/{}{}{}{}  （コメント {}）{}{} ",
            " {}/{}{}{}{}  ({} comment(s)){}{} ",
            app.project,
            app.title,
            time_badge,
            if app.mode == Mode::Source { ts!("  [ソース]", "  [source]") } else { "" },
            if app.editable { "" } else { ts!("  [読み取り専用]", "  [read-only]") },
            app.comments.len(),
            unread,
            sel_info
        )))
        .style(
            Style::default()
                .fg(app.header_colors.fg)
                .bg(app.header_colors.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // help / status: a leading position readout (akapen's `VIEW L23/118`),
    // then the contextual hint — numbered links when the cursor line has
    // them, else the static key list, else a transient status message.
    let mode_tag = if app.session.is_some() {
        "EDIT"
    } else if app.mode == Mode::Source {
        "SRC"
    } else {
        "VIEW"
    };
    let pos = if app.lines.is_empty() {
        "L-/-".to_string()
    } else if app.cursor >= app.lines.len() && !app.virtual_items.is_empty() {
        // Cursor is on a related row below the body.
        format!("link {}/{}", app.cursor - app.lines.len() + 1, app.virtual_items.len())
    } else {
        let cur = app.cursor_src().map(|s| s + 1).unwrap_or(1).min(app.lines.len());
        format!("L{}/{}", cur, app.lines.len())
    };
    let cur_links = if app.session.is_some() { Vec::new() } else { app.cursor_line_links() };
    let hint = app.hint_text(&cur_links);
    f.render_widget(
        Paragraph::new(format!(" {} {} · {}", mode_tag, pos, hint))
            .style(Style::default().fg(CHROME_DIM)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    // The page is boxed flush with the terminal edge. The cursor `>` rides
    // ON the left frame column, while the telomere keeps its own inside
    // column and therefore remains visible on the cursor line. One blank
    // column separates that telomere from the text. The right side keeps a
    // blank, the scrollbar thumb, and the frame column.
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(2),
    );
    // Layout: frame/caret, telomere, blank, text, blank, thumb, frame.
    let caret_x = body.x;
    let gutter_x = body.x + 1;
    let bar_x = body.x + body.width.saturating_sub(2);
    let text = Rect::new(
        body.x + 3,
        body.y + 1,
        body.width.saturating_sub(5),
        body.height.saturating_sub(2),
    );
    // While the top rule is visible, the scrollbar starts immediately
    // below it. After scrolling the rule away, the track reclaims that row
    // just like text, images, and telomeres do.
    let (bar_y, bar_h) = if app.scroll == 0 {
        (body.y.saturating_add(1), body.height.saturating_sub(1))
    } else {
        (body.y, body.height)
    };
    let bar = Rect::new(bar_x, bar_y, 1, bar_h);

    // The page frame hugs PAGE CONTENT ONLY. Its explicit FrameEnd closes
    // the box; related sections continue beneath it without vertical sides.
    // Everything scrolls in one coordinate system, so j/k crosses naturally.
    let top_rule = text.y as i32 - 1 - app.scroll as i32;
    let bot_rule = text.y as i32 + app.frame_end_top() as i32 - app.scroll as i32;
    let band_top = body.y as i32;
    let band_bot = (area.y + area.height - 2) as i32; // one above status
    let band_h = (band_bot - band_top + 1).max(1) as u16; // visible rows
    if page_frame_visible(app.session.is_some()) {
        let buf = f.buffer_mut();
        // READ has a terminal-colored page boundary. EDIT removes that
        // boundary entirely; the absence is the mode signal and does not
        // depend on a particular terminal palette.
        let frame_style = page_frame_style();
        let right_x = body.x + body.width.saturating_sub(1);
        let set = |buf: &mut ratatui::buffer::Buffer, x: u16, y: i32, s: &str| {
            if y < band_top || y > band_bot {
                return;
            }
            if let Some(c) = buf.cell_mut((x, y as u16)) {
                c.set_symbol(s);
                c.set_style(frame_style);
            }
        };
        // top rule ┌─…─┐
        if top_rule >= band_top && top_rule <= band_bot {
            set(buf, body.x, top_rule, "┌");
            for x in (body.x + 1)..right_x {
                set(buf, x, top_rule, "─");
            }
            set(buf, right_x, top_rule, "┐");
        }
        // bottom rule └─…─┘
        if bot_rule >= band_top && bot_rule <= band_bot {
            set(buf, body.x, bot_rule, "└");
            for x in (body.x + 1)..right_x {
                set(buf, x, bot_rule, "─");
            }
            set(buf, right_x, bot_rule, "┘");
        }
        // vertical sides: strictly between the content's rule rows, so the
        // sides scroll off with the page (ratatui clips rows off-screen;
        // we only limit writes to the band so we never paint over the
        // header/status strips).
        let v_top = (top_rule + 1).max(band_top);
        let v_bot = (bot_rule - 1).min(band_bot);
        for y in v_top..=v_bot {
            set(buf, body.x, y, "│");
            set(buf, right_x, y, "│");
        }
    }
    app.text_rect = text;
    app.bar_rect = bar;

    if app.laid_width != body.width {
        // Width changed: re-wrap, keeping the cursor line on its screen row
        // (the resize equivalent of akapen's cursor_fraction handoff — the
        // cursor is a source line, so only the viewport needs adjusting).
        app.relayout_preserving_screen_row(body.width, band_h);
    }
    if app.view_h != band_h {
        // Height changed: the old offset may now overshoot the end, and the
        // cursor may have fallen off the bottom.
        app.view_h = band_h;
        app.scroll = app.scroll.min(app.max_scroll(band_h));
        app.follow = true;
    }
    if app.follow {
        app.follow_cursor(band_h);
        app.follow = false;
    }

    let view_top = app.scroll as i32;
    // Row coordinates exclude the top rule, while drawing uses
    // `text.y + row - scroll`. Therefore one row above `view_top` remains
    // visible at the band's top after scrolling; omitting it left screen row
    // 2 permanently blank.
    let visible_rows_top = view_top - 1;
    let visible_rows_bottom = view_top + band_h as i32 - 1;
    // The grabbed block wears the SAME highlight a selection does — it is
    // the thing being carried around, which is what a selection means
    // here. The footer says the mode and its keys in words, so this colour
    // is never the only thing telling the reader what is going on.
    let sel_range = move_block_range(app)
        .or_else(|| app.selection.map(|s| s.range()))
        // EDIT has no line-range, but a character range across lines
        // still BANDS the lines it covers: the caret line carries its
        // exact reversed characters (`caret_sel_bytes`), the others read
        // as whole lines — the honest shape of a selection whose ends
        // are not on them.
        .or_else(|| {
            app.session
                .as_ref()
                .and_then(|s| s.sel_ends())
                .map(|((a, _), (b, _))| (a, b))
        });
    // Telomeres and frame-column carets are painted after the rows.
    let mut gutter: Vec<(u16, &'static str, Style)> = Vec::new();
    let mut carets: Vec<(u16, Style)> = Vec::new();
    // Rows inside a `code:` block. Painted LAST, as a background-only pass:
    // the band has to run to the frame, and the telomere, the thumb and the
    // padding columns are all drawn after the rows.
    let mut wash_rows: Vec<u16> = Vec::new();
    // Bullets for image rows. An indented picture is a LIST ITEM whose
    // content is the picture (cosense web draws the bullet there too), and
    // the image protocol paints its own area, so the marker is written
    // beside it in the same pass as the telomeres.
    let mut image_bullets: Vec<(u16, u16)> = Vec::new();

    // Which rows are code: a `code:` block reads as one surface, so its
    // rows carry a wash. Computed once per frame from the text the screen
    // is showing (the caret line included, uncommitted and all).
    let code_flags = cosense::render::code_line_flags(&app.source_texts());
    let wash = cosense::theme::code_wash(ctx.terminal_bg);

    let mut y = 0i32;
    for row in app.rows.iter() {
        let h = row.height() as i32;
        let top = y;
        let bottom = y + h;
        y = bottom;
        if bottom <= visible_rows_top || top >= visible_rows_bottom {
            continue;
        }
        let screen_y = top - view_top;

        // Every display row of the cursor's source line is the cursor row
        // (wrapped continuations too), so the band covers the whole line.
        let is_cursor = row.src() == Some(app.cursor);
        let in_sel = row
            .src()
            .and_then(|s| sel_range.map(|(a, b)| a <= s && s <= b))
            .unwrap_or(false);
        let has_comment = row.src().map(|s| app.src_has_comment(s)).unwrap_or(false);
        // Telomere: age + read state of this row's source line (None for
        // synthesized rows).
        let age = row
            .src()
            .and_then(|s| app.lines.get(s))
            .map(|l| ((now_secs() - l.updated).max(0), app.line_unread(l)));
        let related_unread = row.src().and_then(|src| app.related_unread(src));
        let in_code = row.src().map(|s| code_flags.get(s) == Some(&true)).unwrap_or(false);
        let mut base = Style::default();
        // The wash goes down first: selection and the cursor band are
        // stronger signals and paint over it.
        if in_code && !in_sel && !is_cursor {
            base = base.bg(wash);
            for k in 0..h {
                let y = text.y as i32 + screen_y + k;
                if y >= band_top && y <= band_bot {
                    wash_rows.push(y as u16);
                }
            }
        }
        if in_sel {
            base = base.bg(SEL_BG);
        }
        if is_cursor {
            base = base.bg(CURSOR_BG);
            // Paint the WHOLE row between the frame columns before drawing
            // its content: telomere, the blank after it, text-side padding,
            // and the scrollbar column must read as one cursor band.
            let left = body.x.saturating_add(1);
            let right = body.x.saturating_add(body.width).saturating_sub(1);
            let buf = f.buffer_mut();
            for k in 0..h {
                let sy = screen_y + k;
                let y = text.y as i32 + sy;
                if y >= band_top && y <= band_bot {
                    for x in left..right {
                        if let Some(c) = buf.cell_mut((x, y as u16)) {
                            c.set_bg(CURSOR_BG);
                        }
                    }
                }
            }
        }

        // The gutter marker for each on-screen row of this Row (an image
        // occupies several). Uses the same screen row as the text: `text.y`
        // anchors content, rows flow with `-scroll` (top is the row's
        // content offset, view_top = scroll).
        if row.src().is_some() {
            for k in 0..h {
                let sy = screen_y + k;
                let y = text.y as i32 + sy;
                if y >= band_top && y <= band_bot {
                    let (glyph, mut style) =
                        gutter_cell(has_comment, age, related_unread, ctx.light);
                    if let Some(bg) = base.bg {
                        style = style.bg(bg);
                    }
                    gutter.push((y as u16, glyph, style));
                    if is_cursor {
                        let mut caret_style =
                            Style::default().fg(CHROME_CARET).add_modifier(Modifier::BOLD);
                        if let Some(bg) = base.bg {
                            caret_style = caret_style.bg(bg);
                        }
                        carets.push((y as u16, caret_style));
                    }
                }
            }
        }

        let one_row = |screen_y: i32| -> Option<Rect> {
            let y = text.y as i32 + screen_y;
            if y >= band_top && y <= band_bot {
                Some(Rect::new(text.x, y as u16, text.width, 1))
            } else {
                None
            }
        };

        match row {
            Row::Line { line, src, .. } => {
                if let Some(r) = one_row(screen_y) {
                    // A diagram being rendered dims its code and lets a band
                    // of brightness run down it: the reader sees the work
                    // happening without the text moving under them.
                    let painted = match app.web_shimmer.get(src) {
                        Some((pos, len)) => shimmer(line, *pos, *len, app, ctx),
                        None => line.clone(),
                    };
                    f.render_widget(Paragraph::new(painted).style(base), r);
                }
            }
            Row::Card { line } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(Paragraph::new(line.clone()).style(base), r);
                }
            }
            // Already painted as the frame's └───┘ rule above.
            Row::FrameEnd => {}
            Row::Blank { .. } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(Paragraph::new("").style(base), r);
                }
            }
            Row::ImageError { msg, indent, item, .. } => {
                if let Some(r) = one_row(screen_y) {
                    let pad = bullet_pad(*indent, *item);
                    f.render_widget(
                        Paragraph::new(format!("{pad} {msg}")).style(base.fg(Color::Red)),
                        r,
                    );
                }
            }
            Row::ImageLoading { indent, item, .. } => {
                if let Some(r) = one_row(screen_y) {
                    let pad = bullet_pad(*indent, *item);
                    f.render_widget(
                        Paragraph::new(format!("{pad} □ loading image…"))
                            .style(base.fg(Color::DarkGray)),
                        r,
                    );
                }
            }
            Row::Inline { images, texts, indent, item, .. } => {
                // The bullet belongs at the item's TOP-left: the line
                // starts there, however tall the pictures on it are.
                if *item && *indent >= 2 {
                    let y = text.y as i32 + screen_y;
                    if y >= band_top && y <= band_bot {
                        image_bullets.push((y as u16, text.x + *indent as u16 - 2));
                    }
                }
                // Text first: a picture paints its own cells over the top,
                // and nothing here may write into them.
                for (row_off, col, line) in texts {
                    if let Some(r) = one_row(screen_y + *row_off as i32) {
                        let w = r.width.saturating_sub(*col);
                        if w > 0 {
                            f.render_widget(
                                Paragraph::new(line.clone()).style(base),
                                Rect::new(r.x + *col, r.y, w, 1),
                            );
                        }
                    }
                }
                for (row_off, col, url) in images {
                    let Some(info) = app.images.get(url) else { continue };
                    let top = screen_y + *row_off as i32;
                    let pos = SignedPosition {
                        x: 0,
                        y: (top + text.y as i32 - band_top) as i16,
                    };
                    let w = info.cells_w.min(text.width.saturating_sub(*col));
                    if w == 0 {
                        continue;
                    }
                    f.render_widget(
                        SlicedImage::new(&info.sliced, pos),
                        Rect::new(text.x + *col, band_top as u16, w, band_h),
                    );
                }
            }
            Row::Image { url, indent, item, .. } => {
                if *item && *indent >= 2 {
                    let y = text.y as i32 + screen_y;
                    if y >= band_top && y <= band_bot {
                        image_bullets.push((y as u16, text.x + *indent as u16 - 2));
                    }
                }
                // Render into the full text area with a signed position: when
                // the image top is scrolled above the viewport, screen_y is
                // negative and the sliced protocol clips the hidden rows; when
                // it extends past the bottom, the area clips it. Either way it
                // stays visible for its in-view portion (no all-or-nothing).
                // The cursor band cannot paint over image pixels, so on an
                // image line only the gutter marker shows the cursor (akapen
                // behaves the same). Position the slice relative to text.y
                // (the content anchor), matching how text rows move.
                if let Some(info) = app.images.get(url) {
                    // The image area starts at the whole band's top, unlike
                    // the unscrolled text anchor (`text.y`) one row below it.
                    // Adding that one-cell delta lets a partially scrolled
                    // image paint the reclaimed top row too.
                    // The picture starts exactly at the text column of its
                    // level, so it lines up with the lines around it. (The
                    // protocol's own one-column inset used to add itself on
                    // top of the indent and pushed indented pictures right.)
                    let off = (*indent as u16).min(text.width.saturating_sub(2));
                    let pos = SignedPosition {
                        x: 0,
                        y: (screen_y + text.y as i32 - band_top) as i16,
                    };
                    // Never hand the protocol more columns than it has, and
                    // never more than the pane: a diagram encoded for a
                    // wider pane (a rescale still in flight) is clipped here
                    // rather than painting over the frame.
                    let img_area = Rect::new(
                        text.x + off,
                        band_top as u16,
                        info.cells_w.min(text.width.saturating_sub(off)),
                        band_h,
                    );
                    f.render_widget(SlicedImage::new(&info.sliced, pos), img_area);
                }
            }
        }
    }

    // Marker columns: the telomere remains in the inside gutter, while `>`
    // replaces the frame glyph at the same screen row. Rows without a
    // source line leave both blank. (`sy` is already an absolute row.)
    let buf = f.buffer_mut();
    for (sy, glyph, style) in gutter {
        if let Some(c) = buf.cell_mut((gutter_x, sy)) {
            c.set_symbol(glyph);
            c.set_style(style);
        }
    }
    for (sy, x) in image_bullets {
        if let Some(c) = buf.cell_mut((x, sy)) {
            c.set_symbol(BULLET);
            c.set_style(Style::default().fg(ctx.palette.bullet));
        }
    }
    for (sy, style) in carets {
        if let Some(c) = buf.cell_mut((caret_x, sy)) {
            c.set_symbol(">");
            c.set_style(style);
        }
    }
    // Scrollbar: ONLY the thumb is drawn, in its own column just inside the
    // frame's right border — there is no always-on track, so non-thumb rows
    // show just the clean `│` border (akapen: `…▐│` only where the thumb
    // is, never a double line). The thumb tracks the VIEWPORT offset (wheel
    // scroll moves the viewport only); when the content fits, nothing is
    // drawn and the border stays clean.
    // The track starts below the top rule while that rule is visible, then
    // expands into the reclaimed row. Its scroll range still matches the
    // full keyboard/wheel viewport.
    let thumb = Style::default().fg(CHROME_SCROLL);
    if let Some((start, len)) = cosense::theme::scroll_thumb_in_track(
        (app.total_height() + 1) as usize,
        band_h as usize,
        bar.height as usize,
        app.scroll as usize,
    ) {
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((bar.x, bar.y + i as u16)) {
                c.set_symbol("▐");
                c.set_style(thumb);
            }
        }
    }

    // The code wash, run to the frame on both sides. Only the background
    // is touched, so the telomere glyph, the scrollbar thumb and the text
    // keep their own colors and simply sit on the block's surface.
    let left = body.x.saturating_add(1);
    let right = body.x.saturating_add(body.width).saturating_sub(1);
    for sy in wash_rows {
        for x in left..right {
            if let Some(c) = buf.cell_mut((x, sy)) {
                c.set_bg(wash);
            }
        }
    }

    // The edit session's caret: put the HARDWARE cursor on it. Terminal
    // IMEs anchor their inline composition window to the hardware cursor,
    // so this is what makes 日本語入力 land visually at the caret (akapen's
    // composer technique). `wrap_plain_columns` drives both the display
    // and this math, so they can never disagree.
    if let Some(s) = &app.session {
        if let Some((first, _)) = app.src_rows(s.line) {
            let text_w = App::text_width(app.mode, app.laid_width.max(1));
            let code = app.raw_span_at_line(s.line);
            let disp = session_display(&s.input.buf, code);
            let dcaret = display_caret(&s.input.buf, s.input.cur, code);
            let wrapped =
                SessionWrap::new(&disp, text_w, session_hang(&s.input.buf, code));
            let (crow, ccol) = wrapped.row_col(dcaret);
            let y = text.y as i32 + (app.row_top(first) as i32 + crow as i32) - app.scroll as i32;
            let x = text.x as i32 + (ccol as i32).min(text.width.saturating_sub(1) as i32);
            if y >= band_top && y <= band_bot {
                f.set_cursor_position(ratatui::layout::Position::new(x as u16, y as u16));
            }
        }
    }

    // Comment composer at the bottom. The caret is drawn at the input's
    // cursor (←/→/^a/^e move it), not glued to the end.
    if let Some(input) = &app.composing {
        let h = 3u16;
        let r = Rect::new(area.x, area.y + area.height.saturating_sub(1 + h), area.width, h);
        f.render_widget(Clear, r);
        let (a, b) = app.selection.map(|s| s.range()).unwrap_or((app.cursor, app.cursor));
        let label = if a == b {
            t!(" {} 行目へのコメント ", " comment on line {} ", a + 1)
        } else {
            t!(" {}-{} 行目へのコメント ", " comment on lines {}-{} ", a + 1, b + 1)
        };
        let (before, after) = input.parts();
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(CHROME_ACTIVE)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!("> {before}▏{after}")),
                Line::from(Span::styled(
                    "Enter save · Esc cancel · ←/→ ^a/^e ^w ^u",
                    Style::default().fg(CHROME_DIM),
                )),
            ]),
            r,
        );
    }

    // Modal overlay (comments list / link picker / help) on top.
    if app.overlay.is_some() {
        draw_overlay(f, app, area);
    }
}

/// One thing on a line: a run of text, or a picture with its cell size.
#[derive(Clone, Debug)]
enum Inline {
    Text(Line<'static>),
    Image { url: String, w: u16, h: u16 },
}

/// Where the layout put something.
#[derive(Clone, Debug, PartialEq)]
struct Placed<T> {
    row: u16,
    col: u16,
    what: T,
}

/// A line laid out the way a browser lays out inline images: items run
/// left to right, and each picture sits ON the text line — its BOTTOM
/// edge level with the text, growing upwards — so the words before and
/// after it read as one sentence. When a row fills, the next "line box"
/// starts below, indented like the rest of the item.
///
/// This is what lets `本文 [画像]`, `[画像]本文` and `[A] と [B]` all be
/// the same thing: a sequence, not three special cases.
#[allow(unused_assignments)] // the final `flush!` resets state nobody reads
fn layout_inline(
    items: &[Inline],
    indent: usize,
    width: usize,
) -> (Vec<Placed<String>>, Vec<Placed<Line<'static>>>, u16) {
    let body = width.saturating_sub(indent).max(1);
    let mut images: Vec<Placed<String>> = Vec::new();
    let mut texts: Vec<Placed<Line<'static>>> = Vec::new();
    // The box being filled: its top row, how tall it is so far, how far
    // along it we are, and what has been put in it (positions are relative
    // to the box, resolved against its height when it is flushed).
    let mut top = 0u16;
    let mut box_h = 1u16;
    let mut x = 0usize;
    let mut pending_img: Vec<(u16, u16, u16, String)> = Vec::new(); // (col, w, h, url)
    let mut pending_txt: Vec<(u16, Line<'static>)> = Vec::new(); // (col, text)

    // Close the current box: pictures sit on the text line, so each one is
    // placed so its LAST row is the box's last row.
    macro_rules! flush {
        () => {
            for (col, _, h, url) in pending_img.drain(..) {
                let row = top + box_h - h;
                images.push(Placed { row, col: col + indent as u16, what: url });
            }
            for (col, line) in pending_txt.drain(..) {
                texts.push(Placed {
                    row: top + box_h - 1,
                    col: col + indent as u16,
                    what: line,
                });
            }
            top += box_h;
            box_h = 1;
            x = 0;
        };
    }

    for item in items {
        match item {
            Inline::Image { url, w, h } => {
                let w = (*w).min(body as u16);
                if x > 0 && x + w as usize > body {
                    flush!();
                }
                pending_img.push((x as u16, w, (*h).max(1), url.clone()));
                box_h = box_h.max((*h).max(1));
                x += w as usize;
            }
            Inline::Text(line) => {
                let mut rest = line.clone();
                loop {
                    let room = body.saturating_sub(x);
                    // Too little room to say anything: start a new box.
                    if room < 2 && x > 0 {
                        flush!();
                        continue;
                    }
                    let mut pieces = wrap_line(&rest, room.max(1)).into_iter();
                    let Some(first) = pieces.next() else { break };
                    let used = str_width(
                        &first.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
                    );
                    pending_txt.push((x as u16, first));
                    x += used;
                    let tail: Vec<Span<'static>> =
                        pieces.flat_map(|l| l.spans.into_iter()).collect();
                    if tail.is_empty() {
                        break;
                    }
                    flush!();
                    rest = Line::from(tail);
                }
            }
        }
    }
    flush!();
    (images, texts, top.max(1))
}

/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder: the bullet where the
/// picture's own bullet goes, then the space the picture would start at.
fn bullet_pad(indent: usize, item: bool) -> String {
    if item && indent >= 2 {
        format!("{}{BULLET} ", " ".repeat(indent - 2))
    } else {
        " ".repeat(indent)
    }
}

/// One row of a block the web renderer is still working on/// One row of a block the web renderer is still working on, with the
/// brightness band applied to every span.
fn shimmer(line: &Line<'static>, pos: u16, len: u16, app: &App, ctx: &Ctx) -> Line<'static> {
    let level = cosense::theme::shimmer_level(pos, len, app.web_anim.elapsed().as_secs_f32());
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| {
            Span::styled(
                s.content.clone(),
                cosense::theme::shimmer_style(s.style, ctx.terminal_bg, level),
            )
        })
        .collect();
    Line::from(spans)
}

/// Draw the open overlay as a centered panel.
fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    let (title, items, cursor): (String, Vec<String>, usize) = match app.overlay.as_ref() {
        Some(Overlay::Links { items, cursor }) => (
            t!("この行のリンク", "links on this line"),
            items.iter().map(LinkItem::label).collect(),
            *cursor,
        ),
        Some(Overlay::Comments { cursor }) => (
            format!("comments ({})", app.comments.len()),
            app.comments
                .iter()
                .map(|c| {
                    let first = c.text.lines().next().unwrap_or("");
                    format!("{} :{}  {}", c.title, c.range_label(), first)
                })
                .collect(),
            *cursor,
        ),
        Some(Overlay::LineInfo) => {
            let src = app.cursor_src();
            let items = match src.and_then(|s| app.lines.get(s).map(|l| (s, l))) {
                Some((s, l)) => {
                    let name_of = |id: &str| -> String { app.member_name(id) };
                    // `line.user_id` is the LAST UPDATER (verified against the
                    // commit log). The original author would need the commit
                    // history (~1s per page), which is deliberately not worth
                    // it here — so only the updater is named.
                    // The line's text is already on screen under the cursor,
                    // and its id is only needed by `e` (browser deep-link),
                    // so neither is repeated here.
                    vec![
                        t!("行        {}", "line      {}", s + 1),
                        t!(
                            "更新      {}  （{}前）  {}",
                            "updated   {}  ({} ago)   {}",
                            cosense::theme::format_local(l.updated),
                            relative_age(l.updated),
                            name_of(&l.user_id)
                        ),
                        t!(
                            "作成      {}  （{}前）",
                            "created   {}  ({} ago)",
                            cosense::theme::format_local(l.created),
                            relative_age(l.created)
                        ),
                    ]
                }
                None => vec![t!("カーソルの下に行がありません", "no line under the cursor")],
            };
            (t!("行の詳細", "line detail"), items, usize::MAX)
        }
        Some(Overlay::Help) => {
            // The label column is 12 display cells wide. Japanese labels
            // are two cells per character, so each language pads its own
            // labels here rather than through a `{:<12}` that counts bytes.
            let mut keys: Vec<String> = vec![
                t!("移動        j/k · g/G · ^u/^d · PgUp/PgDn",
                   "move        j/k · g/G · ^u/^d · PgUp/PgDn"),
                t!("リンク      Enter/f で開く: ページ · 📎 ファイル → 保存先 · ↗ URL → ブラウザ",
                   "link        Enter/f open: page · 📎 file → download dir · ↗ URL → browser"),
                t!("マウス      クリックでリンク/行移動 · ドラッグで選択 · ホイールでスクロール",
                   "mouse       click link/open · click row/move · drag/select · wheel/scroll"),
                t!("移動履歴    [ 戻る · ] 進む", "history     [ back · ] forward"),
                t!("表示切替    Tab 表示⇄ソース", "mode        Tab view⇄source"),
                t!("ページ一覧  ^o 一覧＋抜粋 · Esc/[ 戻る · ] 進む",
                   "index       ^o list + excerpt · Esc/[ back · ] forward"),
            ];
            if app.editable {
                keys.extend([
                    t!("編集        e 行末 · i 行頭 · o/O 行を追加 · ダブルクリック — モードレスな編集",
                       "edit        e line end · i line start · o/O new line · double-click — modeless"),
                    t!("            編集中: そのまま入力 · ↑↓ 行移動 · Enter 改行 · ⌫@行頭 前の行と結合 · Esc 終了",
                       "            in session: type freely · ↑↓ lines · Enter new line · ⌫@BOL join · Esc done"),
                    t!("            x 行/選択を削除 · ^e ページ全体を $EDITOR で編集 · コミットは自動",
                       "            x delete line/selection · ^e whole page in $EDITOR · commits are automatic"),
                    t!("構造編集    m 移動モード（ブロックをつかむ）: j/k/↑↓ 1行 · J/K 兄弟ごと",
                       "outline     m move mode (grab a block): j/k/↑↓ one line · J/K whole sibling"),
                    t!("            h/l/←→ 字下げ · Esc/Enter/m 確定（掴んだまま他のキーを押すと確定）",
                       "            h/l/←→ indent · Esc/Enter/m commit (any other key commits too)"),
                    t!("            Ctrl+←/→/↑/↓ 行・選択範囲 · Alt+←/→/↑/↓ ブロック",
                       "            Ctrl+←/→/↑/↓ line/range · Alt+←/→/↑/↓ block"),
                    t!("            ^g h/j/k/l 行 左/下/上/右 · ^g H/J/K/L ブロック（選択中は不可）",
                       "            ^g h/j/k/l line left/down/up/right · ^g H/J/K/L block (no selection)"),
                    t!("取り消し    u 取り消し · ^r やり直し（どのコミットも戻せます）",
                       "undo        u undo · ^r redo (every commit is reversible)"),
                ]);
            } else {
                keys.push(t!(
                    "編集        できません — このアカウントはプロジェクトのメンバーではありません",
                    "edit        unavailable — this account is not a project member"
                ));
            }
            keys.extend([
                t!("ブラウザ    w カーソル行でページを開く", "browser     w open page at cursor line"),
                t!("ページ履歴  ← 古い履歴 · → 新しい · Esc 最新へ戻る（履歴中は読み取り専用）",
                   "time        ← older snapshot · → newer · Esc back to NOW (read-only while back)"),
                t!("コメント    v 選択 · c 追加 · d 削除 · ^n/^p 移動",
                   "comment     v select · c add · d delete · ^n/^p jump"),
                t!("行の詳細    t この行をいつ誰が更新したか", "detail      t who/when edited this line"),
                t!("図          code:mmd / code:mermaid / code:<名前>.mmd を図として描く。",
                   "diagram     code:mmd / code:mermaid / code:<name>.mmd draw as pictures,"),
                t!("            ヘッドレス Chrome で実際の Cosense ページを撮って切り出す",
                   "            screenshotted from the real Cosense page by headless Chrome"),
                t!("            （場所は COSENSE_CHROME）。ブラウザが無い、COSENSE_SID 無しで",
                   "            (COSENSE_CHROME to point at it). No browser, a private page"),
                t!("            非公開ページ、Mermaid のエラーのときはコードブロックのまま。",
                   "            without COSENSE_SID, or a Mermaid error → the code block stays."),
                t!("            R でこのページの未生成分を描く。ページを開いただけでは",
                   "            R renders this page's missing web artifacts. Opening a page"),
                t!("            キャッシュ済みしか出ない（ブラウザは高価なので、頼んだときに",
                   "            only shows cached ones: a browser is expensive, so it starts"),
                t!("            起動する）。COSENSE_WEB_RENDER=auto|off で振る舞いを変えられ、",
                   "            when you ask. COSENSE_WEB_RENDER=auto|off changes that;"),
                t!("            COSENSE_WEB_IDLE_SECS でブラウザを残す長さを決められる。",
                   "            COSENSE_WEB_IDLE_SECS is how long the browser stays warm."),
                t!("出力        y コメントを全件コピー", "output      y copy all comments"),
                t!("画面        l コメント一覧 · ? ヘルプ", "list        l comments · ? help"),
                t!("終了        q（Esc で取消。コメントは標準出力へ）",
                   "quit        q  (Esc cancels; comments print to stdout)"),
            ]);
            (t!("キー割り当て", "keys"), keys, usize::MAX)
        }
        None => return,
    };

    let w = area.width.saturating_sub(8).min(90).max(20);
    let want_h = items.len() as u16 + 2;
    let h = want_h.min(area.height.saturating_sub(4)).max(3);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let panel = Rect::new(x, y, w, h);
    f.render_widget(Clear, panel);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::Black)
            .bg(CHROME_ACCENT)
            .add_modifier(Modifier::BOLD),
    )));
    let visible = (h.saturating_sub(2)) as usize;
    let start = if cursor != usize::MAX && cursor >= visible {
        cursor + 1 - visible
    } else {
        0
    };
    for (i, it) in items.iter().enumerate().skip(start).take(visible) {
        let sel = i == cursor;
        let style = if sel {
            Style::default()
                .fg(Color::Black)
                .bg(CHROME_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if sel { "▸ " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{it}"), style)));
    }
    lines.push(Line::from(Span::styled(
        ts!(" ↑/↓ 移動 · Enter 開く · Esc 閉じる ", " ↑/↓ move · Enter open · Esc close "),
        Style::default().fg(CHROME_DIM),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Black)),
        panel,
    );
}

/// The telomere gutter cell for a row with a source line (`age` = seconds
/// since the edit, plus whether the line is unread). The cursor does not
/// compete for this cell: its `>` is painted separately on the frame column.
///
/// A comment marker is deliberately NOT drawn yet: akapen colors comments
/// yellow (`▌`) and reserves green for changed lines, so guessing a color
/// before the telomere/comment balance is settled would bake in a wrong
/// convention. `has_comment` is kept so the caller can still tint the row
/// without touching the marker column.
fn gutter_cell(
    _has_comment: bool,
    age: Option<(i64, bool)>,
    related_unread: Option<bool>,
    light: bool,
) -> (&'static str, Style) {
    // Related rows deliberately have no age encoding: read and unread use
    // the SAME thin mark; color alone carries the read state.
    if let Some(unread) = related_unread {
        let color = if unread { CHROME_CARET } else { CHROME_DIM };
        return ("▏", Style::default().fg(color));
    }
    match age {
        Some((a, unread)) => {
            let (glyph, color) = cosense::theme::telomere(a, unread, light);
            (glyph, Style::default().fg(color))
        }
        None => (" ", Style::default()),
    }
}

#[cfg(test)]
mod tests;


