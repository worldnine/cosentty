use super::support::*;
use crate::*;

#[test]
fn an_unchanged_fallback_poll_backs_off_and_a_change_makes_it_fast_again() {
    let mut cadence = PollCadence::new(capability::FAST_POLL);
    assert_eq!(cadence.interval(), Duration::from_secs(3));

    // The first success establishes the baseline; subsequent equal revisions
    // walk the idle schedule to its one-minute ceiling.
    for expected in [6, 12, 30, 60, 60] {
        cadence.observed("proj", "page", 1);
        assert_eq!(cadence.interval(), Duration::from_secs(expected));
    }

    cadence.observed("proj", "page", 2);
    assert_eq!(cadence.interval(), Duration::from_secs(3));
    cadence.observed("proj", "page", 2);
    assert_eq!(cadence.interval(), Duration::from_secs(6));
}

#[test]
fn a_new_target_restarts_the_idle_schedule_and_live_push_stays_fixed() {
    let mut cadence = PollCadence::new(capability::FAST_POLL);
    for _ in 0..4 {
        cadence.observed("proj", "first", 1);
    }
    assert_eq!(cadence.interval(), Duration::from_secs(60));

    cadence.retune(capability::FAST_POLL);
    cadence.observed("proj", "second", 9);
    assert_eq!(cadence.interval(), Duration::from_secs(6));

    cadence.retune(capability::LIVE_POLL);
    cadence.observed("proj", "second", 10);
    assert_eq!(
        cadence.interval(),
        Duration::from_secs(60),
        "a websocket room uses a fixed insurance poll"
    );
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

#[test]
fn a_result_for_an_older_page_generation_is_dropped() {
    let mut app = mermaid_page();
    app.rebuild(80);
    let req = app
        .web_request(cosense::webrender::WebKind::Mermaid, "flowchart", 3)
        .unwrap();
    let key = req.cache_key();
    app.web_pending.insert(key.clone());
    let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
    // The reader navigated away and back while the browser was busy.
    let stale_gen = app.gen_now().wrapping_sub(1);
    app.web_tx
        .send(WebMsg {
            gen: stale_gen,
            key: key.clone(),
            rescale: false,
            attempted: None,
            res: WebOutcome::Drawn(info),
        })
        .unwrap();
    assert!(!app.drain_web_renders(), "a stale result changes nothing");
    assert!(
        !app.images.contains_key(&key),
        "yesterday's diagram is not installed"
    );
    // …and it touches NO state. The same key can legitimately be
    // pending again for the current page, so clearing by key alone
    // would cancel that live request. What keeps pending from leaking
    // is `set_page` — see
    // `set_page_clears_the_state_a_dropped_job_would_have_answered`.
    assert!(app.web_pending.contains(&key));
}

#[test]
fn editing_a_drawn_artifact_under_image_renders_the_new_source() {
    let ctx = test_ctx();
    let mut app = mermaid_page();
    app.render_policy = capability::RenderPolicy::Image;
    app.rebuild(80);
    let before = diagram_keys(&app);
    let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
    app.images.insert(before[0].clone(), info);

    // Editing changes the artifact key: the old picture no longer shows,
    // the text drawing takes its place, and the new source is rendered —
    // browser allowed, since the reader chose pictures.
    app.lines[3].text = "   A-->C".into();
    rerender(&mut app, &ctx);
    let after = diagram_keys(&app);
    assert_ne!(after[0], before[0]);
    assert!(
        !app.rows.iter().any(|r| matches!(r, Row::Image { .. })),
        "the stale picture is not shown for the new source"
    );
    let job = app
        .web_jobs_rx
        .as_ref()
        .unwrap()
        .try_recv()
        .expect("a render for the new source");
    let WebJob::Render { reqs, auth, .. } = job else {
        panic!("expected a render job")
    };
    assert!(auth.is_some(), "image renders edits without being asked");
    assert!(reqs.iter().any(|r| r.cache_key() == after[0]));
}

#[test]
fn r_pressed_while_the_cache_probe_is_out_is_served_when_it_answers() {
    let ctx = test_ctx();
    let mut app = web_tier_page();
    app.render_policy = capability::RenderPolicy::Image;
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
    assert!(
        app.note.is_none(),
        "a request in flight is not 'nothing to draw'"
    );
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
    let WebJob::Render { reqs, auth, .. } = &job else {
        panic!("expected a render job")
    };
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
    app.render_policy = capability::RenderPolicy::Image;
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
    assert!(
        !app.web_errors.contains_key(&keys[0]),
        "a miss is not an error"
    );
    // ...and `R` still asks for it.
    app.start_web_renders(capability::Trigger::Manual);
    let (_, reqs) = render_job(app.web_jobs_rx.as_ref().unwrap().recv().unwrap());
    assert!(reqs.iter().any(|r| r.cache_key() == keys[0]));
}

#[test]
fn no_sid_on_a_private_project_serves_the_cache_and_never_launches_a_browser() {
    let mut app = web_tier_page();
    app.editable = true; // a PAT / service account is reading this page
    app.caps = capability::Capabilities {
        sid: false,
        visibility: capability::Visibility::Private,
        ..Default::default()
    };
    app.render_policy = capability::RenderPolicy::Image;
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
    assert!(
        app.images.contains_key(&keys[0]),
        "the cached diagram still shows"
    );
    // The uncached one is a miss, NOT a failure: it stays source and
    // stays drawable.
    assert!(app.web_missing.contains(&keys[1]));
    assert!(!app.web_errors.contains_key(&keys[1]));
    assert!(app.web_pending.is_empty(), "nothing is left pulsing");
    assert!(app.hint_text(&[]).contains("connect.sid"));
    assert!(app.editable, "the renderer's limits never reach the editor");
}

#[test]
fn a_stale_cookie_on_a_public_page_retries_without_it() {
    let mut app = mermaid_page();
    app.caps = capability::Capabilities {
        sid: true,
        visibility: capability::Visibility::Public,
        ..Default::default()
    };
    app.render_policy = capability::RenderPolicy::Image;
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
    assert!(
        !app.caps.browser_denied,
        "no anonymous attempt has been made yet"
    );
    let job = app.web_jobs_rx.as_ref().unwrap().recv().unwrap();
    let WebJob::Render { reqs, auth, .. } = &job else {
        panic!("expected a render job")
    };
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
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
        .unwrap();
    let req_b = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->C",
            3,
        )
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
    assert!(
        cache.get(&key_b).is_some(),
        "the current source is cached normally"
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
    app.render_policy = capability::RenderPolicy::Image;
    app.rebuild(80);
    let keys = diagram_keys(&app);
    assert_eq!(
        keys.len(),
        2,
        "this test needs two diagrams to mean anything"
    );
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
    let WebJob::Render { reqs, auth, .. } = &job else {
        panic!("expected a render job")
    };
    assert_eq!(*auth, Some(RenderCapability::Anonymous));
    let retried: Vec<String> = reqs.iter().map(|r| r.cache_key()).collect();
    for k in &keys {
        assert!(
            retried.contains(k),
            "every diagram the batch lost is retried"
        );
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
    assert!(
        app.web_jobs_rx.as_ref().unwrap().try_recv().is_err(),
        "no third attempt"
    );
    // ...and none of it is a diagram failure.
    assert!(app.web_errors.is_empty());
}

#[test]
fn a_refused_private_render_falls_back_to_source_without_disabling_edits() {
    let mut app = mermaid_page();
    // lib spike: web経路の検証なのでテキスト段を止める。
    app.mermaid_text = false;
    app.editable = true;
    app.caps = capability::Capabilities {
        sid: true,
        visibility: capability::Visibility::Private,
        ..Default::default()
    };
    app.render_policy = capability::RenderPolicy::Image;
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
fn the_ui_thread_never_waits_for_the_browser() {
    let backend = Arc::new(cosense::webrender::FakeBackend::new());
    let mut app = web_tier_page();
    app.rebuild(80);
    let key = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
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
    assert!(
        app.images.is_empty(),
        "nothing is drawn until the browser answers"
    );
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
fn the_worker_joins_on_stop_and_answers_every_accepted_job() {
    let backend = Arc::new(cosense::webrender::FakeBackend::new());
    let mut app = mermaid_page();
    app.rebuild(80);
    let key = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
        .unwrap()
        .cache_key();
    backend.answer(&key, Ok(tiny_png()));
    let gen = app.gen_now();
    let req = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
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
            WebJob::Rescale {
                gen,
                key: "web:mermaid:absent".into(),
                max_cols: 20,
            },
        ],
        2,
    );
    assert_eq!(msgs.len(), 2, "one reply per accepted job");
    let render = msgs.iter().find(|m| m.key == key).unwrap();
    assert!(!render.rescale && render.res.is_drawn());
    let rescale = msgs.iter().find(|m| m.key == "web:mermaid:absent").unwrap();
    assert!(
        rescale.rescale && !rescale.res.is_drawn(),
        "a cache miss is answered, not dropped"
    );
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
    assert!(
        !app.web_rescaling.contains(&key),
        "it stops being in flight"
    );
    assert!(
        app.images.contains_key(&key),
        "the picture on screen survives"
    );
    assert!(
        app.web_errors.is_empty(),
        "a resize failure is not a diagram failure"
    );
    assert!(app.note.is_none(), "and the reader is not told about it");
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
    app.web_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    app.web_pending.insert(key.clone());
    app.web_rescaling.insert(key.clone());
    let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
    app.web_tx
        .send(WebMsg {
            gen: old_gen,
            key: key.clone(),
            rescale: false,
            attempted: None,
            res: WebOutcome::Drawn(info),
        })
        .unwrap();
    assert!(
        !app.drain_web_renders(),
        "yesterday's answer changes nothing"
    );
    assert!(
        app.web_pending.contains(&key),
        "the live request is still in flight"
    );
    assert!(app.web_rescaling.contains(&key));
    assert!(
        !app.images.contains_key(&key),
        "and no stale picture is installed"
    );
}

#[test]
fn work_queued_for_a_page_the_reader_left_never_reaches_the_browser() {
    let backend = Arc::new(cosense::webrender::FakeBackend::new());
    let mut app = mermaid_page();
    app.rebuild(80);
    let stale_gen = app.gen_now();
    // The reader moves on before the worker gets to it.
    app.web_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let live_gen = app.gen_now();
    let req = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
        .unwrap();
    backend.answer(&req.cache_key(), Ok(tiny_png()));
    let msgs = worker_replies(
        &mut app,
        Arc::clone(&backend),
        scratch_cache(),
        vec![
            WebJob::Render {
                gen: stale_gen,
                src_epoch: 0,
                reqs: vec![req.clone()],
                max_cols: 64,
                auth: Some(RenderCapability::Authenticated),
            },
            WebJob::Rescale {
                gen: stale_gen,
                key: "web:mermaid:old".into(),
                max_cols: 20,
            },
            WebJob::Render {
                gen: live_gen,
                src_epoch: 0,
                reqs: vec![req.clone()],
                max_cols: 64,
                auth: Some(RenderCapability::Authenticated),
            },
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
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
        .unwrap();
    let b = app
        .web_request(cosense::webrender::WebKind::Mermaid, "pie", 7)
        .unwrap();
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
        .send(WebJob::Render {
            gen,
            src_epoch: 0,
            reqs: vec![a.clone()],
            max_cols: 64,
            auth: Some(RenderCapability::Authenticated),
        })
        .unwrap();
    app.web_job_tx
        .send(WebJob::Render {
            gen,
            src_epoch: 0,
            reqs: vec![b.clone(), a.clone()],
            max_cols: 64,
            auth: Some(RenderCapability::Authenticated),
        })
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
    assert_eq!(
        keys, want,
        "each request answered exactly once, no duplicates"
    );
    assert_eq!(
        backend.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "and the whole page cost ONE browser batch, which is the point",
    );
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
    assert_eq!(
        app.web_cols, IMAGE_MAX_COLS,
        "80 columns leaves room for the ceiling"
    );
    app.web_pending.insert(key.clone());
    app.web_tx
        .send(WebMsg {
            gen: app.gen_now(),
            key: key.clone(),
            rescale: false,
            attempted: None,
            res: WebOutcome::Drawn(wide),
        })
        .unwrap();
    assert!(app.drain_web_renders());

    // Two narrow panes, well under the 64-column image ceiling.
    for (cols, want_cap) in [(40u16, 34u16), (20, 14)] {
        app.laid_width = 0;
        app.rebuild(cols);
        assert_eq!(app.web_cols, want_cap, "text width at {cols} columns");
        app.rescale_diagrams();
        // The UI thread only queued work; nothing was decoded here.
        let job = app
            .web_jobs_rx
            .as_ref()
            .unwrap()
            .try_recv()
            .expect("a rescale was queued");
        let WebJob::Rescale {
            key: k,
            max_cols,
            gen,
        } = job
        else {
            panic!("a resize must never send the browser a render job")
        };
        assert_eq!(
            (k.as_str(), max_cols, gen),
            (key.as_str(), want_cap, app.gen_now())
        );
        // Asking again while it is in flight queues nothing.
        app.rescale_diagrams();
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());

        // The worker answers; the diagram now fits the pane.
        let info = decode_web_png(&Picker::halfblocks(), &wide_png(), max_cols).unwrap();
        app.web_tx
            .send(WebMsg {
                gen: app.gen_now(),
                key: key.clone(),
                rescale: true,
                attempted: None,
                res: WebOutcome::Drawn(info),
            })
            .unwrap();
        assert!(app.drain_web_renders());
        let shown_w = app.images[&key].cells_w;
        assert!(
            shown_w <= want_cap,
            "{shown_w} cells in a {cols}-column pane"
        );

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
    assert!(
        app.hint_text(&[]).contains("ソースを表示します"),
        "the reader is told once"
    );
    // It holds the line for its few seconds…
    assert!(!app.expire_note());
    // …and then hands the row back to the key hints.
    let (msg, _) = app.note.clone().unwrap();
    app.note = Some((msg, std::time::Instant::now()));
    assert!(app.expire_note());
    assert!(app.note.is_none());
    assert!(
        app.hint_text(&[]).contains("? ヘルプ"),
        "hints are visible again"
    );
    assert!(!app.expire_note(), "and it stays gone");
}
