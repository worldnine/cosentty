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
