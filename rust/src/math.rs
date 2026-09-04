//! 数式のテキスト描画(NOTE-math-text.md)。
//!
//! ブロック(`code:tex` / `code:latex`)とインライン(`[$ ... ]`)の
//! 両方がここを通る。描画本体は `term-maths`。こちらは入力の整形・
//! 非対応の見分け・幅の見張りだけを持つ。縮退の作法は mmd と同じ:
//! テキスト段 → (math にブラウザ段はない) → ソース。

/// Display width of a string in terminal columns.
fn str_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

/// `COSENSE_MATH=off`: 数式をテキストで組まず、書かれたまま見せる。
/// 罫線・大括弧・数学記号を持たないフォントの逃げ道。
pub fn text_tier_off() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // インラインは1行に何個でも出てくるので、描画のたびに
    // 環境変数を引かない。途中で変わる値でもない。
    *OFF.get_or_init(|| std::env::var("COSENSE_MATH").unwrap_or_default() == "off")
}

/// 数式に空行はない。書き手が空行を打っても(ブロックの中に置けるようになった)、
/// LaTeXとしての意味はなく、libに渡すと組図の**中**に紛れ込んで形を壊す
/// (` a ─── \n\n\n b`)。投げる前に落とす。
/// 行末の `\\` は「次の行がある」という記号なので、最後の行に付いていると
/// lib は空の行をもう一段組んでしまう(`⎝  ⎠` だけの行が浮く)。
/// pmatrix を素直に書くと必ずこうなるため、投げる前に落とす。
fn trim_trailing_row_break(code: &str) -> String {
    let no_blanks: Vec<&str> = code
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let code = no_blanks.join("\n");
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

/// ブロック数式を組む。失敗・panic・幅超過・非対応は `None` で縮退せよ。
pub fn render_text(code: &str, width: usize) -> Option<Vec<String>> {
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

/// 組まれた式。行の中に置くには、高さだけでなく**どの行が本文と
/// 揃う行なのか**が要る。分数なら真ん中の罫線の行。
#[derive(Debug)]
pub struct Rendered {
    /// 各行。すべて同じ表示幅に揃えられている。
    pub rows: Vec<String>,
    /// 本文と揃える行の番号(0始まり)。
    pub baseline: usize,
    /// 表示幅(端末の桁数)。
    pub width: usize,
}

/// インライン `[$ ... ]` を組む。1行にも複数行にもなる。
pub fn render_rows(latex: &str) -> Option<Rendered> {
    if text_tier_off() || latex.trim().is_empty() {
        return None;
    }
    let src = trim_trailing_row_break(latex);
    let block = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let b = term_maths::render(&src);
        (b.to_string(), b.baseline(), b.width())
    }))
    .ok()?;
    let (rendered, baseline, width) = block;
    if looks_unparsed(&rendered) {
        return None;
    }
    let rows: Vec<String> = rendered.lines().map(str::to_string).collect();
    if rows.is_empty() || rows.iter().all(|r| r.trim().is_empty()) {
        return None;
    }
    Some(Rendered { baseline: baseline.min(rows.len() - 1), width, rows })
}

/// 行の中にそのまま置ける式だけを1行の文字列で返す。
pub fn render_inline(latex: &str) -> Option<String> {
    let r = render_rows(latex)?;
    if r.rows.len() != 1 {
        return None;
    }
    let only = r.rows[0].trim();
    (!only.is_empty()).then(|| only.to_string())
}

/// いくつもの式を**横に並べて**1枚の組図にする(本文行に複数のインライン
/// 数式があるときのプレビュー用)。各式のベースラインをそろえる ——
/// 1行の式はその行に立ち、分数はまたぐ。余白は削る。
pub fn join_beside(blocks: &[Rendered]) -> Option<Rendered> {
    let above = blocks.iter().map(|b| b.baseline).max()?;
    let below = blocks
        .iter()
        .map(|b| b.rows.len().saturating_sub(1 + b.baseline))
        .max()?;
    let mut rows = Vec::with_capacity(above + below + 1);
    for off in -(above as i32)..=(below as i32) {
        let mut line = String::new();
        for b in blocks {
            let idx = b.baseline as i32 + off;
            if (0..b.rows.len() as i32).contains(&idx) {
                line.push_str(&b.rows[idx as usize]);
            } else {
                line.push_str(&" ".repeat(b.width));
            }
        }
        rows.push(line.trim_end().to_string());
    }
    let width = rows.iter().map(|r| str_width(r)).max()?;
    (!rows.is_empty()).then(|| Rendered { rows, baseline: above, width })
}

/// 1行に書かれたインライン数式を全部組んで、横に並べた1枚にする。
/// 一つも組めなければ None。
pub fn preview_line(latexes: &[String]) -> Option<Rendered> {
    let blocks: Vec<Rendered> =
        latexes.iter().filter_map(|l| render_rows(l)).collect();
    join_beside(&blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_join_beside_on_a_shared_baseline() {
        let frac = render_rows(r"\frac{a}{b}").expect("draws");
        let plain = render_rows(r"x = 1").expect("draws");
        let joined = join_beside(&[frac, plain]).expect("draws");
        // 分数は3行(上1+ベースライン+下1)、x = 1 はベースラインに立つ。
        // 並べると高さ3で、x = 1 は真ん中の行に来る。
        assert_eq!(joined.rows.len(), 3, "{joined:?}");
        assert_eq!(joined.baseline, 1);
        assert!(joined.rows[1].contains("x = 1"), "{joined:?}");
        assert!(joined.rows[0].contains('a') && joined.rows[2].contains('b'), "{joined:?}");
        // 各行の行末の空白は削られている。
        for r in &joined.rows {
            assert_eq!(r.trim_end(), r, "{joined:?}");
        }
    }

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
    fn blank_lines_the_writer_typed_do_not_reach_the_lib() {
        // ブロックの中に空行が打てるようになった。LaTeXに空行は意味がなく、
        // libに渡すと組図の中に紛れ込むので、こちらで落とす。
        let with = render_text("\\frac{a}{b}\n\n ", 40).expect("draws");
        assert_eq!(with, render_text("\\frac{a}{b}", 40).expect("draws"));
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
