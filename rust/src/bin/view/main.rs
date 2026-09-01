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
mod images;
use images::*;
mod links;
use links::*;
mod web;
use web::*;
mod nav;
use nav::*;
mod sync;
use sync::*;
mod mouse;
use mouse::*;
mod outline;
use outline::*;
mod editing;
use editing::*;
mod session;
use session::*;
mod keys;
use keys::*;


