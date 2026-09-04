use crate::*;
use super::support::*;

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

    #[test]
    fn read_only_project_never_enters_or_applies_editing() {
        let ctx = test_ctx();
        let mut app = page(&["t", "body"]);
        app.rebuild(40);
        app.editable = false;

        enter_session(&mut app, &ctx, 1, 4);
        assert!(app.session.is_none());
        assert!(app.toast_text().contains("編集権限"));

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
        let span = cosense::render::CodeSpan { header: 0, header_indent: 0, mermaid_header: false };
        let disp = session_display(row, Some(span));
        assert!(disp.contains(TAB_MARK), "{disp:?}");
        assert_eq!(disp.chars().count(), row.chars().count() + 1, "one column each way");
        // …and the caret still maps through it.
        for byte in [1usize, 2, 3, 4] {
            let d = display_caret(row, byte, Some(span));
            assert_eq!(raw_caret_from_display(row, d, Some(span)), byte, "byte {byte}");
        }
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
            app.toast_text().contains("英数") || app.toast_text().contains("IME"),
            "status: {}",
            app.toast_text(),
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
        assert!(app.toast_text().contains("編集中"), "status: {}", app.toast_text());
        assert_eq!(app.lines.len(), 2, "and nothing is written");

        // The index filters by what you paste (first line only).
        app.index = Some(cosense::index::Index::new(Vec::new(), 0, cosense::index::SortKey::Updated));
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

    /// Blank code lines stack freely — Enter NEVER ejects the writer.
    /// The only way out is deleting the indent: the last one gone takes
    /// the membership with it, and the flush line is ordinary again.
    #[test]
    fn leaving_a_code_block_takes_deleting_the_indent() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", " a = 1"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 6); // end of " a = 1"

        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        assert_eq!(app.lines[3].text, " ");
        assert_eq!(app.lines[4].text, " ");
        assert_eq!(app.lines[5].text, " ", "a third blank is still blank code");
        assert!(app.line_in_code(5), "Enter never ejects");

        // ⌫ from the inherited position eats the indent one char at a
        // time. The last one out takes the block with it.
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        let s = app.session.as_ref().unwrap();
        assert_eq!((s.line, s.input.buf.as_str()), (5, ""), "flush now");
        assert!(!app.line_in_code(5), "the indent was the membership");
        assert_eq!(app.lines[3].text, " ", "the blanks above stay code");
        assert_eq!(app.lines[4].text, " ");
    }

    /// At column 0, ⌫ on a blank code line eats one indent character —
    /// cosense web's way — instead of joining the line above.
    #[test]
    fn backspace_at_the_head_of_a_blank_code_line_eats_the_indent() {
        let ctx = test_ctx();
        let mut app = page(&["title", "code:x.py", " a = 1"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 6);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        if let Some(s) = app.session.as_mut() {
            s.input.cur = 0;
        }
        handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf.as_str(), "", "one ⌫ at BOL ate the indent");
        assert!(!app.line_in_code(3), "and the writer is out");
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
        assert!(app.toast_text().contains("タイトル行"), "status: {}", app.toast_text());
        assert!(app.session.is_some(), "and the session stays open");
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
