use crate::*;
use crate::tests::support::*;

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
    // lib spike: web経路の検証なのでテキスト段を止める。
    app.mermaid_text = false;
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
    assert_ne!(app.toast_text(), "最新", "and it is not labelled as the live page");
    assert!(app.toast_text().contains("読み直しに失敗"), "the reason is shown: {}", app.toast_text());
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
    let mut app = web_tier_page();
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
    let mut app = web_tier_page();
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
    // lib spike: web経路の検証なのでテキスト段を止める。
    app.mermaid_text = false;
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
    // lib spike: web経路の検証なのでテキスト段を止める。
    app.mermaid_text = false;
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
/// The default policy is OFF: no worker, no backend, no cache I/O, and
/// nothing automatic asks the browser for anything. The text tier is
/// the mainline; the browser is a tool the reader raises when they want
/// it (`COSENSE_WEB_RENDER=manual|auto`) — and a session without a
/// `connect.sid` stays at full strength.
#[test]
fn the_default_policy_is_off_and_touches_nothing() {
    let mut app = web_tier_page();
    // The fixture raises Auto for the render-pipeline tests; the DEFAULT
    // is whatever the environment says, which here is nothing.
    app.render_policy = capability::RenderPolicy::from_env();
    assert_eq!(app.render_policy, capability::RenderPolicy::Off);
    app.rebuild(80);
    assert!(!app.start_web_renders(capability::Trigger::Auto));
    assert!(!app.start_web_renders(capability::Trigger::Manual));
    assert!(
        app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
        "no job was ever queued"
    );
    // The note names the way out.
    handle_key(&mut app, &test_ctx(), key(KeyCode::Char('R')));
    assert!(
        app.note.as_ref().is_some_and(|n| n.0.contains("COSENSE_WEB_RENDER")),
        "R says why: {:?}",
        app.note
    );
}

#[test]
fn a_manual_page_load_shows_cached_diagrams_and_starts_no_browser() {
    let mut app = web_tier_page();
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
    assert!(app.note.is_none(), "an empty cache is not worth a notice");
}

#[test]
fn r_renders_the_artifacts_the_page_load_could_not() {
    let mut app = web_tier_page();
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
    let mut app = web_tier_page();
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
    let mut app = web_tier_page();
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
    app.note = None;
    // A second pass over the same page says nothing more.
    app.start_web_renders(capability::Trigger::Auto);
    assert!(app.note.is_none());
}

#[test]
fn unknown_visibility_waits_for_an_explicit_r() {
    let mut app = web_tier_page();
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
    let mut app = web_tier_page();
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
