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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Bumped whenever OUR capture pipeline changes in a way that alters the
/// pixels for unchanged source: the viewport width, the capture scale, the
/// selector, or the readiness condition. It is part of the cache key, so a
/// bump orphans every old artifact rather than showing a stale one.
///
/// It cannot cover the OTHER half of the rendering environment — Cosense's
/// own Mermaid version and the project's CSS, which change without telling
/// us. Nothing in the page identifies them, so a complete identity is not
/// available; `artifact_ttl` bounds how long we trust an artifact instead.
pub const ARTIFACT_SCHEMA: u32 = 1;

/// Default lifetime of a cached PNG. Long enough that ordinary re-reading
/// of a page never launches a browser, short enough that a Cosense-side
/// renderer or ProjectCSS change works itself out without the reader
/// knowing there is a cache. `COSENSE_WEB_CACHE_TTL_DAYS` overrides it;
/// `0` disables the disk cache's reuse entirely.
pub const ARTIFACT_TTL_DAYS: u64 = 7;

pub fn artifact_ttl() -> Duration {
    let days = std::env::var("COSENSE_WEB_CACHE_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|d| *d <= 3650)
        .unwrap_or(ARTIFACT_TTL_DAYS);
    Duration::from_secs(days * 24 * 60 * 60)
}

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
    /// line — not its `code:` header (see NOTE-webrender-handoff.md).
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
        h.write(&ARTIFACT_SCHEMA.to_le_bytes());
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
            WebError::NoBrowser => write!(f, "{}", crate::ts!("Chrome が見つかりません（COSENSE_CHROME で指定できます）", "no Chrome found (set COSENSE_CHROME)")),
            WebError::Timeout { seconds } => write!(f, "{}", crate::t!("ブラウザが {seconds} 秒でタイムアウトしました", "browser timed out after {seconds}s")),
            WebError::NotRendered => write!(f, "{}", crate::ts!("Cosense 側が図を描きませんでした", "diagram not drawn by Cosense")),
            WebError::NotAuthorized => write!(f, "{}", crate::ts!("このログインではページを見られません", "page not visible to this login")),
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
    ///
    /// `auth` says whether the session cookie may be presented. It is a
    /// parameter rather than backend state because the same session
    /// legitimately renders a public page anonymously right after a private
    /// one — and because a cookie the server has rejected must be dropped
    /// without tearing down the rest of the app's credentials.
    fn render_batch(
        &self,
        reqs: &[WebRequest],
        auth: crate::capability::RenderCapability,
    ) -> Vec<Result<Vec<u8>, WebError>>;

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
    fn render_batch(
        &self,
        reqs: &[WebRequest],
        _auth: crate::capability::RenderCapability,
    ) -> Vec<Result<Vec<u8>, WebError>> {
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
    /// The `auth` of the most recent call, so a test can prove which
    /// credential state the renderer actually asked for.
    pub last_auth: std::sync::Mutex<Option<crate::capability::RenderCapability>>,
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
            last_auth: std::sync::Mutex::new(None),
        }
    }

    pub fn answer(&self, key: &str, res: Result<Vec<u8>, WebError>) {
        self.answers.lock().unwrap().insert(key.to_string(), res);
    }
}

impl WebBackend for FakeBackend {
    fn render_batch(
        &self,
        reqs: &[WebRequest],
        auth: crate::capability::RenderCapability,
    ) -> Vec<Result<Vec<u8>, WebError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_auth.lock().unwrap() = Some(auth);
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
///
/// `Clone` shares the same directory: it is a handle, not the data. Cloning
/// re-runs no setup — `at` already hardened and swept it.
#[derive(Clone)]
pub struct ArtifactCache {
    dir: PathBuf,
}

/// Temp files live in the cache directory (rename is only atomic within a
/// filesystem) and are named so a sweep can tell them from artifacts.
const TMP_PREFIX: &str = "incoming-";

impl ArtifactCache {
    pub fn new() -> Self {
        Self::at(cache_root().join("cosense-tui").join("webrender"))
    }

    /// A cache rooted anywhere — tests point it at a scratch directory so a
    /// run never reads or writes the user's real cache.
    pub fn at(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        let cache = Self { dir };
        cache.harden_dir();
        cache.sweep();
        cache
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// These PNGs are renders of the user's pages, including private ones
    /// reached with their `connect.sid`. On a shared machine the directory
    /// must not be world- or group-readable.
    fn harden_dir(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700)).ok();
        }
    }

    /// One pass over the directory that repairs and prunes:
    ///   * artifacts written before this hardening (or with a lax umask)
    ///     get their permissions tightened;
    ///   * artifacts past the TTL, and empty ones, are removed;
    ///   * artifacts orphaned by a schema bump are removed by the same TTL
    ///     rule, since nothing will ever ask for their key again;
    ///   * temp files left by an interrupted write are removed once they
    ///     are too old to belong to a live writer.
    fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let ttl = artifact_ttl();
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or_default();
            if name.starts_with(TMP_PREFIX) {
                // A writer that is still running owns its temp file; an
                // hour is far longer than any render.
                if age > Duration::from_secs(3600) {
                    std::fs::remove_file(&path).ok();
                }
                continue;
            }
            if meta.len() == 0 || age > ttl {
                std::fs::remove_file(&path).ok();
                continue;
            }
            harden_file(&path);
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        // The key is `web:<kind>:<hex>`; ':' is legal on the platforms we
        // target but reads badly in a directory, so flatten it.
        self.dir.join(format!("{}.png", key.replace(':', "-")))
    }

    /// The cached PNG, if there is a fresh one. An entry past the TTL is
    /// removed and reported as a miss, so the caller re-renders it.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.path(key);
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() == 0 {
            std::fs::remove_file(&path).ok();
            return None;
        }
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|age| age <= artifact_ttl())
            .unwrap_or(true);
        if !fresh {
            std::fs::remove_file(&path).ok();
            return None;
        }
        std::fs::read(&path).ok().filter(|b| !b.is_empty())
    }

    /// Drop an entry — used when what came back off disk would not decode.
    /// A corrupt artifact must never become a permanent failure: the caller
    /// re-renders it in a browser instead.
    pub fn remove(&self, key: &str) {
        std::fs::remove_file(self.path(key)).ok();
    }

    /// Write an artifact so that no reader — this process, another viewer,
    /// or the next run after a crash — can ever observe a partial PNG:
    /// a private temp file in the same directory, then an atomic rename.
    pub fn put(&self, key: &str, png: &[u8]) {
        if png.is_empty() {
            return;
        }
        let Some((tmp, mut file)) = self.open_tmp() else { return };
        let written = file
            .write_all(png)
            // The rename is atomic, but only the bytes that reached the
            // disk survive a power cut; a torn PNG would then be cached
            // forever (until the TTL), so pay for the sync.
            .and_then(|()| file.sync_all())
            .is_ok();
        drop(file);
        if !written || std::fs::rename(&tmp, self.path(key)).is_err() {
            std::fs::remove_file(&tmp).ok();
        }
    }

    /// A uniquely named, private (0600) file in the cache directory.
    /// `create_new` means an existing name — or a symlink planted at one —
    /// is a failure, never a silent overwrite of somebody else's target.
    fn open_tmp(&self) -> Option<(PathBuf, std::fs::File)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        for _ in 0..8 {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0)
                ^ N.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let tmp = self.dir.join(format!(
                "{TMP_PREFIX}{}-{nonce:016x}",
                std::process::id()
            ));
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            if let Ok(f) = opts.open(&tmp) {
                return Some((tmp, f));
            }
        }
        None
    }
}

/// Tighten an artifact that a previous version (or a lax umask) left
/// readable by group or other.
fn harden_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else { return };
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o700)).ok();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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

use crate::url::encode_component as enc;

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
    use crate::capability::RenderCapability;

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

    fn scratch(tag: &str) -> ArtifactCache {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "cosense-tui-cachetest-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&dir).ok();
        ArtifactCache::at(dir)
    }

    #[cfg(unix)]
    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn the_cache_is_private_and_repairs_what_it_finds() {
        use std::os::unix::fs::PermissionsExt;
        let c = scratch("perm");
        c.put("web:mermaid:aaaa", b"png-bytes");
        assert_eq!(mode_of(c.dir()), 0o700, "the directory holds renders of private pages");
        let entry = c.dir().join("web-mermaid-aaaa.png");
        assert_eq!(mode_of(&entry), 0o600);

        // An artifact left world-readable by an older version is repaired
        // the next time the cache is opened, not left as it was.
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(c.dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let reopened = ArtifactCache::at(c.dir().to_path_buf());
        assert_eq!(mode_of(reopened.dir()), 0o700);
        assert_eq!(mode_of(&entry), 0o600);
        assert_eq!(reopened.get("web:mermaid:aaaa").as_deref(), Some(&b"png-bytes"[..]));
    }

    #[test]
    fn a_reader_never_sees_a_half_written_artifact() {
        // Writes go to a private temp file and are renamed into place, so a
        // concurrent reader observes either the old bytes or the new ones —
        // never a prefix of the new ones.
        let c = std::sync::Arc::new(scratch("atomic"));
        let key = "web:mermaid:concurrent";
        let a = vec![b'a'; 300_000];
        let b = vec![b'b'; 300_000];
        c.put(key, &a);
        let writer = {
            let (c, b) = (std::sync::Arc::clone(&c), b.clone());
            std::thread::spawn(move || {
                for _ in 0..40 {
                    c.put("web:mermaid:concurrent", &b);
                }
            })
        };
        for _ in 0..200 {
            if let Some(got) = c.get(key) {
                assert!(
                    got == a || got == b,
                    "a torn artifact reached a reader ({} bytes)",
                    got.len()
                );
            }
        }
        writer.join().unwrap();
        // The temp files all got renamed or cleaned up; nothing is left
        // lying around to be mistaken for an artifact.
        let leftovers: Vec<String> = std::fs::read_dir(c.dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(TMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    #[test]
    fn empty_and_expired_entries_are_dropped_rather_than_served() {
        let c = scratch("ttl");
        // An empty file is never a valid PNG; it must not be handed out.
        std::fs::write(c.dir().join("web-mermaid-empty.png"), b"").unwrap();
        assert!(c.get("web:mermaid:empty").is_none());
        assert!(!c.dir().join("web-mermaid-empty.png").exists(), "and it is removed");

        // Past the TTL an artifact is a miss: Cosense's own Mermaid version
        // and the project CSS can change without anything in the key moving.
        c.put("web:mermaid:old", b"png-bytes");
        let path = c.dir().join("web-mermaid-old.png");
        let ancient =
            SystemTime::now() - artifact_ttl() - Duration::from_secs(3600);
        filetime_set(&path, ancient);
        assert!(c.get("web:mermaid:old").is_none(), "expired entries are misses");
        assert!(!path.exists());

        // Inside the TTL it is served.
        c.put("web:mermaid:new", b"png-bytes");
        assert!(c.get("web:mermaid:new").is_some());
    }

    #[test]
    fn the_ttl_is_configurable_and_bounded() {
        let key = "COSENSE_WEB_CACHE_TTL_DAYS";
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        assert_eq!(artifact_ttl(), Duration::from_secs(ARTIFACT_TTL_DAYS * 86400));
        std::env::set_var(key, "1");
        assert_eq!(artifact_ttl(), Duration::from_secs(86400));
        std::env::set_var(key, "0");
        assert_eq!(artifact_ttl(), Duration::ZERO, "0 disables reuse");
        std::env::set_var(key, "999999");
        assert_eq!(
            artifact_ttl(),
            Duration::from_secs(ARTIFACT_TTL_DAYS * 86400),
            "nonsense falls back to the default"
        );
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn a_corrupt_entry_can_be_dropped_so_it_is_re_rendered() {
        let c = scratch("corrupt");
        // Truncated: a real PNG signature with nothing behind it.
        c.put("web:mermaid:trunc", &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert!(c.get("web:mermaid:trunc").is_some(), "bytes are there…");
        assert!(
            image::load_from_memory(&c.get("web:mermaid:trunc").unwrap()).is_err(),
            "…but they do not decode"
        );
        // The worker's response to that is to drop the entry, which turns
        // the next request back into a browser render rather than a
        // permanently cached failure.
        c.remove("web:mermaid:trunc");
        assert!(c.get("web:mermaid:trunc").is_none());
        // Removing something absent is not an error.
        c.remove("web:mermaid:never-existed");
    }

    #[test]
    fn a_schema_bump_orphans_old_artifacts_instead_of_serving_them() {
        // The schema is part of the key, so nothing can ask for the old
        // one again; the TTL sweep is what eventually reclaims the file.
        let k = req().cache_key();
        let mut h = Fnv::new();
        for part in ["mermaid", "help-jp", "Mermaid", "65695a556db42200239324b9", "65695bc797c2910000c699b2"] {
            h.write(part.as_bytes());
            h.write(b"\x1f");
        }
        h.write(&req().code_hash.to_le_bytes());
        h.write(&[1u8]);
        h.write(&(ARTIFACT_SCHEMA + 1).to_le_bytes());
        assert_ne!(k, format!("web:mermaid:{:016x}", h.0));
    }

    /// Backdate a file's mtime. No filetime crate: this is the one syscall
    /// the TTL tests need.
    fn filetime_set(path: &Path, when: SystemTime) {
        #[cfg(unix)]
        {
            let secs = when
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let tv = libc::timeval { tv_sec: secs, tv_usec: 0 };
            let times = [tv, tv];
            let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) }, 0);
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
        let out = b.render_batch(&[req(), req()], RenderCapability::Anonymous);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Err(WebError::NoBrowser)));
        // The reason is told in the reader's own language.
        crate::lang::set_for_thread(crate::lang::Lang::En);
        assert_eq!(WebError::NoBrowser.to_string(), "no Chrome found (set COSENSE_CHROME)");
        crate::lang::set_for_thread(crate::lang::Lang::Ja);
        assert_eq!(
            WebError::NoBrowser.to_string(),
            "Chrome が見つかりません（COSENSE_CHROME で指定できます）"
        );
    }

    #[test]
    fn fake_backend_answers_by_key() {
        let f = FakeBackend::new();
        f.answer(&req().cache_key(), Ok(vec![1, 2, 3]));
        let mut other = req();
        other.line_id = "zzz".into();
        let out = f.render_batch(&[req(), other], RenderCapability::Anonymous);
        assert_eq!(out[0].as_ref().unwrap(), &vec![1, 2, 3]);
        assert!(matches!(out[1], Err(WebError::NotRendered)));
    }
}
