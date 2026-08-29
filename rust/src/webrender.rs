//! Web renderer: turn a *live Cosense page* into images for constructs the
//! TUI cannot draw itself.
//!
//! The first construct is Mermaid (`code:mmd` / `code:mermaid` /
//! `code:<name>.mmd`). Cosense renders those in the browser — it dynamically
//! imports a hashed Mermaid chunk and injects the resulting SVG into a
//! `div.mermaid-preview` — and there is no public "source -> SVG" REST API.
//! So instead of re-implementing Mermaid locally, we open the *real page* in
//! a headless browser and screenshot the element Cosense itself drew.
//!
//! The point of this module is the boundary, not Mermaid: a `WebRequest`
//! goes in, a PNG (or a `WebError`) comes out. TeX, `.icon` rows and
//! ProjectCSS-styled blocks are future `WebKind`s behind the same trait,
//! and the TUI never learns anything about browsers.
//!
//! Security notes that must survive refactoring:
//!   * the `connect.sid` cookie is passed to the backend out of band (never
//!     in argv, never in a cache key, never in an error string);
//!   * nothing here executes page script or SVG in-process — the browser
//!     does the rendering, and we only ever handle the resulting PNG bytes.

use std::fmt;
use std::path::PathBuf;

/// What kind of construct a request wants drawn. One variant today; the
/// contract exists so TeX / `.icon` can be added without touching the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebKind {
    Mermaid,
}

impl WebKind {
    /// Short tag used in cache keys and status messages.
    pub fn tag(self) -> &'static str {
        match self {
            WebKind::Mermaid => "mermaid",
        }
    }

    /// The DOM element Cosense draws this construct into, addressed by the
    /// *line id* it hangs off. Verified live on scrapbox.io/help-jp/Mermaid:
    /// `div.mermaid-preview#mermaid-preview-<lineId>` sits inside
    /// `div.line#L<lineId>`, and that line is the code block's LAST content
    /// line — not its `code:` header (see HANDOFF.md).
    pub fn selector(self, line_id: &str) -> String {
        match self {
            WebKind::Mermaid => format!("#mermaid-preview-{line_id}"),
        }
    }

    /// The element must contain this before the capture is meaningful.
    /// `.mermaid-preview` exists in the DOM from the first paint; the `<svg>`
    /// only appears once the dynamic Mermaid import has actually run.
    pub fn ready_child(self) -> &'static str {
        match self {
            WebKind::Mermaid => "svg",
        }
    }
}

/// One "draw this for me" order. Everything that can change the picture is a
/// field, so `cache_key` is also the staleness guard: a different page, line,
/// block source or theme is a different key and can never be shown for the
/// current one. Note what is deliberately absent — the page's commit id and
/// the pane width; see `cache_key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRequest {
    pub kind: WebKind,
    pub project: String,
    pub title: String,
    /// Immutable page id: guards against a title that was renamed under us.
    pub page_id: String,
    /// Cosense line id the construct hangs off (`PageLine.id`).
    pub line_id: String,
    /// Hash of the block's OWN source text. This — NOT the page's commit id
    /// — is what invalidates a diagram. The page commit moves on every
    /// keystroke-commit anywhere on the page (a newline included), so keying
    /// on it re-rendered every diagram whenever an unrelated line was
    /// edited. A block's own text is exactly as precise as the picture it
    /// produces.
    pub code_hash: u64,
    /// Terminal background is dark (picks the browser color scheme).
    pub dark: bool,
}

impl WebRequest {
    /// The page the browser must open.
    pub fn page_url(&self) -> String {
        format!(
            "https://scrapbox.io/{}/{}",
            enc(&self.project),
            enc(&self.title)
        )
    }

    pub fn selector(&self) -> String {
        self.kind.selector(&self.line_id)
    }

    /// Stable identity for this artifact: the in-memory map key, the on-disk
    /// cache filename, and the staleness test all in one.
    ///
    /// Contains NO credential — see `cache_key_never_carries_a_credential`.
    pub fn cache_key(&self) -> String {
        let mut h = Fnv::new();
        for part in [
            self.kind.tag(),
            &self.project,
            &self.title,
            &self.page_id,
            &self.line_id,
        ] {
            h.write(part.as_bytes());
            h.write(b"\x1f");
        }
        h.write(&self.code_hash.to_le_bytes());
        h.write(&[self.dark as u8]);
        // The `web:` prefix keeps these apart from image URLs, which share
        // the viewer's image maps.
        format!("web:{}:{:016x}", self.kind.tag(), h.0)
    }

    /// NOTE on what is deliberately NOT here: the pane width. The viewer
    /// scales every image to a fixed cell width (`build_image` caps at 64
    /// columns and never consults the pane), so the browser's viewport
    /// affects only raster quality — never what the reader sees. Keying on
    /// it meant a ten-column resize cost a 3–6 second re-render, and
    /// dragging a window edge queued one batch per width crossed, which the
    /// worker then ground through serially while the final width waited at
    /// the back. The backend renders at a fixed viewport instead.

    /// Do two requests address the same artifact? Used to drop results that
    /// came back after the reader moved on.
    pub fn same_artifact(&self, other: &WebRequest) -> bool {
        self.cache_key() == other.cache_key()
    }
}

/// Why a render did not produce pixels. Every variant is recoverable: the
/// viewer falls back to the plain code block and shows a short note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebError {
    /// No Chrome/Chromium found (and `COSENSE_CHROME` unset).
    NoBrowser,
    /// The browser never produced the element — page still loading, the
    /// diagram has a syntax error Cosense refused to draw, or the DOM moved.
    Timeout { seconds: u64 },
    /// Chrome ran but the element is not in the DOM at all.
    NotRendered,
    /// The page did not load for this credential (private project, no/stale
    /// sid). Never carries the credential itself.
    NotAuthorized,
    /// Anything else, already stripped of credentials by the backend.
    Backend(String),
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebError::NoBrowser => write!(f, "no Chrome found (set COSENSE_CHROME)"),
            WebError::Timeout { seconds } => write!(f, "browser timed out after {seconds}s"),
            WebError::NotRendered => write!(f, "diagram not drawn by Cosense"),
            WebError::NotAuthorized => write!(f, "page not visible to this login"),
            WebError::Backend(m) => write!(f, "{m}"),
        }
    }
}

/// A finished render, tagged with the key it was requested under so a late
/// arrival can be matched (or dropped) without trusting delivery order.
#[derive(Clone)]
pub struct WebArtifact {
    pub key: String,
    /// PNG bytes. The viewer decodes these with the same path it uses for
    /// downloaded images; no SVG or HTML is ever interpreted in-process.
    pub png: Vec<u8>,
}

impl fmt::Debug for WebArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebArtifact")
            .field("key", &self.key)
            .field("png_len", &self.png.len())
            .finish()
    }
}

/// Anything that can turn a `WebRequest` into PNG bytes. The real one drives
/// headless Chrome (`crate::chrome`); tests use `FakeBackend`.
///
/// Implementations block, and are only ever called from the render worker
/// thread — never from the UI thread.
pub trait WebBackend: Send + Sync {
    /// Render one batch of requests that all target the SAME page, so a
    /// single navigation serves them all. Results are returned per request,
    /// in the same order.
    fn render_batch(&self, reqs: &[WebRequest]) -> Vec<Result<Vec<u8>, WebError>>;

    /// No work has arrived for a while. A backend that keeps a browser warm
    /// between batches releases it here, so an idle viewer holds no browser
    /// process. The next request starts a fresh one.
    fn idle(&self) {}

    /// Release any browser process. Called once on shutdown; must not panic
    /// and must leave no orphan behind.
    fn shutdown(&self) {}
}

/// A backend that always fails the same way — what the viewer gets when no
/// browser could be located, so the code-block fallback is exercised for
/// real rather than special-cased.
pub struct UnavailableBackend(pub WebError);

impl WebBackend for UnavailableBackend {
    fn render_batch(&self, reqs: &[WebRequest]) -> Vec<Result<Vec<u8>, WebError>> {
        reqs.iter().map(|_| Err(self.0.clone())).collect()
    }
}

/// Scripted backend for tests: answers by cache key, and counts calls so a
/// test can prove the UI never waited on it.
pub struct FakeBackend {
    answers: std::sync::Mutex<std::collections::HashMap<String, Result<Vec<u8>, WebError>>>,
    /// Held for the duration of every `render_batch`; a test can lock it to
    /// pin the worker mid-render and check the UI still draws.
    pub gate: std::sync::Mutex<()>,
    pub calls: std::sync::atomic::AtomicUsize,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            answers: std::sync::Mutex::new(std::collections::HashMap::new()),
            gate: std::sync::Mutex::new(()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn answer(&self, key: &str, res: Result<Vec<u8>, WebError>) {
        self.answers.lock().unwrap().insert(key.to_string(), res);
    }
}

impl WebBackend for FakeBackend {
    fn render_batch(&self, reqs: &[WebRequest]) -> Vec<Result<Vec<u8>, WebError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _held = self.gate.lock().unwrap();
        let answers = self.answers.lock().unwrap();
        reqs.iter()
            .map(|r| {
                answers
                    .get(&r.cache_key())
                    .cloned()
                    .unwrap_or(Err(WebError::NotRendered))
            })
            .collect()
    }
}

/// On-disk PNG cache, shared with the image cache's directory layout.
pub struct ArtifactCache {
    dir: PathBuf,
}

impl ArtifactCache {
    pub fn new() -> Self {
        Self::at(cache_root().join("cosense-tui").join("webrender"))
    }

    /// A cache rooted anywhere — tests point it at a scratch directory so a
    /// run never reads or writes the user's real cache.
    pub fn at(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        Self { dir }
    }

    fn path(&self, key: &str) -> PathBuf {
        // The key is `web:<kind>:<hex>`; ':' is legal on the platforms we
        // target but reads badly in a directory, so flatten it.
        self.dir.join(format!("{}.png", key.replace(':', "-")))
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(key)).ok().filter(|b| !b.is_empty())
    }

    pub fn put(&self, key: &str, png: &[u8]) {
        if png.is_empty() {
            return;
        }
        std::fs::write(self.path(key), png).ok();
    }
}

impl Default for ArtifactCache {
    fn default() -> Self {
        Self::new()
    }
}

fn cache_root() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

/// Percent-encode a path segment the way scrapbox.io's own URLs do.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// FNV-1a. Small, stable across runs (unlike `DefaultHasher`), which matters
/// because these keys name files on disk.
pub struct Fnv(pub u64);

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a code block's source text for `WebRequest.code_hash`.
pub fn hash_code(code: &str) -> u64 {
    let mut h = Fnv::new();
    h.write(code.as_bytes());
    h.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> WebRequest {
        WebRequest {
            kind: WebKind::Mermaid,
            project: "help-jp".into(),
            title: "Mermaid".into(),
            page_id: "65695a556db42200239324b9".into(),
            line_id: "65695bc797c2910000c699b2".into(),
            code_hash: hash_code("flowchart LR\nA-->B"),
            dark: true,
        }
    }

    #[test]
    fn selector_addresses_the_preview_by_line_id() {
        // Verified live: div.mermaid-preview#mermaid-preview-<lineId>.
        assert_eq!(req().selector(), "#mermaid-preview-65695bc797c2910000c699b2");
        assert_eq!(WebKind::Mermaid.ready_child(), "svg");
    }

    #[test]
    fn each_block_on_a_page_gets_its_own_key() {
        let a = req();
        let mut b = req();
        b.line_id = "65695d8097c2910000c699e8".into();
        assert_ne!(a.cache_key(), b.cache_key());
        assert!(!a.same_artifact(&b));
    }

    #[test]
    fn a_new_source_or_page_is_a_different_artifact() {
        let base = req();
        for mutate in [
            (|r: &mut WebRequest| r.title = "Other".into()) as fn(&mut WebRequest),
            |r| r.project = "other-project".into(),
            |r| r.page_id = "other-page".into(),
            |r| r.dark = false,
            |r| r.code_hash = hash_code("flowchart LR\nA-->C"),
        ] {
            let mut v = base.clone();
            mutate(&mut v);
            assert_ne!(base.cache_key(), v.cache_key(), "{v:?} must not reuse the base key");
        }
        // …and an identical request is the same artifact (cache hits work).
        assert!(base.same_artifact(&req()));
    }

    #[test]
    fn resizing_the_pane_is_not_a_new_artifact() {
        // There is no width in a request at all: the same diagram in a
        // narrow and a wide pane is one artifact, rendered once.
        let a = req();
        let b = req();
        assert_eq!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn cache_key_never_carries_a_credential() {
        // The sid lives in the backend, not the request — but a future field
        // could leak into the key, so assert the shape stays credential-free.
        let k = req().cache_key();
        assert!(k.starts_with("web:mermaid:"));
        assert!(k.len() < 40, "key is a hash, not a transcript: {k}");
        assert!(!k.contains("connect.sid"));
        assert!(k
            .trim_start_matches("web:mermaid:")
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn page_url_escapes_the_title() {
        let mut r = req();
        r.title = "テスト ページ".into();
        let url = r.page_url();
        assert!(url.starts_with("https://scrapbox.io/help-jp/"));
        assert!(!url.contains(' '));
        assert!(url.contains("%E3%83%86")); // テ
    }

    #[test]
    fn unavailable_backend_fails_every_request_without_a_browser() {
        let b = UnavailableBackend(WebError::NoBrowser);
        let out = b.render_batch(&[req(), req()]);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Err(WebError::NoBrowser)));
        assert_eq!(WebError::NoBrowser.to_string(), "no Chrome found (set COSENSE_CHROME)");
    }

    #[test]
    fn fake_backend_answers_by_key() {
        let f = FakeBackend::new();
        f.answer(&req().cache_key(), Ok(vec![1, 2, 3]));
        let mut other = req();
        other.line_id = "zzz".into();
        let out = f.render_batch(&[req(), other]);
        assert_eq!(out[0].as_ref().unwrap(), &vec![1, 2, 3]);
        assert!(matches!(out[1], Err(WebError::NotRendered)));
    }
}
