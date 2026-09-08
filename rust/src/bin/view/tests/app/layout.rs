use crate::tests::support::*;
use crate::*;

#[test]
fn wrapped_bullets_hang_and_stay_one_source_line() {
    let long = format!(" {}", "字".repeat(30)); // a 30-char bullet at level 1
    let mut app = page(&["t", &long, "after"]);
    app.rebuild(22); // text width 20
    let rows: Vec<String> = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Line { line, src, .. } if *src == 1 => Some(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert!(rows.len() >= 3);
    assert!(rows[0].starts_with("• 字"), "{:?}", rows[0]);
    for r in &rows[1..] {
        assert!(
            r.starts_with("  字"),
            "continuation hangs under the text: {r:?}"
        );
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
            Row::Line { line, src, .. } if *src == 1 => Some(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            ),
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
        vec![LinkItem::ProjectPage {
            project: "other".into(),
            title: "Page".into()
        }]
    );
    // j/k walk body → related → body seamlessly
    app.goto_src(1);
    app.move_cursor(true);
    assert_eq!(app.cursor, 2, "steps over the heading rule onto Alpha");
    assert_eq!(
        app.cursor_line_links(),
        vec![LinkItem::Page("Alpha".into())]
    );
    // The frame closes after the body, BEFORE the related section.
    assert_eq!(app.frame_end_top(), 2);
    let end = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::FrameEnd))
        .unwrap();
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
    app.selection = Some(Selection {
        anchor: 1,
        cursor: 3,
    });
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
                accessed: 0,
                created: 0,
                linked: 0,
                unread: true,
            },
            RelEntry {
                item: LinkItem::Page("読んだ".into()),
                title: "読んだ".into(),
                desc: String::new(),
                age: now_secs(),
                accessed: 0,
                created: 0,
                linked: 0,
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
            project_display: String::new(),
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
            open_stamp: 0,
            editable: true,
            related: Vec::new(),
            facts: PageFacts::default(),
            links: LinkTruth::default(),
            palette: ctx.palette(),
            telomere_tint: None,
        },
        &ctx,
    );
    assert_eq!(
        app.links.exists("タグ"),
        None,
        "a dead word does not travel"
    );
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
    assert_eq!(
        asked.asked_by, "pid-B",
        "asked on behalf of the page that now writes it"
    );
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
        input: Input {
            buf: text.clone(),
            cur: 0,
        },
        orig: text,
        want_col: None,
        sel_from: None,
    });
    app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
    app.probe_unknown_links();
    assert!(
        rx.try_recv().is_err(),
        "the line under the caret is not judged"
    );

    // The caret moves away — now the question is worth asking, and
    // only about the title the page could not answer for.
    app.session = None;
    app.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
    app.probe_unknown_links();
    let asked = rx.try_recv().unwrap();
    assert_eq!(
        (asked.project.as_str(), asked.title.as_str()),
        ("proj", "いま打った")
    );
    assert_eq!(
        asked.asked_by, app.page_id,
        "the asking page cannot vouch for itself"
    );
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
    app.link_probe_res_tx
        .send((answer("proj", "いま打った", &me), false))
        .unwrap();
    assert!(app.drain_link_probes());
    assert!(app.links.missing("いま打った"));
    assert!(!app.links.missing("もとからある"));

    // An answer about a project we have left is not about this page.
    app.link_probe_res_tx
        .send((answer("elsewhere", "もとからある", &me), false))
        .unwrap();
    assert!(
        !app.drain_link_probes(),
        "another project's answer changes nothing"
    );
    assert!(!app.links.missing("もとからある"));

    // Nor is "nobody but the asking page writes this" an answer once
    // the reader has moved to another page — that page may be the
    // second one writing it, which is a different question.
    app.link_probe_res_tx
        .send((answer("proj", "よそで聞いた", "another-page-id"), false))
        .unwrap();
    assert!(
        !app.drain_link_probes(),
        "a dead answer belongs to the page that asked"
    );
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

    let event = |kind| MouseEvent {
        kind,
        column: 6,
        row: 3,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, &ctx, event(MouseEventKind::Moved));
    assert!(app.move_mode.is_some(), "motion is not an action");
    handle_mouse(&mut app, &ctx, event(MouseEventKind::ScrollDown));
    assert!(
        app.move_mode.is_some(),
        "and the wheel only moves the viewport"
    );
    assert!(drain_jobs(&mut app).is_empty(), "nothing was sent");

    handle_mouse(
        &mut app,
        &ctx,
        event(MouseEventKind::Down(MouseButton::Left)),
    );
    assert!(app.move_mode.is_none(), "a button does let go");
    assert_eq!(drain_jobs(&mut app).len(), 1);
    finish_outline(&mut app, &ctx);
}

#[test]
fn movement_skips_comment_cards() {
    let mut app = page(&["a", "b", "c"]);
    app.comments.push(Comment {
        project: "proj".into(),
        title: "t".into(),
        page_id: "pid".into(),
        revision: None,
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
        page_id: "pid".into(),
        revision: None,
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
    assert_eq!(
        app.src_at_screen_row(2),
        Some(1),
        "wrapped continuation -> its line"
    );
    assert_eq!(app.src_at_screen_row(5), Some(2));
    assert_eq!(app.src_at_screen_row(6), None, "past the end");
    app.scroll = 3;
    assert_eq!(
        app.src_at_screen_row(-1),
        Some(1),
        "reclaimed top row maps too"
    );
    assert_eq!(app.src_at_screen_row(0), Some(1));
    assert_eq!(app.src_at_screen_row(2), Some(2));
}

#[test]
fn rendered_links_are_mouse_hit_targets() {
    let mut app = page(&["t", "see [Target] and [Docs https://example.com] #tag"]);
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
    assert_eq!(
        tbl.link_at_screen_position(1, 40),
        None,
        "past the label is not the label"
    );

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
    let mut dup = page(&["t", "see [Docs] then [Docs https://example.com]"]);
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
    handle_mouse_content(
        &mut app,
        &test_ctx(),
        mouse(MouseEventKind::Down(MouseButton::Left), 41, 1 + 7),
    );
    let end = app.max_scroll(app.view_h);
    assert_eq!(app.scroll, end, "track bottom -> last page");
    assert_eq!(app.cursor, 3, "the cursor keeps its line");
    assert!(!app.follow);
    handle_mouse_content(
        &mut app,
        &test_ctx(),
        mouse(MouseEventKind::Drag(MouseButton::Left), 41, 1 + 3),
    );
    assert!(app.scroll < end && app.scroll > 0);
    handle_mouse_content(
        &mut app,
        &test_ctx(),
        mouse(MouseEventKind::Up(MouseButton::Left), 41, 1 + 3),
    );
    assert_eq!(app.scrollbar_drag, None);
    // wheel over the body: one row per event, cursor untouched
    handle_mouse_content(
        &mut app,
        &test_ctx(),
        mouse(MouseEventKind::ScrollUp, 10, 5),
    );
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
    assert!(
        !app.follow,
        "the next frame must not yank the viewport back"
    );
    app.wheel_scroll(1000, 10);
    assert_eq!(
        app.scroll, 22,
        "clamped to max (content+2 rules - viewport)"
    );
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

/// 画像と文字が混ざった行のリンクも押せる。レイアウトが各テキスト片に
/// 「どの部品の何列目から」を持ち、レンダラの Hit(テキスト部品を通した
/// スパン番号)へ逆引きする。画像の右に続く文字も、折り返して下の箱に
/// 落ちた文字も同じ道。
#[test]
fn links_on_a_line_of_text_and_pictures_are_mouse_hit_targets() {
    let mut app = page(&[
        "t",
        "see [Target] [https://example.com/a.png] then [Docs https://example.com]",
    ]);
    // 画像は未着(56×16 のプレースホルダ)。後ろの文字が長いので箱の
    // 横(80-11-56=13 桁)には収まりきらず1行折り返し、行は17行の高さに
    // なる。文字は最下段から2行目(row 15=ベースライン)の画像の左右へ:
    // `see Target ` は col 0..、` then Docs` は col 11+56=67 から。
    app.rebuild(80);
    let Some(Row::Inline { height, texts, .. }) = app.rows.get(1) else {
        panic!(
            "expected an inline row, got {:?}",
            app.rows.get(1).map(Row::height)
        )
    };
    assert_eq!(*height, 17);
    assert_eq!(texts.len(), 3, "{texts:?}");
    // ベースラインは最下段の1つ上(row 15)。後ろの片は箱の横に
    // " then D" までしか入らず、"ocs" 以下は下の箱(row 16)へ折り返す:
    // リンクが2つの片に割れる配置になった。
    let base = (*height - 2) as i32;
    // 左の片の中のリンク。
    assert_eq!(
        app.link_at_screen_position(1 + base, 5),
        Some((1, LinkItem::Page("Target".into())))
    );
    // 画像の右の片の中のリンク(2つ目のテキスト部品なので Hit のスパン
    // 番号は1つ目の分だけ先へずれている)。"D" はベースラインの
    // col 67+6=73 に、続きは折り返し先の col 1 にある。
    let docs = Some((
        1,
        LinkItem::Url {
            label: "Docs".into(),
            url: "https://example.com".into(),
        },
    ));
    assert_eq!(app.link_at_screen_position(1 + base, 73), docs);
    assert_eq!(app.link_at_screen_position(1 + base + 1, 1), docs);
    // 文字の無い列・画像の上の段は何でもない。
    assert_eq!(app.link_at_screen_position(1 + base, 1), None, "plain text");
    assert_eq!(
        app.link_at_screen_position(1 + base, 20),
        None,
        "the picture"
    );
    assert_eq!(
        app.link_at_screen_position(1, 5),
        None,
        "a row above the text line"
    );
}
