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
