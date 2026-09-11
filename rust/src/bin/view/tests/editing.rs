use super::support::*;
use crate::*;

#[test]
fn edit_delete_restore_keeps_older_history_across_redo_cycles() {
    let ctx = test_ctx();
    let mut app = page(&["title", "body", ""]);
    do_edit(
        &mut app,
        &ctx,
        "edit",
        vec![EditOp::Replace {
            id: "id1".into(),
            text: "changed".into(),
        }],
    );
    do_edit(
        &mut app,
        &ctx,
        "delete",
        vec![EditOp::Delete { id: "id1".into() }],
    );
    for _ in 0..3 {
        undo(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "changed");
        assert_ne!(app.lines[1].id, "id1");
        undo(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "body");
        redo(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "changed");
        redo(&mut app, &ctx);
        assert_eq!(app.lines.len(), 2);
    }
}

#[test]
fn stale_history_is_not_applied_or_queued() {
    let ctx = test_ctx();
    for back in [true, false] {
        for op in [
            EditOp::Delete {
                id: "missing".into(),
            },
            EditOp::Replace {
                id: "missing".into(),
                text: "changed".into(),
            },
            EditOp::Insert {
                anchor: "missing".into(),
                lines: vec![("fresh".into(), "restored".into())],
            },
        ] {
            let mut app = page(&["title", "body"]);
            let entry = ("stale".into(), vec![op]);
            if back {
                app.undo_stack.push(entry);
            } else {
                app.redo_stack.push(entry);
            }
            let next_job = app.next_job_id;
            assert!(!if back {
                undo(&mut app, &ctx)
            } else {
                redo(&mut app, &ctx)
            });
            assert_eq!(app.next_job_id, next_job);
            assert_eq!(app.lines.len(), 2);
            assert_eq!(app.lines[1].text, "body");
            assert_eq!(app.undo_stack.len() + app.redo_stack.len(), 1);
        }
    }
}

#[test]
fn a_conflict_whose_reload_fails_stops_rendering_until_it_is_resolved() {
    // 409 means our ops did not land: local and server disagree. If the
    // recovery fetch then fails too, we are STILL divergent — rendering
    // would screenshot the server's text and file it under ours.
    let ctx = offline_ctx();
    let mut app = mermaid_page();
    app.render_policy = capability::RenderPolicy::Image;
    app.rebuild(80);
    app.inflight = 1;
    handle_commit_outcome(
        &mut app,
        &ctx,
        CommitOutcome::Conflict { job: UNRELATED_JOB },
    );
    assert!(app.web_unsynced, "a failed recovery leaves us divergent");
    assert!(
        app.toast_text().contains("読み直しに失敗"),
        "the real reason is not overwritten: {}",
        app.toast_text()
    );
    while app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok() {}
    assert!(!app.start_web_renders(capability::Trigger::Manual));
    assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    assert!(app.web_pending.is_empty());
}

#[test]
fn a_render_whose_source_moved_is_never_filed_under_the_old_hash() {
    // A page of diagrams takes seconds to draw. If a commit lands in
    // that window, the browser screenshots the NEW text — but the job
    // is keyed on the old hash, and the artifact cache keeps what it is
    // given for a week. The next reader of the old text would be served
    // a picture of something else.
    let ctx = test_ctx();
    let mut app = web_tier_page();
    app.render_policy = capability::RenderPolicy::Image;
    app.rebuild(80);
    let key_a = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->B",
            3,
        )
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
    assert!(
        !app.web_pending.contains(&key_a),
        "the abandoned key stopped pulsing"
    );

    // ...and B is what gets asked for instead.
    let key_b = app
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->C",
            3,
        )
        .unwrap()
        .cache_key();
    app.start_web_renders(capability::Trigger::Auto);
    assert!(
        app.web_pending.contains(&key_b),
        "the new source is what gets rendered next"
    );
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
        .web_request(
            cosense::webrender::WebKind::Mermaid,
            "flowchart LR\n  A-->C",
            3,
        )
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
        CommitOutcome::Failed {
            job: UNRELATED_JOB,
            structural: false,
            label: "line 4".into(),
            msg: "500".into(),
        },
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
    assert!(
        app.web_unsynced,
        "one success does not prove the page agrees"
    );
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
    install_remote_lines(&mut app, &ctx, &page, Some("⟳ resync"));
    assert!(
        !app.web_unsynced,
        "an authoritative install resolves the drift"
    );
    app.rebuild(80);
    app.start_web_renders(capability::Trigger::Auto);
    assert!(
        app.web_jobs_rx.as_ref().unwrap().try_recv().is_ok(),
        "rendering resumes"
    );
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
    assert!(
        app.move_mode.is_some(),
        "the arrow drags, it does not let go"
    );
    assert!(drain_jobs(&mut app).is_empty(), "still local");

    handle_key(&mut app, &ctx, key(KeyCode::Right));
    assert_eq!(texts(&app), vec!["title", "two", " one", "three"]);
    handle_key(&mut app, &ctx, key(KeyCode::Left));
    handle_key(&mut app, &ctx, key(KeyCode::Up));
    assert_eq!(
        texts(&app),
        vec!["title", "one", "two", "three"],
        "back home"
    );
    assert!(app.move_mode.is_some());

    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none(), "m lets go");
    assert!(
        drain_jobs(&mut app).is_empty(),
        "nothing changed, nothing sent"
    );
    assert_eq!(app.cursor, 1, "the cursor stayed on the block");
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
    // Leaving the mode with nothing changed is visible (the MOVE hint
    // goes); it is not worth a word.
    assert!(
        app.toast_text().is_empty() && app.note_text().is_empty(),
        "toast: {} / note: {}",
        app.toast_text(),
        app.note_text()
    );
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
                structural: false,
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
            app.toast_text().contains("アウトライン操作に失敗"),
            "status: {}",
            app.toast_text()
        );
    }
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

    assert!(
        app.move_mode.is_none(),
        "the drag committed before anything moved"
    );
    assert_eq!(texts(&app), vec!["title", " b", " a"]);
    assert_eq!(drain_jobs(&mut app).len(), 1);
    assert!(app.outline_pending.is_some());
    finish_outline(&mut app, &ctx);
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
        vec![EditOp::Replace {
            id: "id0".into(),
            text: "title".into(),
        }],
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
            structural: false,
            label: "earlier edit".into(),
            msg: "500".into(),
        },
    );
    assert!(
        app.web_unsynced,
        "the unrelated failure still marks the page divergent"
    );
    assert!(
        app.toast_text().contains("コミットに失敗"),
        "the unrelated failure keeps its own status: {}",
        app.toast_text()
    );
    assert!(
        app.outline_pending.is_some(),
        "and it does not touch the gate"
    );
    assert_eq!(
        app.lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>(),
        optimistic
    );

    handle_commit_outcome(&mut app, &ctx, CommitOutcome::Conflict { job: outline });
    assert!(app.outline_pending.is_none());
    assert_eq!(
        app.lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>(),
        original,
        "the rejected action is rolled back to the pre-action page"
    );
    assert!(
        app.toast_text().contains("アウトライン操作に失敗"),
        "status: {}",
        app.toast_text()
    );
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
        vec![EditOp::Replace {
            id: "id0".into(),
            text: "title".into(),
        }],
    )
    .expect("the worker took the ordinary job");
    drain_jobs(&mut app);

    assert!(
        undo(&mut app, &ctx),
        "structural history no longer waits at the door"
    );
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
    let undone: Vec<(String, String)> = app
        .lines
        .iter()
        .map(|line| (line.id.clone(), line.text.clone()))
        .collect();
    let stacks = (app.undo_stack.clone(), app.redo_stack.clone());
    assert!(
        !redo(&mut app, &ctx),
        "the rapid opposite action is blocked"
    );
    let edit_id = app.lines[1].id.clone();
    do_edit(
        &mut app,
        &ctx,
        "rapid edit",
        vec![EditOp::Replace {
            id: edit_id,
            text: "changed".into(),
        }],
    );
    assert_eq!(
        app.lines
            .iter()
            .map(|line| (line.id.clone(), line.text.clone()))
            .collect::<Vec<_>>(),
        undone,
        "other mutations are blocked too"
    );
    assert_eq!((app.undo_stack.clone(), app.redo_stack.clone()), stacks);
    assert_eq!(
        drain_jobs(&mut app).len(),
        1,
        "only the structural undo was queued"
    );
}

#[test]
fn history_recovery_validates_dependent_entries_in_pop_order() {
    let app = page(&["title", "body"]);
    let mut stack = vec![
        (
            "older delete".into(),
            vec![EditOp::Delete {
                id: "restored-id".into(),
            }],
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
    assert_eq!(
        stack.len(),
        2,
        "the newer undo recreates the id needed by the older one"
    );
}

#[test]
fn undo_redo_roundtrip_with_queue() {
    let ctx = test_ctx();
    let mut app = page(&["title", "one", "two"]);
    app.rebuild(40);
    // x-style delete of line 1
    do_edit(
        &mut app,
        &ctx,
        "delete",
        vec![EditOp::Delete { id: "id1".into() }],
    );
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
    assert_eq!(
        changed_end("あい", "あうえい"),
        "あうえ".len(),
        "utf-8 safe"
    );

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
    assert_eq!(
        s.input.cur, 5,
        "redo: after what came back — NOT the line start"
    );

    // From READ, the cursor follows the change too.
    leave_session(&mut app, &ctx);
    app.cursor = 0;
    handle_key(&mut app, &ctx, ctrl('z'));
    assert_eq!(app.cursor, 1, "the cursor went to the line that changed");
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
        matches!(
            &jobs[0].1[..],
            [
                EditOp::Replace { .. },
                EditOp::Replace { .. },
                EditOp::Delete { .. }
            ]
        ),
        "ops: {:?}",
        jobs[0].1
    );
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
    assert!(
        drain_jobs(&mut app).is_empty(),
        "nothing can be committed yet"
    );
    assert_eq!(
        app.create_state,
        CreateState::Needed,
        "…but the page owes the server its existence"
    );

    dispatch_create(&mut app);
    let jobs = drain_jobs(&mut app);
    assert_eq!(jobs.len(), 1, "one create, whatever was typed");
    assert_eq!(jobs[0].0, "ページの作成");
    match &jobs[0].1[0] {
        EditOp::Insert { anchor, lines } => {
            assert_eq!(anchor, "_end");
            let texts: Vec<&str> = lines.iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(
                texts,
                vec!["new title", "body"],
                "title first, then the body"
            );
        }
        other => panic!("expected an insert, got {other:?}"),
    }

    // A SECOND create would make a second page (same title, auto-
    // suffixed), and the writing would split between them.
    assert_eq!(app.create_state, CreateState::Sent);
    dispatch_create(&mut app);
    assert!(
        drain_jobs(&mut app).is_empty(),
        "not while one is in flight"
    );

    // Even after it lands, further typing waits for the page to come
    // back with an id instead of creating a second one.
    handle_commit_outcome(
        &mut app,
        &ctx,
        CommitOutcome::Done {
            job: UNRELATED_JOB,
            label: "ページの作成".into(),
            title: "new title".into(),
            commit_id: String::new(),
        },
    );
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

/// A create that failed leaves the page uncreated: the next edit must
/// be able to try again, without retrying in a loop meanwhile.
#[test]
fn a_failed_create_can_be_retried_by_the_next_edit() {
    let ctx = test_ctx();
    let mut app = page(&["new title", "body"]);
    app.title = "new title".into();
    app.page_id = String::new();
    app.rebuild(40);
    do_edit(
        &mut app,
        &ctx,
        "edit",
        vec![EditOp::Replace {
            id: "id1".into(),
            text: "body!".into(),
        }],
    );
    dispatch_create(&mut app);
    drain_jobs(&mut app);

    handle_commit_outcome(
        &mut app,
        &ctx,
        CommitOutcome::Failed {
            job: UNRELATED_JOB,
            structural: false,
            label: "ページの作成".into(),
            msg: "500".into(),
        },
    );
    assert_eq!(app.create_state, CreateState::Idle, "not stuck as sent");
    dispatch_create(&mut app);
    assert!(
        drain_jobs(&mut app).is_empty(),
        "and not retrying by itself"
    );

    do_edit(
        &mut app,
        &ctx,
        "edit",
        vec![EditOp::Replace {
            id: "id1".into(),
            text: "body!!".into(),
        }],
    );
    dispatch_create(&mut app);
    assert_eq!(drain_jobs(&mut app).len(), 1, "the next edit tries again");
}

/// A web-side edit elsewhere on the page must not take the redo stack
/// with it. Wiping the whole lineage on every remote refresh is what
/// made `^r` look like a dead key: with fallback polling, a single edit in
/// the browser was enough to silently empty it.
#[test]
fn a_remote_edit_keeps_the_history_it_can_still_replay() {
    let ctx = test_ctx();
    let mut app = page(&["title", "one", "two"]);
    app.rebuild(40);
    do_edit(
        &mut app,
        &ctx,
        "line 2",
        vec![EditOp::Replace {
            id: "id1".into(),
            text: "ONE".into(),
        }],
    );
    undo(&mut app, &ctx);
    assert_eq!(app.redo_stack.len(), 1);

    // Someone edits an UNRELATED line in the browser.
    let remote = polled(&[("id0", "title"), ("id1", "one"), ("id2", "TWO")]);
    install_remote_lines(&mut app, &ctx, &remote.page, Some("⟳ remote"));
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
    do_edit(
        &mut app,
        &ctx,
        "line 2",
        vec![EditOp::Replace {
            id: "id1".into(),
            text: "ONE".into(),
        }],
    );
    undo(&mut app, &ctx);

    // The browser deletes that very line.
    let remote = polled(&[("id0", "title"), ("id2", "two")]);
    install_remote_lines(&mut app, &ctx, &remote.page, Some("⟳ remote"));
    assert!(app.redo_stack.is_empty());
    assert!(app.history_dropped);

    redo(&mut app, &ctx);
    assert!(
        app.toast_text().contains("失効"),
        "toast: {}",
        app.toast_text()
    );

    // A new edit starts a clean lineage, and the excuse expires with it.
    do_edit(
        &mut app,
        &ctx,
        "line 3",
        vec![EditOp::Replace {
            id: "id2".into(),
            text: "x".into(),
        }],
    );
    assert!(!app.history_dropped);
    redo(&mut app, &ctx);
    assert_eq!(app.toast_text(), "やり直せる編集がありません");
}

/// The undo reflex must not depend on which mode you are in: `^z`
/// works in READ as well, next to the akapen `u` seat.
#[test]
fn read_mode_takes_ctrl_z_as_undo_too() {
    let ctx = test_ctx();
    let mut app = page(&["title", "one", "two"]);
    app.rebuild(40);
    do_edit(
        &mut app,
        &ctx,
        "delete",
        vec![EditOp::Delete { id: "id1".into() }],
    );
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
    assert_eq!(
        app.lines[1].text, "one",
        "the dirty text committed, then undid"
    );
    let s = app.session.as_ref().expect("still editing");
    assert_eq!(s.line, 1);
    assert_eq!(
        s.input.buf, "one",
        "the caret line reloaded from the undone state"
    );
    assert_eq!(s.orig, "one", "and is clean again");

    handle_session_key(&mut app, &ctx, ctrl('r'));
    assert_eq!(app.lines[1].text, "one!!", "^r redoes without leaving EDIT");
    assert_eq!(app.session.as_ref().unwrap().input.buf, "one!!");

    let labels: Vec<String> = drain_jobs(&mut app).into_iter().map(|j| j.0).collect();
    assert_eq!(
        labels.len(),
        4,
        "each keystroke, then undo and redo, committed: {labels:?}"
    );
    assert!(labels[2].starts_with("undo"));
    assert!(labels[3].starts_with("redo"));
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
    assert_eq!(
        s.input.cur,
        s.input.buf.len(),
        "at its end, as an editor would"
    );
    assert_eq!(s.input.buf, "one");

    // Nothing left to undo: the session survives that too.
    handle_session_key(&mut app, &ctx, ctrl('z'));
    assert!(app.session.is_some());
    assert_eq!(app.toast_text(), "取り消せる編集がありません");
}

/// A failed save of a plain replace must not stop the saves behind it:
/// typing keeps saving after a network blip. Only a failed INSERT/DELETE
/// holds the queue, because the jobs behind it name lines the server
/// never got — and that hold ends when the UI re-bases the page (`gen`).
#[test]
fn only_a_failed_structural_save_holds_the_jobs_behind_it() {
    let ctx = offline_ctx();
    let mut app = page(&["title", "one", "two"]);
    let jobs_rx = app.commit_jobs_rx.take().unwrap();
    let (res_tx, res_rx) = mpsc::channel();
    spawn_commit_worker(
        ctx.client.clone(),
        jobs_rx,
        res_tx,
        Arc::clone(&app.gen),
        Arc::clone(&app.live_superseded),
    );
    let replace = |text: &str| {
        vec![EditOp::Replace {
            id: "id1".into(),
            text: text.into(),
        }]
    };
    let wait = || std::time::Duration::from_secs(30);
    let j1 = queue_commit(&mut app, "line 2", replace("a")).unwrap();
    let j2 = queue_commit(&mut app, "line 2", replace("ab")).unwrap();
    for expected in [j1, j2] {
        match res_rx.recv_timeout(wait()).expect("answered") {
            CommitOutcome::Failed {
                job, structural, ..
            } => {
                assert_eq!(job, expected);
                assert!(!structural);
            }
            _ => panic!("a replace that fails is reported as failed, not skipped"),
        }
    }
    // A failed split holds the next job; a new gen releases the one after.
    let j3 = queue_commit(
        &mut app,
        "split",
        vec![
            EditOp::Replace {
                id: "id1".into(),
                text: "o".into(),
            },
            EditOp::insert("id2", "ne"),
        ],
    )
    .unwrap();
    let j4 = queue_commit(&mut app, "line 2", replace("x")).unwrap();
    app.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let j5 = queue_commit(&mut app, "line 2", replace("y")).unwrap();
    match res_rx.recv_timeout(wait()).expect("answered") {
        CommitOutcome::Failed {
            job, structural, ..
        } => {
            assert_eq!(job, j3);
            assert!(structural);
        }
        _ => panic!("the split fails"),
    }
    assert!(matches!(
        res_rx.recv_timeout(wait()).expect("answered"),
        CommitOutcome::Skipped { job } if job == j4
    ));
    assert!(matches!(
        res_rx.recv_timeout(wait()).expect("answered"),
        CommitOutcome::Failed { job, .. } if job == j5
    ));
}

/// The UI's half of the same story: a failed structural save re-bases the
/// page on the server's copy (new `gen`, reload), which is what releases
/// the worker's hold. A failed replace only reports and marks desynced.
#[test]
fn a_failed_structural_save_rebases_the_page() {
    let ctx = offline_ctx();
    let mut app = page(&["title", "one"]);
    app.rebuild(40);
    let gen0 = app.gen.load(std::sync::atomic::Ordering::SeqCst);
    app.inflight = 2;
    handle_commit_outcome(
        &mut app,
        &ctx,
        CommitOutcome::Failed {
            job: UNRELATED_JOB,
            structural: false,
            label: "line 2".into(),
            msg: "boom".into(),
        },
    );
    assert_eq!(
        app.gen.load(std::sync::atomic::Ordering::SeqCst),
        gen0,
        "a failed replace leaves the queue alone"
    );
    assert!(app.web_unsynced);
    handle_commit_outcome(
        &mut app,
        &ctx,
        CommitOutcome::Failed {
            job: UNRELATED_JOB,
            structural: true,
            label: "split".into(),
            msg: "boom".into(),
        },
    );
    assert_eq!(
        app.gen.load(std::sync::atomic::Ordering::SeqCst),
        gen0 + 1,
        "a failed split invalidates the jobs built on it and reloads"
    );
    assert!(
        app.toast_text().contains("読み直しに失敗"),
        "offline, the reload says so: {}",
        app.toast_text()
    );
}
