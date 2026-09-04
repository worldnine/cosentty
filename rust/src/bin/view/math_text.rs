//! `code:tex` / `code:latex` のテキスト描画(NOTE-math-text.md)。
//!
//! 描画本体は `term-maths`。こちらは入力の整形・非対応の見分け・
//! 幅の見張りだけを持つ。mmd_text と同じ縮退の作法:
//! テキスト段 → (mathにブラウザ段はない) → コード行。

use crate::session::str_width;

/// `COSENSE_MATH=off`: 数式をテキストで組まず、コードブロックのまま見せる。
/// 罫線・大括弧・数学記号を持たないフォントの逃げ道。
pub(crate) fn text_tier_off() -> bool {
    std::env::var("COSENSE_MATH").unwrap_or_default() == "off"
}

/// 行末の `\\` は「次の行がある」という記号なので、最後の行に付いていると
/// lib は空の行をもう一段組んでしまう(`⎝  ⎠` だけの行が浮く)。
/// pmatrix を素直に書くと必ずこうなるため、投げる前に落とす。
fn trim_trailing_row_break(code: &str) -> String {
    // 行区切りが意味を持たないのは2か所だけ: 式の末尾と、
    // `\end{...}` の直前。中途の `\\` は行を分ける本来の仕事。
    let drop = |chunk: &str| -> String {
        let t = chunk.trim_end();
        match t.strip_suffix(r"\\") {
            Some(rest) => rest.trim_end().to_string(),
            None => t.to_string(),
        }
    };
    let chunks: Vec<&str> = code.split(r"\end{").collect();
    let last = chunks.len() - 1;
    let mut out = String::with_capacity(code.len());
    for (i, chunk) in chunks.iter().enumerate() {
        out.push_str(&drop(chunk));
        if i < last {
            out.push('\n');
            out.push_str(r"\end{");
        }
    }
    out
}

/// パースが最後まで通らなかった出力を見分ける。lib は知らない命令を
/// エラーにせず、`\begin{align}` のように**そのまま**吐く。読めない字面を
/// 数式のふりで出すより、コードブロックのまま見せたほうがましなので、
/// バックスラッシュ＋英字が残っていたら降りる。
fn looks_unparsed(rendered: &str) -> bool {
    let mut chars = rendered.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            return true;
        }
    }
    false
}

/// 数式そのものが空(空行だけのブロック)なら描くものがない。
fn is_blank(code: &str) -> bool {
    code.lines().all(|l| l.trim().is_empty())
}

/// lib に描かせる。失敗・panic・幅超過・非対応は `None` で縮退せよ。
pub(crate) fn render_text(code: &str, width: usize) -> Option<Vec<String>> {
    if is_blank(code) {
        return None;
    }
    let src = trim_trailing_row_break(code);
    // LaTeX パーサは入力を受ける境界。mermaid と同じく、lib 内の panic を
    // viewer 全体の終了にしない。
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        term_maths::render(&src).to_string()
    }))
    .ok()?;
    if rendered.trim().is_empty() || looks_unparsed(&rendered) {
        return None;
    }
    let lines: Vec<String> = rendered.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return None;
    }
    // 数式は折り返せない(2次元の組みが崩れる)ので、入らなければ降りる。
    if lines.iter().any(|l| str_width(l) > width.max(1)) {
        return None;
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_page_formulas_all_draw() {
        for code in [
            r"E = mc^2",
            r"\sqrt{3 \times 4}",
            r"\frac{-b \pm \sqrt{b^2-4ac}}{2a}",
            r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
            r"\lim_{x \to 0} \frac{\sin x}{x} = 1",
            r"\alpha + \beta \le \gamma",
            "L = \\int_{b}^{a} \\sqrt{ \\left( \\frac{dx}{dt} \\right)^{2} } dt",
        ] {
            let lines = render_text(code, 80).unwrap_or_else(|| panic!("{code}"));
            for l in &lines {
                assert!(str_width(l) <= 80, "too wide: {l}");
            }
        }
    }

    #[test]
    fn a_fraction_is_drawn_over_three_rows() {
        let out = render_text(r"\frac{a}{b}", 40).expect("draws");
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out[0].contains('a') && out[2].contains('b'), "{out:?}");
        assert!(out[1].chars().all(|c| c == '─'), "{out:?}");
    }

    #[test]
    fn a_matrix_does_not_grow_an_empty_last_row() {
        // 行末の `\\` は Cosense の作例そのままの書き方。
        let src = "\\begin{pmatrix}\na & b \\\\\nc & d \\\\\n\\end{pmatrix}";
        let out = render_text(src, 40).expect("draws");
        assert_eq!(out.len(), 2, "phantom row: {out:?}");
        assert!(out[0].contains('a') && out[1].contains('d'), "{out:?}");
    }

    #[test]
    fn japanese_labels_keep_every_row_the_same_width() {
        // term-maths はセルを String＋unicode-width で持つので、
        // mermaid-text のような continuation cell の補正は要らない。
        let out = render_text(r"\frac{速度}{時間} + x^2", 40).expect("draws");
        let w = str_width(&out[0]);
        for l in &out {
            assert_eq!(str_width(l), w, "rows must align: {out:?}");
        }
    }

    #[test]
    fn unsupported_latex_declines_instead_of_printing_commands() {
        // align は lib が知らない。`\begin{align}` の字面を数式のふりで
        // 出さず、コードブロックのまま見せる。
        assert_eq!(render_text("\\begin{align}\nx &= 1\n\\end{align}", 80), None);
        assert_eq!(render_text(r"\qquad", 80), None);
        assert_eq!(render_text("", 80), None);
        assert_eq!(render_text("\n \n", 80), None);
    }

    #[test]
    fn a_formula_wider_than_the_pane_declines() {
        // 折り返すと2次元の組みが壊れるので、狭いペインではソースを見せる。
        assert_eq!(render_text(r"\frac{-b \pm \sqrt{b^2-4ac}}{2a}", 8), None);
    }
}
