use super::support::*;
use crate::*;

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
    assert!(app.toast_text().contains("タイトル行"));
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
    assert!(app.toast_text().contains("選択中"));
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
        app.toast_text().contains("兄弟ブロック"),
        "status: {}",
        app.toast_text()
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
    assert!(
        app.toast_text().contains("タイトル行"),
        "status: {}",
        app.toast_text()
    );

    // A related row below the body is not a source line.
    let mut app = page(&["title", " one"]);
    app.cursor = app.lines.len();
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("ソース行"),
        "status: {}",
        app.toast_text()
    );

    // A page that does not exist yet has nothing to commit against.
    let mut app = page_uncreated(&["title", " one"]);
    app.cursor = 1;
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("未作成"),
        "status: {}",
        app.toast_text()
    );

    let mut app = page(&["title", " one"]);
    app.cursor = 1;
    in_history(&mut app);
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("履歴を表示中"),
        "status: {}",
        app.toast_text()
    );

    let mut app = page(&["title", " one"]);
    app.cursor = 1;
    app.editable = false;
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("編集権限"),
        "status: {}",
        app.toast_text()
    );

    // One structural action at a time: the outstanding one has the only
    // snapshot to roll back to.
    let mut app = page(&["title", " one", " two"]);
    app.cursor = 1;
    handle_key(
        &mut app,
        &ctx,
        modified(KeyCode::Down, KeyModifiers::CONTROL),
    );
    assert!(app.outline_pending.is_some());
    drain_jobs(&mut app);
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("完了待ち"),
        "status: {}",
        app.toast_text()
    );
    assert!(drain_jobs(&mut app).is_empty());

    // A landed action whose ids we no longer trust: reopen the page.
    let mut app = page(&["title", " one"]);
    app.cursor = 1;
    app.outline_refresh_needed = true;
    handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
    assert!(app.move_mode.is_none());
    assert!(
        app.toast_text().contains("開き直して"),
        "status: {}",
        app.toast_text()
    );
    assert!(drain_jobs(&mut app).is_empty());
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
    assert!(
        footer.contains("j/k") && footer.contains("J/K"),
        "footer: {footer}"
    );
    assert!(
        footer.contains("h/l") && footer.contains("Esc"),
        "footer: {footer}"
    );
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
    assert!(
        drain_jobs(&mut app).is_empty(),
        "and nothing left the machine"
    );

    // While the block is in hand, other mutations wait their turn.
    let id = app.lines[1].id.clone();
    do_edit(
        &mut app,
        &ctx,
        "rapid edit",
        vec![EditOp::Replace {
            id,
            text: "changed".into(),
        }],
    );
    assert_eq!(app.lines[1].text, " parent");
    assert!(
        app.toast_text().contains("移動モード中"),
        "status: {}",
        app.toast_text()
    );
    assert!(drain_jobs(&mut app).is_empty());
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
    assert_eq!(
        move_block_range(&app),
        Some((3, 4)),
        "still the same two lines"
    );
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
            assert_eq!(
                app.session.as_ref().map(|s| s.line),
                Some(2),
                "the caret line"
            );
            assert_eq!(
                app.session.as_ref().map(|s| s.input.cur),
                Some(1),
                "and the caret"
            );
            assert!(
                app.session.as_ref().unwrap().sel_from.is_none(),
                "and no selection"
            );
            assert!(
                app.toast_text().contains("移動モード"),
                "status: {}",
                app.toast_text()
            );
        }
    }
    assert!(drain_jobs(&mut app).is_empty());

    // Shift keeps its own meaning: it selects — across the line now.
    handle_session_key(&mut app, &ctx, modified(KeyCode::Up, KeyModifiers::SHIFT));
    let s = app.session.as_ref().unwrap();
    assert_eq!(s.line, 1, "Shift+Up carried the caret");
    assert_eq!(
        s.sel_from,
        Some((2, 1)),
        "and dragged the range's active end with it"
    );
    assert!(app.selection.is_none(), "no line band in EDIT anymore");
    assert!(app.session.is_some(), "and EDIT is untouched throughout");
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
    assert_eq!(
        move_block_range(&app),
        Some((1, 2)),
        "the grab is A and its child"
    );

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
    assert_eq!(
        texts(&app),
        vec!["title", "B", "A", " A1", " B1"],
        "and back"
    );

    // `J` from the start would have cleared B's whole subtree at once.
    handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
    assert_eq!(texts(&app), vec!["title", "A", " A1", "B", " B1"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char('J')));
    assert_eq!(texts(&app), vec!["title", "B", " B1", "A", " A1"]);

    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert_eq!(
        drain_jobs(&mut app).len(),
        1,
        "one commit for the whole drag"
    );
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
    let current_lines: Vec<(String, String)> = app
        .lines
        .iter()
        .map(|line| (line.id.clone(), line.text.clone()))
        .collect();
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
        app.lines
            .iter()
            .map(|line| (line.id.clone(), line.text.clone()))
            .collect::<Vec<_>>(),
        current_lines
    );
    assert_eq!(app.status, "current page status");
    assert_eq!(
        app.own_commits.iter().cloned().collect::<Vec<_>>(),
        vec!["current-echo"]
    );
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
    app.web_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    assert_eq!(
        app.title, "current title",
        "the old result cannot rename this visit"
    );
    assert_eq!(
        app.own_commits.iter().cloned().collect::<Vec<_>>(),
        vec!["current-echo"],
        "a failed refresh cannot file an old echo under the new visit"
    );
    assert!(app.server_epoch_now() > epoch);
    assert!(
        app.web_unsynced,
        "a failed refresh leaves the stale visit read-only"
    );
    assert!(app.outline_refresh_needed);
    assert!(
        app.toast_text().contains("失敗") || app.toast_text().contains("failed"),
        "the same-page stale visit attempted an authoritative refresh: {}",
        app.toast_text()
    );

    // The successful commit may have replaced the IDs we still display.
    // No ordinary edit is allowed to target those stale IDs.
    drain_jobs(&mut app);
    let before: Vec<(String, String)> = app
        .lines
        .iter()
        .map(|line| (line.id.clone(), line.text.clone()))
        .collect();
    let id = app.lines[1].id.clone();
    do_edit(
        &mut app,
        &ctx,
        "stale edit",
        vec![EditOp::Replace {
            id,
            text: "unsafe".into(),
        }],
    );
    assert_eq!(
        app.lines
            .iter()
            .map(|line| (line.id.clone(), line.text.clone()))
            .collect::<Vec<_>>(),
        before
    );
    assert!(drain_jobs(&mut app).is_empty());
    assert!(app.toast_text().contains("開き直して"));
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
        vec![EditOp::Replace {
            id: "id0".into(),
            text: "title".into(),
        }],
    )
    .expect("the worker took the ordinary job");

    handle_key(
        &mut app,
        &ctx,
        modified(KeyCode::Right, KeyModifiers::CONTROL),
    );
    assert_eq!(
        app.lines[1].text, " one",
        "the action starts under an in-flight commit"
    );
    let outline = outline_job(&app);
    assert_ne!(outline, earlier);
    assert_eq!(app.inflight, 2);
    assert_eq!(
        drain_jobs(&mut app).len(),
        2,
        "both jobs reached the serial worker"
    );

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
    assert!(
        !undo(&mut app, &ctx),
        "mutations stay blocked until the action lands"
    );

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
    assert!(
        app.outline_pending.is_none(),
        "its own outcome completes it"
    );
    assert_eq!(app.inflight, 0);
    assert!(
        undo(&mut app, &ctx),
        "the released gate lets history run again"
    );
    assert_eq!(app.lines[1].text, "one");
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
            app.toast_text().contains("アウトライン操作に失敗"),
            "status: {}",
            app.toast_text()
        );
        assert!(
            app.toast_text()
                .contains(if conflict { "競合" } else { "500" }),
            "status: {}",
            app.toast_text()
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
            app.selection = Some(Selection {
                anchor: 1,
                cursor: 2,
            });
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

            let before_lines: Vec<(String, String)> = app
                .lines
                .iter()
                .map(|line| (line.id.clone(), line.text.clone()))
                .collect();
            let before_cursor = app.cursor;
            let before_selection = app.selection;
            let before_undo = app.undo_stack.clone();
            let before_redo = app.redo_stack.clone();
            app.history_dropped = true;

            assert!(if back {
                undo(&mut app, &ctx)
            } else {
                redo(&mut app, &ctx)
            });
            assert!(
                app.outline_pending.is_some(),
                "move history installs the structural gate"
            );
            assert_eq!(drain_jobs(&mut app).len(), 1);
            let job = outline_job(&app);
            let outcome = if conflict {
                CommitOutcome::Conflict { job }
            } else {
                CommitOutcome::Failed {
                    job,
                    label: if back {
                        "undo outline".into()
                    } else {
                        "redo outline".into()
                    },
                    msg: "500".into(),
                }
            };
            handle_commit_outcome(&mut app, &ctx, outcome);

            assert_eq!(
                app.lines
                    .iter()
                    .map(|line| (line.id.clone(), line.text.clone()))
                    .collect::<Vec<_>>(),
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
fn successful_outline_recovery_restores_history_cursor_and_selection_by_id() {
    let ctx = test_ctx();
    let mut app = page(&["title", "one", "two", "three", "tail"]);
    app.cursor = 1;
    app.selection = Some(Selection {
        anchor: 3,
        cursor: 1,
    });
    app.undo_stack.push((
        "older undo".into(),
        vec![EditOp::Replace {
            id: "id2".into(),
            text: "old two".into(),
        }],
    ));
    app.redo_stack.push((
        "older redo".into(),
        vec![EditOp::Replace {
            id: "id4".into(),
            text: "new tail".into(),
        }],
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

    assert_eq!(
        app.cursor, 2,
        "the active line follows id1, not its old index"
    );
    assert_eq!(
        app.selection,
        Some(Selection {
            anchor: 4,
            cursor: 2
        }),
        "both directional endpoints follow their saved IDs"
    );
    assert_eq!(app.undo_stack.len(), 1);
    assert_eq!(app.undo_stack[0].0, "older undo");
    assert_eq!(app.redo_stack.len(), 1);
    assert_eq!(app.redo_stack[0].0, "older redo");
    assert!(app.history_dropped);
    assert!(app.toast_text().contains("サーバーの内容を読み直しました"));
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
    assert!(app.note_text().contains("取り消しました"));
    assert_eq!(app.cursor, 1, "the unknown second key was consumed");

    handle_key(&mut app, &ctx, ctrl('g'));
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(!app.outline_prefix);
    assert!(app.note_text().contains("取り消しました"));

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
    println!("status: {}", app.toast_text());
    let mut t = Terminal::new(TestBackend::new(72, 14)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            let cell = buf.cell((x, y)).unwrap();
            row.push_str(cell.symbol());
        }
        let sel_bg = selection_bg(ctx.terminal_bg());
        let held =
            (0..buf.area.width).any(|x| buf.cell((x, y)).map(|c| c.bg == sel_bg).unwrap_or(false));
        // "HELD" marks the rows carrying the grabbed block's highlight.
        println!("{}|{row}|", if held { "HELD" } else { "    " });
    }
}
