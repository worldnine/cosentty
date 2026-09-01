    use super::*;

    /// A network-free Ctx for tests that need one (mouse/session paths).
    /// The client never gets used: the commit worker is not spawned in
    /// tests, so jobs pile up in the App's own channel for inspection.
    /// These tests assert the Japanese wording, so every test fixture
    /// fixes the language for its own thread. The library's default is
    /// English: an environment that says nothing gets the wording the most
    /// readers can follow.
    fn use_japanese() {
        cosense::lang::set_for_thread(cosense::lang::Lang::Ja);
    }

    fn test_ctx() -> Ctx {
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
            project_themes: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// A Ctx whose every network call fails, fast and offline: `.invalid`
    /// is reserved by RFC 2606 and cannot resolve. Used to exercise the
    /// "the fetch did not work" branches without touching the network.
    fn offline_ctx() -> Ctx {
        let cfg = Config {
            project: "proj".into(),
            auth: AuthStore::default(),
            api_domain: "cosense-tui-test.invalid".into(),
        };
        Ctx { client: Client::new(cfg).unwrap(), ..test_ctx() }
    }

    /// A page parked on a historical snapshot.
    fn in_history(app: &mut App) {
        app.time = Some(TimeMachine {
            points: vec![cosense::api::SnapshotStamp { id: "s1".into(), created: 1 }],
            pos: 0,
            cache: HashMap::new(),
        });
    }

    /// A page that does not exist yet: the template Cosense returns for an
    /// unwritten title (its provisional id is deliberately NOT kept).
    fn page_uncreated(texts: &[&str]) -> App {
        let mut app = page(texts);
        app.title = texts[0].to_string();
        app.page_id = String::new();
        app.rebuild(40);
        app
    }

    fn page(texts: &[&str]) -> App {
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

    // ---------------------------------------------------------------
    // Live-update plan: the poller's interval follows a TYPED push state.
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // A poll response is a photograph of the past.
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // A fetch that fails must not be treated as one that succeeded.
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // Leaving an edit inside a diagram block.
    // ---------------------------------------------------------------

    /// `mermaid_page` with the first block's picture installed, so that
    /// block collapses to a single image row.
    fn drawn_mermaid_page() -> App {
        let mut app = mermaid_page();
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key, info);
        app
    }

    #[test]
    fn leaving_an_edit_inside_a_diagram_lands_on_the_picture_not_before_it() {
        // The drawn block is ONE image row, owned by its last source line.
        // A cursor left on the header or an interior line owns no row, and
        // the generic clamp searches UP — landing the reader on the line
        // BEFORE the diagram, which is not where they were working.
        for (name, start) in [("header", 1usize), ("interior", 2), ("last", 3)] {
            let mut app = drawn_mermaid_page();
            app.cursor = start;
            app.rebuild(80);
            assert_eq!(
                app.cursor, 3,
                "a cursor on the {name} line belongs to the picture it is inside"
            );
            assert!(
                app.cursor_rows().is_some(),
                "and that line owns a row ({name})"
            );
        }
    }

    #[test]
    fn a_cursor_before_a_diagram_is_left_where_it_is() {
        // The remap must not swallow lines that merely sit near a block.
        let mut app = drawn_mermaid_page();
        app.cursor = 0;
        app.rebuild(80);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn an_undrawn_diagram_block_keeps_its_source_lines_addressable() {
        // Without a picture the block is ordinary code rows; every line of
        // it owns rows and nothing should be remapped.
        let mut app = mermaid_page();
        for start in [1usize, 2, 3] {
            app.cursor = start;
            app.rebuild(80);
            assert_eq!(app.cursor, start, "source lines stay addressable");
        }
    }

    #[test]
    fn a_snapshot_never_renders_diagrams() {
        // A historical page is not what the server is showing now, so a
        // screenshot of the live page would be filed under the snapshot's
        // source hash.
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        in_history(&mut app);
        app.rebuild(80);
        assert!(!app.start_web_renders(capability::Trigger::Auto));
        assert!(app.start_web_renders(capability::Trigger::Manual) == false);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
        assert!(app.web_pending.is_empty());
    }

    #[test]
    fn a_failed_reload_keeps_the_reader_in_history() {
        let ctx = offline_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        in_history(&mut app);
        app.rebuild(80);
        // Esc asks for NOW. The fetch fails, so there is no NOW to show.
        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.time.is_some(), "the snapshot stays on screen");
        assert_ne!(app.status, "最新", "and it is not labelled as the live page");
        assert!(app.status.contains("読み直しに失敗"), "the reason is shown: {}", app.status);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(), "no diagram work");
    }

    #[test]
    fn a_failed_reload_at_the_newest_snapshot_also_stays_put() {
        let ctx = offline_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        in_history(&mut app);
        app.rebuild(80);
        // Right from the newest snapshot is the other way out of history.
        travel(&mut app, &ctx, 1);
        assert!(app.time.is_some());
        assert_ne!(app.status, "最新");
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn a_conflict_whose_reload_fails_stops_rendering_until_it_is_resolved() {
        // 409 means our ops did not land: local and server disagree. If the
        // recovery fetch then fails too, we are STILL divergent — rendering
        // would screenshot the server's text and file it under ours.
        let ctx = offline_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        app.inflight = 1;
        handle_commit_outcome(&mut app, &ctx, CommitOutcome::Conflict { job: UNRELATED_JOB });
        assert!(app.web_unsynced, "a failed recovery leaves us divergent");
        assert!(
            app.status.contains("読み直しに失敗"),
            "the real reason is not overwritten: {}",
            app.status
        );
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        assert!(!app.start_web_renders(capability::Trigger::Manual));
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
        assert!(app.web_pending.is_empty());
    }

    #[test]
    fn a_poll_that_started_before_a_websocket_commit_never_rolls_it_back() {
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        // A GET goes out now, seeing "a"/"b".
        let in_flight =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        // While it is out, a websocket commit lands and the screen moves on.
        app.lines[1].text = "b2".into();
        app.bump_server_epoch();
        // Now the old response arrives.
        apply_remote(&mut app, &ctx, in_flight);
        assert_eq!(
            app.lines[1].text, "b2",
            "a snapshot older than the applied commit must not be installed"
        );
    }

    #[test]
    fn a_poll_that_started_before_a_local_commit_never_rolls_it_back() {
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        let in_flight =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        // The reader's own edit commits while the GET is out.
        app.lines[1].text = "mine".into();
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: UNRELATED_JOB,
                label: "line 2".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        apply_remote(&mut app, &ctx, in_flight);
        assert_eq!(app.lines[1].text, "mine", "the server has our commit; the snapshot predates it");
    }

    #[test]
    fn an_undo_that_restores_agreement_lets_diagrams_render_again() {
        // A failed commit leaves local and server divergent, and diagrams
        // stop rendering so the browser cannot screenshot the server's old
        // text and file it under the local source. Undoing the edit makes
        // the two agree again — but nothing was telling the app so, and the
        // flag stayed set for the rest of the session.
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        app.lines[1].text = "typo".into();
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Failed { job: UNRELATED_JOB, label: "line 2".into(), msg: "500".into() },
        );
        assert!(app.web_unsynced, "the page is known to have drifted");

        // The reader undoes it: the screen is back to what the server has.
        app.lines[1].text = "b".into();
        // A fresh poll now agrees with the screen, line for line.
        let agreeing =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        apply_remote(&mut app, &ctx, agreeing);
        assert!(
            !app.web_unsynced,
            "an authoritative snapshot that MATCHES proves the drift is over"
        );
        assert_eq!(app.lines[1].text, "b", "and it changed nothing else");
        assert_eq!(app.cursor, 0);

        // ...so the next frame can render again.
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
    }

    #[test]
    fn a_stale_equal_poll_does_not_clear_the_drift() {
        // Agreement only counts when the snapshot is CURRENT. An old
        // response that happens to match says nothing about now.
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        app.web_unsynced = true;
        let stale = polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        app.bump_server_epoch();
        apply_remote(&mut app, &ctx, stale);
        assert!(app.web_unsynced, "a photograph of the past proves nothing");
    }

    #[test]
    fn a_fresh_poll_still_applies_web_edits() {
        // The guard must not turn into "polling stopped working".
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        let fresh =
            polled_at(&[("id0", "a"), ("id1", "web edit")], app.server_epoch_now());
        apply_remote(&mut app, &ctx, fresh);
        assert_eq!(app.lines[1].text, "web edit");
    }

    #[test]
    fn a_stale_sid_polls_fast_from_the_first_second() {
        let mut app = page(&["a"]);
        app.ws_attempted = true; // a sid exists...
        app.status = "認証: pat · 編集可 · 同期: poll · ? ヘルプ".into();
        // ...but nothing has joined a room, so the plan is still the fast one.
        assert_eq!(app.sync_state, SyncState::Polling);
        assert_eq!(app.sync_state.poll_interval(), Duration::from_secs(3));
        // A sid that never works keeps failing; each failure re-asserts fast.
        app.set_sync_state(SyncState::Reconnecting);
        assert_eq!(
            app.poll_ctrl_rx_for_test().recv().unwrap(),
            Duration::from_secs(3)
        );
        assert!(app.status.contains("同期: 再接続中"));
    }

    #[test]
    fn only_a_proven_room_relaxes_the_poll_and_a_drop_tightens_it_again() {
        let mut app = page(&["a"]);
        app.ws_attempted = true;
        app.status = "認証: sid · 編集可 · 同期: poll · ? ヘルプ".into();
        app.set_sync_state(SyncState::Live);
        assert!(app.status.contains("同期: ws"));
        app.set_sync_state(SyncState::Reconnecting);
        assert!(app.status.contains("同期: 再接続中"));
        let rx = app.poll_ctrl_rx_for_test();
        assert_eq!(rx.recv().unwrap(), Duration::from_secs(60));
        // The drop is delivered as its own message, so the sleeping poller
        // wakes on it instead of finishing a 60 s nap in silence.
        assert_eq!(rx.recv().unwrap(), Duration::from_secs(3));
    }

    #[test]
    fn navigating_away_from_a_live_room_goes_back_to_the_fast_poll() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "t".into();
        app.ws_attempted = true;
        app.status = "認証: sid · 編集可 · 同期: poll · ? ヘルプ".into();
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "t".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(app.sync_state, SyncState::Live);
        assert!(app.status.contains("同期: ws"));

        // The reader moves to another page. The old room is gone; nothing
        // has joined the new one yet, so the insurance poll must be fast.
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "other".into(),
                page_id: "P2".into(),
                header_colors: HeaderColors::fallback(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                read_at: None,
                editable: true,
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert_eq!(app.sync_state, SyncState::Polling);
        let rx = app.poll_ctrl_rx_for_test();
        let mut last = None;
        while let Ok(d) = rx.try_recv() {
            last = Some(d);
        }
        assert_eq!(last, Some(Duration::from_secs(3)), "the poller is retuned at once");
    }

    #[test]
    fn a_live_from_the_room_the_reader_left_is_ignored() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "other".into();
        app.ws_attempted = true;
        // Still in the channel from before the navigation.
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "t".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(
            app.sync_state,
            SyncState::Polling,
            "a Live for the old page must not hold the new one on the 60 s poll"
        );
        // The new room's own Live is honoured.
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "other".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(app.sync_state, SyncState::Live);
    }

    #[test]
    fn a_rejoin_that_stalls_leaves_the_reader_on_the_fast_poll() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "other".into();
        app.ws_attempted = true;
        app.status = "認証: sid · 編集可 · 同期: ws · ? ヘルプ".into();
        app.sync_state = SyncState::Live;
        // The rejoin fails, repeatedly. Every failure edge says so.
        for _ in 0..3 {
            handle_ws_event(
                &mut app,
                &ctx,
                WsEvent::State {
                    project: "proj".into(),
                    title: "other".into(),
                    state: SyncState::Reconnecting,
                },
            );
        }
        assert_eq!(app.sync_state.poll_interval(), Duration::from_secs(3));
        assert!(app.status.contains("同期: 再接続中"));
    }

    #[test]
    fn a_session_without_a_sid_is_never_called_reconnecting() {
        let mut app = page(&["a"]);
        app.ws_attempted = false;
        app.set_sync_state(SyncState::Reconnecting);
        assert_eq!(app.sync_label(), "poll");
    }

    #[test]
    fn speeding_up_the_poll_fetches_at_once_slowing_down_does_not() {
        let (tx, rx) = mpsc::channel::<Duration>();
        // Slow -> fast: the push channel just died; close the gap now.
        tx.send(Duration::from_secs(3)).unwrap();
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(60)),
            Some((Duration::from_secs(3), true))
        );
        // Fast -> slow: the room just delivered a catch-up; nothing to fetch.
        tx.send(Duration::from_secs(60)).unwrap();
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(3)),
            Some((Duration::from_secs(60), false))
        );
    }

    #[test]
    fn a_pile_of_state_changes_collapses_to_the_last_one() {
        let (tx, rx) = mpsc::channel::<Duration>();
        // Flap during one sleep: live, dropped, live again.
        for d in [60, 3, 60] {
            tx.send(Duration::from_secs(d)).unwrap();
        }
        // Only the world as it now IS matters; the interval ends slow and
        // no fetch is forced, because the channel came back up.
        assert_eq!(
            absorb_interval(&rx, Duration::from_secs(60)),
            Some((Duration::from_secs(60), false))
        );
    }

    #[test]
    fn the_poller_stops_when_the_app_is_gone() {
        let (tx, rx) = mpsc::channel::<Duration>();
        drop(tx);
        assert_eq!(absorb_interval(&rx, Duration::from_secs(60)), None);
    }

    // ---------------------------------------------------------------
    // Web renderer (Mermaid). The browser itself is never launched here:
    // `FakeBackend` stands in at the request→artifact boundary.
    // ---------------------------------------------------------------

    /// A 4×4 PNG — the smallest thing `image` will decode for us.
    fn tiny_png() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn scratch_cache() -> cosense::webrender::ArtifactCache {
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
    fn render_job(job: WebJob) -> (u64, Vec<WebRequest>) {
        match job {
            WebJob::Render { gen, reqs, .. } => (gen, reqs),
            WebJob::Rescale { key, .. } => panic!("expected a render job, got a rescale of {key}"),
            WebJob::Stop => panic!("expected a render job, got Stop"),
        }
    }

    /// A page with two Mermaid blocks and one ordinary code block.
    fn mermaid_page() -> App {
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

    #[test]
    fn each_mermaid_block_is_requested_against_its_own_cosense_line_id() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let rx = app.web_jobs_rx.take().unwrap();
        let (gen, reqs) = render_job(rx.try_recv().expect("one batch was queued"));
        assert_eq!(gen, app.gen_now());
        let ids: Vec<&str> = reqs.iter().map(|r| r.line_id.as_str()).collect();
        // The LAST content line of each block — not the `code:` header, and
        // nothing at all for the js block.
        assert_eq!(ids, vec!["id3", "id7"]);
        assert_eq!(
            reqs[0].selector(),
            "#mermaid-preview-id3",
            "the selector addresses Cosense's own preview element"
        );
        assert_ne!(reqs[0].cache_key(), reqs[1].cache_key());
        assert!(reqs.iter().all(|r| r.page_id == "PAGE"));
        // Asking again while they are in flight queues nothing new.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(rx.try_recv().is_err(), "no duplicate batch for pending keys");
    }

    #[test]
    fn only_the_diagrams_own_source_invalidates_it() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = |a: &App, code: &str| {
            a.web_request(cosense::webrender::WebKind::Mermaid, code, 3).unwrap().cache_key()
        };
        let before = key(&app, "flowchart");
        // Someone (or the reader) commits elsewhere on the page: pressing
        // Enter for a new line does exactly this. The diagram is untouched,
        // so it must NOT be re-rendered — keying on the page commit used to
        // re-render every diagram on the page for each such edit.
        app.ws_head = Some("COMMIT2".into());
        assert_eq!(before, key(&app, "flowchart"), "an unrelated commit changes nothing");
        // Editing the diagram itself does invalidate it.
        assert_ne!(before, key(&app, "flowchart LR"));
        // A pane resize does NOT. The viewer scales every image to a cell
        // width of its own, so the browser viewport only decides raster
        // quality — re-rendering (3-6s) on a window drag bought nothing and
        // queued one batch per width crossed.
        app.rebuild(140);
        assert_eq!(before, key(&app, "flowchart"));
        // With the page's diagrams already requested once, resizing queues
        // no further work at all.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok(), "the initial batch");
        for w in [60, 100, 200, 37] {
            app.rebuild(w);
            app.start_web_renders(capability::Trigger::Auto);
            assert!(
                app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
                "resizing to {w} columns must queue nothing",
            );
        }
        // …and a snapshot of an older page is never rendered from the web.
        app.time = Some(TimeMachine { points: vec![], pos: 0, cache: HashMap::new() });
        assert!(app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart", 3)
            .is_none());
    }

    #[test]
    fn an_open_session_only_holds_back_the_block_under_the_caret() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let session_on = |app: &mut App, line: usize| {
            app.session = Some(EditSession {
                line,
                input: Input::new(app.lines[line].text.clone()),
                orig: app.lines[line].text.clone(),
                want_col: None,
                sel_from: None,
            });
        };
        // The caret is inside the FIRST diagram (lines 1..=3): that one is
        // still being typed, but the second diagram has nothing to wait for.
        session_on(&mut app, 2);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) =
            render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().expect("the other block goes"));
        assert_eq!(
            reqs.iter().map(|r| r.line_id.as_str()).collect::<Vec<_>>(),
            vec!["id7"],
            "only the block the caret is NOT in",
        );

        // The caret moves off the diagram — every caret move commits the
        // dirty line first, so the block is now renderable. (Queueing a
        // batch invalidates the layout so the pulse can appear, and the
        // next request goes out after the frame that rebuilds it.)
        app.rebuild(80);
        session_on(&mut app, 5);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        assert_eq!(
            reqs.iter().map(|r| r.line_id.as_str()).collect::<Vec<_>>(),
            vec!["id3"],
            "the block the caret just left starts rendering, session still open",
        );
    }

    #[test]
    fn typing_never_launches_a_browser() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // A commit is still on its way to the server: the browser would see
        // the pre-edit page, so nothing is requested at all.
        app.inflight = 1;
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
        assert!(app.web_pending.is_empty());
        // Settled: one batch goes out, against text the server now has.
        app.inflight = 0;
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok());
    }

    #[test]
    fn a_result_for_an_older_page_generation_is_dropped() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let req = app.web_request(cosense::webrender::WebKind::Mermaid, "flowchart", 3).unwrap();
        let key = req.cache_key();
        app.web_pending.insert(key.clone());
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        // The reader navigated away and back while the browser was busy.
        let stale_gen = app.gen_now().wrapping_sub(1);
        app.web_tx.send(WebMsg { gen: stale_gen, key: key.clone(), rescale: false, attempted: None, res: WebOutcome::Drawn(info) }).unwrap();
        assert!(!app.drain_web_renders(), "a stale result changes nothing");
        assert!(!app.images.contains_key(&key), "yesterday's diagram is not installed");
        // …and it touches NO state. The same key can legitimately be
        // pending again for the current page, so clearing by key alone
        // would cancel that live request. What keeps pending from leaking
        // is `set_page` — see
        // `set_page_clears_the_state_a_dropped_job_would_have_answered`.
        assert!(app.web_pending.contains(&key));
    }

    #[test]
    fn a_renderer_failure_leaves_the_code_block_on_screen() {
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        app.web_pending.insert(key.clone());
        app.web_tx
            .send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, attempted: None, res: WebOutcome::Failed(WebError::NoBrowser.to_string()) })
            .unwrap();
        assert!(app.drain_web_renders());
        app.laid_width = 0;
        app.rebuild(80);
        let text: Vec<String> = app
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, .. } => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(text.iter().any(|t| t.contains("code:mmd")));
        assert!(text.iter().any(|t| t.contains("flowchart LR")));
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Image { .. })));
        assert!(
            app.hint_text(&[]).contains("ソースを表示します"),
            "and the reader is told: {}",
            app.hint_text(&[]),
        );
    }

    #[test]
    fn an_artifact_replaces_the_code_block_and_edit_puts_it_back() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.web_tx.send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, attempted: None, res: WebOutcome::Drawn(info) }).unwrap();
        assert!(app.drain_web_renders());
        app.laid_width = 0;
        app.rebuild(80);
        // The first block draws as a picture, the second is still code.
        assert!(app.rows.iter().any(|r| matches!(r, Row::Image { url, src, .. } if *url == key && *src == 3)));
        let code_rows = |app: &App| {
            app.rows
                .iter()
                .filter_map(|r| match r {
                    Row::Line { line, .. } => {
                        Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(!code_rows(&app).iter().any(|t| t.contains("flowchart LR")));

        // Opening the edit session on a line of that block hands the raw
        // source back: editing always addresses the text, never the picture.
        app.session = Some(EditSession {
            line: 2,
            input: Input::new("  flowchart LR".into()),
            orig: "  flowchart LR".into(),
            want_col: None,
            sel_from: None,
        });
        app.laid_width = 0;
        app.rebuild(80);
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Image { .. })));
        assert!(code_rows(&app).iter().any(|t| t.contains("flowchart LR")));
    }

    // ---------------------------------------------------------------
    // Capability fallback: what a session without a usable `connect.sid`
    // may still do. The REST side must be untouched by ALL of it.
    // ---------------------------------------------------------------

    /// Wire a page to a worker and a fake backend, and run one pass.
    /// Returns the backend so the test can count what it was asked to do.
    fn run_pass(
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
    fn text_rows(app: &App) -> Vec<String> {
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
    fn diagram_keys(app: &App) -> Vec<String> {
        let mut out = Vec::new();
        for b in &app.blocks {
            if let Block::WebRender { kind, code, last_src, .. } = b {
                if let Some(r) = app.web_request(*kind, code, *last_src) {
                    out.push(r.cache_key());
                }
            }
        }
        out
    }

    /// Wait for one reply per key, then apply them all. Collected first
    /// because `drain_web_renders` empties the whole channel.
    fn settle(app: &mut App, n: usize) {
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

    // ---------------------------------------------------------------
    // Render policy: when a browser is allowed to start at all.
    // ---------------------------------------------------------------

    #[test]
    fn by_default_opening_a_page_shows_cached_diagrams_and_starts_no_browser() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the default page load must never launch a browser"
        );
        // Both are misses, and misses are not failures.
        assert_eq!(app.web_missing.len(), n);
        assert!(app.web_errors.is_empty());
        assert!(app.web_pending.is_empty());
        assert!(app.web_notice.is_none(), "an empty cache is not worth a notice");
    }

    #[test]
    fn r_renders_the_artifacts_the_page_load_could_not() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let keys = diagram_keys(&app);
        settle(&mut app, keys.len());
        assert_eq!(backend.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The reader presses `R`.
        for k in &keys {
            backend.answer(k, Ok(tiny_png()));
        }
        handle_key(&mut app, &test_ctx(), key(KeyCode::Char('R')));
        settle(&mut app, keys.len());
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one batch, not one browser per diagram"
        );
        assert!(keys.iter().all(|k| app.images.contains_key(k)));
        assert!(app.web_missing.is_empty(), "they are no longer missing");
    }

    #[test]
    fn editing_a_drawn_artifact_in_manual_mode_waits_for_explicit_render() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let before = diagram_keys(&app);
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(before[0].clone(), info);
        // The initial cache-only pass has already established that the other
        // current artifacts are absent.
        app.web_missing.extend(before.iter().cloned());

        // Editing changes the artifact key. Manual mode may probe the cache,
        // but it must not launch Chrome just because the old picture existed.
        app.lines[3].text = "   A-->C".into();
        rerender(&mut app, &ctx);
        let after = diagram_keys(&app);
        assert_ne!(after[0], before[0]);
        assert!(!app.images.contains_key(&after[0]), "the changed source falls back to source");
        let job = app.web_jobs_rx.as_ref().unwrap().try_recv().expect("cache probe");
        let WebJob::Render { reqs, auth, .. } = job else { panic!("expected a render job") };
        assert!(auth.is_none(), "manual edits never start Chrome implicitly");
        assert!(reqs.iter().any(|r| r.cache_key() == after[0]));

        // A cache miss keeps the source visible until the explicit render key.
        for req in reqs {
            app.web_tx
                .send(WebMsg {
                    gen: app.gen_now(),
                    key: req.cache_key(),
                    rescale: false,
                    attempted: None,
                    res: WebOutcome::Missing,
                })
                .unwrap();
        }
        app.drain_web_renders();
        assert!(!app.images.contains_key(&after[0]));
        handle_key(&mut app, &ctx, key(KeyCode::Char('R')));
        let job = app.web_jobs_rx.as_ref().unwrap().try_recv().expect("explicit render");
        let WebJob::Render { reqs, auth, .. } = job else { panic!("expected a render job") };
        assert!(auth.is_some(), "R permits the browser in manual mode");
        assert!(reqs.iter().any(|r| r.cache_key() == after[0]));
    }

    #[test]
    fn r_pressed_while_the_cache_probe_is_out_is_served_when_it_answers() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        // The page-load probe is in flight and owns both keys.
        app.start_web_renders(capability::Trigger::Auto);
        assert_eq!(app.web_pending.len(), keys.len());
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}

        // The reader presses `R` now. It cannot queue anything yet, and it
        // must NOT report "nothing to draw".
        handle_key(&mut app, &ctx, key(KeyCode::Char('R')));
        assert!(app.web_manual_wanted);
        assert!(app.web_notice.is_none(), "a request in flight is not 'nothing to draw'");
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "no duplicate batch while the probe owns the keys"
        );

        // The probe reports misses. The remembered `R` is served, once.
        for k in &keys {
            app.web_tx
                .send(WebMsg {
                    gen: app.gen_now(),
                    key: k.clone(),
                    rescale: false,
                    attempted: None,
                    res: WebOutcome::Missing,
                })
                .unwrap();
        }
        app.drain_web_renders();
        assert!(!app.web_manual_wanted);
        let job = app
            .web_jobs_rx
            .as_ref()
            .unwrap()
            .try_recv()
            .expect("the remembered R is served without a second keypress");
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert!(auth.is_some(), "this one may reach the browser");
        assert_eq!(reqs.len(), keys.len());
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "exactly one batch, not one per key"
        );
    }

    #[test]
    fn a_cache_miss_never_blocks_the_later_r() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Manual;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        // A miss is recorded...
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                attempted: None,
                res: WebOutcome::Missing,
            })
            .unwrap();
        app.drain_web_renders();
        assert!(!app.web_errors.contains_key(&keys[0]), "a miss is not an error");
        // ...and `R` still asks for it.
        app.start_web_renders(capability::Trigger::Manual);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().recv().unwrap());
        assert!(reqs.iter().any(|r| r.cache_key() == keys[0]));
    }

    #[test]
    fn auto_draws_on_load_and_off_draws_never() {
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().recv().unwrap());
        assert_eq!(reqs.len(), 2, "auto renders both on load");

        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Off;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.start_web_renders(capability::Trigger::Manual);
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "off queues no work at all — not even a cache lookup"
        );
        assert!(app.web_pending.is_empty());
    }

    #[test]
    fn r_is_an_ordinary_character_while_editing() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        enter_session(&mut app, &ctx, 0, 1);
        handle_key(&mut app, &ctx, key(KeyCode::Char('R')));
        let s = app.session.as_ref().expect("still editing");
        assert!(s.input.buf.contains('R'), "R typed a character, not a render");
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn no_sid_on_a_public_project_renders_anonymously_and_leaves_rest_alone() {
        let mut app = mermaid_page();
        app.editable = true;
        app.caps = capability::Capabilities {
            sid: false,
            visibility: capability::Visibility::Public,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        // The browser WAS used, with no cookie.
        assert!(backend.calls.load(std::sync::atomic::Ordering::SeqCst) > 0);
        assert_eq!(
            *backend.last_auth.lock().unwrap(),
            Some(RenderCapability::Anonymous)
        );
        // ...and nothing about the missing sid touched the REST side.
        assert!(app.editable, "no sid is not a reason to stop editing");
        assert_eq!(app.sync_label(), "poll");
    }

    #[test]
    fn no_sid_on_a_private_project_serves_the_cache_and_never_launches_a_browser() {
        let mut app = mermaid_page();
        app.editable = true; // a PAT / service account is reading this page
        app.caps = capability::Capabilities {
            sid: false,
            visibility: capability::Visibility::Private,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        // One diagram is already on disk from an earlier, authenticated
        // session. Reaching this page needed REST access, so showing it is
        // not a leak — the cache itself is 0700/0600.
        let cache = scratch_cache();
        cache.put(&keys[0], &tiny_png());
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            cache,
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        );
        app.start_web_renders(capability::Trigger::Auto);
        settle(&mut app, keys.len());
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a private project with no sid must not start a browser"
        );
        assert!(app.images.contains_key(&keys[0]), "the cached diagram still shows");
        // The uncached one is a miss, NOT a failure: it stays source and
        // stays drawable.
        assert!(app.web_missing.contains(&keys[1]));
        assert!(!app.web_errors.contains_key(&keys[1]));
        assert!(app.web_pending.is_empty(), "nothing is left pulsing");
        assert!(app.hint_text(&[]).contains("connect.sid"));
        assert!(app.editable, "the renderer's limits never reach the editor");
    }

    #[test]
    fn the_private_notice_is_said_once_per_page_not_once_per_diagram() {
        let mut app = mermaid_page();
        app.caps.sid = false;
        app.caps.visibility = capability::Visibility::Private;
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_notice_shown);
        app.web_notice = None;
        // A second pass over the same page says nothing more.
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_notice.is_none());
    }

    #[test]
    fn unknown_visibility_waits_for_an_explicit_r() {
        let mut app = mermaid_page();
        app.caps.sid = false;
        app.caps.visibility = capability::Visibility::Unknown;
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let backend = run_pass(&mut app, capability::Trigger::Auto);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an automatic pass never gambles a browser on an unknown project"
        );
        // The reader asks explicitly: one anonymous attempt is allowed.
        app.start_web_renders(capability::Trigger::Manual);
        let n = diagram_keys(&app).len();
        settle(&mut app, n);
        assert!(backend.calls.load(std::sync::atomic::Ordering::SeqCst) > 0);
        assert_eq!(
            *backend.last_auth.lock().unwrap(),
            Some(RenderCapability::Anonymous)
        );
        assert!(app.caps.anonymous_spent);
    }

    #[test]
    fn a_stale_cookie_on_a_public_page_retries_without_it() {
        let mut app = mermaid_page();
        app.caps = capability::Capabilities {
            sid: true,
            visibility: capability::Visibility::Public,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                attempted: Some(RenderCapability::Authenticated),
                res: WebOutcome::Denied,
            })
            .unwrap();
        app.drain_web_renders();
        // The refusal was about the COOKIE. A second, cookie-free attempt
        // is queued, and the REST credential is not touched.
        assert!(app.caps.cookie_rejected, "the cookie is what was refused");
        assert!(!app.caps.browser_denied, "no anonymous attempt has been made yet");
        let job = app.web_jobs_rx.as_ref().unwrap().recv().unwrap();
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert!(reqs.iter().any(|r| r.cache_key() == keys[0]));
        assert_eq!(
            *auth,
            Some(RenderCapability::Anonymous),
            "retrying with the SAME rejected cookie just fails again"
        );
    }

    #[test]
    fn a_stale_job_merged_with_a_fresh_one_is_still_thrown_away() {
        // Coalescing is per BATCH, so a job queued against the old source
        // that happens to be drained alongside a job queued against the new
        // one would ride in on its freshness — and A's key would be filed
        // with a screenshot of B's text.
        let mut app = mermaid_page();
        app.rebuild(80);
        let req_a = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        let req_b = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->C", 3)
            .unwrap();
        let (key_a, key_b) = (req_a.cache_key(), req_b.cache_key());
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        // Only the CURRENT source has an answer; A must never be asked for.
        backend.answer(&key_b, Ok(tiny_png()));
        let cache = scratch_cache();

        // Both jobs are waiting before the worker starts, so they are
        // guaranteed to be drained together.
        let gen = app.gen_now();
        app.web_job_tx
            .send(WebJob::Render {
                gen,
                src_epoch: 0,
                reqs: vec![req_a],
                max_cols: 64,
                auth: Some(RenderCapability::Authenticated),
            })
            .unwrap();
        app.web_job_tx
            .send(WebJob::Render {
                gen,
                src_epoch: 1,
                reqs: vec![req_b],
                max_cols: 64,
                auth: Some(RenderCapability::Authenticated),
            })
            .unwrap();
        app.src_epoch.store(1, std::sync::atomic::Ordering::SeqCst);
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            cache.clone(),
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        );

        let mut stale_a = false;
        let mut drawn_b = false;
        for _ in 0..2 {
            let msg = app
                .web_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("both jobs are answered");
            if msg.key == key_a {
                assert!(
                    matches!(msg.res, WebOutcome::Stale),
                    "the old source's request must be abandoned, not drawn"
                );
                stale_a = true;
            } else if msg.key == key_b {
                assert!(msg.res.is_drawn());
                drawn_b = true;
            }
        }
        assert!(stale_a && drawn_b);
        assert!(
            cache.get(&key_a).is_none(),
            "nothing may be filed under the source that moved"
        );
        assert!(cache.get(&key_b).is_some(), "the current source is cached normally");
    }

    #[test]
    fn a_render_whose_source_moved_is_never_filed_under_the_old_hash() {
        // A page of diagrams takes seconds to draw. If a commit lands in
        // that window, the browser screenshots the NEW text — but the job
        // is keyed on the old hash, and the artifact cache keeps what it is
        // given for a week. The next reader of the old text would be served
        // a picture of something else.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let key_a = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        backend.answer(&key_a, Ok(tiny_png()));
        let cache = scratch_cache();
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            cache.clone(),
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        );

        // Pin the backend so the job is provably still mid-render...
        let held = backend.gate.lock().unwrap();
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_pending.contains(&key_a));
        // ...and commit B underneath it.
        app.lines[3].text = "   A-->C".into();
        rerender(&mut app, &ctx);
        drop(held);

        // Every request still gets exactly one reply, so nothing pulses on.
        let mut replies = 0;
        while replies < 2 {
            let msg = app
                .web_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("an accepted request is always answered");
            app.web_tx.send(msg).unwrap();
            replies += 1;
        }
        app.drain_web_renders();

        assert!(
            cache.get(&key_a).is_none(),
            "B's picture must never be written under A's hash"
        );
        assert!(!app.images.contains_key(&key_a));
        // Stale is not a failure and not a miss: B is simply asked for.
        assert!(app.web_errors.is_empty());
        assert!(!app.web_missing.contains(&key_a));
        assert!(!app.web_pending.contains(&key_a), "the abandoned key stopped pulsing");

        // ...and B is what gets asked for instead.
        let key_b = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->C", 3)
            .unwrap()
            .cache_key();
        app.start_web_renders(capability::Trigger::Auto);
        assert!(
            app.web_pending.contains(&key_b),
            "the new source is what gets rendered next"
        );
    }

    #[test]
    fn a_whole_refused_batch_is_retried_anonymously_in_one_go() {
        // Chrome refuses a BATCH, not a key: one navigation, one login
        // wall, N replies. Handling those one at a time made the first key
        // start the anonymous retry and the second key — seeing the
        // bookkeeping the first had just written — declare the browser
        // dead. Two diagrams is the smallest page that shows it.
        let mut app = mermaid_page();
        app.caps = capability::Capabilities {
            sid: true,
            visibility: capability::Visibility::Public,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        assert_eq!(keys.len(), 2, "this test needs two diagrams to mean anything");
        for k in &keys {
            app.web_pending.insert(k.clone());
            app.web_tx
                .send(WebMsg {
                    gen: app.gen_now(),
                    key: k.clone(),
                    rescale: false,
                    attempted: Some(RenderCapability::Authenticated),
                    res: WebOutcome::Denied,
                })
                .unwrap();
        }
        app.drain_web_renders();

        assert!(app.caps.cookie_rejected);
        assert!(
            !app.caps.browser_denied,
            "only an anonymous attempt that was itself refused may end the browser"
        );
        // ONE retry batch, cookie-free, carrying BOTH keys.
        let job = app
            .web_jobs_rx
            .as_ref()
            .unwrap()
            .try_recv()
            .expect("the refused batch is retried");
        let WebJob::Render { reqs, auth, .. } = &job else { panic!("expected a render job") };
        assert_eq!(*auth, Some(RenderCapability::Anonymous));
        let retried: Vec<String> = reqs.iter().map(|r| r.cache_key()).collect();
        for k in &keys {
            assert!(retried.contains(k), "every diagram the batch lost is retried");
        }
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "one retry batch, not one per diagram"
        );

        // Now the anonymous attempt is refused too. THAT ends it.
        for k in &keys {
            app.web_tx
                .send(WebMsg {
                    gen: app.gen_now(),
                    key: k.clone(),
                    rescale: false,
                    attempted: Some(RenderCapability::Anonymous),
                    res: WebOutcome::Denied,
                })
                .unwrap();
        }
        app.drain_web_renders();
        assert!(app.caps.browser_denied);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(), "no third attempt");
        // ...and none of it is a diagram failure.
        assert!(app.web_errors.is_empty());
    }

    #[test]
    fn a_refused_private_render_falls_back_to_source_without_disabling_edits() {
        let mut app = mermaid_page();
        app.editable = true;
        app.caps = capability::Capabilities {
            sid: true,
            visibility: capability::Visibility::Private,
            ..Default::default()
        };
        app.render_policy = capability::RenderPolicy::Auto;
        app.rebuild(80);
        let keys = diagram_keys(&app);
        app.web_pending.insert(keys[0].clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: keys[0].clone(),
                rescale: false,
                attempted: Some(RenderCapability::Authenticated),
                res: WebOutcome::Denied,
            })
            .unwrap();
        app.drain_web_renders();
        assert!(app.caps.cookie_rejected, "the cookie is what was refused");
        assert!(app.editable, "...and nothing else stops working");
        assert!(!app.web_errors.contains_key(&keys[0]));
        app.rebuild(80);
        assert!(
            text_rows(&app).iter().any(|t| t.contains("flowchart LR")),
            "the source is what the reader sees"
        );
        // No further BROWSER work is queued for this project: even an
        // explicit `R` can now only consult the disk cache.
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();
        app.start_web_renders(capability::Trigger::Manual);
        if let Ok(WebJob::Render { auth, .. }) = app.web_jobs_rx.as_ref().unwrap().try_recv() {
            assert_eq!(auth, None, "a denied project never reaches a browser again");
        }
    }

    #[test]
    fn moving_to_another_project_forgets_the_old_one_s_verdict() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.caps.visibility = capability::Visibility::Private;
        app.caps.browser_denied = true;
        app.caps.sid = true;
        app.set_page(
            Loaded {
                project: "other".into(),
                title: "t".into(),
                page_id: "P2".into(),
                header_colors: HeaderColors::fallback(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                read_at: None,
                editable: true,
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert_eq!(app.caps.visibility, capability::Visibility::Unknown);
        assert!(!app.caps.browser_denied);
        assert!(app.caps.sid, "the session's cookie did not go anywhere");
    }

    #[test]
    fn the_ui_thread_never_waits_for_the_browser() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        backend.answer(&key, Ok(tiny_png()));
        spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        );
        // Pin the backend mid-render: the worker cannot make progress while
        // this guard is held.
        let held = backend.gate.lock().unwrap();
        let t0 = std::time::Instant::now();
        app.start_web_renders(capability::Trigger::Auto);
        // …and the UI thread still lays out and would draw, immediately.
        app.laid_width = 0;
        app.rebuild(80);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "queueing a render must not block the UI (took {elapsed:?})"
        );
        assert!(app.web_pending.contains(&key));
        assert!(app.images.is_empty(), "nothing is drawn until the browser answers");
        // Let the worker through; the artifact arrives on the channel.
        drop(held);
        let msg = app
            .web_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the worker answered");
        assert_eq!(msg.gen, app.gen_now());
        assert_eq!(msg.key, key);
        assert!(msg.res.is_drawn());
    }

    #[test]
    fn a_rendering_diagram_pulses_its_code_and_stops_when_it_lands() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.rebuild(80);
        // Every row of both Mermaid blocks pulses — header and body — and
        // nothing else on the page does.
        let mut srcs: Vec<usize> = app.web_shimmer.keys().copied().collect();
        srcs.sort();
        assert_eq!(srcs, vec![1, 2, 3, 6, 7]);
        assert_eq!(app.web_shimmer[&1], (0, 3), "the code: header leads its block");
        assert_eq!(app.web_shimmer[&3], (2, 3));
        assert_eq!(app.web_shimmer[&7], (1, 2), "the second block counts from its own top");

        let dim_cells = |app: &mut App, ctx: &Ctx| {
            let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
            terminal.draw(|f| ui(f, app, ctx)).unwrap();
            terminal.draw(|f| ui(f, app, ctx)).unwrap();
            let buf = terminal.backend().buffer();
            (0..buf.area.width)
                .flat_map(|x| (0..buf.area.height).map(move |y| (x, y)))
                .filter(|(x, y)| {
                    buf.cell((*x, *y)).unwrap().modifier.contains(Modifier::DIM)
                })
                .count()
        };
        assert!(dim_cells(&mut app, &ctx) > 0, "the reader can see the renderer working");

        // The renders land: the pulse stops and the page goes back to normal.
        let keys: Vec<String> = app.web_pending.iter().cloned().collect();
        for key in keys {
            app.web_pending.remove(&key);
        }
        app.laid_width = 0;
        app.rebuild(80);
        assert!(app.web_shimmer.is_empty());
        assert_eq!(dim_cells(&mut app, &ctx), 0, "nothing pulses once nothing is pending");
    }

    /// A wide PNG, like a real Mermaid capture (they come back ~1500px).
    fn wide_png() -> Vec<u8> {
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
    fn worker_replies(
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

    #[test]
    fn a_failed_commit_stops_the_server_s_old_diagram_being_filed_under_the_new_source() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        // The reader changes the diagram A -> B locally. The key follows
        // the LOCAL text…
        app.lines[3].text = "   A-->C".into();
        rerender(&mut app, &ctx);
        app.rebuild(80);
        let key_b = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->C", 3)
            .unwrap()
            .cache_key();
        // Clear whatever the re-render already queued, and forget it was
        // ever pending: the question is what happens from here on.
        while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.web_pending.clear();
        // …but the commit is refused, so the SERVER still has A. Rendering
        // now would screenshot A and store it under B's key.
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Failed { job: UNRELATED_JOB, label: "line 4".into(), msg: "500".into() },
        );
        assert_eq!(app.inflight, 0, "the job is no longer in flight…");
        assert!(app.web_unsynced, "…but the page is known to have drifted");
        app.start_web_renders(capability::Trigger::Auto);
        assert!(
            app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
            "inflight == 0 is not enough: nothing may be rendered while desynced",
        );
        assert!(!app.images.contains_key(&key_b));
        assert!(app.web_pending.is_empty());

        // A later commit succeeding says nothing about the one that failed.
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: UNRELATED_JOB,
                label: "line 9".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        assert!(app.web_unsynced, "one success does not prove the page agrees");
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());

        // Only a whole page from the server settles it.
        let mut page = cosense::api::Page {
            id: app.page_id.clone(),
            persistent: true,
            title: app.title.clone(),
            commit_id: String::new(),
            lines: vec![],
            links: vec![],
            project_links: vec![],
            related: None,
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        page.lines = app
            .lines
            .iter()
            .map(|l| PageLine {
                id: l.id.clone(),
                text: l.text.clone(),
                user_id: String::new(),
                created: 0,
                updated: 0,
            })
            .collect();
        install_remote_lines(&mut app, &ctx, &page, "⟳ resync");
        assert!(!app.web_unsynced, "an authoritative install resolves the drift");
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok(), "rendering resumes");
    }

    #[test]
    fn an_edit_that_never_reached_the_commit_worker_also_desyncs() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // The worker is gone, so the send fails exactly as it does at exit.
        drop(app.commit_jobs_rx.take());
        queue_commit(&mut app, "line 4", vec![]);
        assert_eq!(app.inflight, 0, "nothing is in flight — it never left");
        assert!(app.web_unsynced);
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn the_worker_joins_on_stop_and_answers_every_accepted_job() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        backend.answer(&key, Ok(tiny_png()));
        let gen = app.gen_now();
        let req = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        let msgs = worker_replies(
            &mut app,
            Arc::clone(&backend),
            scratch_cache(),
            vec![
                WebJob::Render {
                    gen,
                    src_epoch: 0,
                    reqs: vec![req],
                    max_cols: 64,
                    auth: Some(RenderCapability::Authenticated),
                },
                // A rescale with nothing on disk STILL owes a reply, or the
                // viewer would wait on it forever.
                WebJob::Rescale { gen, key: "web:mermaid:absent".into(), max_cols: 20 },
            ],
            2,
        );
        assert_eq!(msgs.len(), 2, "one reply per accepted job");
        let render = msgs.iter().find(|m| m.key == key).unwrap();
        assert!(!render.rescale && render.res.is_drawn());
        let rescale = msgs.iter().find(|m| m.key == "web:mermaid:absent").unwrap();
        assert!(rescale.rescale && !rescale.res.is_drawn(), "a cache miss is answered, not dropped");
    }

    #[test]
    fn a_rescale_that_cannot_be_served_keeps_the_diagram_it_has() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key.clone(), info);
        app.web_rescaling.insert(key.clone());
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: key.clone(),
                rescale: true,
                attempted: None,
                res: WebOutcome::Failed("作り直せる図はありません".into()),
            })
            .unwrap();
        app.drain_web_renders();
        assert!(!app.web_rescaling.contains(&key), "it stops being in flight");
        assert!(app.images.contains_key(&key), "the picture on screen survives");
        assert!(app.web_errors.is_empty(), "a resize failure is not a diagram failure");
        assert!(app.web_notice.is_none(), "and the reader is not told about it");
    }

    #[test]
    fn a_stale_result_never_cancels_the_live_request_for_the_same_key() {
        // Navigate away and back: the same diagram is pending again under
        // the same key, but for a NEW generation. The old page's result
        // must not clear that.
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (old_gen, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        app.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        app.web_pending.insert(key.clone());
        app.web_rescaling.insert(key.clone());
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.web_tx
            .send(WebMsg { gen: old_gen, key: key.clone(), rescale: false, attempted: None, res: WebOutcome::Drawn(info) })
            .unwrap();
        assert!(!app.drain_web_renders(), "yesterday's answer changes nothing");
        assert!(app.web_pending.contains(&key), "the live request is still in flight");
        assert!(app.web_rescaling.contains(&key));
        assert!(!app.images.contains_key(&key), "and no stale picture is installed");
    }

    #[test]
    fn set_page_clears_the_state_a_dropped_job_would_have_answered() {
        // The worker drops stale-generation jobs without replying, which is
        // only safe because installing a page clears what those replies
        // would have cleared. This pins that invariant.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.web_rescaling.insert("web:mermaid:whatever".into());
        assert!(!app.web_pending.is_empty());
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "next".into(),
                header_colors: HeaderColors::fallback(),
                page_id: "p2".into(),
                lines: vec![],
                blocks: vec![],
                srcs: vec![],
                hits: vec![],
                read_at: None,
                editable: true,
                related: Vec::new(),
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert!(app.web_pending.is_empty(), "no request outlives the page it was for");
        assert!(app.web_rescaling.is_empty());
    }

    #[test]
    fn work_queued_for_a_page_the_reader_left_never_reaches_the_browser() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let stale_gen = app.gen_now();
        // The reader moves on before the worker gets to it.
        app.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let live_gen = app.gen_now();
        let req = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        backend.answer(&req.cache_key(), Ok(tiny_png()));
        let msgs = worker_replies(
            &mut app,
            Arc::clone(&backend),
            scratch_cache(),
            vec![
                WebJob::Render { gen: stale_gen, src_epoch: 0, reqs: vec![req.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) },
                WebJob::Rescale { gen: stale_gen, key: "web:mermaid:old".into(), max_cols: 20 },
                WebJob::Render { gen: live_gen, src_epoch: 0, reqs: vec![req.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) },
            ],
            1,
        );
        assert_eq!(msgs.len(), 1, "only the live page is answered");
        assert_eq!(msgs[0].gen, live_gen);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the abandoned page cost no browser navigation",
        );
    }

    #[test]
    fn several_passes_over_one_page_cost_a_single_browser_batch() {
        let backend = Arc::new(cosense::webrender::FakeBackend::new());
        let mut app = mermaid_page();
        app.rebuild(80);
        let gen = app.gen_now();
        let a = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap();
        let b = app.web_request(cosense::webrender::WebKind::Mermaid, "pie", 7).unwrap();
        backend.answer(&a.cache_key(), Ok(tiny_png()));
        backend.answer(&b.cache_key(), Ok(tiny_png()));

        // Both passes are queued BEFORE the worker exists. Merging happens
        // when a job arrives while others are already waiting, so the only
        // way to test it deterministically is to have them all waiting.
        // (Holding the backend gate instead pins the worker AFTER it has
        // taken the first job — it would already have formed a batch of
        // one, and the second pass would be a batch of its own. That race
        // is what made this test flaky.)
        app.web_job_tx
            .send(WebJob::Render { gen, src_epoch: 0, reqs: vec![a.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) })
            .unwrap();
        app.web_job_tx
            .send(WebJob::Render { gen, src_epoch: 0, reqs: vec![b.clone(), a.clone()], max_cols: 64, auth: Some(RenderCapability::Authenticated) })
            .unwrap();
        let handle = spawn_web_worker(
            app.web_jobs_rx.take().unwrap(),
            app.web_tx.clone(),
            Arc::clone(&backend) as Arc<dyn WebBackend>,
            Picker::halfblocks(),
            scratch_cache(),
            Arc::clone(&app.web_gen),
            Arc::clone(&app.src_epoch),
        );

        let mut keys = Vec::new();
        for _ in 0..2 {
            keys.push(
                app.web_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap()
                    .key,
            );
        }
        app.web_job_tx.send(WebJob::Stop).unwrap();
        handle.join().unwrap();
        keys.sort();
        let mut want = vec![a.cache_key(), b.cache_key()];
        want.sort();
        assert_eq!(keys, want, "each request answered exactly once, no duplicates");
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "and the whole page cost ONE browser batch, which is the point",
        );
    }

    #[test]
    fn a_send_to_a_dead_worker_stops_the_pulse_instead_of_hanging_it() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // No worker was ever spawned and the receiver is dropped: every
        // send fails, exactly as it does once the worker has stopped.
        drop(app.web_jobs_rx.take());
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_pending.is_empty(), "nothing waits on a reply that cannot come");
        app.rebuild(80);
        assert!(app.web_shimmer.is_empty(), "so nothing pulses");

        // Same for a resize.
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key.clone(), info);
        app.laid_width = 0;
        app.rebuild(30);
        app.rescale_diagrams();
        assert!(app.web_rescaling.is_empty());
    }

    #[test]
    fn a_diagram_is_never_encoded_wider_than_the_pane() {
        // The cap itself: the pane wins while it is narrower than the
        // image ceiling, and never goes to zero.
        assert_eq!(diagram_max_cols(120), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(IMAGE_MAX_COLS), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(35), 35);
        assert_eq!(diagram_max_cols(14), 14);
        assert_eq!(diagram_max_cols(0), 1);

        // …and the encoder honours it for an image far wider than any pane.
        let picker = Picker::halfblocks();
        for cap in [IMAGE_MAX_COLS, 35, 14, 5, 1] {
            let info = decode_web_png(&picker, &wide_png(), cap).unwrap();
            assert!(
                info.cells_w <= cap,
                "cap {cap} produced {} cells wide",
                info.cells_w
            );
            assert_eq!(info.built_for, cap);
            assert!(info.cells_h >= 1);
        }
    }

    #[test]
    fn narrowing_the_pane_re_encodes_the_diagram_from_its_cached_png() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        // The artifact arrives sized for the wide pane.
        let wide = decode_web_png(&Picker::halfblocks(), &wide_png(), app.web_cols).unwrap();
        assert_eq!(app.web_cols, IMAGE_MAX_COLS, "80 columns leaves room for the ceiling");
        app.web_pending.insert(key.clone());
        app.web_tx.send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: false, attempted: None, res: WebOutcome::Drawn(wide) }).unwrap();
        assert!(app.drain_web_renders());

        // Two narrow panes, well under the 64-column image ceiling.
        for (cols, want_cap) in [(40u16, 34u16), (20, 14)] {
            app.laid_width = 0;
            app.rebuild(cols);
            assert_eq!(app.web_cols, want_cap, "text width at {cols} columns");
            app.rescale_diagrams();
            // The UI thread only queued work; nothing was decoded here.
            let job = app.web_jobs_rx.as_ref().unwrap().try_recv().expect("a rescale was queued");
            let WebJob::Rescale { key: k, max_cols, gen } = job else {
                panic!("a resize must never send the browser a render job")
            };
            assert_eq!((k.as_str(), max_cols, gen), (key.as_str(), want_cap, app.gen_now()));
            // Asking again while it is in flight queues nothing.
            app.rescale_diagrams();
            assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());

            // The worker answers; the diagram now fits the pane.
            let info = decode_web_png(&Picker::halfblocks(), &wide_png(), max_cols).unwrap();
            app.web_tx
                .send(WebMsg { gen: app.gen_now(), key: key.clone(), rescale: true, attempted: None, res: WebOutcome::Drawn(info) })
                .unwrap();
            assert!(app.drain_web_renders());
            let shown_w = app.images[&key].cells_w;
            assert!(shown_w <= want_cap, "{shown_w} cells in a {cols}-column pane");

            // And the frame really does keep it inside the text column.
            app.laid_width = 0;
            let mut t = Terminal::new(TestBackend::new(cols, 16)).unwrap();
            t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
            t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
            assert!(app.text_rect.width >= shown_w, "image fits the text column");
        }
    }

    #[test]
    fn a_diagram_failure_gives_the_key_hints_back() {
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().try_recv().unwrap());
        let key = reqs[0].cache_key();
        app.web_pending.insert(key.clone());
        assert!(app.hint_text(&[]).contains("? ヘルプ"), "hints by default");
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key,
                rescale: false,
                attempted: None,
                res: WebOutcome::Failed(WebError::NoBrowser.to_string()),
            })
            .unwrap();
        assert!(app.drain_web_renders());
        // It never touches the status line — it has its own slot.
        assert!(app.status.is_empty());
        assert!(app.hint_text(&[]).contains("ソースを表示します"), "the reader is told once");
        // It holds the line for its few seconds…
        assert!(!app.expire_web_notice());
        // …and then hands the row back to the key hints.
        let (msg, _) = app.web_notice.clone().unwrap();
        app.web_notice = Some((msg, std::time::Instant::now()));
        assert!(app.expire_web_notice());
        assert!(app.web_notice.is_none());
        assert!(app.hint_text(&[]).contains("? ヘルプ"), "hints are visible again");
        assert!(!app.expire_web_notice(), "and it stays gone");
    }

    #[test]
    fn a_diagram_note_never_hides_or_erases_a_commit_or_auth_message() {
        let mut app = mermaid_page();
        // Live sync keeps the footer quiet, so this test sees only the
        // status-vs-note ordering it is about.
        app.sync_state = capability::SyncState::Live;
        // Something the reader must not miss is already on the status line.
        app.status = "コミットに失敗しました: line 4 — 500".into();
        app.note_web_failure("diagram: Chrome が見つかりません（ソースを表示します）".into());
        assert_eq!(
            app.hint_text(&[]),
            "コミットに失敗しました: line 4 — 500",
            "status outranks a diagram note",
        );
        // When the note times out it takes only itself with it.
        let (msg, _) = app.web_notice.clone().unwrap();
        app.web_notice = Some((msg, std::time::Instant::now()));
        assert!(app.expire_web_notice());
        assert_eq!(app.status, "コミットに失敗しました: line 4 — 500", "the real message survives");
        assert_eq!(app.hint_text(&[]), "コミットに失敗しました: line 4 — 500");

        // And with the status line free, the note would have shown.
        app.note_web_failure("diagram: ブラウザがタイムアウトしました（ソースを表示します）".into());
        app.status.clear();
        assert!(app.hint_text(&[]).contains("ブラウザがタイムアウト"));
        // The edit session and cursor links still outrank both.
        let links = vec![LinkItem::Page("Somewhere".into())];
        assert!(app.hint_text(&links).contains("Enter/f で開く"));
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
                Row::Line { line, src, .. } if *src == 1 => {
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
                Row::Line { line, src, .. } if *src == 1 => {
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
    fn edit_session_removes_the_ansi_colored_page_frame() {
        assert!(page_frame_visible(false));
        assert!(!page_frame_visible(true));
        let style = page_frame_style();
        assert_eq!(style.fg, Some(Color::DarkGray));
        assert_eq!(style.bg, None);
        assert!(!matches!(style.fg, Some(Color::Rgb(..))), "chrome must follow ANSI palette");
    }

    #[test]
    fn cursor_is_a_source_line_spanning_all_wrapped_rows() {
        let mut app = page(&["short", &"x".repeat(50), "tail"]);
        app.rebuild(22); // one extra chrome column leaves 16 text columns
        assert_eq!(app.cursor, 0);
        app.move_cursor(true);
        assert_eq!(app.cursor, 1);
        let (first, last) = app.cursor_rows().unwrap();
        assert_eq!((first, last), (1, 4), "all wrapped rows belong to the line");
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
    fn related_telomere_has_only_read_and_unread_states() {
        let mut visits = HashMap::new();
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 80);
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 120);
        assert!(!related_is_unread(&visits, "proj", "Page", 100));
        // No timestamp (external project link): a recorded visit is enough.
        assert!(!related_is_unread(&visits, "proj", "Page", 0));

        let (ug, us) = gutter_cell(false, None, Some(true), false);
        let (rg, rs) = gutter_cell(false, None, Some(false), false);
        assert_eq!((ug, rg), ("▏", "▏"), "read state must not encode thickness");
        assert_eq!(us.fg, Some(Color::LightBlue));
        assert_eq!(rs.fg, Some(Color::DarkGray));
    }

    /// A related section for tests: two 1-hop pages and one external.
    fn test_related() -> Vec<RelSection> {
        vec![
            RelSection {
                heading: "Links (2)".into(),
                entries: vec![
                    RelEntry {
                        item: LinkItem::Page("Alpha".into()),
                        title: "Alpha".into(),
                        desc: "first line".into(),
                        age: 0,
                        unread: true,
                    },
                    RelEntry {
                        item: LinkItem::Page("Beta".into()),
                        title: "Beta".into(),
                        desc: String::new(),
                        age: 0,
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
                    unread: true,
                }],
            },
        ]
    }

    #[test]
    fn related_rows_are_cursor_addressable_virtual_lines() {
        let mut app = page(&["title", "body"]);
        app.related = test_related();
        app.virtual_items = app
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
        app.rebuild(60);
        // body lines 0..=1, then virtual srcs 2..=4
        assert_eq!(app.src_count(), 5);
        assert!(app.src_rows(2).is_some(), "first related row renders");
        assert!(app.src_rows(4).is_some(), "external link row renders");
        // Direct navigation can still address the LAST related row and link.
        app.goto_src(99);
        assert_eq!(app.cursor, 4);
        assert_eq!(
            app.cursor_line_links(),
            vec![LinkItem::ProjectPage { project: "other".into(), title: "Page".into() }]
        );
        // j/k walk body → related → body seamlessly
        app.goto_src(1);
        app.move_cursor(true);
        assert_eq!(app.cursor, 2, "steps over the heading rule onto Alpha");
        assert_eq!(app.cursor_line_links(), vec![LinkItem::Page("Alpha".into())]);
        // The frame closes after the body, BEFORE the related section.
        assert_eq!(app.frame_end_top(), 2);
        let end = app.rows.iter().position(|r| matches!(r, Row::FrameEnd)).unwrap();
        assert!(app.rows[end + 1..].iter().any(|r| r.src() == Some(2)));
        // G is "bottom of page frame", not "last related row".
        app.view_h = 3;
        let _ = handle_key(&mut app, &test_ctx(), key(KeyCode::Char('G')));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.scroll, app.frame_end_scroll(app.view_h));
        // j then crosses the boundary into Links normally.
        app.move_cursor(true);
        assert_eq!(app.cursor, 2);
        // comments cannot anchor on virtual rows
        app.goto_src(3);
        assert!(app.make_comment("x".into()).is_none());
        // …but a selection overshooting into them clamps to the body
        app.selection = Some(Selection { anchor: 1, cursor: 3 });
        let c = app.make_comment("y".into()).unwrap();
        assert_eq!((c.start, c.end), (1, 1));
        // source mode hides related rows and clamps the cursor back
        app.selection = None;
        app.mode = Mode::Source;
        app.rebuild(60);
        assert_eq!(app.src_count(), 2);
        assert!(app.cursor < 2, "cursor clamped into the body");
    }

    #[test]
    fn edit_session_hides_the_related_section() {
        let mut app = page(&["title", "body"]);
        app.related = test_related();
        app.virtual_items = app
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
        app.rebuild(60);
        let frame = app.rows.iter().position(|r| matches!(r, Row::FrameEnd)).unwrap();
        assert!(app.rows[frame + 1..].iter().any(|r| r.src() == Some(2)));
        assert_eq!(app.src_count(), 5);

        // Entering the session hides the related section entirely: nothing
        // follows the frame, and the rows are not addressable.
        enter_session(&mut app, &test_ctx(), 1, 0);
        app.rebuild(60);
        assert_eq!(app.src_count(), 2, "related rows are unaddressable in the session");
        let end = app.rows.iter().position(|r| matches!(r, Row::FrameEnd)).unwrap();
        assert_eq!(end, app.rows.len() - 1, "nothing follows the frame while editing");
        assert!(app.cursor < 2, "cursor stays on a body line");

        // Leaving the session restores them.
        leave_session(&mut app, &test_ctx());
        app.rebuild(60);
        assert_eq!(app.src_count(), 5);
        let end = app.rows.iter().position(|r| matches!(r, Row::FrameEnd)).unwrap();
        assert!(app.rows[end + 1..].iter().any(|r| r.src() == Some(2)));
    }

    /// One reading of a list row on both screens: the link needs no mark
    /// (every row is one), so the colour says whether the page has been
    /// seen — blue while unread, plain once read, the same blue its
    /// telomere uses.
    #[test]
    fn a_list_row_is_blue_while_unread_and_plain_once_seen() {
        let mut app = page(&["t", "one"]);
        app.related = vec![RelSection {
            heading: "Links".into(),
            entries: vec![
                RelEntry {
                    item: LinkItem::Page("新しい".into()),
                    title: "新しい".into(),
                    desc: String::new(),
                    age: now_secs(),
                    unread: true,
                },
                RelEntry {
                    item: LinkItem::Page("読んだ".into()),
                    title: "読んだ".into(),
                    desc: String::new(),
                    age: now_secs(),
                    unread: false,
                },
            ],
        }];
        app.virtual_items = vec![
            LinkItem::Page("新しい".into()),
            LinkItem::Page("読んだ".into()),
        ];
        app.laid_width = 0;
        app.rebuild(60);
        let colour_of = |title: &str| -> Option<Style> {
            app.rows.iter().find_map(|r| match r {
                Row::Line { line, .. } => line
                    .spans
                    .first()
                    .filter(|sp| sp.content.trim() == title)
                    .map(|sp| sp.style),
                _ => None,
            })
        };
        let unread = colour_of("新しい").expect("the unread row is drawn");
        let read = colour_of("読んだ").expect("the read row is drawn");
        assert_eq!(unread.fg, Some(CHROME_CARET));
        assert_eq!(read.fg, None, "read rows are ordinary text");
        for st in [unread, read] {
            assert!(
                !st.add_modifier.contains(Modifier::UNDERLINED),
                "a list of links needs no underline on every row"
            );
        }
    }

    #[test]
    fn build_related_groups_2hop_under_hubs_and_dedupes() {
        use cosense::api::{Page, RelatedPage, RelatedPages};
        let rp = |title: &str, links: &[&str]| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: title.to_lowercase(),
            descriptions: vec![],
            links_lc: links.iter().map(|s| s.to_lowercase()).collect(),
            linked: 0,
            updated: 0,
        };
        let page = Page {
            id: String::new(),
            persistent: true,
            title: "me".into(),
            commit_id: String::new(),
            lines: vec![],
            links: vec!["Hub1".into(), "Hub2".into()],
            project_links: vec!["/other/Page".into(), "/bare".into()],
            related: Some(RelatedPages {
                links1hop: vec![rp("Direct", &[])],
                links2hop: vec![
                    rp("Shared12", &["hub1", "hub2"]),
                    rp("OnlyHub2", &["hub2"]),
                    rp("Direct", &["hub1"]), // already 1-hop → dropped
                    rp("Orphan", &["gone"]), // hub no longer on the page
                ],
                has_back_links_or_icons: true,
            }),
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        let secs = build_related(&page, "proj");
        let heads: Vec<&str> = secs.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(
            heads,
            vec![
                "Links (1)",
                "Hub1 (1)",
                "Hub2 (1)",
                "2 hop links (1)",
                "External links (1)"
            ]
        );
        assert_eq!(secs[1].entries[0].title, "Shared12", "first hub claims the shared page");
        assert_eq!(secs[2].entries[0].title, "OnlyHub2");
        assert_eq!(secs[3].entries[0].title, "Orphan");
        assert_eq!(secs[4].entries[0].title, "/other/Page", "bare /project is not a page");
    }

    /// Adding a tag that some OTHER page already writes makes it a shared
    /// word, and it has to stop being red at once.
    ///
    /// It did not, because "dead" had been learned on the page before and
    /// carried over: the viewer thought it already knew, so it never
    /// asked again — and the answer it was holding was the answer to a
    /// question about a different page.
    #[test]
    fn a_word_this_page_shares_is_asked_about_again_after_navigating() {
        let ctx = test_ctx();
        let mut app = page(&["A", "#タグ"]);
        let (tx, rx) = mpsc::channel();
        app.link_probe_tx = Some(tx);
        // On page A the tag was nobody else's: red, and asked about once.
        app.links.learn("タグ", false);
        app.links.learn("書かれているページ", true);
        app.link_pending.insert(cosense::render::title_lc("タグ"));

        // The reader opens another page and writes the same tag there.
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "B".into(),
                header_colors: HeaderColors::fallback(),
                page_id: "pid-B".into(),
                lines: vec![PageLine {
                    id: "b0".into(),
                    text: "B".into(),
                    user_id: String::new(),
                    created: 0,
                    updated: 0,
                }],
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                read_at: None,
                editable: true,
                related: Vec::new(),
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert_eq!(app.links.exists("タグ"), None, "a dead word does not travel");
        assert_eq!(
            app.links.exists("書かれているページ"),
            Some(true),
            "a live one does: whoever made it live is not the page asking"
        );

        app.lines.push(PageLine {
            id: "b1".into(),
            text: "#タグ".into(),
            user_id: String::new(),
            created: 0,
            updated: 0,
        });
        app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
        app.probe_unknown_links();
        let asked = rx.try_recv().expect("the tag is asked about again");
        assert_eq!(asked.title, "タグ");
        assert_eq!(asked.asked_by, "pid-B", "asked on behalf of the page that now writes it");
    }

    /// A link written during the session is the case the page response
    /// cannot answer — it was not there when the page was fetched. So the
    /// viewer asks about that one title, and only that one, and only once
    /// the caret has left the line it is being typed on.
    #[test]
    fn a_link_typed_now_is_asked_about_once_the_caret_leaves_its_line() {
        let mut app = page(&["t", "[もとからある]", "[いま打った]"]);
        let (tx, rx) = mpsc::channel();
        app.link_probe_tx = Some(tx);
        // What the page arrived knowing.
        app.links = LinkTruth::seed(["もとからある"], ["もとからある"]);

        // The caret sits on the new link: it is still being written, so
        // there is nothing to ask yet.
        let text = app.lines[2].text.clone();
        app.session = Some(EditSession {
            line: 2,
            input: Input { buf: text.clone(), cur: 0 },
            orig: text,
            want_col: None,
            sel_from: None,
        });
        app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
        app.probe_unknown_links();
        assert!(rx.try_recv().is_err(), "the line under the caret is not judged");

        // The caret moves away — now the question is worth asking, and
        // only about the title the page could not answer for.
        app.session = None;
        app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
        app.probe_unknown_links();
        let asked = rx.try_recv().unwrap();
        assert_eq!((asked.project.as_str(), asked.title.as_str()), ("proj", "いま打った"));
        assert_eq!(asked.asked_by, app.page_id, "the asking page cannot vouch for itself");
        assert!(rx.try_recv().is_err(), "a known link is not asked about");

        // Not asked twice while the answer is out.
        app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
        app.probe_unknown_links();
        assert!(rx.try_recv().is_err(), "one question per title");

        // The answer arrives and the link is marked.
        let answer = |project: &str, title: &str, from: &str| LinkProbe {
            project: project.into(),
            title: title.into(),
            asked_by: from.into(),
        };
        let me = app.page_id.clone();
        app.link_probe_res_tx.send((answer("proj", "いま打った", &me), false)).unwrap();
        assert!(app.drain_link_probes());
        assert!(app.links.missing("いま打った"));
        assert!(!app.links.missing("もとからある"));

        // An answer about a project we have left is not about this page.
        app.link_probe_res_tx
            .send((answer("elsewhere", "もとからある", &me), false))
            .unwrap();
        assert!(!app.drain_link_probes(), "another project's answer changes nothing");
        assert!(!app.links.missing("もとからある"));

        // Nor is "nobody but the asking page writes this" an answer once
        // the reader has moved to another page — that page may be the
        // second one writing it, which is a different question.
        app.link_probe_res_tx
            .send((answer("proj", "よそで聞いた", "another-page-id"), false))
            .unwrap();
        assert!(!app.drain_link_probes(), "a dead answer belongs to the page that asked");
        assert_eq!(app.links.exists("よそで聞いた"), None);
        // A LIVE answer travels: whoever made it live is not the page that
        // asked, so it is still live here.
        app.link_probe_res_tx
            .send((answer("proj", "よそで聞いた", "another-page-id"), true))
            .unwrap();
        assert!(app.drain_link_probes());
        assert_eq!(app.links.exists("よそで聞いた"), Some(true));
    }

    /// The uncreated links of a page are read out of the same response
    /// that drew it: `links` minus the 1-hop neighbours (which the API
    /// builds from EXISTING pages only). Shaped after a real reply — the
    /// project's own 「改善案」, whose only dead links were its `#issue`
    /// tags.
    #[test]
    fn a_pages_links_are_read_against_its_existing_neighbours() {
        use cosense::api::{Page, RelatedPage, RelatedPages};
        let rp = |title: &str| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: cosense::render::title_lc(title),
            descriptions: vec![],
            links_lc: vec![],
            linked: 0,
            updated: 0,
        };
        let page = Page {
            id: "P".into(),
            persistent: true,
            title: "改善案".into(),
            commit_id: String::new(),
            lines: vec![],
            links: vec![
                "テスト".into(),
                "選択した文字 URL".into(),
                "732".into(),
                "改善案".into(), // a page linking to itself
            ],
            project_links: vec![],
            related: Some(RelatedPages {
                links1hop: vec![rp("テスト"), rp("選択した文字_URL")],
                links2hop: vec![],
                has_back_links_or_icons: true,
            }),
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        let m = link_truth(&page);
        assert!(m.missing("732"), "a tag with no page behind it");
        assert!(!m.missing("テスト"), "listed as a neighbour");
        assert!(
            !m.missing("選択した文字 URL"),
            "the neighbour is spelled with `_`, the link with a space"
        );
        assert!(!m.missing("改善案"), "a page exists while you are reading it");
        assert_eq!(m.exists("何も知らないページ"), None, "no reading, no claim");

        // The rule is not "does the page exist". A title nobody wrote is
        // still live once a SECOND page uses it — Cosense's index marks
        // every link target live except where the only page writing it is
        // the one on screen. Shaped after villagepump/井戸端, where `実況`
        // is unwritten yet drawn as an ordinary link because other pages
        // link to it too.
        let shared = Page {
            links: vec!["実況".into(), "だれも書いていない".into()],
            related: Some(RelatedPages {
                links1hop: vec![RelatedPage { links_lc: vec!["実況".into()], ..rp("日記") }],
                links2hop: vec![],
                has_back_links_or_icons: true,
            }),
            ..page
        };
        let m = link_truth(&shared);
        assert!(
            !m.missing("実況"),
            "another page writes the same word: a shared word is not empty"
        );
        assert!(m.missing("だれも書いていない"), "this page is the only one saying it");
        let page = shared;

        // Opening a link to an uncreated page answers the question about
        // THAT title too — in the negative. Recording it as existing (it
        // is, after all, the page being read) would tell the page we came
        // from that its link is fine.
        let template = Page { persistent: false, ..page };
        assert_eq!(link_truth(&template).exists("改善案"), Some(false));

        // A response without relatedPages must not turn every link red.
        let bare = Page { related: None, ..template };
        assert!(!link_truth(&bare).missing("732"));
        assert!(link_truth(&bare).is_empty());
    }

    fn key(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(ch: char) -> event::KeyEvent {
        event::KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }
    fn modified(code: KeyCode, modifiers: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent::new(code, modifiers)
    }
    fn type_str(app: &mut App, ctx: &Ctx, s: &str) {
        for ch in s.chars() {
            handle_session_key(app, ctx, key(KeyCode::Char(ch)));
        }
    }
    /// The page's source texts, for comparing one arrangement to another.
    fn texts(app: &App) -> Vec<&str> {
        app.lines.iter().map(|line| line.text.as_str()).collect()
    }

    /// The page's line ids, in order.
    fn ids(app: &App) -> Vec<&str> {
        app.lines.iter().map(|line| line.id.as_str()).collect()
    }

    /// The same two readings, for a remembered set of lines.
    fn texts_of(lines: &[PageLine]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    fn ids_of(lines: &[PageLine]) -> Vec<&str> {
        lines.iter().map(|line| line.id.as_str()).collect()
    }

    /// Drain the (unspawned) commit queue: (label, ops) per job.
    fn drain_jobs(app: &mut App) -> Vec<(String, Vec<EditOp>)> {
        let rx = app.commit_jobs_rx.as_ref().unwrap();
        let mut out = Vec::new();
        while let Ok(j) = rx.try_recv() {
            out.push((j.label, j.ops));
        }
        out
    }

    /// An outcome for a commit no outline gate is waiting on. Real ids start
    /// at 1, so this can never be mistaken for a queued job.
    const UNRELATED_JOB: CommitJobId = 0;

    /// Answer the commit the outstanding outline action is waiting on.
    fn finish_outline(app: &mut App, ctx: &Ctx) {
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
    fn outline_job(app: &App) -> CommitJobId {
        app.outline_pending.as_ref().expect("an outline action is outstanding").job
    }

    #[test]
    fn read_outline_indent_replaces_only_the_selected_source_lines() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "\ttwo", "\u{3000}three", "tail"]);
        app.cursor = 3;
        app.selection = Some(Selection {
            anchor: 3,
            cursor: 1,
        });
        let ids: Vec<String> = app.lines.iter().map(|line| line.id.clone()).collect();

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Right, KeyModifiers::CONTROL),
        );

        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["title", " one", " \ttwo", " \u{3000}three", "tail"]
        );
        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(
            app.selection.map(|selection| selection.range()),
            Some((1, 3))
        );
        assert_eq!(app.cursor, 3);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "the range is one commit and undo unit");
        assert_eq!(jobs[0].1.len(), 3);
        assert!(jobs[0]
            .1
            .iter()
            .all(|op| matches!(op, EditOp::Replace { .. })));
        finish_outline(&mut app, &ctx);

        app.cursor = 0;
        app.selection = None;
        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert!(app.status.contains("タイトル行"));
        assert!(drain_jobs(&mut app).is_empty());
    }

    #[test]
    fn read_outline_range_move_recreates_only_the_target_and_rebases_through_undo_redo() {
        let ctx = test_ctx();
        let mut app = page(&["title", "a", "b", "neighbor", "after"]);
        app.lines[3].created = 30;
        app.lines[3].updated = 31;
        app.lines[4].created = 40;
        app.lines[4].updated = 41;
        app.cursor = 2;
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        });

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Down, KeyModifiers::CONTROL),
        );

        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["title", "neighbor", "a", "b", "after"]
        );
        assert_eq!(
            (
                app.lines[1].id.as_str(),
                app.lines[1].created,
                app.lines[1].updated
            ),
            ("id3", 30, 31)
        );
        assert_eq!(
            (
                app.lines[4].id.as_str(),
                app.lines[4].created,
                app.lines[4].updated
            ),
            ("id4", 40, 41)
        );
        assert_ne!(app.lines[2].id, "id1");
        assert_ne!(app.lines[3].id, "id2");
        assert_eq!(app.cursor, 3);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: 2,
                cursor: 3
            })
        );
        assert_eq!(app.undo_stack.len(), 1);

        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Delete { id: a }, EditOp::Delete { id: b }, EditOp::Insert { anchor, lines }] if a == "id1" && b == "id2" && anchor == "id4" && lines.len() == 2)
        );
        assert!(!jobs[0]
            .1
            .iter()
            .any(|op| matches!(op, EditOp::Replace { .. })));
        finish_outline(&mut app, &ctx);

        handle_key(&mut app, &ctx, key(KeyCode::Char('u')));
        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["title", "a", "b", "neighbor", "after"]
        );
        assert_eq!(
            app.lines[3].id, "id3",
            "undo does not recreate the neighbor"
        );
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: 1,
                cursor: 2
            })
        );
        assert_eq!(app.cursor, 2);
        finish_outline(&mut app, &ctx);

        handle_key(&mut app, &ctx, ctrl('r'));
        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["title", "neighbor", "a", "b", "after"]
        );
        assert_eq!(app.lines[1].id, "id3", "redo still moves the user's target");
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: 2,
                cursor: 3
            })
        );
        assert_eq!(app.cursor, 3);
        assert_eq!(drain_jobs(&mut app).len(), 2);
    }

    #[test]
    fn read_outline_block_move_obeys_sibling_and_selection_boundaries() {
        let ctx = test_ctx();
        let mut app = page(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            "    skipped-child",
            " next-parent",
        ]);
        app.cursor = 2;
        app.selection = Some(Selection::new(2));
        handle_key(&mut app, &ctx, modified(KeyCode::Down, KeyModifiers::ALT));
        assert!(app.status.contains("選択中"));
        assert!(drain_jobs(&mut app).is_empty());

        app.selection = None;
        handle_key(&mut app, &ctx, modified(KeyCode::Down, KeyModifiers::ALT));
        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "title",
                " parent",
                "  second",
                "    skipped-child",
                "  first",
                "   child",
                " next-parent"
            ]
        );
        assert_eq!(app.lines[2].id, "id4", "the sibling keeps its ID");
        assert_eq!(app.lines[3].id, "id5");
        assert_ne!(app.lines[4].id, "id2");
        assert_ne!(app.lines[5].id, "id3");
        assert_eq!(app.cursor, 4);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Delete { id: a }, EditOp::Delete { id: b }, EditOp::Insert { anchor, .. }] if a == "id2" && b == "id3" && anchor == "id6")
        );
        finish_outline(&mut app, &ctx);

        app.cursor = 2;
        handle_key(&mut app, &ctx, modified(KeyCode::Up, KeyModifiers::ALT));
        assert!(
            app.status.contains("兄弟ブロック"),
            "status: {}",
            app.status
        );
        assert!(drain_jobs(&mut app).is_empty(), "an ancestor is not moved");
    }

    // ---- the sticky move mode (NOTE-outline-editing.md §移動モード) --------

    /// `m` grabs a block, so it needs everything an outline action needs.
    /// Each refusal says which one is missing, and none of them starts the
    /// mode.
    #[test]
    fn move_mode_refuses_to_start_wherever_an_outline_action_would() {
        let ctx = test_ctx();

        let mut app = page(&["title", " one"]);
        app.cursor = 0;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("タイトル行"), "status: {}", app.status);

        // A related row below the body is not a source line.
        let mut app = page(&["title", " one"]);
        app.cursor = app.lines.len();
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("ソース行"), "status: {}", app.status);

        // A page that does not exist yet has nothing to commit against.
        let mut app = page_uncreated(&["title", " one"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("未作成"), "status: {}", app.status);

        let mut app = page(&["title", " one"]);
        app.cursor = 1;
        in_history(&mut app);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("履歴を表示中"), "status: {}", app.status);

        let mut app = page(&["title", " one"]);
        app.cursor = 1;
        app.editable = false;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("編集権限"), "status: {}", app.status);

        // One structural action at a time: the outstanding one has the only
        // snapshot to roll back to.
        let mut app = page(&["title", " one", " two"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, modified(KeyCode::Down, KeyModifiers::CONTROL));
        assert!(app.outline_pending.is_some());
        drain_jobs(&mut app);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("完了待ち"), "status: {}", app.status);
        assert!(drain_jobs(&mut app).is_empty());

        // A landed action whose ids we no longer trust: reopen the page.
        let mut app = page(&["title", " one"]);
        app.cursor = 1;
        app.outline_refresh_needed = true;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert!(app.status.contains("開き直して"), "status: {}", app.status);
        assert!(drain_jobs(&mut app).is_empty());
    }

    /// Inside the mode nothing leaves the machine: the same line values are
    /// carried around, so every id survives the whole drag, and the rest of
    /// the editor waits exactly as it does for a dirty caret line.
    /// A held block is visible, so the arrows a reader reaches for must
    /// drag it rather than silently drop it and move the cursor. `m` puts
    /// it down again, because `m` is what picked it up.
    #[test]
    fn bare_arrows_drag_the_block_and_m_puts_it_down() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));

        handle_key(&mut app, &ctx, key(KeyCode::Down));
        assert_eq!(texts(&app), vec!["title", "two", "one", "three"]);
        assert!(app.move_mode.is_some(), "the arrow drags, it does not let go");
        assert!(drain_jobs(&mut app).is_empty(), "still local");

        handle_key(&mut app, &ctx, key(KeyCode::Right));
        assert_eq!(texts(&app), vec!["title", "two", " one", "three"]);
        handle_key(&mut app, &ctx, key(KeyCode::Left));
        handle_key(&mut app, &ctx, key(KeyCode::Up));
        assert_eq!(texts(&app), vec!["title", "one", "two", "three"], "back home");
        assert!(app.move_mode.is_some());

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none(), "m lets go");
        assert!(drain_jobs(&mut app).is_empty(), "nothing changed, nothing sent");
        assert_eq!(app.cursor, 1, "the cursor stayed on the block");
    }

    #[test]
    fn move_mode_steps_are_local_and_keep_every_line_id() {
        let ctx = test_ctx();
        let mut app = page(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            " next-parent",
        ]);
        app.cursor = 2;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert_eq!(
            move_block_range(&app),
            Some((2, 3)),
            "the grabbed block is what the screen highlights"
        );
        let footer = app.hint_text(&[]);
        assert!(footer.contains("MOVE"), "footer: {footer}");
        // The footer has to fit beside the mode and sync tags, so it names
        // the four steps and the way out; the overlay carries the rest.
        assert!(footer.contains("j/k") && footer.contains("J/K"), "footer: {footer}");
        assert!(footer.contains("h/l") && footer.contains("Esc"), "footer: {footer}");
        assert!(
            !remote_gate_clear(&app),
            "remote edits wait, exactly as they do for a dirty line"
        );

        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                "  second",
                "   first",
                "    child",
                " next-parent"
            ]
        );
        assert_eq!(
            ids(&app),
            vec!["id0", "id1", "id4", "id2", "id3", "id5"],
            "reordering carries the very same lines"
        );
        assert_eq!(move_block_range(&app), Some((3, 4)));
        assert!(app.undo_stack.is_empty(), "a step is not a commit");
        assert!(app.outline_pending.is_none());
        assert_eq!(app.inflight, 0);
        assert!(drain_jobs(&mut app).is_empty(), "and nothing left the machine");

        // While the block is in hand, other mutations wait their turn.
        let id = app.lines[1].id.clone();
        do_edit(
            &mut app,
            &ctx,
            "rapid edit",
            vec![EditOp::Replace { id, text: "changed".into() }],
        );
        assert_eq!(app.lines[1].text, " parent");
        assert!(app.status.contains("移動モード中"), "status: {}", app.status);
        assert!(drain_jobs(&mut app).is_empty());
    }

    /// The block being carried is marked on screen the way a selection is:
    /// the same highlight, on every line of it. (The mode's name and its
    /// keys are in the footer, so this colour never carries the news
    /// alone.)
    #[test]
    fn the_grabbed_block_is_marked_like_a_selection() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", " parent", "  first", "   child", "  second"]);
        app.cursor = 2;
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let highlighted = |app: &mut App, term: &mut Terminal<TestBackend>| -> usize {
            term.draw(|f| ui(f, app, &ctx)).unwrap();
            let x = app.text_rect.x;
            let buf = term.backend().buffer().clone();
            (0..buf.area.height)
                .filter(|&y| buf.cell((x, y)).map(|cell| cell.bg == SEL_BG).unwrap_or(false))
                .count()
        };

        let cursor_band = highlighted(&mut app, &mut term);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert_eq!(
            highlighted(&mut app, &mut term),
            cursor_band + 1,
            "the grabbed child line is marked next to the cursor line"
        );

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert_eq!(
            highlighted(&mut app, &mut term),
            cursor_band,
            "and the mark goes when the block is let go"
        );
    }

    /// Depth-only drags keep the lines they are about: `Replace` per line,
    /// same ids, so permalinks and telomeres survive.
    #[test]
    fn an_indent_only_exit_replaces_the_block_and_keeps_its_ids() {
        let ctx = test_ctx();
        let mut app = page(&["title", " root", "  child", " peer"]);
        app.cursor = 1;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
        handle_key(&mut app, &ctx, key(KeyCode::Esc));

        assert!(app.move_mode.is_none());
        assert_eq!(texts(&app), vec!["title", "  root", "   child", " peer"]);
        assert_eq!(ids(&app), vec!["id0", "id1", "id2", "id3"]);
        assert_eq!(app.cursor, 1);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "one drag, one commit");
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Replace { id: a, text: at }, EditOp::Replace { id: b, text: bt }]
                if a == "id1" && at == "  root" && b == "id2" && bt == "   child"),
            "ops: {:?}",
            jobs[0].1
        );
        assert_eq!(app.undo_stack.len(), 1);
        assert!(app.outline_pending.is_some(), "and it uses the structural gate");
        finish_outline(&mut app, &ctx);
    }

    /// A drag that changed the block's POSITION is saved the way cosense
    /// itself saves one: the block's own lines are deleted and re-inserted
    /// with fresh ids at the anchor it ended up in front of. Everything it
    /// passed keeps its id and its timestamps.
    #[test]
    fn a_positional_exit_recreates_only_the_block() {
        let ctx = test_ctx();
        let mut app = page(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            " next-parent",
        ]);
        app.lines[4].created = 40;
        app.lines[4].updated = 41;
        app.cursor = 2;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Enter));

        assert!(app.move_mode.is_none(), "Enter commits the drag too");
        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                "  second",
                "  first",
                "   child",
                " next-parent"
            ]
        );
        assert_eq!(
            (app.lines[2].id.as_str(), app.lines[2].created, app.lines[2].updated),
            ("id4", 40, 41),
            "the sibling it passed is not rewritten"
        );
        assert_eq!(app.lines[5].id, "id5");
        assert_ne!(app.lines[3].id, "id2");
        assert_ne!(app.lines[4].id, "id3");
        assert_eq!(app.cursor, 3, "the cursor rides the block it moved");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Delete { id: a }, EditOp::Delete { id: b }, EditOp::Insert { anchor, lines }]
                if a == "id2"
                    && b == "id3"
                    && anchor == "id5"
                    && lines.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>()
                        == vec!["  first", "   child"]),
            "ops: {:?}",
            jobs[0].1
        );
        assert_eq!(app.undo_stack.len(), 1);
        finish_outline(&mut app, &ctx);
    }

    /// A block put back where it started is not an edit. Nothing is sent,
    /// no id is burned, and there is nothing to undo.
    #[test]
    fn a_block_brought_home_commits_nothing() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;
        let before = app.lines.clone();

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        for step in ['j', 'k', 'l', 'h'] {
            handle_key(&mut app, &ctx, key(KeyCode::Char(step)));
        }
        handle_key(&mut app, &ctx, key(KeyCode::Esc));

        assert!(app.move_mode.is_none());
        assert_eq!(texts(&app), texts_of(&before));
        assert_eq!(ids(&app), ids_of(&before));
        assert!(app.undo_stack.is_empty());
        assert!(app.outline_pending.is_none());
        assert_eq!(app.inflight, 0);
        assert!(drain_jobs(&mut app).is_empty());
        assert!(app.status.contains("変更なし"), "status: {}", app.status);
    }

    /// The point of the mode: however many presses it took, `u` is one
    /// press back.
    #[test]
    fn one_undo_reverses_a_whole_drag() {
        let ctx = test_ctx();
        let mut app = page(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            "  third",
            " next-parent",
        ]);
        app.cursor = 2;
        let original: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Esc));

        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                "  second",
                "  third",
                "  first",
                "   child",
                " next-parent"
            ]
        );
        assert_eq!(drain_jobs(&mut app).len(), 1, "two presses, one commit");
        assert_eq!(app.undo_stack.len(), 1);
        finish_outline(&mut app, &ctx);

        handle_key(&mut app, &ctx, key(KeyCode::Char('u')));
        assert_eq!(texts(&app), original.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(app.undo_stack.is_empty(), "one press put the whole drag back");
        assert_eq!(drain_jobs(&mut app).len(), 1);
        finish_outline(&mut app, &ctx);
    }

    /// The screen must never keep what the server refused. A rejected drag
    /// — failure or conflict — leaves the page as it was before `m`.
    #[test]
    fn a_refused_drag_restores_the_pre_mode_page() {
        for conflict in [false, true] {
            let ctx = offline_ctx();
            let mut app = page(&["title", " parent", "  first", "   child", "  second"]);
            app.cursor = 2;
            let before = app.lines.clone();

            handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
            handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
            handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
            handle_key(&mut app, &ctx, key(KeyCode::Esc));
            assert_ne!(texts(&app), texts_of(&before));
            drain_jobs(&mut app);

            let job = outline_job(&app);
            let outcome = if conflict {
                CommitOutcome::Conflict { job }
            } else {
                CommitOutcome::Failed {
                    job,
                    label: "outline move".into(),
                    msg: "500".into(),
                }
            };
            handle_commit_outcome(&mut app, &ctx, outcome);

            assert_eq!(texts(&app), texts_of(&before), "the drag is gone");
            assert_eq!(ids(&app), ids_of(&before));
            assert_eq!(app.cursor, 2);
            assert!(app.move_mode.is_none());
            assert!(app.outline_pending.is_none());
            assert_eq!(app.inflight, 0);
            assert!(
                app.status.contains("アウトライン操作に失敗"),
                "status: {}",
                app.status
            );
        }
    }

    /// The reader grabbed two lines. Outdenting makes the lines below read
    /// as their children, and the grab still holds exactly those two. `j`
    /// steps one line and never refuses, so the route out of a list is
    /// `h` → `j` → `l` with nothing in the way.
    #[test]
    fn the_grabbed_block_carries_out_of_the_list_after_an_outdent() {
        let ctx = test_ctx();
        let mut app = page(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            " next-parent",
        ]);
        app.cursor = 2;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('h')));
        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                " first",
                "  child",
                "  second",
                " next-parent"
            ]
        );
        assert_eq!(
            move_block_range(&app),
            Some((2, 3)),
            "indentation cannot grow the grab"
        );

        // `j` clears the line that now reads as a child, in one press.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                "  second",
                " first",
                "  child",
                " next-parent"
            ]
        );
        assert_eq!(move_block_range(&app), Some((3, 4)), "still the same two lines");
        assert!(app.hint_text(&[]).contains("MOVE"), "the mode is still up");

        // And `l` puts it under the next parent — out of the list it
        // started in, which is the whole point of the three presses.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
        assert_eq!(
            texts(&app),
            vec![
                "title",
                " parent",
                "  second",
                " next-parent",
                "  first",
                "   child"
            ]
        );
        assert!(drain_jobs(&mut app).is_empty(), "every press was local");

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Delete { id: a }, EditOp::Delete { id: b }, EditOp::Insert { anchor, lines }]
                if a == "id2" && b == "id3" && anchor == "_end"
                    && lines.iter().map(|(_, t)| t.as_str()).eq(["  first", "   child"])),
            "one commit recreates only the grabbed lines, at the end: {:?}",
            jobs[0].1
        );
        finish_outline(&mut app, &ctx);
    }

    /// In EDIT a modified arrow used to move the caret, because the arrow
    /// arms match whatever modifiers came with them. A cosense reader
    /// pressing Alt+Up expects a block to move, so the quiet caret move was
    /// the wrong answer: say where the block keys are and touch nothing.
    #[test]
    fn a_modified_arrow_in_edit_points_at_the_move_mode_instead_of_moving_the_caret() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b", " c"]);
        enter_session(&mut app, &ctx, 2, 1);

        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            for code in [KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right] {
                handle_session_key(&mut app, &ctx, modified(code, modifiers));
                assert_eq!(texts(&app), vec!["title", " a", " b", " c"]);
                assert_eq!(app.session.as_ref().map(|s| s.line), Some(2), "the caret line");
                assert_eq!(app.session.as_ref().map(|s| s.input.cur), Some(1), "and the caret");
                assert!(app.session.as_ref().unwrap().sel_from.is_none(), "and no selection");
                assert!(app.status.contains("移動モード"), "status: {}", app.status);
            }
        }
        assert!(drain_jobs(&mut app).is_empty());

        // Shift keeps its own meaning: it selects — across the line now.
        handle_session_key(&mut app, &ctx, modified(KeyCode::Up, KeyModifiers::SHIFT));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1, "Shift+Up carried the caret");
        assert_eq!(s.sel_from, Some((2, 1)), "and dragged the range's active end with it");
        assert!(app.selection.is_none(), "no line band in EDIT anymore");
        assert!(app.session.is_some(), "and EDIT is untouched throughout");
    }

    /// Two vertical steps: `j` moves one line and always goes, `J` steps
    /// over a whole sibling subtree and may have nowhere to go. Requiring
    /// the sibling step was the old mistake — every arrangement it refused
    /// was reachable anyway, so it only cost presses.
    #[test]
    fn a_line_step_always_goes_and_the_sibling_step_jumps_a_subtree() {
        let ctx = test_ctx();
        let mut app = page(&["title", "a", "b", " b-child", "c"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));

        // `J` clears b AND its child in one press.
        handle_key(&mut app, &ctx, modified(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", "b", " b-child", "a", "c"]);
        // `K` brings it back the same way.
        handle_key(&mut app, &ctx, modified(KeyCode::Char('K'), KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", "a", "b", " b-child", "c"]);

        // The bare arrow is the LINE step, not the sibling one: it stops
        // between b and its child, a position `J` cannot reach.
        handle_key(&mut app, &ctx, key(KeyCode::Down));
        assert_eq!(texts(&app), vec!["title", "b", "a", " b-child", "c"]);
        handle_key(&mut app, &ctx, key(KeyCode::Up));
        assert_eq!(texts(&app), vec!["title", "a", "b", " b-child", "c"]);

        // A shifted arrow is the same line step, not the sibling one.
        handle_key(&mut app, &ctx, modified(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", "b", "a", " b-child", "c"]);
        handle_key(&mut app, &ctx, modified(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", "a", "b", " b-child", "c"]);

        // And so is `j`.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", "b", "a", " b-child", "c"]);
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", "b", " b-child", "a", "c"]);
        // And back, press for press.
        handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
        assert_eq!(texts(&app), vec!["title", "a", "b", " b-child", "c"]);
        assert!(drain_jobs(&mut app).is_empty(), "every press was local");

        // The sibling step still explains itself when there is none, and
        // the block stays in hand.
        handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
        handle_key(&mut app, &ctx, modified(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert!(app.status.contains("兄弟ブロック"), "status: {}", app.status);
        assert!(app.move_mode.is_some());
        // While `j` from the same spot simply goes.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", "b", " a", " b-child", "c"]);

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert_eq!(drain_jobs(&mut app).len(), 1, "one commit for the whole drag");
        finish_outline(&mut app, &ctx);
    }

    /// The step is one SOURCE LINE even when the grab holds several lines
    /// and the line below has children of its own. This is the shape where
    /// a line step and a sibling step visibly differ.
    #[test]
    fn a_multi_line_grab_still_steps_one_line_at_a_time() {
        let ctx = test_ctx();
        let mut app = page(&["title", "A", " A1", "B", " B1"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert_eq!(move_block_range(&app), Some((1, 2)), "the grab is A and its child");

        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(
            texts(&app),
            vec!["title", "B", "A", " A1", " B1"],
            "one line: the block sits between B and B1"
        );
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(
            texts(&app),
            vec!["title", "B", " B1", "A", " A1"],
            "a second press clears B1 as well"
        );
        handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
        assert_eq!(texts(&app), vec!["title", "B", "A", " A1", " B1"], "and back");

        // `J` from the start would have cleared B's whole subtree at once.
        handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
        assert_eq!(texts(&app), vec!["title", "A", " A1", "B", " B1"]);
        handle_key(&mut app, &ctx, key(KeyCode::Char('J')));
        assert_eq!(texts(&app), vec!["title", "B", " B1", "A", " A1"]);

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert_eq!(drain_jobs(&mut app).len(), 1, "one commit for the whole drag");
        finish_outline(&mut app, &ctx);
    }

    /// Nothing in the mode can lose the grabbed lines, so if they are gone
    /// the page has drifted: the pre-grab arrangement goes back rather than
    /// leaving a half-dragged order that exists nowhere but the screen.
    #[test]
    fn a_lost_block_restores_the_arrangement_from_before_the_grab() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b", " c"]);
        app.cursor = 1;
        let before: Vec<(String, String)> =
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect();

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", " b", " a", " c"]);

        // Only something outside the mode could do this.
        app.lines.retain(|line| line.id != "id1");
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));

        assert!(app.move_mode.is_none());
        assert_eq!(
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect::<Vec<_>>(),
            before,
            "the arrangement from before the grab"
        );
        assert!(app.web_unsynced, "and the disagreement is recorded");
        assert!(drain_jobs(&mut app).is_empty(), "nothing was sent");
        assert!(app.status.contains("つかむ前の並び"), "status: {}", app.status);
    }

    /// Caps lock, or the habit of the `^g H/J/K/L` block bindings, must not
    /// commit a half-finished drag. A paste is not a move key, so it lets
    /// go first like every other key.
    #[test]
    fn shifted_move_keys_drag_and_a_paste_lets_go_first() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));

        handle_key(&mut app, &ctx, modified(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", " b", " a"]);
        assert!(app.move_mode.is_some(), "Shift+j drags");
        handle_key(&mut app, &ctx, modified(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert_eq!(texts(&app), vec!["title", " b", "  a"]);
        assert!(app.move_mode.is_some());
        assert!(drain_jobs(&mut app).is_empty(), "still local");

        handle_paste(&mut app, &ctx, "pasted");
        assert!(app.move_mode.is_none(), "the paste let go of the block");
        assert_eq!(drain_jobs(&mut app).len(), 1, "and the drag went out as one commit");
        // And the paste itself was handled, not swallowed: in READ it
        // points at the edit keys, which is what it does without a drag.
        assert!(app.status.contains("貼り付けは編集中に"), "status: {}", app.status);
        finish_outline(&mut app, &ctx);
    }

    /// Mouse capture asks the terminal for any-event tracking, so bare
    /// pointer motion reaches the handler. A trackpad brushed in passing
    /// must not commit a half-finished drag; a button still lets go.
    #[test]
    fn pointer_motion_does_not_let_go_of_the_block() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;
        app.rebuild(40);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));

        let event = |kind| MouseEvent { kind, column: 6, row: 3, modifiers: KeyModifiers::NONE };
        handle_mouse(&mut app, &ctx, event(MouseEventKind::Moved));
        assert!(app.move_mode.is_some(), "motion is not an action");
        handle_mouse(&mut app, &ctx, event(MouseEventKind::ScrollDown));
        assert!(app.move_mode.is_some(), "and the wheel only moves the viewport");
        assert!(drain_jobs(&mut app).is_empty(), "nothing was sent");

        handle_mouse(&mut app, &ctx, event(MouseEventKind::Down(MouseButton::Left)));
        assert!(app.move_mode.is_none(), "a button does let go");
        assert_eq!(drain_jobs(&mut app).len(), 1);
        finish_outline(&mut app, &ctx);
    }

    /// A selection is a range the reader chose by hand, and the drag would
    /// have to drop it: `m` refuses while one is open, the same answer the
    /// Alt and ^g block bindings give.
    #[test]
    fn a_selection_refuses_the_grab_rather_than_losing_it() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;
        app.selection = Some(Selection { anchor: 1, cursor: 2 });

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert!(app.move_mode.is_none());
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)), "kept as it was");
        assert!(app.status.contains("選択中"), "status: {}", app.status);
        assert!(drain_jobs(&mut app).is_empty());
    }

    /// The cursor is on the block, not on a line number: it follows the
    /// block by id while dragging, and follows the fresh id the commit
    /// gives it.
    #[test]
    fn the_cursor_rides_the_moved_block() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", " b", " a"]);
        assert_eq!(app.cursor, 2);

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert_eq!(texts(&app), vec!["title", " b", " a"]);
        assert_eq!(app.cursor, 2, "and the new id is where it sits");
        assert_eq!(app.lines[1].id, "id2");
        assert_ne!(app.lines[2].id, "id1");
        assert_eq!(drain_jobs(&mut app).len(), 1);
        finish_outline(&mut app, &ctx);
    }

    /// Anything that is not a move key lets go of the block FIRST: the drag
    /// is committed, and only then does the key do its ordinary job. A page
    /// can never change under a block still being carried.
    #[test]
    fn leaving_the_page_lets_go_of_the_block_first() {
        let ctx = test_ctx();
        let mut app = page(&["title", " a", " b"]);
        app.cursor = 1;

        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('[')));

        assert!(app.move_mode.is_none(), "the drag committed before anything moved");
        assert_eq!(texts(&app), vec!["title", " b", " a"]);
        assert_eq!(drain_jobs(&mut app).len(), 1);
        assert!(app.outline_pending.is_some());
        finish_outline(&mut app, &ctx);
    }

    #[test]
    fn outline_commit_gate_blocks_mutations_but_not_navigation_until_done() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.cursor = 1;

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert!(app.outline_pending.is_some());
        assert_eq!(app.inflight, 1);
        let optimistic: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
        let undo_len = app.undo_stack.len();

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_eq!(
            app.lines
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>(),
            optimistic
        );
        do_edit(
            &mut app,
            &ctx,
            "rapid edit",
            vec![EditOp::Replace {
                id: "id2".into(),
                text: "changed".into(),
            }],
        );
        assert_eq!(app.lines[2].text, "two", "a rapid edit is blocked");
        assert!(!undo(&mut app, &ctx), "a rapid undo is blocked");
        assert_eq!(app.undo_stack.len(), undo_len);
        handle_key(&mut app, &ctx, key(KeyCode::Char('e')));
        assert!(app.session.is_none(), "EDIT cannot open under the gate");

        app.rebuild(40); // the event loop lays out the optimistic frame
        handle_key(&mut app, &ctx, key(KeyCode::Down));
        assert_eq!(app.cursor, 2, "navigation remains available");
        assert_eq!(
            drain_jobs(&mut app).len(),
            1,
            "only the first outline action was queued"
        );

        finish_outline(&mut app, &ctx);
        assert!(app.outline_pending.is_none());
        assert_eq!(app.inflight, 0);
        assert!(undo(&mut app, &ctx), "Done releases undo");
        assert_eq!(app.lines[1].text, "one");
    }

    #[test]
    fn outline_success_after_navigation_only_releases_the_old_gate() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.cursor = 1;
        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Down, KeyModifiers::CONTROL),
        );
        assert!(app.outline_pending.is_some());

        // READ navigation is allowed while the structural commit is pending.
        app.project = "next-project".into();
        app.page_id = "next-page".into();
        app.title = "next title".into();
        app.lines = page(&["next title", "current body"]).lines;
        app.status = "current page status".into();
        app.own_commits.push_back("current-echo".into());
        if let Ok(mut target) = app.poll_target.lock() {
            *target = (app.project.clone(), app.title.clone());
        }
        let epoch = app.server_epoch_now();
        let current_lines: Vec<(String, String)> =
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect();
        let current_target = app.poll_target.lock().unwrap().clone();
        let job = outline_job(&app);

        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job,
                label: "old outline".into(),
                title: "old renamed title".into(),
                commit_id: "old-commit".into(),
            },
        );

        assert!(app.outline_pending.is_none());
        assert_eq!(app.inflight, 0);
        assert_eq!(app.project, "next-project");
        assert_eq!(app.page_id, "next-page");
        assert_eq!(app.title, "next title");
        assert_eq!(
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect::<Vec<_>>(),
            current_lines
        );
        assert_eq!(app.status, "current page status");
        assert_eq!(app.own_commits.iter().cloned().collect::<Vec<_>>(), vec!["current-echo"]);
        assert_eq!(app.server_epoch_now(), epoch);
        assert_eq!(*app.poll_target.lock().unwrap(), current_target);
    }

    #[test]
    fn outline_success_after_away_and_back_refreshes_instead_of_retargeting() {
        let ctx = offline_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.cursor = 1;
        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Down, KeyModifiers::CONTROL),
        );
        assert!(app.outline_pending.is_some());

        // The immutable page id is the same after an away/back trip, but
        // this is a newly installed snapshot that may predate the commit.
        app.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        app.title = "current title".into();
        app.status = "current visit".into();
        app.own_commits.push_back("current-echo".into());
        let epoch = app.server_epoch_now();
        let job = outline_job(&app);

        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job,
                label: "old outline".into(),
                title: "old title".into(),
                commit_id: "old-commit".into(),
            },
        );

        assert!(app.outline_pending.is_none());
        assert_eq!(app.inflight, 0);
        assert_eq!(app.title, "current title", "the old result cannot rename this visit");
        assert_eq!(
            app.own_commits.iter().cloned().collect::<Vec<_>>(),
            vec!["current-echo"],
            "a failed refresh cannot file an old echo under the new visit"
        );
        assert!(app.server_epoch_now() > epoch);
        assert!(app.web_unsynced, "a failed refresh leaves the stale visit read-only");
        assert!(app.outline_refresh_needed);
        assert!(
            app.status.contains("失敗") || app.status.contains("failed"),
            "the same-page stale visit attempted an authoritative refresh: {}",
            app.status
        );

        // The successful commit may have replaced the IDs we still display.
        // No ordinary edit is allowed to target those stale IDs.
        drain_jobs(&mut app);
        let before: Vec<(String, String)> =
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect();
        let id = app.lines[1].id.clone();
        do_edit(
            &mut app,
            &ctx,
            "stale edit",
            vec![EditOp::Replace { id, text: "unsafe".into() }],
        );
        assert_eq!(
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect::<Vec<_>>(),
            before
        );
        assert!(drain_jobs(&mut app).is_empty());
        assert!(app.status.contains("開き直して"));
    }

    /// The gate waits for one job id, so an ordinary commit still on its way
    /// to the server neither delays the action nor releases it. Order is the
    /// serial worker's business, identity is the id's.
    #[test]
    fn outline_action_under_an_unrelated_commit_waits_for_its_own_outcome() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.cursor = 1;
        let earlier = queue_commit(
            &mut app,
            "earlier edit",
            vec![EditOp::Replace { id: "id0".into(), text: "title".into() }],
        )
        .expect("the worker took the ordinary job");

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Right, KeyModifiers::CONTROL),
        );
        assert_eq!(app.lines[1].text, " one", "the action starts under an in-flight commit");
        let outline = outline_job(&app);
        assert_ne!(outline, earlier);
        assert_eq!(app.inflight, 2);
        assert_eq!(drain_jobs(&mut app).len(), 2, "both jobs reached the serial worker");

        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: earlier,
                label: "earlier edit".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        assert!(
            app.outline_pending.is_some(),
            "an unrelated outcome cannot release the gate"
        );
        assert_eq!(app.lines[1].text, " one");
        assert!(!undo(&mut app, &ctx), "mutations stay blocked until the action lands");

        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: outline,
                label: "outline indent".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        assert!(app.outline_pending.is_none(), "its own outcome completes it");
        assert_eq!(app.inflight, 0);
        assert!(undo(&mut app, &ctx), "the released gate lets history run again");
        assert_eq!(app.lines[1].text, "one");
    }

    /// An outcome the gate does not own keeps its ordinary handling, and the
    /// outline action is still decided by its own outcome afterwards.
    #[test]
    fn an_unrelated_failure_under_the_gate_keeps_its_own_handling() {
        let ctx = offline_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.cursor = 1;
        let original: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
        let earlier = queue_commit(
            &mut app,
            "earlier edit",
            vec![EditOp::Replace { id: "id0".into(), text: "title".into() }],
        )
        .expect("the worker took the ordinary job");

        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Down, KeyModifiers::CONTROL),
        );
        let outline = outline_job(&app);
        let optimistic: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
        assert_ne!(optimistic, original);

        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Failed {
                job: earlier,
                label: "earlier edit".into(),
                msg: "500".into(),
            },
        );
        assert!(app.web_unsynced, "the unrelated failure still marks the page divergent");
        assert!(
            app.status.contains("コミットに失敗"),
            "the unrelated failure keeps its own status: {}",
            app.status
        );
        assert!(app.outline_pending.is_some(), "and it does not touch the gate");
        assert_eq!(
            app.lines.iter().map(|line| line.text.clone()).collect::<Vec<_>>(),
            optimistic
        );

        handle_commit_outcome(&mut app, &ctx, CommitOutcome::Conflict { job: outline });
        assert!(app.outline_pending.is_none());
        assert_eq!(
            app.lines.iter().map(|line| line.text.clone()).collect::<Vec<_>>(),
            original,
            "the rejected action is rolled back to the pre-action page"
        );
        assert!(
            app.status.contains("アウトライン操作に失敗"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn failed_and_conflicting_outline_commits_restore_the_pre_action_page() {
        for conflict in [false, true] {
            let ctx = offline_ctx();
            let mut app = page(&["title", "one", "two"]);
            app.cursor = 1;
            let original = app.lines.clone();
            let generation = app.gen.load(std::sync::atomic::Ordering::SeqCst);

            handle_key(
                &mut app,
                &ctx,
                modified(KeyCode::Down, KeyModifiers::CONTROL),
            );
            assert_ne!(
                app.lines
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>(),
                original
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>()
            );
            let job = outline_job(&app);
            let outcome = if conflict {
                CommitOutcome::Conflict { job }
            } else {
                CommitOutcome::Failed {
                    job,
                    label: "outline move".into(),
                    msg: "500".into(),
                }
            };
            handle_commit_outcome(&mut app, &ctx, outcome);

            assert_eq!(
                app.lines
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>(),
                original
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>(),
                "the optimistic move is gone even when the reload itself fails"
            );
            assert!(app.outline_pending.is_none());
            assert_eq!(app.inflight, 0);
            assert!(app.gen.load(std::sync::atomic::Ordering::SeqCst) > generation);
            assert!(
                app.status.contains("アウトライン操作に失敗"),
                "status: {}",
                app.status
            );
            assert!(
                app.status.contains(if conflict { "競合" } else { "500" }),
                "status: {}",
                app.status
            );
        }
    }

    #[test]
    fn structural_outline_history_is_gated_and_uses_outline_recovery() {
        for back in [true, false] {
            for conflict in [false, true] {
                let ctx = offline_ctx();
                let mut app = page(&["title", "one", "two", "tail"]);
                app.cursor = 1;
                app.selection = Some(Selection { anchor: 1, cursor: 2 });
                handle_key(
                    &mut app,
                    &ctx,
                    modified(KeyCode::Down, KeyModifiers::CONTROL),
                );
                drain_jobs(&mut app);
                finish_outline(&mut app, &ctx);

                if !back {
                    assert!(undo(&mut app, &ctx));
                    drain_jobs(&mut app);
                    finish_outline(&mut app, &ctx);
                }

                let before_lines: Vec<(String, String)> =
                    app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect();
                let before_cursor = app.cursor;
                let before_selection = app.selection;
                let before_undo = app.undo_stack.clone();
                let before_redo = app.redo_stack.clone();
                app.history_dropped = true;

                assert!(if back { undo(&mut app, &ctx) } else { redo(&mut app, &ctx) });
                assert!(app.outline_pending.is_some(), "move history installs the structural gate");
                assert_eq!(drain_jobs(&mut app).len(), 1);
                let job = outline_job(&app);
                let outcome = if conflict {
                    CommitOutcome::Conflict { job }
                } else {
                    CommitOutcome::Failed {
                        job,
                        label: if back { "undo outline".into() } else { "redo outline".into() },
                        msg: "500".into(),
                    }
                };
                handle_commit_outcome(&mut app, &ctx, outcome);

                assert_eq!(
                    app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect::<Vec<_>>(),
                    before_lines,
                    "rejected history restores the pre-action lines"
                );
                assert_eq!(app.cursor, before_cursor);
                assert_eq!(app.selection, before_selection);
                assert_eq!(app.undo_stack, before_undo);
                assert_eq!(app.redo_stack, before_redo);
                assert!(app.history_dropped);
                assert!(app.outline_pending.is_none());
                assert_eq!(app.inflight, 0);
            }
        }
    }

    #[test]
    fn structural_undo_queues_under_an_unrelated_commit_and_blocks_opposite_mutations() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "tail"]);
        app.cursor = 1;
        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Down, KeyModifiers::CONTROL),
        );
        drain_jobs(&mut app);
        finish_outline(&mut app, &ctx);

        // An ordinary commit is on its way to the server. Structural history
        // is queued behind it and gates on its own job id.
        let earlier = queue_commit(
            &mut app,
            "earlier edit",
            vec![EditOp::Replace { id: "id0".into(), text: "title".into() }],
        )
        .expect("the worker took the ordinary job");
        drain_jobs(&mut app);

        assert!(undo(&mut app, &ctx), "structural history no longer waits at the door");
        assert!(app.outline_pending.is_some());
        assert_ne!(outline_job(&app), earlier);
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: earlier,
                label: "earlier edit".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        assert!(
            app.outline_pending.is_some(),
            "the older commit's outcome is not the one the gate waits for"
        );
        let undone: Vec<(String, String)> =
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect();
        let stacks = (app.undo_stack.clone(), app.redo_stack.clone());
        assert!(!redo(&mut app, &ctx), "the rapid opposite action is blocked");
        let edit_id = app.lines[1].id.clone();
        do_edit(
            &mut app,
            &ctx,
            "rapid edit",
            vec![EditOp::Replace { id: edit_id, text: "changed".into() }],
        );
        assert_eq!(
            app.lines.iter().map(|line| (line.id.clone(), line.text.clone())).collect::<Vec<_>>(),
            undone,
            "other mutations are blocked too"
        );
        assert_eq!((app.undo_stack.clone(), app.redo_stack.clone()), stacks);
        assert_eq!(drain_jobs(&mut app).len(), 1, "only the structural undo was queued");
    }

    #[test]
    fn history_recovery_validates_dependent_entries_in_pop_order() {
        let app = page(&["title", "body"]);
        let mut stack = vec![
            (
                "older delete".into(),
                vec![EditOp::Delete { id: "restored-id".into() }],
            ),
            (
                "newer insert".into(),
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("restored-id".into(), "restored".into())],
                }],
            ),
        ];

        assert_eq!(retain_replayable_history(&mut stack, &app.lines), 0);
        assert_eq!(stack.len(), 2, "the newer undo recreates the id needed by the older one");
    }

    #[test]
    fn successful_outline_recovery_restores_history_cursor_and_selection_by_id() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three", "tail"]);
        app.cursor = 1;
        app.selection = Some(Selection { anchor: 3, cursor: 1 });
        app.undo_stack.push((
            "older undo".into(),
            vec![EditOp::Replace { id: "id2".into(), text: "old two".into() }],
        ));
        app.redo_stack.push((
            "older redo".into(),
            vec![EditOp::Replace { id: "id4".into(), text: "new tail".into() }],
        ));
        app.history_dropped = true;
        let pending = outline_snapshot(&app);

        let mut server_lines = app.lines.clone();
        server_lines.insert(
            1,
            PageLine {
                id: "web-id".into(),
                text: "web line".into(),
                user_id: String::new(),
                created: 0,
                updated: 0,
            },
        );
        recover_outline_action_with(
            &mut app,
            &ctx,
            pending,
            "conflict".into(),
            move |app, ctx| {
                // Model the state-destructive part of a successful set_page.
                app.lines = server_lines;
                app.cursor = 0;
                app.selection = None;
                app.undo_stack.clear();
                app.redo_stack.clear();
                app.history_dropped = false;
                app.mark_synced();
                rerender(app, ctx);
                true
            },
        );

        assert_eq!(app.cursor, 2, "the active line follows id1, not its old index");
        assert_eq!(
            app.selection,
            Some(Selection { anchor: 4, cursor: 2 }),
            "both directional endpoints follow their saved IDs"
        );
        assert_eq!(app.undo_stack.len(), 1);
        assert_eq!(app.undo_stack[0].0, "older undo");
        assert_eq!(app.redo_stack.len(), 1);
        assert_eq!(app.redo_stack[0].0, "older redo");
        assert!(app.history_dropped);
        assert!(app.status.contains("サーバーの内容を読み直しました"));
    }

    #[test]
    fn outline_replace_undo_redo_keep_both_selection_directions_active() {
        let ctx = test_ctx();
        for selection in [
            Selection {
                anchor: 1,
                cursor: 3,
            },
            Selection {
                anchor: 3,
                cursor: 1,
            },
        ] {
            let mut app = page(&["title", "one", " two", "three", "tail"]);
            app.selection = Some(selection);
            app.cursor = selection.cursor;

            handle_key(
                &mut app,
                &ctx,
                modified(KeyCode::Right, KeyModifiers::CONTROL),
            );
            drain_jobs(&mut app);
            finish_outline(&mut app, &ctx);

            assert!(undo(&mut app, &ctx));
            assert_eq!(
                app.selection,
                Some(selection),
                "undo preserves anchor and active end"
            );
            assert_eq!(
                app.cursor, selection.cursor,
                "undo follows the active selection endpoint"
            );

            assert!(redo(&mut app, &ctx));
            assert_eq!(
                app.selection,
                Some(selection),
                "redo preserves anchor and active end"
            );
            assert_eq!(
                app.cursor, selection.cursor,
                "redo follows the active selection endpoint"
            );
        }
    }

    #[test]
    fn outline_keys_require_exact_modifiers_and_prefix_is_one_shot() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.cursor = 1;
        let original: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();

        for modifiers in [
            KeyModifiers::SHIFT | KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            handle_key(&mut app, &ctx, modified(KeyCode::Down, modifiers));
            assert_eq!(
                app.lines
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>(),
                original
            );
            assert_eq!(app.cursor, 1, "modified arrows never become plain arrows");
        }
        handle_key(
            &mut app,
            &ctx,
            modified(KeyCode::Char('j'), KeyModifiers::ALT),
        );
        assert_eq!(app.cursor, 1, "Alt+j is intentionally not bound");
        assert!(drain_jobs(&mut app).is_empty());

        handle_key(&mut app, &ctx, ctrl('g'));
        assert!(app.outline_prefix);
        assert!(app.hint_body(&[]).contains("h/j/k/l"));
        handle_key(&mut app, &ctx, key(KeyCode::Char('x')));
        assert!(!app.outline_prefix);
        assert!(app.status.contains("取り消しました"));
        assert_eq!(app.cursor, 1, "the unknown second key was consumed");

        handle_key(&mut app, &ctx, ctrl('g'));
        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(!app.outline_prefix);
        assert!(app.status.contains("取り消しました"));

        handle_key(&mut app, &ctx, ctrl('g'));
        handle_key(&mut app, &ctx, key(KeyCode::Char('l')));
        assert_eq!(app.lines[1].text, " one");
        assert_eq!(drain_jobs(&mut app).len(), 1);

        let mut block = page(&["title", " root", "  child", "peer"]);
        block.cursor = 1;
        handle_key(&mut block, &ctx, ctrl('g'));
        handle_key(
            &mut block,
            &ctx,
            modified(KeyCode::Char('L'), KeyModifiers::SHIFT),
        );
        assert_eq!(block.lines[1].text, "  root");
        assert_eq!(block.lines[2].text, "   child");
        assert_eq!(drain_jobs(&mut block).len(), 1);
    }

    #[test]
    fn read_only_project_never_enters_or_applies_editing() {
        let ctx = test_ctx();
        let mut app = page(&["t", "body"]);
        app.rebuild(40);
        app.editable = false;

        enter_session(&mut app, &ctx, 1, 4);
        assert!(app.session.is_none());
        assert!(app.status.contains("編集権限"));

        let before: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        open_line(&mut app, &ctx, false);
        do_edit(
            &mut app,
            &ctx,
            "forbidden",
            vec![EditOp::Replace { id: "id1".into(), text: "changed".into() }],
        );
        assert_eq!(app.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>(), before);
        assert!(drain_jobs(&mut app).is_empty());

        let ctrl_e = event::KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut app, &ctx, ctrl_e), Action::Continue));
    }

    #[test]
    fn session_edits_many_lines_without_leaving() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);

        // e-style entry on line 1, caret at end; type; ↓ commits and moves
        let end = app.lines[1].text.len();
        enter_session(&mut app, &ctx, 1, end);
        type_str(&mut app, &ctx, "!");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!");
        assert!(drain_jobs(&mut app).is_empty(), "typing alone commits nothing");
        handle_session_key(&mut app, &ctx, key(KeyCode::Down));
        assert_eq!(app.lines[1].text, "one!", "leaving the line applied it locally");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text == "one!"));

        // session continued on line 2: Enter at EOL creates line 3 and
        // KEEPS the session going; type there; Esc commits the text
        assert_eq!(app.session.as_ref().unwrap().line, 2);
        handle_session_key(&mut app, &ctx, key(KeyCode::End));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines.len(), 4, "a real line exists locally at once");
        assert_eq!(app.session.as_ref().unwrap().line, 3);
        type_str(&mut app, &ctx, "three");
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.session.is_none());
        assert_eq!(app.lines[3].text, "three");
        let jobs = drain_jobs(&mut app);
        // split (replace+insert) then the final text replace
        assert_eq!(jobs.len(), 2);
        assert!(matches!(&jobs[0].1[1], EditOp::Insert { .. }));
        assert!(matches!(&jobs[1].1[0], EditOp::Replace { text, .. } if text == "three"));
        // ids: the insert's id is the line's id — client-generated, stable
        if let EditOp::Insert { lines, .. } = &jobs[0].1[1] {
            assert_eq!(lines[0].0, app.lines[3].id);
        }
    }

    #[test]
    fn session_split_mid_line_inherits_indent() {
        let ctx = test_ctx();
        let mut app = page(&["title", " indented line"]);
        app.rebuild(40);
        // caret between "indented" and " line"
        let caret = " indented".len();
        enter_session(&mut app, &ctx, 1, caret);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[1].text, " indented");
        assert_eq!(app.lines[2].text, "  line", "tail keeps the indent");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, 1, "caret sits after the inherited indent");
    }

    #[test]
    fn enter_on_empty_bullet_escapes_the_list() {
        let ctx = test_ctx();
        let mut app = page(&["t", " item", "  ", "after"]);
        app.rebuild(40);
        // caret on the empty bullet (whitespace-only, level 2)
        enter_session(&mut app, &ctx, 2, 2);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        // the empty bullet dissolved into a true blank…
        assert_eq!(app.lines[2].text, "", "spaces removed from the empty bullet");
        // …and the fresh line is flush, no inherited indent
        assert_eq!(app.lines[3].text, "");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3);
        assert_eq!(s.input.cur, 0);
        assert_eq!(app.lines[4].text, "after");
        let jobs = drain_jobs(&mut app);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text.is_empty()));
        assert!(matches!(&jobs[0].1[1], EditOp::Insert { lines, .. } if lines[0].1.is_empty()));
        // a NON-empty bullet still inherits its indent on Enter
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        enter_session(&mut app, &ctx, 1, " item".len());
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ", "normal path unchanged");
    }

    #[test]
    fn session_join_up_at_bol() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 0);
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(app.lines.len(), 2);
        assert_eq!(app.lines[1].text, "onetwo");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1);
        assert_eq!(s.input.cur, 3, "caret on the junction");
        let jobs = drain_jobs(&mut app);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text == "onetwo"));
        assert!(matches!(&jobs[0].1[1], EditOp::Delete { .. }));
    }

    #[test]
    fn undo_redo_roundtrip_with_queue() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        // x-style delete of line 1
        do_edit(&mut app, &ctx, "delete", vec![EditOp::Delete { id: "id1".into() }]);
        assert_eq!(app.lines.len(), 2);
        undo(&mut app, &ctx);
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.lines[1].text, "one", "undo restored the line in place");
        redo(&mut app, &ctx);
        assert_eq!(app.lines.len(), 2);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 3, "delete, undo, redo each committed");
        assert!(jobs[1].0.starts_with("undo"));
        assert!(jobs[2].0.starts_with("redo"));
    }

    /// The index is a list of links, so the mouse treats it as one: the
    /// wheel moves through it and a click opens the row it landed on.
    #[test]
    fn a_click_in_the_index_opens_that_page() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(100);
        let mk = |title: &str| cosense::index::Entry {
            title: title.into(),
            updated: now_secs(),
            descriptions: vec!["body".into()],
            unread: false,
        };
        let mut entries = vec![mk("最初"), mk("二番目"), mk("三番目")];
        // …and enough behind them that the list has somewhere to scroll.
        entries.extend((0..30).map(|i| mk(&format!("その他{i}"))));
        let n = entries.len();
        app.index = Some(cosense::index::Index::new(entries, n));
        // A frame has to have been drawn: the click is answered with the
        // geometry that was on screen.
        let mut t = Terminal::new(TestBackend::new(100, 10)).unwrap();
        t.draw(|f| ui(f, &mut app, &ctx)).unwrap();

        let at = |kind: MouseEventKind, col: u16, row: u16| MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // The wheel scrolls the list under the pointer and leaves the
        // selection where it is — content scrolls, cursors do not.
        for _ in 0..3 {
            handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollDown, 5, 3));
        }
        assert_eq!(app.index.as_ref().unwrap().cursor, 0, "the selection stayed");
        assert!(app.index.as_ref().unwrap().scroll > 0, "and the view moved");
        assert!(app.index_scrolled_at.is_some(), "so the scrollbar shows itself");
        for _ in 0..3 {
            handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollUp, 5, 3));
        }
        assert_eq!(app.index.as_ref().unwrap().scroll, 0);

        // Over the excerpt dock the wheel scrolls the excerpt instead.
        let preview_x = app.index_preview_rect.x + 2;
        let preview_y = app.index_preview_rect.y;
        handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollDown, preview_x, preview_y));
        assert_eq!(app.index.as_ref().unwrap().preview_scroll, 1);
        assert_eq!(app.index.as_ref().unwrap().cursor, 0, "and leaves the list alone");

        // A click on the third row opens the third page — one click. The
        // opening itself is a page fetch, which a test has no server for;
        // what is checked here is that the click resolved to THAT row and
        // went to open it (the status names what was asked for).
        handle_mouse(
            &mut app,
            &ctx,
            at(MouseEventKind::Down(MouseButton::Left), 5, 3),
        );
        // The opening itself is a page fetch, which a test has no server
        // for — so the list is still there (a failed open keeps the
        // reader's place). What is checked is which row it resolved to.
        assert!(
            app.status.contains("三番目"),
            "the third row was opened, not another: {}",
            app.status
        );
    }

    /// The index is a place, so it is on the back stack: leaving it for a
    /// page and pressing `[` comes back to the list, not to whatever page
    /// happened to be open before it.
    #[test]
    fn the_index_is_somewhere_you_can_come_back_to() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.project = "proj".into();
        app.title = "A".into();
        app.index_project = "proj".into();
        app.index = Some(cosense::index::Index::new(
            vec![cosense::index::Entry {
                title: "B".into(),
                updated: now_secs(),
                descriptions: vec![],
                unread: false,
            }],
            1,
        ));
        assert!(matches!(
            app.here(),
            Place::Index { project, state }
                if project == "proj" && state.selected().map(|e| e.title.as_str()) == Some("B")
        ));

        // Opening a row that cannot be fetched (no server in a test) puts
        // the reader back in the list rather than dropping them onto the
        // page they had left.
        open_from_index(&mut app, &ctx, Some(("B".into(), false)));
        assert!(app.index.is_some(), "the list survives a failed open");
        assert!(app.history.is_empty(), "and nothing went on the stack");

        // `[/other-project]` opens someone else's index, and that is a
        // place too — reached from the PAGE the reader is on.
        app.history.clear();
        app.index = None;
        activate_link(&mut app, &ctx, LinkItem::ProjectIndex { project: "help-jp".into() });
        // (No network in tests: the index only opens when the list arrives.
        // Either way the reader must not lose their place.)
        if app.index.is_some() {
            assert_eq!(app.index_project, "help-jp");
            assert_eq!(
                app.history.last(),
                Some(&Place::Page { project: "proj".into(), title: "A".into() })
            );
        } else {
            assert!(app.history.is_empty(), "a failed open leaves the stack alone");
        }
    }

    /// Back is back even while printable keys normally belong to the
    /// filter. The index itself goes onto the forward stack with its exact
    /// cursor/filter/scroll state, so returning does not restart the list.
    #[test]
    fn brackets_and_escape_leave_and_restore_the_index() {
        let ctx = test_ctx();
        let mut app = page(&["A", "one"]);
        app.project = "proj".into();
        app.title = "A".into();
        app.index_project = "proj".into();
        let entries: Vec<_> = (0..12)
            .map(|i| cosense::index::Entry {
                title: format!("B{i}"),
                updated: now_secs() - i,
                descriptions: vec![format!("body {i}")],
                unread: i % 2 == 0,
            })
            .collect();
        let mut index = cosense::index::Index::new(entries, 12);
        index.cursor = 4;
        index.scroll = 2;
        index.preview_scroll = 1;
        index.focus = cosense::index::Pane::Preview;
        let saved = index.clone();
        app.index = Some(index);
        app.history.push(Place::Page { project: "proj".into(), title: "A".into() });

        handle_index_key(&mut app, &ctx, key(KeyCode::Char('[')));
        assert!(app.index.is_none(), "[ leaves the index rather than filtering");
        assert!(matches!(
            app.forward.last(),
            Some(Place::Index { project, state }) if project == "proj" && **state == saved
        ));

        go_history(&mut app, &ctx, false);
        assert_eq!(app.index.as_ref(), Some(&saved), "] restores the exact list state");

        handle_index_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.index.is_none(), "Esc uses the same back route");
    }

    /// The same screen answers in English when the environment asks for
    /// it. Only the words change: keys, flags and notation are names, not
    /// words, and stay put in both.
    #[test]
    fn the_whole_screen_can_be_read_in_english() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "body"]);
        app.editable = true;
        cosense::lang::set_for_thread(cosense::lang::Lang::En);

        app.status = String::new();
        app.sync_state = capability::SyncState::Live; // no sync tag in front
        assert_eq!(
            app.hint_text(&[]),
            "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit"
        );
        handle_key(&mut app, &ctx, key(KeyCode::Char('y')));
        assert!(app.status.contains("copied"), "status: {}", app.status);

        app.overlay = Some(Overlay::Help);
        // Tall enough for every help line, diagram notes included: the
        // panel clips from the bottom, and this test reads the whole list.
        let mut t = Terminal::new(TestBackend::new(100, 34)).unwrap();
        t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let screen: String = {
            let buf = t.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(screen.contains("move        j/k"), "help is in English: {screen}");
        assert!(screen.contains("Esc close"));
        assert!(!screen.contains("移動"), "and nothing Japanese is left behind");
        // Keys and env vars are names, not words: they read the same either way.
        assert!(screen.contains("COSENSE_WEB_IDLE_SECS"));
        assert!(screen.contains("^u/^d · PgUp/PgDn"));

        cosense::lang::set_for_thread(cosense::lang::Lang::Ja);
    }

    /// Eyeball the move mode: the grabbed block wears the selection
    /// highlight and the footer names the mode and its keys.
    #[test]
    #[ignore]
    fn move_mode_screen_dump() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&[
            "アウトライン編集の実験",
            "はじめに",
            " 前提を書く",
            "  さらに細かい話",
            "本文",
            " 具体例",
        ]);
        app.cursor = 2;
        app.rebuild(72);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        println!("status: {}", app.status);
        let mut t = Terminal::new(TestBackend::new(72, 14)).unwrap();
        t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = t.backend().buffer().clone();
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                let cell = buf.cell((x, y)).unwrap();
                row.push_str(cell.symbol());
            }
            let held = (0..buf.area.width)
                .any(|x| buf.cell((x, y)).map(|c| c.bg == SEL_BG).unwrap_or(false));
            // "HELD" marks the rows carrying the grabbed block's highlight.
            println!("{}|{row}|", if held { "HELD" } else { "    " });
        }
    }

    /// Eyeball the help and footer: prints the drawn screen so the mixed
    /// language can be seen rather than argued about.
    /// `cargo test -- --ignored --nocapture help_screen_dump`
    #[test]
    #[ignore]
    fn help_screen_dump() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["ページ", "本文の行"]);
        app.editable = true;
        app.overlay = Some(Overlay::Help);
        let mut t = Terminal::new(TestBackend::new(100, 34)).unwrap();
        t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = t.backend().buffer().clone();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            println!("|{}|", row.trim_end());
        }
    }

    /// Eyeball the index: prints the drawn screen so the look can be
    /// judged, not just asserted. `cargo test -- --ignored --nocapture
    /// index_screen_dump`
    #[test]
    #[ignore]
    fn index_screen_dump() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(100);
        let mk = |title: &str, mins: i64, unread: bool, desc: &[&str]| cosense::index::Entry {
            title: title.into(),
            updated: now_secs() - mins * 60,
            descriptions: desc.iter().map(|s| s.to_string()).collect(),
            unread,
        };
        app.index = Some(cosense::index::Index::new(
            vec![
                mk("改善案", 1, true, &["from [テスト]", "進め方", " 方針: 編集は EDIT セッションを本筋にする", " READ は読むためのモードに寄せる"]),
                mk("画像表示テスト", 60 * 26, false, &["画像の出方を並べたページ", "[https://gyazo.com/abc]"]),
                mk("ブラケット記法テスト", 60 * 24 * 9, false, &["各行は「`ソース` → 実際の描画」の形で並べてある", "`[]` → []"]),
                mk("websocket 同期の設計メモ", 60 * 24 * 40, true, &["socket.io の生フレームで話す", "code:frame.txt", " 0{\"sid\":…}"]),
                mk("テスト", 60 * 24 * 400, false, &["ここはテスト用のページ"]),
            ],
            137,
        ));
        // …and a real project's worth of pages, where the scrollbar has
        // something to say.
        for i in 0..200 {
            let e = mk(&format!("ページ{i}"), 60 * (i as i64 + 2), i % 7 == 0, &["本文"]);
            app.index.as_mut().unwrap().entries.push(e);
        }
        for (w, h) in [(100u16, 14u16), (78, 12)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
            let buf = t.backend().buffer().clone();
            println!("\n┌{}┐  ({w}x{h})", "─".repeat(w as usize));
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                    .collect();
                println!("│{row}│");
            }
            println!("└{}┘", "─".repeat(w as usize));
        }
    }

    /// The index draws a full-width list above a shallow excerpt dock, and
    /// gives the whole height to the list when `--preview auto` is below
    /// its established 80-column threshold.
    #[test]
    fn the_index_draws_a_list_above_a_shallow_excerpt() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(80);
        app.index = Some(cosense::index::Index::new(
            vec![
                cosense::index::Entry {
                    title: "改善案".into(),
                    updated: now_secs() - 60,
                    descriptions: vec!["from [テスト]".into(), "進め方".into()],
                    unread: true,
                },
                cosense::index::Entry {
                    title: "画像表示テスト".into(),
                    updated: now_secs() - 86_400,
                    descriptions: vec!["画像の出方を並べたページ".into()],
                    unread: false,
                },
            ],
            2,
        ));

        let draw = |app: &mut App, cols: u16| -> Vec<String> {
            let mut t = Terminal::new(TestBackend::new(cols, 10)).unwrap();
            t.draw(|f| ui(f, app, &ctx)).unwrap();
            let buf = t.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf.cell((x, y)).map_or(" ".into(), |c| c.symbol().to_string()))
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };

        // A wide (CJK) glyph occupies two cells, the second of which is
        // blank; the text is read back without those.
        let flat = |s: &str| s.replace(' ', "");
        let has = |rows: &[String], needle: &str| rows.iter().any(|r| flat(r).contains(needle));

        // Wide: the list spends the full row; the excerpt starts below it.
        let rows = draw(&mut app, 100);
        assert!(rows[0].contains("proj"), "header names the project: {:?}", rows[0]);
        assert!(flat(&rows[1]).contains("改善案"), "first row: {:?}", rows[1]);
        let layout = cosense::index::layout(ctx.preview, 100, 8);
        let excerpt_y = 1 + layout.list as usize + layout.gap as usize;
        assert!(
            flat(&rows[excerpt_y]).contains("改善案"),
            "the page under the cursor heads the excerpt below: {:?}",
            rows[excerpt_y]
        );
        assert!(has(&rows[excerpt_y + 1..], "進め方"), "…including its first lines: {rows:?}");
        assert!(rows.last().unwrap().contains("Esc"), "footer: {:?}", rows.last());

        // Narrow: no excerpt, and the list has the height to itself. If a
        // resize removed the focused excerpt, focus returns to what remains.
        app.index.as_mut().unwrap().focus = cosense::index::Pane::Preview;
        let rows = draw(&mut app, 70);
        assert!(flat(&rows[1]).contains("改善案"));
        assert!(!has(&rows, "進め方"), "under 80 columns the excerpt is gone: {rows:?}");
        assert_eq!(app.index.as_ref().unwrap().focus, cosense::index::Pane::List);

        // Moving the cursor updates the excerpt.
        app.index.as_mut().unwrap().move_cursor(1);
        let rows = draw(&mut app, 100);
        assert!(has(&rows, "画像の出方"), "the preview follows the cursor: {rows:?}");
    }

    /// Opening a name and walking away must leave nothing behind: a title
    /// with no body is not a page.
    #[test]
    fn an_empty_new_page_is_never_created() {
        let ctx = test_ctx();
        let mut app = page(&["just a title"]);
        app.title = "just a title".into(); // the template's title line
        app.page_id = String::new();
        app.rebuild(40);

        app.cursor = 0;
        open_line(&mut app, &ctx, false); // the picker's "create" landing
        assert_eq!(app.create_state, CreateState::Needed, "the edit registered…");
        dispatch_create(&mut app);
        assert!(drain_jobs(&mut app).is_empty(), "…but an empty page is not sent");

        type_str(&mut app, &ctx, "now it has something");
        leave_session(&mut app, &ctx);
        dispatch_create(&mut app);
        assert_eq!(drain_jobs(&mut app).len(), 1, "content makes it real");
    }

    fn shift(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    /// Undo and redo take the caret to what they changed — not back to
    /// wherever it happened to be, and not to the start of the line.
    #[test]
    fn undo_and_redo_land_the_caret_on_the_change() {
        assert_eq!(changed_end("x", "xabc"), 4, "typing: after what was typed");
        assert_eq!(changed_end("xabc", "x"), 1, "removing: where it was");
        assert_eq!(
            changed_end("hello world", "hello brave world"),
            "hello brave ".len(),
            "middle edit: after the inserted run (the shared space stays with the prefix)",
        );
        assert_eq!(changed_end("あい", "あうえい"), "あうえ".len(), "utf-8 safe");

        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!!");

        handle_session_key(&mut app, &ctx, ctrl('z'));
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.buf.as_str()), (1, "one"));
        assert_eq!(s.input.cur, 3, "undo: where the text was taken from");

        handle_session_key(&mut app, &ctx, ctrl('r'));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "one!!");
        assert_eq!(s.input.cur, 5, "redo: after what came back — NOT the line start");

        // From READ, the cursor follows the change too.
        leave_session(&mut app, &ctx);
        app.cursor = 0;
        handle_key(&mut app, &ctx, ctrl('z'));
        assert_eq!(app.cursor, 1, "the cursor went to the line that changed");
    }

    /// Shift+↑↓ grows a character range that crosses lines — the very
    /// one the mouse drags. ⌫ deletes it, lines merging the way a
    /// textarea's would, and `^z` brings it all back.
    #[test]
    fn shift_arrows_select_characters_across_lines_and_backspace_deletes_them() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three", "four"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);

        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3, "the caret came along");
        assert_eq!(s.sel_from, Some((1, 0)), "anchored where it started");
        assert!(app.selection.is_none(), "EDIT has no line band");
        assert!(app.status.contains("3 行を選択"), "status: {}", app.status);

        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(
            texts(&app),
            vec!["title", "three", "four"],
            "one and two are gone, absorbed into three's start",
        );
        let s = app.session.as_ref().expect("still editing");
        assert_eq!(s.line, 1, "the caret took the range's place");
        assert_eq!(s.input.buf, "three");
        assert_eq!(s.input.cur, 0);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "the merge is one commit");
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Replace { .. }, EditOp::Delete { .. }, EditOp::Delete { .. }]),
            "ops: {:?}",
            jobs[0].1
        );

        handle_session_key(&mut app, &ctx, ctrl('z'));
        assert_eq!(app.lines.len(), 5, "^z brings the lines back");
    }

    /// Enter over a range across lines cuts it out and keeps exactly one
    /// break: the anchor line its head, the far line its tail, the lines
    /// between gone — one cut, one commit, one undo step.
    #[test]
    fn enter_over_a_range_cuts_it_out_and_keeps_one_break() {
        let ctx = test_ctx();
        let mut app = page(&["t", "abcdef", "ghijkl", "mnopqr", "end"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3); // abc|def
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        // head on line 3, same sticky column: mno|pqr
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(texts(&app), vec!["t", "abc", "pqr", "end"]);
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.cur), (2, 0), "the caret opens the tail");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "one commit for the whole cut");
        assert!(
            matches!(&jobs[0].1[..], [EditOp::Replace { .. }, EditOp::Replace { .. }, EditOp::Delete { .. }]),
            "ops: {:?}",
            jobs[0].1
        );
    }

    /// Typing replaces a range (that is what "type over a selection"
    /// means), Esc drops it before it leaves, and a range that reaches
    /// the title merges INTO the title: its line always survives — EDIT
    /// has no gesture that deletes the page's first line.
    #[test]
    fn a_cross_line_selection_reacts_like_a_textareas() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "end"]);
        app.rebuild(40);

        // Typing swallows the range: the character lands at its start
        // and the lines merge.
        enter_session(&mut app, &ctx, 1, 0);
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        assert!(app.session.as_ref().unwrap().sel_ends().is_some());
        type_str(&mut app, &ctx, "x");
        assert!(
            app.session.as_ref().unwrap().sel_from.is_none(),
            "the range was spent"
        );
        assert_eq!(texts(&app), vec!["title", "xtwo", "end"], "typed over the range");

        // Esc drops the selection first; it never un-selects and leaves
        // in one press.
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        assert!(app.session.as_ref().unwrap().sel_ends().is_some());
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.session.as_ref().unwrap().sel_ends().is_none());
        assert!(app.session.is_some(), "one Esc, one job");

        // A range that reaches the title renames the title line — which
        // is what typing on it does too — but the line itself stays.
        enter_session(&mut app, &ctx, 0, 0);
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, key(KeyCode::Delete));
        assert_eq!(texts(&app), vec!["xtwo", "end"]);
        assert_eq!(app.lines[0].id, "id0", "still the title's own line");
    }

    /// Enter over a range replaces it with the break just typed: across
    /// lines, the anchor keeps its head and the far line its tail (the
    /// lines between are gone); on one line, the cut range splits at its
    /// own start — the junction the caret fell to.
    #[test]
    fn enter_replaces_the_range_with_one_break() {
        let ctx = test_ctx();
        let mut app = page(&["t", "abcdef", "ghijkl", "x"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(texts(&app), vec!["t", "abc", "jkl", "x"]);
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2, "the caret opens the far line's tail");
        assert_eq!(s.input.buf, "jkl");
        assert_eq!(s.input.cur, 0);

        // One line: "abc" selected by Shift+→, Enter cuts it and splits
        // at the junction its start left behind.
        let mut app = page(&["t", "abcdef"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        for _ in 0..3 {
            handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        }
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(texts(&app), vec!["t", "", "def"], "the range's text left the page");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, 0);
    }

    /// "Web edits are slow" has three different causes, and the footer
    /// should say which one is in play — including the one that is not
    /// slowness at all: updates arrived and are waiting for the caret line.
    #[test]
    fn the_footer_says_why_the_page_is_not_the_newest_state() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);

        app.sync_state = capability::SyncState::Live;
        assert_eq!(app.sync_notice(), None, "live and idle says nothing");

        app.sync_state = capability::SyncState::Polling;
        assert_eq!(app.sync_notice().as_deref(), Some("同期: poll"));
        assert!(app.hint_text(&[]).starts_with("同期: poll · "), "and it leads the footer");

        app.sync_state = capability::SyncState::Live;
        app.ws_pending.push_back(commit("c1", "p0", "pid", "someone", vec![]));
        let held = app.sync_notice().unwrap_or_default();
        assert!(held.contains('1'), "how many: {held}");
        assert!(held.contains("適用待ち"), "{held}");

        // With an unsaved caret line, that IS the reason — say so.
        enter_session(&mut app, &ctx, 1, 0);
        type_str(&mut app, &ctx, "!");
        let held = app.sync_notice().unwrap_or_default();
        assert!(held.contains("編集中の行"), "{held}");
    }

    /// A picture is measured in whole rows and capped in height: the first
    /// so a baseline lands level with its bottom edge, the second so one
    /// tall screenshot cannot take the whole screen.
    #[test]
    fn a_picture_is_whole_rows_and_never_taller_than_the_cap() {
        let picker = Picker::halfblocks();
        let font = picker.font_size();
        let build = |w: u32, h: u32, cols: u16| {
            let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(w, h));
            build_image(&picker, img, cols).unwrap()
        };

        // A picture whose pixels do not fill its last row keeps the rows it
        // FILLS — the leftover would read as a gap under the picture.
        let one_and_a_half = font.height as u32 + font.height as u32 / 2;
        let info = build(font.width as u32 * 4, one_and_a_half, 64);
        assert_eq!(info.cells_h, 1, "a row and a half is one row");

        // Tall pictures are scaled down, keeping their shape.
        let tall = build(font.width as u32 * 10, font.height as u32 * 100, 64);
        assert_eq!(tall.cells_h, MAX_IMAGE_ROWS);
        assert!(tall.cells_w < 10, "narrowed to match: {}", tall.cells_w);

        // Ordinary pictures are untouched by the cap.
        let normal = build(font.width as u32 * 8, font.height as u32 * 4, 64);
        assert_eq!((normal.cells_w, normal.cells_h), (8, 4));
    }

    /// Mixing text into an indented line must not cost it its bullet: it
    /// is still an item at that level, whatever it holds.
    #[test]
    fn an_indented_line_keeps_its_bullet_when_it_holds_a_picture() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let url = "https://example.com/a.png";
        let mut app = page(&["t", &format!(" [{url}]ワイワイ")]);
        app.images
            .insert(url.to_string(), decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap());
        app.rebuild(40);
        // The row knows it is an item, at its level's column.
        let row = app
            .content_view(40)
            .into_iter()
            .find_map(|r| match r {
                Row::Inline { item, indent, .. } => Some((item, indent)),
                _ => None,
            })
            .expect("an inline row");
        assert_eq!(row, (true, 2));

        // …and the bullet really is drawn.
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let has_bullet = (0..12).any(|y| {
            (0..40).any(|x| buf.cell((x, y)).map(|c| c.symbol() == BULLET).unwrap_or(false))
        });
        assert!(has_bullet, "the item lost its bullet");
    }

    /// A picture written beside text has to be FETCHED like any other.
    /// Collecting only whole-line pictures left the inline ones with a box
    /// reserved and nothing ever put in it.
    #[test]
    fn pictures_inside_a_line_are_fetched_too() {
        let ctx = test_ctx();
        let a = "https://example.com/a.png";
        let b = "https://example.com/b.png";
        let mut app = page(&["t", &format!("[{a}] と [{b}]"), &format!("[{a}]")]);
        app.rebuild(80);
        app.start_image_loads(&ctx);
        for url in [a, b] {
            assert!(app.pending.contains(url), "{url} was never asked for");
        }
    }

    /// The browser lays an inline image ON the text line: its bottom edge
    /// level with the words, so a sentence reads straight through it. The
    /// same layout answers all three shapes — text then picture, picture
    /// then text, and two pictures with words between them.
    #[test]
    fn a_line_of_text_and_pictures_is_laid_out_like_one_sentence() {
        let txt = |s: &str| Inline::Text(Line::from(s.to_string()));
        let img = |name: &str, w: u16, h: u16| Inline::Image { url: name.into(), w, h };

        // Picture first, then text: the words ride the picture\'s last row.
        let (imgs, texts, h) = layout_inline(&[img("a", 10, 6), txt("こんな感じ")], 0, 60);
        assert_eq!(h, 6, "the box is as tall as the picture");
        assert_eq!(imgs[0].row, 0);
        assert_eq!(texts[0].row, 5, "on the picture\'s last row — its baseline");
        assert_eq!(texts[0].col, 10, "right after it");

        // Text first, then picture: the picture grows UPWARD from the text
        // line, which is what an inline image does in a browser.
        let (imgs, texts, h) = layout_inline(&[txt("先に本文 "), img("a", 10, 4)], 0, 60);
        assert_eq!(h, 4);
        assert_eq!(texts[0].row, 3, "the text sits on the baseline");
        assert_eq!(imgs[0].row, 0, "and the picture reaches up from it");
        assert!(imgs[0].col >= 6, "after the words: {}", imgs[0].col);

        // Two pictures with text between them, side by side on one line.
        let (imgs, texts, h) =
            layout_inline(&[img("a", 10, 4), txt(" と "), img("b", 8, 6)], 0, 60);
        assert_eq!(imgs.len(), 2);
        assert_eq!(h, 6, "the tallest picture sets the height");
        assert_eq!(imgs[0].row, 2, "the shorter one is bottom-aligned with it");
        assert_eq!(imgs[1].row, 0);
        assert!(imgs[1].col > imgs[0].col, "in the order written");
        assert_eq!(texts[0].row, 5, "the words are on the shared baseline");

        // Out of room: the next picture starts a new box below.
        let (imgs, _, h) = layout_inline(&[img("a", 20, 3), img("b", 20, 3)], 0, 30);
        assert_eq!(imgs[0].row, 0);
        assert_eq!(imgs[1].row, 3, "wrapped under the first");
        assert_eq!(h, 6);

        // An indented line puts everything at its own column.
        let (imgs, texts, _) = layout_inline(&[img("a", 6, 2), txt("と本文")], 4, 40);
        assert_eq!(imgs[0].col, 4);
        assert_eq!(texts[0].col, 10);
    }

    /// An indented picture is a LIST ITEM: cosense web draws the bullet
    /// beside it, and without one the picture floats free of the item it
    /// belongs to.
    #[test]
    fn an_indented_picture_gets_its_bullet() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let url = "https://example.com/a.png";
        let mut app = page(&["t", &format!(" [{url}]"), "  after"]);
        // Pretend the picture has landed.
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap();
        app.images.insert(url.to_string(), info);
        app.rebuild(40);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let row_text = |y: u16| -> String {
            (0..40).map(|x| buf.cell((x, y)).unwrap().symbol()).collect()
        };
        // The picture is painted by the protocol; its bullet is ours.
        let bulleted: Vec<String> =
            (0..12).map(row_text).filter(|r| r.contains(BULLET)).collect();
        assert!(!bulleted.is_empty(), "a bullet is drawn for the picture");
        let col = bulleted[0].find(BULLET).unwrap();
        assert!(col >= 3, "at the item's own indent: {:?}", bulleted[0]);

        // A picture with text on the same line is not a picture ROW at all:
        // it is laid out inline with the words, so there is no bullet to
        // place — the sentence itself shows what it belongs to.
        let mut app = page(&["t", &format!(" [{url}]と本文が続く")]);
        app.images
            .insert(url.to_string(), decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap());
        app.rebuild(40);
        let rows = app.content_view(40);
        assert!(
            rows.iter().any(|r| matches!(r, Row::Inline { images, texts, .. }
                if images.len() == 1 && !texts.is_empty())),
            "one inline row carrying both",
        );
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Image { .. })),
            "and no bare picture row to hang a bullet off",
        );

        // The placeholder shown before it lands wears the same lead-in.
        assert_eq!(bullet_pad(2, true), format!("{BULLET} "));
        assert_eq!(bullet_pad(4, true), format!("  {BULLET} "));
        assert_eq!(bullet_pad(0, true), "", "a flush picture has no bullet");
        assert_eq!(bullet_pad(4, false), "    ", "a hanging picture has none either");
    }

    /// Everything in Cosense is written in brackets, so the closing half
    /// is the most repeated keystroke there is. `[` opens the pair, `]`
    /// steps over one that is already waiting, and ⌫ takes both back.
    #[test]
    fn typing_a_bracket_opens_a_pair() {
        let ctx = test_ctx();
        let mut app = page(&["title", ""]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);

        type_str(&mut app, &ctx, "[");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "[]");
        assert_eq!(s.input.cur, 1, "the caret waits between them");

        type_str(&mut app, &ctx, "改善案");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "[改善案]");

        // Typing the closing bracket steps over the one already there
        // instead of leaving `]]` behind.
        type_str(&mut app, &ctx, "]");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "[改善案]");
        assert_eq!(s.input.cur, s.input.buf.len(), "past the pair");

        // ⌫ inside an empty pair removes both halves.
        type_str(&mut app, &ctx, "[");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "[改善案][]");
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(app.session.as_ref().unwrap().input.buf, "[改善案]");
    }

    /// Inside a `code:` block notation is off by contract    /// Inside a `code:` block notation is off by contract — links, images
    /// and quotes all stop working there — so brackets are just characters
    /// someone typed on purpose. Completing them would be the one place
    /// the block leaks.
    #[test]
    fn brackets_are_not_completed_inside_a_code_block() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", " ", "after"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 1);
        assert!(session_in_code(&app));

        type_str(&mut app, &ctx, "a[0");
        assert_eq!(app.session.as_ref().unwrap().input.buf, " a[0", "no closing half");
        type_str(&mut app, &ctx, "]");
        assert_eq!(app.session.as_ref().unwrap().input.buf, " a[0]", "and `]` is a `]`");

        // ⌫ takes one character, not a pair it never made.
        type_str(&mut app, &ctx, "[");
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " a[0]");

        // Outside the block it completes as before.
        enter_session(&mut app, &ctx, 3, 5);
        type_str(&mut app, &ctx, "[");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "after[]");
    }

    /// Selecting a word and pressing `[` is how a link gets written.    /// Selecting a word and pressing `[` is how a link gets written.
    #[test]
    fn typing_a_bracket_over_a_selection_wraps_it() {
        let ctx = test_ctx();
        let mut app = page(&["title", "改善案 を見る"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        for _ in 0..3 {
            handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        }

        type_str(&mut app, &ctx, "[");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "[改善案] を見る");
        assert_eq!(s.input.cur, "[改善案]".len(), "the caret follows the new link");
        assert!(s.sel_span().is_none());
    }

    /// Backticks quote notation. A line that WRITES about `[a link]` must
    /// not BE one — a page documenting the syntax turned into a field of
    /// links to pages nobody meant to name, and Enter followed them.
    #[test]
    fn quoted_notation_is_not_a_link() {
        let line = "リンク付き画像 `[リンク先 画像URL]` → 画像を出し、Enter でリンク先へ";
        let mut app = page(&["t", line]);
        app.rebuild(80);
        app.goto_src(1);
        assert!(app.cursor_line_links().is_empty(), "{:?}", app.cursor_line_links());

        // A quoted URL is not a link either.
        let mut app = page(&["t", "`[https://example.com/a.png]` は画像になる"]);
        app.rebuild(80);
        app.goto_src(1);
        assert!(app.cursor_line_links().is_empty());

        // Outside the quotes, everything still works — including a real
        // link on the same line as a quoted one.
        let mut app = page(&["t", "`[quoted]` と [本物]"]);
        app.rebuild(80);
        app.goto_src(1);
        assert_eq!(app.cursor_line_links(), vec![LinkItem::Page("本物".into())]);

        // The masking keeps byte offsets, so mouse targets stay put.
        let masked = mask_inline_code("`[a]` [b]");
        assert_eq!(masked.len(), "`[a]` [b]".len());
        assert_eq!(&masked[6..], "[b]");
    }

    /// A linked image (`[href imageUrl]`) is ONE link: the picture is the
    /// label, the href is where Enter goes. Treating the two URLs as two
    /// links made the viewer ask which one you meant — and label each
    /// choice with the other one's address, so either pick looked wrong.
    #[test]
    fn a_linked_image_is_one_link_to_its_href() {
        let img = "https://example.com/photo.png";
        let href = "https://scrapbox.io/proj/Page";
        let mut app = page(&["t", &format!("[{href} {img}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1, "one link, not a question: {links:?}");
        match &links[0] {
            LinkItem::Url { label, url } => {
                assert_eq!(url, href, "Enter follows the href");
                assert!(label.contains("photo.png"), "labelled by the picture: {label}");
            }
            other => panic!("expected a url link, got {other:?}"),
        }

        // The order the two are written in does not change the answer.
        let mut app = page(&["t", &format!("[{img} {href}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1, "{links:?}");
        assert!(matches!(&links[0], LinkItem::Url { url, .. } if url == href));

        // A picture with no href is still just itself.
        let mut app = page(&["t", &format!("[{img}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], LinkItem::Url { url, .. } if url == img));
    }

    /// Which protocol draws the pictures decides whether they clip with
    /// the panes around them, so it is named in the status line and can be
    /// forced by anyone whose terminal answers badly.
    #[test]
    fn the_picture_protocol_is_named_and_can_be_forced() {
        use ratatui_image::picker::ProtocolType;
        let name = |p: ProtocolType| {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(p);
            image_protocol_name(&picker)
        };
        assert_eq!(name(ProtocolType::Kitty), "kitty");
        assert_eq!(name(ProtocolType::Iterm2), "iterm2");
        assert_eq!(name(ProtocolType::Sixel), "sixel");
        assert_eq!(name(ProtocolType::Halfblocks), "halfblocks");

        // `COSENSE_IMAGE` takes a protocol NAME; anything else means auto.
        let forced = |want: &str| -> Option<ProtocolType> {
            match want {
                "kitty" => Some(ProtocolType::Kitty),
                "iterm2" => Some(ProtocolType::Iterm2),
                "sixel" => Some(ProtocolType::Sixel),
                "halfblocks" => Some(ProtocolType::Halfblocks),
                _ => None,
            }
        };
        assert_eq!(forced("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(forced("halfblocks"), Some(ProtocolType::Halfblocks));
        assert_eq!(forced("auto"), None);
        assert_eq!(forced(""), None, "unset changes nothing");
    }

    /// A table is written the way cosense web writes one: `table:名前`,
    /// Enter for a row, Tab for the next cell. The session has to know it
    /// is standing in a table — otherwise Tab indents the row (pushing it
    /// out of the table) and Enter escapes the list.
    #[test]
    fn a_table_is_written_with_enter_and_tab() {
        let ctx = test_ctx();
        let mut app = page(&["title", ""]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        type_str(&mut app, &ctx, "table:表１");

        // Enter on the header opens the first row, indented into it.
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[1].text, "table:表１");
        assert_eq!(app.lines[2].text, " ", "the row belongs to the table");
        assert!(app.table_span_at_line(2).is_some());

        // Tab adds a cell instead of indenting the row out of the table.
        type_str(&mut app, &ctx, "グループ１");
        handle_session_key(&mut app, &ctx, key(KeyCode::Tab));
        type_str(&mut app, &ctx, "グループ２");
        assert_eq!(
            app.session.as_ref().unwrap().input.buf,
            " グループ１\tグループ２",
            "one TAB between the cells, the row still one level in",
        );

        // Enter starts the next row at the same indent (no list escape).
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ");
        assert!(app.table_span_at_line(3).is_some(), "still inside the table");

        // Shift+Tab takes a cell separator back; with none left it
        // outdents, which is how a row leaves the table.
        type_str(&mut app, &ctx, "５人");
        handle_session_key(&mut app, &ctx, key(KeyCode::Tab));
        handle_session_key(&mut app, &ctx, key(KeyCode::BackTab));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ５人");
        handle_session_key(&mut app, &ctx, key(KeyCode::BackTab));
        assert_eq!(app.session.as_ref().unwrap().input.buf, "５人", "out of the table");
    }

    /// A code block and a table are files: `Enter` saves them, written
    /// from the page in hand (Cosense's own endpoints answer 401 to the
    /// token this viewer holds — they want a session cookie).
    #[test]
    fn a_code_or_table_header_saves_the_block() {
        let mut app = page(&["t", "code:sample.py", " print(1)", "table:売上", " a\tb"]);
        app.project = "proj".into();
        app.title = "ページ".into();
        app.rebuild(80);

        app.goto_src(1);
        match app.cursor_line_links().first() {
            Some(LinkItem::Export { label, csv, .. }) => {
                assert_eq!(label, "sample.py");
                assert!(!csv);
            }
            other => panic!("expected the code file, got {other:?}"),
        }
        assert_eq!(
            block_export_body(&app.lines, 1, false),
            "print(1)\n",
            "the code as written, without the block's own indent",
        );

        app.goto_src(3);
        match app.cursor_line_links().first() {
            Some(LinkItem::Export { label, csv, .. }) => {
                assert_eq!(label, "売上.csv", "a table comes back as CSV");
                assert!(csv);
            }
            other => panic!("expected the table file, got {other:?}"),
        }
        assert_eq!(block_export_body(&app.lines, 3, true), "a,b\n");

        // Cells that need quoting get it, and nothing else does.
        assert_eq!(csv_row("a\tb"), "a,b");
        assert_eq!(csv_row("a,b\tc"), "\"a,b\",c");
        assert_eq!(csv_row("say \"hi\"\tx"), "\"say \"\"hi\"\"\",x");

        // Body lines are not links, and a nameless block offers nothing.
        app.goto_src(2);
        assert!(app.cursor_line_links().is_empty());
        assert!(block_export_link("code:", 0).is_none());
    }

    /// A table ends the way a list does: Enter on a row with nothing in it
    /// leaves. (A code block does not — a blank line there is blank code.)
    #[test]
    fn an_empty_row_plus_enter_leaves_the_table() {
        let ctx = test_ctx();
        let mut app = page(&["title", "table:表", " a\tb", "after"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, " a\tb".len());

        // First Enter: a new row, still in the table.
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ");
        assert!(app.table_span_at_line(3).is_some());

        // Second Enter on that empty row: out.
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "", "the new line starts flush");
        assert_eq!(app.lines[3].text, "", "and the empty row stopped being one");
        assert!(app.table_span_at_line(s.line).is_none(), "outside the table");

        // A code block keeps its blank lines instead.
        let mut app = page(&["title", "code:x.py", " a = 1"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, " a = 1".len());
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.session.as_ref().unwrap().input.buf, " ", "still inside the code");
        assert!(app.code_span_at_line(app.session.as_ref().unwrap().line).is_some());
    }

    /// A tab is invisible at width zero: the cells around it would run
    /// together and the caret would sit in a gap nobody can see.
    #[test]
    fn a_cell_separator_is_visible_while_editing() {
        let row = " a\tb";
        let span = cosense::render::CodeSpan { header: 0, header_indent: 0 };
        let disp = session_display(row, Some(span));
        assert!(disp.contains(TAB_MARK), "{disp:?}");
        assert_eq!(disp.chars().count(), row.chars().count() + 1, "one column each way");
        // …and the caret still maps through it.
        for byte in [1usize, 2, 3, 4] {
            let d = display_caret(row, byte, Some(span));
            assert_eq!(raw_caret_from_display(row, d, Some(span)), byte, "byte {byte}");
        }
    }

    /// Cosense has no official shortcut for heading levels — the community
    /// UserScript uses `Ctrl+8`, which a terminal delivers as Backspace —
    /// so one key cycles: bigger, bigger, bigger, then back to plain text.
    #[test]
    fn ctrl_t_cycles_the_heading_level() {
        let ctx = test_ctx();
        let mut app = page(&["title", "  見出し"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, "  見出し".len());
        let buf = |app: &App| app.session.as_ref().unwrap().input.buf.clone();

        handle_session_key(&mut app, &ctx, ctrl('t'));
        assert_eq!(buf(&app), "  [* 見出し]", "the indent is untouched");
        handle_session_key(&mut app, &ctx, ctrl('t'));
        assert_eq!(buf(&app), "  [** 見出し]");
        handle_session_key(&mut app, &ctx, ctrl('t'));
        handle_session_key(&mut app, &ctx, ctrl('t'));
        assert_eq!(buf(&app), "  [**** 見出し]", "four is the top");
        handle_session_key(&mut app, &ctx, ctrl('t'));
        assert_eq!(buf(&app), "  見出し", "and off the top it is plain again");

        // The caret keeps its place in the TEXT, not in the notation.
        let mut app = page(&["title", "abcdef"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3); // between c and d
        handle_session_key(&mut app, &ctx, ctrl('t'));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "[* abcdef]");
        assert_eq!(&s.input.buf[s.input.cur..s.input.cur + 1], "d", "still before d");

        // A heading with other notation inside keeps it.
        let mut app = page(&["title", "[改善案] を見る"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        handle_session_key(&mut app, &ctx, ctrl('t'));
        assert_eq!(buf(&app), "[* [改善案] を見る]");
    }

    /// Reading happens in ASCII — `j`/`k`/`e`/`q` are the language of the
    /// viewer — and typing Japanese belongs to the edit session, which
    /// turns the IME on by itself. A full-width character arriving in READ
    /// therefore means the input source drifted: put it back rather than
    /// only complaining about it.
    #[test]
    fn a_full_width_key_in_read_puts_the_input_source_back() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);

        let before: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        handle_key(&mut app, &ctx, key(KeyCode::Char('あ')));
        assert!(
            app.status.contains("英数") || app.status.contains("IME"),
            "status: {}",
            app.status,
        );
        assert_eq!(
            app.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>(),
            before,
            "and the key does nothing else",
        );

        // In EDIT the same key is just text.
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "あ");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "oneあ");
    }

    /// A trailing newline in a paste is how the text was COPIED, not a
    /// line the writer asked for. Keeping it dropped a blank line under
    /// every pasted line — and carried the caret onto it, which reads as
    /// the caret jumping one line too far.
    #[test]
    fn a_pasted_trailing_newline_does_not_leave_a_blank_line() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);

        handle_paste(&mut app, &ctx, "tail\n");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "onetail", "pasted into the line, no new line");
        assert_eq!(s.line, 1, "and the caret stays on it");
        assert_eq!(app.lines.len(), 2, "nothing structural happened");

        // Newlines INSIDE the paste still make lines — and the trailing one
        // still does not.
        handle_paste(&mut app, &ctx, "a\nb\n");
        assert_eq!(
            app.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["title", "onetaila", "b"],
        );
        assert_eq!(app.session.as_ref().unwrap().line, 2, "on the last pasted line");

        // A paste of nothing but a newline does nothing at all.
        let before: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        handle_paste(&mut app, &ctx, "\n");
        assert_eq!(app.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>(), before);
    }

    /// A pasted URL usually wants brackets: that is what makes an image a
    /// picture, and a selection a labelled link. Anything else is pasted
    /// exactly as it came.
    #[test]
    fn a_pasted_url_gets_the_brackets_it_needs() {
        let ctx = test_ctx();
        let mut app = page(&["title", "see "]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 4);

        // An image URL draws only in brackets — so put it in brackets.
        handle_paste(&mut app, &ctx, "https://example.com/a.png");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "see [https://example.com/a.png]");

        // A page URL is a link either way: paste it as it came.
        let mut app = page(&["title", ""]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        handle_paste(&mut app, &ctx, "https://example.com/page.html");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "https://example.com/page.html");

        // Over a selection, the selected text becomes the link's label.
        let mut app = page(&["title", "cosense"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        for _ in 0..7 {
            handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        }
        handle_paste(&mut app, &ctx, "https://scrapbox.io/");
        assert_eq!(
            app.session.as_ref().unwrap().input.buf,
            "[cosense https://scrapbox.io/]",
        );

        // Ordinary text is never reshaped.
        handle_paste(&mut app, &ctx, " and more");
        assert!(app.session.as_ref().unwrap().input.buf.ends_with(" and more"));
    }

    /// Paste is the TERMINAL's paste (bracketed paste), so the only
    /// question is where it lands. Everywhere text is being typed — and a
    /// word about it where none is.
    #[test]
    fn paste_lands_where_text_is_being_typed() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);

        // READ: nowhere to put it, so say where it goes.
        handle_paste(&mut app, &ctx, "hello");
        assert!(app.status.contains("編集中"), "status: {}", app.status);
        assert_eq!(app.lines.len(), 2, "and nothing is written");

        // The index filters by what you paste (first line only).
        app.index = Some(cosense::index::Index::new(Vec::new(), 0));
        app.index.as_mut().unwrap().cursor = 3;
        handle_paste(&mut app, &ctx, "Some Page\nsecond line");
        let ix = app.index.as_ref().expect("the index is still open");
        assert_eq!(ix.filter, "Some Page", "a filter is one line");
        assert_eq!(ix.cursor, 0, "and the list starts from the top again");

        // EDIT: real lines, as before.
        app.index = None;
        enter_session(&mut app, &ctx, 1, 3);
        handle_paste(&mut app, &ctx, "X\r\nY");
        assert_eq!(app.lines[1].text, "oneX");
        assert_eq!(app.lines[2].text, "Y", "CRLF is normalised on the way in");
    }

    /// Selecting text with the mouse inside EDIT is character-unit even
    /// across lines: the caret — the range's moving end — follows the
    /// pointer, the anchor stays where the button came down. A
    /// double-click takes a word, a triple-click the line. There is no
    /// line band in EDIT at all — that is READ's unit.
    #[test]
    fn the_mouse_in_edit_selects_characters_across_lines() {
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world", "second line", "third"]);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 2, 40, 8);
        app.bar_rect = Rect::new(41, 1, 1, 10);
        app.view_h = 10;
        enter_session(&mut app, &ctx, 1, 0);

        // Drag across "hello" on its own line → characters.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), 1, 3));
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 6, 3));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.sel_span(), Some((0, 5)));
        assert!(app.selection.is_none(), "one selection at a time");
        assert_eq!(copy_payload(&app, false).unwrap().0, "hello");

        // A click that does not drag selects nothing: the caret just moves.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), 8, 3));
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Up(MouseButton::Left), 8, 3));
        assert!(app.session.as_ref().unwrap().sel_span().is_none());
        assert_eq!(copy_payload(&app, false).unwrap().0, "hello world", "the line, as before");

        // Dragging onto other lines keeps selecting CHARACTERS: the range
        // crosses the line break, the caret rides the pointer, and the
        // copy carries the very text the range holds.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), 3, 3));
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 3, 5));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3, "the caret followed the pointer");
        assert_eq!(s.sel_from, Some((1, 2)), "the anchor stayed at the press");
        assert!(app.selection.is_none(), "EDIT never selects a line band with the mouse");
        assert_eq!(
            copy_payload(&app, false).unwrap().0,
            "llo world\nsecond line\nth",
            "tail, whole lines, head — what the range really covers"
        );
        // …back onto the anchor line and it is a plain one-line range
        // again, exactly where the button went down.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 5, 3));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1);
        assert_eq!(
            s.sel_span().map(|(a, b)| s.input.buf[a..b].to_string()),
            Some("ll".into())
        );
    }

    /// A drag in EDIT is characters, across lines: the caret follows the
    /// pointer, the anchor holds, and drifting back onto the anchor line
    /// collapses the range right back into the plain one-line selection.
    /// NOTHING may panic in between. (The caret click_caret answers used
    /// to be written into whatever buffer the session happened to stand
    /// on: mid-character on CJK, the next draw died.)
    #[test]
    fn a_drag_that_wobbles_across_lines_stays_characters() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world", "あいx", "after"]);
        app.rebuild(42);
        let set_screen = |app: &mut App| {
            app.text_rect = Rect::new(1, 2, 40, 8);
            app.bar_rect = Rect::new(41, 1, 1, 10);
            app.view_h = 10;
        };
        set_screen(&mut app);
        enter_session(&mut app, &ctx, 1, 0);
        // A draw each time, like the real loop: this is where the crash
        // used to surface. `ui` overwrites the hit-test geometry, so the
        // fixed screen the clicks assume goes back after every frame.
        let mut term = Terminal::new(TestBackend::new(42, 12)).unwrap();
        let mut draw = |app: &mut App| {
            term.draw(|f| ui(f, app, &ctx)).unwrap();
            set_screen(app);
        };

        // Press at column 7 of "hello world" (caret byte 7, on the "o")
        // and drag one line down: the caret follows onto "あいx" (its
        // end — the click's column is past the line), the anchor stays.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), 8, 3));
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 8, 4));
        draw(&mut app);
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2, "the caret follows the pointer");
        assert_eq!(s.sel_from, Some((1, 7)), "the anchor stayed put");
        assert!(app.selection.is_none(), "no line band to escape from");
        assert_eq!(
            copy_payload(&app, false).unwrap().0,
            "orld\n\u{3042}\u{3044}x",
            "the range's text, across the line break"
        );

        // Drag back onto the anchor line: column-accurate characters
        // again. Byte 5 is where the pre-fix code sliced "あいx"
        // mid-'い' and died on this draw.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 6, 3));
        draw(&mut app);
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1, "the caret came back with the pointer");
        assert_eq!(
            s.sel_span().map(|(a, b)| s.input.buf[a..b].to_string()),
            Some(" w".into()),
            "the character range tracks the pointer again"
        );

        // And one line above the anchor: the range crosses upward —
        // title's tail, then the anchor line's head.
        handle_mouse_content(&mut app, &ctx, mouse(MouseEventKind::Drag(MouseButton::Left), 3, 2));
        draw(&mut app);
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 0);
        assert_eq!(s.sel_from, Some((1, 7)));
        assert_eq!(
            copy_payload(&app, false).unwrap().0,
            "tle\nhello w",
            "upward too: the range's text, not a line band"
        );
    }

    /// Double-click takes the word, triple-click the whole line — inside
    /// EDIT, where a selection is a character range on the caret line.
    /// The count restarts after the third click.
    #[test]
    fn clicks_in_a_session_take_a_word_then_the_line() {
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world こんにちは", "next"]);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 2, 40, 8);
        app.bar_rect = Rect::new(41, 1, 1, 10);
        app.view_h = 10;
        enter_session(&mut app, &ctx, 1, 0);
        let click = |app: &mut App, col: u16| {
            handle_mouse_content(app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), col, 3));
            handle_mouse_content(app, &ctx, mouse(MouseEventKind::Up(MouseButton::Left), col, 3));
        };
        let selected = |app: &App| {
            let s = app.session.as_ref().unwrap();
            s.sel_span().map(|(a, b)| s.input.buf[a..b].to_string())
        };

        // First press on the "w" of "world": the caret only.
        click(&mut app, 7);
        assert_eq!(selected(&app), None, "a single click selects nothing");
        // Same spot again within the window: the word.
        click(&mut app, 7);
        assert_eq!(selected(&app).as_deref(), Some("world"));
        assert_eq!(copy_payload(&app, false).unwrap().0, "world");
        // A third: the whole line. A fourth starts the count over.
        click(&mut app, 7);
        assert_eq!(
            selected(&app).as_deref(),
            Some("hello world こんにちは"),
            "the line as one character range"
        );
        click(&mut app, 7);
        assert_eq!(selected(&app), None, "and plain again");
    }

    /// The double-click that turns READ into EDIT does NOT select: it
    /// parks the caret where the pointer was and nothing more. Selecting
    /// is a gesture for a line you are already editing — the third click
    /// of the series lands in the open session, where it takes the line.
    #[test]
    fn a_read_double_click_only_places_the_caret() {
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world", "next"]);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 2, 40, 8);
        app.bar_rect = Rect::new(41, 1, 1, 10);
        app.view_h = 10;
        let click = |app: &mut App, col: u16| {
            handle_mouse_content(app, &ctx, mouse(MouseEventKind::Down(MouseButton::Left), col, 3));
            handle_mouse_content(app, &ctx, mouse(MouseEventKind::Up(MouseButton::Left), col, 3));
        };

        click(&mut app, 7);
        assert!(app.session.is_none(), "a single click only moves the cursor");
        assert_eq!(app.cursor, 1);
        click(&mut app, 7);
        let s = app.session.as_ref().unwrap();
        assert!(s.sel_from.is_none(), "the transition selects nothing");
        assert_eq!(s.input.cur, 6, "the caret sits on the clicked character");
        assert_eq!(s.line, 1, "and on the clicked line");
        // The same spot again: now inside EDIT, the gesture is a triple
        // click, and it takes the line.
        click(&mut app, 7);
        let s = app.session.as_ref().unwrap();
        assert_eq!(
            s.sel_span().map(|(a, b)| s.input.buf[a..b].to_string()),
            Some("hello world".into()),
            "the third click selects the whole line"
        );
    }

    #[test]
    fn a_word_is_letters_and_digits_across_scripts() {
        assert_eq!(word_span("hello world", 0), Some((0, 5)));
        assert_eq!(word_span("hello world", 5), Some((0, 5)), "right edge still takes it");
        assert_eq!(word_span("hello world", 6), Some((6, 11)));
        assert_eq!(word_span("hello  world", 6), None, "inside the gap: nothing");
        assert_eq!(word_span("abc こんにちは", 4), Some((4, 19)), "a CJK run is a word");
        assert_eq!(word_span("#tag ok", 0), Some((0, 4)), "a hashtag goes together");
        assert_eq!(word_span("a-b", 2), Some((2, 3)), "the dash breaks the run");
        assert_eq!(word_span("...", 1), None, "punctuation alone");
        assert_eq!(word_span("", 0), None);
        // A stray mid-character caret is floored, not fatal.
        assert_eq!(word_span("あいx", 5), Some((0, 7)), "one run of letters: all of it");
        assert_eq!(display_caret("あいx", 5, None), 3, "and the caret maps the same way");
    }

    /// Shift+↑↓ selects in READ too. `v` then j/k is the akapen way and
    /// still works; this is the one people try first, and it has to reach
    /// the same selection — the one `y` and `c` act on.
    #[test]
    fn shift_arrows_select_lines_in_read_too() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.rebuild(40);
        app.cursor = 1;

        handle_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_key(&mut app, &ctx, shift(KeyCode::Down));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 3)));
        assert_eq!(app.cursor, 3, "the cursor carries the far end");
        assert!(app.status.contains("3 行を選択"), "status: {}", app.status);

        // It is the same selection the rest of READ acts on.
        assert_eq!(copy_payload(&app, false).unwrap().0, "one\ntwo\nthree");

        // Shrinking works the same way, and Esc clears it.
        handle_key(&mut app, &ctx, shift(KeyCode::Up));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)));
        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.selection.is_none());

        // A plain arrow moves without selecting.
        handle_key(&mut app, &ctx, key(KeyCode::Down));
        assert!(app.selection.is_none());
    }

    /// ↑/↓ mean "the row above / the row below" — the movement the eye
    /// makes. On a wrapped line that is another row of the SAME line;
    /// jumping the whole block to the next source line loses the reader's
    /// place in the middle of a long paragraph.
    #[test]
    fn arrows_walk_display_rows_inside_a_wrapped_line() {
        let ctx = test_ctx();
        let long = "aaaaaaaaaa bbbbbbbbbb cccccccccc"; // 32 columns
        let mut app = page(&["title", long, "after"]);
        app.rebuild(26); // text area 20 wide → two rows
        let width = App::text_width(app.mode, app.laid_width);
        assert_eq!(width, 20);
        // The geometry a drawn frame would have recorded (an edit marks the
        // layout stale, so this is what the wrap width comes from).
        app.text_rect = Rect::new(1, 2, width as u16, 8);
        enter_session(&mut app, &ctx, 1, 3);

        // Down inside the line: same line, one row further in.
        handle_session_key(&mut app, &ctx, key(KeyCode::Down));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1, "still the same source line");
        assert_eq!(s.input.cur, width + 3, "the row below, same column");

        // Down again: now it leaves, landing on the first row of the next.
        handle_session_key(&mut app, &ctx, key(KeyCode::Down));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, "after".len().min(3), "the sticky column, clamped");

        // Up from there lands on the LAST row of the wrapped line, not its
        // first — the row that is visually just above.
        handle_session_key(&mut app, &ctx, key(KeyCode::Up));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1);
        assert!(s.input.cur >= width, "landed on the last row: {}", s.input.cur);

        // And up again walks back inside the line.
        handle_session_key(&mut app, &ctx, key(KeyCode::Up));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 1);
        assert!(s.input.cur < width, "the first row: {}", s.input.cur);
    }

    /// A wrapped line owns several display rows. Clicking the second one
    /// has to land in the second half of the TEXT — with only the column
    /// to go on, every continuation row mapped onto the first, so the
    /// caret jumped and a drag selected the wrong run.
    #[test]
    fn clicking_a_wrapped_row_lands_where_the_eye_is() {
        let ctx = test_ctx();
        // 30 columns of text area → "aaaa…" wraps into three rows.
        let long = "aaaaaaaaaa bbbbbbbbbb cccccccccc"; // 32 columns
        let mut app = page(&["title", long]);
        app.rebuild(26); // text area 20 columns wide → two rows
        app.text_rect = Rect::new(1, 2, 20, 8);
        app.bar_rect = Rect::new(24, 1, 1, 10);
        app.view_h = 10;
        let (first, last) = app.src_rows(1).expect("the wrapped line");
        assert!(last > first, "the line really does wrap");

        // Column 2 of the FIRST row is near the start…
        let head = click_caret(&app, 1, 2, first as i32);
        assert!(head < 5, "got {head}");
        // …and column 2 of the SECOND row is a full row further in.
        let next = click_caret(&app, 1, 2, first as i32 + 1);
        let row_w = App::text_width(app.mode, app.laid_width);
        assert_eq!(head, 2, "first row: the column IS the offset");
        assert_eq!(next, row_w + 2, "second row: one row further into the text");

        // And a drag between them selects exactly that run.
        enter_session(&mut app, &ctx, 1, 0);
        handle_mouse_content(
            &mut app,
            &ctx,
            mouse(MouseEventKind::Down(MouseButton::Left), 1 + 2, 2 + first as u16),
        );
        handle_mouse_content(
            &mut app,
            &ctx,
            mouse(MouseEventKind::Drag(MouseButton::Left), 1 + 2, 2 + first as u16 + 1),
        );
        let (a, b) = app.session.as_ref().unwrap().sel_span().expect("a span");
        assert_eq!((a, b), (head, next), "the run between the two clicks");
        assert_eq!(&long[a..b], &long[2..row_w + 2], "…which is what the eye dragged over");
    }

    /// A selection you cannot see is not a selection. The caret line is
    /// painted with the cursor band, so the highlight has to stand out
    /// against it — which a same-grey background did not.
    #[test]
    fn a_character_selection_is_visible_on_the_caret_line() {
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        for _ in 0..5 {
            handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        }

        let rows = app.content_view(40);
        let line = rows
            .iter()
            .find_map(|r| match r {
                Row::Line { line, src, .. } if *src == 1 => Some(line.clone()),
                _ => None,
            })
            .expect("the caret row");
        let picked: String = line
            .spans
            .iter()
            .filter(|sp| sp.style.add_modifier.contains(Modifier::REVERSED))
            .map(|sp| sp.content.as_ref())
            .collect();
        assert_eq!(picked, "hello", "the selected run is the one that stands out");
        let rest: String = line
            .spans
            .iter()
            .filter(|sp| !sp.style.add_modifier.contains(Modifier::REVERSED))
            .map(|sp| sp.content.as_ref())
            .collect();
        assert_eq!(rest, " world");
        assert_ne!(SEL_BG, Color::Reset);
    }

    /// A character selection behaves like a selection everywhere else:
    /// Shift+arrows grow it, typing replaces it, ⌫ removes it.
    #[test]
    fn shift_arrows_type_and_backspace_act_on_a_character_selection() {
        let ctx = test_ctx();
        let mut app = page(&["title", "abcdef"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);

        for _ in 0..3 {
            handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        }
        assert_eq!(app.session.as_ref().unwrap().sel_span(), Some((0, 3)));

        // Typing replaces the selection.
        type_str(&mut app, &ctx, "X");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "Xdef");
        assert_eq!(s.input.cur, 1);
        assert!(s.sel_span().is_none());

        // ⌫ removes a selection instead of one character.
        handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Right));
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        assert_eq!(app.session.as_ref().unwrap().input.buf, "Xf");
        assert_eq!(app.lines[1].text, "abcdef", "still uncommitted — it is just typing");

        // Moving without shift drops it.
        handle_session_key(&mut app, &ctx, shift(KeyCode::Left));
        assert!(app.session.as_ref().unwrap().sel_span().is_some());
        handle_session_key(&mut app, &ctx, key(KeyCode::Left));
        assert!(app.session.as_ref().unwrap().sel_span().is_none());
    }

    /// A note app has to be able to hand its text to something else. What
    /// leaves is the Cosense SOURCE — indentation and notation intact —
    /// because that is what survives the round trip back into a page.
    #[test]
    fn y_copies_the_line_the_selection_or_the_page_as_source() {
        let ctx = test_ctx();
        let mut app = page(&["title", " [link] one", "  two", "three"]);
        app.rebuild(40);

        app.cursor = 1;
        let (text, label) = copy_payload(&app, false).unwrap();
        assert_eq!(text, " [link] one", "the source, not the rendering");
        assert_eq!(label, "line");

        app.selection = Some(Selection { anchor: 1, cursor: 2 });
        let (text, label) = copy_payload(&app, false).unwrap();
        assert_eq!(text, " [link] one\n  two", "indentation carries the structure");
        assert_eq!(label, "2 lines");

        let (text, label) = copy_payload(&app, true).unwrap();
        assert_eq!(text, "title\n [link] one\n  two\nthree", "Y takes the title too");
        assert_eq!(label, "page (4 lines)");

        // In EDIT the caret line copies what is ON SCREEN, not the last
        // text the server heard.
        app.selection = None;
        enter_session(&mut app, &ctx, 3, 5);
        type_str(&mut app, &ctx, "!!");
        let (text, _) = copy_payload(&app, false).unwrap();
        assert_eq!(text, "three!!", "the working line, uncommitted and all");
    }

    /// OSC 52 is the only clipboard that reaches the machine the user is
    /// sitting at when the viewer runs over SSH, so its encoding has to be
    /// right.
    #[test]
    fn base64_for_osc52_matches_the_standard() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_encode("あ".as_bytes()), "44GC");
        // Round-trips through the decoder the browser side already uses.
        let round = cosense::chrome::b64_decode(&b64_encode("行 の コピー".as_bytes())).unwrap();
        assert_eq!(String::from_utf8(round).unwrap(), "行 の コピー");
    }

    /// The bug that made a new page look saved and arrive empty: Cosense
    /// hands out an id for a page it has NOT made, and committing against
    /// that id writes into nothing. Only `persistent` says a page is real.
    #[test]
    fn a_provisional_id_is_not_a_page_id() {
        let mut page = polled(&[("prov0", "a title")]).page;
        page.id = "6a92b6a60000000000000c05".into(); // the API really does send one
        page.persistent = false;
        assert_eq!(live_page_id(&page), "", "nothing to edit against yet");

        page.persistent = true;
        assert_eq!(live_page_id(&page), "6a92b6a60000000000000c05");

        // And a non-persistent page is never adopted, however real its id
        // looks: adopting it would resume committing into nothing.
        let ctx = test_ctx();
        let mut app = page_uncreated(&["a title", "typed"]);
        let mut ghost = polled(&[("prov0", "a title")]).page;
        ghost.id = "6a92b6a60000000000000c05".into();
        ghost.persistent = false;
        adopt_created_page(&mut app, &ctx, &ghost);
        assert_eq!(app.page_id, "", "still uncreated");
        assert_eq!(app.lines[1].text, "typed", "and the local text is untouched");
    }

    /// A page nobody has written yet: Cosense answers 200 for any title,
    /// so the viewer opens a template. Editing it must CREATE the page,
    /// which is a different request (no pageId) — and it must go out
    /// exactly once, no matter how much is typed first.
    #[test]
    fn an_uncreated_page_is_created_by_the_first_edit() {
        let ctx = test_ctx();
        let mut app = page(&["new title"]);
        app.title = "new title".into();
        app.page_id = String::new(); // the page does not exist yet
        app.rebuild(40);

        enter_session(&mut app, &ctx, 0, "new title".len());
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        type_str(&mut app, &ctx, "body");
        leave_session(&mut app, &ctx);
        assert!(drain_jobs(&mut app).is_empty(), "nothing can be committed yet");
        assert_eq!(app.create_state, CreateState::Needed, "…but the page owes the server its existence");

        dispatch_create(&mut app);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "one create, whatever was typed");
        assert_eq!(jobs[0].0, "ページの作成");
        match &jobs[0].1[0] {
            EditOp::Insert { anchor, lines } => {
                assert_eq!(anchor, "_end");
                let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
                assert_eq!(texts, vec!["new title", "body"], "title first, then the body");
            }
            other => panic!("expected an insert, got {other:?}"),
        }

        // A SECOND create would make a second page (same title, auto-
        // suffixed), and the writing would split between them.
        assert_eq!(app.create_state, CreateState::Sent);
        dispatch_create(&mut app);
        assert!(drain_jobs(&mut app).is_empty(), "not while one is in flight");

        // Even after it lands, further typing waits for the page to come
        // back with an id instead of creating a second one.
        handle_commit_outcome(&mut app, &ctx, CommitOutcome::Done {
            job: UNRELATED_JOB,
            label: "ページの作成".into(),
            title: "new title".into(),
            commit_id: String::new(),
        });
        assert_eq!(app.inflight, 0);
        enter_session(&mut app, &ctx, 1, 4);
        type_str(&mut app, &ctx, " more");
        leave_session(&mut app, &ctx);
        dispatch_create(&mut app);
        assert!(drain_jobs(&mut app).is_empty(), "one page, one create");
        assert_eq!(app.create_state, CreateState::Sent);

        // The page comes back: its id is adopted and the extra typing goes
        // up as an ordinary edit.
        let mut server = polled(&[("id0", "new title"), ("id1", "body")]).page;
        server.id = "PID".into();
        adopt_created_page(&mut app, &ctx, &server);
        assert_eq!(app.create_state, CreateState::Idle);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "the typing that the create missed");
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { text, .. } if text == "body more"));
    }

    /// A title with no body is not a page — not even a title that was
    /// typed over. Only content brings a page into being.
    #[test]
    fn a_title_alone_never_creates_the_page() {
        let ctx = test_ctx();
        let mut app = page(&["Untitled"]);
        app.title = "Untitled".into();
        app.page_id = String::new();
        app.rebuild(40);

        enter_session(&mut app, &ctx, 0, "Untitled".len());
        type_str(&mut app, &ctx, " for real"); // renaming, still no body
        leave_session(&mut app, &ctx);
        dispatch_create(&mut app);
        assert!(drain_jobs(&mut app).is_empty(), "a title is not content");

        // The moment there is a body, the page is real — with that title.
        app.cursor = 0;
        open_line(&mut app, &ctx, false);
        type_str(&mut app, &ctx, "body");
        leave_session(&mut app, &ctx);
        dispatch_create(&mut app);
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        match &jobs[0].1[0] {
            EditOp::Insert { lines, .. } => {
                let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
                assert_eq!(texts, vec!["Untitled for real", "body"]);
            }
            other => panic!("expected an insert, got {other:?}"),
        }
    }

    /// Quitting straight after writing a new page must still create it:
    /// nothing dispatches the create once the run loop is gone.
    #[test]
    fn quitting_dispatches_a_pending_create() {
        let ctx = test_ctx();
        let mut app = page(&["new title"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);

        app.cursor = 0;
        open_line(&mut app, &ctx, false);
        type_str(&mut app, &ctx, "written and quit");
        // What `flush_commits` does before draining the queue.
        leave_session(&mut app, &ctx);
        dispatch_create(&mut app);

        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1, "the page goes up before the viewer exits");
        match &jobs[0].1[0] {
            EditOp::Insert { lines, .. } => {
                let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
                assert_eq!(texts, vec!["new title", "written and quit"]);
            }
            other => panic!("expected an insert, got {other:?}"),
        }
    }

    /// A create that failed leaves the page uncreated: the next edit must
    /// be able to try again, without retrying in a loop meanwhile.
    #[test]
    fn a_failed_create_can_be_retried_by_the_next_edit() {
        let ctx = test_ctx();
        let mut app = page(&["new title", "body"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);
        do_edit(&mut app, &ctx, "edit", vec![EditOp::Replace { id: "id1".into(), text: "body!".into() }]);
        dispatch_create(&mut app);
        drain_jobs(&mut app);

        handle_commit_outcome(&mut app, &ctx, CommitOutcome::Failed {
            job: UNRELATED_JOB,
            label: "ページの作成".into(),
            msg: "500".into(),
        });
        assert_eq!(app.create_state, CreateState::Idle, "not stuck as sent");
        dispatch_create(&mut app);
        assert!(drain_jobs(&mut app).is_empty(), "and not retrying by itself");

        do_edit(&mut app, &ctx, "edit", vec![EditOp::Replace { id: "id1".into(), text: "body!!".into() }]);
        dispatch_create(&mut app);
        assert_eq!(drain_jobs(&mut app).len(), 1, "the next edit tries again");
    }

    /// Once the page exists, the viewer adopts its id and commits whatever
    /// was typed after the create left — those keystrokes were never in it.
    #[test]
    fn adopting_a_created_page_commits_what_the_create_missed() {
        let ctx = test_ctx();
        let mut app = page(&["new title", "body"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);
        // Typed while the create was in flight.
        app.lines[1].text = "body, and more".into();

        let mut server = polled(&[("id0", "new title"), ("id1", "body")]).page;
        server.id = "PID".into();
        adopt_created_page(&mut app, &ctx, &server);

        assert_eq!(app.page_id, "PID", "later commits have something to name");
        assert_eq!(app.lines[1].text, "body, and more", "the local text wins");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { id, .. } if id == "id1"));
    }

    /// While the page does not exist, every poll returns the same empty
    /// template. Installing it would wipe the page being typed.
    #[test]
    fn polls_never_overwrite_a_page_being_typed_into_existence() {
        let ctx = test_ctx();
        let mut app = page(&["new title", "body"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);

        let mut template = polled(&[("", "new title")]);
        template.page.id = String::new();
        template.page.persistent = false;
        apply_remote(&mut app, &ctx, template);
        assert_eq!(app.lines.len(), 2, "the template did not replace anything");
        assert_eq!(app.lines[1].text, "body");
    }

    /// Typing a code block has to be possible at all: Enter on the
    /// `code:` header opens the block's first body line, indented one step
    /// deeper — which is what makes the renderer read it as code.
    #[test]
    fn enter_on_a_code_header_opens_the_block_body() {
        let ctx = test_ctx();
        let mut app = page(&["title", ""]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        type_str(&mut app, &ctx, "code:x.py");

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[1].text, "code:x.py", "the header committed with the split");
        assert_eq!(app.lines[2].text, " ", "the new line is body-indented");
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.cur), (2, 1), "caret sits after the indent");

        // …and typing there really is inside the block now.
        type_str(&mut app, &ctx, "print(1)");
        assert!(app.line_in_code(2), "the renderer would read this as code");
        assert!(!app.line_in_code(0));
    }

    /// A blank line inside a block is blank CODE. The list-escape rule
    /// (empty bullet + Enter → flush line) must not fire there, or two
    /// Enters silently drop you out of the block.
    #[test]
    fn enter_on_a_blank_code_line_stays_in_the_block() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", " a = 1", " b = 2"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 6); // end of " a = 1"

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[3].text, " ", "the split inherits the code indent");
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[3].text, " ", "the blank code line keeps its indent");
        assert_eq!(app.lines[4].text, " ", "and so does the next one");
        assert!(app.line_in_code(4), "still inside the block");
        assert_eq!(app.session.as_ref().unwrap().line, 4);
    }

    /// One blank line is blank code, but a second blank in a row is the
    /// writer asking out: Enter there dissolves both blanks into true
    /// empty lines and continues flush below the block — the table's
    /// escape, delayed one line for the blank code's sake.
    #[test]
    fn a_second_blank_enter_leaves_the_code_block() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", " a = 1"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 6); // end of " a = 1"

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[3].text, " ", "one blank stays code");
        assert_eq!(app.lines[4].text, " ", "so does a second");
        assert!(app.line_in_code(4));

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.buf.as_str()), (4, ""), "the caret stays put, flush");
        assert_eq!(app.lines[3].text, "", "both blanks dissolved — no trailing blank code");
        assert_eq!(app.lines[4].text, "");
        assert!(!app.line_in_code(4), "and the block is behind us");
    }

    /// Code has depths of its own past the block's base indent. Enter at
    /// the end of `   x = 1` continues at those three spaces — resetting
    /// to the base flattened every deeper line (改善案3).
    #[test]
    fn enter_in_code_keeps_the_line_s_own_depth() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:y.py", " def f():", "   x = 1"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 3, 8); // end of "   x = 1"

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[4].text, "   ", "the new line starts at the same depth");
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.cur), (4, 3), "caret after the inherited indent");
        assert!(app.line_in_code(4), "still inside the block");
    }

    /// Inside a code block the leading whitespace is content: it must not
    /// be drawn as an outline bullet, and the caret must not step over one
    /// that is not there.
    #[test]
    fn a_code_line_shows_no_bullet_while_editing() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", "  indented", "  after"]);
        app.rebuild(40);

        enter_session(&mut app, &ctx, 2, 2);
        let rows = app.content_view(40);
        let shown: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src, .. } if *src == 2 => Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()),
                _ => None,
            })
            .collect();
        // What the renderer draws for that very line, with no session open.
        let mut read = page(&["title", "code:x.py", "  indented", "  after"]);
        read.rebuild(40);
        let rendered: Vec<String> = read
            .content_view(40)
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src, .. } if *src == 2 => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert_eq!(shown, rendered, "the caret line sits exactly where the code sits");
        assert_eq!(shown, vec!["   indented".to_string()], "code gutter, no bullet");
        let span = app.code_span_at_line(2).expect("in code");
        assert_eq!(
            display_caret("  indented", 2, Some(span)),
            "   ".len(),
            "the caret lands on the text, in the column the code is drawn in",
        );
        assert_eq!(
            raw_caret_from_display("  indented", "   ".len(), Some(span)),
            2,
            "and a click there comes back to the same source offset",
        );

        // The same line OUTSIDE a block still gets its bullet.
        let mut plain = page(&["title", "  indented"]);
        plain.rebuild(40);
        enter_session(&mut plain, &ctx, 1, 2);
        let rows = plain.content_view(40);
        let shown: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src, .. } if *src == 1 => Some(line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, vec!["  • indented".to_string()]);
    }

    /// Deletion belongs to EDIT: `^k` kills to the end of the line, and
    /// again on an exhausted line kills the line itself — all without
    /// leaving the session.
    #[test]
    fn ctrl_k_kills_to_end_then_kills_the_line() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one two", "three"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3); // caret after "one"

        handle_session_key(&mut app, &ctx, ctrl('k'));
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one", "tail killed");
        assert_eq!(app.lines.len(), 3, "still just a dirty line");

        handle_session_key(&mut app, &ctx, ctrl('k'));
        assert_eq!(app.lines.len(), 2, "the exhausted line went");
        assert_eq!(app.lines[1].text, "three");
        let s = app.session.as_ref().expect("still editing");
        assert_eq!(s.line, 1, "caret took the place the line left behind");
        assert_eq!(s.input.buf, "three");

        // Undo gives back what was on screen, not the pre-edit server text.
        handle_session_key(&mut app, &ctx, ctrl('z'));
        assert_eq!(app.lines[1].text, "one", "the line comes back as it looked");
    }

    /// The title line renames the page, so `^k` will not delete it either
    /// — the same rule `x` follows in READ.
    #[test]
    fn ctrl_k_never_deletes_the_title_line() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 0, 5); // caret at end of the title

        handle_session_key(&mut app, &ctx, ctrl('k'));
        assert_eq!(app.lines.len(), 2, "nothing deleted");
        assert!(app.status.contains("タイトル行"), "status: {}", app.status);
        assert!(app.session.is_some(), "and the session stays open");
    }

    /// READ is for reading. `x` no longer deletes anything; it points at
    /// the keys that do, in the session where editing happens.
    #[test]
    fn x_no_longer_deletes_and_says_where_deletion_lives() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        app.cursor = 1;

        handle_key(&mut app, &ctx, key(KeyCode::Char('x')));
        assert_eq!(app.lines.len(), 3, "nothing was deleted");
        assert!(drain_jobs(&mut app).is_empty(), "and nothing was committed");
        assert!(app.status.contains("^k"), "status: {}", app.status);
        assert!(app.status.contains("⌫"), "status: {}", app.status);

        // Not even with a selection: that range belongs to `c` (comment)
        // in READ, and to ⌫ in EDIT.
        app.selection = Some(Selection { anchor: 1, cursor: 2 });
        handle_key(&mut app, &ctx, key(KeyCode::Char('x')));
        assert_eq!(app.lines.len(), 3);
    }

    /// A web-side edit elsewhere on the page must not take the redo stack
    /// with it. Wiping the whole lineage on every remote refresh is what
    /// made `^r` look like a dead key: with 3 s polling, a single edit in
    /// the browser was enough to silently empty it.
    #[test]
    fn a_remote_edit_keeps_the_history_it_can_still_replay() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        do_edit(&mut app, &ctx, "line 2", vec![EditOp::Replace { id: "id1".into(), text: "ONE".into() }]);
        undo(&mut app, &ctx);
        assert_eq!(app.redo_stack.len(), 1);

        // Someone edits an UNRELATED line in the browser.
        let remote = polled(&[("id0", "title"), ("id1", "one"), ("id2", "TWO")]);
        install_remote_lines(&mut app, &ctx, &remote.page, "⟳ remote");
        assert_eq!(app.redo_stack.len(), 1, "the entry still names a live line");
        assert!(!app.history_dropped);

        redo(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "ONE", "^r still works after the refresh");
        assert_eq!(app.lines[2].text, "TWO", "and the remote edit survived it");
    }

    /// When the line an entry names is gone, that entry really cannot be
    /// replayed — but then the empty stack has to say so instead of
    /// looking like a broken key.
    #[test]
    fn history_that_cannot_be_replayed_is_dropped_with_a_reason() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        do_edit(&mut app, &ctx, "line 2", vec![EditOp::Replace { id: "id1".into(), text: "ONE".into() }]);
        undo(&mut app, &ctx);

        // The browser deletes that very line.
        let remote = polled(&[("id0", "title"), ("id2", "two")]);
        install_remote_lines(&mut app, &ctx, &remote.page, "⟳ remote");
        assert!(app.redo_stack.is_empty());
        assert!(app.history_dropped);

        redo(&mut app, &ctx);
        assert!(app.status.contains("失効"), "status: {}", app.status);

        // A new edit starts a clean lineage, and the excuse expires with it.
        do_edit(&mut app, &ctx, "line 3", vec![EditOp::Replace { id: "id2".into(), text: "x".into() }]);
        assert!(!app.history_dropped);
        redo(&mut app, &ctx);
        assert_eq!(app.status, "やり直せる編集がありません");
    }

    /// The undo reflex must not depend on which mode you are in: `^z`
    /// works in READ as well, next to the akapen `u` seat.
    #[test]
    fn read_mode_takes_ctrl_z_as_undo_too() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        do_edit(&mut app, &ctx, "delete", vec![EditOp::Delete { id: "id1".into() }]);
        assert_eq!(app.lines.len(), 2);

        handle_key(&mut app, &ctx, ctrl('z'));
        assert_eq!(app.lines.len(), 3, "^z undid the delete from READ");
        assert_eq!(app.lines[1].text, "one");

        handle_key(&mut app, &ctx, ctrl('r'));
        assert_eq!(app.lines.len(), 2, "^r still redoes");
    }

    /// SPEC §6: the safety net has to be reachable without leaving EDIT.
    #[test]
    fn session_undo_takes_back_the_typing_and_keeps_editing() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3); // caret at end of "one"
        type_str(&mut app, &ctx, "!!");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!!");

        handle_session_key(&mut app, &ctx, ctrl('z'));
        assert_eq!(app.lines[1].text, "one", "the dirty text committed, then undid");
        let s = app.session.as_ref().expect("still editing");
        assert_eq!(s.line, 1);
        assert_eq!(s.input.buf, "one", "the caret line reloaded from the undone state");
        assert_eq!(s.orig, "one", "and is clean again");

        handle_session_key(&mut app, &ctx, ctrl('r'));
        assert_eq!(app.lines[1].text, "one!!", "^r redoes without leaving EDIT");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!!");

        let labels: Vec<String> = drain_jobs(&mut app).into_iter().map(|j| j.0).collect();
        assert_eq!(labels.len(), 3, "typing, undo and redo each committed");
        assert!(labels[1].starts_with("undo"));
        assert!(labels[2].starts_with("redo"));
    }

    /// Undo held down: past the point where the caret line was created,
    /// the caret falls back to the line above instead of ejecting you from
    /// EDIT. Keeping the session is the whole point of the safety net.
    #[test]
    fn repeated_session_undo_never_ejects_you_from_edit() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        app.cursor = 1;
        open_line(&mut app, &ctx, false); // new line 2, session on it
        type_str(&mut app, &ctx, "fresh");

        handle_session_key(&mut app, &ctx, ctrl('z')); // undo the typing
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.session.as_ref().unwrap().line, 2);

        handle_session_key(&mut app, &ctx, ctrl('z')); // undo the line itself
        assert_eq!(app.lines.len(), 2, "the new line is gone");
        let s = app.session.as_ref().expect("still editing");
        assert_eq!(s.line, 1, "caret fell back to the line above");
        assert_eq!(s.input.cur, s.input.buf.len(), "at its end, as an editor would");
        assert_eq!(s.input.buf, "one");

        // Nothing left to undo: the session survives that too.
        handle_session_key(&mut app, &ctx, ctrl('z'));
        assert!(app.session.is_some());
        assert_eq!(app.status, "取り消せる編集がありません");
    }

    #[test]
    fn session_paste_multiline_becomes_lines() {
        let ctx = test_ctx();
        let mut app = page(&["title", "ab"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 1); // caret between a and b
        session_paste(&mut app, &ctx, "X\nY\nZ");
        assert_eq!(app.lines[1].text, "aX");
        assert_eq!(app.lines[2].text, "Y");
        assert_eq!(app.lines[3].text, "Zb", "line tail follows the last fragment");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 3);
        assert_eq!(s.input.cur, 1, "caret after the pasted Z");
    }

    #[test]
    fn open_line_below_inherits_indent_and_opens_session() {
        let ctx = test_ctx();
        let mut app = page(&["title", "  bullet", "after"]);
        app.rebuild(40);
        app.cursor = 1;
        open_line(&mut app, &ctx, false);
        assert_eq!(app.lines.len(), 4);
        assert_eq!(app.lines[2].text, "  ", "new line inherits the indent");
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.line, 2);
        assert_eq!(s.input.cur, 2);
    }

    /// A server page shaped like the poller would deliver it.
    fn polled(texts: &[(&str, &str)]) -> PolledPage {
        polled_at(texts, 0)
    }

    /// A poll response, stamped with the epoch its fetch started at.
    fn polled_at(texts: &[(&str, &str)], epoch: u64) -> PolledPage {
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

    #[test]
    fn remote_edits_apply_when_idle_and_keep_the_cursor_line() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.cursor = 2; // on "two" (id2)
        // remote inserted a line above and edited "one"
        apply_remote(
            &mut app,
            &ctx,
            polled(&[("id0", "t"), ("idN", "new!"), ("id1", "ONE"), ("id2", "two")]),
        );
        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["t", "new!", "ONE", "two"]);
        assert_eq!(app.cursor, 3, "cursor followed its line id, not its number");
        assert!(app.status.contains("web"));
    }

    #[test]
    fn remote_edits_never_touch_a_dirty_caret_line() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!"); // dirty
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        assert_eq!(app.lines[1].text, "one", "deferred while dirty");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one!");
        // …but a CLEAN session re-anchors and picks up the remote text
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        let _ = drain_jobs(&mut app);
        enter_session(&mut app, &ctx, 1, 0);
        app.inflight = 0;
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "REMOTE");
        assert_eq!(s.orig, "REMOTE");
    }

    #[test]
    fn remote_apply_waits_for_inflight_commits() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.inflight = 1;
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        assert_eq!(app.lines[1].text, "one", "own commits must land first");
    }

    // ---- websocket push (NOTE-websocket-sync.md) ----

    /// One remote commit event, shaped like the ws thread would deliver it.
    fn commit(id: &str, parent: &str, page: &str, user: &str, ops: Vec<EditOp>) -> RemoteCommit {
        RemoteCommit {
            commit_id: id.into(),
            parent_id: parent.into(),
            page_id: page.into(),
            user_id: user.into(),
            ops,
        }
    }

    #[test]
    fn ws_contiguous_commit_applies_a_diff_and_advances_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "ONE!".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "ONE!", "diff applied in place");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.status.contains("websocket"));
    }

    #[test]
    fn ws_same_user_browser_commit_applies() {
        // A commit from the SAME account is the primary use case (edit in
        // the browser, watch the TUI follow) — it must apply, not be
        // dropped as a "self-echo" (idempotency protects against real
        // echoes instead). There is no me_id concept anymore.
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "me", // same user that runs the TUI
                vec![EditOp::Replace { id: "id1".into(), text: "SELF".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "SELF", "same-user browser edit reflects");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert_eq!(app.lines[1].user_id, "me", "the committer is stamped for blame");
    }

    #[test]
    fn ws_own_echo_does_not_double_insert() {
        // OUR commit came back as an event: applying it again must not
        // duplicate the inserted line (insert-by-id skip).
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        let echo = commit(
            "c1",
            "c0",
            &pid,
            "me",
            vec![EditOp::Insert {
                anchor: "_end".into(),
                lines: vec![("n1".into(), "new line".into())],
            }],
        );
        ws_on_commit(&mut app, &ctx, echo.clone());
        assert_eq!(app.lines.len(), 3);
        // the echo reapplies (it is just an idempotent no-op now)
        ws_on_commit(&mut app, &ctx, echo);
        assert_eq!(app.lines.len(), 3, "no duplicate from the echo");
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "one", "new line"]);
    }

    /// The reported bug: confirm an IME composition (Enter), then press
    /// Enter again straight away. The caret ends up a line lower, or the
    /// session drops out to view mode, and the status says the websocket
    /// applied an update.
    ///
    /// The sequence is: the text commit goes out, the split commit goes
    /// out, both are acknowledged — and only THEN does the websocket echo
    /// of the first one arrive. It carries the line as it was before the
    /// split, which is older than what is on screen.
    #[test]
    fn a_late_echo_of_our_own_commit_does_not_undo_the_split_after_it() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();

        // Type into the line (as an IME commit does), then Enter: the
        // dirty text commits, and the line splits at the caret.
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "日本語");
        handle_session_key(&mut app, &ctx, key(KeyCode::Left));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        let after_split: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        let caret_line = app.session.as_ref().map(|s| s.line).unwrap();
        let caret_id = app.lines[caret_line].id.clone();
        assert_eq!(after_split, vec!["t", "one日本", "語"]);

        // Both commits are acknowledged: nothing is in flight, so the
        // gates are down.
        while app.inflight > 0 {
            handle_commit_outcome(
                &mut app,
                &ctx,
                CommitOutcome::Done {
                    job: UNRELATED_JOB,
                    label: "line".into(),
                    title: String::new(),
                    commit_id: "c1".into(),
                },
            );
        }

        // Now the echo of the FIRST commit turns up.
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "me",
                vec![EditOp::Replace { id: "id1".into(), text: "one日本語".into() }],
            ),
        );
        let now: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(now, after_split, "our own echo must not roll the page back");
        assert!(app.session.is_some(), "and must not drop the reader out of EDIT");
        assert_eq!(
            app.session.as_ref().map(|s| app.lines[s.line].id.clone()),
            Some(caret_id),
            "the caret stays on the line it was typing"
        );
    }

    #[test]
    fn ws_commits_buffer_while_gated_and_flush_in_order() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        app.inflight = 1; // our own commit is landing — gate up
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "FIRST".into() }],
            ),
        );
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "c1",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("n1".into(), "tail".into())],
                }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "nothing applied while gated");
        assert_eq!(app.ws_pending.len(), 2);
        // the commit lands: gate drops, buffered events apply oldest first
        app.inflight = 0;
        ws_flush_pending(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "FIRST", "two", "tail"]);
        assert_eq!(app.ws_head.as_deref(), Some("c2"));
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_dirty_caret_line_buffers_the_commit() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!"); // dirty caret line — gate up
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "REMOTE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "buffered, not applied");
        assert_eq!(app.ws_pending.len(), 1);
        // leaving the line commits it and drops the gate; flush applies
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        let _ = drain_jobs(&mut app);
        app.inflight = 0; // the queued commit is "done" in this test harness
        ws_flush_pending(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "REMOTE");
    }

    #[test]
    fn ws_different_page_events_are_dropped() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                "some-other-page",
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NOPE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "stale-room events go nowhere");
        assert!(app.ws_pending.is_empty());
    }

    /// A resync result as the ws thread would ship it, stamped with the
    /// epoch the fetch started at (tests that do not care pass the current
    /// one, which is what a fetch with nothing racing it would carry).
    fn resync(page: cosense::api::Page, head: Option<&str>) -> ws::ResyncPage {
        resync_at(page, head, 0)
    }
    fn resync_at(page: cosense::api::Page, head: Option<&str>, epoch: u64) -> ws::ResyncPage {
        ws::ResyncPage { page, head: head.map(str::to_string), epoch }
    }

    /// The other half of the report: pressing Enter twice quickly could
    /// throw the reader out of EDIT into view mode.
    ///
    /// A resync fetch that STARTED before our commit landed comes back
    /// without the line we just made. Installing it deletes that line, and
    /// a session whose line has vanished closes — mid-word. The page is a
    /// photograph of the past, so it is refused and another one asked for,
    /// which is what polls have always done.
    #[test]
    fn a_resync_fetched_before_our_own_commit_is_refused() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();

        // The ws thread starts a resync fetch: it stamps the epoch it saw.
        let fetched_at = app.server_epoch_now();

        // Meanwhile we split the line and the commit lands, which moves the
        // server state on (and the epoch with it).
        enter_session(&mut app, &ctx, 1, 3);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        while app.inflight > 0 {
            handle_commit_outcome(
                &mut app,
                &ctx,
                CommitOutcome::Done {
                    job: UNRELATED_JOB,
                    label: "line".into(),
                    title: String::new(),
                    commit_id: "c1".into(),
                },
            );
        }
        let lines_now = app.lines.len();
        let caret_id = app.session.as_ref().map(|s| app.lines[s.line].id.clone());
        assert!(caret_id.is_some());

        // Now the stale page arrives: it predates the split.
        let mut p = polled(&[("id0", "t"), ("id1", "one")]).page;
        p.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync_at(p, Some("c1"), fetched_at)));

        assert_eq!(app.lines.len(), lines_now, "the new line survives");
        assert!(app.session.is_some(), "and the reader stays in EDIT");
        assert_eq!(
            app.session.as_ref().map(|s| app.lines[s.line].id.clone()),
            caret_id,
            "on the same line"
        );
        assert!(app.ws_resync_pending, "a fresh page is asked for instead");
    }

    #[test]
    fn ws_resync_installs_the_page_and_resumes_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c9".into());
        let pid = app.page_id.clone();
        let mut p = polled(&[("id0", "t"), ("idX", "fresh!"), ("id1", "one")]).page;
        p.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c12"))));
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "fresh!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c12"), "chain resumes at head");
        // …and the next commit with that parent applies as a contiguous diff
        let pid2 = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c13",
                "c12",
                &pid2,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NEXT".into() }],
            ),
        );
        // "id1" is at index 2 now (the resync inserted "idX" above it)
        assert_eq!(app.lines[2].text, "NEXT", "the replace landed on id1");
        assert_eq!(app.ws_head.as_deref(), Some("c13"));
    }

    #[test]
    fn ws_resync_while_gated_is_held_and_applied_afterwards() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        app.inflight = 1; // gate up
        // a commit that must be superseded by the resync
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("o1".into(), "old".into())],
                }],
            ),
        );
        let mut p = polled(&[("id0", "t"), ("idN", "fresh!"), ("id1", "one")]).page;
        p.id = pid.clone();
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c1"))));
        // still gated: the resync is HELD, the commit is buffered before it
        assert!(app.ws_held_resync.is_some(), "the latest resync is kept");
        assert_eq!(app.lines[1].text, "one");
        // a second resync while gated REPLACES the held one (latest wins)
        let mut p2 = polled(&[("id0", "t"), ("idN2", "newer!"), ("id1", "one")]).page;
        p2.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p2, Some("c1"))));
        // gate drops: latest held resync applies, the pre-resync commit is
        // superseded (its effect is inside the fetched page)
        app.inflight = 0;
        ws_apply_held_resync(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "newer!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.ws_held_resync.is_none());
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_chain_break_requests_a_background_resync() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "cNOPE",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "??".into() }],
            ),
        );
        // the stray commit is not applied and NOT silently dropped: a
        // background resync is requested (no network on the UI thread)
        assert_eq!(app.lines[1].text, "one");
        assert!(app.ws_resync_pending, "gap event requests a resync");
        ws_send_resync_request(&mut app);
        let req_rx = app.ws_req_rx.as_ref().unwrap();
        assert_eq!(req_rx.try_recv(), Ok(ws::WsRequest::Resync));
        assert!(!app.ws_resync_pending, "one request per gap flag");
    }

    #[test]
    fn session_line_shows_bullets_for_indent() {
        // Two cells per nesting step after the flush-left level 1; DATA
        // remains one whitespace character per logical level.
        assert_eq!(session_display(" ab", None), "• ab");
        assert_eq!(session_display("  ab", None), "  • ab");
        assert_eq!(session_display(" \t\tab", None), "    • ab", "tabs count as levels too");
        assert_eq!(session_display("ab", None), "ab", "no indent, no bullet");
        assert_eq!(session_display("  ", None), "  • ", "a whitespace-only line shows just its bullet");
        // Caret mapping expands and contracts the visual indentation.
        assert_eq!(display_caret("  ab", 1, None), 2, "one logical step = two cells");
        assert_eq!(display_caret("  ab", 2, None), "  • ".len(), "text start lands after '• '");
        assert_eq!(raw_caret_from_display("  ab", "  • ".len(), None), 2);
        // clicking ON the injected space snaps to the text start
        assert_eq!(raw_caret_from_display("  ab", "  •".len(), None), 2);
        assert_eq!(display_caret("  ab", 4, None), "  • ab".len(), "end maps to end");
        assert_eq!(raw_caret_from_display("  ab", "  • ab".len(), None), 4);
        assert_eq!(display_caret("\t\tab", 2, None), "  • ".len());
        assert_eq!(raw_caret_from_display("\t\tab", "  • ".len(), None), 2);
        assert_eq!(display_caret("ab", 1, None), 1, "no indent → identity");
        // the rendered session row carries the bullet + space
        let ctx = test_ctx();
        let mut app = page(&["t", "  deep"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 2);
        app.rebuild(40);
        let (first, _) = app.src_rows(1).unwrap();
        let row_text: String = match &app.rows[first] {
            Row::Line { line, .. } => line.spans.iter().map(|s| s.content.as_ref()).collect(),
            _ => String::new(),
        };
        assert_eq!(row_text, "  • deep");
        // …and typing still edits the RAW text underneath
        type_str(&mut app, &ctx, "!");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "  !deep");
    }

    #[test]
    fn caret_math_matches_the_wrapped_display() {
        // 10-wide: "あいうえお" is 10 cols → caret after う = row0 col6;
        // after お = boundary → next row col0 when more text follows.
        let s = "あいうえおxy";
        let flat = SessionWrap::new(s, 10, 0);
        assert_eq!(flat.row_col(9), (0, 6));
        assert_eq!(flat.row_col(15), (1, 0));
        assert_eq!(flat.row_col(17), (1, 2));
        assert_eq!(byte_at_col("あいう", 4), 6, "col 4 → third kana");
        // segments carry every char: display and math agree
        assert_eq!(flat.segs.join(""), s);
        // …and a click comes back to where the caret was.
        for byte in [0, 3, 9, 15, 17] {
            let (row, col) = flat.row_col(byte);
            assert_eq!(flat.offset_at(row, col), byte, "byte {byte}");
        }
    }

    /// EDIT wrapped flat while READ hung its continuations, so editing a
    /// long bullet made its text jump back to the margin. The caret math
    /// has to move with the drawing: a click, ↑/↓ and the hardware cursor
    /// all read the same wrap.
    #[test]
    fn the_caret_line_hangs_its_continuations_like_the_view() {
        let ctx = test_ctx();
        // A bullet at level 2: its text starts at column 4 ("  • ").
        let long = format!("  {}", "あ".repeat(30));
        let mut app = page(&["t", &long]);
        app.rebuild(46);
        let width = App::text_width(app.mode, app.laid_width);
        app.text_rect = Rect::new(1, 2, width as u16, 8);
        enter_session(&mut app, &ctx, 1, long.len());

        let disp = session_display(&long, None);
        let hang = session_hang(&long, None);
        assert_eq!(hang, 4, "under the text, not under the bullet");
        let wrapped = SessionWrap::new(&disp, width, hang);
        assert!(wrapped.segs.len() > 1, "it really wraps");
        assert_eq!(wrapped.indent_of(0), 0);
        assert_eq!(wrapped.indent_of(1), hang);

        // The drawn rows carry that indent…
        let rows: Vec<String> = app
            .content_view(46)
            .iter()
            .filter_map(|r| match r {
                Row::Line { line, src, .. } if *src == 1 => {
                    Some(line.spans.iter().map(|s| s.content.as_ref()).collect())
                }
                _ => None,
            })
            .collect();
        assert!(rows[1].starts_with("    "), "continuation hangs: {:?}", rows[1]);

        // …and the caret math agrees with them, in both directions.
        let last = display_caret(&long, long.len(), None);
        let (row, col) = wrapped.row_col(last);
        assert!(row > 0);
        assert!(col >= hang, "the caret is inside the hanging body: {col}");
        assert_eq!(wrapped.offset_at(row, col), last);
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
    fn table_rows_are_individually_addressable() {
        let mut app = page(&["table:t", "\ta\tb", "\tc\td", "after"]);
        app.rebuild(40);
        // every source line of the table owns display rows now
        assert!(app.src_rows(0).is_some(), "opener: name line + top border");
        assert!(app.src_rows(1).is_some(), "header row");
        assert!(app.src_rows(2).is_some(), "body row");
        app.goto_src(2);
        assert_eq!(app.cursor, 2, "cursor rests on the row itself");
        // j/k walk opener → header → row → after
        app.goto_src(0);
        app.move_cursor(true);
        assert_eq!(app.cursor, 1);
        app.move_cursor(true);
        assert_eq!(app.cursor, 2);
        app.move_cursor(true);
        assert_eq!(app.cursor, 3);
        // the last line is reachable and the clamp respects the end
        app.goto_src(99);
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn follow_cursor_uses_last_row_going_down_and_first_going_up() {
        let mut app = page(&["a", "b", "c", &"y".repeat(50), "e"]);
        app.rebuild(22); // line 3 wraps to 4 rows: rows 3,4,5,6
        app.goto_src(3);
        app.follow_cursor(4);
        // rows 3..=6 inside a 4-row viewport, with one row below for the
        // bottom rule -> scroll = 7 - 4 + 1
        assert_eq!(app.scroll, 4);
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
        app.rebuild(22); // rows: a, b, b, b, b, c
        assert_eq!(app.src_at_screen_row(0), Some(0));
        assert_eq!(app.src_at_screen_row(2), Some(1), "wrapped continuation -> its line");
        assert_eq!(app.src_at_screen_row(5), Some(2));
        assert_eq!(app.src_at_screen_row(6), None, "past the end");
        app.scroll = 3;
        assert_eq!(app.src_at_screen_row(-1), Some(1), "reclaimed top row maps too");
        assert_eq!(app.src_at_screen_row(0), Some(1));
        assert_eq!(app.src_at_screen_row(2), Some(2));
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    /// A 30-line page in the same vertical geometry as `ui`: body band
    /// y=1..10, unscrolled content anchor y=2, scrollbar at column 41.
    fn page_with_screen() -> App {
        let texts: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.rebuild(42);
        app.text_rect = Rect::new(1, 2, 40, 8);
        app.bar_rect = Rect::new(41, 1, 1, 10);
        app.view_h = 10;
        app
    }

    #[test]
    fn page_chrome_separates_caret_telomere_pad_and_top_scrollbar() {
        use ratatui::{backend::TestBackend, Terminal};

        let texts: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.header_colors = HeaderColors {
            fg: Color::White,
            bg: Color::Rgb(80, 102, 184),
        };
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 12)).unwrap();
        // The first draw establishes wrapped rows; the next is the stable
        // frame the event loop presents.
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            // body starts at y=1; its first content row is y=2.
            assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::White);
            assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(80, 102, 184));
            assert_eq!(buf.cell((0, 2)).unwrap().symbol(), ">");
            assert_eq!(buf.cell((0, 2)).unwrap().fg, Color::LightBlue);
            assert_ne!(buf.cell((1, 2)).unwrap().symbol(), " ", "telomere remains visible");
            assert_eq!(buf.cell((2, 2)).unwrap().symbol(), " ", "one blank after telomere");
            assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "l", "text starts after the blank");
            for x in 1..=40 {
                assert_eq!(
                    buf.cell((x, 2)).unwrap().bg,
                    Color::DarkGray,
                    "cursor band must fill every inside column at x={x}",
                );
            }
            // The thumb starts below the top frame rule instead of overwriting it.
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "─");
            assert_eq!(buf.cell((40, 1)).unwrap().fg, Color::DarkGray);
            assert_eq!(buf.cell((40, 2)).unwrap().symbol(), "▐");
            assert_eq!(buf.cell((40, 2)).unwrap().fg, Color::Gray);
        }

        // Once the top rule scrolls away, screen row 2 (y=1) is reclaimed
        // by every page layer instead of remaining a permanent blank strip.
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            assert_eq!(buf.cell((0, 1)).unwrap().symbol(), ">");
            assert_ne!(buf.cell((1, 1)).unwrap().symbol(), " ", "telomere at top row");
            assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "l", "text at top row");
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "▐", "scrollbar at top row");
        }
        app.cursor = 5;
        handle_mouse_content(
            &mut app,
            &ctx,
            mouse(MouseEventKind::Down(MouseButton::Left), 8, 1),
        );
        assert_eq!(app.cursor, 0, "reclaimed top row remains mouse-addressable");

        enter_session(&mut app, &ctx, 0, 0);
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((20, 1)).unwrap().symbol(), " ", "EDIT removes the top frame");
        assert_eq!(buf.cell((41, 3)).unwrap().symbol(), " ", "EDIT removes the side frame");
    }

    #[test]
    fn scrolled_image_placeholder_reclaims_the_top_body_row() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = page(&["image"]);
        app.blocks = vec![Block::Image { url: "https://example.com/a.png".into(), indent: 0, item: false }];
        app.srcs = vec![0];
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 8)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "□");
            assert_ne!(buf.cell((1, 1)).unwrap().symbol(), " ", "image telomere at top row");
        }

        // The real sliced image uses a different widget path from its text
        // placeholder and must reclaim the same row.
        let pixels = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]));
        let info = build_image(&ctx.picker, image::DynamicImage::ImageRgba8(pixels), IMAGE_MAX_COLS).unwrap();
        app.images.insert("https://example.com/a.png".into(), info);
        app.rebuild(42);
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        assert!(
            (3..20).any(|x| {
                let cell = buf.cell((x, 1)).unwrap();
                cell.fg == Color::Rgb(255, 0, 0) || cell.bg == Color::Rgb(255, 0, 0)
            }),
            "sliced image paints the reclaimed top row",
        );
    }

    #[test]
    fn click_moves_cursor_and_drag_selects_a_line_range() {
        let mut app = page_with_screen();
        app.scroll = 5;
        // press two rows below the content anchor -> display row 7
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 10, 4));
        assert_eq!(app.cursor, 7);
        assert_eq!(app.selection, None, "a click alone selects nothing");
        // drag four rows down -> line 11: selection 7..=11
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 12, 8));
        assert_eq!(app.selection.map(|s| s.range()), Some((7, 11)));
        assert_eq!(app.cursor, 11);
        // dragging back above the anchor flips the range
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 12, 2));
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Up(MouseButton::Left), 12, 2));
        assert_eq!(app.drag_anchor, None);
        // the selection survives the release (c comments on it)
        assert_eq!(app.selection.map(|s| s.range()), Some((5, 7)));
        // a click outside the rows (header) changes nothing
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 10, 0));
        assert_eq!(app.cursor, 5);
    }

    #[test]
    fn rendered_links_are_mouse_hit_targets() {
        let mut app = page(&[
            "t",
            "see [Target] and [Docs https://example.com] #tag",
        ]);
        app.rebuild(80);
        // Rendered row: `see Target and Docs #tag`.
        assert_eq!(
            app.link_at_screen_position(1, 4),
            Some((1, LinkItem::Page("Target".into())))
        );
        assert_eq!(
            app.link_at_screen_position(1, 15),
            Some((
                1,
                LinkItem::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into(),
                },
            ))
        );
        assert_eq!(
            app.link_at_screen_position(1, 20),
            Some((1, LinkItem::Page("tag".into())))
        );
        assert_eq!(
            app.link_at_screen_position(1, 3),
            None,
            "plain text is not clickable"
        );

        // A `table:` header is followable too (Enter saves the CSV), and
        // a table's rows are not Text blocks — they are laid out against
        // the pane at draw time — so it takes the column path of its own.
        let mut tbl = page(&["t", "table:売上", "\t1\t2"]);
        tbl.rebuild(80);
        assert!(matches!(
            tbl.link_at_screen_position(1, 3),
            Some((1, LinkItem::Export { .. })),
        ));
        assert_eq!(tbl.link_at_screen_position(1, 40), None, "past the label is not the label");

        // A link to a page nobody has written is drawn in another colour,
        // and clicking it is how that page gets written — so the hit test
        // has to know that colour too. (It did not, and a red link was the
        // one thing on a page a click could not follow.) The same goes for
        // a tag: `#foo` and `[foo]` are one page, drawn one way.
        let mut fresh = page(&["t", "see [Target] and #tag"]);
        fresh.links = LinkTruth::seed(["Target", "tag"], ["別のページ"]);
        rerender(&mut fresh, &test_ctx());
        fresh.rebuild(80);
        assert_eq!(
            fresh.link_at_screen_position(1, 4),
            Some((1, LinkItem::Page("Target".into()))),
            "an uncreated link still opens"
        );
        assert_eq!(
            fresh.link_at_screen_position(1, 16),
            Some((1, LinkItem::Page("tag".into()))),
            "and so does an uncreated tag"
        );

        // Identical labels still resolve by their rendered occurrence.
        let mut dup = page(&[
            "t",
            "see [Docs] then [Docs https://example.com]",
        ]);
        dup.rebuild(80);
        assert_eq!(
            dup.link_at_screen_position(1, 14),
            Some((
                1,
                LinkItem::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into(),
                },
            ))
        );

        let mut wrapped = page(&["t", "[ABCDEFGHIJK]"]);
        wrapped.rebuild(12); // text width 6: the link occupies rows 1 and 2
        assert_eq!(
            wrapped.link_at_screen_position(2, 1),
            Some((1, LinkItem::Page("ABCDEFGHIJK".into())))
        );

        // Related-page rows stay clickable after visual truncation — and
        // without an underline to look for: those rows are drawn plain
        // (the row IS the link), so the row itself is the target.
        let related_src = wrapped.lines.len();
        wrapped.virtual_items = vec![LinkItem::Page("Very long related page".into())];
        wrapped.rows = vec![Row::Line {
            line: Line::from(Span::styled("Very…", Style::default().fg(CHROME_CARET))),
            src: related_src,
            start: 0,
            hang: 0,
        }];
        assert_eq!(
            wrapped.link_at_screen_position(0, 2),
            Some((related_src, LinkItem::Page("Very long related page".into())))
        );
        // …but empty space below the list is not a target.
        wrapped.rows = vec![Row::Line {
            line: Line::from("   "),
            src: related_src,
            start: 0,
            hang: 0,
        }];
        assert_eq!(wrapped.link_at_screen_position(0, 2), None);

        // Pressing records the target, but dragging cancels activation and
        // keeps the existing line-selection gesture available.
        app.text_rect = Rect::new(2, 1, 74, 8);
        app.bar_rect = Rect::new(77, 1, 1, 8);
        handle_mouse_content(
            &mut app,
            &test_ctx(),
            mouse(MouseEventKind::Down(MouseButton::Left), 2 + 4, 1 + 1),
        );
        assert_eq!(app.pressed_link, Some((1, LinkItem::Page("Target".into()))));
        handle_mouse_content(
            &mut app,
            &test_ctx(),
            mouse(MouseEventKind::Drag(MouseButton::Left), 2 + 8, 1 + 1),
        );
        assert_eq!(app.pressed_link, None);
    }

    #[test]
    fn scrollbar_click_and_drag_scrub_the_viewport_only() {
        let mut app = page_with_screen();
        app.goto_src(3);
        // top rule + 30 rows + FrameEnd / 10 viewport: the scrollbar
        // includes the complete visual extent.
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Down(MouseButton::Left), 41, 1 + 7));
        let end = app.max_scroll(app.view_h);
        assert_eq!(app.scroll, end, "track bottom -> last page");
        assert_eq!(app.cursor, 3, "the cursor keeps its line");
        assert!(!app.follow);
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Drag(MouseButton::Left), 41, 1 + 3));
        assert!(app.scroll < end && app.scroll > 0);
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::Up(MouseButton::Left), 41, 1 + 3));
        assert_eq!(app.scrollbar_drag, None);
        // wheel over the body: one row per event, cursor untouched
        handle_mouse_content(&mut app, &test_ctx(), mouse(MouseEventKind::ScrollUp, 10, 5));
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
        // Cross-project links: `[/project/title]` is that page, and a bare
        // `[/project]` is the project itself — on Cosense its home screen,
        // here its index. Tags stay in-project.
        assert_eq!(
            links_on_line("see [/shokai/階層整理型WiKiはスケールしない] and [/tus-alpine] #Scrapboxの哲学 [/ italic]"),
            vec![
                LinkItem::ProjectPage { project: "shokai".into(), title: "階層整理型WiKiはスケールしない".into() },
                LinkItem::ProjectIndex { project: "tus-alpine".into() },
                LinkItem::Page("Scrapboxの哲学".into()),
            ]
        );
        assert_eq!(
            LinkItem::ProjectPage { project: "shokai".into(), title: "t".into() }.label(),
            "/shokai/t"
        );
        assert!(matches!(&items[1], LinkItem::File { label, .. } if label == "260826ニセコ.pdf"));
        assert_eq!(items[1].label(), "↓ 260826ニセコ.pdf");
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

    /// Where downloads go is asked in a fixed order, and `~/Downloads` is
    /// only one answer in it — a box without that directory used to save
    /// into whatever directory the viewer started in (改善案3).
    #[test]
    fn the_download_directory_is_chosen_in_a_fixed_order() {
        let tmp = std::env::temp_dir().join(format!("cosense-dl-{}", std::process::id()));
        let home = tmp.join("home");
        let xdg = tmp.join("xdg");
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();
        let cwd = tmp.join("cwd");
        let h = home.to_str().unwrap();
        let pick = |explicit, env, xdg: Option<&str>| {
            pick_download_dir(explicit, env, xdg, Some(h), cwd.clone())
        };

        // The flag wins, and naming a place means "make it" — even under a
        // `~`, which arrives unexpanded when it was set by hand.
        let (dir, named) = pick(Some("~/elsewhere"), Some("/env"), xdg.to_str());
        assert_eq!((dir, named), (home.join("elsewhere"), true));
        // then the env var
        assert_eq!(pick(None, Some("/env"), xdg.to_str()).0, std::path::PathBuf::from("/env"));
        // then the desktop's own setting, but only where it really exists
        assert_eq!(pick(None, None, xdg.to_str()), (xdg.clone(), false));
        assert_eq!(pick(None, None, Some("/nope")).0, home.join("Downloads"), "missing XDG dir is skipped");
        // then ~/Downloads, and the working directory when even that is gone
        assert_eq!(pick(None, None, None), (home.join("Downloads"), false));
        std::fs::remove_dir_all(home.join("Downloads")).unwrap();
        assert_eq!(pick(None, None, None), (cwd.clone(), false));
        // blanks are not answers
        assert_eq!(pick(Some("  "), Some(""), None), (cwd, false));

        std::fs::remove_dir_all(&tmp).unwrap();
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
