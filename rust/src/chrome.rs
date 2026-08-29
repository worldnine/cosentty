//! Headless-Chrome backend for `crate::webrender`, spoken over raw CDP.
//!
//! Why raw CDP and not Playwright/Puppeteer/a browser crate: the viewer is a
//! single Rust binary and already depends on `tungstenite` (websocket, for
//! Cosense's push channel) and `reqwest` (http). CDP is exactly those two
//! plus a child process, so the whole backend costs ZERO new dependencies
//! and no Node runtime at install time. A crate like `headless_chrome` or
//! `chromiumoxide` would pull an async runtime and a large tree for the four
//! commands we actually send. See HANDOFF.md.
//!
//! The flow mirrors what Puppeteer's `elementHandle.screenshot` does:
//! navigate, wait for the element Cosense drew, read its bounding box, then
//! `Page.captureScreenshot` clipped to that box. No full-page screenshot is
//! guessed at and cropped.

use crate::capability::RenderCapability;
use crate::webrender::{WebBackend, WebError, WebRequest};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Chrome executables we look for, after `COSENSE_CHROME`.
const MAC_CANDIDATES: [&str; 4] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

const UNIX_CANDIDATES: [&str; 6] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "brave-browser",
];

/// Locate a Chrome/Chromium binary. `COSENSE_CHROME` always wins, so an
/// unusual install (or a Nix/flatpak path) needs no code change.
pub fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COSENSE_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        // An explicit-but-wrong setting should not silently fall back to a
        // different browser than the user asked for.
        return None;
    }
    if cfg!(target_os = "macos") {
        for c in MAC_CANDIDATES {
            let p = PathBuf::from(c);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for name in UNIX_CANDIDATES {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Viewport width the page is rendered at, in CSS pixels. Fixed on purpose:
/// the viewer scales every image to a cell width of its own, so the browser
/// window size only decides raster quality. Making it follow the pane meant
/// re-rendering (3–6s) on every modest resize. 1000px is roughly the width
/// Cosense itself lays a page out at, and at `clip.scale = 2` it yields a
/// 2000px capture — far more than the ~500px a 64-column image ever needs.
const RENDER_WIDTH_PX: u32 = 1000;

/// How long one batch (launch → navigate → all captures) may take.
fn budget() -> Duration {
    let secs = std::env::var("COSENSE_WEB_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s >= 3 && *s <= 300)
        .unwrap_or(25);
    Duration::from_secs(secs)
}

pub struct ChromeBackend {
    exe: PathBuf,
    /// Browser `connect.sid` for private projects. Passed to Chrome only via
    /// a CDP `Network.setCookie` call — never argv, never a log, never a key.
    sid: Option<String>,
    /// PID of the Chrome we currently own, so shutdown can reap it even if
    /// the app quits while a batch is in flight.
    live_pid: Mutex<Option<u32>>,
    /// The last browser this backend owned, kept even after shutdown so a
    /// smoke run can prove that specific process and profile are gone
    /// rather than counting every Chrome on the machine.
    last_owned: Mutex<Option<(u32, PathBuf)>>,
    /// The browser, kept alive between batches. Relaunching per batch cost
    /// ~1.4s of startup AND threw away Chrome's warm HTTP/V8 caches, which
    /// is most of what makes a Cosense page slow to draw: measured on
    /// help-jp/Mermaid, navigate+draw is ~4.6s cold and ~2.5s in a browser
    /// that has already loaded the site once. The worker closes it after an
    /// idle spell (`idle`), and quitting kills it (`shutdown`).
    session: Mutex<Option<Session>>,
    /// Set once the app is quitting: an in-flight batch stops early instead
    /// of holding the exit open for the full budget.
    stopped: std::sync::atomic::AtomicBool,
}

/// A live browser: the process, its DevTools socket and its throwaway
/// profile. Dropping it reaps all three.
struct Session {
    child: Child,
    cdp: Cdp,
    /// Which credential state this browser was built for. Chrome keeps the
    /// cookie in its profile, so a session that has ever been handed the
    /// sid cannot be reused for an anonymous render: switching modes throws
    /// the whole browser away (Drop reaps the profile with it).
    auth: RenderCapability,
    /// Removed when the session drops. Chrome's helper processes outlive
    /// the SIGKILL on their parent by a moment and keep files open in
    /// here, so removal is retried briefly rather than attempted once.
    profile: tempfile::TempDir,
}

impl Drop for Session {
    fn drop(&mut self) {
        kill(&mut self.child);
        let path = self.profile.path().to_path_buf();
        for _ in 0..40 {
            if std::fs::remove_dir_all(&path).is_ok() || !path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl ChromeBackend {
    /// `None` when no browser could be found — the caller then installs
    /// `UnavailableBackend(WebError::NoBrowser)` and the viewer keeps
    /// showing plain code blocks.
    pub fn detect(sid: Option<String>) -> Option<Self> {
        find_chrome().map(|exe| Self {
            exe,
            sid,
            live_pid: Mutex::new(None),
            last_owned: Mutex::new(None),
            session: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn run_batch(
        &self,
        reqs: &[WebRequest],
        auth: RenderCapability,
    ) -> Result<Vec<Result<Vec<u8>, WebError>>, WebError> {
        self.check_running()?;
        let width = RENDER_WIDTH_PX;
        let dark = reqs.first().map(|r| r.dark).unwrap_or(true);

        let mut held = self.session.lock().unwrap();
        // Reuse only a browser built for the SAME credential state.
        if held.as_ref().is_some_and(|s| s.auth != auth) {
            *held = None;
            *self.live_pid.lock().unwrap() = None;
        }
        let reused = held.is_some();
        if held.is_none() {
            *held = Some(self.launch(width, auth)?);
        }
        let deadline = Instant::now() + budget();
        let out = self.drive(held.as_mut().unwrap(), reqs, width, dark, deadline);
        if out.is_err() {
            // A CDP error can leave the socket half-consumed, so a session
            // that failed is never reused: it is reaped here and the next
            // batch starts a fresh browser.
            *held = None;
            *self.live_pid.lock().unwrap() = None;
            if reused && !self.stopped.load(Ordering::Relaxed) {
                // The browser we inherited may simply have died (crash, OOM,
                // the machine slept). One clean retry before giving up.
                *held = Some(self.launch(width, auth)?);
                let deadline = Instant::now() + budget();
                let retry = self.drive(held.as_mut().unwrap(), reqs, width, dark, deadline);
                if retry.is_err() {
                    *held = None;
                    *self.live_pid.lock().unwrap() = None;
                }
                return retry;
            }
        }
        out
    }

    /// Refuse to do anything once the app is quitting. Checked on the way
    /// into a batch and again immediately before a browser is spawned, so
    /// `shutdown` cannot be raced into starting a Chrome that then outlives
    /// the TUI.
    fn check_running(&self) -> Result<(), WebError> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(WebError::Backend("cancelled".into()));
        }
        Ok(())
    }

    /// Start a browser and attach to its page target.
    fn launch(&self, width: u32, auth: RenderCapability) -> Result<Session, WebError> {
        self.check_running()?;
        let deadline = Instant::now() + budget();
        // The profile holds the session cookie for as long as the browser
        // runs, so it is created 0700 with a name an attacker cannot
        // predict or pre-create — never a fixed path plus create_dir_all.
        let profile = tempfile::Builder::new()
            .prefix("cosense-tui-chrome-")
            .tempdir()
            .map_err(|e| WebError::Backend(format!("no private profile directory: {e}")))?;
        let t0 = Instant::now();
        let mut child = self.spawn(profile.path(), width)?;
        *self.live_pid.lock().unwrap() = Some(child.id());
        *self.last_owned.lock().unwrap() = Some((child.id(), profile.path().to_path_buf()));
        let attach = self
            .wait_for_port(&mut child, profile.path(), deadline)
            .and_then(|port| self.page_target(port, deadline))
            .and_then(|url| Cdp::connect(&url));
        self.debug(format!("launch+attach {:?}", t0.elapsed()));
        match attach {
            Ok(cdp) => Ok(Session { child, cdp, auth, profile }),
            Err(e) => {
                // Session::drop is what normally reaps these; there is no
                // Session yet, so do it by hand.
                kill(&mut child);
                *self.live_pid.lock().unwrap() = None;
                Err(e)
            }
        }
    }

    fn page_target(&self, port: u16, deadline: Instant) -> Result<String, WebError> {
        page_target_at(port, deadline, &self.stopped)
    }

    /// Opt-in diagnostics. They must NEVER reach stdout or stderr: this
    /// thread runs while the TUI owns the alternate screen, so a print
    /// would corrupt the display. `COSENSE_WEB_DEBUG` is a FILE PATH.
    fn debug(&self, line: String) {
        let Some(path) = std::env::var_os("COSENSE_WEB_DEBUG") else { return };
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[webrender] {line}");
        }
    }

    fn spawn(&self, profile: &Path, width: u32) -> Result<Child, WebError> {
        self.check_running()?;
        Command::new(&self.exe)
            .arg("--headless=new")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--window-size={width},1400"))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
                "--hide-scrollbars",
                "--mute-audio",
                "--disable-extensions",
                "--disable-background-networking",
                "--disable-sync",
            ])
            // The sandbox stays ON by default: this browser holds the user's
            // live `connect.sid` and loads remote content (ProjectCSS can
            // pull third-party resources). Containers that cannot sandbox
            // opt out explicitly; if Chrome then refuses to start, the batch
            // fails and the code block stays on screen.
            .args(if std::env::var_os("COSENSE_CHROME_NO_SANDBOX").is_some() {
                &["--no-sandbox", "about:blank"][..]
            } else {
                &["about:blank"][..]
            })
            // stdout/stderr MUST be discarded: anything Chrome prints would
            // land in the alternate screen and corrupt the TUI.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| WebError::Backend(format!("chrome did not start: {e}")))
    }

    fn drive(
        &self,
        session: &mut Session,
        reqs: &[WebRequest],
        width: u32,
        dark: bool,
        deadline: Instant,
    ) -> Result<Vec<Result<Vec<u8>, WebError>>, WebError> {
        macro_rules! phase {
            ($label:expr, $t:expr) => {
                self.debug(format!("{} {:?}", $label, $t.elapsed()));
            };
        }
        let auth = session.auth;
        let cdp = &mut session.cdp;
        let t1 = Instant::now();
        // Re-applied every batch: the pane may have been resized, and the
        // cookie is idempotent.
        cdp.call("Page.enable", serde_json::json!({}), deadline)?;
        cdp.call(
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({
                "width": width, "height": 1400,
                "deviceScaleFactor": 1, "mobile": false,
            }),
            deadline,
        )?;
        cdp.call(
            "Emulation.setEmulatedMedia",
            serde_json::json!({
                "features": [{
                    "name": "prefers-color-scheme",
                    "value": if dark { "dark" } else { "light" },
                }]
            }),
            deadline,
        )
        .ok(); // cosmetic; an old Chrome without it is still usable

        // Anonymous is a real mode, not "no cookie configured": a public
        // page renders fine without one, and it is the retry when the
        // server has told us this cookie is stale.
        let cookie = match auth {
            RenderCapability::Authenticated => self.sid.as_ref(),
            RenderCapability::Anonymous => None,
        };
        if let Some(sid) = cookie {
            cdp.call("Network.enable", serde_json::json!({}), deadline)?;
            // The ONLY place the credential is handed over. Chrome stores it
            // in the throwaway profile, which is deleted with the batch.
            cdp.call(
                "Network.setCookie",
                serde_json::json!({
                    "name": "connect.sid",
                    "value": sid,
                    "domain": ".scrapbox.io",
                    "path": "/",
                    "secure": true,
                    "httpOnly": true,
                }),
                deadline,
            )
            .map_err(|_| WebError::NotAuthorized)?;
        }

        let url = reqs[0].page_url();
        cdp.call("Page.navigate", serde_json::json!({ "url": url }), deadline)?;

        phase!("setup+navigate", t1);
        let t2 = Instant::now();
        let selectors: Vec<String> = reqs.iter().map(|r| r.selector()).collect();
        let ready = self.wait_for_elements(cdp, reqs, &selectors, deadline)?;
        phase!("wait-for-draw", t2);
        let t3 = Instant::now();

        // Capturing gets its own allowance: waiting for a slow (or broken)
        // diagram must not eat the time needed to screenshot the ones that
        // DID render — that used to fail a whole page because of one bad
        // block.
        let capture_deadline = Instant::now() + Duration::from_secs(10);
        let mut out = Vec::with_capacity(reqs.len());
        for (i, req) in reqs.iter().enumerate() {
            if !ready[i] {
                // The page rendered but Cosense never drew this one — a
                // Mermaid syntax error looks exactly like this.
                out.push(Err(WebError::NotRendered));
                continue;
            }
            out.push(capture(cdp, &selectors[i], req, capture_deadline));
        }
        phase!("capture", t3);
        Ok(out)
    }

    /// Chrome writes the port it actually bound into `DevToolsActivePort`.
    fn wait_for_port(
        &self,
        child: &mut Child,
        profile: &Path,
        deadline: Instant,
    ) -> Result<u16, WebError> {
        let file = profile.join("DevToolsActivePort");
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(WebError::Backend(format!("chrome exited early ({status})")));
            }
            if let Ok(text) = std::fs::read_to_string(&file) {
                if let Some(first) = text.lines().next() {
                    if let Ok(port) = first.trim().parse::<u16>() {
                        return Ok(port);
                    }
                }
            }
            self.tick(deadline, Duration::from_millis(50))?;
        }
    }

    /// Poll the page until every target element has actually been drawn.
    /// One evaluate covers the whole batch, so N diagrams cost one page load.
    fn wait_for_elements(
        &self,
        cdp: &mut Cdp,
        reqs: &[WebRequest],
        selectors: &[String],
        deadline: Instant,
    ) -> Result<Vec<bool>, WebError> {
        let ready_child = reqs[0].kind.ready_child();
        let expr = format!(
            r#"(function(){{
                var sels = {sels};
                var body = document.querySelector('.lines') || document.querySelector('.page');
                return JSON.stringify({{
                    loaded: !!body,
                    href: location.href,
                    ready: sels.map(function(s){{
                        var e = document.querySelector(s);
                        if (!e) return false;
                        var c = e.querySelector({child});
                        if (!c) return false;
                        var r = c.getBoundingClientRect();
                        return r.width > 1 && r.height > 1;
                    }})
                }});
            }})()"#,
            sels = serde_json::to_string(selectors).unwrap(),
            child = serde_json::to_string(ready_child).unwrap(),
        );
        let mut last = vec![false; reqs.len()];
        let mut ever_loaded = false;
        let mut login_wall = false;
        // A block Cosense refuses to draw (a Mermaid syntax error) never
        // becomes ready, and waiting out the whole budget for it would make
        // every OTHER diagram on the page arrive 20s late. So the wait also
        // ends once the page has gone quiet: loaded, and nothing new drawn
        // for this long.
        let quiet = Duration::from_secs(6);
        let mut last_progress = Instant::now();
        loop {
            if let Ok(v) = eval_json(cdp, &expr, deadline) {
                ever_loaded |= v.get("loaded").and_then(|b| b.as_bool()).unwrap_or(false);
                login_wall |= v
                    .get("href")
                    .and_then(|h| h.as_str())
                    .map(|h| h.contains("/login") || h.contains("/auth"))
                    .unwrap_or(false);
                if let Some(arr) = v.get("ready").and_then(|r| r.as_array()) {
                    for (i, b) in arr.iter().enumerate() {
                        if i < last.len() && b.as_bool().unwrap_or(false) && !last[i] {
                            last[i] = true;
                            last_progress = Instant::now();
                        }
                    }
                }
                if last.iter().all(|b| *b) {
                    return Ok(last);
                }
            }
            let stalled = ever_loaded && last_progress.elapsed() > quiet;
            if stalled || Instant::now() >= deadline || self.stopped.load(Ordering::Relaxed) {
                if login_wall {
                    // Redirected away from the page: no sid, or a stale one.
                    return Err(WebError::NotAuthorized);
                }
                if !ever_loaded {
                    return Err(WebError::Timeout { seconds: budget().as_secs() });
                }
                // Some diagrams may still have made it; report per request.
                return Ok(last);
            }
            self.tick(deadline, Duration::from_millis(200))?;
        }
    }

    /// Sleep, but give up as soon as the deadline passes or the app quits.
    fn tick(&self, deadline: Instant, step: Duration) -> Result<(), WebError> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(WebError::Backend("cancelled".into()));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(WebError::Timeout { seconds: budget().as_secs() });
        }
        std::thread::sleep(step.min(deadline - now));
        Ok(())
    }
}

impl ChromeBackend {
    /// `(pid, profile dir)` of the last browser this backend started, kept
    /// after shutdown so a caller can verify it is really gone.
    pub fn last_owned(&self) -> Option<(u32, PathBuf)> {
        self.last_owned.lock().unwrap().clone()
    }
}

impl WebBackend for ChromeBackend {
    fn render_batch(
        &self,
        reqs: &[WebRequest],
        auth: RenderCapability,
    ) -> Vec<Result<Vec<u8>, WebError>> {
        if reqs.is_empty() {
            return Vec::new();
        }
        match self.run_batch(reqs, auth) {
            Ok(out) => out,
            // A whole-session failure (no port, dead socket, login wall)
            // applies to every request in the batch.
            Err(e) => reqs.iter().map(|_| Err(e.clone())).collect(),
        }
    }

    fn idle(&self) {
        // Dropping the Session kills the browser and removes its profile.
        // A viewer left open overnight holds nothing.
        *self.session.lock().unwrap() = None;
        *self.live_pid.lock().unwrap() = None;
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        // `try_lock`: a batch in flight holds the session lock, and the
        // whole point here is not to wait for it — the pid below is what
        // reaps that browser.
        if let Ok(mut held) = self.session.try_lock() {
            *held = None;
        }
        if let Some(pid) = *self.live_pid.lock().unwrap() {
            // The batch's own cleanup may already have reaped it; SIGKILL on
            // a gone pid is harmless, and this is the last chance to avoid
            // leaving a headless Chrome behind after the TUI exits.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Screenshot one element, clipped to the box Cosense actually laid out.
fn capture(
    cdp: &mut Cdp,
    selector: &str,
    req: &WebRequest,
    deadline: Instant,
) -> Result<Vec<u8>, WebError> {
    let expr = format!(
        r#"(function(){{
            var e = document.querySelector({sel});
            if (!e) return JSON.stringify(null);
            // Scroll to the very top before measuring: Cosense's navbar is
            // position:sticky, so with `captureBeyondViewport` it paints
            // wherever the viewport happens to be — right on top of the
            // diagram if we scrolled it into view. Parked at y=0 it can only
            // ever cover the page header.
            window.scrollTo(0, 0);
            var r = e.getBoundingClientRect();
            return JSON.stringify({{
                x: r.left + window.pageXOffset,
                y: r.top + window.pageYOffset,
                w: r.width, h: r.height
            }});
        }})()"#,
        sel = serde_json::to_string(selector).unwrap(),
    );
    let v = eval_json(cdp, &expr, deadline)?;
    let (x, y, w, h) = match (
        v.get("x").and_then(|n| n.as_f64()),
        v.get("y").and_then(|n| n.as_f64()),
        v.get("w").and_then(|n| n.as_f64()),
        v.get("h").and_then(|n| n.as_f64()),
    ) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 1.0 && h > 1.0 => (x, y, w, h),
        _ => return Err(WebError::NotRendered),
    };
    let res = cdp.call(
        "Page.captureScreenshot",
        serde_json::json!({
            "format": "png",
            "captureBeyondViewport": true,
            "clip": { "x": x, "y": y, "width": w, "height": h, "scale": 2 },
        }),
        deadline,
    )?;
    let b64 = res
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or(WebError::NotRendered)?;
    let png = b64_decode(b64).ok_or_else(|| WebError::Backend("bad screenshot payload".into()))?;
    debug_assert_eq!(req.kind.ready_child(), "svg");
    Ok(png)
}

/// `Runtime.evaluate` an expression that returns a JSON string, and parse it.
fn eval_json(cdp: &mut Cdp, expr: &str, deadline: Instant) -> Result<serde_json::Value, WebError> {
    let res = cdp.call(
        "Runtime.evaluate",
        serde_json::json!({ "expression": expr, "returnByValue": true }),
        deadline,
    )?;
    let s = res
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .ok_or(WebError::NotRendered)?;
    serde_json::from_str(s).map_err(|_| WebError::NotRendered)
}

/// Ask the DevTools HTTP endpoint for the page target's websocket URL.
fn page_target_at(
    port: u16,
    deadline: Instant,
    stopped: &std::sync::atomic::AtomicBool,
) -> Result<String, WebError> {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| WebError::Backend(e.to_string()))?;
    loop {
        let got = http
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .and_then(|r| r.json::<serde_json::Value>());
        if let Ok(list) = got {
            if let Some(url) = list.as_array().and_then(|a| {
                a.iter()
                    .find(|t| t.get("type").and_then(|s| s.as_str()) == Some("page"))
                    .and_then(|t| t.get("webSocketDebuggerUrl"))
                    .and_then(|s| s.as_str())
            }) {
                return Ok(url.to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err(WebError::Timeout { seconds: budget().as_secs() });
        }
        if stopped.load(Ordering::Relaxed) {
            // Without this the quit path could wait out the whole budget
            // here, and the render worker could not be joined.
            return Err(WebError::Backend("cancelled".into()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A minimal CDP client: request/response over one websocket, skipping the
/// event stream. Nothing here interprets page content — only our own
/// command results.
struct Cdp {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
}

impl Cdp {
    fn connect(url: &str) -> Result<Self, WebError> {
        let (ws, _) = tungstenite::connect(url)
            .map_err(|e| WebError::Backend(format!("devtools connect failed: {e}")))?;
        if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() {
            // Without this a wedged browser would hang the render worker
            // forever, and with it the quit path.
            tcp.set_read_timeout(Some(Duration::from_millis(500))).ok();
        }
        Ok(Self { ws })
    }

    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
    ) -> Result<serde_json::Value, WebError> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({ "id": id, "method": method, "params": params });
        self.ws
            .send(tungstenite::Message::Text(msg.to_string().into()))
            .map_err(|e| WebError::Backend(format!("devtools send failed: {e}")))?;
        loop {
            if Instant::now() >= deadline {
                return Err(WebError::Timeout { seconds: budget().as_secs() });
            }
            match self.ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    let v: serde_json::Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                        continue; // an event, or another call's answer
                    }
                    if let Some(err) = v.get("error") {
                        let m = err.get("message").and_then(|m| m.as_str()).unwrap_or("cdp error");
                        return Err(WebError::Backend(format!("{method}: {m}")));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
                }
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue // read timeout: loop back and re-check the deadline
                }
                Err(e) => return Err(WebError::Backend(format!("devtools read failed: {e}"))),
            }
        }
    }
}

fn kill(child: &mut Child) {
    child.kill().ok();
    // Reap it: an unwaited child would sit as a zombie for the life of the
    // TUI, which is exactly the orphan we promised not to leave.
    child.wait().ok();
}

/// Standard base64 (CDP screenshots). Small enough to not warrant a crate,
/// and it only ever sees our own DevTools socket.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend pointed at a path that cannot be executed. `shutdown`
    /// must make `run_batch` refuse BEFORE it ever gets as far as spawning.
    fn dead_backend() -> ChromeBackend {
        ChromeBackend {
            exe: PathBuf::from("/nonexistent/definitely-not-chrome"),
            sid: None,
            live_pid: Mutex::new(None),
            last_owned: Mutex::new(None),
            session: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn a_request() -> WebRequest {
        WebRequest {
            kind: crate::webrender::WebKind::Mermaid,
            project: "p".into(),
            title: "t".into(),
            page_id: "id".into(),
            line_id: "line".into(),
            code_hash: 1,
            dark: true,
        }
    }

    #[test]
    fn a_shut_down_backend_never_starts_another_browser() {
        let b = dead_backend();
        b.shutdown();
        // No spawn is even attempted: the failure is the cancellation, not
        // "chrome did not start".
        let out = b.render_batch(&[a_request()], RenderCapability::Anonymous);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Err(WebError::Backend(m)) => assert_eq!(m, "cancelled"),
            other => panic!("a batch after shutdown must be cancelled, got {other:?}"),
        }
        assert!(b.last_owned().is_none(), "and nothing was ever owned");
        // Repeated shutdowns are harmless.
        b.shutdown();
    }

    #[test]
    fn a_running_backend_reports_the_real_launch_failure() {
        // The contrast case: without shutdown it does try, and says so.
        let b = dead_backend();
        match &b.render_batch(&[a_request()], RenderCapability::Anonymous)[0] {
            Err(WebError::Backend(m)) => {
                assert!(m.contains("chrome did not start"), "got {m}");
            }
            other => panic!("expected a launch failure, got {other:?}"),
        }
    }

    #[test]
    fn base64_round_trips_a_png_signature() {
        // "iVBORw0KGgo=" is the standard PNG magic in base64.
        assert_eq!(
            b64_decode("iVBORw0KGgo=").unwrap(),
            vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
        assert_eq!(b64_decode("").unwrap(), Vec::<u8>::new());
        assert_eq!(b64_decode("QUJD").unwrap(), b"ABC".to_vec());
        assert!(b64_decode("!!!!").is_none());
    }

    #[test]
    fn an_explicit_but_missing_cosense_chrome_does_not_fall_back() {
        // Set it to a path that cannot exist; detect must report "no browser"
        // rather than quietly launching some other Chrome on the machine.
        let key = "COSENSE_CHROME";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/nonexistent/definitely-not-chrome");
        assert!(find_chrome().is_none());
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn the_render_budget_is_clamped_to_something_survivable() {
        let key = "COSENSE_WEB_TIMEOUT";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "0");
        assert_eq!(budget(), Duration::from_secs(25), "0 is nonsense; use the default");
        std::env::set_var(key, "8");
        assert_eq!(budget(), Duration::from_secs(8));
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
