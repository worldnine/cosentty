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
//   click     move the cursor to that line   drag  select a range
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

use cosense::api::{Client, Config, PageLine};
use cosense::comment::{format_all, Comment, Selection};
use cosense::image_fetch::ImageFetcher;
use cosense::highlight::Highlighter;
use cosense::render::{file_name_of_url, gyazo_permalink, is_scrapbox_file_url, render_lines_with, Block};
use cosense::wrap::{hanging_prefix, wrap_line, wrap_line_continued};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

const SEL_BG: Color = Color::Rgb(60, 60, 90);
/// Source mode's line-number gutter: `"  12 "` (4 digits + space).
const SOURCE_NUM_W: usize = 5;
/// Cursor marker in the left gutter column (akapen's `>`).
const CURSOR_FG: Color = Color::Rgb(130, 170, 255);
const CURSOR_BG: Color = Color::Rgb(80, 80, 120);
const CARD_BG: Color = Color::Rgb(38, 42, 54);

struct ImageInfo {
    /// Sliced protocol: renders row-by-row so partial vertical scroll clips
    /// cleanly (SignedPosition.y may be negative) instead of vanishing.
    sliced: SlicedProtocol,
    /// Display height in cells (from the sliced size), used for layout.
    cells_h: u16,
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
}

/// A laid-out visual row. Content rows carry their source line; card rows
/// are synthesized (not selectable, no source).
enum Row {
    Line { line: Line<'static>, src: usize },
    Blank { src: usize },
    Image { url: String, height: u16, src: usize },
    /// Space reserved for an image still downloading in the background.
    ImageLoading { src: usize },
    ImageError { msg: String, src: usize },
    Card { line: Line<'static> },
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
            Row::Card { .. } => None,
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

struct App {
    mode: Mode,
    project: String,
    title: String,
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

    rows: Vec<Row>,
    laid_width: u16,
    /// Viewport height (text rows) as of the last frame; movement keys use
    /// it to keep the cursor in view without waiting for the next draw.
    view_h: u16,
    /// Screen geometry as of the last frame, for mouse hit-testing: the
    /// text area and the scrollbar column.
    text_rect: Rect,
    bar_x: u16,
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
    composing: Option<String>,
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
        App {
            mode: Mode::View,
            project,
            title: String::new(),
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
            bar_x: 0,
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
            read_at: None,
            members: HashMap::new(),
        }
    }

    /// Install a freshly loaded page, resetting view state (keeps comments).
    /// Install a freshly loaded page and immediately kick off its image
    /// downloads. Loading is folded in here so no navigation path can forget
    /// it (back/forward included).
    fn set_page(&mut self, l: Loaded, ctx: &Ctx) {
        self.project = l.project;
        self.title = l.title;
        self.lines = l.lines;
        self.blocks = l.blocks;
        self.srcs = l.srcs;
        self.read_at = l.read_at;
        self.images.clear();
        self.image_errors.clear();
        self.pending.clear();
        self.laid_width = 0; // force rebuild
        self.scroll = 0;
        self.cursor = 0;
        self.selection = None;
        self.follow = true;
        self.start_image_loads(ctx);
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
                    .and_then(|img| build_image(&picker, img));
                let _ = tx.send((url, res));
            });
        }
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

    /// Everything Enter/f can follow on the cursor's line: internal page
    /// links first, then uploaded files, then external URLs.
    fn cursor_line_links(&self) -> Vec<LinkItem> {
        let Some(text) = self.cursor_src().and_then(|s| self.lines.get(s)).map(|l| l.text.as_str()) else {
            return Vec::new();
        };
        let mut items: Vec<LinkItem> = links_on_line(text);
        for (label, url) in labelled_urls(text) {
            items.push(if is_scrapbox_file_url(&url) {
                LinkItem::File { label, url }
            } else if let Some(page) = gyazo_permalink(&url) {
                // An inline image: jump to its gyazo page (comments, the
                // original, the Teams org), not the raw pixels.
                let label = if label == url { "gyazo".to_string() } else { label };
                LinkItem::Url { label, url: page }
            } else {
                LinkItem::Url { label, url }
            });
        }
        items
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
    /// a 1-column gutter on the left and a 1-column scrollbar track on the
    /// right; source mode also spends the line-number label.
    fn text_width(mode: Mode, width: u16) -> usize {
        let w = width as usize;
        // Left: border(1) + telomere(1) + pad(1). Right: thumb(1) + border(1).
        match mode {
            Mode::View => w.saturating_sub(5).max(1),
            Mode::Source => w.saturating_sub(5 + SOURCE_NUM_W).max(1),
        }
    }

    /// View mode: rendered blocks wrapped to the pane, each tagged with the
    /// source line it came from.
    fn content_view(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::View, width);
        let mut content: Vec<Row> = Vec::new();
        for (b, &src) in self.blocks.iter().zip(self.srcs.iter()) {
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
                    for line in t.layout(text_w) {
                        content.push(Row::Line { line, src });
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
        // 1. content rows — rendered blocks, or raw numbered source lines.
        let content: Vec<Row> = match self.mode {
            Mode::View => self.content_view(width),
            Mode::Source => self.content_source(width),
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
        self.rows = rows;
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
        if self.lines.is_empty() || self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = self.cursor.min(self.lines.len() - 1);
        if self.cursor_rows().is_some() {
            return;
        }
        let up = (0..self.cursor).rev().find(|&s| self.src_rows(s).is_some());
        let down = (self.cursor + 1..self.lines.len()).find(|&s| self.src_rows(s).is_some());
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

    /// The source line rendered at `screen_row` (0-based inside the text
    /// area) under the current scroll — `None` on a card row or past the
    /// end. Mouse clicks map through here.
    fn src_at_screen_row(&self, screen_row: u16) -> Option<usize> {
        let want = self.scroll as u32 + screen_row as u32;
        let mut y = 0u32;
        for row in &self.rows {
            let h = row.height() as u32;
            if want < y + h {
                return row.src();
            }
            y += h;
        }
        None
    }

    /// Put the cursor on source line `src` (clamped / snapped to a rendered
    /// line), extending any selection and scheduling a viewport follow.
    fn goto_src(&mut self, src: usize) {
        self.cursor = src;
        self.clamp_cursor();
        self.after_cursor_move();
    }

    /// First / last source line that has display rows (g / G targets).
    fn first_src(&self) -> Option<usize> {
        self.rows.iter().find_map(Row::src)
    }
    fn last_src(&self) -> Option<usize> {
        self.rows.iter().rev().find_map(Row::src)
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

    /// Largest scroll offset. The frame (content + 2 rule rows) scrolls as
    /// a whole; at max scroll the bottom rule sits on the band's last row:
    /// scroll = text.y + total - (body.y + band_h - 1) = total + 2 - band_h
    /// (text.y = body.y + 1).
    fn max_scroll(&self, band_h: u16) -> u16 {
        self.total_height().saturating_add(2).saturating_sub(band_h)
    }

    /// Y offset of the cursor line's first display row, if it has any.
    fn cursor_top(&self) -> Option<u16> {
        self.cursor_rows().map(|(first, _)| self.row_top(first))
    }

    /// Keep the cursor's source line within the viewport of `body_h` rows:
    /// scrolling up aligns its FIRST display row to the top, scrolling down
    /// aligns its LAST display row (wrapped continuations, card rows and
    /// tall images included) to the bottom. akapen's `keep_cursor_visible`.
    fn follow_cursor(&mut self, body_h: u16) {
        let body_h = body_h.max(1);
        let Some((first, last)) = self.cursor_rows() else { return };
        let top = self.row_top(first);
        let bottom = self.row_top(last) + self.rows[last].height();
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + body_h {
            self.scroll = bottom - body_h;
        }
        self.scroll = self.scroll.min(self.max_scroll(body_h));
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

/// Render a comment as inline card lines (indented, distinct background).
fn card_lines(c: &Comment, width: usize) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4).max(10);
    let bar = "─".repeat(inner);
    let cs = Style::default().bg(CARD_BG).fg(Color::Rgb(180, 190, 210));
    let accent = Style::default().bg(CARD_BG).fg(Color::Rgb(130, 170, 255));
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
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--theme" => theme = it.next(),
            "--light" => force_light = Some(true),
            "--dark" => force_light = Some(false),
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

    let cfg = Config { project: project.clone(), sid: sid.clone(), api_domain: "scrapbox.io".into() };
    let client = Client::new(cfg)?;

    let title = match title {
        Some(t) => t,
        None => {
            let (_, pages) = client.list_pages(1, 0, "updated")?;
            pages.first().map(|p| p.title.clone()).ok_or("empty project")?
        }
    };

    let fetcher = Arc::new(ImageFetcher::new(gyazo_token, sid)?);

    let mut terminal = ratatui::init();
    // Wheel scrolling moves the viewport (akapen parity); failure to enable
    // mouse reporting only loses that, so it is not fatal.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // Detect light/dark after entering the alt screen (unless forced).
    let light = force_light.unwrap_or_else(|| cosense::theme::detect_light().unwrap_or(false));
    let hl = Highlighter::new(theme.as_deref(), light);
    // One color scheme for the whole page: headings, links, quotes and
    // code labels take the theme's markdown colors (akapen parity).
    let palette = cosense::theme::Palette::from_theme(&hl, light);
    let picker = Picker::from_query_stdio()?;

    let ctx = Ctx { client, hl, palette, picker, fetcher, light };
    let loaded = load_page(&ctx, &project, &title)?;

    let mut app = App::new(project);
    app.set_page(loaded, &ctx);
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
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

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
}

/// Everything produced by loading one page.
struct Loaded {
    project: String,
    title: String,
    lines: Vec<PageLine>,
    blocks: Vec<Block>,
    srcs: Vec<usize>,
    /// See `App::read_at`.
    read_at: Option<i64>,
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
    Ok(Loaded {
        project: project.to_string(),
        title: title.to_string(),
        lines,
        blocks: rendered.blocks,
        srcs: rendered.srcs,
        read_at,
    })
}

/// Turn a decoded image into a sliced protocol at a width-capped cell size.
/// Runs on the image worker thread (`Picker` is a plain clone of the
/// terminal's capabilities).
fn build_image(picker: &Picker, dyn_img: image::DynamicImage) -> Result<ImageInfo, String> {
    let font = picker.font_size();
    let (px_w, px_h) = (dyn_img.width(), dyn_img.height());
    let nat_cols = (px_w as f32 / font.width as f32).ceil() as u32;
    let nat_rows = (px_h as f32 / font.height as f32).ceil() as u32;
    let max_cols = 64u32;
    let (cw, ch) = if nat_cols > max_cols {
        let scale = max_cols as f32 / nat_cols as f32;
        (max_cols, ((nat_rows as f32) * scale).ceil() as u32)
    } else {
        (nat_cols.max(1), nat_rows.max(1))
    };
    let size = Size::new(cw as u16, ch as u16);
    SlicedProtocol::new(picker, dyn_img, Some(size))
        .map(|sliced| {
            let cells_h = sliced.size().height.max(1);
            ImageInfo { sliced, cells_h }
        })
        .map_err(|e| e.to_string())
}

/// Page links on a raw Scrapbox line, left to right: `[PageName]` (not a
/// URL, decoration, or icon) and `#hashtag` in the current project, and
/// `[/project/PageName]` into another project. A bare `[/project]` (the
/// project's top page) is skipped: it is not a page.
fn links_on_line(text: &str) -> Vec<LinkItem> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        if let Some(rel) = rest[start + 1..].find(']') {
            let end = start + 1 + rel;
            let inner = &rest[start + 1..end];
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
                            out.push(LinkItem::ProjectPage {
                                project: project.to_string(),
                                title: title.to_string(),
                            });
                        }
                    }
                } else {
                    out.push(LinkItem::Page(inner.to_string()));
                }
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    // hashtags
    let mut hrest = text;
    while let Some(pos) = hrest.find('#') {
        let ok = pos == 0
            || hrest[..pos].chars().next_back().map(|c| c.is_whitespace()).unwrap_or(false);
        let tag: String = hrest[pos + 1..].chars().take_while(|c| !c.is_whitespace()).collect();
        if ok && !tag.is_empty() {
            out.push(LinkItem::Page(tag.clone()));
        }
        hrest = &hrest[pos + 1..];
    }
    out
}

/// Every http(s) URL on a raw Scrapbox line, as `(label, url)`, left to
/// right. The bracket form `[title https://…]` (either order) labels the
/// URL with `title`; a bare URL is labelled by its file name for uploads
/// and by the URL itself otherwise.
fn labelled_urls(text: &str) -> Vec<(String, String)> {
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
        out.push((label, url.to_string()));
    }
    out
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

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    loop {
        // Install any images that finished downloading, then draw.
        if app.drain_images() {
            app.laid_width = 0; // heights changed — rebuild layout
        }
        app.drain_downloads();
        terminal.draw(|f| ui(f, app, ctx))?;
        // Wait up to one tick for input (short, so arriving images refresh
        // promptly), then drain everything that queued up into ONE frame.
        // A wheel flick — or herdr, which delivers bursts at once — queues
        // dozens of mouse events; a redraw per event (images included)
        // made scrolling crawl. akapen's event_loop does the same.
        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        for _ in 0..MAX_EVENTS_PER_FRAME {
            if !event::poll(Duration::ZERO)? {
                break;
            }
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if handle_key(app, ctx, k) {
                        return Ok(());
                    }
                }
                Event::Mouse(m) => handle_mouse(app, ctx, m),
                _ => {}
            }
        }
    }
}

/// One key press. Returns true to quit.
fn handle_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> bool {
    // Composer captures all keys.
    if let Some(buf) = app.composing.as_mut() {
        match k.code {
            KeyCode::Esc => {
                app.composing = None;
                app.status = "comment cancelled".into();
            }
            KeyCode::Enter => {
                let body = buf.clone();
                if body.trim().is_empty() {
                    app.composing = None;
                    app.status = "empty comment discarded".into();
                } else if let Some(c) = app.make_comment(body) {
                    app.comments.push(c);
                    app.composing = None;
                    app.selection = None;
                    app.status = format!("comment saved ({} total)", app.comments.len());
                    app.laid_width = 0; // force rebuild to weave the card
                } else {
                    app.composing = None;
                    app.status = "could not anchor comment".into();
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        }
        return false;
    }

    // Overlays capture keys while open.
    if app.overlay.is_some() {
        handle_overlay_key(app, ctx, k.code, k.modifiers);
        return false;
    }

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match (k.code, ctrl) {
        // ---- quit ---- (akapen default: q quits, Esc only cancels)
        (KeyCode::Char('q'), false) => return true,
        (KeyCode::Esc, false) => {
            if app.selection.is_some() {
                app.selection = None;
                app.status = "selection cleared".into();
            } else {
                app.status.clear();
            }
        }

        // ---- move (akapen parity) ----
        (KeyCode::Char('j'), false) | (KeyCode::Down, _) => app.move_cursor(true),
        (KeyCode::Char('k'), false) | (KeyCode::Up, _) => app.move_cursor(false),
        (KeyCode::Char('g'), false) => {
            if let Some(s) = app.first_src() {
                app.goto_src(s);
            }
        }
        (KeyCode::Char('G'), false) => {
            if let Some(s) = app.last_src() {
                app.goto_src(s);
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

        // ---- open in browser (akapen's `e edit` slot) ----
        (KeyCode::Char('e'), false) => {
            let url = app.cursor_url();
            app.status = if open_in_browser(&url) {
                format!("opened {url}")
            } else {
                "failed to open browser".into()
            };
        }

        // ---- comments (akapen parity) ----
        (KeyCode::Char('v'), false) => {
            if app.selection.is_some() {
                app.selection = None;
                app.status = "selection cleared".into();
            } else {
                app.selection = Some(Selection::new(app.cursor));
                app.status = "selecting — j/k extend, c comment".into();
            }
        }
        (KeyCode::Char('c'), false) => {
            app.composing = Some(String::new());
            app.status = "type comment, Enter save, Esc cancel".into();
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

        // Unbound: show what arrived, so a terminal that rewrites a
        // shortcut (e.g. a Cmd-key) can be diagnosed from the status bar.
        _ => app.status = format!("unbound key: {:?} {:?}", k.code, k.modifiers),
    }
    false
}

/// Mouse handling (akapen parity).
///
/// - Wheel: the viewport alone moves, one row per event; the cursor keeps
///   its line (scrolling back finds it where it was). Over an overlay the
///   wheel moves the overlay's cursor instead.
/// - Left click on a line: cursor there, selection dropped. Dragging
///   selects from the pressed line to the line under the pointer.
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
    handle_mouse_content(app, m);
}

/// Mouse over the page body (no overlay open): wheel, click, drag,
/// scrollbar. Uses the geometry the last frame recorded in
/// `App::text_rect` / `App::bar_x`.
fn handle_mouse_content(app: &mut App, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => {
            app.drag_anchor = None;
            app.wheel_scroll(1, app.view_h);
            return;
        }
        MouseEventKind::ScrollUp => {
            app.drag_anchor = None;
            app.wheel_scroll(-1, app.view_h);
            return;
        }
        _ => {}
    }
    if app.composing.is_some() {
        return;
    }

    let text = app.text_rect;
    let in_rows = m.row >= text.y && m.row < text.y + text.height;
    let screen_row = m.row.saturating_sub(text.y);
    // the pointer may leave the rows while dragging: clamp it back in
    let clamped_row = m.row.clamp(text.y, (text.y + text.height).saturating_sub(1)) - text.y;
    let total = app.total_height() as usize;
    let view_h = text.height as usize;
    let on_track = m.column == app.bar_x
        && in_rows
        && cosense::theme::scroll_thumb(total, view_h, app.scroll as usize).is_some();

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if on_track => {
            if let Some(off) = cosense::theme::scroll_offset_at(total, view_h, screen_row as usize) {
                app.scroll = off as u16;
                app.follow = false;
                app.scrollbar_drag = Some((screen_row, off as u16));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            let (start_row, start_off) = app.scrollbar_drag.unwrap();
            if let Some(off) = cosense::theme::scroll_offset_drag(
                total,
                view_h,
                start_row as usize,
                start_off as usize,
                clamped_row as usize,
            ) {
                app.scroll = off as u16;
                app.follow = false;
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            app.scrollbar_drag = None;
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Any column left of the scrollbar counts (gutter included):
            // the row is what selects the line.
            if in_rows && m.column < app.bar_x {
                if let Some(src) = app.src_at_screen_row(screen_row) {
                    app.selection = None;
                    app.drag_anchor = Some(src);
                    app.goto_src(src);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(anchor) = app.drag_anchor {
                if let Some(end) = app.src_at_screen_row(clamped_row) {
                    app.selection = Some(Selection { anchor, cursor: end });
                    app.goto_src(end);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
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
    // hop is visible without any notion of a "home" project.
    f.render_widget(
        Paragraph::new(Line::from(format!(
            " {}/{}{}  ({} comment(s)){}{} ",
            app.project,
            app.title,
            if app.mode == Mode::Source { "  [source]" } else { "" },
            app.comments.len(),
            unread,
            sel_info
        )))
        .style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // help / status: a leading position readout (akapen's `VIEW L23/118`),
    // then the contextual hint — numbered links when the cursor line has
    // them, else the static key list, else a transient status message.
    let mode_tag = if app.mode == Mode::Source { "SRC" } else { "VIEW" };
    let pos = if app.lines.is_empty() {
        "L-/-".to_string()
    } else {
        let cur = app.cursor_src().map(|s| s + 1).unwrap_or(1).min(app.lines.len());
        format!("L{}/{}", cur, app.lines.len())
    };
    let cur_links = app.cursor_line_links();
    let hint = if !cur_links.is_empty() {
        let listed: Vec<String> = cur_links
            .iter()
            .take(9)
            .enumerate()
            .map(|(i, l)| format!("{}:{}", i + 1, l.label()))
            .collect();
        format!("Enter/f open → {}", listed.join("  "))
    } else if app.status.is_empty() {
        "j/k move  Enter link  [/] back/fwd  Tab source  e browser  c comment  ? help  q quit"
            .to_string()
    } else {
        app.status.clone()
    };
    f.render_widget(
        Paragraph::new(format!(" {} {} · {}", mode_tag, pos, hint))
            .style(Style::default().fg(Color::DarkGray)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    // akapen-style frame: the page is boxed. The frame floats ONE column
    // off the terminal's left edge (akapen's `margin = 1`) so the markers
    // riding the border — the telomere (or cursor `>`) REPLACES the block
    // border glyph there — never touch the screen edge. The text column
    // then sits one more column in (`▊ text`), and the right border carries
    // the scrollbar thumb only, never a full track.
    // The frame: page boxed with box-drawing borders. body.x sits flush at
    // the screen's left edge (no margin column). The telomere / cursor
    // markers live in their own column INSIDE the left border, padded from
    // the text; the right border carries the scrollbar thumb only.
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(2),
    );
    // Layout (inside the box): border │, telomere column, pad, text, pad,
    // thumb column, border │.
    let gutter_x = body.x + 1; // telomere / `>` / comment column
    let bar_x = body.x + body.width.saturating_sub(2); // thumb column
    let text = Rect::new(
        body.x + 2,
        body.y + 1,
        body.width.saturating_sub(4),
        body.height.saturating_sub(2),
    );

    // The frame HUGS THE CONTENT: the whole frame (top rule, content rows,
    // bottom rule) moves together as `-scroll`. Content row n sits at
    // text.y + n - scroll; the rules sit one above the first row and one
    // below the last. Rules scroll off-screen with the page; the visible
    // band is body.y .. area.y+area.height-2. Everything uses the SAME
    // coordinate system, so the thumb, cursor-follow and the rules agree.
    let total_h = app.total_height() as i32;
    let top_rule = text.y as i32 - 1 - app.scroll as i32;
    // The last content row is at text.y + (total_h - 1) - scroll; the
    // bottom rule sits one below it.
    let bot_rule = text.y as i32 + total_h - app.scroll as i32;
    let band_top = body.y as i32;
    let band_bot = (area.y + area.height - 2) as i32; // one above status
    let band_h = (band_bot - band_top + 1).max(1) as u16; // visible rows
    {
        let buf = f.buffer_mut();
        let frame_style = Style::default().fg(cosense::theme::border_color(ctx.light));
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
    app.bar_x = bar_x;

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
    // The visible band holds band_h rows; the content plus rules scroll
    // through it. Rows are tested against `text.y + n - scroll` below.
    let view_bottom = view_top + band_h as i32;
    let sel_range = app.selection.map(|s| s.range());
    // Gutter cells to paint after the rows: (screen row, glyph, style).
    let mut gutter: Vec<(u16, &'static str, Style)> = Vec::new();

    let mut y = 0i32;
    for row in app.rows.iter() {
        let h = row.height() as i32;
        let top = y;
        let bottom = y + h;
        y = bottom;
        if bottom <= view_top || top >= view_bottom {
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
        let mut base = Style::default();
        if in_sel {
            base = base.bg(SEL_BG);
        }
        if is_cursor {
            base = base.bg(CURSOR_BG);
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
                    let (glyph, mut style) = gutter_cell(is_cursor, has_comment, age, ctx.light);
                    if let Some(bg) = base.bg {
                        style = style.bg(bg);
                    }
                    gutter.push((y as u16, glyph, style));
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
            Row::Line { line, .. } | Row::Card { line } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(Paragraph::new(line.clone()).style(base), r);
                }
            }
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
                    let pos = SignedPosition { x: 1, y: screen_y as i16 };
                    // SlicedImage draws relative to the area's top-left; give
                    // it the full band so y = screen_y lands on the same row
                    // the text uses.
                    let img_area = Rect::new(
                        text.x,
                        text.y,
                        text.width,
                        (band_bot - text.y as i32 + 1) as u16,
                    );
                    f.render_widget(SlicedImage::new(&info.sliced, pos), img_area);
                }
            }
        }
    }

    // Gutter column: painted after the rows. Rows without a source line
    // (cards, past the end) leave it blank. (`sy` is already the absolute
    // screen row.)
    let buf = f.buffer_mut();
    for (sy, glyph, style) in gutter {
        if let Some(c) = buf.cell_mut((gutter_x, sy)) {
            c.set_symbol(glyph);
            c.set_style(style);
        }
    }
    // Scrollbar: ONLY the thumb is drawn, in its own column just inside the
    // frame's right border — there is no always-on track, so non-thumb rows
    // show just the clean `│` border (akapen: `…▐│` only where the thumb
    // is, never a double line). The thumb tracks the VIEWPORT offset (wheel
    // scroll moves the viewport only); when the content fits, nothing is
    // drawn and the border stays clean.
    // Scrollbar: ONLY the thumb is drawn, in its own column just inside the
    // frame's right border. The scrollable extent is the whole frame
    // (content + 2 rule rows), and the thumb tracks the offset over a track
    // of `band_h` rows (the visible band), so at max scroll the thumb sits
    // at the band's bottom exactly where the bottom rule appears.
    let thumb = Style::default().fg(cosense::theme::scrollbar_thumb(ctx.light));
    if let Some((start, len)) = cosense::theme::scroll_thumb(
        (app.total_height() + 2) as usize,
        band_h as usize,
        app.scroll as usize,
    ) {
        let mut i = start as u16;
        for _ in start..start + len {
            if let Some(c) = buf.cell_mut((bar_x, band_top as u16 + i)) {
                c.set_symbol("▐");
                c.set_style(thumb);
            }
            i += 1;
        }
    }

    // Composer overlay at the bottom.
    if let Some(buf) = &app.composing {
        let h = 3u16;
        let r = Rect::new(area.x, area.y + area.height.saturating_sub(1 + h), area.width, h);
        f.render_widget(Clear, r);
        let (a, b) = app.selection.map(|s| s.range()).unwrap_or((app.cursor, app.cursor));
        let label = if a == b {
            format!(" comment on line {} ", a + 1)
        } else {
            format!(" comment on lines {}-{} ", a + 1, b + 1)
        };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(label, Style::default().fg(Color::Black).bg(Color::Green))),
                Line::from(format!("> {buf}▏")),
                Line::from(Span::styled("Enter save · Esc cancel", Style::default().fg(Color::DarkGray))),
            ]),
            r,
        );
    }

    // Modal overlay (comments list / link picker / help) on top.
    if app.overlay.is_some() {
        draw_overlay(f, app, area);
    }
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
        Some(Overlay::Help) => (
            "keys".into(),
            vec![
                "move      j/k · g/G · ^u/^d · PgUp/PgDn".into(),
                "link      Enter/f open: page · 📎 file → ~/Downloads · ↗ URL → browser".into(),
                "history   [ back · ] forward".into(),
                "mode      Tab view⇄source".into(),
                "pages     ^o recent pages (type to filter)".into(),
                "browser   e open page at cursor line".into(),
                "comment   v select · c add · d delete · ^n/^p jump".into(),
                "detail    t who/when edited this line".into(),
                "output    y copy all comments".into(),
                "list      l comments · ? help".into(),
                "quit      q  (Esc cancels; comments print to stdout)".into(),
            ],
            usize::MAX,
        ),
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
        Style::default().fg(Color::Black).bg(Color::Cyan),
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
            Style::default().bg(CURSOR_BG).add_modifier(Modifier::BOLD)
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
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(CARD_BG)),
        panel,
    );
}

/// The 1-column gutter cell for a row with a source line: the cursor `>`
/// wins, otherwise the telomere bar (`age` = seconds since the edit, plus
/// whether the line is unread — see `theme::telomere`). akapen's
/// marker-column precedence.
///
/// A comment marker is deliberately NOT drawn yet: akapen colors comments
/// yellow (`▌`) and reserves green for changed lines, so guessing a color
/// before the telomere/comment balance is settled would bake in a wrong
/// convention. `has_comment` is kept so the caller can still tint the row
/// without touching the marker column.
fn gutter_cell(
    is_cursor: bool,
    _has_comment: bool,
    age: Option<(i64, bool)>,
    light: bool,
) -> (&'static str, Style) {
    if is_cursor {
        (">", Style::default().fg(CURSOR_FG).add_modifier(Modifier::BOLD))
    } else {
        match age {
            Some((a, unread)) => {
                let (glyph, color) = cosense::theme::telomere(a, unread, light);
                (glyph, Style::default().fg(color))
            }
            None => (" ", Style::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(texts: &[&str]) -> App {
        let mut app = App::new("proj".into());
        app.title = "t".into();
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
    fn cursor_is_a_source_line_spanning_all_wrapped_rows() {
        let mut app = page(&["short", &"x".repeat(50), "tail"]);
        app.rebuild(22); // text width 20 -> the 50-char line wraps to 3 rows
        assert_eq!(app.cursor, 0);
        app.move_cursor(true);
        assert_eq!(app.cursor, 1);
        let (first, last) = app.cursor_rows().unwrap();
        assert_eq!((first, last), (1, 3), "all wrapped rows belong to the line");
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
    fn cursor_on_rowless_table_body_snaps_to_its_header() {
        let mut app = page(&["table:t", "\ta\tb", "\tc\td", "after"]);
        app.rebuild(40);
        assert!(app.src_rows(1).is_none(), "table body lines render into the header's block");
        app.goto_src(2);
        assert_eq!(app.cursor, 0, "snaps up to the line that owns the block");
        // the last line is reachable and the clamp respects the end
        app.goto_src(99);
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn follow_cursor_uses_last_row_going_down_and_first_going_up() {
        let mut app = page(&["a", "b", "c", &"y".repeat(50), "e"]);
        app.rebuild(22); // line 3 wraps to 3 rows: rows 3,4,5
        app.goto_src(3);
        app.follow_cursor(4);
        // rows 3..=5 must be inside a 4-row viewport -> scroll = 6 - 4
        assert_eq!(app.scroll, 2);
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
        app.rebuild(22); // rows: a, b, b, b, c
        assert_eq!(app.src_at_screen_row(0), Some(0));
        assert_eq!(app.src_at_screen_row(2), Some(1), "wrapped continuation -> its line");
        assert_eq!(app.src_at_screen_row(4), Some(2));
        assert_eq!(app.src_at_screen_row(5), None, "past the end");
        app.scroll = 3;
        assert_eq!(app.src_at_screen_row(0), Some(1));
        assert_eq!(app.src_at_screen_row(1), Some(2));
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    /// A 30-line page laid out in a 40x10 text area at (1,1) under a
    /// 1-row header, with the scrollbar at column 41.
    fn page_with_screen() -> App {
        let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 1, 40, 10);
        app.bar_x = 41;
        app.view_h = 10;
        app
    }

    #[test]
    fn click_moves_cursor_and_drag_selects_a_line_range() {
        let mut app = page_with_screen();
        app.scroll = 5;
        // press on screen row 2 -> display row 7 -> line 7
        handle_mouse_content(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 10, 3));
        assert_eq!(app.cursor, 7);
        assert_eq!(app.selection, None, "a click alone selects nothing");
        // drag down to screen row 6 -> line 11: selection 7..=11
        handle_mouse_content(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 12, 7));
        assert_eq!(app.selection.map(|s| s.range()), Some((7, 11)));
        assert_eq!(app.cursor, 11);
        // dragging back above the anchor flips the range
        handle_mouse_content(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 12, 1));
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        handle_mouse_content(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 12, 1));
        assert_eq!(app.drag_anchor, None);
        // the selection survives the release (c comments on it)
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        // a click outside the rows (header) changes nothing
        handle_mouse_content(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 10, 0));
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn scrollbar_click_and_drag_scrub_the_viewport_only() {
        let mut app = page_with_screen();
        app.goto_src(3);
        // 30 rows / 10 viewport: thumb 3 rows, track rows 0..=7, max offset 20
        handle_mouse_content(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 41, 1 + 7));
        assert_eq!(app.scroll, 20, "track bottom -> last page");
        assert_eq!(app.cursor, 3, "the cursor keeps its line");
        assert!(!app.follow);
        handle_mouse_content(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 41, 1 + 3));
        assert!(app.scroll < 20 && app.scroll > 0);
        handle_mouse_content(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 41, 1 + 3));
        assert_eq!(app.scrollbar_drag, None);
        // wheel over the body: one row per event, cursor untouched
        handle_mouse_content(&mut app, mouse(MouseEventKind::ScrollUp, 10, 5));
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
