use crate::*;

/// A network-free Ctx for tests that need one (mouse/session paths).
/// The client never gets used: the commit worker is not spawned in
/// tests, so jobs pile up in the App's own channel for inspection.
/// These tests assert the Japanese wording, so every test fixture
/// fixes the language for its own thread. The library's default is
/// English: an environment that says nothing gets the wording the most
/// readers can follow.
pub(crate) fn use_japanese() {
    cosense::lang::set_for_thread(cosense::lang::Lang::Ja);
}

pub(crate) fn test_ctx() -> Ctx {
    use_japanese();
    let cfg = Config {
        project: "proj".into(),
        auth: AuthStore::default(),
        api_domain: "scrapbox.io".into(),
    };
    Ctx {
        client: Client::new(cfg).unwrap(),
        picker: Picker::halfblocks(),
        fetcher: Arc::new(ImageFetcher::new(None, None).unwrap()),
        view: std::sync::RwLock::new(ViewSettings::for_tests()),
        visits_path: None,
        editability: Arc::new(std::sync::Mutex::new(HashMap::new())),
        project_settings: Arc::new(std::sync::Mutex::new(HashMap::new())),
        gyazo_teams_token: None,
        gyazo_personal_token: None,
        config: std::sync::Mutex::new(cosense::config::Config::default()),
        config_error: std::sync::Mutex::new(None),
        config_path: None,
        keyboard_enhanced: false,
        project_theme_preview: std::sync::Mutex::new(None),
        send_target: SendTarget::None,
    }
}

/// A Ctx whose every network call fails, fast and offline. The target is
/// a port on the loopback interface that nothing listens on: bind an
/// ephemeral port, drop the listener, and point the client there. The
/// connect is refused at once, and no resolver is involved.
///
/// (An earlier version used a `.invalid` host name. RFC 2606 says it
/// cannot resolve, but *how fast* that failure comes back is up to the
/// network: a slow or hijacking resolver held the lookup for tens of
/// seconds, and tests that waited on the outcome came and went.)
pub(crate) fn offline_ctx() -> Ctx {
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("an ephemeral loopback port");
    let cfg = Config {
        project: "proj".into(),
        auth: AuthStore::default(),
        api_domain: format!("127.0.0.1:{port}"),
    };
    Ctx {
        client: Client::new(cfg).unwrap(),
        ..test_ctx()
    }
}

/// A server page as `fetch_page` would return it, for feeding a
/// pending load by hand.
pub(crate) fn server_page(title: &str, texts: &[&str]) -> cosense::api::Page {
    cosense::api::Page {
        id: format!("pid-{title}"),
        persistent: true,
        title: title.to_string(),
        commit_id: String::new(),
        lines: texts
            .iter()
            .enumerate()
            .map(|(i, t)| PageLine {
                id: format!("{title}-{i}"),
                text: t.to_string(),
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
        lines_count: texts.len() as i64,
        last_accessed: None,
    }
}

/// Answer the fetch the app is waiting on, as the fetch thread would
/// (tests do not spawn it: `page_loads_on` is off), and let the app
/// install the result.
pub(crate) fn answer_pending_load(
    app: &mut App,
    ctx: &Ctx,
    result: Result<cosense::api::Page, String>,
) -> bool {
    let pending = app.pending_load.as_ref().expect("a fetch is pending");
    let gen = pending.gen;
    // テストのスレッドは立たないので、権限は ctx のキャッシュから読む(未登録は false)。
    let editable = ctx
        .editability
        .lock()
        .ok()
        .and_then(|c| c.get(&pending.project).copied())
        .unwrap_or(false);
    let result = result.map(|page| LoadedPage { page, editable });
    app.page_load_tx.send(PageLoadMsg { gen, result }).unwrap();
    drain_page_loads(app, ctx)
}

pub(crate) fn fail_pending_load(app: &mut App, ctx: &Ctx, why: &str) {
    assert!(
        !answer_pending_load(app, ctx, Err(why.to_string())),
        "a failure installs nothing"
    );
}

/// A page parked on a historical snapshot.
pub(crate) fn in_history(app: &mut App) {
    app.time = Some(TimeMachine {
        points: vec![cosense::api::SnapshotStamp {
            id: "s1".into(),
            created: 1,
        }],
        pos: 0,
        cache: HashMap::new(),
    });
}

/// A page that does not exist yet: the template Cosense returns for an
/// unwritten title (its provisional id is deliberately NOT kept).
pub(crate) fn page_uncreated(texts: &[&str]) -> App {
    let mut app = page(texts);
    app.title = texts[0].to_string();
    app.page_id = String::new();
    app.rebuild(40);
    app
}

pub(crate) fn page(texts: &[&str]) -> App {
    use_japanese();
    let mut app = App::new("proj".into());
    app.title = "t".into();
    // A page that EXISTS: an empty page_id means "not created yet",
    // which holds every commit back until the create lands.
    app.page_id = "pid".into();
    app.editable = true;
    // Typing saves at once here; the debounce has tests of its own.
    app.live_debounce = Duration::ZERO;
    app.live_max_wait = Duration::ZERO;
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
    let r = render_lines_with(
        &owned,
        None,
        &cosense::theme::Palette::for_light(false),
        &LinkTruth::default(),
    );
    app.blocks = r.blocks;
    app.srcs = r.srcs;
    app.hits = r.hits;
    app
}

/// A 4×4 PNG — the smallest thing `image` will decode for us.
pub(crate) fn tiny_png() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

/// A page with two Mermaid blocks and one ordinary code block.
pub(crate) fn mermaid_page() -> App {
    let mut app = page(&[
        "t",             // 0
        "code:mmd",      // 1
        " flowchart LR", // 2
        "   A-->B",      // 3  <- preview hangs here
        "code:js",       // 4
        " let a = 1",    // 5
        "code:two.mmd",  // 6
        " pie",          // 7  <- and here
    ]);
    app.page_id = "PAGE".into();
    app
}

/// The page's rendered text rows, for "the source is still on screen".
pub(crate) fn text_rows(app: &App) -> Vec<String> {
    app.rows
        .iter()
        .filter_map(|r| match r {
            Row::Line { line, .. } => Some(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

/// A related section for tests: two 1-hop pages and one external.
pub(crate) fn test_related() -> Vec<RelSection> {
    vec![
        RelSection {
            heading: "Links (2)".into(),
            entries: vec![
                RelEntry {
                    item: LinkItem::Page("Alpha".into()),
                    title: "Alpha".into(),
                    desc: "first line".into(),
                    age: 0,
                    accessed: 0,
                    created: 0,
                    linked: 0,
                    unread: true,
                },
                RelEntry {
                    item: LinkItem::Page("Beta".into()),
                    title: "Beta".into(),
                    desc: String::new(),
                    age: 0,
                    accessed: 0,
                    created: 0,
                    linked: 0,
                    unread: false,
                },
            ],
        },
        RelSection {
            heading: "External links (1)".into(),
            entries: vec![RelEntry {
                item: LinkItem::ProjectPage {
                    project: "other".into(),
                    title: "Page".into(),
                },
                title: "/other/Page".into(),
                desc: String::new(),
                age: 0,
                accessed: 0,
                created: 0,
                linked: 0,
                unread: true,
            }],
        },
    ]
}

pub(crate) fn key(code: KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, KeyModifiers::NONE)
}

pub(crate) fn ctrl(ch: char) -> event::KeyEvent {
    event::KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

pub(crate) fn modified(code: KeyCode, modifiers: KeyModifiers) -> event::KeyEvent {
    event::KeyEvent::new(code, modifiers)
}

pub(crate) fn type_str(app: &mut App, ctx: &Ctx, s: &str) {
    for ch in s.chars() {
        handle_session_key(app, ctx, key(KeyCode::Char(ch)));
    }
}

/// The page's source texts, for comparing one arrangement to another.
pub(crate) fn texts(app: &App) -> Vec<&str> {
    app.lines.iter().map(|line| line.text.as_str()).collect()
}

/// The page's line ids, in order.
pub(crate) fn ids(app: &App) -> Vec<&str> {
    app.lines.iter().map(|line| line.id.as_str()).collect()
}

/// The same two readings, for a remembered set of lines.
pub(crate) fn texts_of(lines: &[PageLine]) -> Vec<&str> {
    lines.iter().map(|line| line.text.as_str()).collect()
}

pub(crate) fn ids_of(lines: &[PageLine]) -> Vec<&str> {
    lines.iter().map(|line| line.id.as_str()).collect()
}

/// Drain the (unspawned) commit queue: (label, ops) per job.
pub(crate) fn drain_jobs(app: &mut App) -> Vec<(String, Vec<EditOp>)> {
    let rx = app.commit_jobs_rx.as_ref().unwrap();
    let mut out = Vec::new();
    while let Ok(j) = rx.try_recv() {
        out.push((j.label, j.ops));
    }
    out
}

/// An outcome for a commit no outline gate is waiting on. Real ids start
/// at 1, so this can never be mistaken for a queued job.
pub(crate) const UNRELATED_JOB: CommitJobId = 0;

/// Answer the commit the outstanding outline action is waiting on.
pub(crate) fn finish_outline(app: &mut App, ctx: &Ctx) {
    let job = outline_job(app);
    handle_commit_outcome(
        app,
        ctx,
        CommitOutcome::Done {
            job,
            label: "outline".into(),
            title: String::new(),
            commit_id: String::new(),
        },
    );
}

/// The job id the outstanding outline action is waiting on.
pub(crate) fn outline_job(app: &App) -> CommitJobId {
    app.outline_pending
        .as_ref()
        .expect("an outline action is outstanding")
        .job
}

pub(crate) fn shift(code: KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, KeyModifiers::SHIFT)
}

/// A server page shaped like the poller would deliver it.
pub(crate) fn polled(texts: &[(&str, &str)]) -> PolledPage {
    polled_at(texts, 0)
}

/// A poll response, stamped with the epoch its fetch started at.
pub(crate) fn polled_at(texts: &[(&str, &str)], epoch: u64) -> PolledPage {
    PolledPage {
        epoch,
        project: "proj".into(),
        title: "t".into(),
        page: cosense::api::Page {
            id: "pid".into(),
            persistent: true,
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

/// One remote commit event, shaped like the ws thread would deliver it.
pub(crate) fn commit(
    id: &str,
    parent: &str,
    page: &str,
    user: &str,
    ops: Vec<EditOp>,
) -> RemoteCommit {
    RemoteCommit {
        commit_id: id.into(),
        parent_id: parent.into(),
        page_id: page.into(),
        user_id: user.into(),
        ops,
    }
}

/// A resync result as the ws thread would ship it, stamped with the
/// epoch the fetch started at (tests that do not care pass the current
/// one, which is what a fetch with nothing racing it would carry).
pub(crate) fn resync(page: cosense::api::Page, head: Option<&str>) -> ws::ResyncPage {
    resync_at(page, head, 0)
}

pub(crate) fn resync_at(
    page: cosense::api::Page,
    head: Option<&str>,
    epoch: u64,
) -> ws::ResyncPage {
    ws::ResyncPage {
        page,
        head: head.map(str::to_string),
        epoch,
    }
}

pub(crate) fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// A 30-line page in the same vertical geometry as `ui`: body band
/// y=1..10, unscrolled content anchor y=2, scrollbar at column 41.
pub(crate) fn page_with_screen() -> App {
    let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let mut app = page(&refs);
    app.rebuild(42);
    app.text_rect = Rect::new(1, 2, 40, 8);
    app.bar_rect = Rect::new(41, 1, 1, 10);
    app.view_h = 10;
    app
}
