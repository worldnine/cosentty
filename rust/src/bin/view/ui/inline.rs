//! Inline image/formula placement and text selection decoration.

use super::*;

/// One thing on a line: a run of text, a picture with its cell size, or a
/// formula drawn over several rows.
#[derive(Clone, Debug)]
pub(crate) enum Inline {
    Text(Line<'static>),
    Image { url: String, w: u16, h: u16 },
    /// Unlike a picture, a formula straddles the text line: a fraction has
    /// its numerator ABOVE the words and its denominator BELOW them.
    Formula { rows: Vec<String>, baseline: u16, w: u16 },
}

/// Where the layout put something.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed<T> {
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) what: T,
}

/// One row's worth of one text part, and where in the UNWRAPPED part it
/// begins — what turns a clicked cell back into a span of the renderer's
/// line (see `App::link_at_screen_position`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextPiece {
    /// Index into the `items` given to `layout_inline`.
    pub(crate) part: usize,
    /// Display column of the part's text this piece starts at.
    pub(crate) start: usize,
    pub(crate) line: Line<'static>,
}

/// A line laid out the way a browser lays out inline images: items run
/// left to right, and each picture sits ON the text line — its BOTTOM
/// edge level with the text, growing upwards — so the words before and
/// after it read as one sentence. When a row fills, the next "line box"
/// starts below, indented like the rest of the item.
///
/// This is what lets `本文 [画像]`, `[画像]本文` and `[A] と [B]` all be
/// the same thing: a sequence, not three special cases.
#[allow(unused_assignments)] // the final `flush!` resets state nobody reads
pub(crate) fn layout_inline(
    items: &[Inline],
    indent: usize,
    width: usize,
) -> (Vec<Placed<String>>, Vec<Placed<TextPiece>>, u16) {
    let body = width.saturating_sub(indent).max(1);
    let mut images: Vec<Placed<String>> = Vec::new();
    let mut texts: Vec<Placed<TextPiece>> = Vec::new();
    // The box being filled: its top row, how tall it is so far, how far
    // along it we are, and what has been put in it (positions are relative
    // to the box, resolved against its height when it is flushed).
    let mut top = 0u16;
    // The box grows in both directions from the row the words sit on:
    // `above` for what rises over it (a picture, a numerator), `below` for
    // what hangs under it (a denominator). Text itself needs neither.
    let mut above = 0u16;
    let mut below = 0u16;
    let mut x = 0usize;
    let mut pending_img: Vec<(u16, u16, u16, String)> = Vec::new(); // (col, w, h, url)
    // (col, rows off the baseline, text). Only a formula's rows are ever
    // off it; words are always ON it.
    let mut pending_txt: Vec<(u16, i16, TextPiece)> = Vec::new();

    // Close the current box: everything is placed against the row the text
    // sits on. A picture's LAST row is that row; a formula's baseline row
    // is that row.
    macro_rules! flush {
        () => {
            let base = top + above;
            for (col, _, h, url) in pending_img.drain(..) {
                images.push(Placed { row: base + 1 - h, col: col + indent as u16, what: url });
            }
            for (col, off, piece) in pending_txt.drain(..) {
                let row = (base as i32 + off as i32).max(0) as u16;
                texts.push(Placed { row, col: col + indent as u16, what: piece });
            }
            top = base + below + 1;
            above = 0;
            below = 0;
            x = 0;
        };
    }

    for (part, item) in items.iter().enumerate() {
        match item {
            Inline::Image { url, w, h } => {
                let w = (*w).min(body as u16);
                if x > 0 && x + w as usize > body {
                    flush!();
                }
                let h = (*h).max(1);
                pending_img.push((x as u16, w, h, url.clone()));
                above = above.max(h - 1);
                x += w as usize;
            }
            Inline::Formula { rows, baseline, w } => {
                let w = (*w).min(body as u16);
                if x > 0 && x + w as usize > body {
                    flush!();
                }
                // Its rows are text like any other: placed one per row and
                // drawn by the same code, so a formula can be read, found
                // and copied the way the words around it can.
                let baseline = (*baseline).min(rows.len() as u16 - 1);
                for (k, row) in rows.iter().enumerate() {
                    pending_txt.push((
                        x as u16,
                        k as i16 - baseline as i16,
                        TextPiece { part, start: 0, line: Line::from(row.clone()) },
                    ));
                }
                above = above.max(baseline);
                below = below.max(rows.len() as u16 - 1 - baseline);
                x += w as usize;
            }
            Inline::Text(line) => {
                let mut rest = line.clone();
                // How far into the part's text the next piece begins. The
                // wrap drops nothing, so the widths of the pieces add up.
                let mut start = 0usize;
                loop {
                    let room = body.saturating_sub(x);
                    // Too little room to say anything: start a new box.
                    if room < 2 && x > 0 {
                        flush!();
                        continue;
                    }
                    let mut pieces = wrap_line(&rest, room.max(1)).into_iter();
                    let Some(first) = pieces.next() else { break };
                    let used = str_width(
                        &first.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
                    );
                    pending_txt.push((x as u16, 0, TextPiece { part, start, line: first }));
                    start += used;
                    x += used;
                    let tail: Vec<Span<'static>> =
                        pieces.flat_map(|l| l.spans.into_iter()).collect();
                    if tail.is_empty() {
                        break;
                    }
                    flush!();
                    rest = Line::from(tail);
                }
            }
        }
    }
    flush!();
    (images, texts, top.max(1))
}

/// One row of a drawn artifact, styled.
///
/// A diagram is read in two layers: the RULES that hold its shape, and the
/// WORDS in its boxes. Cosense's own SVG says this with weight and colour;
/// a terminal says it by dimming the rules — exactly what a `table:` block
/// already does with its borders. The words keep the body's ink so the
/// Japanese in a node reads as text, not as decoration.
///
/// A formula gets none of this: every glyph in it carries meaning (the bar
/// of a fraction as much as the ∫ beside it), so it is set in one ink, the
/// same one the sentence around it uses.
pub(crate) fn drawn_line(text: &str, indent: usize, kind: ArtifactKind) -> Line<'static> {
    let pad = " ".repeat(indent);
    if kind == ArtifactKind::Math {
        return Line::from(format!("{pad}{text}"));
    }
    let rule = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span<'static>> = vec![Span::raw(pad)];
    let mut run = String::new();
    let mut run_is_rule = false;
    for ch in text.chars() {
        let is_rule = is_diagram_rule(ch);
        if !run.is_empty() && is_rule != run_is_rule {
            let done = std::mem::take(&mut run);
            spans.push(if run_is_rule { Span::styled(done, rule) } else { Span::raw(done) });
        }
        run_is_rule = is_rule;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(if run_is_rule { Span::styled(run, rule) } else { Span::raw(run) });
    }
    Line::from(spans)
}

/// Is this character part of a diagram's SHAPE rather than its words?
///
/// Only characters no label would contain: the box-drawing and block
/// ranges, plus the arrow heads and shade the renderer draws with. ASCII
/// mode (`COSENSE_MERMAID=ascii`) draws its rules with `- | + > v`, which
/// are also letters in labels — there the two layers cannot be told apart
/// by the character, so nothing is dimmed and everything reads as text.
fn is_diagram_rule(ch: char) -> bool {
    matches!(ch, '\u{2190}'..='\u{21FF}')     // arrows
        || matches!(ch, '\u{2500}'..='\u{257F}') // box drawing
        || matches!(ch, '\u{2580}'..='\u{259F}') // blocks and shades
        || matches!(ch, '\u{25A0}'..='\u{25FF}') // geometric shapes (arrow heads)
}

/// The formula rows of a part, ready for `layout_inline`.
pub(crate) fn inline_formula(rows: &[String], baseline: usize) -> Inline {
    let w = rows.iter().map(|r| str_width(r)).max().unwrap_or(0) as u16;
    Inline::Formula { rows: rows.to_vec(), baseline: baseline as u16, w }
}

/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder: the bullet where the
/// picture's own bullet goes, then the space the picture would start at.
pub(crate) fn bullet_pad(indent: usize, item: bool) -> String {
    if item && indent >= 2 {
        format!("{}{BULLET} ", " ".repeat(indent - 2))
    } else {
        " ".repeat(indent)
    }
}

/// One row of a block the web renderer is still working on/// One row of a block the web renderer is still working on, with the
/// brightness band applied to every span.
/// EDIT の行またぎ選択の境界(行内バイト位置)を、折り返し前の表示列に
/// 変換する。rendered な行の列とは厳密には一致しないことがある(記法が
/// 隠れる行など)が、click_caret がアンカーを置くときと同じ近似なので、
/// マウスが選んだ端と描画は食い違わない。
pub(crate) fn sel_boundary_col(raw: &str, byte: usize, code: Option<CodeSpan>) -> usize {
    let disp = session_display(raw, code);
    let d = display_caret(raw, byte, code);
    str_width(&disp[..floor_boundary(&disp, d)])
}

/// 1つの表示行の [from, to) 列を REVERSED にする。スパンは表示列で
/// 切り分け、境界をまたぐスパンだけ分割する。
pub(crate) fn reverse_cols(line: Line<'static>, from: usize, to: usize) -> Line<'static> {
    if from >= to {
        return line;
    }
    let style = line.style;
    let alignment = line.alignment;
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut at = 0usize;
    for sp in line.spans {
        let w = str_width(sp.content.as_ref());
        let (s, e) = (at, at + w);
        at = e;
        if e <= from || s >= to || w == 0 {
            out.push(sp);
            continue;
        }
        let text = sp.content.into_owned();
        let cut_a = byte_at_col(&text, from.saturating_sub(s));
        let cut_b = byte_at_col(&text, to.saturating_sub(s).min(w));
        if cut_a > 0 {
            out.push(Span::styled(text[..cut_a].to_string(), sp.style));
        }
        if cut_b > cut_a {
            out.push(Span::styled(
                text[cut_a..cut_b].to_string(),
                sp.style.add_modifier(Modifier::REVERSED),
            ));
        }
        if cut_b < text.len() {
            out.push(Span::styled(text[cut_b..].to_string(), sp.style));
        }
    }
    let mut line = Line::from(out);
    line.style = style;
    line.alignment = alignment;
    line
}

pub(crate) fn shimmer(line: &Line<'static>, pos: u16, len: u16, app: &App, ctx: &Ctx) -> Line<'static> {
    let level = cosense::theme::shimmer_level(pos, len, app.web_anim.elapsed().as_secs_f32());
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| {
            Span::styled(
                s.content.clone(),
                cosense::theme::shimmer_style(s.style, ctx.terminal_bg, level),
            )
        })
        .collect();
    Line::from(spans)
}

/// `shimmer` turned sideways: the band runs ALONG a single row, character
/// by character, for the rows that are one line tall (a picture's `[URL]`
/// while it downloads, and the box a mixed line reserves for one).
///
/// Two things differ from the vertical band beyond the direction, and both
/// are the reason this row used to look dead: it animates against the
/// pictures' own clock, and `shimmer_level_across` times the sweep instead
/// of counting cells per second, so a sixty-cell URL crosses in the same
/// moment a twenty-cell one does. Same brightness floor and ceiling, so the
/// two still read as one signal.
pub(crate) fn shimmer_across(line: &Line<'static>, app: &App, ctx: &Ctx) -> Line<'static> {
    let elapsed = app.image_anim.elapsed().as_secs_f32();
    let len = str_width(&line.to_string()) as u16;
    let mut col: u16 = 0;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for s in &line.spans {
        for ch in s.content.chars() {
            let level = cosense::theme::shimmer_level_across(col, len, elapsed);
            spans.push(Span::styled(
                ch.to_string(),
                cosense::theme::shimmer_style(s.style, ctx.terminal_bg, level),
            ));
            col += str_width(&ch.to_string()) as u16;
        }
    }
    Line::from(spans)
}

