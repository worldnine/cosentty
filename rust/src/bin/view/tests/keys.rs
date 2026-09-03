use crate::*;
use super::support::*;

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
        assert!(app.toast_text().contains("兄弟ブロック"), "status: {}", app.toast_text());
        assert!(app.move_mode.is_some());
        // While `j` from the same spot simply goes.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert_eq!(texts(&app), vec!["title", "b", " a", " b-child", "c"]);

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
        assert!(app.toast_text().contains("つかむ前の並び"), "status: {}", app.toast_text());
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
        assert!(app.toast_text().contains("貼り付けは編集中に"), "status: {}", app.toast_text());
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
        assert!(app.toast_text().contains("選択中"), "toast: {}", app.toast_text());
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

    /// READ の Tab はブラウザの Tab: リンクのある行へ巡回する。端では
    /// 折り返し、Shift+Tab は逆回り。リンクが1つも無いページでは動かず、
    /// ソース表示の引っ越し先(`s`)を教える。
    #[test]
    fn tab_cycles_through_link_lines_and_teaches_s_when_there_are_none() {
        let ctx = test_ctx();
        let mut app = page(&["title", "plain", "see [alpha]", "plain2", "[beta] too"]);
        app.rebuild(40);
        app.cursor = 0;

        handle_key(&mut app, &ctx, key(KeyCode::Tab));
        assert_eq!(app.cursor, 2, "Tab lands on the first link line");
        handle_key(&mut app, &ctx, key(KeyCode::Tab));
        assert_eq!(app.cursor, 4);
        handle_key(&mut app, &ctx, key(KeyCode::Tab));
        assert_eq!(app.cursor, 2, "and wraps around");
        handle_key(&mut app, &ctx, key(KeyCode::BackTab));
        assert_eq!(app.cursor, 4, "Shift+Tab goes the other way");

        let mut bare = page(&["title", "plain", "still plain"]);
        bare.rebuild(40);
        bare.cursor = 1;
        handle_key(&mut bare, &ctx, key(KeyCode::Tab));
        assert_eq!(bare.cursor, 1, "nowhere to go");
        assert!(bare.note_text().contains("ソース表示は s"), "note: {}", bare.note_text());
    }

    /// ソース表示はモードから表示オプションへ降格: `s` でトグルし、
    /// Tab はもうモードを切り替えない。
    #[test]
    fn s_toggles_the_raw_source_view_instead_of_tab() {
        let ctx = test_ctx();
        let mut app = page(&["title", "body"]);
        app.rebuild(40);

        handle_key(&mut app, &ctx, key(KeyCode::Char('s')));
        assert_eq!(app.mode, Mode::Source);
        assert!(app.toast_text().is_empty(), "the footer badge says SRC; no toast");
        handle_key(&mut app, &ctx, key(KeyCode::Char('s')));
        assert_eq!(app.mode, Mode::View);

        // Tab はリンク巡回であってモード切替ではない。
        handle_key(&mut app, &ctx, key(KeyCode::Tab));
        assert_eq!(app.mode, Mode::View, "Tab no longer toggles source");
    }
