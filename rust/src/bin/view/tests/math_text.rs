//! `code:tex` / `code:latex` のテキスト描画(NOTE-math-text.md)。
//!
//! 単位の検証は `math_text` 内のテストが持つ。ここでは配線を見る:
//! ブロックが数式として描かれること、編集中はソースに戻ること、
//! lib が読めない式はコードブロックのまま残ること。

use crate::*;
use super::support::*;

fn math_page(header: &str, body: &[&str]) -> App {
    let mut texts: Vec<String> = vec!["t".into(), header.into()];
    texts.extend(body.iter().map(|b| format!(" {b}")));
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let mut app = page(&refs);
    app.page_id = "PAGE".into();
    app
}

#[test]
fn a_tex_block_draws_as_a_formula() {
    for header in ["code:tex", "code:latex", "code:proof.tex"] {
        let mut app = math_page(header, &[r"\frac{a}{b}"]);
        app.rebuild(80);
        let text = text_rows(&app);
        assert!(
            text.iter().any(|t| t.contains('─')),
            "{header}: fraction bar on screen: {text:?}"
        );
        assert!(
            !text.iter().any(|t| t.contains("frac")),
            "{header}: source hidden while drawn: {text:?}"
        );
        assert!(
            !text.iter().any(|t| t.contains(header)),
            "{header}: header hidden while drawn: {text:?}"
        );
    }
}

#[test]
fn a_formula_never_asks_the_browser() {
    // Cosense は KaTeX で数式を描くが、その要素は特定できていない。
    // 見当違いの場所を撮るくらいなら、テキストとソースで止める。
    let mut app = math_page("code:tex", &[r"\frac{a}{b}"]);
    app.rebuild(80);
    assert!(!app.start_web_renders(capability::Trigger::Auto), "no job queued");
    assert!(diagram_keys(&app).is_empty(), "no cache key for math");
}

#[test]
fn editing_a_formula_shows_its_source() {
    let mut app = math_page("code:tex", &[r"\frac{a}{b}"]);
    app.session = Some(EditSession {
        line: 2,
        input: Input::new(r" \frac{a}{b}".into()),
        orig: r" \frac{a}{b}".into(),
        want_col: None,
        sel_from: None,
    });
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains("frac")), "raw source: {text:?}");
    assert!(text.iter().any(|t| t.contains("code:tex")), "header too: {text:?}");
}

#[test]
fn latex_the_lib_cannot_read_stays_a_code_block() {
    // `align` は非対応。`\begin{align}` の字面を数式のふりで出さない。
    let mut app = math_page("code:tex", &["\\begin{align}", "x &= 1", "\\end{align}"]);
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains("code:tex")), "header stays: {text:?}");
    assert!(
        text.iter().any(|t| t.contains(r"\begin{align}")),
        "source stays: {text:?}"
    );
}

#[test]
fn a_formula_too_wide_for_the_pane_falls_back_to_source() {
    let mut app = math_page("code:tex", &[r"\frac{-b \pm \sqrt{b^2-4ac}}{2a}"]);
    app.rebuild(20);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains("frac")), "source at 20 cols: {text:?}");
    app.laid_width = 0;
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(!text.iter().any(|t| t.contains("frac")), "drawn at 80 cols: {text:?}");
}

#[test]
fn one_step_of_indent_is_already_too_deep_for_a_formula() {
    // 図と違って数式は左端だけ。箇条書きの下に入った時点で
    // コードブロックですらなくなり、各行がそのままリストの行になる。
    for header in [" code:tex", "　　code:latex"] {
        let body = format!("{} \\frac{{a}}{{b}}", " ".repeat(header.chars().count()));
        let mut app = page(&["親", header, &body]);
        app.page_id = "PAGE".into();
        app.rebuild(80);
        let text = text_rows(&app);
        for source in [header.trim(), r"\frac{a}{b}"] {
            assert!(
                text.iter().any(|t| t.contains('•') && t.contains(source)),
                "{source} stays a list row: {text:?}"
            );
        }
        assert!(!text.iter().any(|t| t.contains('─')), "not drawn: {text:?}");
    }
}

#[test]
fn a_formula_at_the_left_margin_still_draws() {
    let mut app = page(&["t", "code:tex", " \\frac{a}{b}"]);
    app.page_id = "PAGE".into();
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains('─')), "drawn: {text:?}");
    assert!(!text.iter().any(|t| t.contains('•')), "no bullet: {text:?}");
}
