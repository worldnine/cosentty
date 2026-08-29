// Single-page viewer (akapen-style): richly rendered read view with a
// persistent line cursor, in-place range selection, inline comment cards,
// and agent-actionable comment export.
//
// Keys:
//   j/k ↑/↓  move line cursor        Space/b  page down/up
//   g/G       top/bottom             v        start/stop range selection
//   c         write comment on cursor/selection
//   y         copy all comments (clipboard)   D  delete all comments
//   q         quit (comments also printed to stdout)
//   Enter/f   follow the line's link: a page navigates, an uploaded file
//             (📎) is saved to ~/Downloads and opened, an http(s) URL (↗)
//             opens in the browser (a gyazo image → its gyazo page)
//   wheel     scroll the viewport only (the cursor keeps its line)
//   click     open a link under the pointer, otherwise move the cursor
//   drag      select a range
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
use cosense::capability::{self, RenderCapability, SyncState};
use cosense::editops::{apply_ops, diff_to_ops, invert_ops};
use cosense::ws::{self, RemoteCommit, WsEvent};
use cosense::comment::{format_all, Comment, Selection};
use cosense::image_fetch::ImageFetcher;
use cosense::webrender::{ArtifactCache, WebBackend, WebError, WebRequest};
use cosense::highlight::Highlighter;
use cosense::render::{
    bullet_indent_width, file_name_of_url, gyazo_permalink, is_scrapbox_file_url,
    render_lines_with, Block,
};
use cosense::wrap::{hanging_prefix, wrap_line, wrap_line_continued};

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
/// Source mode's line-number gutter: `"  12 "` (4 digits + space).
const SOURCE_NUM_W: usize = 5;
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
    res: WebOutcome,
}

/// What became of one request. Typed because the three failures are NOT
/// interchangeable: a cache miss must leave the artifact renderable later,
/// a refusal must retune the session's capabilities, and only a genuine
/// failure is worth telling the reader about.
enum WebOutcome {
    Drawn(ImageInfo),
    /// Nothing on disk, and this pass was not allowed to draw. Not an
    /// error: it must not land in `web_errors`, or `m` could never draw it.
    Missing,
    /// The browser was refused (login wall). The session decides what to do
    /// with that — it says nothing about the REST credential.
    Denied,
    Failed(String),
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
                            None => WebOutcome::Failed("no cached artifact to resize".into()),
                        };
                        let _ = out.send(WebMsg { gen, key, rescale: true, res });
                    }
                    WebJob::Render { gen, reqs, max_cols, auth } => {
                        merged_gen = gen;
                        merged_cols = Some(max_cols);
                        // Coalescing several passes: the most permissive
                        // one wins, so an explicit `m` arriving behind a
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
                let _ =
                    out.send(WebMsg { gen, key, rescale: false, res: WebOutcome::Drawn(info) });
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
    let Some(auth) = auth else {
        // Cache-only pass. Every miss still owes exactly one reply, or the
        // viewer would pulse those blocks forever.
        for req in &to_render {
            let key = req.cache_key();
            let _ = out.send(WebMsg { gen, key, rescale: false, res: WebOutcome::Missing });
        }
        return;
    };
    let results = backend.render_batch(&to_render, auth);
    for (req, res) in to_render.iter().zip(results) {
        let key = req.cache_key();
        let res = match res {
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
        let _ = out.send(WebMsg { gen, key, rescale: false, res });
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
    /// An uploaded file (`[name https://scrapbox.io/files/…pdf]`).
    File { label: String, url: String },
    /// Any other http(s) URL (`[title https://…]`, bare URL) — the browser's.
    Url { label: String, url: String },
}

impl LinkItem {
    fn label(&self) -> String {
        match self {
            LinkItem::Page(t) => t.clone(),
            LinkItem::ProjectPage { project, title } => format!("/{project}/{title}"),
            LinkItem::File { label, .. } => format!("📎 {label}"),
            LinkItem::Url { label, .. } => format!("↗ {label}"),
        }
    }

    /// Labels this target can have in the rendered page. The link picker
    /// adds `↗`, while the page itself does not; hashtags and Gyazo add
    /// their own visible marker.
    fn rendered_labels(&self) -> Vec<String> {
        match self {
            LinkItem::Page(t) => vec![t.clone(), format!("#{t}")],
            LinkItem::ProjectPage { project, title } => vec![format!("/{project}/{title}")],
            LinkItem::File { label, .. } => vec![format!("📎 {label}")],
            LinkItem::Url { label, .. } => {
                vec![label.clone(), format!("🖼 {label}"), format!("🖼 [{label}]")]
            }
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
    Line { line: Line<'static>, src: usize },
    Blank { src: usize },
    Image { url: String, height: u16, src: usize },
    /// Space reserved for an image still downloading in the background.
    ImageLoading { src: usize },
    ImageError { msg: String, src: usize },
    Card { line: Line<'static> },
    FrameEnd,
}

impl Row {
    fn height(&self) -> u16 {
        match self {
            Row::Image { height, .. } => *height,
            Row::ImageLoading { .. } => IMAGE_PLACEHOLDER_H,
            _ => 1,
        }
    }
    fn src(&self) -> Option<usize> {
        match self {
            Row::Line { src, .. }
            | Row::Blank { src }
            | Row::Image { src, .. }
            | Row::ImageLoading { src }
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
    /// Source line where a left-button drag started (selection anchor).
    drag_anchor: Option<usize>,
    /// Scrollbar thumb grab: (track row, scroll offset) at the press.
    scrollbar_drag: Option<(u16, u16)>,
    scroll: u16, // row-height units from top

    /// SOURCE line under the cursor (0-based). Display rows are derived —
    /// see `cursor_rows`. akapen's `ViewState.cursor`.
    cursor: usize,
    /// Range selection over SOURCE lines.
    selection: Option<Selection>,
    /// Scroll the viewport to the cursor on the next frame. Set by keyboard
    /// navigation; wheel scrolling leaves it clear so the viewport can move
    /// away from the cursor (akapen's herdr-review style).
    follow: bool,
    /// Comment composer (the bottom input; `c`).
    composing: Option<Input>,
    comments: Vec<Comment>,
    status: String,

    /// Back stack of visited page titles (`[`), and forward stack (`]`).
    history: Vec<(String, String)>,
    forward: Vec<(String, String)>,
    /// Open overlay (comments list / link picker / help), if any.
    overlay: Option<Overlay>,

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
    /// Blocks whose diagram has been drawn on THIS page, identified by the
    /// id of the block's `code:` HEADER line. A block in here is one the
    /// reader is actually looking at, so when its source changes the picture
    /// must follow — even under the manual policy. The browser cost was
    /// already accepted for this page; making the reader press `m` again
    /// after every edit is not "manual", it is broken.
    ///
    /// The header line is the anchor because it is the one line an edit to
    /// the diagram does NOT move. Keying this on the block's LAST line —
    /// which is what the render request itself is keyed on — meant that
    /// pressing Enter inside the block moved the anchor too, and the
    /// refresh silently stopped firing for the most ordinary edit there is.
    web_drawn_blocks: HashSet<String>,
    /// Artifacts a cache-only pass looked for and did not find. They are
    /// NOT failures: they stay code blocks, stop shimmering, and `m` can
    /// still draw them.
    web_missing: HashSet<String>,
    /// The "you need a cookie" notice has been shown for THIS page already.
    /// Reset by `set_page`, so it is said once per page and not per block.
    web_notice_shown: bool,

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
    /// Last left-click (for double-click detection): time, column, row.
    last_click: Option<(Instant, u16, u16)>,
    /// Link pressed with the left button. Activation waits for button-up on
    /// the same target, so dragging can still select text/lines.
    pressed_link: Option<(usize, LinkItem)>,
    /// Related-pages sections below the body (view mode only).
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
}

/// One queued commit for the serial background worker. `gen` invalidates
/// jobs queued before a conflict reload (their base state is gone).
struct CommitJob {
    gen: u64,
    project: String,
    page_id: String,
    label: String,
    ops: Vec<EditOp>,
}

/// What a commit attempt came back with.
enum CommitOutcome {
    Done { label: String, title: String },
    Conflict,
    Skipped,
    Failed { label: String, msg: String },
}

/// A freshly polled page: how web-side edits reach the screen (~3 s).
struct PolledPage {
    project: String,
    title: String,
    page: cosense::api::Page,
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
            if let Ok(page) = client.get_page_in(&project, &title) {
                if tx.send(PolledPage { project, title, page }).is_err() {
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
            if job.gen < gen.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = out.send(CommitOutcome::Skipped);
                continue;
            }
            let res = client
                .preview_edit(&job.project, &job.page_id, &job.ops)
                .and_then(|p| client.submit_edit(&job.project, &p.preview_id));
            let outcome = match res {
                Ok(c) => CommitOutcome::Done { label: job.label, title: c.title },
                Err(EditError::NotFastForward) => CommitOutcome::Conflict,
                Err(e) => CommitOutcome::Failed { label: job.label, msg: e.to_string() },
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
    /// Recently-updated page picker with incremental filtering (akapen's
    /// `^o files` slot). Typing filters; ↑/↓ or ^n/^p move.
    Pages { all: Vec<(String, i64)>, filter: String, cursor: usize },
    /// Details for the cursor's line: who last edited it and when
    /// (akapen's `t` detail slot).
    LineInfo,
    /// Key reference.
    Help,
}

impl Overlay {
    /// Titles matching the current filter (case-insensitive substring).
    fn filtered_pages(all: &[(String, i64)], filter: &str) -> Vec<(String, i64)> {
        if filter.is_empty() {
            return all.to_vec();
        }
        let needle = filter.to_lowercase();
        all.iter()
            .filter(|(t, _)| t.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }
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
            follow: true,
            composing: None,
            comments: Vec::new(),
            status: String::new(),
            history: Vec::new(),
            forward: Vec::new(),
            overlay: None,
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
            commit_tx,
            commit_res_rx,
            commit_jobs_rx: Some(commit_jobs_rx),
            commit_res_tx,
            inflight: 0,
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
            web_drawn_blocks: HashSet::new(),
            web_notice_shown: false,
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
    /// related entries in view mode (virtual lines after the body).
    fn src_count(&self) -> usize {
        self.lines.len()
            + if self.mode == Mode::View { self.virtual_items.len() } else { 0 }
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
        self.time = None; // installing a live page always exits history
        // A page install ends any edit session and cuts the undo lineage:
        // undo ops reference THIS page's line ids.
        self.session = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
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
        self.lines = l.lines;
        self.blocks = l.blocks;
        self.srcs = l.srcs;
        self.read_at = l.read_at;
        self.editable = l.editable;
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
        self.web_drawn_blocks.clear();
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
        let urls: Vec<String> = self
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Image { url } => Some(url.clone()),
                _ => None,
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
    fn start_web_renders(&mut self, trigger: capability::Trigger) {
        // The browser can only ever show what the SERVER has, so nothing is
        // requested while a commit is still on its way there, or while a
        // commit is known to have failed. Both are whole-page gates: any
        // queued or lost commit could be the one that changes a diagram,
        // and `inflight == 0` alone only means "nothing in flight", not
        // "everything landed".
        if self.inflight > 0 || self.web_unsynced {
            return;
        }
        // A block whose picture is already on screen and whose source has
        // just moved is a REFRESH, not a first draw: the reader is watching
        // that diagram and expects it to keep up. Refreshes are treated as
        // explicit, so `manual` does not mean "press m after every edit".
        // One navigation draws the whole page anyway, so the other diagrams
        // on it ride along for free.
        let trigger = if trigger == capability::Trigger::Auto && self.has_stale_diagram() {
            capability::Trigger::Manual
        } else {
            trigger
        };
        // What this session may do, right now, for this project.
        let decision = capability::decide(&self.caps, self.render_policy, trigger);
        let auth = match decision {
            capability::Decision::Nothing => return,
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
        // when it is dispatched — not when it answers, or a second `m`
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
            if self.images.contains_key(&key) {
                // On screen: if this block's source later moves, that is a
                // refresh rather than a first draw.
                if let Some(anchor) = self.block_anchor(rows) {
                    self.web_drawn_blocks.insert(anchor);
                }
            }
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
            return;
        }
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
        }
    }

    /// The browser hit a login wall. This says something about the COOKIE,
    /// never about the PAT that is reading and committing this page — so
    /// nothing here touches `editable`, the credential, or the status.
    fn note_denied(&mut self, key: String) {
        match capability::on_not_authorized(&self.caps) {
            // A public page that refused a cookie refused a STALE cookie.
            // Anonymous is a different request, and usually works.
            capability::Denial::RetryAnonymous => {
                // From here the cookie counts as absent, so the retry that
                // `start_web_renders` queues is genuinely cookie-free.
                self.caps.cookie_rejected = true;
                self.caps.anonymous_spent = true;
                self.web_missing.insert(key);
                self.start_web_renders(capability::Trigger::Manual);
            }
            capability::Denial::GiveUp => {
                self.caps.browser_denied = true;
                self.web_missing.insert(key);
                if !self.web_notice_shown {
                    self.web_notice_shown = true;
                    let msg = if self.caps.sid {
                        capability::SID_REJECTED
                    } else {
                        capability::NEEDS_SID
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
        let Some(at) = self.status.find("sync: ") else { return };
        let Some(rest) = self.status.get(at + "sync: ".len()..) else { return };
        let end = rest.find(' ').map(|i| at + "sync: ".len() + i).unwrap_or(self.status.len());
        self.status.replace_range(at + "sync: ".len()..end, self.sync_label());
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

    /// Does the page hold a diagram that HAD a picture and no longer does,
    /// because its own source changed? That is the edit loop, and it must
    /// not wait for a keypress.
    fn has_stale_diagram(&self) -> bool {
        self.blocks.iter().any(|b| {
            let Block::WebRender { kind, code, rows, last_src } = b else { return false };
            if self.caret_is_inside(rows) {
                return false;
            }
            let Some(anchor) = self.block_anchor(rows) else { return false };
            if !self.web_drawn_blocks.contains(&anchor) {
                return false;
            }
            let Some(req) = self.web_request(*kind, code, *last_src) else { return false };
            let key = req.cache_key();
            !self.images.contains_key(&key) && !self.web_errors.contains_key(&key)
        })
    }

    /// A stable name for a diagram block: the id of its `code:` header line.
    /// Every other line in the block can be added, removed or retyped by an
    /// ordinary edit; the header is what stays put.
    fn block_anchor(&self, rows: &[(usize, Line<'static>)]) -> Option<String> {
        let (src, _) = rows.first()?;
        self.lines.get(*src).map(|l| l.id.clone())
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
    fn hint_text(&self, cursor_links: &[LinkItem]) -> String {
        if let Some(s) = self.session.as_ref() {
            let dirty = s.input.buf != s.orig;
            return format!(
                "{}↑↓ move · Enter new line · ⌫@行頭 join · Tab indent · Esc done",
                if dirty { "● " } else { "" }
            );
        }
        if !cursor_links.is_empty() {
            let listed: Vec<String> = cursor_links
                .iter()
                .take(9)
                .enumerate()
                .map(|(i, l)| format!("{}:{}", i + 1, l.label()))
                .collect();
            return format!("Enter/f open → {}", listed.join("  "));
        }
        if !self.status.is_empty() {
            return self.status.clone();
        }
        if let Some((msg, _)) = self.web_notice.as_ref() {
            return msg.clone();
        }
        if self.editable {
            "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit".into()
        } else {
            "j/k move  Enter link  w browser  ? help  q quit  · read-only".into()
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
        while let Ok(msg) = self.web_rx.try_recv() {
            if msg.gen != self.gen_now() {
                continue;
            }
            let WebMsg { key, rescale, res, .. } = msg;
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
                // back to being source, silently — `m` will pick it up.
                WebOutcome::Missing => {
                    self.web_missing.insert(key);
                }
                // The BROWSER was refused. The REST credential is untouched:
                // edits and reads carry on exactly as before.
                WebOutcome::Denied => {
                    self.note_denied(key);
                }
                WebOutcome::Failed(e) => {
                    self.note_web_failure(format!("diagram: {e} (showing source)"));
                    self.web_errors.insert(key, e);
                }
            }
            changed = true;
        }
        changed
    }

    /// Install images that finished loading. Returns true if anything
    /// changed (the caller rebuilds the layout, since heights shift). Cheap:
    /// the worker already did the decoding and encoding.
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
        let mut items: Vec<LinkItem> = links_on_line(text);
        items.extend(labelled_urls(text).into_iter().map(|(label, url)| {
            link_item_for_url(label, url)
        }));
        items
    }

    /// Mouse targets in rendered left-to-right order. Keyboard activation
    /// intentionally keeps its historic page-first ordering above.
    fn mouse_links_at_src(&self, src: usize) -> Vec<LinkItem> {
        if src >= self.lines.len() {
            return self.links_at_src(src);
        }
        let text = &self.lines[src].text;
        let mut positioned = positioned_links_on_line(text);
        positioned.extend(
            positioned_labelled_urls(text)
                .into_iter()
                .map(|(pos, label, url)| (pos, link_item_for_url(label, url))),
        );
        positioned.sort_by_key(|(pos, _)| *pos);
        positioned.into_iter().map(|(_, item)| item).collect()
    }

    fn cursor_line_links(&self) -> Vec<LinkItem> {
        self.links_at_src(self.cursor)
    }

    /// Save an uploaded file to ~/Downloads in the background; the result
    /// lands in `file_rx` and is reported (and opened) by the event loop.
    fn start_download(&mut self, ctx: &Ctx, label: String, url: String) {
        let dest = download_path(&label, &url);
        let tx = self.file_tx.clone();
        let fetcher = Arc::clone(&ctx.fetcher);
        self.status = format!("downloading {label}…");
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
                        format!("saved {shown} · opened")
                    } else {
                        format!("saved {shown}")
                    };
                }
                Err(e) => self.status = format!("download failed: {label} — {e}"),
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
                self.status = format!("member list failed for {}: {e}", self.project);
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

    /// View mode: rendered blocks wrapped to the pane, each tagged with the
    /// source line it came from. The edit session's caret line renders as
    /// RAW SOURCE (cosense web: the line with the caret reveals its
    /// notation; everything else stays rendered).
    fn content_view(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::View, width);
        let edit: Option<(usize, &str)> =
            self.session.as_ref().map(|s| (s.line, s.input.buf.as_str()));
        let raw_rows = |content: &mut Vec<Row>, buf: &str, src: usize| {
            // The indent renders as its bullet (dim) — same shape as the
            // view — while the underlying data stays whitespace.
            let disp = session_display(buf);
            let prefix = display_prefix_bytes(buf);
            for (k, seg) in wrap_plain_columns(&disp, text_w).into_iter().enumerate() {
                let line = if k == 0 && prefix > 0 && seg.len() >= prefix {
                    Line::from(vec![
                        Span::styled(
                            seg[..prefix].to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(seg[prefix..].to_string()),
                    ])
                } else {
                    Line::from(seg)
                };
                content.push(Row::Line { line, src });
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
                    for wrapped in wrap_line_continued(line, text_w, &hanging_prefix(line)) {
                        content.push(Row::Line { line: wrapped, src });
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
                        content.push(Row::Line { line, src: row_src });
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
                        for wrapped in
                            wrap_line_continued(line, text_w, &hanging_prefix(line))
                        {
                            content.push(Row::Line { line: wrapped, src: *rsrc });
                        }
                    }
                }
                Block::Image { url } => {
                    if let Some(info) = self.images.get(url) {
                        content.push(Row::Image { url: url.clone(), height: info.cells_h.max(1), src });
                    } else if let Some(msg) = self.image_errors.get(url) {
                        content.push(Row::ImageError { msg: msg.clone(), src });
                    } else {
                        // still downloading — reserve space so the page is
                        // readable now and the image slots in when it lands
                        content.push(Row::ImageLoading { src });
                    }
                }
            }
        }
        content
    }

    /// Build the related-pages sections that live BELOW and OUTSIDE the page
    /// frame. Rows remain cursor-addressable virtual lines; only the visual
    /// containment changes.
    fn related_rows(&self, text_w: usize) -> Vec<Row> {
        if self.related.is_empty() {
            return Vec::new();
        }
        let dim = Style::default().fg(CHROME_DIM);
        let link_style = Style::default()
            .fg(CHROME_CARET)
            .add_modifier(Modifier::UNDERLINED);
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
                rows.push(Row::Line { line: related_row(e, text_w, link_style), src: vsrc });
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
                content.push(Row::Line { line: Line::from(spans), src });
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
        let (content, related): (Vec<Row>, Vec<Row>) = match self.mode {
            Mode::View => {
                let text_w = Self::text_width(Mode::View, width);
                (self.content_view(width), self.related_rows(text_w))
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
        let up = (0..self.cursor).rev().find(|&s| self.src_rows(s).is_some());
        let down = (self.cursor + 1..n).find(|&s| self.src_rows(s).is_some());
        if let Some(s) = up.or(down) {
            self.cursor = s;
        }
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

    /// Link under a mouse cell in VIEW mode. Rendered links are underlined,
    /// so decoration-hidden source syntax and wrapped CJK labels do not need
    /// to be reverse-mapped to raw byte offsets.
    fn link_at_screen_position(
        &self,
        screen_row: i32,
        col: usize,
        pal: &cosense::theme::Palette,
    ) -> Option<(usize, LinkItem)> {
        if self.mode != Mode::View || self.session.is_some() {
            return None;
        }
        let Row::Line { line, src } = self.row_at_screen_row(screen_row)? else {
            return None;
        };
        let mut left = 0usize;
        let clicked = line.spans.iter().find_map(|span| {
            let width = str_width(span.content.as_ref());
            let hit = left <= col
                && col < left + width
                && span.style.add_modifier.contains(Modifier::UNDERLINED);
            left += width;
            hit.then(|| (span.content.trim().to_string(), span.style.fg, left - width))
                .filter(|(s, _, _)| !s.is_empty())
        })?;
        let (clicked, clicked_fg, clicked_left) = clicked;
        let candidates = self.mouse_links_at_src(*src);
        // A related-page row has exactly one target; its visible title may
        // be truncated with `…`, so no label reconstruction is necessary.
        if *src >= self.lines.len() {
            return candidates.into_iter().next().map(|item| (*src, item));
        }
        let color_matches = |item: &LinkItem| {
            match item {
                LinkItem::Page(_) | LinkItem::ProjectPage { .. } => {
                    clicked_fg == Some(pal.link) || clicked_fg == Some(pal.hashtag)
                }
                LinkItem::File { .. } | LinkItem::Url { .. } => {
                    clicked_fg == Some(pal.url) || clicked_fg == Some(Color::Magenta)
                }
            }
        };
        // Prefer exact labels. If two targets render with the same label,
        // count equal underlined spans before this cell and select the same
        // occurrence from the source-ordered candidates.
        let exact: Vec<&LinkItem> = candidates
            .iter()
            .filter(|item| {
                color_matches(item)
                    && item.rendered_labels().iter().any(|label| label == &clicked)
            })
            .collect();
        let matching: Vec<&LinkItem> = if exact.is_empty() {
            // Wrapping can split one link span; then the clicked fragment is
            // contained in its full rendered label.
            candidates
                .iter()
                .filter(|item| {
                    color_matches(item)
                        && item.rendered_labels().iter().any(|label| label.contains(&clicked))
                })
                .collect()
        } else {
            exact
        };
        let target_y = self.scroll as i32 + screen_row;
        if target_y < 0 {
            return None;
        }
        let target_y = target_y as u32;
        let mut y = 0u32;
        let mut occurrence = 0usize;
        for row in &self.rows {
            if y > target_y {
                break;
            }
            if let Row::Line { line, src: row_src } = row {
                if row_src == src {
                    let mut span_left = 0usize;
                    for span in &line.spans {
                        let width = str_width(span.content.as_ref());
                        let before_clicked = y < target_y || span_left < clicked_left;
                        if before_clicked
                            && span.style.add_modifier.contains(Modifier::UNDERLINED)
                            && span.style.fg == clicked_fg
                            && span.content.trim() == clicked
                        {
                            occurrence += 1;
                        }
                        span_left += width;
                    }
                }
            }
            y += row.height() as u32;
        }
        let item = matching.get(occurrence).or_else(|| matching.first())?;
        Some((*src, (*item).clone()))
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
fn wrap_plain_columns(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let w = w.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push(ch);
        used += cw;
    }
    out.push(cur);
    out
}

/// (row, column) of byte offset `caret` within `wrap_plain_columns(s, w)`.
/// A caret exactly at a segment boundary sits at the START of the next
/// segment (that is where the next typed char will appear).
fn caret_row_col(s: &str, caret: usize, w: usize) -> (usize, usize) {
    let segs = wrap_plain_columns(s, w);
    let mut off = 0usize;
    for (i, seg) in segs.iter().enumerate() {
        let end = off + seg.len();
        if caret < end || (caret == end && i + 1 == segs.len()) {
            return (i, str_width(&seg[..caret - off]));
        }
        off = end;
    }
    (segs.len().saturating_sub(1), str_width(segs.last().map(String::as_str).unwrap_or("")))
}

/// Byte offset in `s` whose display column is closest to `col` (used by
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
fn session_display(buf: &str) -> String {
    let ind = indent_of(buf);
    let n = ind.chars().count();
    if n == 0 {
        return buf.to_string();
    }
    let mut out = String::with_capacity(buf.len() + n + 4);
    out.push_str(&" ".repeat(bullet_indent_width(n)));
    out.push('•');
    out.push(' ');
    out.push_str(&buf[ind.len()..]);
    out
}

/// Byte length of the `…• ` prefix in `session_display(buf)` (0 when the
/// line has no indent).
fn display_prefix_bytes(buf: &str) -> usize {
    let n = indent_of(buf).chars().count();
    if n == 0 { 0 } else { bullet_indent_width(n) + '•'.len_utf8() + 1 }
}

/// Byte offset in `session_display(buf)` for byte offset `caret` in `buf`.
fn display_caret(buf: &str, caret: usize) -> usize {
    let n = indent_of(buf).chars().count();
    let ci = buf[..caret.min(buf.len())].chars().count();
    let di = if n == 0 {
        ci
    } else if ci < n {
        ci * 2
    } else {
        // The display prefix has 2n chars; the source prefix has n.
        ci + n
    };
    let disp = session_display(buf);
    disp.char_indices().nth(di).map(|(b, _)| b).unwrap_or_else(|| disp.len())
}

/// Inverse: a byte offset in `session_display(buf)` back to `buf`. A click
/// in either cell of one visual indent step maps to the nearest logical
/// source boundary; the injected space after `•` maps to the text start.
fn raw_caret_from_display(buf: &str, disp_byte: usize) -> usize {
    let n = indent_of(buf).chars().count();
    let disp = session_display(buf);
    let di = disp[..disp_byte.min(disp.len())].chars().count();
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

/// One related-pages row: `title · age  description`, title in the link
/// color, the rest dim, truncated to the pane width (related rows do not
/// wrap: they are a scannable list, not body text).
fn related_row(e: &RelEntry, text_w: usize, link_style: Style) -> Line<'static> {
    let dim = Style::default().fg(CHROME_DIM);
    let title = truncate_width(&e.title, text_w);
    let mut spans = vec![Span::styled(title.clone(), link_style)];
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
    let head = format!("  💬 lines {}", c.range_label());
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
            _ => positional.push(a),
        }
    }
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
    let picker = Picker::from_query_stdio()?;

    let ctx = Ctx {
        client,
        hl,
        palette,
        picker,
        fetcher,
        light,
        terminal_bg,
        ime_mode,
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
        initial_state.poll_interval(),
    );
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
        );
    }
    app.set_page(loaded, &ctx);
    // Say how we are authenticated (or that we are not): edits and private
    // reads depend on it, and `cosense login` is the fix when missing.
    // `sync:` shows which live-update path is active (ws = websocket push,
    // poll = 3 s polling).
    app.status = match ctx.client.credential_for(&project) {
        Some(c) if app.editable => format!(
            "auth: {} · edit enabled · sync: {} · ? help",
            c.kind(),
            app.sync_label()
        ),
        Some(c) => format!(
            "auth: {} · read-only (not a project member) · sync: {} · ? help",
            c.kind(),
            app.sync_label()
        ),
        None => "no auth — public read-only (`cosense login` to enable edits)".into(),
    };
    // `#<lineId>` from the URL: start on that line (the first frame's layout
    // clamps it onto a rendered line and scrolls it into view).
    if let Some(id) = line_id {
        match app.lines.iter().position(|l| l.id == id) {
            Some(i) => {
                app.cursor = i;
                app.follow = true;
            }
            None => app.status = format!("line {id} not found on this page"),
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
    /// See `App::read_at`.
    read_at: Option<i64>,
    /// Whether this credential may edit the loaded project.
    editable: bool,
    /// Related-pages sections (see `build_related`).
    related: Vec<RelSection>,
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

/// Fetch and render one page. Images are NOT downloaded here: they are
/// fetched on background threads (see `App::start_image_loads`) so a page
/// with many images still appears immediately.
fn load_page(ctx: &Ctx, project: &str, title: &str) -> Result<Loaded, Box<dyn Error>> {
    let page = ctx.client.get_page_in(project, title)?;
    let lines: Vec<PageLine> = page.lines.clone();
    let texts: Vec<String> = page.lines.iter().map(|l| l.text.clone()).collect();
    let rendered = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette);
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
        page_id: page.id.clone(),
        lines,
        blocks: rendered.blocks,
        srcs: rendered.srcs,
        read_at,
        editable,
        related,
    })
}

/// Turn a decoded image into a sliced protocol at a width-capped cell size.
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
    let nat_rows = (px_h as f32 / font.height as f32).ceil() as u32;
    let cap = max_cols.max(1) as u32;
    let (cw, ch) = if nat_cols > cap {
        let scale = cap as f32 / nat_cols as f32;
        (cap, ((nat_rows as f32) * scale).ceil() as u32)
    } else {
        (nat_cols.max(1), nat_rows.max(1))
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
                if let Some(rest) = inner.strip_prefix('/') {
                    if let Some((project, title)) = rest.split_once('/') {
                        if !project.is_empty() && !title.is_empty() {
                            out.push((
                                start,
                                LinkItem::ProjectPage {
                                    project: project.to_string(),
                                    title: title.to_string(),
                                },
                            ));
                        }
                    }
                } else {
                    out.push((start, LinkItem::Page(inner.to_string())));
                }
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
        // the enclosing bracket, if the URL sits inside one on this line
        let label = text[..start]
            .rfind('[')
            .filter(|&lb| !text[lb..start].contains(']'))
            .and_then(|lb| text[url_end..].find(']').map(|rb| (lb, url_end + rb)))
            .map(|(lb, rb)| text[lb + 1..rb].replace(url, "").trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| {
                if is_scrapbox_file_url(url) {
                    file_name_of_url(url).to_string()
                } else {
                    url.to_string()
                }
            });
        out.push((start, label, url.to_string()));
    }
    out
}

fn labelled_urls(text: &str) -> Vec<(String, String)> {
    positioned_labelled_urls(text)
        .into_iter()
        .map(|(_, label, url)| (label, url))
        .collect()
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

/// Where a downloaded file goes: `~/Downloads/<name>` (see
/// `download_path_in`).
fn download_path(label: &str, url: &str) -> std::path::PathBuf {
    let dir = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .ok()
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    download_path_in(&dir, label, url)
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
        LinkItem::File { label, url } => app.start_download(ctx, label, url),
        LinkItem::Url { url, .. } => {
            app.status = if open_in_browser(&url) {
                format!("opened {url}")
            } else {
                "failed to open browser".into()
            };
        }
    }
}

fn short(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

fn copy_to_clipboard(text: &str) -> bool {
    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Load `project/title` and install it, pushing the current page onto
/// history. `project` may differ from the current one (`[/project/title]`).
fn navigate_to(app: &mut App, ctx: &Ctx, project: &str, title: &str) {
    let from = (app.project.clone(), app.title.clone());
    let same_project = from.0 == project;
    match load_page(ctx, project, title) {
        Ok(loaded) => {
            app.history.push(from);
            app.set_page(loaded, ctx);
            app.status = if same_project {
                format!("→ {title}")
            } else {
                format!("→ /{project}/{title}")
            };
        }
        Err(e) => {
            app.status = format!("open failed: /{project}/{title} — {e}");
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
        // Install any images that finished downloading, then draw.
        if app.drain_images() | app.drain_web_renders() {
            app.laid_width = 0; // heights changed — rebuild layout
        }
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
    if app.session.is_some() {
        leave_session(app, ctx);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while app.inflight > 0 && Instant::now() < deadline {
        app.status = format!("…flushing {} edit(s)", app.inflight);
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
    let clean = data.replace("\r\n", "\n").replace('\r', "\n");
    if let Some(input) = app.composing.as_mut() {
        input.insert_str(&clean);
        return;
    }
    if app.session.is_some() {
        session_paste(app, ctx, &clean);
    }
}

/// One key press.
fn handle_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    // Comment composer captures all keys. Emacs/readline-style line
    // editing, so a Japanese sentence can be corrected mid-line.
    if app.composing.is_some() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (k.code, ctrl) {
            (KeyCode::Esc, _) => {
                app.composing = None;
                app.ime_guard = None; // back to ASCII for command mode
                app.status = "cancelled".into();
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

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match (k.code, ctrl) {
        // ---- quit ---- (akapen default: q quits, Esc only cancels)
        (KeyCode::Char('q'), false) => return Action::Quit,
        (KeyCode::Esc, false) => {
            if app.time.is_some() {
                app.time = None;
                reload_page(app, ctx);
                app.status = "NOW".into();
            } else if app.selection.is_some() {
                app.selection = None;
                app.status = "selection cleared".into();
            } else {
                app.status.clear();
            }
        }

        // ---- time machine (←/→, akapen's timeline; server snapshots) ----
        (KeyCode::Left, _) => travel(app, ctx, -1),
        (KeyCode::Right, _) => travel(app, ctx, 1),

        // ---- move (akapen parity) ----
        (KeyCode::Char('j'), false) | (KeyCode::Down, _) => app.move_cursor(true),
        (KeyCode::Char('k'), false) | (KeyCode::Up, _) => app.move_cursor(false),
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
                0 => app.status = "no link on this line".into(),
                1 => activate_link(app, ctx, links.remove(0)),
                _ => app.overlay = Some(Overlay::Links { items: links, cursor: 0 }),
            }
        }
        (KeyCode::Char('['), false) => {
            if let Some((project, title)) = app.history.pop() {
                let cur = (app.project.clone(), app.title.clone());
                match load_page(ctx, &project, &title) {
                    Ok(loaded) => {
                        app.forward.push(cur);
                        app.set_page(loaded, ctx);
                        app.status = format!("← {title}");
                    }
                    Err(e) => app.status = format!("back failed: {e}"),
                }
            } else {
                app.status = "no history".into();
            }
        }
        (KeyCode::Char(']'), false) => {
            if let Some((project, title)) = app.forward.pop() {
                let cur = (app.project.clone(), app.title.clone());
                match load_page(ctx, &project, &title) {
                    Ok(loaded) => {
                        app.history.push(cur);
                        app.set_page(loaded, ctx);
                        app.status = format!("→ {title}");
                    }
                    Err(e) => app.status = format!("forward failed: {e}"),
                }
            } else {
                app.status = "no forward history".into();
            }
        }

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

        // ---- draw this page's diagrams (**m**ermaid) ----
        // Rendering is manual by default: a page load only shows diagrams
        // it already has on disk, because launching a browser is by far the
        // most expensive thing this viewer does. This is how you ask.
        (KeyCode::Char('m'), false) => {
            let before = app.web_pending.len();
            app.start_web_renders(capability::Trigger::Manual);
            if app.web_pending.len() == before && app.web_notice.is_none() {
                app.note_web_failure(match app.render_policy {
                    capability::RenderPolicy::Off => {
                        "diagram: レンダラは off です (COSENSE_WEB_RENDER)".into()
                    }
                    _ => "diagram: 描画するものはありません".to_string(),
                });
            }
        }

        // ---- open in browser (moved from `e`: **w**eb) ----
        (KeyCode::Char('w'), false) => {
            let url = app.cursor_url();
            app.status = if open_in_browser(&url) {
                format!("opened {url}")
            } else {
                "failed to open browser".into()
            };
        }

        // ---- undo / redo (the safety net; no confirmation gates) ----
        (KeyCode::Char('u'), false) => undo(app, ctx),
        (KeyCode::Char('r'), true) => redo(app, ctx),

        // ---- comments (akapen parity) ----
        (KeyCode::Char('v'), false) => {
            if app.selection.is_some() {
                app.selection = None;
                app.status = "selection cleared".into();
            } else if app.cursor >= app.lines.len() {
                app.status = "related rows cannot be selected".into();
            } else {
                app.selection = Some(Selection::new(app.cursor));
                app.status = "selecting — j/k extend, c comment".into();
            }
        }
        (KeyCode::Char('c'), false) => {
            if app.time.is_some() {
                app.status = "viewing history — comments need NOW (Esc)".into();
                return Action::Continue;
            }
            app.composing = Some(Input::new(String::new()));
            app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
            app.status = "type comment, Enter save, Esc cancel".into();
        }

        // ---- new lines (vim's o/O; the session opens on the new line) ----
        (KeyCode::Char('o'), false) => open_line(app, ctx, false),
        (KeyCode::Char('O'), false) => open_line(app, ctx, true),
        (KeyCode::Char('e'), true) => {
            // Whole-page edit in $EDITOR (the “big edit” path: multi-line,
            // IME-free — the editor is the user's own environment).
            if !ensure_editable(app) {
                return Action::Continue;
            }
            if app.time.is_some() {
                app.status = "viewing history — read-only (Esc → NOW)".into();
                return Action::Continue;
            }
            return Action::Editor;
        }
        (KeyCode::Char('x'), false) => {
            if app.time.is_some() {
                app.status = "viewing history — read-only (Esc → NOW)".into();
                return Action::Continue;
            }
            // Delete the cursor line, or every line of the selection.
            let (a, b) = app
                .selection
                .map(|s| s.range())
                .unwrap_or((app.cursor, app.cursor));
            if a >= app.lines.len() {
                app.status = "related rows cannot be deleted".into();
                return Action::Continue;
            }
            let b = b.min(app.lines.len() - 1);
            let ops: Vec<EditOp> = (a..=b)
                .filter_map(|i| {
                    let id = app.lines[i].id.clone();
                    if id.is_empty() { None } else { Some(EditOp::Delete { id }) }
                })
                .collect();
            if ops.is_empty() {
                app.status = "nothing deletable here".into();
            } else {
                let n = ops.len();
                app.selection = None;
                do_edit(app, ctx, "delete", ops);
                app.status = format!("✓ deleted {n} line(s) · u to undo");
            }
        }
        (KeyCode::Char('d'), false) => {
            if let Some(i) = app.comment_at_cursor() {
                app.comments.remove(i);
                app.laid_width = 0;
                app.status = "comment deleted".into();
            } else {
                app.status = "no comment on this line".into();
            }
        }
        // jump between comments on this page
        (KeyCode::Char('n'), true) => jump_comment(app, true),
        (KeyCode::Char('p'), true) => jump_comment(app, false),

        // ---- output ----
        (KeyCode::Char('y'), false) => {
            if app.comments.is_empty() {
                app.status = "no comments to copy".into();
            } else {
                let text = format_all(&app.comments);
                app.status = if copy_to_clipboard(&text) {
                    format!("copied {} comment(s)", app.comments.len())
                } else {
                    "clipboard copy failed (pbcopy?)".into()
                };
            }
        }

        // ---- lists / help ----
        (KeyCode::Char('l'), false) => {
            app.overlay = Some(Overlay::Comments { cursor: 0 });
        }
        // page picker: recently-updated pages (akapen's `^o files` slot)
        (KeyCode::Char('o'), true) => {
            match ctx.client.list_pages_in(&app.project, 500, 0, "updated") {
                Ok((_, pages)) => {
                    let mut all: Vec<(String, i64)> =
                        pages.into_iter().map(|p| (p.title, p.updated)).collect();
                    // The API floats pinned pages to the top even with
                    // sort=updated, so a pinned 2-year-old page would head
                    // a "recently updated" list. Sort by real mtime here.
                    all.sort_by(|a, b| b.1.cmp(&a.1));
                    app.status = format!("{} pages", all.len());
                    app.overlay = Some(Overlay::Pages { all, filter: String::new(), cursor: 0 });
                }
                Err(e) => app.status = format!("page list failed: {e}"),
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
                    app.status =
                        "IMEがONのようです — 英数に切り替えてください（入力欄では自動で日本語になります）".into();
                    return Action::Continue;
                }
            }
            app.status = format!("unbound key: {:?} {:?}", k.code, k.modifiers);
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
        app.status = "empty comment discarded".into();
    } else if let Some(c) = app.make_comment(buf) {
        app.comments.push(c);
        app.selection = None;
        app.status = format!("comment saved ({} total)", app.comments.len());
        app.laid_width = 0; // force rebuild to weave the card
    } else {
        app.status = "could not anchor comment".into();
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
    let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
    let r = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette);
    app.blocks = r.blocks;
    app.srcs = r.srcs;
    app.laid_width = 0;
    app.start_image_loads(ctx);
    app.start_web_renders(capability::Trigger::Auto);
}

fn ensure_editable(app: &mut App) -> bool {
    if app.editable {
        true
    } else {
        app.status = "このプロジェクトでは編集権限がありません".into();
        false
    }
}

/// Apply `ops` locally, push their inverse onto the undo stack, and queue
/// the background commit. The single write path for every edit.
fn do_edit(app: &mut App, ctx: &Ctx, label: &str, ops: Vec<EditOp>) {
    if !ensure_editable(app) || ops.is_empty() {
        return;
    }
    let inverse = invert_ops(&app.lines, &ops);
    apply_ops(&mut app.lines, &ops);
    app.undo_stack.push((label.to_string(), inverse));
    if app.undo_stack.len() > 200 {
        app.undo_stack.remove(0);
    }
    app.redo_stack.clear();
    queue_commit(app, label, ops);
    rerender(app, ctx);
}

/// Send one commit job to the serial worker.
fn queue_commit(app: &mut App, label: &str, ops: Vec<EditOp>) {
    let job = CommitJob {
        gen: app.gen.load(std::sync::atomic::Ordering::SeqCst),
        project: app.project.clone(),
        page_id: app.page_id.clone(),
        label: label.to_string(),
        ops,
    };
    if app.commit_tx.send(job).is_ok() {
        app.inflight += 1;
    } else {
        app.status = "commit worker gone — edits are LOCAL ONLY".into();
        // The edit never left the machine: local lines and the server have
        // parted ways, and every diagram on the page must stay as source.
        app.mark_desynced();
    }
}

/// `u`: revert the newest commit (locally at once, on the server via the
/// queue). Ids of replaced lines survive; re-inserted lines get fresh ids.
fn undo(app: &mut App, ctx: &Ctx) {
    if !ensure_editable(app) {
        return;
    }
    let Some((label, ops)) = app.undo_stack.pop() else {
        app.status = "nothing to undo".into();
        return;
    };
    let redo = invert_ops(&app.lines, &ops);
    apply_ops(&mut app.lines, &ops);
    app.redo_stack.push((label.clone(), redo));
    queue_commit(app, &format!("undo {label}"), ops);
    rerender(app, ctx);
    app.status = format!("undid {label} ({} more)", app.undo_stack.len());
}

/// `^r`: re-apply the newest undone commit.
fn redo(app: &mut App, ctx: &Ctx) {
    if !ensure_editable(app) {
        return;
    }
    let Some((label, ops)) = app.redo_stack.pop() else {
        app.status = "nothing to redo".into();
        return;
    };
    let undo_ops = invert_ops(&app.lines, &ops);
    apply_ops(&mut app.lines, &ops);
    app.undo_stack.push((label.clone(), undo_ops));
    queue_commit(app, &format!("redo {label}"), ops);
    rerender(app, ctx);
    app.status = format!("redid {label}");
}

// ---------------------------------------------------------------------------
// The modeless edit session (SPEC §1–§3).
// ---------------------------------------------------------------------------

/// Open the session on body line `line` with the caret at byte `caret`.
fn enter_session(app: &mut App, ctx: &Ctx, line: usize, caret: usize) {
    if !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.status = "viewing history — read-only (Esc → NOW)".into();
        return;
    }
    if line >= app.lines.len() {
        app.status = "related rows cannot be edited (o adds a line at the end)".into();
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
    });
    app.cursor = line;
    app.selection = None;
    app.follow = true;
    app.laid_width = 0;
    app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
    app.status = "EDIT — ↑↓ move · Enter new line · Esc done".into();
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

/// Leave the session (Esc): commit the dirty line, back to READ + ASCII.
fn leave_session(app: &mut App, ctx: &Ctx) {
    session_commit_dirty(app, ctx);
    app.session = None;
    app.ime_guard = None;
    app.laid_width = 0;
    app.status = "✓ done".into();
}

/// ↑/↓ inside the session: commit the dirty line, carry the caret to the
/// next/previous BODY line, keeping the display column (sticky).
fn session_move_line(app: &mut App, ctx: &Ctx, delta: i32) {
    session_commit_dirty(app, ctx);
    let Some(s) = app.session.as_ref() else { return };
    let cur = s.line as i32;
    let last = app.lines.len().saturating_sub(1) as i32;
    let target = (cur + delta).clamp(0, last);
    if target == cur {
        app.status = if delta < 0 { "top of page".into() } else { "end of page — Enter adds a line".into() };
        return;
    }
    let col = {
        let s = app.session.as_ref().unwrap();
        s.want_col
            .unwrap_or_else(|| str_width(&s.input.buf[..s.input.cur]))
    };
    let line = target as usize;
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

/// Enter inside the session: split at the caret (at EOL this creates a
/// fresh line). The new line inherits the indent; the caret lands after
/// it; the session CONTINUES there — keep typing.
///
/// Exception — the standard list-escape: Enter on an EMPTY bullet (a
/// whitespace-only line) does not chain another empty bullet. The empty
/// bullet dissolves into a true blank line (its spaces are removed) and
/// the fresh line starts flush, with no indent.
fn session_split(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
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
        do_edit(app, ctx, "new line", ops);
        if let Some(s) = app.session.as_mut() {
            s.line = line + 1;
            s.input = Input { buf: String::new(), cur: 0 };
            s.orig = String::new();
            s.want_col = None;
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
    do_edit(app, ctx, "new line", ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input { buf: tail.clone(), cur: caret_new };
        s.orig = tail;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
}

/// Backspace at BOL: join this line into the previous one; the caret
/// lands on the junction. Joining line 1 into line 0 edits the title.
fn session_join_up(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line == 0 {
        app.status = "top of page".into();
        return;
    }
    let prev_text = app.lines[line - 1].text.clone();
    let merged = format!("{prev_text}{buf}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line - 1].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line].id.clone() },
    ];
    do_edit(app, ctx, "join", ops);
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
        app.status = "end of page".into();
        return;
    }
    let next_text = app.lines[line + 1].text.clone();
    let merged = format!("{buf}{next_text}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line + 1].id.clone() },
    ];
    do_edit(app, ctx, "join", ops);
    if let Some(s) = app.session.as_mut() {
        s.input = Input { buf: merged.clone(), cur: buf.len() };
        s.orig = merged;
        s.want_col = None;
    }
}

/// Tab / Shift+Tab: indent or outdent by one logical level (one source
/// space, rendered as a two-cell nesting step).
fn session_indent(app: &mut App, delta: i32) {
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
    if !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.status = "viewing history — read-only (Esc → NOW)".into();
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
    do_edit(app, ctx, "new line", ops);
    if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
        enter_session(app, ctx, idx, indent.len());
    }
}

/// One key while the session is open — the modeless core: printable keys
/// type, arrows move the caret, Enter makes lines, Esc leaves.
fn handle_session_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    // Any horizontal edit resets the sticky ↑/↓ column.
    let reset_col = |app: &mut App| {
        if let Some(s) = app.session.as_mut() {
            s.want_col = None;
        }
    };
    let edit_input = |app: &mut App, f: fn(&mut Input)| {
        if let Some(s) = app.session.as_mut() {
            f(&mut s.input);
            s.want_col = None;
        }
        app.laid_width = 0;
        app.follow = true;
    };
    match (k.code, ctrl) {
        (KeyCode::Esc, _) => leave_session(app, ctx),
        (KeyCode::Enter, _) => session_split(app, ctx),
        (KeyCode::Up, _) => session_move_line(app, ctx, -1),
        (KeyCode::Down, _) => session_move_line(app, ctx, 1),
        (KeyCode::Left, _) | (KeyCode::Char('b'), true) => edit_input(app, Input::left),
        (KeyCode::Right, _) | (KeyCode::Char('f'), true) => edit_input(app, Input::right),
        (KeyCode::Home, _) | (KeyCode::Char('a'), true) => edit_input(app, Input::home),
        (KeyCode::End, _) | (KeyCode::Char('e'), true) => edit_input(app, Input::end),
        (KeyCode::Char('w'), true) => edit_input(app, Input::delete_word),
        (KeyCode::Char('u'), true) => edit_input(app, Input::kill_to_start),
        (KeyCode::Backspace, _) => {
            let at_bol = app.session.as_ref().map(|s| s.input.cur == 0).unwrap_or(false);
            if at_bol {
                session_join_up(app, ctx);
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
        (KeyCode::Char(ch), false) => {
            if let Some(s) = app.session.as_mut() {
                s.input.insert_char(ch);
                s.want_col = None;
            }
            app.laid_width = 0;
            app.follow = true;
        }
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
        if let Some(s) = app.session.as_mut() {
            s.input.insert_str(clean);
            s.want_col = None;
        }
        app.laid_width = 0;
        return;
    }
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
    do_edit(app, ctx, "paste", ops);
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
        app.status = format!("temp file failed: {e}");
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
        app.status = format!("editor aborted ({editor}) — nothing written");
        return;
    }
    let edited = edited.strip_suffix('\n').unwrap_or(&edited).to_string();
    if edited == original {
        app.status = "no changes".into();
        return;
    }
    let new_lines: Vec<String> = edited.split('\n').map(str::to_string).collect();
    if new_lines.iter().all(|l| l.trim().is_empty()) {
        app.status = "page emptied — refusing (delete pages in the browser)".into();
        return;
    }
    let old: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let ops = diff_to_ops(&old, &new_lines);
    if ops.is_empty() {
        app.status = "no changes".into();
    } else {
        let n = ops.len();
        do_edit(app, ctx, "editor", ops);
        app.status = format!("✓ editor: {n} op(s) committed · u to undo");
    }
}

/// A commit came back from the worker (drained per frame).
fn handle_commit_outcome(app: &mut App, ctx: &Ctx, outcome: CommitOutcome) {
    app.inflight = app.inflight.saturating_sub(1);
    match outcome {
        CommitOutcome::Done { label, title } => {
            // Title-line edits rename the page (auto-suffix included).
            if !title.is_empty() && title != app.title {
                app.title = title;
                if let Ok(mut t) = app.poll_target.lock() {
                    *t = (app.project.clone(), app.title.clone());
                }
            }
            if app.status.is_empty() || app.status.starts_with('✓') || app.status.starts_with("EDIT") {
                app.status = format!("✓ {label}");
            }
        }
        CommitOutcome::Skipped => {}
        CommitOutcome::Failed { label, msg } => {
            app.status = format!("commit failed: {label} — {msg}");
            app.mark_desynced();
        }
        CommitOutcome::Conflict => recover_conflict(app, ctx),
    }
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
    // Remote lineage: local undo anchors may no longer exist.
    app.undo_stack.clear();
    app.redo_stack.clear();
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
        return;
    }
    install_remote_lines(app, ctx, &polled.page, "⟳ web側の編集を反映");
}

// -------------------------------------------------------------------------
// Websocket push sync (NOTE-websocket-sync.md)
// -------------------------------------------------------------------------

/// One event from the ws thread → the event loop.
fn handle_ws_event(app: &mut App, ctx: &Ctx, ev: WsEvent) {
    match ev {
        WsEvent::State(st) => app.set_sync_state(st),
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
    if !remote_gate_clear(app) {
        // Buffered commits so far precede this full page (their effects are
        // inside it); commits received after it are in the channel still.
        app.ws_held_resync = Some((res, app.ws_pending.len()));
        return;
    }
    app.ws_pending.clear();
    app.ws_head = res.head;
    install_remote_lines(app, ctx, &res.page, "⟳ websocket 全同期");
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
    for _ in 0..pre.min(app.ws_pending.len()) {
        app.ws_pending.pop_front();
    }
    app.ws_head = res.head;
    install_remote_lines(app, ctx, &res.page, "⟳ websocket 全同期");
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
        app.ws_head = Some(c.commit_id.clone());
        rerender(app, ctx);
        reanchor_cursor_session(app, cursor_id, session_id);
        app.follow = true;
        app.status = "⟳ websocket で更新を反映".into();
        return true;
    }
    // Chain broke (reconnect gap, join replay, meta-only commits in
    // between): the event is NOT applied and NOT silently dropped — a
    // background full-page resync is requested, and the fetched page will
    // carry this commit's effect.
    app.ws_resync_pending = true;
    app.status = "⟳ websocket 差分に欠落 — 再同期します".into();
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
    let stash = app.session.as_ref().map(|s| {
        (app.lines.get(s.line).map(|l| l.id.clone()).unwrap_or_default(), s.input.buf.clone(), s.input.cur)
    });
    reload_page(app, ctx); // set_page clears session + undo lineage
    match stash {
        None => {
            app.status = "page changed by someone else — reloaded".into();
        }
        Some((id, buf, caret)) => {
            if let Some(idx) = app.lines.iter().position(|l| l.id == id) {
                enter_session(app, ctx, idx, 0);
                if let Some(s) = app.session.as_mut() {
                    s.input = Input { buf: buf.clone(), cur: caret.min(buf.len()) };
                }
                app.status = "page changed by someone else — reloaded, your line kept".into();
            } else if !buf.trim().is_empty() {
                // The line is gone: rescue the text as a fresh last line.
                let new_id = new_line_id();
                let ops = vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![(new_id.clone(), buf.clone())],
                }];
                do_edit(app, ctx, "rescue", ops);
                if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
                    enter_session(app, ctx, idx, buf.len());
                }
                app.status = "your line was deleted by someone else — text rescued at the end".into();
            } else {
                app.status = "page changed by someone else — reloaded".into();
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
            app.status = "already at NOW".into();
            return;
        }
        match ctx.client.list_snapshots(&app.project, &app.page_id) {
            Ok(points) if !points.is_empty() => {
                let last = points.len() - 1;
                app.time = Some(TimeMachine { points, pos: last, cache: HashMap::new() });
                show_snapshot(app, ctx, last);
            }
            Ok(_) => app.status = "no snapshots for this page".into(),
            Err(e) => app.status = format!("snapshot list failed: {e}"),
        }
        return;
    }
    let (pos, len) = {
        let tm = app.time.as_ref().unwrap();
        (tm.pos, tm.points.len())
    };
    if dir < 0 {
        if pos == 0 {
            app.status = "oldest snapshot".into();
        } else {
            show_snapshot(app, ctx, pos - 1);
        }
    } else if pos + 1 >= len {
        app.time = None;
        reload_page(app, ctx);
        app.status = "NOW".into();
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
                app.status = format!("snapshot fetch failed: {e}");
                return;
            }
        },
    };
    let texts: Vec<String> = snap.lines.iter().map(|l| l.text.clone()).collect();
    let rendered = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette);
    app.lines = snap.lines;
    app.blocks = rendered.blocks;
    app.srcs = rendered.srcs;
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
    app.status = format!(
        "⏪ {}/{} · {} ({} ago) · ← older · → newer · Esc NOW",
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
            app.status = format!("reload failed: {e}");
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

/// Caret byte for a click at display column `col` of body line `line`:
/// exact on the session's caret line (it shows raw source), best-effort on
/// rendered lines (raw and rendered columns differ where notation hides).
fn click_caret(app: &App, line: usize, col: usize) -> usize {
    let text = app
        .session
        .as_ref()
        .filter(|s| s.line == line)
        .map(|s| s.input.buf.clone())
        .or_else(|| app.lines.get(line).map(|l| l.text.clone()))
        .unwrap_or_default();
    // Map through the bullet display: what the eye clicked is the display
    // column, which the view (bullets at indent) also approximates.
    let disp = session_display(&text);
    raw_caret_from_display(&text, byte_at_col(&disp, col))
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
                    // Session open: a click just moves the caret (the
                    // dirty line commits when the caret leaves it).
                    if app.session.is_some() {
                        if src < app.lines.len() {
                            session_commit_dirty(app, ctx);
                            let caret = click_caret(app, src, col);
                            if let Some(s) = app.session.as_mut() {
                                if s.line != src {
                                    let t = app.lines[src].text.clone();
                                    s.line = src;
                                    s.orig = t.clone();
                                    s.input = Input { buf: t, cur: 0 };
                                }
                                s.input.cur = caret.min(s.input.buf.len());
                                s.want_col = None;
                            }
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
                            app.link_at_screen_position(screen_row, col, &ctx.palette)
                        {
                            app.selection = None;
                            app.drag_anchor = Some(link_src);
                            app.goto_src(link_src);
                            app.pressed_link = Some((link_src, item));
                            app.last_click = None;
                            return;
                        }
                    }
                    // READ: double-click enters the session at the clicked
                    // character (the cosense-web gesture); a single click
                    // moves the line cursor as before.
                    let now = Instant::now();
                    let dbl = app
                        .last_click
                        .map(|(t, cx, cy)| {
                            now.duration_since(t) < Duration::from_millis(450)
                                && cx.abs_diff(m.column) <= 1
                                && cy.abs_diff(m.row) <= 1
                        })
                        .unwrap_or(false);
                    app.last_click = Some((now, m.column, m.row));
                    if dbl && src < app.lines.len() && app.time.is_none() {
                        let caret = click_caret(app, src, col);
                        enter_session(app, ctx, src, caret);
                        return;
                    }
                    app.selection = None;
                    app.drag_anchor = Some(src);
                    app.goto_src(src);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.pressed_link = None;
            if let Some(anchor) = app.drag_anchor {
                if let Some(end) = app.src_at_screen_row(clamped_row) {
                    app.selection = Some(Selection { anchor, cursor: end });
                    app.goto_src(end);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some((src, target)) = app.pressed_link.take() {
                let same_target = in_rows
                    && in_text_col
                    && app
                        .link_at_screen_position(
                            screen_row,
                            m.column.saturating_sub(text.x) as usize,
                            &ctx.palette,
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
            app.status = format!("comment at line {}", src + 1);
        }
        None => app.status = "no more comments".into(),
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
        Type(char),
        Backspace,
    }
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let is_picker = matches!(app.overlay, Some(Overlay::Pages { .. }));

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
        KeyCode::Backspace if is_picker => Act::Backspace,
        // In the picker, plain characters filter; elsewhere j/k/q act.
        KeyCode::Char(c) if is_picker && !ctrl => Act::Type(c),
        KeyCode::Char('q') => Act::Close,
        KeyCode::Char('j') => Act::Down,
        KeyCode::Char('k') => Act::Up,
        _ => Act::None,
    };

    let len = match &app.overlay {
        Some(Overlay::Comments { .. }) => app.comments.len(),
        Some(Overlay::Links { items, .. }) => items.len(),
        Some(Overlay::Pages { all, filter, .. }) => {
            Overlay::filtered_pages(all, filter).len()
        }
        _ => 0,
    };

    match act {
        Act::Close => app.overlay = None,
        Act::Down => {
            if let Some(
                Overlay::Comments { cursor }
                | Overlay::Links { cursor, .. }
                | Overlay::Pages { cursor, .. },
            ) = app.overlay.as_mut()
            {
                if len > 0 {
                    *cursor = (*cursor + 1).min(len - 1);
                }
            }
        }
        Act::Up => {
            if let Some(
                Overlay::Comments { cursor }
                | Overlay::Links { cursor, .. }
                | Overlay::Pages { cursor, .. },
            ) = app.overlay.as_mut()
            {
                *cursor = cursor.saturating_sub(1);
            }
        }
        Act::Type(c) => {
            if let Some(Overlay::Pages { filter, cursor, .. }) = app.overlay.as_mut() {
                filter.push(c);
                *cursor = 0;
            }
        }
        Act::Backspace => {
            if let Some(Overlay::Pages { filter, cursor, .. }) = app.overlay.as_mut() {
                filter.pop();
                *cursor = 0;
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
            let target: Option<(String, String, Option<usize>)> = match &app.overlay {
                Some(Overlay::Comments { cursor }) => app
                    .comments
                    .get(*cursor)
                    .map(|c| (c.project.clone(), c.title.clone(), Some(c.start))),
                Some(Overlay::Pages { all, filter, cursor }) => {
                    Overlay::filtered_pages(all, filter)
                        .get(*cursor)
                        .map(|(t, _)| (app.project.clone(), t.clone(), None))
                }
                _ => None,
            };
            app.overlay = None;
            if let Some((project, title, line)) = target {
                if project != app.project || title != app.title {
                    navigate_to(app, ctx, &project, &title);
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

    // header
    let sel_info = app
        .selection
        .map(|s| {
            let (a, b) = s.range();
            format!("  [sel {}-{}]", a + 1, b + 1)
        })
        .unwrap_or_default();
    // Unread badge: first visit anywhere, or how many lines changed since.
    let unread = match (app.read_at, app.unread_count()) {
        (None, _) => "  · first visit".to_string(),
        (Some(_), 0) => String::new(),
        (Some(_), n) => format!("  · {n} new"),
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
        Paragraph::new(Line::from(format!(
            " {}/{}{}{}{}  ({} comment(s)){}{} ",
            app.project,
            app.title,
            time_badge,
            if app.mode == Mode::Source { "  [source]" } else { "" },
            if app.editable { "" } else { "  [read-only]" },
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
        format!("R{}/{}", app.cursor - app.lines.len() + 1, app.virtual_items.len())
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
    let sel_range = app.selection.map(|s| s.range());
    // Telomeres and frame-column carets are painted after the rows.
    let mut gutter: Vec<(u16, &'static str, Style)> = Vec::new();
    let mut carets: Vec<(u16, Style)> = Vec::new();

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
        let mut base = Style::default();
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
            Row::Line { line, src } => {
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
            Row::ImageError { msg, .. } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(
                        Paragraph::new(format!(" {msg}")).style(base.fg(Color::Red)),
                        r,
                    );
                }
            }
            Row::ImageLoading { .. } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(
                        Paragraph::new(" □ loading image…").style(base.fg(Color::DarkGray)),
                        r,
                    );
                }
            }
            Row::Image { url, .. } => {
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
                    let pos = SignedPosition {
                        x: 1,
                        y: (screen_y + text.y as i32 - band_top) as i16,
                    };
                    // Never hand the protocol more columns than it has, and
                    // never more than the pane: a diagram encoded for a
                    // wider pane (a rescale still in flight) is clipped here
                    // rather than painting over the frame.
                    let img_area = Rect::new(
                        text.x,
                        band_top as u16,
                        info.cells_w.min(text.width),
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

    // The edit session's caret: put the HARDWARE cursor on it. Terminal
    // IMEs anchor their inline composition window to the hardware cursor,
    // so this is what makes 日本語入力 land visually at the caret (akapen's
    // composer technique). `wrap_plain_columns` drives both the display
    // and this math, so they can never disagree.
    if let Some(s) = &app.session {
        if let Some((first, _)) = app.src_rows(s.line) {
            let text_w = App::text_width(app.mode, app.laid_width.max(1));
            let disp = session_display(&s.input.buf);
            let dcaret = display_caret(&s.input.buf, s.input.cur);
            let (crow, ccol) = caret_row_col(&disp, dcaret, text_w);
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
            format!(" comment on line {} ", a + 1)
        } else {
            format!(" comment on lines {}-{} ", a + 1, b + 1)
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

/// One row of a block the web renderer is still working on, with the
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
            "links on this line".into(),
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
        Some(Overlay::Pages { all, filter, cursor }) => {
            let items = Overlay::filtered_pages(all, filter);
            let title = if filter.is_empty() {
                format!("pages — recently updated ({})", items.len())
            } else {
                format!("pages: {filter}_ ({} match)", items.len())
            };
            (
                title,
                items
                    .iter()
                    .map(|(t, u)| format!("{:>4}  {}", relative_age(*u), t))
                    .collect(),
                *cursor,
            )
        }
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
                        format!("line      {}", s + 1),
                        format!(
                            "updated   {}  ({} ago)   {}",
                            cosense::theme::format_local(l.updated),
                            relative_age(l.updated),
                            name_of(&l.user_id)
                        ),
                        format!(
                            "created   {}  ({} ago)",
                            cosense::theme::format_local(l.created),
                            relative_age(l.created)
                        ),
                    ]
                }
                None => vec!["no line under the cursor".into()],
            };
            ("line detail".into(), items, usize::MAX)
        }
        Some(Overlay::Help) => {
            let mut keys: Vec<String> = vec![
                "move      j/k · g/G · ^u/^d · PgUp/PgDn".into(),
                "link      Enter/f open: page · 📎 file → ~/Downloads · ↗ URL → browser".into(),
                "mouse     click link/open · click row/move · drag/select · wheel/scroll".into(),
                "history   [ back · ] forward".into(),
                "mode      Tab view⇄source".into(),
                "pages     ^o recent pages (type to filter)".into(),
            ];
            if app.editable {
                keys.extend([
                    "edit      e line end · i line start · o/O new line · double-click — modeless session".into(),
                    "          in session: type freely · ↑↓ lines · Enter new line · ⌫@BOL join · Esc done".into(),
                    "          x delete line/selection · ^e whole page in $EDITOR · commits are automatic".into(),
                    "undo      u undo · ^r redo (every commit is reversible)".into(),
                ]);
            } else {
                keys.push("edit      unavailable — this account is not a project member".into());
            }
            keys.extend([
                "browser   w open page at cursor line".into(),
                "time      ← older snapshot · → newer · Esc back to NOW (read-only while back)".into(),
                "comment   v select · c add · d delete · ^n/^p jump".into(),
                "detail    t who/when edited this line".into(),
                "diagram   code:mmd / code:mermaid / code:<name>.mmd draw as pictures,".into(),
                "          screenshotted from the real Cosense page by headless Chrome".into(),
                "          (COSENSE_CHROME to point at it). No browser, a private page".into(),
                "          without COSENSE_SID, or a Mermaid error → the code block stays.".into(),
                "          m draws this page's diagrams. Opening a page only shows the".into(),
                "          ones already cached: a browser is expensive, so it starts".into(),
                "          when you ask. COSENSE_WEB_RENDER=auto|off changes that;".into(),
                "          COSENSE_WEB_IDLE_SECS is how long the browser stays warm.".into(),
                "output    y copy all comments".into(),
                "list      l comments · ? help".into(),
                "quit      q  (Esc cancels; comments print to stdout)".into(),
            ]);
            ("keys".into(), keys, usize::MAX)
        }
        None => return,
    };

    let is_picker = matches!(app.overlay, Some(Overlay::Pages { .. }));
    let w = area.width.saturating_sub(8).min(90).max(20);
    // The page picker is a long list: give it most of the screen so filtering
    // shows plenty of candidates at once.
    let want_h = if is_picker {
        area.height.saturating_sub(4)
    } else {
        items.len() as u16 + 2
    };
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
    let footer = if is_picker {
        " type to filter · ↑/↓ or ^n/^p move · Enter open · Esc close "
    } else {
        " ↑/↓ move · Enter open · Esc close "
    };
    lines.push(Line::from(Span::styled(
        footer,
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
mod tests {
    use super::*;

    /// A network-free Ctx for tests that need one (mouse/session paths).
    /// The client never gets used: the commit worker is not spawned in
    /// tests, so jobs pile up in the App's own channel for inspection.
    fn test_ctx() -> Ctx {
        let cfg = Config {
            project: "proj".into(),
            auth: AuthStore::default(),
            api_domain: "scrapbox.io".into(),
        };
        Ctx {
            client: Client::new(cfg).unwrap(),
            hl: Highlighter::new(None, false),
            palette: cosense::theme::Palette::for_light(false),
            picker: Picker::halfblocks(),
            fetcher: Arc::new(ImageFetcher::new(None, None).unwrap()),
            light: false,
            terminal_bg: (24, 24, 24),
            ime_mode: cosense::ime::ImeMode::Off,
            editability: std::sync::Mutex::new(HashMap::new()),
            project_themes: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn page(texts: &[&str]) -> App {
        let mut app = App::new("proj".into());
        app.title = "t".into();
        app.editable = true;
        // Most tests predate the capability split and care about the render
        // pipeline, not the gate: give them a session that may draw. The
        // gate's own behaviour is tested explicitly further down.
        app.render_policy = capability::RenderPolicy::Auto;
        app.caps.sid = true;
        app.caps.visibility = capability::Visibility::Private;
        app.lines = texts
            .iter()
            .enumerate()
            .map(|(i, t)| PageLine {
                id: format!("id{i}"),
                text: t.to_string(),
                user_id: String::new(),
                created: 0,
                updated: 0,
            })
            .collect();
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let r = render_lines_with(&owned, None, &cosense::theme::Palette::for_light(false));
        app.blocks = r.blocks;
        app.srcs = r.srcs;
        app
    }

    // ---------------------------------------------------------------
    // Live-update plan: the poller's interval follows a TYPED push state.
    // ---------------------------------------------------------------

    #[test]
    fn a_stale_sid_polls_fast_from_the_first_second() {
        let mut app = page(&["a"]);
        app.ws_attempted = true; // a sid exists...
        app.status = "auth: pat · edit enabled · sync: poll · ? help".into();
        // ...but nothing has joined a room, so the plan is still the fast one.
        assert_eq!(app.sync_state, SyncState::Polling);
        assert_eq!(app.sync_state.poll_interval(), Duration::from_secs(3));
        // A sid that never works keeps failing; each failure re-asserts fast.
        app.set_sync_state(SyncState::Reconnecting);
        assert_eq!(
            app.poll_ctrl_rx_for_test().recv().unwrap(),
            Duration::from_secs(3)
        );
        assert!(app.status.contains("sync: reconnecting"));
    }

    #[test]
    fn only_a_proven_room_relaxes_the_poll_and_a_drop_tightens_it_again() {
        let mut app = page(&["a"]);
        app.ws_attempted = true;
        app.status = "auth: sid · edit enabled · sync: poll · ? help".into();
        app.set_sync_state(SyncState::Live);
        assert!(app.status.contains("sync: ws"));
        app.set_sync_state(SyncState::Reconnecting);
        assert!(app.status.contains("sync: reconnecting"));
        let rx = app.poll_ctrl_rx_for_test();
        assert_eq!(rx.recv().unwrap(), Duration::from_secs(60));
        // The drop is delivered as its own message, so the sleeping poller
        // wakes on it instead of finishing a 60 s nap in silence.
        assert_eq!(rx.recv().unwrap(), Duration::from_secs(3));
    }

    #[test]
    fn a_session_without_a_sid_is_never_called_reconnecting() {
        let mut app = page(&["a"]);
        app.ws_attempted = false;
        app.set_sync_state(SyncState::Reconnecting);
        assert_eq!(app.sync_label(), "poll");
    }

    #[test]
    fn speeding_up_the_poll_fetches_at_once_slowing_down_does_not() {
        let (tx, rx) = mpsc::channel::<Duration>();
        // Slow -> fast: the push channel just died; close the gap now.
        tx.send(Duration::from_secs(3)).unwrap();
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(60)),
            Some((Duration::from_secs(3), true))
        );
        // Fast -> slow: the room just delivered a catch-up; nothing to fetch.
        tx.send(Duration::from_secs(60)).unwrap();
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(3)),
            Some((Duration::from_secs(60), false))
        );
    }

    #[test]
    fn a_pile_of_state_changes_collapses_to_the_last_one() {
        let (tx, rx) = mpsc::channel::<Duration>();
        // Flap during one sleep: live, dropped, live again.
        for d in [60, 3, 60] {
            tx.send(Duration::from_secs(d)).unwrap();
        }
        // Only the world as it now IS matters; the interval ends slow and
        // no fetch is forced, because the channel came back up.
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(60)),
            Some((Duration::from_secs(60), false))
        );
    }

    #[test]
    fn the_poller_stops_when_the_app_is_gone() {
        let (tx, rx) = mpsc::channel::<Duration>();
        drop(tx);
        assert_eq!(absorb_interval(&rx, Duration::from_secs(60)), None);
    }

    // ---------------------------------------------------------------
    // Web renderer (Mermaid). The browser itself is never launched here:
    // `FakeBackend` stands in at the request→artifact boundary.
    // ---------------------------------------------------------------

    /// A 4×4 PNG — the smallest thing `image` will decode for us.
    fn tiny_png() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn scratch_cache() -> cosense::webrender::ArtifactCache {
        let dir = std::env::temp_dir().join(format!(
            "cosense-tui-test-webcache-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        cosense::webrender::ArtifactCache::at(dir)
    }

    /// The `(gen, reqs)` of a queued render job — tests only ever queue
    /// render jobs, and a rescale would be a test bug.
    fn render_job(job: WebJob) -> (u64, Vec<WebRequest>) {
        match job {
            WebJob::Render { gen, reqs, .. } => (gen, reqs),
            WebJob::Rescale { key, .. } => panic!("expected a render job, got a rescale of {key}"),
            WebJob::Stop => panic!("expected a render job, got Stop"),
        }
    }

    /// A page with two Mermaid blocks and one ordinary code block.
    fn mermaid_page() -> App {
        let mut app = page(&[
            "t",           // 0
            "code:mmd",    // 1
            " flowchart LR", // 2
            "   A-->B",    // 3  <- preview hangs here
            "code:js",     // 4
            " let a = 1",  // 5
            "code:two.mmd", // 6
            " pie",        // 7  <- and here
        ]);
        app.page_id = "PAGE".into();
        app
    }

    #[test]
    fn each_mermaid_block_is_requested_against_its_own_cosense_line_id() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let rx = app.web_jobs_rx.take().unwrap();
        let (gen, reqs) = render_job(rx.try_recv().expect("one batch was queued"));
        assert_eq!(gen, app.gen_now());
        let ids: Vec<&str> = reqs.iter().map(|r| r.line_id.as_str()).collect();
        // The LAST content line of each block — not the `code:` header, and
        // nothing at all for the js block.
        assert_eq!(ids, vec!["id3", "id7"]);
        assert_eq!(
            reqs[0].selector(),
            "#mermaid-preview-id3",
            "the selector addresses Cosense's own preview element"
        );
        assert_ne!(reqs[0].cache_key(), reqs[1].cache_key());
        assert!(reqs.iter().all(|r| r.page_id == "PAGE"));
        // Asking again while they are in flight queues nothing new.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(rx.try_recv().is_err(), "no duplicate batch for pending keys");
    }

    #[test]
    fn only_the_diagrams_own_source_invalidates_it() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = |a: &App, code: &str| {
            a.web_request(cosense::webrender::WebKind::Mermaid, code, 3).unwrap().cache_key()
        };
        let before = key(&app, "flowchart");
        // Someone (or the reader) commits elsewhere on the page: pressing
        // Enter for a new line does exactly this. The diagram is untouched,
        // so it must NOT be re-rendered — keying on the page commit used to
        // re-render every diagram on the page for each such edit.
        app.ws_head = Some("COMMIT2".into());
        assert_eq!(before, key(&app, "flowchart"), "an unrelated commit changes nothing");
        // Editing the diagram itself does invalidate it.
        assert_ne!(before, key(&app, "flowchart LR"));
        // A pane resize does NOT. The viewer scales every image to a cell
        // width of its own, so the browser viewport only decides raster
        // quality — re-rendering (3-6s) on a window drag bought nothing and
        // queued one batch per width crossed.
        app.rebuild(140);
        assert_eq!(before, key(&app, "flowchart"));
        // With the page's diagrams already requested once, resizing queues
        // no further work at all.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok(), "the initial batch");
        for w in [60, 100, 200, 37] {
            app.rebuild(w);
            app.start_web_renders(capability::Trigger::Auto);
            assert!(
                app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
                "resizing to {w} columns must queue nothing",
            );
        }
        // …and a snapshot of an older page is never rendered from the web.
        app.time = Some(TimeMachine { points: vec![], pos: 0, cache: HashMap::new() });
        assert!(app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart", 3)
            .is_none());
    }

    #[test]
    fn an_open_session_only_holds_back_the_block_under_the_caret() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let session_on = |app: &mut App, line: usize| {
            app.session = Some(EditSession {
                line,
                input: Input::new(app.lines[line].text.clone()),
                orig: app.lines[line].text.clone(),
                want_col: None,
            });
        };
        // The caret is inside the FIRST diagram (lines 1..=3): that one is
        // still being typed, but the second diagram has nothing to wait for.
        session_on(&mut app, 2);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) =
            render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().expect("the other block goes"));
        assert_eq!(
            reqs.iter().map(|r| r.line_id.as_str()).collect::<Vec<_>>(),
            vec!["id7"],
            "only the block the caret is NOT in",
        );

        // The caret moves off the diagram — every caret move commits the
        // dirty line first, so the block is now renderable. (Queueing a
        // batch invalidates the layout so the pulse can appear, and the
        // next request goes out after the frame that rebuilds it.)
        app.rebuild(80);
        session_on(&mut app, 5);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        assert_eq!(
            reqs.iter().map(|r| r.line_id.as_str()).collect::<Vec<_>>(),
            vec!["id3"],
            "the block the caret just left starts rendering, session still open",
        );
    }

    #[test]
    fn typing_never_launches_a_browser() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // A commit is still on its way to the server: the browser would see
        // the pre-edit page, so nothing is requested at all.
        app.inflight = 1;
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
        assert!(app.web_pending.is_empty());
        // Settled: one batch goes out, against text the server now has.
        app.inflight = 0;
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok());
    }

    #[test]
    fn a_result_for_an_older_page_generation_is_dropped() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let req = app.web_request(cosense::webrender::WebKind::Mermaid, "flowchart", 3).unwrap();
        let key = req.cache_key();
        app.web_pending.insert(key.clone());
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        // The reader navigated away and back while the browser was busy.
        let stale_gen = app.gen_now().wrapping_sub(1);
        app.web_tx.send(WebMsg { gen: stale_gen, key: key.clone(), rescale: false, res: WebOutcome::Drawn(info) }).unwrap();
        assert!(!app.drain_web_renders(), "a stale result changes nothing");
        assert!(!app.images.contains_key(&key), "yesterday's diagram is not installed");
        // …and it touches NO state. The same key can legitimately be
        // pending again for the current page, so clearing by key alone
        // would cancel that live request. What keeps pending from leaking
        // is `set_page` — see
        // `set_page_clears_the_state_a_dropped_job_would_have_answered`.
        assert!(app.web_pending.contains(&key));
    }

    #[test]
    fn a_renderer_failure_leaves_the_code_block_on_screen() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        app.web_pending.insert(key.clone());
        app.web_tx
            .send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, res: WebOutcome::Failed(WebError::NoBrowser.to_string()) })
            .unwrap();
        assert!(app.drain_web_renders());
        app.laid_width = 0;
        app.rebuild(80);
        let text: Vec<String> = app
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, .. } => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(text.iter().any(|t| t.contains("code:mmd")));
        assert!(text.iter().any(|t| t.contains("flowchart LR")));
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Image { .. })));
        assert!(
            app.hint_text(&[]).contains("showing source"),
            "and the reader is told: {}",
            app.hint_text(&[]),
        );
    }

    #[test]
    fn an_artifact_replaces_the_code_block_and_edit_puts_it_back() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.web_tx.send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, res: WebOutcome::Drawn(info) }).unwrap();
        assert!(app.drain_web_renders());
        app.laid_width = 0;
        app.rebuild(80);
        // The first block draws as a picture, the second is still code.
        assert!(app.rows.iter().any(|r| matches!(r, Row::Image { url, src, .. } if *url == key && *src == 3)));
        let code_rows = |app: &App| {
            app.rows
                .iter()
                .filter_map(|r| match r {
                    Row::Line { line, .. } => {
                        Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(!code_rows(&app).iter().any(|t| t.contains("flowchart LR")));

        // Opening the edit session on a line of that block hands the raw
        // source back: editing always addresses the text, never the picture.
        app.session = Some(EditSession {
            line: 2,
            input: Input::new("  flowchart LR".into()),
            orig: "  flowchart LR".into(),
            want_col: None,
        });
        app.laid_width = 0;
        app.rebuild(80);
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Image { .. })));
        assert!(code_rows(&app).iter().any(|t| t.contains("flowchart LR")));
    }

    // ---------------------------------------------------------------
    // Capability fallback: what a session without a usable `connect.sid`
    // may still do. The REST side must be untouched by ALL of it.
    // ---------------------------------------------------------------

    /// Wire a page to a worker and a fake backend, and run one pass.
    /// Returns the backend so the test can count what it was asked to do.
    fn run_pass(
        app: &mut App,
        trigger: capability::Trigger,
    ) -> Arc<cosense::webrender::FakeBackend> {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
        );
        app.start_web_renders(trigger);
        backend
    }

    /// The page's rendered text rows, for "the source is still on screen".
    fn text_rows(app: &App) -> Vec<String> {
        app.rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, .. } => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect()
    }

    /// Every key this page's diagrams will be filed under.
    fn diagram_keys(app: &App) -> Vec<String> {
        let mut out = Vec::new();
        for b in &app.blocks {
            if let Block::WebRender { kind, code, last_src, .. } = b {
                if let Some(r) = app.web_request(*kind, code, *last_src) {
                    out.push(r.cache_key());
                }
            }
        }
        out
    }

    /// Wait for one reply per key, then apply them all. Collected first
    /// because `drain_web_renders` empties the whole channel.
    fn settle(app: &mut App, n: usize) {
        let mut msgs = Vec::new();
        for _ in 0..n {
            msgs.push(
                app.web_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("every accepted request is answered exactly once"),
            );
        }
        for m in msgs {
            app.web_tx.send(m).unwrap();
        }
        app.drain_web_renders();
    }

    // ---------------------------------------------------------------
    // Render policy: when a browser is allowed to start at all.
    // ---------------------------------------------------------------

    #[test]
    fn by_default_opening_a_page_shows_cached_diagrams_and_starts_no_browser() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the default page load must never launch a browser"
        );
        // Both are misses, and misses are not failures.
        assert_eq!(app.web_missing.len(), n);
        assert!(app.web_errors.is_empty());
        assert!(app.web_pending.is_empty());
        assert!(app.web_notice.is_none(), "an empty cache is not worth a notice");
    }

    #[test]
    fn m_draws_the_diagrams_the_page_load_could_not() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let keys = diagram_keys(&app);
        settle(&mut app, keys.len());
        assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The reader presses `m`.
        for k in &keys {
            backend.answer(k, Ok(tiny_png()));
        }
        handle_key(&mut app, &test_ctx(), key(KeyCode::Char('m')));
        settle(&mut app, keys.len());
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one batch, not one browser per diagram"
        );
        assert!(keys.iter().all(|k| app.images.contains_key(k)));
        assert!(app.web_missing.is_empty(), "they are no longer missing");
    }

    #[test]
    fn editing_a_drawn_diagram_redraws_it_without_asking_again() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let first = diagram_keys(&app);
        // The reader pressed `m` once and got a picture.
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        for k in &first {
            backend.answer(k, Ok(tiny_png()));
        }
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
        );
        app.start_web_renders(capability::Trigger::Manual);
        settle(&mut app, first.len());
        assert!(app.images.contains_key(&first[0]), "the diagram is on screen");
        app.start_web_renders(capability::Trigger::Auto);

        // Now they edit that block's source. The picture they were looking
        // at is now stale, and its key has moved with the source.
        app.lines[3].text = "   A-->C".into();
        rerender(&mut app, &ctx);
        let after = diagram_keys(&app);
        assert_ne!(after[0], first[0], "the edit is a different artifact");
        backend.answer(&after[0], Ok(tiny_png()));
        settle(&mut app, 1);
        assert!(
            app.images.contains_key(&after[0]),
            "a diagram the reader is watching must follow its source without a second `m`"
        );
    }

    #[test]
    fn a_committed_edit_redraws_the_diagram_the_reader_was_watching() {
        // The shape the app actually runs: leaving the block COMMITS it, so
        // the re-render that `rerender` attempts happens while the commit is
        // still in flight and is refused. Whatever redraws the diagram has
        // to happen after the commit lands, on an ordinary frame.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let first = diagram_keys(&app);
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(first[0].clone(), info);
        // One ordinary frame notices the diagram is on screen.
        app.start_web_renders(capability::Trigger::Auto);
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();

        // The reader edits the block and moves the caret off it: the line is
        // committed, and the re-render lands mid-flight.
        app.lines[3].text = "   A-->C".into();
        app.inflight = 1;
        rerender(&mut app, &ctx);
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "nothing may be rendered while the server has not got the edit yet"
        );

        // The commit lands. From here the server agrees with the screen.
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done { label: "line 4".into(), title: String::new() },
        );
        assert_eq!(app.inflight, 0);
        app.start_web_renders(capability::Trigger::Auto);

        let after = diagram_keys(&app);
        assert_ne!(after[0], first[0], "the edit is a different artifact");
        let job = app
            .web_jobs_rx
            .as_ref()
            .unwrap()
            .try_recv()
            .expect("the edited diagram must be redrawn once the commit lands");
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert!(reqs.iter().any(|r| r.cache_key() == after[0]));
        assert!(auth.is_some(), "a refresh must be allowed to reach the browser");
    }

    #[test]
    fn adding_a_line_to_a_drawn_diagram_still_redraws_it() {
        // Pressing Enter inside a Mermaid block is the ordinary edit. It
        // moves the block's LAST line, which is the line the request is
        // keyed on.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let first = diagram_keys(&app);
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(first[0].clone(), info);
        app.start_web_renders(capability::Trigger::Auto);
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();

        // A new line at the end of the block.
        app.lines.insert(
            4,
            PageLine {
                id: "id-new".into(),
                text: "   B-->C".into(),
                user_id: String::new(),
                created: 0,
                updated: 0,
            },
        );
        app.inflight = 1;
        rerender(&mut app, &ctx);
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done { label: "line 5".into(), title: String::new() },
        );
        app.start_web_renders(capability::Trigger::Auto);

        let after = diagram_keys(&app);
        let job = app
            .web_jobs_rx
            .as_ref()
            .unwrap()
            .try_recv()
            .expect("adding a line to a diagram must redraw it too");
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert!(reqs.iter().any(|r| r.cache_key() == after[0]));
        assert!(auth.is_some());
    }

    #[test]
    fn a_cache_miss_never_blocks_the_later_m() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        // A miss is recorded...
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                res: WebOutcome::Missing,
            })
            .unwrap();
        app.drain_web_renders();
        assert!(!app.web_errors.contains_key(&keys[0]), "a miss is not an error");
        // ...and `m` still asks for it.
        app.start_web_renders(capability::Trigger::Manual);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().recv().unwrap());
        assert!(reqs.iter().any(|r| r.cache_key() == keys[0]));
    }

    #[test]
    fn auto_draws_on_load_and_off_draws_never() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().recv().unwrap());
        assert_eq!(reqs.len(), 2, "auto renders both on load");

        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Off;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.start_web_renders(capability::Trigger::Manual);
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "off queues no work at all — not even a cache lookup"
        );
        assert!(app.web_pending.is_empty());
    }

    #[test]
    fn m_is_an_ordinary_character_while_editing() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        enter_session(&mut app, &ctx, 0, 1);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        let s = app.session.as_ref().expect("still editing");
        assert!(s.input.buf.contains('m'), "m typed a character, not a render");
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn no_sid_on_a_public_project_renders_anonymously_and_leaves_rest_alone() {
        let mut app = mermaid_page();
        app.editable = true;
        app.caps = capability::Capabilities {
            sid: false,
            visibility: capability::Visibility::Public,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        // The browser WAS used, with no cookie.
        assert!(backend.calls.load(std::sync::atomic::Ordering::SeqCst) > 0);
        assert_eq!(
            *backend.last_auth.lock().unwrap(),
            Some(RenderCapability::Anonymous)
        );
        // ...and nothing about the missing sid touched the REST side.
        assert!(app.editable, "no sid is not a reason to stop editing");
        assert_eq!(app.sync_label(), "poll");
    }

    #[test]
    fn no_sid_on_a_private_project_serves_the_cache_and_never_launches_a_browser() {
        let mut app = mermaid_page();
        app.editable = true; // a PAT / service account is reading this page
        app.caps = capability::Capabilities {
            sid: false,
            visibility: capability::Visibility::Private,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        // One diagram is already on disk from an earlier, authenticated
        // session. Reaching this page needed REST access, so showing it is
        // not a leak — the cache itself is 0700/0600.
        let cache = scratch_cache();
        cache.put(&keys[0], &tiny_png());
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            cache,
            Arc::clone(&app.web_gen),
        );
        app.start_web_renders(capability::Trigger::Auto);
        settle(&mut app, keys.len());
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a private project with no sid must not start a browser"
        );
        assert!(app.images.contains_key(&keys[0]), "the cached diagram still shows");
        // The uncached one is a miss, NOT a failure: it stays source and
        // stays drawable.
        assert!(app.web_missing.contains(&keys[1]));
        assert!(!app.web_errors.contains_key(&keys[1]));
        assert!(app.web_pending.is_empty(), "nothing is left pulsing");
        assert!(app.hint_text(&[]).contains("connect.sid"));
        assert!(app.editable, "the renderer's limits never reach the editor");
    }

    #[test]
    fn the_private_notice_is_said_once_per_page_not_once_per_diagram() {
        let mut app = mermaid_page();
        app.caps.sid = false;
        app.caps.visibility = capability::Visibility::Private;
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_notice_shown);
        app.web_notice = None;
        // A second pass over the same page says nothing more.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_notice.is_none());
    }

    #[test]
    fn unknown_visibility_waits_for_an_explicit_m() {
        let mut app = mermaid_page();
        app.caps.sid = false;
        app.caps.visibility = capability::Visibility::Unknown;
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an automatic pass never gambles a browser on an unknown project"
        );
        // The reader asks explicitly: one anonymous attempt is allowed.
        app.start_web_renders(capability::Trigger::Manual);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert!(backend.calls.load(std::sync::atomic::Ordering::SeqCst) > 0);
        assert_eq!(
            *backend.last_auth.lock().unwrap(),
            Some(RenderCapability::Anonymous)
        );
        assert!(app.caps.anonymous_spent);
    }

    #[test]
    fn a_stale_cookie_on_a_public_page_retries_without_it() {
        let mut app = mermaid_page();
        app.caps = capability::Capabilities {
            sid: true,
            visibility: capability::Visibility::Public,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                res: WebOutcome::Denied,
            })
            .unwrap();
        app.drain_web_renders();
        // The refusal was about the COOKIE. A second, cookie-free attempt
        // is queued, and the REST credential is not touched.
        assert!(app.caps.anonymous_spent);
        assert!(!app.caps.browser_denied);
        let job = app.web_jobs_rx.as_ref().unwrap().recv().unwrap();
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert!(reqs.iter().any(|r| r.cache_key() == keys[0]));
        assert_eq!(
            *auth,
            Some(RenderCapability::Anonymous),
            "retrying with the SAME rejected cookie just fails again"
        );
    }

    #[test]
    fn a_refused_private_render_falls_back_to_source_without_disabling_edits() {
        let mut app = mermaid_page();
        app.editable = true;
        app.caps = capability::Capabilities {
            sid: true,
            visibility: capability::Visibility::Private,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                res: WebOutcome::Denied,
            })
            .unwrap();
        app.drain_web_renders();
        assert!(app.caps.browser_denied, "the RENDERER gives up");
        assert!(app.editable, "...and nothing else does");
        assert!(!app.web_errors.contains_key(&keys[0]));
        app.rebuild(80);
        assert!(
            text_rows(&app).iter().any(|t| t.contains("flowchart LR")),
            "the source is what the reader sees"
        );
        // No further BROWSER work is queued for this project: even an
        // explicit `m` can now only consult the disk cache.
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();
        app.start_web_renders(capability::Trigger::Manual);
        if let Ok(WebJob::Render { auth, .. }) = app.web_jobs_rx.as_ref().unwrap().try_recv() {
            assert_eq!(auth, None, "a denied project never reaches a browser again");
        }
    }

    #[test]
    fn moving_to_another_project_forgets_the_old_one_s_verdict() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.caps.visibility = capability::Visibility::Private;
        app.caps.browser_denied = true;
        app.caps.sid = true;
        app.set_page(
            Loaded {
                project: "other".into(),
                title: "t".into(),
                page_id: "P2".into(),
                header_colors: HeaderColors::fallback(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                related: Vec::new(),
                read_at: None,
                editable: true,
            },
            &ctx,
        );
        assert_eq!(app.caps.visibility, capability::Visibility::Unknown);
        assert!(!app.caps.browser_denied);
        assert!(app.caps.sid, "the session's cookie did not go anywhere");
    }

    #[test]
    fn the_ui_thread_never_waits_for_the_browser() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        backend.answer(&key, Ok(tiny_png()));
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
        );
        // Pin the backend mid-render: the worker cannot make progress while
        // this guard is held.
        let held = backend.gate.lock().unwrap();
        let t0 = std::time::Instant::now();
        app.start_web_renders(capability::Trigger::Auto);
        // …and the UI thread still lays out and would draw, immediately.
        app.laid_width = 0;
        app.rebuild(80);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "queueing a render must not block the UI (took {elapsed:?})"
        );
        assert!(app.web_pending.contains(&key));
        assert!(app.images.is_empty(), "nothing is drawn until the browser answers");
        // Let the worker through; the artifact arrives on the channel.
        drop(held);
        let msg = app
            .web_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the worker answered");
        assert_eq!(msg.gen, app.gen_now());
        assert_eq!(msg.key, key);
        assert!(msg.res.is_drawn());
    }

    #[test]
    fn a_rendering_diagram_pulses_its_code_and_stops_when_it_lands() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.rebuild(80);
        // Every row of both Mermaid blocks pulses — header and body — and
        // nothing else on the page does.
        let mut srcs: Vec<usize> = app.web_shimmer.keys().copied().collect();
        srcs.sort();
        assert_eq!(srcs, vec![1, 2, 3, 6, 7]);
        assert_eq!(app.web_shimmer[&1], (0, 3), "the code: header leads its block");
        assert_eq!(app.web_shimmer[&3], (2, 3));
        assert_eq!(app.web_shimmer[&7], (1, 2), "the second block counts from its own top");

        let dim_cells = |app: &mut App, ctx: &Ctx| {
            let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
            terminal.draw(|f| ui(f, app, ctx)).unwrap();
            terminal.draw(|f| ui(f, app, ctx)).unwrap();
            let buf = terminal.backend().buffer();
            (0..buf.area.width)
                .flat_map(|x| (0..buf.area.height).map(move |y| (x, y)))
                .filter(|(x, y)| {
                    buf.cell((*x, *y)).unwrap().modifier.contains(Modifier::DIM)
                })
                .count()
        };
        assert!(dim_cells(&mut app, &ctx) > 0, "the reader can see the renderer working");

        // The renders land: the pulse stops and the page goes back to normal.
        let keys: Vec<String> = app.web_pending.iter().cloned().collect();
        for key in keys {
            app.web_pending.remove(&key);
        }
        app.laid_width = 0;
        app.rebuild(80);
        assert!(app.web_shimmer.is_empty());
        assert_eq!(dim_cells(&mut app, &ctx), 0, "nothing pulses once nothing is pending");
    }

    /// A wide PNG, like a real Mermaid capture (they come back ~1500px).
    fn wide_png() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1500, 300, |x, _| {
            image::Rgb([if x > 1400 { 255 } else { 40 }, 40, 40])
        }));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    /// Run the worker against a scripted backend and hand back everything
    /// it replies with, using the channel itself as the synchronisation —
    /// no sleeps, no wall-clock assumptions.
    fn worker_replies(
        app: &mut App,
        backend: Arc<cosense::webrender::FakeBackend>,
        cache: cosense::webrender::ArtifactCache,
        jobs: Vec<WebJob>,
        expect: usize,
    ) -> Vec<WebMsg> {
        let handle = spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            cache,
            Arc::clone(&app.web_gen),
        );
        for job in jobs {
            app.web_job_tx.send(job).unwrap();
        }
        let mut out = Vec::new();
        for _ in 0..expect {
            out.push(
                app.web_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("the worker owes a reply"),
            );
        }
        app.web_job_tx.send(WebJob::Stop).unwrap();
        handle.join().expect("the worker joins on Stop");
        out
    }

    #[test]
    fn a_failed_commit_stops_the_server_s_old_diagram_being_filed_under_the_new_source() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        // The reader changes the diagram A -> B locally. The key follows
        // the LOCAL text…
        app.lines[3].text = "   A-->C".into();
        rerender(&mut app, &ctx);
        app.rebuild(80);
        let key_b = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->C", 3)
            .unwrap()
            .cache_key();
        // Clear whatever the re-render already queued, and forget it was
        // ever pending: the question is what happens from here on.
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();
        // …but the commit is refused, so the SERVER still has A. Rendering
        // now would screenshot A and store it under B's key.
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Failed { label: "line 4".into(), msg: "500".into() },
        );
        assert_eq!(app.inflight, 0, "the job is no longer in flight…");
        assert!(app.web_unsynced, "…but the page is known to have drifted");
        app.start_web_renders(capability::Trigger::Auto);
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "inflight == 0 is not enough: nothing may be rendered while desynced",
        );
        assert!(!app.images.contains_key(&key_b));
        assert!(app.web_pending.is_empty());

        // A later commit succeeding says nothing about the one that failed.
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done { label: "line 9".into(), title: String::new() },
        );
        assert!(app.web_unsynced, "one success does not prove the page agrees");
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());

        // Only a whole page from the server settles it.
        let mut page = cosense::api::Page {
            id: app.page_id.clone(),
            title: app.title.clone(),
            commit_id: String::new(),
            lines: vec![],
            links: vec![],
            project_links: vec![],
            related: None,
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        page.lines = app
            .lines
            .iter()
            .map(|l| PageLine {
                id: l.id.clone(),
                text: l.text.clone(),
                user_id: String::new(),
                created: 0,
                updated: 0,
            })
            .collect();
        install_remote_lines(&mut app, &ctx, &page, "⟳ resync");
        assert!(!app.web_unsynced, "an authoritative install resolves the drift");
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok(), "rendering resumes");
    }

    #[test]
    fn an_edit_that_never_reached_the_commit_worker_also_desyncs() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // The worker is gone, so the send fails exactly as it does at exit.
        drop(app.commit_jobs_rx.take());
        queue_commit(&mut app, "line 4", vec![]);
        assert_eq!(app.inflight, 0, "nothing is in flight — it never left");
        assert!(app.web_unsynced);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn the_worker_joins_on_stop_and_answers_every_accepted_job() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        backend.answer(&key, Ok(tiny_png()));
        let gen = app.gen_now();
        let req = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        let msgs = worker_replies(
            &mut app,
            Arc::clone(&backend),
            scratch_cache(),
            vec![
                WebJob::Render {
                    gen,
                    reqs: vec![req],
                    max_cols: 64,
                    auth: Some(RenderCapability::Authenticated),
                },
                // A rescale with nothing on disk STILL owes a reply, or the
                // viewer would wait on it forever.
                WebJob::Rescale { gen, key: "web:mermaid:absent".into(), max_cols: 20 },
            ],
            2,
        );
        assert_eq!(msgs.len(), 2, "one reply per accepted job");
        let render = msgs.iter().find(|m| m.key == key).unwrap();
        assert!(!render.rescale && render.res.is_drawn());
        let rescale = msgs.iter().find(|m| m.key == "web:mermaid:absent").unwrap();
        assert!(rescale.rescale && !rescale.res.is_drawn(), "a cache miss is answered, not dropped");
    }

    #[test]
    fn a_rescale_that_cannot_be_served_keeps_the_diagram_it_has() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key.clone(), info);
        app.web_rescaling.insert(key.clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: key.clone(),
                rescale: true,
                res: WebOutcome::Failed("no cached artifact to resize".into()),
            })
            .unwrap();
        app.drain_web_renders();
        assert!(!app.web_rescaling.contains(&key), "it stops being in flight");
        assert!(app.images.contains_key(&key), "the picture on screen survives");
        assert!(app.web_errors.is_empty(), "a resize failure is not a diagram failure");
        assert!(app.web_notice.is_none(), "and the reader is not told about it");
    }

    #[test]
    fn a_stale_result_never_cancels_the_live_request_for_the_same_key() {
        // Navigate away and back: the same diagram is pending again under
        // the same key, but for a NEW generation. The old page's result
        // must not clear that.
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (old_gen, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        app.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        app.web_pending.insert(key.clone());
        app.web_rescaling.insert(key.clone());
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.web_tx
            .send(WebMsg { gen: old_gen, key: key.clone(), rescale: false, res: WebOutcome::Drawn(info) })
            .unwrap();
        assert!(!app.drain_web_renders(), "yesterday's answer changes nothing");
        assert!(app.web_pending.contains(&key), "the live request is still in flight");
        assert!(app.web_rescaling.contains(&key));
        assert!(!app.images.contains_key(&key), "and no stale picture is installed");
    }

    #[test]
    fn set_page_clears_the_state_a_dropped_job_would_have_answered() {
        // The worker drops stale-generation jobs without replying, which is
        // only safe because installing a page clears what those replies
        // would have cleared. This pins that invariant.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.web_rescaling.insert("web:mermaid:whatever".into());
        assert!(!app.web_pending.is_empty());
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "next".into(),
                header_colors: HeaderColors::fallback(),
                page_id: "p2".into(),
                lines: vec![],
                blocks: vec![],
                srcs: vec![],
                read_at: None,
                editable: true,
                related: Vec::new(),
            },
            &ctx,
        );
        assert!(app.web_pending.is_empty(), "no request outlives the page it was for");
        assert!(app.web_rescaling.is_empty());
    }

    #[test]
    fn work_queued_for_a_page_the_reader_left_never_reaches_the_browser() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let stale_gen = app.gen_now();
        // The reader moves on before the worker gets to it.
        app.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let live_gen = app.gen_now();
        let req = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        backend.answer(&req.cache_key(), Ok(tiny_png()));
        let msgs = worker_replies(
            &mut app,
            Arc::clone(&backend),
            scratch_cache(),
            vec![
                WebJob::Render { gen: stale_gen, reqs: vec![req.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) },
                WebJob::Rescale { gen: stale_gen, key: "web:mermaid:old".into(), max_cols: 20 },
                WebJob::Render { gen: live_gen, reqs: vec![req.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) },
            ],
            1,
        );
        assert_eq!(msgs.len(), 1, "only the live page is answered");
        assert_eq!(msgs[0].gen, live_gen);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the abandoned page cost no browser navigation",
        );
    }

    #[test]
    fn several_passes_over_one_page_cost_a_single_browser_batch() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let gen = app.gen_now();
        let a = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        let b = app.web_request(cosense::webrender::WebKind::Mermaid, "pie", 7).unwrap();
        backend.answer(&a.cache_key(), Ok(tiny_png()));
        backend.answer(&b.cache_key(), Ok(tiny_png()));
        // Hold the worker until BOTH jobs are queued, so it sees them
        // together — the gate, not a sleep, is what makes this repeatable.
        let held = backend.gate.lock().unwrap();
        let handle = spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
        );
        app.web_job_tx
            .send(WebJob::Render { gen, reqs: vec![a.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) })
            .unwrap();
        app.web_job_tx
            .send(WebJob::Render { gen, reqs: vec![b.clone(), a.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) })
            .unwrap();
        drop(held);
        let mut keys = Vec::new();
        for _ in 0..2 {
            keys.push(
                app.web_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap()
                    .key,
            );
        }
        app.web_job_tx.send(WebJob::Stop).unwrap();
        handle.join().unwrap();
        keys.sort();
        let mut want = vec![a.cache_key(), b.cache_key()];
        want.sort();
        assert_eq!(keys, want, "each request answered exactly once, no duplicates");
    }

    #[test]
    fn a_send_to_a_dead_worker_stops_the_pulse_instead_of_hanging_it() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // No worker was ever spawned and the receiver is dropped: every
        // send fails, exactly as it does once the worker has stopped.
        drop(app.web_jobs_rx.take());
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_pending.is_empty(), "nothing waits on a reply that cannot come");
        app.rebuild(80);
        assert!(app.web_shimmer.is_empty(), "so nothing pulses");

        // Same for a resize.
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key.clone(), info);
        app.laid_width = 0;
        app.rebuild(30);
        app.rescale_diagrams();
        assert!(app.web_rescaling.is_empty());
    }

    #[test]
    fn a_diagram_is_never_encoded_wider_than_the_pane() {
        // The cap itself: the pane wins while it is narrower than the
        // image ceiling, and never goes to zero.
        assert_eq!(diagram_max_cols(120), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(IMAGE_MAX_COLS), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(35), 35);
        assert_eq!(diagram_max_cols(14), 14);
        assert_eq!(diagram_max_cols(0), 1);

        // …and the encoder honours it for an image far wider than any pane.
        let picker = Picker::halfblocks();
        for cap in [IMAGE_MAX_COLS, 35, 14, 5, 1] {
            let info = decode_web_png(&picker, &wide_png(), cap).unwrap();
            assert!(
                info.cells_w <= cap,
                "cap {cap} produced {} cells wide",
                info.cells_w
            );
            assert_eq!(info.built_for, cap);
            assert!(info.cells_h >= 1);
        }
    }

    #[test]
    fn narrowing_the_pane_re_encodes_the_diagram_from_its_cached_png() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        // The artifact arrives sized for the wide pane.
        let wide = decode_web_png(&Picker::halfblocks(), &wide_png(), app.web_cols).unwrap();
        assert_eq!(app.web_cols, IMAGE_MAX_COLS, "80 columns leaves room for the ceiling");
        app.web_pending.insert(key.clone());
        app.web_tx.send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, res: WebOutcome::Drawn(wide) }).unwrap();
        assert!(app.drain_web_renders());

        // Two narrow panes, well under the 64-column image ceiling.
        for (cols, want_cap) in [(40u16, 34u16), (20, 14)] {
            app.laid_width = 0;
            app.rebuild(cols);
            assert_eq!(app.web_cols, want_cap, "text width at {cols} columns");
            app.rescale_diagrams();
            // The UI thread only queued work; nothing was decoded here.
            let job = app.web_jobs_rx.as_ref().unwrap().try_recv().expect("a rescale was queued");
            let WebJob::Rescale { key: k, max_cols, gen } = job else {
                panic!("a resize must never send the browser a render job")
            };
            assert_eq!((k.as_str(), max_cols, gen), (key.as_str(), want_cap, app.gen_now()));
            // Asking again while it is in flight queues nothing.
            app.rescale_diagrams();
            assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());

            // The worker answers; the diagram now fits the pane.
            let info = decode_web_png(&Picker::halfblocks(), &wide_png(), max_cols).unwrap();
            app.web_tx
                .send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: true, res: WebOutcome::Drawn(info) })
                .unwrap();
            assert!(app.drain_web_renders());
            let shown_w = app.images[&key].cells_w;
            assert!(shown_w <= want_cap, "{shown_w} cells in a {cols}-column pane");

            // And the frame really does keep it inside the text column.
            app.laid_width = 0;
            let mut t = Terminal::new(TestBackend::new(cols, 16)).unwrap();
            t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
            t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
            assert!(app.text_rect.width >= shown_w, "image fits the text column");
        }
    }

    #[test]
    fn a_diagram_failure_gives_the_key_hints_back() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        app.web_pending.insert(key.clone());
        assert!(app.hint_text(&[]).contains("? help"), "hints by default");
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key,
                rescale: false,
                res: WebOutcome::Failed(WebError::NoBrowser.to_string()),
            })
            .unwrap();
        assert!(app.drain_web_renders());
        // It never touches the status line — it has its own slot.
        assert!(app.status.is_empty());
        assert!(app.hint_text(&[]).contains("showing source"), "the reader is told once");
        // It holds the line for its few seconds…
        assert!(!app.expire_web_notice());
        // …and then hands the row back to the key hints.
        let (msg, _) = app.web_notice.clone().unwrap();
        app.web_notice = Some((msg, std::time::Instant::now()));
        assert!(app.expire_web_notice());
        assert!(app.web_notice.is_none());
        assert!(app.hint_text(&[]).contains("? help"), "hints are visible again");
        assert!(!app.expire_web_notice(), "and it stays gone");
    }

    #[test]
    fn a_diagram_note_never_hides_or_erases_a_commit_or_auth_message() {
        let mut app = mermaid_page();
        // Something the reader must not miss is already on the status line.
        app.status = "commit failed: line 4 — 500".into();
        app.note_web_failure("diagram: no Chrome found (showing source)".into());
        assert_eq!(
            app.hint_text(&[]),
            "commit failed: line 4 — 500",
            "status outranks a diagram note",
        );
        // When the note times out it takes only itself with it.
        let (msg, _) = app.web_notice.clone().unwrap();
        app.web_notice = Some((msg, std::time::Instant::now()));
        assert!(app.expire_web_notice());
        assert_eq!(app.status, "commit failed: line 4 — 500", "the real message survives");
        assert_eq!(app.hint_text(&[]), "commit failed: line 4 — 500");

        // And with the status line free, the note would have shown.
        app.note_web_failure("diagram: browser timed out (showing source)".into());
        app.status.clear();
        assert!(app.hint_text(&[]).contains("browser timed out"));
        // The edit session and cursor links still outrank both.
        let links = vec![LinkItem::Page("Somewhere".into())];
        assert!(app.hint_text(&links).contains("Enter/f open"));
    }

    #[test]
    fn wrapped_bullets_hang_and_stay_one_source_line() {
        let long = format!(" {}", "字".repeat(30)); // a 30-char bullet at level 1
        let mut app = page(&["t", &long, "after"]);
        app.rebuild(22); // text width 20
        let rows: Vec<String> = app
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src } if *src == 1 => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(rows.len() >= 3);
        assert!(rows[0].starts_with("• 字"), "{:?}", rows[0]);
        for r in &rows[1..] {
            assert!(r.starts_with("  字"), "continuation hangs under the text: {r:?}");
        }
        // still one cursor stop
        app.goto_src(1);
        assert_eq!(app.cursor_rows().map(|(a, b)| b - a + 1), Some(rows.len()));
    }

    #[test]
    fn wrapped_quotes_carry_the_bar_on_every_row() {
        let long = format!("> {}", "引".repeat(30));
        let mut app = page(&["t", &long]);
        app.rebuild(22); // text width 20
        let rows: Vec<String> = app
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src } if *src == 1 => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(rows.len() >= 3);
        for r in &rows {
            assert!(r.starts_with("┃ 引"), "{r:?}");
        }
    }

    #[test]
    fn edit_session_removes_the_ansi_colored_page_frame() {
        assert!(page_frame_visible(false));
        assert!(!page_frame_visible(true));
        let style = page_frame_style();
        assert_eq!(style.fg, Some(Color::DarkGray));
        assert_eq!(style.bg, None);
        assert!(!matches!(style.fg, Some(Color::Rgb(..))), "chrome must follow ANSI palette");
    }

    #[test]
    fn cursor_is_a_source_line_spanning_all_wrapped_rows() {
        let mut app = page(&["short", &"x".repeat(50), "tail"]);
        app.rebuild(22); // one extra chrome column leaves 16 text columns
        assert_eq!(app.cursor, 0);
        app.move_cursor(true);
        assert_eq!(app.cursor, 1);
        let (first, last) = app.cursor_rows().unwrap();
        assert_eq!((first, last), (1, 4), "all wrapped rows belong to the line");
        // one more step leaves from the LAST wrapped row straight to line 2
        app.move_cursor(true);
        assert_eq!(app.cursor, 2);
        // and back up lands on line 1 again (from its FIRST row upward: 0)
        app.move_cursor(false);
        app.move_cursor(false);
        assert_eq!(app.cursor, 0);
        // g / G address source lines
        assert_eq!(app.first_src(), Some(0));
        assert_eq!(app.last_src(), Some(2));
    }

    #[test]
    fn related_telomere_has_only_read_and_unread_states() {
        let mut visits = HashMap::new();
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 80);
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 120);
        assert!(!related_is_unread(&visits, "proj", "Page", 100));
        // No timestamp (external project link): a recorded visit is enough.
        assert!(!related_is_unread(&visits, "proj", "Page", 0));

        let (ug, us) = gutter_cell(false, None, Some(true), false);
        let (rg, rs) = gutter_cell(false, None, Some(false), false);
        assert_eq!((ug, rg), ("▏", "▏"), "read state must not encode thickness");
        assert_eq!(us.fg, Some(Color::LightBlue));
        assert_eq!(rs.fg, Some(Color::DarkGray));
    }

    /// A related section for tests: two 1-hop pages and one external.
    fn test_related() -> Vec<RelSection> {
        vec![
            RelSection {
                heading: "Links (2)".into(),
                entries: vec![
                    RelEntry {
                        item: LinkItem::Page("Alpha".into()),
                        title: "Alpha".into(),
                        desc: "first line".into(),
                        age: 0,
                        unread: true,
                    },
                    RelEntry {
                        item: LinkItem::Page("Beta".into()),
                        title: "Beta".into(),
                        desc: String::new(),
                        age: 0,
                        unread: false,
                    },
                ],
            },
            RelSection {
                heading: "External links (1)".into(),
                entries: vec![RelEntry {
                    item: LinkItem::ProjectPage { project: "other".into(), title: "Page".into() },
                    title: "/other/Page".into(),
                    desc: String::new(),
                    age: 0,
                    unread: true,
                }],
            },
        ]
    }

    #[test]
    fn related_rows_are_cursor_addressable_virtual_lines() {
        let mut app = page(&["title", "body"]);
        app.related = test_related();
        app.virtual_items = app
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
        app.rebuild(60);
        // body lines 0..=1, then virtual srcs 2..=4
        assert_eq!(app.src_count(), 5);
        assert!(app.src_rows(2).is_some(), "first related row renders");
        assert!(app.src_rows(4).is_some(), "external link row renders");
        // Direct navigation can still address the LAST related row and link.
        app.goto_src(99);
        assert_eq!(app.cursor, 4);
        assert_eq!(
            app.cursor_line_links(),
            vec![LinkItem::ProjectPage { project: "other".into(), title: "Page".into() }]
        );
        // j/k walk body → related → body seamlessly
        app.goto_src(1);
        app.move_cursor(true);
        assert_eq!(app.cursor, 2, "steps over the heading rule onto Alpha");
        assert_eq!(app.cursor_line_links(), vec![LinkItem::Page("Alpha".into())]);
        // The frame closes after the body, BEFORE the related section.
        assert_eq!(app.frame_end_top(), 2);
        let end = app.rows.iter().position(|r| matches!(r, Row::FrameEnd)).unwrap();
        assert!(app.rows[end + 1..].iter().any(|r| r.src() == Some(2)));
        // G is "bottom of page frame", not "last related row".
        app.view_h = 3;
        let _ = handle_key(&mut app, &test_ctx(), key(KeyCode::Char('G')));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.scroll, app.frame_end_scroll(app.view_h));
        // j then crosses the boundary into Links normally.
        app.move_cursor(true);
        assert_eq!(app.cursor, 2);
        // comments cannot anchor on virtual rows
        app.goto_src(3);
        assert!(app.make_comment("x".into()).is_none());
        // …but a selection overshooting into them clamps to the body
        app.selection = Some(Selection { anchor: 1, cursor: 3 });
        let c = app.make_comment("y".into()).unwrap();
        assert_eq!((c.start, c.end), (1, 1));
        // source mode hides related rows and clamps the cursor back
        app.selection = None;
        app.mode = Mode::Source;
        app.rebuild(60);
        assert_eq!(app.src_count(), 2);
        assert!(app.cursor < 2, "cursor clamped into the body");
    }

    #[test]
    fn build_related_groups_2hop_under_hubs_and_dedupes() {
        use cosense::api::{Page, RelatedPage, RelatedPages};
        let rp = |title: &str, links: &[&str]| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: title.to_lowercase(),
            descriptions: vec![],
            links_lc: links.iter().map(|s| s.to_lowercase()).collect(),
            linked: 0,
            updated: 0,
        };
        let page = Page {
            id: String::new(),
            title: "me".into(),
            commit_id: String::new(),
            lines: vec![],
            links: vec!["Hub1".into(), "Hub2".into()],
            project_links: vec!["/other/Page".into(), "/bare".into()],
            related: Some(RelatedPages {
                links1hop: vec![rp("Direct", &[])],
                links2hop: vec![
                    rp("Shared12", &["hub1", "hub2"]),
                    rp("OnlyHub2", &["hub2"]),
                    rp("Direct", &["hub1"]), // already 1-hop → dropped
                    rp("Orphan", &["gone"]), // hub no longer on the page
                ],
                has_back_links_or_icons: true,
            }),
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        let secs = build_related(&page, "proj");
        let heads: Vec<&str> = secs.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(
            heads,
            vec![
                "Links (1)",
                "Hub1 (1)",
                "Hub2 (1)",
                "2 hop links (1)",
                "External links (1)"
            ]
        );
        assert_eq!(secs[1].entries[0].title, "Shared12", "first hub claims the shared page");
        assert_eq!(secs[2].entries[0].title, "OnlyHub2");
        assert_eq!(secs[3].entries[0].title, "Orphan");
        assert_eq!(secs[4].entries[0].title, "/other/Page", "bare /project is not a page");
    }

    fn key(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn type_str(app: &mut App, ctx: &Ctx, s: &str) {
        for ch in s.chars() {
            handle_session_key(app, ctx, key(KeyCode::Char(ch)));
        }
    }
    /// Drain the (unspawned) commit queue: (label, ops) per job.
    fn drain_jobs(app: &mut App) -> Vec<(String, Vec<EditOp>)> {
        let rx = app.commit_jobs_rx.as_ref().unwrap();
        let mut out = Vec::new();
        while let Ok(j) = rx.try_recv() {
            out.push((j.label, j.ops));
        }
        out
    }

    #[test]
    fn read_only_project_never_enters_or_applies_editing() {
        let ctx = test_ctx();
        let mut app = page(&["t", "body"]);
        app.rebuild(40);
        app.editable = false;

        enter_session(&mut app, &ctx, 1, 4);
        assert!(app.session.is_none());
        assert!(app.status.contains("編集権限"));

        let before: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        open_line(&mut app, &ctx, false);
        do_edit(
            &mut app,
            &ctx,
            "forbidden",
            vec![EditOp::Replace { id: "id1".into(), text: "changed".into() }],
        );
        assert_eq!(app.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>(), before);
        assert!(drain_jobs(&mut app).is_empty());

        let ctrl_e = event::KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut app, &ctx, ctrl_e), Action::Continue));
    }

    #[test]
    fn session_edits_many_lines_without_leaving() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);

        // e-style entry on line 1, caret at end; type; ↓ commits and moves
        let end = app.lines[1].text.len();
        enter_session(&mut app, &ctx, 1, end);
        type_str(&mut app, &ctx, "!");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!");
        assert!(drain_jobs(&mut app).is_empty(), "typing alone commits nothing");
        handle_session_key(&mut app, &ctx, key(KeyCode::Down));
        assert_eq!(app.lines[1].text, "one!", "leaving the line applied it locally");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text == "one!"));

        // session continued on line 2: Enter at EOL creates line 3 and
        // KEEPS the session going; type there; Esc commits the text
        assert_eq!(app.session.as_ref().unwrap().line, 2);
        handle_session_key(&mut app, &ctx, key(KeyCode::End));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines.len(), 4, "a real line exists locally at once");
        assert_eq!(app.session.as_ref().unwrap().line, 3);
        type_str(&mut app, &ctx, "three");
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.session.is_none());
        assert_eq!(app.lines[3].text, "three");
        let jobs = drain_jobs(&mut app);
        // split (replace+insert) then the final text replace
        assert_eq!(jobs.len(), 2);
        assert!(matches!(&jobs[0].1[1], EditOp::Insert { .. }));
        assert!(matches!(&jobs[1].1[0], EditOp::Replace { text, .. } if text == "three"));
        // ids: the insert's id is the line's id — client-generated, stable
        if let EditOp::Insert { lines, .. } = &jobs[0].1[1] {
            assert_eq!(lines[0].0, app.lines[3].id);
        }
    }

    #[test]
    fn session_split_mid_line_inherits_indent() {
        let ctx = test_ctx();
        let mut app = page(&["title", " indented line"]);
        app.rebuild(40);
        // caret between "indented" and " line"
        let caret = " indented".len();
        enter_session(&mut app, &ctx, 1, caret);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[1].text, " indented");
        assert_eq!(app.lines[2].text, "  line", "tail keeps the indent");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, 1, "caret sits after the inherited indent");
    }

    #[test]
    fn enter_on_empty_bullet_escapes_the_list() {
        let ctx = test_ctx();
        let mut app = page(&["t", " item", "  ", "after"]);
        app.rebuild(40);
        // caret on the empty bullet (whitespace-only, level 2)
        enter_session(&mut app, &ctx, 2, 2);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        // the empty bullet dissolved into a true blank…
        assert_eq!(app.lines[2].text, "", "spaces removed from the empty bullet");
        // …and the fresh line is flush, no inherited indent
        assert_eq!(app.lines[3].text, "");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3);
        assert_eq!(s.input.cur, 0);
        assert_eq!(app.lines[4].text, "after");
        let jobs = drain_jobs(&mut app);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text.is_empty()));
        assert!(matches!(&jobs[0].1[1], EditOp::Insert { lines, .. } if lines[0].1.is_empty()));
        // a NON-empty bullet still inherits its indent on Enter
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        enter_session(&mut app, &ctx, 1, " item".len());
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ", "normal path unchanged");
    }

    #[test]
    fn session_join_up_at_bol() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 0);
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(app.lines.len(), 2);
        assert_eq!(app.lines[1].text, "onetwo");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1);
        assert_eq!(s.input.cur, 3, "caret on the junction");
        let jobs = drain_jobs(&mut app);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text == "onetwo"));
        assert!(matches!(&jobs[0].1[1], EditOp::Delete { .. }));
    }

    #[test]
    fn undo_redo_roundtrip_with_queue() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        // x-style delete of line 1
        do_edit(&mut app, &ctx, "delete", vec![EditOp::Delete { id: "id1".into() }]);
        assert_eq!(app.lines.len(), 2);
        undo(&mut app, &ctx);
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.lines[1].text, "one", "undo restored the line in place");
        redo(&mut app, &ctx);
        assert_eq!(app.lines.len(), 2);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 3, "delete, undo, redo each committed");
        assert!(jobs[1].0.starts_with("undo"));
        assert!(jobs[2].0.starts_with("redo"));
    }

    #[test]
    fn session_paste_multiline_becomes_lines() {
        let ctx = test_ctx();
        let mut app = page(&["title", "ab"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 1); // caret between a and b
        session_paste(&mut app, &ctx, "X\nY\nZ");
        assert_eq!(app.lines[1].text, "aX");
        assert_eq!(app.lines[2].text, "Y");
        assert_eq!(app.lines[3].text, "Zb", "line tail follows the last fragment");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3);
        assert_eq!(s.input.cur, 1, "caret after the pasted Z");
    }

    #[test]
    fn open_line_below_inherits_indent_and_opens_session() {
        let ctx = test_ctx();
        let mut app = page(&["title", "  bullet", "after"]);
        app.rebuild(40);
        app.cursor = 1;
        open_line(&mut app, &ctx, false);
        assert_eq!(app.lines.len(), 4);
        assert_eq!(app.lines[2].text, "  ", "new line inherits the indent");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, 2);
    }

    /// A server page shaped like the poller would deliver it.
    fn polled(texts: &[(&str, &str)]) -> PolledPage {
        PolledPage {
            project: "proj".into(),
            title: "t".into(),
            page: cosense::api::Page {
                id: "pid".into(),
                title: "t".into(),
                commit_id: String::new(),
                lines: texts
                    .iter()
                    .map(|(id, t)| PageLine {
                        id: (*id).into(),
                        text: (*t).into(),
                        user_id: String::new(),
                        created: 0,
                        updated: 0,
                    })
                    .collect(),
                links: vec![],
                project_links: vec![],
                related: None,
                updated: 0,
                created: 0,
                lines_count: 0,
                last_accessed: None,
            },
        }
    }

    #[test]
    fn remote_edits_apply_when_idle_and_keep_the_cursor_line() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.cursor = 2; // on "two" (id2)
        // remote inserted a line above and edited "one"
        apply_remote(
            &mut app,
            &ctx,
            polled(&[("id0", "t"), ("idN", "new!"), ("id1", "ONE"), ("id2", "two")]),
        );
        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["t", "new!", "ONE", "two"]);
        assert_eq!(app.cursor, 3, "cursor followed its line id, not its number");
        assert!(app.status.contains("web"));
    }

    #[test]
    fn remote_edits_never_touch_a_dirty_caret_line() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!"); // dirty
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        assert_eq!(app.lines[1].text, "one", "deferred while dirty");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!");
        // …but a CLEAN session re-anchors and picks up the remote text
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        let _ = drain_jobs(&mut app);
        enter_session(&mut app, &ctx, 1, 0);
        app.inflight = 0;
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "REMOTE");
        assert_eq!(s.orig, "REMOTE");
    }

    #[test]
    fn remote_apply_waits_for_inflight_commits() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.inflight = 1;
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        assert_eq!(app.lines[1].text, "one", "own commits must land first");
    }

    // ---- websocket push (NOTE-websocket-sync.md) ----

    /// One remote commit event, shaped like the ws thread would deliver it.
    fn commit(id: &str, parent: &str, page: &str, user: &str, ops: Vec<EditOp>) -> RemoteCommit {
        RemoteCommit {
            commit_id: id.into(),
            parent_id: parent.into(),
            page_id: page.into(),
            user_id: user.into(),
            ops,
        }
    }

    #[test]
    fn ws_contiguous_commit_applies_a_diff_and_advances_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "ONE!".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "ONE!", "diff applied in place");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.status.contains("websocket"));
    }

    #[test]
    fn ws_same_user_browser_commit_applies() {
        // A commit from the SAME account is the primary use case (edit in
        // the browser, watch the TUI follow) — it must apply, not be
        // dropped as a "self-echo" (idempotency protects against real
        // echoes instead). There is no me_id concept anymore.
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "me", // same user that runs the TUI
                vec![EditOp::Replace { id: "id1".into(), text: "SELF".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "SELF", "same-user browser edit reflects");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert_eq!(app.lines[1].user_id, "me", "the committer is stamped for blame");
    }

    #[test]
    fn ws_own_echo_does_not_double_insert() {
        // OUR commit came back as an event: applying it again must not
        // duplicate the inserted line (insert-by-id skip).
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        let echo = commit(
            "c1",
            "c0",
            &pid,
            "me",
            vec![EditOp::Insert {
                anchor: "_end".into(),
                lines: vec![("n1".into(), "new line".into())],
            }],
        );
        ws_on_commit(&mut app, &ctx, echo.clone());
        assert_eq!(app.lines.len(), 3);
        // the echo reapplies (it is just an idempotent no-op now)
        ws_on_commit(&mut app, &ctx, echo);
        assert_eq!(app.lines.len(), 3, "no duplicate from the echo");
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "one", "new line"]);
    }

    #[test]
    fn ws_commits_buffer_while_gated_and_flush_in_order() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        app.inflight = 1; // our own commit is landing — gate up
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "FIRST".into() }],
            ),
        );
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "c1",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("n1".into(), "tail".into())],
                }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "nothing applied while gated");
        assert_eq!(app.ws_pending.len(), 2);
        // the commit lands: gate drops, buffered events apply oldest first
        app.inflight = 0;
        ws_flush_pending(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "FIRST", "two", "tail"]);
        assert_eq!(app.ws_head.as_deref(), Some("c2"));
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_dirty_caret_line_buffers_the_commit() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!"); // dirty caret line — gate up
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "REMOTE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "buffered, not applied");
        assert_eq!(app.ws_pending.len(), 1);
        // leaving the line commits it and drops the gate; flush applies
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        let _ = drain_jobs(&mut app);
        app.inflight = 0; // the queued commit is "done" in this test harness
        ws_flush_pending(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "REMOTE");
    }

    #[test]
    fn ws_different_page_events_are_dropped() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                "some-other-page",
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NOPE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "stale-room events go nowhere");
        assert!(app.ws_pending.is_empty());
    }

    /// A resync result as the ws thread would ship it.
    fn resync(page: cosense::api::Page, head: Option<&str>) -> ws::ResyncPage {
        ws::ResyncPage { page, head: head.map(str::to_string) }
    }

    #[test]
    fn ws_resync_installs_the_page_and_resumes_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c9".into());
        let pid = app.page_id.clone();
        let mut p = polled(&[("id0", "t"), ("idX", "fresh!"), ("id1", "one")]).page;
        p.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c12"))));
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "fresh!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c12"), "chain resumes at head");
        // …and the next commit with that parent applies as a contiguous diff
        let pid2 = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c13",
                "c12",
                &pid2,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NEXT".into() }],
            ),
        );
        // "id1" is at index 2 now (the resync inserted "idX" above it)
        assert_eq!(app.lines[2].text, "NEXT", "the replace landed on id1");
        assert_eq!(app.ws_head.as_deref(), Some("c13"));
    }

    #[test]
    fn ws_resync_while_gated_is_held_and_applied_afterwards() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        app.inflight = 1; // gate up
        // a commit that must be superseded by the resync
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("o1".into(), "old".into())],
                }],
            ),
        );
        let mut p = polled(&[("id0", "t"), ("idN", "fresh!"), ("id1", "one")]).page;
        p.id = pid.clone();
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c1"))));
        // still gated: the resync is HELD, the commit is buffered before it
        assert!(app.ws_held_resync.is_some(), "the latest resync is kept");
        assert_eq!(app.lines[1].text, "one");
        // a second resync while gated REPLACES the held one (latest wins)
        let mut p2 = polled(&[("id0", "t"), ("idN2", "newer!"), ("id1", "one")]).page;
        p2.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p2, Some("c1"))));
        // gate drops: latest held resync applies, the pre-resync commit is
        // superseded (its effect is inside the fetched page)
        app.inflight = 0;
        ws_apply_held_resync(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "newer!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.ws_held_resync.is_none());
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_chain_break_requests_a_background_resync() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "cNOPE",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "??".into() }],
            ),
        );
        // the stray commit is not applied and NOT silently dropped: a
        // background resync is requested (no network on the UI thread)
        assert_eq!(app.lines[1].text, "one");
        assert!(app.ws_resync_pending, "gap event requests a resync");
        ws_send_resync_request(&mut app);
        let req_rx = app.ws_req_rx.as_ref().unwrap();
        assert_eq!(req_rx.try_recv(), Ok(ws::WsRequest::Resync));
        assert!(!app.ws_resync_pending, "one request per gap flag");
    }

    #[test]
    fn session_line_shows_bullets_for_indent() {
        // Two cells per nesting step after the flush-left level 1; DATA
        // remains one whitespace character per logical level.
        assert_eq!(session_display(" ab"), "• ab");
        assert_eq!(session_display("  ab"), "  • ab");
        assert_eq!(session_display(" \t\tab"), "    • ab", "tabs count as levels too");
        assert_eq!(session_display("ab"), "ab", "no indent, no bullet");
        assert_eq!(session_display("  "), "  • ", "a whitespace-only line shows just its bullet");
        // Caret mapping expands and contracts the visual indentation.
        assert_eq!(display_caret("  ab", 1), 2, "one logical step = two cells");
        assert_eq!(display_caret("  ab", 2), "  • ".len(), "text start lands after '• '");
        assert_eq!(raw_caret_from_display("  ab", "  • ".len()), 2);
        // clicking ON the injected space snaps to the text start
        assert_eq!(raw_caret_from_display("  ab", "  •".len()), 2);
        assert_eq!(display_caret("  ab", 4), "  • ab".len(), "end maps to end");
        assert_eq!(raw_caret_from_display("  ab", "  • ab".len()), 4);
        assert_eq!(display_caret("\t\tab", 2), "  • ".len());
        assert_eq!(raw_caret_from_display("\t\tab", "  • ".len()), 2);
        assert_eq!(display_caret("ab", 1), 1, "no indent → identity");
        // the rendered session row carries the bullet + space
        let ctx = test_ctx();
        let mut app = page(&["t", "  deep"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 2);
        app.rebuild(40);
        let (first, _) = app.src_rows(1).unwrap();
        let row_text: String = match &app.rows[first] {
            Row::Line { line, .. } => line.spans.iter().map(|s| s.content.as_ref()).collect(),
            _ => String::new(),
        };
        assert_eq!(row_text, "  • deep");
        // …and typing still edits the RAW text underneath
        type_str(&mut app, &ctx, "!");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "  !deep");
    }

    #[test]
    fn caret_math_matches_the_wrapped_display() {
        // 10-wide: "あいうえお" is 10 cols → caret after う = row0 col6;
        // after お = boundary → next row col0 when more text follows.
        let s = "あいうえおxy";
        assert_eq!(caret_row_col(s, 9, 10), (0, 6));
        assert_eq!(caret_row_col(s, 15, 10), (1, 0));
        assert_eq!(caret_row_col(s, 17, 10), (1, 2));
        assert_eq!(byte_at_col("あいう", 4), 6, "col 4 → third kana");
        // segments carry every char: display and math agree
        assert_eq!(wrap_plain_columns(s, 10).join(""), s);
    }

    #[test]
    fn movement_skips_comment_cards() {
        let mut app = page(&["a", "b", "c"]);
        app.comments.push(Comment {
            project: "proj".into(),
            title: "t".into(),
            start: 0,
            end: 0,
            line_texts: vec!["a".into()],
            line_ids: vec!["id0".into()],
            text: "note".into(),
        });
        app.rebuild(40);
        assert!(app.rows.iter().any(|r| matches!(r, Row::Card { .. })));
        app.move_cursor(true);
        assert_eq!(app.cursor, 1, "card rows (no source) are stepped over");
        app.move_cursor(false);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn table_rows_are_individually_addressable() {
        let mut app = page(&["table:t", "\ta\tb", "\tc\td", "after"]);
        app.rebuild(40);
        // every source line of the table owns display rows now
        assert!(app.src_rows(0).is_some(), "opener: name line + top border");
        assert!(app.src_rows(1).is_some(), "header row");
        assert!(app.src_rows(2).is_some(), "body row");
        app.goto_src(2);
        assert_eq!(app.cursor, 2, "cursor rests on the row itself");
        // j/k walk opener → header → row → after
        app.goto_src(0);
        app.move_cursor(true);
        assert_eq!(app.cursor, 1);
        app.move_cursor(true);
        assert_eq!(app.cursor, 2);
        app.move_cursor(true);
        assert_eq!(app.cursor, 3);
        // the last line is reachable and the clamp respects the end
        app.goto_src(99);
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn follow_cursor_uses_last_row_going_down_and_first_going_up() {
        let mut app = page(&["a", "b", "c", &"y".repeat(50), "e"]);
        app.rebuild(22); // line 3 wraps to 4 rows: rows 3,4,5,6
        app.goto_src(3);
        app.follow_cursor(4);
        // rows 3..=6 inside a 4-row viewport, with one row below for the
        // bottom rule -> scroll = 7 - 4 + 1
        assert_eq!(app.scroll, 4);
        app.goto_src(0);
        app.follow_cursor(4);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn paging_moves_by_display_rows_and_snaps_to_a_source_line() {
        let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.rebuild(40);
        app.move_cursor_display(10);
        assert_eq!(app.cursor, 10);
        app.move_cursor_display(5);
        assert_eq!(app.cursor, 15);
        app.move_cursor_display(-100);
        assert_eq!(app.cursor, 0, "clamped at the top");
        app.move_cursor_display(100);
        assert_eq!(app.cursor, 29, "clamped at the bottom");
        // landing on a comment card slides onward to the next sourced row
        app.comments.push(Comment {
            project: "proj".into(),
            title: "t".into(),
            start: 5,
            end: 5,
            line_texts: vec!["line 5".into()],
            line_ids: vec!["id5".into()],
            text: "note".into(),
        });
        app.rebuild(40);
        app.goto_src(0);
        // rows: 0..=5 lines, then the card (4 rows), then line 6 ...
        app.move_cursor_display(7); // row 7 = inside the card
        assert_eq!(app.cursor, 6);
    }

    #[test]
    fn screen_row_maps_back_to_the_source_line_under_it() {
        let mut app = page(&["a", &"b".repeat(50), "c"]);
        app.rebuild(22); // rows: a, b, b, b, b, c
        assert_eq!(app.src_at_screen_row(0), Some(0));
        assert_eq!(app.src_at_screen_row(2), Some(1), "wrapped continuation -> its line");
        assert_eq!(app.src_at_screen_row(5), Some(2));
        assert_eq!(app.src_at_screen_row(6), None, "past the end");
        app.scroll = 3;
        assert_eq!(app.src_at_screen_row(-1), Some(1), "reclaimed top row maps too");
        assert_eq!(app.src_at_screen_row(0), Some(1));
        assert_eq!(app.src_at_screen_row(2), Some(2));
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    /// A 30-line page in the same vertical geometry as `ui`: body band
    /// y=1..10, unscrolled content anchor y=2, scrollbar at column 41.
    fn page_with_screen() -> App {
        let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 2, 40, 8);
        app.bar_rect = Rect::new(41, 1, 1, 10);
        app.view_h = 10;
        app
    }

    #[test]
    fn page_chrome_separates_caret_telomere_pad_and_top_scrollbar() {
        use ratatui::{backend::TestBackend, Terminal};

        let texts: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.header_colors = HeaderColors {
            fg: Color::White,
            bg: Color::Rgb(80, 102, 184),
        };
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 12)).unwrap();
        // The first draw establishes wrapped rows; the next is the stable
        // frame the event loop presents.
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            // body starts at y=1; its first content row is y=2.
            assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::White);
            assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(80, 102, 184));
            assert_eq!(buf.cell((0, 2)).unwrap().symbol(), ">");
            assert_eq!(buf.cell((0, 2)).unwrap().fg, Color::LightBlue);
            assert_ne!(buf.cell((1, 2)).unwrap().symbol(), " ", "telomere remains visible");
            assert_eq!(buf.cell((2, 2)).unwrap().symbol(), " ", "one blank after telomere");
            assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "l", "text starts after the blank");
            for x in 1..=40 {
                assert_eq!(
                    buf.cell((x, 2)).unwrap().bg,
                    Color::DarkGray,
                    "cursor band must fill every inside column at x={x}",
                );
            }
            // The thumb starts below the top frame rule instead of overwriting it.
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "─");
            assert_eq!(buf.cell((40, 1)).unwrap().fg, Color::DarkGray);
            assert_eq!(buf.cell((40, 2)).unwrap().symbol(), "▐");
            assert_eq!(buf.cell((40, 2)).unwrap().fg, Color::Gray);
        }

        // Once the top rule scrolls away, screen row 2 (y=1) is reclaimed
        // by every page layer instead of remaining a permanent blank strip.
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            assert_eq!(buf.cell((0, 1)).unwrap().symbol(), ">");
            assert_ne!(buf.cell((1, 1)).unwrap().symbol(), " ", "telomere at top row");
            assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "l", "text at top row");
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "▐", "scrollbar at top row");
        }
        app.cursor = 5;
        handle_mouse_content(
            &mut app,
            &ctx,
            mouse(MouseEventKind::Down(MouseButton::Left), 8, 1),
        );
        assert_eq!(app.cursor, 0, "reclaimed top row remains mouse-addressable");

        enter_session(&mut app, &ctx, 0, 0);
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((20, 1)).unwrap().symbol(), " ", "EDIT removes the top frame");
        assert_eq!(buf.cell((41, 3)).unwrap().symbol(), " ", "EDIT removes the side frame");
    }

    #[test]
    fn scrolled_image_placeholder_reclaims_the_top_body_row() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = page(&["image"]);
        app.blocks = vec![Block::Image { url: "https://example.com/a.png".into() }];
        app.srcs = vec![0];
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 8)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "□");
            assert_ne!(buf.cell((1, 1)).unwrap().symbol(), " ", "image telomere at top row");
        }

        // The real sliced image uses a different widget path from its text
        // placeholder and must reclaim the same row.
        let pixels = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]));
        let info = build_image(&ctx.picker, image::DynamicImage::ImageRgba8(pixels), IMAGE_MAX_COLS).unwrap();
        app.images.insert("https://example.com/a.png".into(), info);
        app.rebuild(42);
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        assert!(
            (3..20).any(|x| {
                let cell = buf.cell((x, 1)).unwrap();
                cell.fg == Color::Rgb(255, 0, 0) || cell.bg == Color::Rgb(255, 0, 0)
            }),
            "sliced image paints the reclaimed top row",
        );
    }

    #[test]
    fn click_moves_cursor_and_drag_selects_a_line_range() {
        let mut app = page_with_screen();
        app.scroll = 5;
        // press two rows below the content anchor -> display row 7
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 10, 4));
        assert_eq!(app.cursor, 7);
        assert_eq!(app.selection, None, "a click alone selects nothing");
        // drag four rows down -> line 11: selection 7..=11
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 12, 8));
        assert_eq!(app.selection.map(|s| s.range()), Some((7, 11)));
        assert_eq!(app.cursor, 11);
        // dragging back above the anchor flips the range
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 12, 2));
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Up(MouseButton::Left), 12, 2));
        assert_eq!(app.drag_anchor, None);
        // the selection survives the release (c comments on it)
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        // a click outside the rows (header) changes nothing
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 10, 0));
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn rendered_links_are_mouse_hit_targets() {
        let mut app = page(&[
            "t",
            "see [Target] and [Docs https://example.com] #tag",
        ]);
        app.rebuild(80);
        let pal = cosense::theme::Palette::for_light(false);
        // Rendered row: `see Target and Docs #tag`.
        assert_eq!(
            app.link_at_screen_position(1, 4, &pal),
            Some((1, LinkItem::Page("Target".into())))
        );
        assert_eq!(
            app.link_at_screen_position(1, 15, &pal),
            Some((
                1,
                LinkItem::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into(),
                },
            ))
        );
        assert_eq!(
            app.link_at_screen_position(1, 20, &pal),
            Some((1, LinkItem::Page("tag".into())))
        );
        assert_eq!(
            app.link_at_screen_position(1, 3, &pal),
            None,
            "plain text is not clickable"
        );

        // Identical labels still resolve by their rendered occurrence.
        let mut dup = page(&[
            "t",
            "see [Docs] then [Docs https://example.com]",
        ]);
        dup.rebuild(80);
        assert_eq!(
            dup.link_at_screen_position(1, 14, &pal),
            Some((
                1,
                LinkItem::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into(),
                },
            ))
        );

        let mut wrapped = page(&["t", "[ABCDEFGHIJK]"]);
        wrapped.rebuild(12); // text width 6: the link occupies rows 1 and 2
        assert_eq!(
            wrapped.link_at_screen_position(2, 1, &pal),
            Some((1, LinkItem::Page("ABCDEFGHIJK".into())))
        );

        // Related-page labels remain clickable after visual truncation.
        let related_src = wrapped.lines.len();
        wrapped.virtual_items = vec![LinkItem::Page("Very long related page".into())];
        wrapped.rows = vec![Row::Line {
            line: Line::from(Span::styled(
                "Very…",
                Style::default()
                    .fg(CHROME_CARET)
                    .add_modifier(Modifier::UNDERLINED),
            )),
            src: related_src,
        }];
        assert_eq!(
            wrapped.link_at_screen_position(0, 2, &pal),
            Some((related_src, LinkItem::Page("Very long related page".into())))
        );

        // Pressing records the target, but dragging cancels activation and
        // keeps the existing line-selection gesture available.
        app.text_rect = Rect::new(2, 1, 74, 8);
        app.bar_rect = Rect::new(77, 1, 1, 8);
        handle_mouse_content(
            &mut app,
            &test_ctx(),
            mouse(MouseEventKind::Down(MouseButton::Left), 2 + 4, 1 + 1),
        );
        assert_eq!(app.pressed_link, Some((1, LinkItem::Page("Target".into()))));
        handle_mouse_content(
            &mut app,
            &test_ctx(),
            mouse(MouseEventKind::Drag(MouseButton::Left), 2 + 8, 1 + 1),
        );
        assert_eq!(app.pressed_link, None);
    }

    #[test]
    fn scrollbar_click_and_drag_scrub_the_viewport_only() {
        let mut app = page_with_screen();
        app.goto_src(3);
        // top rule + 30 rows + FrameEnd / 10 viewport: the scrollbar
        // includes the complete visual extent.
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 41, 1 + 7));
        let end = app.max_scroll(app.view_h);
        assert_eq!(app.scroll, end, "track bottom -> last page");
        assert_eq!(app.cursor, 3, "the cursor keeps its line");
        assert!(!app.follow);
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 41, 1 + 3));
        assert!(app.scroll < end && app.scroll > 0);
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Up(MouseButton::Left), 41, 1 + 3));
        assert_eq!(app.scrollbar_drag, None);
        // wheel over the body: one row per event, cursor untouched
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::ScrollUp, 10, 5));
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn wheel_scroll_moves_viewport_only() {
        let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.rebuild(40);
        app.goto_src(2);
        app.wheel_scroll(3, 10);
        assert_eq!(app.scroll, 3);
        assert_eq!(app.cursor, 2, "the cursor keeps its line");
        assert!(!app.follow, "the next frame must not yank the viewport back");
        app.wheel_scroll(1000, 10);
        assert_eq!(app.scroll, 22, "clamped to max (content+2 rules - viewport)");
        app.wheel_scroll(-1000, 10);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn mode_toggle_keeps_cursor_line_on_the_same_screen_row() {
        // Long lines wrap differently in the two modes (source loses the
        // number gutter), so the row count above the cursor changes.
        let mut texts: Vec<String> = Vec::new();
        for i in 0..8 {
            texts.push(format!("{i} {}", "w".repeat(30)));
        }
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.mode = Mode::Source;
        app.rebuild(30);
        app.goto_src(6);
        app.follow_cursor(8);
        let before = app.cursor_top().unwrap() as i32 - app.scroll as i32;
        assert!(before >= 0 && before < 8);

        app.mode = Mode::View;
        app.relayout_preserving_screen_row(30, 8);
        assert_eq!(app.cursor, 6, "same source line");
        let after = app.cursor_top().unwrap() as i32 - app.scroll as i32;
        assert_eq!(after, before, "same physical screen row");
    }

    #[test]
    fn selection_and_comment_are_source_line_ranges() {
        let mut app = page(&["a", &"b".repeat(50), "c", "d"]);
        app.rebuild(22);
        app.selection = Some(Selection::new(app.cursor));
        app.move_cursor(true);
        app.move_cursor(true);
        assert_eq!(app.selection.unwrap().range(), (0, 2));
        let c = app.make_comment("fix".into()).unwrap();
        assert_eq!((c.start, c.end), (0, 2));
        assert_eq!(c.line_texts.len(), 3);
        assert_eq!(c.line_ids[2], "id2");
    }

    #[test]
    fn unread_is_edited_after_last_seen_or_never_seen() {
        assert!(unread_since(100, None), "first visit: everything is unread");
        assert!(unread_since(101, Some(100)));
        assert!(!unread_since(100, Some(100)));
        assert!(!unread_since(50, Some(100)));
        let mut app = page(&["a", "b", "c"]);
        app.lines[1].updated = 500;
        app.read_at = Some(200);
        assert_eq!(app.unread_count(), 1);
        app.read_at = None;
        assert_eq!(app.unread_count(), 3);
    }

    #[test]
    fn file_links_are_found_with_their_labels() {
        let pdf = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
        let line = format!("[260826ニセコ.pdf {pdf}] and [Other Page]");
        assert_eq!(files_on_line(&line), vec![("260826ニセコ.pdf".to_string(), pdf.to_string())]);
        // bare URL: labelled by its file name
        assert_eq!(
            files_on_line(&format!("see {pdf} now")),
            vec![("6a8e7e5d714feb3f195319dd.pdf".to_string(), pdf.to_string())]
        );
        // images and plain URLs are not files
        assert!(files_on_line("[https://scrapbox.io/files/abc.png]").is_empty());
        assert!(files_on_line("[https://example.com/a.pdf]").is_empty());
        // …but every URL is offered to the browser, titled or bare
        assert_eq!(
            labelled_urls("[Docs https://example.com/a.pdf] and https://b.example/x?y=1 done"),
            vec![
                ("Docs".to_string(), "https://example.com/a.pdf".to_string()),
                ("https://b.example/x?y=1".to_string(), "https://b.example/x?y=1".to_string()),
            ]
        );
        // Scrapbox's url-first order `[https://… title]` labels too
        assert_eq!(
            labelled_urls("[https://example.com Example]"),
            vec![("Example".to_string(), "https://example.com".to_string())]
        );
        // a gyazo image line jumps to the image's PAGE, whatever URL form
        // the line used; Teams keeps its org host
        let id = "1d507226c261ce8cc513e8736ee5070c";
        for form in [
            format!("[https://gyazo.com/{id}]"),
            format!("[https://i.gyazo.com/{id}.png]"),
            format!("https://i.gyazo.com/{id}.jpg"),
        ] {
            let mut g = page(&["t", &form]);
            g.rebuild(80);
            g.goto_src(1);
            assert_eq!(
                g.cursor_line_links(),
                vec![LinkItem::Url { label: "gyazo".into(), url: format!("https://gyazo.com/{id}") }],
                "{form}"
            );
        }
        let mut g = page(&["t", &format!("[図 https://acme.gyazo.com/{id}]")]);
        g.rebuild(80);
        g.goto_src(1);
        assert_eq!(
            g.cursor_line_links(),
            vec![LinkItem::Url { label: "図".into(), url: format!("https://acme.gyazo.com/{id}") }]
        );
        let mut app2 = page(&["t", "see [Docs https://example.com/a.pdf]"]);
        app2.rebuild(80);
        app2.goto_src(1);
        assert_eq!(
            app2.cursor_line_links(),
            vec![LinkItem::Url { label: "Docs".into(), url: "https://example.com/a.pdf".into() }]
        );
        assert_eq!(app2.cursor_line_links()[0].label(), "↗ Docs");
        // the cursor line offers pages first, then files
        let mut app = page(&["t", &line]);
        app.rebuild(80);
        app.goto_src(1);
        let items = app.cursor_line_links();
        assert_eq!(items[0], LinkItem::Page("Other Page".into()));
        // cross-project links: `[/project/title]` (a bare `[/project]` is
        // the project's top page, not a page link); tags stay in-project
        assert_eq!(
            links_on_line("see [/shokai/階層整理型WiKiはスケールしない] and [/tus-alpine] #Scrapboxの哲学 [/ italic]"),
            vec![
                LinkItem::ProjectPage { project: "shokai".into(), title: "階層整理型WiKiはスケールしない".into() },
                LinkItem::Page("Scrapboxの哲学".into()),
            ]
        );
        assert_eq!(
            LinkItem::ProjectPage { project: "shokai".into(), title: "t".into() }.label(),
            "/shokai/t"
        );
        assert!(matches!(&items[1], LinkItem::File { label, .. } if label == "260826ニセコ.pdf"));
        assert_eq!(items[1].label(), "📎 260826ニセコ.pdf");
    }

    #[test]
    fn download_path_prefers_the_label_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("cosense-tui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = |p: &std::path::Path| p.file_name().unwrap().to_str().unwrap().to_string();
        let url = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ.pdf");
        // a label without an extension falls back to the URL's file name
        assert_eq!(name(&download_path_in(&dir, "資料", url)), "6a8e7e5d714feb3f195319dd.pdf");
        // slashes in a label cannot escape the directory
        assert_eq!(name(&download_path_in(&dir, "a/b.pdf", url)), "a_b.pdf");
        // an existing file is kept: the new one gets a numbered suffix
        std::fs::write(dir.join("260826ニセコ.pdf"), b"x").unwrap();
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ (2).pdf");
        std::fs::write(dir.join("260826ニセコ (2).pdf"), b"x").unwrap();
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ (3).pdf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn page_urls_from_the_command_line_are_parsed() {
        let enc = "CBT%E3%83%93%E3%82%B8%E3%83%8D%E3%82%B9%E9%83%A8%E6%A1%88%E4%BB%B6";
        assert_eq!(
            parse_page_url(&format!("https://scrapbox.io/acme/{enc}#6a8f895000000000000000e1")),
            Some(("acme".into(), Some("CBTビジネス部案件".into()), Some("6a8f895000000000000000e1".into())))
        );
        assert_eq!(
            parse_page_url("https://scrapbox.io/my-sandbox/"),
            Some(("my-sandbox".into(), None, None)),
            "a project URL opens its latest page"
        );
        assert_eq!(
            parse_page_url("scrapbox.io/p/T?x=1"),
            Some(("p".into(), Some("T".into()), None)),
            "scheme-less, query dropped"
        );
        assert_eq!(parse_page_url("https://cosen.se/p/a%2Fb"), Some(("p".into(), Some("a/b".into()), None)));
        // titles may contain a slash: everything after the project is title
        assert_eq!(parse_page_url("https://scrapbox.io/p/a/b"), Some(("p".into(), Some("a/b".into()), None)));
        assert_eq!(parse_page_url("acme"), None, "a bare project name is not a URL");
        assert_eq!(parse_page_url("https://example.com/x/y"), None);
        // decoding round-trips the encoder used for browser deep links
        assert_eq!(percent_decode(&urlencode_component("階層整理型WiKi はスケールしない")), "階層整理型WiKi はスケールしない");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn cursor_fraction_over_source_lines() {
        let mut app = page(&["a", "b", "c", "d", "e"]);
        app.rebuild(40);
        assert_eq!(app.cursor_fraction(), 0.0);
        app.goto_src(4);
        assert_eq!(app.cursor_fraction(), 1.0);
        app.goto_src(2);
        assert_eq!(app.cursor_fraction(), 0.5);
    }
}


