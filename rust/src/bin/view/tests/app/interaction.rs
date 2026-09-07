use crate::tests::support::*;
use crate::*;

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
    assert_eq!(
        &s.input.buf[s.input.cur..s.input.cur + 1],
        "d",
        "still before d"
    );

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
        handle_mouse_content(
            app,
            &ctx,
            mouse(MouseEventKind::Down(MouseButton::Left), col, 3),
        );
        handle_mouse_content(
            app,
            &ctx,
            mouse(MouseEventKind::Up(MouseButton::Left), col, 3),
        );
    };

    click(&mut app, 7);
    assert!(
        app.session.is_none(),
        "a single click only moves the cursor"
    );
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
    assert_eq!(
        s.input.cur,
        "after".len().min(3),
        "the sticky column, clamped"
    );

    // Up from there lands on the LAST row of the wrapped line, not its
    // first — the row that is visually just above.
    handle_session_key(&mut app, &ctx, key(KeyCode::Up));
    let s = app.session.as_ref().unwrap();
    assert_eq!(s.line, 1);
    assert!(
        s.input.cur >= width,
        "landed on the last row: {}",
        s.input.cur
    );

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
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            1 + 2,
            2 + first as u16,
        ),
    );
    handle_mouse_content(
        &mut app,
        &ctx,
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            1 + 2,
            2 + first as u16 + 1,
        ),
    );
    let (a, b) = app.session.as_ref().unwrap().sel_span().expect("a span");
    assert_eq!((a, b), (head, next), "the run between the two clicks");
    assert_eq!(
        &long[a..b],
        &long[2..row_w + 2],
        "…which is what the eye dragged over"
    );
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
    assert_eq!(
        picked, "hello",
        "the selected run is the one that stands out"
    );
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
    assert!(
        app.toast_text().contains("^k"),
        "status: {}",
        app.toast_text()
    );
    assert!(
        app.toast_text().contains("⌫"),
        "status: {}",
        app.toast_text()
    );

    // Not even with a selection: that range belongs to `c` (comment)
    // in READ, and to ⌫ in EDIT.
    app.selection = Some(Selection {
        anchor: 1,
        cursor: 2,
    });
    handle_key(&mut app, &ctx, key(KeyCode::Char('x')));
    assert_eq!(app.lines.len(), 3);
}
