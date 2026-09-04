//! `code:tex` / `code:latex` のテキスト描画(NOTE-math-text.md)。
//!
//! 単位の検証は `math_text` 内のテストが持つ。ここでは配線を見る:
//! ブロックが数式として描かれること、編集中はソースに戻ること、
//! lib が読めない式はコードブロックのまま残ること。

use crate::*;
use super::support::*;

    /// The preview rows (`Row::Aside` — attached chrome, never page content).
    fn preview_rows(app: &App) -> Vec<String> {
        app.rows
            .iter()
            .filter_map(|r| match r {
                Row::Aside { line } => Some(
                    line.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
                ),
                _ => None,
            })
            .collect()
    }

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
fn a_tall_inline_formula_is_drawn_around_the_words() {
    let mut app = page(&["t", r"解は[$ \frac{-b}{2a} ]だと書いてある"]);
    app.rebuild(80);
    let row = app
        .rows
        .iter()
        .find_map(|r| match r {
            Row::Inline { texts, height, .. } => Some((texts.clone(), *height)),
            _ => None,
        })
        .expect("an inline row");
    let (texts, height) = row;
    assert_eq!(height, 3, "the fraction makes the line three rows tall");
    let at = |needle: &str| {
        texts
            .iter()
            .find(|(_, _, piece)| {
                piece.line.spans.iter().any(|s| s.content.contains(needle))
            })
            .unwrap_or_else(|| panic!("{needle} missing: {texts:?}"))
    };
    assert_eq!(at("解は").0, 1, "the words read on the middle row");
    assert_eq!(at("だと書いてある").0, 1);
    assert_eq!(at("-b").0, 0, "numerator above");
    assert_eq!(at("2a").0, 2, "denominator below");
}

#[test]
fn a_tall_inline_formula_falls_back_to_latex_in_a_narrow_pane() {
    let mut app = page(&["t", r"解は[$ \frac{-b \pm \sqrt{b^2-4ac}}{2a} ]だ"]);
    app.rebuild(14);
    let joined = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Inline { texts, .. } => Some(
                texts
                    .iter()
                    .map(|(_, _, p)| {
                        p.line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(joined.contains(r"\frac"), "LaTeX when it cannot fit: {joined:?}");
}

#[test]
fn a_formula_is_set_in_the_body_ink() {
    // 図と違って数式はどの字も意味を持つ。分数の罫線を薬めると
    // 「ここは飾り」と読めてしまうので、全体を本文と同じインクで出す。
    let mut app = math_page("code:tex", &[r"\frac{a}{b}"]);
    app.rebuild(80);
    let drawn: Vec<&Line> = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Line { line, .. } if line.spans.iter().any(|s| s.content.contains('─')) => {
                Some(line)
            }
            _ => None,
        })
        .collect();
    assert!(!drawn.is_empty(), "the bar is on screen");
    for line in drawn {
        for span in &line.spans {
            assert_eq!(span.style.fg, None, "no ink of its own: {span:?}");
        }
    }
}

#[test]
fn a_blank_line_inside_a_tex_block_keeps_the_formula_alive() {
    // 空行は打てる(ブロックは生きる)。ただしLaTeXに空行は意味がなく、
    // libに渡すと組図の中に紛れ込むので、描く前に落とす。
    // ブロック自体は空行がいる間も数式で、インデントを削り切った
    // 行が出たときに初めて終わる — mmd/codeと同じ契約。
    let mut app = math_page("code:tex", &[r"\frac{a}{b}", "", " "]);
    app.page_id = "PAGE".into();
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains('─')), "still a formula: {text:?}");
    assert!(
        !text.iter().any(|t| t.contains("frac")),
        "not source: {text:?}"
    );
    // 空行の行もブロックの内側(編集すればコードとして出る)。
    assert!(app.code_span_at_line(3).is_some(), "blank is inside");
}

#[test]
fn a_flush_blank_line_after_a_tex_block_is_already_outside() {
    // 出るのはインデントを削り切ったとき。フラッシュの空行は
    // もう外なので、そこに打った文字は普通の行になる。
    let mut app = page(&["t", "code:tex", " \\frac{a}{b}", ""]);
    app.page_id = "PAGE".into();
    app.rebuild(80);
    assert!(app.code_span_at_line(3).is_none(), "flush blank is out");
}

#[test]
fn leaving_a_tex_block_takes_deleting_the_indent() {
    let ctx = test_ctx();
    let mut app = math_page("code:tex", &[r"\frac{a}{b}"]);
    app.rebuild(80);
    let len = app.lines[2].text.len();
    enter_session(&mut app, &ctx, 2, len);
    handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
    let s = app.session.as_ref().unwrap();
    assert_eq!(s.input.buf.as_str(), " ", "a blank line with the block's indent");
    assert!(app.code_span_at_line(3).is_some(), "still inside");
    handle_session_key(&mut app, &ctx, key(KeyCode::Backspace));
    let s = app.session.as_ref().unwrap();
    assert_eq!(s.input.buf.as_str(), "", "indent gone");
    assert!(!app.code_span_at_line(3).map(|sp| sp.mermaid_header).unwrap_or(false));
    assert!(app.code_span_at_line(3).is_none(), "out — the block is behind us");
}

#[test]
fn editing_a_formula_previews_it_below_the_source() {
    // cosense web と同じ:コードモードの間、下に生プレビュー。
    // 薄い左罫(▏)が本文との違いの印。
    let ctx = test_ctx();
    let mut app = math_page(
        "code:tex",
        &[r"\begin{pmatrix}", r"a & b \\", r"c & d \\", r"\end{pmatrix}"],
    );
    app.rebuild(80);
    enter_session(&mut app, &ctx, 2, 4);
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains("code:tex")), "source: {text:?}");
    let preview = preview_rows(&app);
    assert!(
        preview.iter().any(|t| t.contains("プレビュー")),
        "a label says what this is: {text:?}"
    );
    assert!(
        preview.iter().any(|t| t.contains("⎛") && t.contains("a")),
        "the matrix is drawn under the source: {text:?}"
    );
    assert!(
        !text.iter().take_while(|t| !t.contains('▏')).any(|t| t.contains('▏')),
        "the bar starts only at the preview"
    );
}

#[test]
fn the_preview_follows_what_you_type() {
    let ctx = test_ctx();
    let mut app = page(&["t", "code:mmd", " flowchart LR", "  A-->B"]);
    app.rebuild(80);
    let end = app.lines[3].text.len();
    enter_session(&mut app, &ctx, 3, end);
    type_str(&mut app, &ctx, "x");
    app.rebuild(80);
    let text = text_rows(&app);
    let preview = preview_rows(&app);
    assert!(
        preview.iter().any(|t| t.contains("Bx")),
        "the drawing followed the keystroke: {text:?}"
    );
}

#[test]
fn an_unsettable_formula_previews_nothing() {
    // 組めない式は黙る。ソースがもう見えているので、
    // 「まだ組めない」の代わりに何かを出す必要はない。
    let ctx = test_ctx();
    let mut app = math_page("code:tex", &[r"\begin{align}", r"x &= 1", r"\end{align}"]);
    app.rebuild(80);
    enter_session(&mut app, &ctx, 2, 4);
    app.rebuild(80);
    let text = text_rows(&app);
    assert!(text.iter().any(|t| t.contains("code:tex")), "source stays: {text:?}");
    assert!(!text.iter().any(|t| t.contains('▏')), "no preview: {text:?}");
    assert!(preview_rows(&app).is_empty(), "no preview rows either");
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
