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
            hl: Highlighter::new(None, false),
            palette: cosense::theme::Palette::for_light(false),
            picker: Picker::halfblocks(),
            fetcher: Arc::new(ImageFetcher::new(None, None).unwrap()),
            light: false,
            terminal_bg: (24, 24, 24),
            ime_mode: cosense::ime::ImeMode::Off,
            preview: cosense::index::PreviewMode::Auto,
            download_dir: std::env::temp_dir(),
            editability: std::sync::Mutex::new(HashMap::new()),
            project_settings: std::sync::Mutex::new(HashMap::new()),
            gyazo_teams_token: None,
            gyazo_personal_token: None,
            config: cosense::config::Config::default(),
            config_error: None,
            send_target: SendTarget::None,
        }
    }

    /// A Ctx whose every network call fails, fast and offline: `.invalid`
    /// is reserved by RFC 2606 and cannot resolve. Used to exercise the
    /// "the fetch did not work" branches without touching the network.
    pub(crate) fn offline_ctx() -> Ctx {
        let cfg = Config {
            project: "proj".into(),
            auth: AuthStore::default(),
            api_domain: "cosense-tui-test.invalid".into(),
        };
        Ctx { client: Client::new(cfg).unwrap(), ..test_ctx() }
    }

    /// A page parked on a historical snapshot.
    pub(crate) fn in_history(app: &mut App) {
        app.time = Some(TimeMachine {
            points: vec![cosense::api::SnapshotStamp { id: "s1".into(), created: 1 }],
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

    /// `mermaid_page` with the first block's picture installed, so that
    /// block collapses to a single image row.
    pub(crate) fn drawn_mermaid_page() -> App {
        let mut app = mermaid_page();
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key, info);
        app
    }

    /// A 4×4 PNG — the smallest thing `image` will decode for us.
    pub(crate) fn tiny_png() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    pub(crate) fn scratch_cache() -> cosense::webrender::ArtifactCache {
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
    pub(crate) fn render_job(job: WebJob) -> (u64, Vec<WebRequest>) {
        match job {
            WebJob::Render { gen, reqs, .. } => (gen, reqs),
            WebJob::Rescale { key, .. } => panic!("expected a render job, got a rescale of {key}"),
            WebJob::Stop => panic!("expected a render job, got Stop"),
        }
    }

    /// A page with two Mermaid blocks and one ordinary code block.
    pub(crate) fn mermaid_page() -> App {
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

    /// The same page, seen by a reader whose text tier cannot draw it — an
    /// unknown diagram type, a pane too narrow, `COSENSE_MERMAID=off`. This
    /// is the fixture for the BROWSER tier: since the text tier became the
    /// mainline, nothing automatic asks for a picture of a block the
    /// terminal already drew, so a test about pictures has to be about the
    /// blocks that need one.
    pub(crate) fn web_tier_page() -> App {
        let mut app = mermaid_page();
        app.mermaid_text = false;
        app.math_text = false;
        app
    }

    /// Wire a page to a worker and a fake backend, and run one pass.
    /// Returns the backend so the test can count what it was asked to do.
    pub(crate) fn run_pass(
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
            Arc::clone(&app.src_epoch),
        );
        app.start_web_renders(trigger);
        backend
    }

    /// The page's rendered text rows, for "the source is still on screen".
    pub(crate) fn text_rows(app: &App) -> Vec<String> {
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
    pub(crate) fn diagram_keys(app: &App) -> Vec<String> {
        let mut out = Vec::new();
        for b in &app.blocks {
            if let Block::Artifact { kind, code, last_src, .. } = b {
                if let Some(r) = kind.web().and_then(|k| app.web_request(k, code, *last_src)) {
                    out.push(r.cache_key());
                }
            }
        }
        out
    }

    /// Wait for one reply per key, then apply them all. Collected first
    /// because `drain_web_renders` empties the whole channel.
    pub(crate) fn settle(app: &mut App, n: usize) {
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

    /// A wide PNG, like a real Mermaid capture (they come back ~1500px).
    pub(crate) fn wide_png() -> Vec<u8> {
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
    pub(crate) fn worker_replies(
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
            Arc::clone(&app.src_epoch),
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
                    item: LinkItem::ProjectPage { project: "other".into(), title: "Page".into() },
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
        app.outline_pending.as_ref().expect("an outline action is outstanding").job
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
    pub(crate) fn commit(id: &str, parent: &str, page: &str, user: &str, ops: Vec<EditOp>) -> RemoteCommit {
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

    pub(crate) fn resync_at(page: cosense::api::Page, head: Option<&str>, epoch: u64) -> ws::ResyncPage {
        ws::ResyncPage { page, head: head.map(str::to_string), epoch }
    }

    pub(crate) fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
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
