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
    Line {
        line: Line<'static>,
        src: usize,
        start: usize,
        hang: usize,
    },
    Blank {
        src: usize,
    },
    /// A picture on a line of its own. `indent`: the display column it
    /// starts at, so it lines up with the text of its level. `item`: the
    /// picture IS the list item (it wears the bullet) rather than hanging
    /// under a line of text.
    Image {
        url: String,
        height: u16,
        src: usize,
        indent: usize,
        item: bool,
    },
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
        /// `(row, col, width, height, url)`: where the layout put the picture
        /// and the box it reserved for it — the real size once the picture
        /// has decoded, the placeholder's while it is still coming. The draw
        /// needs the reservation to say so (see `Row::ImageLoading`).
        images: Vec<(u16, u16, u16, u16, String)>,
        /// `(row, col, piece)`: the piece knows which part of the block
        /// and which column of that part it shows, for the click path.
        texts: Vec<(u16, u16, TextPiece)>,
    },
    /// An image still downloading in the background: one row showing the
    /// notation as written, `[URL]`, with the same brightness band a
    /// rendering diagram wears (see `ui::shimmer_across`). The picture
    /// replaces it when it lands, as a diagram replaces its code.
    ImageLoading {
        src: usize,
        indent: usize,
        item: bool,
        url: String,
    },
    ImageError {
        msg: String,
        src: usize,
        indent: usize,
        item: bool,
    },
    Card {
        line: Line<'static>,
    },
    /// A heading or spacer of the related-pages sections below the frame:
    /// plain chrome, drawn as is (no band — that is the comment card's).
    Aside {
        line: Line<'static>,
    },
    /// One row of the comment composer, woven in under the commented
    /// range like a card (akapen: the bar opens where the comment will
    /// sit). `caret`: the display column of the insertion point when this
    /// row holds it — the hardware cursor goes there, so the IME's
    /// composition window opens in the bar.
    Composer {
        line: Line<'static>,
        caret: Option<u16>,
    },
    FrameEnd,
}

impl Row {
    pub(crate) fn height(&self) -> u16 {
        match self {
            Row::Image { height, .. } | Row::Inline { height, .. } => *height,
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
            Row::Card { .. } | Row::Aside { .. } | Row::Composer { .. } | Row::FrameEnd => None,
        }
    }
}

/// View shows rendered Scrapbox notation; Source shows the raw lines with
/// numbers. Both address the same source lines, so the cursor, comments, and
/// telomere carry across the toggle (akapen's `Tab view⇄source`, now on `s`).
#[derive(Clone, Copy, Debug, PartialEq)]
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
    Page {
        project: String,
        title: String,
    },
    Index {
        project: String,
        state: Box<cosense::index::Index>,
    },
}

pub(crate) struct App {
    pub(crate) mode: Mode,
    pub(crate) project: String,
    pub(crate) title: String,
    /// Cosense project's navbar colors, adapted over the terminal background.
    pub(crate) header_colors: HeaderColors,
    /// The project's proper name (`displayName`) for the header; the slug
    /// when unknown.
    pub(crate) project_display: String,
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
    /// Clock the downloading pictures animate against — reset when new work
    /// is dispatched, so the band runs from the head of the row instead of
    /// from whatever phase the page's first paint left behind. The diagrams
    /// have their own (`web_anim`): the two are asked a different question,
    /// and one page can wait on both.
    pub(crate) image_anim: std::time::Instant,
    pub(crate) image_tx: mpsc::Sender<ImageMsg>,
    pub(crate) image_rx: mpsc::Receiver<ImageMsg>,
    /// File downloads in flight (Enter/f on a 📎 link).
    pub(crate) file_tx: mpsc::Sender<FileMsg>,
    pub(crate) file_rx: mpsc::Receiver<FileMsg>,
    /// Finished image uploads (`App::start_upload` / `drain_uploads`).
    pub(crate) upload_tx: mpsc::Sender<UploadMsg>,
    pub(crate) upload_rx: mpsc::Receiver<UploadMsg>,

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
    /// The header's site-name cells (or the lone `/` when the name has no
    /// room): the click target that opens the project's page list — the
    /// header's way of saying `^o`.
    pub(crate) header_home_rect: Rect,
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
    /// What IS: the selection hint, the history position, an upload in
    /// flight, a stopped commit worker. Stays until the state changes.
    /// What HAPPENED goes to [`App::toast`] instead (see toast.rs).
    pub(crate) status: String,
    /// The one-shot notice floating above the footer, if any.
    pub(crate) toast: Option<Toast>,
    /// When `q` was pressed once; a second press within the window quits
    /// (toast.rs `confirm_quit`). Any other key disarms it.
    pub(crate) quit_armed: Option<Instant>,
    /// The quiet level (toast.rs): a footer line and when it stops being
    /// worth showing. Ranks below `status` — it rides behind it — and
    /// above the key hints, which it gives back after a few seconds.
    pub(crate) note: Option<(String, Instant)>,
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
    /// Its proper name for the index header (slug when unknown).
    pub(crate) index_display: String,
    /// When the index was last scrolled. Its scrollbar appears briefly at
    /// the screen edge while the list moves, then gets out of the way.
    pub(crate) index_scrolled_at: Option<Instant>,
    /// The order the next `^o` opens in. The open list carries its own copy
    /// (`Index::sort`) so history restores it; this is the session's
    /// standing choice, which a fresh index has to be built from before
    /// there is an `Index` to ask.
    pub(crate) index_sort: cosense::index::SortKey,
    /// Page lists already fetched this session, one per (project, order).
    /// Switching the order in `s` reads from here while the entry is
    /// young (`INDEX_CACHE_SECS`); `^o` / `^u` always refetch and refresh
    /// it. Six orders × one 500-page request each is how the site's rate
    /// limit was being hit (see `nav::ListCache`).
    pub(crate) index_cache: HashMap<(String, cosense::index::SortKey), ListCache>,
    /// The projects list (`^o` over the page list), kept for the same
    /// `INDEX_CACHE_SECS` so stepping up and back down is not a request
    /// each time. `^o` over the projects list refetches.
    pub(crate) projects_cache: Option<ProjectsCache>,
    /// The sort menu over the index, and where its cursor is. Deliberately
    /// NOT part of the saved `Index`: a menu is something you are doing,
    /// not somewhere you have been.
    pub(crate) index_sort_menu: Option<usize>,

    /// When this page was last seen by the user before this visit — the
    /// later of Cosense's `lastAccessed` (browser) and the local visit
    /// record (this viewer). `None` = first visit anywhere: every line is
    /// unread, as on scrapbox.io.
    pub(crate) read_at: Option<i64>,

    /// When this visit began. A line edited at/after it changed while the
    /// reader is looking: web's `.updated-after-load`. It becomes that
    /// line's `read_at` floor on the NEXT open, which is how the state
    /// demotes to plain unread when the reader leaves and comes back
    /// (web: the page closing turns after-load into unread).
    pub(crate) open_stamp: i64,

    /// The body palette for THIS page: the terminal scheme tinted with the
    /// project theme's own link colours (web's `--page-link-color`). The
    /// re-render and the index preview must use it, or an edit reverts the
    /// links to the terminal scheme mid-page — the colour of a page the
    /// reader just left.
    pub(crate) palette: cosense::theme::Palette,

    /// This page's project's telomere colours (web's `--telomere-unread` /
    /// `--telomere-updated`): blue on the new themes, green on the old ones
    /// (hacker2/lgreen/mred/summer), the css fallback blue when the theme
    /// defines neither. `None` = settings unreadable or theme unknown.
    pub(crate) telomere_tint: Option<cosense::theme::TelomereTint>,

    /// Per-project member tables (userId -> display name) for line blame,
    /// fetched lazily: see `ensure_members` for when they refresh.
    pub(crate) members: HashMap<String, MembersCache>,

    /// Some while the reader is viewing a historical snapshot (←/→).
    /// The page is read-only in that state.
    pub(crate) time: Option<TimeMachine>,

    /// The live page's line ids, captured when history is entered: the
    /// NEWEST snapshot's next version is NOW, so its will-delete rows are
    /// the ids that vanish against this set.
    pub(crate) present_ids: Option<HashSet<String>>,

    /// While in history: ids of the shown snapshot's rows that the NEXT
    /// version deletes (web's `.will-delete-next` — a red telomere, no
    /// strikethrough). Recomputed on every scrub; empty at NOW.
    pub(crate) deleted_next: HashSet<String>,
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
    /// Undo/redo retained per page while navigating away. Entries are
    /// restored only when the same page is opened again.
    pub(crate) page_histories:
        HashMap<String, (Vec<(String, Vec<EditOp>)>, Vec<(String, Vec<EditOp>)>, bool)>,
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
    /// Outstanding save ownership; navigation must not clear this map.
    pub(crate) commit_origins: HashMap<CommitJobId, CommitOrigin>,
    /// Live-typing jobs the worker may skip: a newer replace of the same
    /// line has been queued behind them, so their text is already stale.
    /// Shared with the worker, which consults it when it takes a job.
    pub(crate) live_superseded: Arc<std::sync::Mutex<HashSet<CommitJobId>>>,
    /// The newest live-typing job still assumed queued, by line id. The
    /// next keystroke on that line supersedes it (see `live_superseded`).
    pub(crate) live_pending: Option<(String, CommitJobId)>,
    /// Quiet time after a keystroke before the caret line is saved, and
    /// the longest typed text may go unsaved while the keys keep coming.
    /// Saving on every key tripped the API's burst limit (a 429 costs
    /// 1+2+4 s of sleep in the serial worker, which looks like sync
    /// stopping), so keystrokes are batched: a pause saves, and a long run
    /// saves every `live_max_wait`. No key goes up alone: the first change
    /// waits like the rest, so a short sentence lands in one piece. Tests
    /// set both to zero.
    pub(crate) live_debounce: Duration,
    pub(crate) live_max_wait: Duration,
    /// When the caret line first became dirty after its last save (the
    /// max wait counts from here), and the deferred save waiting for quiet.
    pub(crate) live_dirty_since: Option<Instant>,
    pub(crate) live_due: Option<Instant>,
    /// The line whose newest undo entry is an open typing run: further
    /// live commits on it fold into that entry instead of stacking one
    /// per keystroke. Cleared by anything else that touches the history.
    pub(crate) live_undo: Option<String>,
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
    /// The page id for the page just installed, left for the websocket
    /// room joiner to pick up (`cosense::ws::PageIdHint`). Without it the
    /// joiner fetches the whole page again on every navigation purely to
    /// read an id the install already had.
    pub(crate) ws_page_hint: cosense::ws::PageIdHint,
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
    /// How diagram blocks are shown (`[view] diagrams` / `COSENSE_WEB_RENDER`).
    pub(crate) render_policy: capability::RenderPolicy,
    /// lib 検証 spike 用のテキスト段旗。手書き分支と同名・同意味。
    pub(crate) mermaid_text: bool,
    /// テキスト段の字種(`[view] diagram_text`)。起動時と設定変更時に入る。
    pub(crate) diagram_text: mmd_text::DiagramText,
    /// 数式のテキスト段旗(`COSENSE_MATH=off` で降ろす)。図とは別の
    /// ライブラリ・別のフォント事情なので、旗も分けて持つ。
    pub(crate) math_text: bool,
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
    /// The related block as it arrived, kept so `rebuild_related` can lay
    /// it out again in a new sort order without another request. `None`
    /// until it lands (and after a page install clears it).
    pub(crate) related_block: Option<cosense::api::RelatedPages>,
    /// The page-level facts the related block is read against when it
    /// arrives (see `PageFacts`).
    pub(crate) facts: PageFacts,
    /// The related block for this page is still in flight. While it is up,
    /// nothing may be concluded about the page's links — every link keeps
    /// its ordinary colour, and `probe_unknown_links` holds its questions
    /// rather than firing one request per link at a page the answer is
    /// already on its way for.
    pub(crate) related_pending: bool,
    /// The related block for this page was REFUSED (`429`), not merely
    /// missed. The prober stays shut while this is up: one lookup per link
    /// is what the block was one request instead of, and spending it while
    /// the server is holding the viewer off only keeps it held off.
    pub(crate) related_refused: bool,
    /// Whether a page install may go and fetch its related block. Set once
    /// the viewer is really running; `false` in tests, which makes
    /// `start_related_load` a no-op — installing a page in a test must not
    /// put a request on the wire. Same reason `link_probe_tx` is an
    /// `Option`.
    pub(crate) related_fetch: bool,
    /// Whether `start_upload` may put a file on the wire. Same gate, same
    /// reason: a test that pastes an image path must go nowhere.
    pub(crate) uploads_on: bool,
    pub(crate) related_tx: mpsc::Sender<RelatedMsg>,
    pub(crate) related_rx: mpsc::Receiver<RelatedMsg>,
    /// The page fetch the reader is waiting on, if any (see `PendingLoad`).
    /// Only the newest one counts: a result whose generation is older is
    /// thrown away when it arrives.
    pub(crate) pending_load: Option<PendingLoad>,
    /// Generation of the newest page fetch requested; each request takes
    /// the next number.
    pub(crate) page_load_gen: u64,
    /// Whether a page fetch may spawn a thread. `false` in tests, where the
    /// request is recorded as pending and the test feeds the answer on
    /// `page_load_tx` itself — same gate as `related_fetch`.
    pub(crate) page_loads_on: bool,
    pub(crate) page_load_tx: mpsc::Sender<PageLoadMsg>,
    pub(crate) page_load_rx: mpsc::Receiver<PageLoadMsg>,
    /// The page's snapshot stamps (oldest → newest), as fetched the first
    /// time the reader entered this page's history. `None` = nobody has
    /// asked, which is the state of every page just opened: the live page
    /// is the newest revision, so the list is not worth a request until ←
    /// is pressed (`travel`).
    pub(crate) snapshots: Option<Vec<cosense::api::SnapshotStamp>>,
    /// Flattened related entries in render order. Entry `i` renders with
    /// the VIRTUAL source index `lines.len() + i`, so the cursor, Enter and
    /// mouse clicks address related rows exactly like body lines.
    pub(crate) virtual_items: Vec<LinkItem>,
    /// Terminal background is light (for related-row colors; set from Ctx).
    pub(crate) light: bool,
    /// Where visit times persist (copied from `Ctx` at startup; `None` in tests).
    pub(crate) visits_path: Option<std::path::PathBuf>,
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
    /// The project's settings (`,`): theme, display name, upload
    /// destination — each with where its value came from. See settings.rs.
    Settings(SettingsView),
}

impl Overlay {}

/// Current wall-clock time in epoch seconds.
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "3m" / "2h" / "5d" style age from an epoch-seconds timestamp. The
/// implementation lives in the lib because the index's row column needs
/// it as well; re-exported here so the drawing code keeps the short name.
pub(crate) use cosense::index::relative_age;

impl App {
    pub(crate) fn new(project: String) -> Self {
        let (image_tx, image_rx) = mpsc::channel();
        let (file_tx, file_rx) = mpsc::channel();
        let (upload_tx, upload_rx) = mpsc::channel();
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
        let (related_tx, related_rx) = mpsc::channel();
        let (page_load_tx, page_load_rx) = mpsc::channel();
        App {
            mode: Mode::View,
            project,
            title: String::new(),
            header_colors: HeaderColors::fallback(),
            project_display: String::new(),
            page_id: String::new(),
            lines: Vec::new(),
            blocks: Vec::new(),
            srcs: Vec::new(),
            images: HashMap::new(),
            image_errors: HashMap::new(),
            pending: HashSet::new(),
            image_anim: std::time::Instant::now(),
            image_tx,
            image_rx,
            file_tx,
            upload_tx,
            upload_rx,
            file_rx,
            rows: Vec::new(),
            laid_width: 0,
            view_h: 0,
            text_rect: Rect::default(),
            bar_rect: Rect::default(),
            header_home_rect: Rect::default(),
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
            toast: None,
            quit_armed: None,
            note: None,
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
            index_display: String::new(),
            index_scrolled_at: None,
            index_sort: cosense::index::SortKey::default(),
            index_cache: HashMap::new(),
            projects_cache: None,
            index_sort_menu: None,
            web_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            web_dark: true,
            web_pending: HashSet::new(),
            web_errors: HashMap::new(),
            web_shimmer: HashMap::new(),
            web_anim: std::time::Instant::now(),
            web_cols: IMAGE_MAX_COLS,
            web_rescaling: HashSet::new(),
            web_unsynced: false,
            web_job_tx,
            web_jobs_rx: Some(web_jobs_rx),
            web_tx,
            web_rx,
            read_at: None,
            open_stamp: 0,
            palette: cosense::theme::Palette::for_light(false),
            telomere_tint: None,
            members: HashMap::new(),
            time: None,
            present_ids: None,
            deleted_next: HashSet::new(),
            editable: false,
            session_ime: cosense::ime::SessionIme::new(cosense::ime::ImeMode::Off),
            ime_ready: false,
            ime_guard: None,
            session: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_dropped: false,
            page_histories: HashMap::new(),
            create_state: CreateState::Idle,
            commit_tx,
            commit_res_rx,
            commit_jobs_rx: Some(commit_jobs_rx),
            commit_res_tx,
            inflight: 0,
            next_job_id: 1,
            commit_origins: HashMap::new(),
            live_superseded: Arc::new(std::sync::Mutex::new(HashSet::new())),
            live_pending: None,
            live_debounce: Duration::from_millis(800),
            live_max_wait: Duration::from_millis(1500),
            live_dirty_since: None,
            live_due: None,
            live_undo: None,
            own_commits: std::collections::VecDeque::new(),
            gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            poll_target: Arc::new(std::sync::Mutex::new((String::new(), String::new()))),
            ws_page_hint: Arc::new(std::sync::Mutex::new(None)),
            poll_rx,
            poll_tx,
            poll_ctrl_tx,
            poll_ctrl_rx: Some(poll_ctrl_rx),
            caps: capability::Capabilities::default(),
            render_policy: capability::RenderPolicy::from_env(),
            mermaid_text: !mmd_text::text_tier_off(),
            diagram_text: mmd_text::DiagramText::default(),
            math_text: !cosense::math::text_tier_off(),
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
            related_block: None,
            facts: PageFacts::default(),
            related_pending: false,
            related_refused: false,
            related_fetch: false,
            uploads_on: false,
            related_tx,
            related_rx,
            pending_load: None,
            page_load_gen: 0,
            page_loads_on: false,
            page_load_tx,
            page_load_rx,
            snapshots: None,
            virtual_items: Vec::new(),
            light: false,
            visits_path: None,
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

    /// The source line index under the cursor, if the page has any lines.
    pub(crate) fn cursor_src(&self) -> Option<usize> {
        if self.cursor < self.lines.len() {
            Some(self.cursor)
        } else {
            None
        }
    }

    /// The snapshot on screen, `None` at NOW. A comment written now is
    /// pinned to this.
    pub(crate) fn shown_revision(&self) -> Option<cosense::comment::Revision> {
        let tm = self.time.as_ref()?;
        let p = tm.points.get(tm.pos)?;
        Some(cosense::comment::Revision {
            snapshot_id: p.id.clone(),
            created: p.created,
        })
    }

    /// Whether a comment belongs on the screen: this page, and the
    /// revision being shown (a NOW comment is not shown on a snapshot,
    /// nor a snapshot's comment on NOW — its line numbers and quoted
    /// text are that version's, and would point at the wrong lines).
    pub(crate) fn comment_is_shown(&self, c: &Comment) -> bool {
        c.project == self.project
            && c.title == self.title
            && c.snapshot_id()
                == self
                    .time
                    .as_ref()
                    .and_then(|tm| tm.points.get(tm.pos))
                    .map(|p| p.id.as_str())
    }

    /// The comment whose range is exactly the current selection (or the
    /// cursor line when nothing is selected): `c` on it edits instead of
    /// stacking a second one.
    pub(crate) fn comment_for_range(&self) -> Option<usize> {
        let (a, b) = match self.selection {
            Some(sel) => sel.range(),
            None => (self.cursor, self.cursor),
        };
        self.comments
            .iter()
            .position(|c| self.comment_is_shown(c) && c.start == a && c.end == b)
    }

    /// True if a source line is covered by any saved comment.
    pub(crate) fn src_has_comment(&self, src: usize) -> bool {
        self.comments
            .iter()
            .any(|c| self.comment_is_shown(c) && c.covers(src))
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
        self.code_span_at_line(line)
            .or_else(|| self.table_span_at_line(line))
    }

    /// Convenience for the places that only care whether it is code.
    #[cfg(test)]
    pub(crate) fn line_in_code(&self, line: usize) -> bool {
        self.code_span_at_line(line).is_some()
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
        let Some((first, last)) = self.cursor_rows() else {
            return;
        };
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

    /// Largest scroll offset. The top rule is one row above `rows`; the
    /// bottom page rule is an explicit FrameEnd inside `rows`, followed by
    /// any unboxed related rows. Thus the whole extent is total + 1.
    pub(crate) fn max_scroll(&self, band_h: u16) -> u16 {
        self.total_height().saturating_add(1).saturating_sub(band_h)
    }

    /// Scroll that pins the PAGE frame's bottom rule to the viewport bottom.
    /// Unlike max_scroll, this intentionally ignores related rows.
    pub(crate) fn frame_end_scroll(&self, band_h: u16) -> u16 {
        self.frame_end_top()
            .saturating_add(2)
            .saturating_sub(band_h)
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
        let Some((first, last)) = self.cursor_rows() else {
            return;
        };
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

    /// Keep the cursor's rows above `avoid_y` (the toast row) by scrolling
    /// down as little as needed. `text_y` is the screen row of layout row
    /// 0 at scroll 0. A cursor line taller than the band cannot be moved
    /// clear and is left alone; so is a cursor already scrolled off-screen.
    pub(crate) fn keep_cursor_above(&mut self, text_y: u16, avoid_y: u16, band_h: u16) {
        let Some((_, last)) = self.cursor_rows() else {
            return;
        };
        let bottom = self.row_top(last) + self.rows[last].height(); // exclusive
        if bottom <= self.scroll {
            return; // above the viewport: not under anything
        }
        let last_y = text_y as i32 + (bottom - 1) as i32 - self.scroll as i32;
        if last_y < avoid_y as i32 {
            return;
        }
        let need = (last_y - avoid_y as i32 + 1) as u16;
        self.scroll = self
            .scroll
            .saturating_add(need)
            .min(self.max_scroll(band_h));
    }

    /// While composing, keep the whole composer bar on screen: it sits
    /// under the commented range, which `follow_cursor` alone may leave
    /// below the fold when the range ends near the bottom (akapen's
    /// `keep_composer_visible_view`).
    pub(crate) fn keep_composer_visible(&mut self, body_h: u16) {
        let Some(last) = self
            .rows
            .iter()
            .rposition(|r| matches!(r, Row::Composer { .. }))
        else {
            return;
        };
        let first = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Composer { .. }))
            .unwrap_or(last);
        let top = self.row_top(first);
        let bottom = self.row_top(last) + 1;
        if bottom > self.scroll + body_h {
            self.scroll = bottom - body_h;
        }
        if top < self.scroll {
            self.scroll = top;
        }
        // Taller than the pane even so (a very small window): the caret's
        // row is the one that must be seen.
        if bottom - top > body_h {
            if let Some(ci) = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Composer { caret: Some(_), .. }))
            {
                let cy = self.row_top(ci);
                if cy < self.scroll {
                    self.scroll = cy;
                } else if cy + 1 > self.scroll + body_h {
                    self.scroll = cy + 1 - body_h;
                }
            }
        }
        self.scroll = self
            .scroll
            .min(self.max_scroll(body_h).max(bottom.saturating_sub(body_h)));
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
        let (a, b) = self
            .selection
            .map(|s| s.range())
            .unwrap_or((self.cursor, self.cursor));
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
            page_id: self.page_id.clone(),
            revision: self.shown_revision(),
            start: a,
            end: b,
            line_texts,
            line_ids,
            text: body,
        })
    }
}
