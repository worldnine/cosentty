use super::*;

/// A laid-out visual row. Content rows carry their source line; card rows
/// are synthesized (not selectable, no source). `FrameEnd` is the explicit
/// boundary between the boxed page body and the unboxed related sections.
pub(crate) enum Row {
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
    pub(crate) fn height(&self) -> u16 {
        match self {
            Row::Image { height, .. } | Row::Inline { height, .. } => *height,
            Row::ImageLoading { .. } => IMAGE_PLACEHOLDER_H,
            _ => 1,
        }
    }
    pub(crate) fn src(&self) -> Option<usize> {
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
pub(crate) enum Mode {
    View,
    Source,
}

/// Somewhere the reader has been. An index is not merely its project name:
/// its filter, cursor, scroll and focused excerpt are the choice the reader
/// was making. Keeping the model makes browser-style back instant and exact
/// — no refetch and no jump back to the first row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Page { project: String, title: String },
    Index { project: String, state: Box<cosense::index::Index> },
}

impl Place {
    pub(crate) fn label(&self) -> String {
        match self {
            Place::Page { title, .. } => title.clone(),
            Place::Index { project, .. } => format!("/{project}"),
        }
    }
}

pub(crate) struct App {
    pub(crate) mode: Mode,
    pub(crate) project: String,
    pub(crate) title: String,
    /// Cosense project's navbar colors, adapted over the terminal background.
    pub(crate) header_colors: HeaderColors,
    /// Immutable page id (the edit API and the commit log key on it).
    pub(crate) page_id: String,
    /// Raw page lines with per-line author/time metadata. Note `user_id` is
    /// the line's LAST UPDATER (verified against the commit log), not its
    /// original author — the page API does not carry the creator.
    pub(crate) lines: Vec<PageLine>,
    pub(crate) blocks: Vec<Block>,
    /// What can be followed on each block (parallel to `blocks`), as the
    /// renderer reported it. The click path reads this instead of looking
    /// at colours.
    pub(crate) hits: Vec<Vec<cosense::render::Hit>>,
    pub(crate) srcs: Vec<usize>,
    pub(crate) images: HashMap<String, ImageInfo>,
    pub(crate) image_errors: HashMap<String, String>,
    /// Images currently downloading in the background.
    pub(crate) pending: HashSet<String>,
    pub(crate) image_tx: mpsc::Sender<ImageMsg>,
    pub(crate) image_rx: mpsc::Receiver<ImageMsg>,
    /// File downloads in flight (Enter/f on a 📎 link).
    pub(crate) file_tx: mpsc::Sender<FileMsg>,
    pub(crate) file_rx: mpsc::Receiver<FileMsg>,

    // --- web renderer (Mermaid today; see cosense::webrender) -------------
    /// Bumped on every page install. Shared with the render worker, which
    /// drops queued work for a page the reader has left before spending a
    /// decode or a browser navigation on it. A result that comes back for
    /// an older generation is dropped even if its key were to collide.
    pub(crate) web_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Terminal background is dark (picks the browser's color scheme).
    pub(crate) web_dark: bool,
    /// Render keys currently in flight.
    pub(crate) web_pending: HashSet<String>,
    /// Why a key failed, for the status line. Its block falls back to code.
    pub(crate) web_errors: HashMap<String, String>,
    /// Source lines whose code block is being rendered right now, each with
    /// `(row's position in the block, block row count)`. The draw pass runs
    /// a band of brightness down them so the reader can see the renderer
    /// working — rebuilt with the layout, empty whenever nothing is pending.
    pub(crate) web_shimmer: HashMap<usize, (u16, u16)>,
    /// Clock the shimmer animates against.
    pub(crate) web_anim: std::time::Instant,
    /// Column cap the on-screen diagrams are encoded for — `text_w` capped
    /// at `IMAGE_MAX_COLS`. Updated by the layout; a change re-encodes the
    /// artifacts from their cached PNGs (no browser).
    pub(crate) web_cols: u16,
    /// Rescales in flight, so a resize does not queue the same key twice.
    pub(crate) web_rescaling: HashSet<String>,
    /// The local page may no longer match the server: a commit was refused
    /// or never left the machine. The browser can only ever show what the
    /// SERVER has, so while this is set no diagram is rendered — a render
    /// would capture the server's version of a block and file it under the
    /// LOCAL text's hash, quietly attaching the wrong picture to the source
    /// the reader is looking at. Cleared only when an authoritative page
    /// install proves the two agree again (see `mark_desynced`).
    pub(crate) web_unsynced: bool,
    /// A diagram-failure note, and when it stops being worth showing.
    ///
    /// It lives in its OWN slot rather than in `app.status`, and ranks
    /// BELOW it: a commit failure, an auth error or a resync notice must
    /// never be overwritten — or worse, wiped when the diagram note times
    /// out — by something as minor as a picture that did not draw. It ranks
    /// above the key hints, and gives the line back after a few seconds so
    /// the hints return.
    pub(crate) web_notice: Option<(String, std::time::Instant)>,
    /// Jobs into the render worker, results back. Artifacts land in
    /// `images` (they are images), so drawing needs no special case.
    pub(crate) web_job_tx: mpsc::Sender<WebJob>,
    /// Held until `main` spawns the worker; tests keep it and inspect jobs.
    pub(crate) web_jobs_rx: Option<mpsc::Receiver<WebJob>>,
    pub(crate) web_tx: mpsc::Sender<WebMsg>,
    pub(crate) web_rx: mpsc::Receiver<WebMsg>,

    pub(crate) rows: Vec<Row>,
    pub(crate) laid_width: u16,
    /// Viewport height (text rows) as of the last frame; movement keys use
    /// it to keep the cursor in view without waiting for the next draw.
    pub(crate) view_h: u16,
    /// Screen geometry as of the last frame, for mouse hit-testing: the
    /// text area and the scrollbar track.
    pub(crate) text_rect: Rect,
    pub(crate) bar_rect: Rect,
    /// Source line where a left-button drag started (selection anchor),
    /// with the character anchor of the same press when the drag runs
    /// inside an edit session. A READ drag has no character range, so its
    /// anchor keeps `None` there.
    pub(crate) drag_anchor: Option<(usize, Option<usize>)>,
    /// Scrollbar thumb grab: (track row, scroll offset) at the press.
    pub(crate) scrollbar_drag: Option<(u16, u16)>,
    pub(crate) scroll: u16, // row-height units from top

    /// SOURCE line under the cursor (0-based). Display rows are derived —
    /// see `cursor_rows`. akapen's `ViewState.cursor`.
    pub(crate) cursor: usize,
    /// Range selection over SOURCE lines — READ's unit. EDIT selects
    /// characters instead (`EditSession::sel_from`); the two never coexist.
    pub(crate) selection: Option<Selection>,
    /// What is known about the pages this page's links lead to. Seeded by
    /// each page load and filled in by background lookups; kept for the
    /// whole project so walking back and forth does not re-ask.
    pub(crate) links: LinkTruth,
    /// Titles (in `title_lc` form) already asked about. A title stays here
    /// even if the lookup failed: one question per title per session, so a
    /// project this credential cannot read costs one request, not one
    /// every scan. The cost of that is a link left uncoloured, which is
    /// the harmless direction.
    pub(crate) link_pending: HashSet<String>,
    /// When the page was last scanned for links nobody has asked about,
    /// and what the page looked like then: the caret's line and the source
    /// epoch. A change to either is a reason to scan now rather than on
    /// the next beat.
    pub(crate) link_scan_at: Instant,
    pub(crate) link_scan_key: (Option<usize>, u64),
    /// To the lookup worker (`spawn_link_prober`); `None` in tests, which
    /// makes `probe_unknown_links` a no-op.
    pub(crate) link_probe_tx: Option<mpsc::Sender<LinkProbe>>,
    pub(crate) link_probe_rx: mpsc::Receiver<(LinkProbe, bool)>,
    pub(crate) link_probe_res_tx: mpsc::Sender<(LinkProbe, bool)>,
    /// Scroll the viewport to the cursor on the next frame. Set by keyboard
    /// navigation; wheel scrolling leaves it clear so the viewport can move
    /// away from the cursor (akapen's herdr-review style).
    pub(crate) follow: bool,
    /// Comment composer (the bottom input; `c`).
    pub(crate) composing: Option<Input>,
    pub(crate) comments: Vec<Comment>,
    pub(crate) status: String,
    /// One-shot portable fallback for terminals that do not deliver
    /// modified arrow keys. The next key is always consumed.
    pub(crate) outline_prefix: bool,
    /// The grabbed block, while the sticky move mode is up.
    pub(crate) move_mode: Option<MoveMode>,
    /// Snapshot held while the one structural commit is outstanding. READ
    /// remains navigable, but no second mutation may be based on its
    /// optimistic line order until the server accepts or rejects it.
    pub(crate) outline_pending: Option<OutlinePending>,
    /// A structural commit succeeded after this installation was fetched,
    /// but its authoritative refresh failed. The displayed line IDs are
    /// unsafe edit targets until another page install replaces them.
    pub(crate) outline_refresh_needed: bool,

    /// Back stack of visited places (`[`), and forward stack (`]`).
    pub(crate) history: Vec<Place>,
    pub(crate) forward: Vec<Place>,
    /// Open overlay (comments list / link picker / help), if any.
    pub(crate) overlay: Option<Overlay>,
    /// The project index (full-width list + excerpt dock). Open = it owns
    /// the screen and the keys; the page it came from remains underneath.
    pub(crate) index: Option<cosense::index::Index>,
    /// The list and excerpt rectangles from the last index frame, so mouse
    /// input uses exactly the geometry that was drawn.
    pub(crate) index_list_rect: Rect,
    pub(crate) index_preview_rect: Rect,
    /// The project the open index is listing. Usually the page's own, but
    /// a `[/other-project]` link opens that project's index while the page
    /// underneath is still this one's.
    pub(crate) index_project: String,
    /// When the index was last scrolled. Its scrollbar appears briefly at
    /// the screen edge while the list moves, then gets out of the way.
    pub(crate) index_scrolled_at: Option<Instant>,

    /// When this page was last seen by the user before this visit — the
    /// later of Cosense's `lastAccessed` (browser) and the local visit
    /// record (this viewer). `None` = first visit anywhere: every line is
    /// unread, as on scrapbox.io.
    pub(crate) read_at: Option<i64>,

    /// Per-project member tables (userId -> display name) for line blame,
    /// fetched lazily: see `ensure_members` for when they refresh.
    pub(crate) members: HashMap<String, MembersCache>,

    /// Some while the reader is viewing a historical snapshot (←/→).
    /// The page is read-only in that state.
    pub(crate) time: Option<TimeMachine>,
    /// Whether the current credential may edit THIS project. Public pages
    /// remain readable with a PAT for another project, but must not enter
    /// EDIT unless that user is actually a member.
    pub(crate) editable: bool,
    /// Session input-source control: command mode runs in ASCII, the
    /// original source is restored when the app drops (Japanese-first
    /// model, see `cosense::ime`).
    pub(crate) session_ime: cosense::ime::SessionIme,
    /// True once the session's force-ascii took hold (the helper may still
    /// be compiling on first run; retried each tick until then).
    pub(crate) ime_ready: bool,
    /// Held while the composer or edit session is open: Japanese in,
    /// ASCII out.
    pub(crate) ime_guard: Option<cosense::ime::ImeGuard>,

    /// The modeless edit session, when open.
    pub(crate) session: Option<EditSession>,
    /// Undo stack: (label, ops that revert the corresponding commit).
    pub(crate) undo_stack: Vec<(String, Vec<EditOp>)>,
    pub(crate) redo_stack: Vec<(String, Vec<EditOp>)>,
    /// A server refresh threw away history that could no longer be
    /// replayed. Only used to explain an empty stack instead of shrugging.
    pub(crate) history_dropped: bool,
    /// Where an uncreated page stands with the server (see `CreateState`).
    pub(crate) create_state: CreateState,
    /// Commit queue into the serial worker, and its outcomes back.
    pub(crate) commit_tx: mpsc::Sender<CommitJob>,
    pub(crate) commit_res_rx: mpsc::Receiver<CommitOutcome>,
    /// The worker's job receiver, held until `main` spawns the worker
    /// (tests keep it here and inspect queued jobs directly).
    pub(crate) commit_jobs_rx: Option<mpsc::Receiver<CommitJob>>,
    /// Result sender handed to the worker (kept for the spawn call).
    pub(crate) commit_res_tx: mpsc::Sender<CommitOutcome>,
    /// Jobs sent but not yet answered (quit flushes until 0).
    pub(crate) inflight: usize,
    /// Next commit-job id. Monotonic for the life of the process, so an
    /// outcome can always be traced back to the job that produced it.
    pub(crate) next_job_id: CommitJobId,
    /// Commit ids this viewer wrote, newest last. Their websocket echoes
    /// carry ops we have already applied locally — and may arrive AFTER we
    /// have edited past them, in which case applying the ops again would
    /// put an older version of a line back on screen. Recognised by id, so
    /// they only move the chain head along.
    ///
    /// Bounded: an echo that has not arrived within `OWN_COMMIT_MEMORY`
    /// commits is not going to, and the page has moved so far past it that
    /// the chain check would send us to a resync anyway.
    pub(crate) own_commits: std::collections::VecDeque<String>,
    /// Conflict generation: bumping it invalidates all queued jobs.
    pub(crate) gen: Arc<std::sync::atomic::AtomicU64>,
    /// What the web poller should watch (project, title); updated on every
    /// page install and rename.
    pub(crate) poll_target: Arc<std::sync::Mutex<(String, String)>>,
    /// Pages the poller fetched (applied by `apply_remote`).
    pub(crate) poll_rx: mpsc::Receiver<PolledPage>,
    /// The poller's sender (kept for the spawn call in `main`).
    pub(crate) poll_tx: mpsc::Sender<PolledPage>,
    /// Retunes the poller's interval while it sleeps. The push channel's
    /// state is what drives it (see `sync_state`).
    pub(crate) poll_ctrl_tx: mpsc::Sender<Duration>,
    pub(crate) poll_ctrl_rx: Option<mpsc::Receiver<Duration>>,
    /// What this session may do for the current project: sid presence,
    /// project visibility, and whatever the renderer has been refused.
    /// Never a single "authenticated" flag — see `cosense::capability`.
    pub(crate) caps: capability::Capabilities,
    /// When diagrams may be drawn (`COSENSE_WEB_RENDER`).
    pub(crate) render_policy: capability::RenderPolicy,
    /// Visibility answers from the background probe (project, verdict).
    pub(crate) vis_rx: mpsc::Receiver<(String, capability::Visibility)>,
    pub(crate) vis_tx: mpsc::Sender<(String, capability::Visibility)>,
    /// The project the probe was last asked about, so a page move inside
    /// the same project does not re-ask.
    pub(crate) vis_asked: Option<String>,
    /// Artifacts a cache-only pass looked for and did not find. They are
    /// NOT failures: they stay as source, stop shimmering, and `R` can
    /// still render them.
    pub(crate) web_missing: HashSet<String>,
    /// `R` was pressed while the page-load cache probe was still out. The
    /// probe owns those keys until it answers, so the keypress cannot queue
    /// anything yet — it is remembered here and served the moment the
    /// misses come back. Cleared per page.
    pub(crate) web_manual_wanted: bool,
    /// The "you need a cookie" notice has been shown for THIS page already.
    /// Reset by `set_page`, so it is said once per page and not per block.
    pub(crate) web_notice_shown: bool,

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
    pub(crate) src_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// Monotonic counter of everything that makes the server's state newer
    /// than a poll already in flight: a local commit landing, a websocket
    /// commit applied, a page installed. A poller stamps this when it
    /// starts a GET; if it has moved by the time the response arrives, that
    /// response is a photograph of the past and is dropped.
    pub(crate) server_epoch: Arc<std::sync::atomic::AtomicU64>,

    /// The push channel's state, TYPED. The status line renders this; the
    /// poller's interval follows it. Nothing reads the status text.
    pub(crate) sync_state: capability::SyncState,
    /// Whether a push channel is being attempted at all (a sid exists).
    /// A session without one shows `poll`, not `reconnecting`.
    pub(crate) ws_attempted: bool,

    /// Websocket push events (commit payloads, resyncs, status notes).
    pub(crate) ws_rx: mpsc::Receiver<ws::WsEvent>,
    /// The sync thread's sender (kept for the spawn call in `main`).
    pub(crate) ws_tx: mpsc::Sender<ws::WsEvent>,
    /// Resync requests toward the sync thread: only it does network I/O —
    /// a gap asks for a full-page refetch here and applies the result when
    /// it comes back. Kept for tests to inspect the requests.
    pub(crate) ws_req_tx: mpsc::Sender<ws::WsRequest>,
    pub(crate) ws_req_rx: Option<mpsc::Receiver<ws::WsRequest>>,
    /// A gap was detected (a commit whose `parentId` broke the chain): a
    /// full-page resync was requested and will arrive on `ws_rx`.
    pub(crate) ws_resync_pending: bool,
    /// A `Resynced` that arrived while an apply gate was up, plus how many
    /// buffered commits preceded it (those are superseded by the page).
    /// Only the LATEST is kept; applied when the gate drops.
    pub(crate) ws_held_resync: Option<(ws::ResyncPage, usize)>,
    /// Remote commits that arrived while an apply gate was up; applied in
    /// order once the gates drop (see `ws_flush_pending`).
    pub(crate) ws_pending: std::collections::VecDeque<RemoteCommit>,
    /// Commit id the local page model is known to be at. A commit whose
    /// `parentId` does not match it means we missed something → a
    /// background resync (see `ws_resync_pending`).
    pub(crate) ws_head: Option<String>,
    /// Last left-click (for double- and triple-click detection): time,
    /// column, row, and how many clicks this spot has counted.
    pub(crate) last_click: Option<(Instant, u16, u16, u8)>,
    /// Link pressed with the left button. Activation waits for button-up on
    /// the same target, so dragging can still select text/lines.
    pub(crate) pressed_link: Option<(usize, LinkItem)>,
    /// Related-pages sections below the body (view mode only; hidden while
    /// the edit session is open).
    pub(crate) related: Vec<RelSection>,
    /// Flattened related entries in render order. Entry `i` renders with
    /// the VIRTUAL source index `lines.len() + i`, so the cursor, Enter and
    /// mouse clicks address related rows exactly like body lines.
    pub(crate) virtual_items: Vec<LinkItem>,
    /// Terminal background is light (for related-row colors; set from Ctx).
    pub(crate) light: bool,
}

/// One project's member table and when it was fetched.
pub(crate) struct MembersCache {
    pub(crate) names: HashMap<String, String>,
    pub(crate) fetched_at: Instant,
}

/// A member table older than this is refetched on the next use: joins,
/// departures and display-name changes are rare, so a long window is fine.
pub(crate) const MEMBERS_TTL: Duration = Duration::from_secs(10 * 60);

/// An unknown userId (a member who joined after the fetch) triggers an
/// early refetch — but at most this often, so a departed member's id (which
/// will never resolve) cannot make every `t` press hit the API.
pub(crate) const MEMBERS_MISS_COOLDOWN: Duration = Duration::from_secs(60);

/// Time-machine state: the page's server-side snapshot history, and where
/// the reader currently stands in it. akapen's document timeline, backed by
/// Cosense's page-snapshots API instead of Git — the server already keeps
/// every version, so nothing is captured locally.
pub(crate) struct TimeMachine {
    /// Snapshot stamps, oldest → newest.
    pub(crate) points: Vec<cosense::api::SnapshotStamp>,
    /// Index into `points` currently shown. Stepping past the newest
    /// point exits the machine back to NOW (the live page).
    pub(crate) pos: usize,
    /// Fetched snapshots, so scrubbing back and forth is instant.
    pub(crate) cache: HashMap<String, cosense::api::Snapshot>,
}

/// A modal list/panel layered over the page.
pub(crate) enum Overlay {
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
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "3m" / "2h" / "5d" style age from an epoch-seconds timestamp.
pub(crate) fn relative_age(updated: i64) -> String {
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
    pub(crate) fn new(project: String) -> Self {
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
    pub(crate) fn here(&self) -> Place {
        match self.index.as_ref() {
            Some(index) => Place::Index {
                project: self.index_project.clone(),
                state: Box::new(index.clone()),
            },
            None => Place::Page { project: self.project.clone(), title: self.title.clone() },
        }
    }

    pub(crate) fn src_count(&self) -> usize {
        self.lines.len()
            + if self.mode == Mode::View && self.session.is_none() {
                self.virtual_items.len()
            } else {
                0
            }
    }

    /// Read state for a cursor-addressable related row. The flattening order
    /// is exactly the same as `virtual_items`.
    pub(crate) fn related_unread(&self, src: usize) -> Option<bool> {
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
    pub(crate) fn set_page(&mut self, l: Loaded, ctx: &Ctx) {
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
    pub(crate) fn start_image_loads(&mut self, ctx: &Ctx) {
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

    /// Adopt the push channel's new state: retune the poller and, when the
    /// status line is showing the session summary, refresh its `sync:` tag.
    ///
    /// The retune is what makes a stale sid cheap. The old code picked 60 s
    /// from the mere existence of a cookie; now the interval is a function
    /// of a state the websocket thread has actually demonstrated.
    pub(crate) fn set_sync_state(&mut self, st: SyncState) {
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
    pub(crate) fn refresh_sync_tag(&mut self) {
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
    pub(crate) fn sync_label(&self) -> &'static str {
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
    pub(crate) fn caret_is_inside(&self, rows: &[(usize, Line<'static>)]) -> bool {
        let Some(s) = &self.session else { return false };
        rows.iter().any(|(src, _)| *src == s.line)
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
    pub(crate) fn sync_notice(&self) -> Option<String> {
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

    pub(crate) fn hint_text(&self, cursor_links: &[LinkItem]) -> String {
        match self.sync_notice() {
            Some(tag) => format!("{tag} · {}", self.hint_body(cursor_links)),
            None => self.hint_body(cursor_links),
        }
    }

    pub(crate) fn hint_body(&self, cursor_links: &[LinkItem]) -> String {
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
    pub(crate) fn mark_desynced(&mut self) {
        self.web_unsynced = true;
    }

    /// A whole page arrived from the server and replaced the local lines,
    /// so whatever had drifted is gone. This is the ONLY way the flag
    /// clears; it must never be called on a partial or local-only update,
    /// which would re-open the window it exists to close.
    pub(crate) fn mark_synced(&mut self) {
        self.web_unsynced = false;
    }

    /// The page source has changed: any render queued before this moment
    /// would be filed under a hash it no longer matches.
    pub(crate) fn bump_src_epoch(&self) {
        self.src_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn src_epoch_now(&self) -> u64 {
        self.src_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The server's state has moved on: any poll response fetched before
    /// this moment is stale.
    pub(crate) fn bump_server_epoch(&self) {
        self.server_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn server_epoch_now(&self) -> u64 {
        self.server_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn gen_now(&self) -> u64 {
        self.web_gen.load(std::sync::atomic::Ordering::SeqCst)
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
    pub(crate) fn probe_unknown_links(&mut self) {
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
    pub(crate) fn drain_link_probes(&mut self) -> bool {
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

    pub(crate) fn drain_images(&mut self) -> bool {
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
    pub(crate) fn links_at_src(&self, src: usize) -> Vec<LinkItem> {
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

    pub(crate) fn cursor_line_links(&self) -> Vec<LinkItem> {
        self.links_at_src(self.cursor)
    }

    /// Save an uploaded file to the download directory in the background;
    /// the result lands in `file_rx` and is reported (and opened) by the
    /// event loop.
    pub(crate) fn start_download(&mut self, ctx: &Ctx, label: String, url: String) {
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
    pub(crate) fn drain_downloads(&mut self) {
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
    pub(crate) fn cursor_src(&self) -> Option<usize> {
        if self.cursor < self.lines.len() {
            Some(self.cursor)
        } else {
            None
        }
    }

    /// Index into `comments` of the comment covering the cursor line.
    pub(crate) fn comment_at_cursor(&self) -> Option<usize> {
        let src = self.cursor_src()?;
        self.comments.iter().position(|c| {
            c.project == self.project && c.title == self.title && c.covers(src)
        })
    }

    /// Browser URL for the current page, deep-linked to the cursor line.
    pub(crate) fn cursor_url(&self) -> String {
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
    pub(crate) fn ensure_members(&mut self, ctx: &Ctx, want: Option<&str>) {
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
    pub(crate) fn member_name(&self, id: &str) -> String {
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
    pub(crate) fn line_unread(&self, l: &PageLine) -> bool {
        unread_since(l.updated, self.read_at)
    }

    /// Number of unread lines on this page.
    pub(crate) fn unread_count(&self) -> usize {
        self.lines.iter().filter(|l| self.line_unread(l)).count()
    }

    /// True if a source line is covered by any saved comment.
    pub(crate) fn src_has_comment(&self, src: usize) -> bool {
        self.comments
            .iter()
            .any(|c| c.project == self.project && c.title == self.title && c.covers(src))
    }

    /// Columns available to the text for a `width`-wide body in `mode`:
    /// frame/caret + telomere + one blank column on the left, and a blank,
    /// scrollbar, and frame column on the right. Source mode also spends
    /// the line-number label.
    pub(crate) fn text_width(mode: Mode, width: u16) -> usize {
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
    pub(crate) fn source_texts(&self) -> Vec<&str> {
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
    pub(crate) fn code_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        (line < self.lines.len())
            .then(|| cosense::render::code_span_at(&self.source_texts(), line))
            .flatten()
    }

    /// The `table:` block the line belongs to, if any. A table row is not
    /// an outline row: its indent is structure and its TABs separate
    /// cells, so the session has to know which one it is standing on.
    pub(crate) fn table_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        (line < self.lines.len())
            .then(|| cosense::render::table_span_at(&self.source_texts(), line))
            .flatten()
    }

    /// A block whose lines are RAW: a `code:` block or a `table:` block.
    /// Both hold structure in their indent rather than an outline level,
    /// so the caret line is drawn without a bullet and the caret arithmetic
    /// hangs from the block's own gutter.
    pub(crate) fn raw_span_at_line(&self, line: usize) -> Option<CodeSpan> {
        self.code_span_at_line(line).or_else(|| self.table_span_at_line(line))
    }

    /// Put the caret on an edit's own location (see `edit_focus`). The
    /// session moves with it, so undo/redo show what changed instead of
    /// leaving the caret wherever it happened to be. Returns whether the
    /// line was still there to stand on.
    pub(crate) fn focus_edit(&mut self, focus: Option<(String, usize)>) -> bool {
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
    pub(crate) fn session_wrap_width(&self) -> usize {
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
    pub(crate) fn line_in_code(&self, line: usize) -> bool {
        self.code_span_at_line(line).is_some()
    }

    /// View mode: rendered blocks wrapped to the pane, each tagged with the
    /// source line it came from. The edit session's caret line renders as
    /// RAW SOURCE (cosense web: the line with the caret reveals its
    /// notation; everything else stays rendered).
    pub(crate) fn content_view(&self, width: u16) -> Vec<Row> {
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
    pub(crate) fn inline_row(
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
    pub(crate) fn related_rows(&self, text_w: usize) -> Vec<Row> {
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
    pub(crate) fn content_source(&self, width: u16) -> Vec<Row> {
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
    pub(crate) fn rebuild(&mut self, width: u16) {
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
    pub(crate) fn clamp_cursor(&mut self) {
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
    pub(crate) fn diagram_row_owner(&self, src: usize) -> Option<usize> {
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
    pub(crate) fn src_rows(&self, src: usize) -> Option<(usize, usize)> {
        let first = self.rows.iter().position(|r| r.src() == Some(src))?;
        let last = self.rows.iter().rposition(|r| r.src() == Some(src))?;
        Some((first, last))
    }

    /// The display rows of the cursor's source line (see `src_rows`).
    pub(crate) fn cursor_rows(&self) -> Option<(usize, usize)> {
        self.src_rows(self.cursor)
    }

    /// Move the cursor by one display row (j/k).
    pub(crate) fn move_cursor(&mut self, forward: bool) {
        self.move_cursor_display(if forward { 1 } else { -1 });
    }

    /// Move the cursor by `delta` display rows and snap to a source line
    /// (akapen's `move_cursor_display`): leaving downward counts from the
    /// line's LAST row and upward from its FIRST row, so wrapped
    /// continuations are jumped past. If the target row has no source
    /// (a comment card), the nearest sourced row onward in the direction
    /// of travel is taken — falling back to the other direction at the
    /// document's ends so the cursor always lands somewhere.
    pub(crate) fn move_cursor_display(&mut self, delta: isize) {
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
    pub(crate) fn row_at_screen_row(&self, screen_row: i32) -> Option<&Row> {
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
    pub(crate) fn src_at_screen_row(&self, screen_row: i32) -> Option<usize> {
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
    pub(crate) fn link_at_screen_position(&self, screen_row: i32, col: usize) -> Option<(usize, LinkItem)> {
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
    pub(crate) fn text_block_at(&self, src: usize) -> Option<(&Line<'static>, &[cosense::render::Hit])> {
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
    pub(crate) fn item_for_hit(&self, src: usize, target: &cosense::render::HitTarget) -> Option<LinkItem> {
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
    pub(crate) fn goto_src(&mut self, src: usize) {
        self.cursor = src;
        self.clamp_cursor();
        self.after_cursor_move();
    }

    /// First / last cursor-addressable source line. Related rows are source
    /// lines too for j/k navigation, but `G` deliberately targets the last
    /// BODY line because the related sections live outside the page frame.
    pub(crate) fn first_src(&self) -> Option<usize> {
        self.rows.iter().find_map(Row::src)
    }
    pub(crate) fn last_src(&self) -> Option<usize> {
        self.rows.iter().rev().find_map(Row::src)
    }
    pub(crate) fn last_body_src(&self) -> Option<usize> {
        self.rows
            .iter()
            .filter_map(Row::src)
            .filter(|&src| src < self.lines.len())
            .last()
    }

    /// Height offset of the page frame's bottom rule (`FrameEnd`).
    pub(crate) fn frame_end_top(&self) -> u16 {
        self.rows
            .iter()
            .take_while(|r| !matches!(r, Row::FrameEnd))
            .map(Row::height)
            .sum()
    }

    pub(crate) fn after_cursor_move(&mut self) {
        if let Some(s) = self.selection.as_mut() {
            s.cursor = self.cursor;
        }
        self.follow = true;
    }

    /// Y offset (in height units) of row index `i` from the top.
    pub(crate) fn row_top(&self, i: usize) -> u16 {
        self.rows[..i].iter().map(Row::height).sum()
    }

    /// Total content height in height units.
    pub(crate) fn total_height(&self) -> u16 {
        self.rows.iter().map(Row::height).sum()
    }

    /// Largest scroll offset. The top rule is one row above `rows`; the
    /// bottom page rule is an explicit FrameEnd inside `rows`, followed by
    /// any unboxed related rows. Thus the whole extent is total + 1.
    pub(crate) fn max_scroll(&self, band_h: u16) -> u16 {
        self.total_height().saturating_add(1).saturating_sub(band_h)
    }

    /// Scroll that pins the PAGE frame's bottom rule to the viewport bottom.
    /// Unlike max_scroll, this intentionally ignores related rows.
    pub(crate) fn frame_end_scroll(&self, band_h: u16) -> u16 {
        self.frame_end_top().saturating_add(2).saturating_sub(band_h)
    }

    /// Y offset of the cursor line's first display row, if it has any.
    pub(crate) fn cursor_top(&self) -> Option<u16> {
        self.cursor_rows().map(|(first, _)| self.row_top(first))
    }

    /// Keep the cursor's source line within the viewport of `body_h` rows,
    /// always leaving ONE row below the cursor's last display row for the
    /// frame's bottom rule. Scrolling up aligns the FIRST display row to
    /// the top; scrolling down keeps the LAST display row + rule row on
    /// screen. When the cursor is on the very LAST source line, the bottom
    /// rule is what matters, so the viewport goes all the way to max
    /// scroll (G does the same). akapen's `keep_cursor_visible`.
    pub(crate) fn follow_cursor(&mut self, body_h: u16) {
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
    pub(crate) fn wheel_scroll(&mut self, delta: i32, body_h: u16) {
        let max = self.max_scroll(body_h) as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max.max(0)) as u16;
        self.follow = false;
    }

    /// Re-lay out for `width` while keeping the cursor's source line on the
    /// SAME physical screen row (akapen's `replace_view_preserving_cursor`).
    /// Used by the Tab mode toggle and by width changes, so the text under
    /// the eye does not jump when wrapping or the mode changes the row
    /// count above the cursor.
    pub(crate) fn relayout_preserving_screen_row(&mut self, width: u16, body_h: u16) {
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
    pub(crate) fn cursor_fraction(&self) -> f64 {
        let max = self.lines.len().saturating_sub(1);
        if max == 0 {
            0.0
        } else {
            self.cursor as f64 / max as f64
        }
    }

    /// Build a Comment from the current selection (or cursor) + body. The
    /// selection is a SOURCE-line range, so it maps straight onto lines.
    pub(crate) fn make_comment(&self, body: String) -> Option<Comment> {
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
