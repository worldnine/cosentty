//! テキスト描画段の結合テスト(NOTE-mmd-text.md)。
//!
//! web経路のテストは `mermaid_text = false` で旧契約を検証する。
//! ここでは旗が立ったまま(図になること、編集中はソースに戻ること)を見る。

use crate::*;
use super::support::*;

#[test]
fn flowchart_block_draws_as_a_diagram() {
    let mut app = mermaid_page();
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(
        text.iter().any(|t| t.contains('┌') || t.contains('╭')),
        "boxes on screen: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("code:mmd")),
        "source header hidden while drawn: {text:?}"
    );
    assert!(
        !app.rows.iter().any(|r| matches!(r, Row::Image { .. })),
        "no browser picture needed"
    );
}

#[test]
fn editing_a_drawn_block_shows_source() {
    let mut app = mermaid_page();
    app.session = Some(EditSession {
        line: 2,
        input: Input::new("  flowchart LR".into()),
        orig: "  flowchart LR".into(),
        want_col: None,
        sel_from: None,
    });
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(
        text.iter().any(|t| t.contains("flowchart LR")),
        "raw source while editing: {text:?}"
    );
    assert!(!text.iter().any(|t| t.contains('•')), "top-level header has no bullet");
}

#[test]
fn nested_mermaid_edit_shows_a_bullet_on_its_header_only() {
    for (header, body, edge, expected_header) in [
        ("　code:mmd", "  flowchart LR", "   A-->B", "• code:mmd"),
        (
            "　　code:mmd",
            "   flowchart LR",
            "    A-->B",
            "  • code:mmd",
        ),
    ] {
        // The marker is present whether the caret is on the header itself or
        // on another source line in the block.
        for caret in [1, 2] {
            let source = ["親", header, body, edge];
            let mut app = page(&source);
            app.session = Some(EditSession {
                line: caret,
                input: Input::new(source[caret].into()),
                orig: source[caret].into(),
                want_col: None,
                sel_from: None,
            });
            app.rebuild(80);
            let text = text_rows(&app);
            let bullet_rows: Vec<&str> = text
                .iter()
                .filter(|t| t.contains('•'))
                .map(String::as_str)
                .collect();
            assert_eq!(
                bullet_rows,
                vec![expected_header],
                "level header={header:?}, caret={caret}: {text:?}"
            );
            assert!(
                text.iter().any(|t| t.contains("flowchart LR") && !t.contains('•')),
                "body remains code without a bullet: {text:?}"
            );
        }
    }
}

#[test]
fn tab_on_a_mermaid_header_carries_the_whole_block() {
    let ctx = test_ctx();
    let mut app = page(&["親", "code:mmd", " flowchart LR", "  A-->B"]);
    app.rebuild(80);
    enter_session(&mut app, &ctx, 1, 0);
    let texts = |app: &App| -> Vec<String> {
        app.lines.iter().map(|l| l.text.clone()).collect()
    };

    handle_key(&mut app, &ctx, key(KeyCode::Tab));
    assert_eq!(
        texts(&app),
        ["親", " code:mmd", "  flowchart LR", "   A-->B"],
        "the body moves with its header"
    );
    assert_eq!(app.session.as_ref().unwrap().input.buf, " code:mmd");

    handle_key(&mut app, &ctx, key(KeyCode::Tab));
    assert_eq!(texts(&app), ["親", "  code:mmd", "   flowchart LR", "    A-->B"]);

    // Two levels is as deep as Cosense draws a diagram, so Tab stops there
    // rather than turning the block into list text under the caret.
    handle_key(&mut app, &ctx, key(KeyCode::Tab));
    assert_eq!(texts(&app), ["親", "  code:mmd", "   flowchart LR", "    A-->B"]);

    for _ in 0..2 {
        handle_key(&mut app, &ctx, key(KeyCode::BackTab));
    }
    assert_eq!(texts(&app), ["親", "code:mmd", " flowchart LR", "  A-->B"]);
    handle_key(&mut app, &ctx, key(KeyCode::BackTab));
    assert_eq!(texts(&app), ["親", "code:mmd", " flowchart LR", "  A-->B"], "level 0 is the floor");

    // Back at level 0 the header wears the code gutter again, not a bullet.
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.trim_start() == "code:mmd"), "{text:?}");
    assert!(!text.iter().any(|t| t.contains('•')), "{text:?}");

    // A space typed at the head of the header, and a Backspace over that
    // indent, are the other two ways to say the same thing.
    handle_key(&mut app, &ctx, key(KeyCode::Char(' ')));
    assert_eq!(texts(&app), ["親", " code:mmd", "  flowchart LR", "   A-->B"]);
    handle_key(&mut app, &ctx, key(KeyCode::Backspace));
    assert_eq!(texts(&app), ["親", "code:mmd", " flowchart LR", "  A-->B"]);

    // Past the indent, a space is just a space.
    if let Some(s) = app.session.as_mut() {
        s.input.cur = s.input.buf.len();
    }
    handle_key(&mut app, &ctx, key(KeyCode::Char(' ')));
    assert_eq!(app.session.as_ref().unwrap().input.buf, "code:mmd ");
}

#[test]
fn tab_inside_a_mermaid_body_still_types_indent_on_that_line_only() {
    let ctx = test_ctx();
    let mut app = page(&["親", " code:mmd", "  flowchart LR", "   A-->B"]);
    app.rebuild(80);
    enter_session(&mut app, &ctx, 2, 0);
    handle_key(&mut app, &ctx, key(KeyCode::Tab));
    assert_eq!(
        app.session.as_ref().unwrap().input.buf,
        "   flowchart LR",
        "a body line is code: its indent is content, and it moves alone"
    );
    assert_eq!(app.lines[1].text, " code:mmd", "the header did not move");
}

#[test]
fn diagrams_nest_twice_without_bullets() {
    for (header, body, edge, indent) in [
        ("　code:mmd", "  flowchart LR", "   A-->B", 2),
        ("　　code:mmd", "   flowchart LR", "    A-->B", 4),
    ] {
        let mut app = page(&["親", header, body, edge]);
        app.rebuild(80);
        let text = text_rows(&app);
        assert!(
            !text.iter().any(|t| t.contains("code:mmd")),
            "source header hidden while drawn: {text:?}"
        );
        assert!(text.iter().any(|t| t.contains('┌')), "diagram: {text:?}");
        assert!(!text.iter().any(|t| t.contains('•')), "no bullet: {text:?}");
        assert!(
            text.iter().all(|t| !t.contains('┌') || t.starts_with(&" ".repeat(indent))),
            "diagram respects level {indent}: {text:?}"
        );
    }
}

#[test]
fn a_third_level_mermaid_is_ordinary_list_text() {
    let mut app = page(&[
        "親",
        "　　　code:mmd",
        "    flowchart LR",
        "     A-->B",
    ]);
    app.rebuild(80);
    let text = text_rows(&app);
    for source in ["code:mmd", "flowchart LR", "A-->B"] {
        assert!(
            text.iter().any(|t| t.contains('•') && t.contains(source)),
            "{source} stays a list row: {text:?}"
        );
    }
    assert!(!text.iter().any(|t| t.contains('┌')), "not drawn: {text:?}");
}

#[test]
fn sequence_block_draws_lifelines() {
    let mut app = page(&[
        "t",
        "code:mmd",
        " sequenceDiagram",
        " Alice->>Bob: hello",
    ]);
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(
        text.iter().any(|t| t.contains("Alice") && t.contains("Bob"))
            || text.iter().any(|t| t.contains("hello")),
        "diagram on screen: {text:?}"
    );
}
