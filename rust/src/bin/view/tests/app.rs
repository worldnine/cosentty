use crate::*;
use super::support::*;

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
    fn a_session_without_a_sid_is_never_called_reconnecting() {
        let mut app = page(&["a"]);
        app.ws_attempted = false;
        app.set_sync_state(SyncState::Reconnecting);
        assert_eq!(app.sync_label(), "poll");
    }

    // ---------------------------------------------------------------
    // Web renderer (Mermaid). The browser itself is never launched here:
    // `FakeBackend` stands in at the request→artifact boundary.
    // ---------------------------------------------------------------
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
        assert_ne!(selection_bg((24, 24, 24)), Color::Reset);
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
    fn cursor_fraction_over_source_lines() {
        let mut app = page(&["a", "b", "c", "d", "e"]);
        app.rebuild(40);
        assert_eq!(app.cursor_fraction(), 0.0);
        app.goto_src(4);
        assert_eq!(app.cursor_fraction(), 1.0);
        app.goto_src(2);
        assert_eq!(app.cursor_fraction(), 0.5);
    }
