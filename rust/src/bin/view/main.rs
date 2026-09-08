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
//   c         comment on the cursor line / selection (again on the same
//             selection: edit it)
//   s         send the comments to the agent (herdr, or --send-cmd) and
//             clear them; l lists them (Enter jump · d delete · y copy · s send)
//   q         quit (asks once; says how many comments are unsent)
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
use cosense::capability::{self, RenderCapability, SyncState};
use cosense::comment::{format_all, Comment, Selection};
use cosense::editops::{apply_ops, diff_to_ops, invert_ops};
use cosense::highlight::Highlighter;
use cosense::image_fetch::ImageFetcher;
use cosense::outline::{
    Destination, Direction as OutlineDirection, LineRange, Plan as OutlinePlan, PlanError,
    Scope as OutlineScope,
};
use cosense::render::{
    bullet_indent_width, file_name_of_url, gyazo_permalink, is_scrapbox_file_url,
    render_lines_with, ArtifactKind, Block, CodeSpan, LinkTruth,
};
use cosense::webrender::{ArtifactCache, WebBackend, WebError, WebRequest};
use cosense::wrap::{hanging_prefix, wrap_line, wrap_line_parts};
use cosense::ws::{self, RemoteCommit, WsEvent};
use cosense::{t, ts};

use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

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

fn main() -> Result<(), Box<dyn Error>> {
    // Parse positional (project, title) + flags (--theme NAME, --light, --dark).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut theme: Option<String> = None;
    let mut force_light: Option<bool> = None;
    let mut ime_mode = cosense::ime::ImeMode::Jp; // Japanese-first default
    let mut preview = cosense::index::PreviewMode::Auto;
    let mut download_dir: Option<String> = None;
    let mut send_cmd: Option<String> = None;
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
            "--send-cmd" => send_cmd = it.next(),
            s if s.starts_with("--send-cmd=") => {
                send_cmd = Some(s["--send-cmd=".len()..].to_string())
            }
            _ => positional.push(a),
        }
    }
    // Before any message is built: the whole UI asks `lang` for its words.
    cosense::lang::set(cosense::lang::Lang::detect(lang.as_deref(), &|k| {
        std::env::var(k).ok()
    }));

    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let env_token = |name: &str| std::env::var(name).ok().filter(|s| !s.is_empty());
    let gyazo_teams_token = env_token("GYAZO_TEAMS_ACCESS_TOKEN");
    let gyazo_personal_token = env_token("GYAZO_ACCESS_TOKEN");
    // Reading is forgiving: either token can look a picture up.
    let gyazo_token = gyazo_teams_token
        .clone()
        .or_else(|| gyazo_personal_token.clone());

    // Auth: the official CLI's `cosense login` store (PAT / service
    // account), then the sid cookie fallback — see `AuthStore`.
    let auth = AuthStore::load(sid);
    let api_domain = "scrapbox.io".to_string();
    let user_cred = auth.resolve_user(&format!("https://{api_domain}"));

    // `view <project> [title]`, or `view https://scrapbox.io/<project>/<title>#<lineId>`
    // — a page URL pasted from the browser opens that project's page, with
    // the cursor on the deep-linked line. `view` alone starts at the
    // projects list: the most recently updated project of the credential's
    // is loaded underneath, and the list of all of them opens on top. With
    // no credential there is no membership to list, so the public help
    // project stands in, as it always has.
    let mut start_at_projects = false;
    let (project, title, line_id) = match positional.first().and_then(|a| parse_page_url(a)) {
        Some(target) => target,
        None => match positional.first() {
            Some(p) => (p.clone(), positional.get(1).cloned(), None),
            None => {
                let probe = Client::new(Config {
                    project: String::new(),
                    auth: auth.clone(),
                    api_domain: api_domain.clone(),
                })?;
                let latest = probe
                    .list_projects()
                    .ok()
                    .and_then(|mut ps| {
                        ps.sort_by(|a, b| b.updated.cmp(&a.updated));
                        ps.into_iter().next()
                    })
                    .map(|p| p.name);
                start_at_projects = latest.is_some();
                (latest.unwrap_or_else(|| "help-jp".into()), None, None)
            }
        },
    };
    let cfg = Config {
        project: project.clone(),
        auth,
        api_domain,
    };
    let client = Client::new(cfg)?;

    // No title on the command line = "show me the project". The most
    // recently updated page is loaded underneath (Esc lands there), and
    // the index opens on top of it: the site top.
    let start_at_index = title.is_none();
    let title = match title {
        Some(t) => t,
        None => {
            let (_, pages) = client.list_pages(1, 0, "updated")?;
            pages
                .first()
                .map(|p| p.title.clone())
                .ok_or("empty project")?
        }
    };

    let fetcher = Arc::new(ImageFetcher::new(gyazo_token, user_cred)?);
    // The viewer's own settings file (upload destinations; key bindings
    // later). A typo must not silently become the defaults, so the error
    // is kept and shown once the screen is up.
    let (config, config_error) = match cosense::config::Config::load() {
        Ok(c) => (c, None),
        Err(e) => (cosense::config::Config::default(), Some(e)),
    };

    let mut terminal = ratatui::init();
    // Wheel scrolling moves the viewport (akapen parity); failure to enable
    // mouse reporting only loses that, so it is not fatal. Bracketed paste
    // lets the composer take multi-line pastes as ONE event.
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    // Shift+Enter is the same byte as Enter to a classic terminal. Where
    // the terminal speaks the kitty keyboard protocol (kitty, WezTerm,
    // Ghostty, iTerm2, Alacritty, …), ask it to disambiguate so the
    // composer can take Shift+Enter as a line break. Elsewhere the
    // request is not sent and `^j` / Alt+Enter remain the way.
    let keyboard_enhanced = matches!(
        ratatui::crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    if keyboard_enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    // Compile the macOS IME helper in the background so the first composer
    // open never blocks on swiftc.
    cosense::ime::start_background_build();
    cosense::clipboard::start_background_build();
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
        visits_path: nav::default_visits_path(),
        editability: Arc::new(std::sync::Mutex::new(HashMap::new())),
        project_settings: Arc::new(std::sync::Mutex::new(HashMap::new())),
        gyazo_teams_token,
        gyazo_personal_token,
        config: std::sync::Mutex::new(config),
        config_error,
        config_path: cosense::config::Config::path(),
        send_target: SendTarget::detect(send_cmd, &|k| std::env::var(k).ok()),
    };
    let loaded = load_page(&ctx, &project, &title)?;

    let mut app = App::new(project.clone());
    app.light = ctx.light;
    app.visits_path = ctx.visits_path.clone();
    if let Some(e) = ctx.config_error.as_ref() {
        app.toast_err(t!(
            "設定ファイルを読めませんでした: {e}",
            "could not read the config file: {e}"
        ));
    }
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
    let web_worker = app
        .web_jobs_rx
        .take()
        .filter(|_| !renderer_off)
        .map(|jobs_rx| {
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
    // …and from here on, every page install also goes and gets its related
    // block, which is the other half of that answer (`start_related_load`).
    app.related_fetch = true;
    app.uploads_on = true;
    app.page_loads_on = true;
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
    if start_at_projects {
        // On top of that project's list, so `[` from the projects lands in
        // it and a second `[` on the page underneath.
        open_projects(&mut app, &ctx, true);
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
            None => app.toast_err(t!(
                "このページに行 {id} はありません",
                "line {id} not found on this page"
            )),
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
    if keyboard_enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        std::io::stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    if let Some(worker) = web_worker {
        let _ = app.web_job_tx.send(WebJob::Stop);
        let _ = worker.join();
    }
    drop(app.ime_guard.take());

    // Unsent comments are NOT printed here: text appearing after the
    // viewer has closed reads as spillage, not as a hand-off (akapen
    // dropped the same print for the same reason). The `q` question says
    // how many are unsent while there is still time to `s` or `y`.
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
    /// Where this viewer's own visit times persist (`nav::default_visits_path`).
    /// `None` keeps everything in memory — that is what tests run with.
    visits_path: Option<std::path::PathBuf>,
    /// Per-project edit permission. Membership changes are rare; navigation
    /// should not refetch `/users/me` + the member table on every page.
    /// Shared with the page-load thread, which warms it off the UI thread
    /// (`spawn_page_load`).
    editability: EditabilityCache,
    /// `/api/projects/<name>` per project: the site theme and the Upload
    /// tab. `None` is cached too: a PAT-only private project falls back
    /// without retrying on every page. Shared like `editability`.
    project_settings: SettingsCache,
    /// Gyazo tokens for uploads, kept APART: which Gyazo a picture lands
    /// in is decided by the token, and the permalink the page gets is
    /// built from the destination — so a Teams token must only ever serve
    /// a Teams destination and a personal one a personal destination, or
    /// the page points at a picture that is somewhere else (404).
    /// `GYAZO_TEAMS_ACCESS_TOKEN` / `GYAZO_ACCESS_TOKEN`.
    gyazo_teams_token: Option<String>,
    gyazo_personal_token: Option<String>,
    /// `~/.config/cosentty/config.toml`, or the defaults when there is
    /// none. A file that failed to parse is the defaults too, and
    /// `config_error` says so once on the status line. Behind a mutex
    /// because the settings screen (`,`) replaces it after a save while
    /// every handler holds `&Ctx`.
    config: std::sync::Mutex<cosense::config::Config>,
    config_error: Option<String>,
    /// Where the settings screen writes (`Config::path`). `None` — no
    /// `$HOME` — makes every save an error it can name; tests point it at
    /// a scratch file so a key press never touches the developer's own.
    config_path: Option<std::path::PathBuf>,
    /// Where `s` delivers the comments (`--send-cmd`, else the herdr agent
    /// of this tab when running inside herdr, else nowhere). See handoff.rs.
    send_target: SendTarget,
}

impl Ctx {
    /// A copy of the settings file as last read or saved. Small and
    /// cloned per call so no lock is held across anything slow.
    fn config(&self) -> cosense::config::Config {
        self.config.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Replace the in-memory settings (after `Config::save_project_key`).
    fn set_config(&self, next: cosense::config::Config) {
        if let Ok(mut c) = self.config.lock() {
            *c = next;
        }
    }

    /// The project's site theme. The web setting is the truth, so the API
    /// wins whenever it answered with one; `[project.<slug>].theme` in
    /// config.toml stands in when it did not (a private project without a
    /// sid). Compare `upload::Destination::resolve`, where the FILE wins:
    /// a destination is the reader's own choice, a theme is the project's.
    fn project_theme(&self, project: &str) -> Option<String> {
        self.project_theme_with(project).0
    }

    /// `project_theme`, with where the answer came from.
    fn project_theme_with(&self, project: &str) -> (Option<String>, cosense::config::Origin) {
        use cosense::config::Origin;
        if let Some(t) = self
            .project_settings(project)
            .and_then(|s| s.theme)
            .filter(|t| !t.is_empty())
        {
            return (Some(t), Origin::Api);
        }
        match self.config().project_theme(project) {
            Some(t) => (Some(t), Origin::File),
            None => (None, Origin::Default),
        }
    }

    /// The project's proper name for the header: the API's, else the
    /// file's, else the slug — which is at least always true.
    fn project_display(&self, project: &str) -> String {
        self.project_display_with(project).0
    }

    /// `project_display`, with where the answer came from.
    fn project_display_with(&self, project: &str) -> (String, cosense::config::Origin) {
        use cosense::config::Origin;
        if let Some(s) = self.project_settings(project) {
            if !s.display_name.trim().is_empty() {
                return (s.display_name, Origin::Api);
            }
        }
        match self.config().project_display_name(project) {
            Some(n) => (n, Origin::File),
            None => (project.to_string(), Origin::Default),
        }
    }

    fn project_settings(&self, project: &str) -> Option<cosense::api::ProjectSettings> {
        project_settings_cached(&self.client, &self.project_settings, project)
    }

    fn can_edit_in(&self, project: &str) -> bool {
        editability_cached(&self.client, &self.editability, project)
    }
}

type SettingsCache = Arc<std::sync::Mutex<HashMap<String, Option<cosense::api::ProjectSettings>>>>;
type EditabilityCache = Arc<std::sync::Mutex<HashMap<String, bool>>>;

/// The project's settings, asked of the server once per project. Free
/// functions rather than `Ctx` methods so the page-load thread can warm
/// the same caches with its own `Client` clone: the first visit to a
/// project then costs the UI thread nothing either.
fn project_settings_cached(
    client: &Client,
    cache: &SettingsCache,
    project: &str,
) -> Option<cosense::api::ProjectSettings> {
    if let Ok(cache) = cache.lock() {
        if let Some(settings) = cache.get(project) {
            return settings.clone();
        }
    }
    let settings = client.get_project_settings(project).ok();
    if let Ok(mut cache) = cache.lock() {
        cache.insert(project.to_string(), settings.clone());
    }
    settings
}

fn editability_cached(client: &Client, cache: &EditabilityCache, project: &str) -> bool {
    if let Ok(cache) = cache.lock() {
        if let Some(&editable) = cache.get(project) {
            return editable;
        }
    }
    let probed: Result<bool, Box<dyn Error>> = match client.credential_for(project) {
        None => Ok(false),
        // A project-scoped service account exists specifically to act in
        // that project; unlike a user it has no `/users/me` membership.
        Some(cosense::api::Credential::ServiceAccount(_)) => Ok(true),
        Some(_) => client.get_me().and_then(|me| {
            client
                .list_members_in(project)
                .map(|members| members.iter().any(|m| m.id == me))
        }),
    };
    let Ok(editable) = probed else { return false };
    if let Ok(mut cache) = cache.lock() {
        cache.insert(project.to_string(), editable);
    }
    editable
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
    /// Drop the screen and redraw whole: recovery from a garbled terminal.
    Repaint,
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    ctx: &Ctx,
) -> Result<(), Box<dyn Error>> {
    // 文字を打ち込める場所に入る/出るでハードウェアカーソルの形を
    // 切り替える: 点滅する縦棒が見えている=入力中、という一次サイン。
    // 応えない端末のために ui 側が同じセルを REVERSED でも塗る
    // (ソフト描画キャレット)。EDIT セッションだけでなく、コメント入力欄と
    // 一覧の絞り込み行も同じ扱い——どれも日本語を打つ場所で、どれも
    // ハードウェアカーソルを置いている(IME の変換窓がそこに付く)。
    let mut was_editing = false;
    loop {
        let editing = app.session.is_some()
            || app.composing.is_some()
            || app.index.as_ref().is_some_and(|ix| ix.filter_editing);
        if editing != was_editing {
            let _ = execute!(
                std::io::stdout(),
                if editing {
                    SetCursorStyle::BlinkingBar
                } else {
                    SetCursorStyle::DefaultUserShape
                }
            );
            was_editing = editing;
        }
        // Keep command mode in ASCII (retried while the IME helper builds).
        if !app.ime_ready && app.composing.is_none() && app.session.is_none() {
            app.ime_ready = app.session_ime.force_ascii();
        }
        // A page the reader asked for came back (or did not): install it,
        // or say why not, while the page they were on stayed usable.
        drain_page_loads(app, ctx);
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
        // The related-pages block came back: sections below the page, and
        // the page's own word on which of its links are live.
        app.drain_snapshots(); // header count; the next draw picks it up
        if app.drain_related() {
            rerender(app, ctx);
            app.laid_width = 0; // related rows joined the layout
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
        app.expire_note();
        app.expire_toast();
        app.drain_downloads();
        app.drain_uploads(ctx);
        terminal.draw(|f| ui(f, app, ctx))?;
        // Wait up to one tick for input (short, so arriving images refresh
        // promptly), then drain everything that queued up into ONE frame.
        // A wheel flick — or herdr, which delivers bursts at once — queues
        // dozens of mouse events; a redraw per event (images included)
        // made scrolling crawl. akapen's event_loop does the same.
        // Idle: one wake-up per 120ms is enough to slot in arriving images.
        // While a diagram renders, the shimmer wants smoother frames — but
        // only then, so an idle viewer still costs ~8 wake-ups a second.
        // The same goes for a picture on its way: its `[URL]` row pulses.
        // A toast fades at both ends, so it wants the smoother rate too.
        let tick = if app.web_shimmer.is_empty() && app.pending.is_empty() && app.toast.is_none() {
            120
        } else {
            60
        };
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
                        Action::Repaint => {
                            terminal.clear()?;
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
        eprintln!(
            "warning: {} edit(s) may not have reached the server",
            app.inflight
        );
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
        app.laid_width = 0; // the bar grows with the text
        return;
    }
    if settings_paste(app, &clean) {
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
        app.toast(t!(
            "貼り付けは編集中に — e / i / o で入ってから",
            "paste while editing — enter with e / i / o first"
        ));
    }
}

mod images;
#[cfg(test)]
mod tests;
use images::*;
mod links;
mod mmd_text;
use links::*;
mod upload;
use upload::*;
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
mod settings;
use settings::*;
mod ui;
use ui::*;
mod app;
use app::*;
mod toast;
use toast::*;
mod handoff;
use handoff::*;
