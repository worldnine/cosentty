//! Width-adaptive table layout, ported/adapted from akapen's tui-markdown
//! `renderer/table.rs` (the column-shrink + CJK cell-wrap algorithm).
//!
//! A Scrapbox table (`table:name` + tab-separated rows) is buffered as
//! structured cells and laid out at *draw time* against the current pane
//! width. When the natural width fits, columns keep their natural widths
//! (light look: header separator only). When it does not, the fixed
//! overhead (borders + per-cell padding) is subtracted and the remainder is
//! shared proportionally to the natural widths, floored at each column's
//! longest unsplittable token; cell content then wraps across lines, and a
//! separator is drawn between every body row so wrapped boundaries stay
//! readable.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const H: char = '─';
const V: &str = "│";

/// A buffered table: first row is treated as the header (bold).
///
/// Every row carries the SOURCE line it came from (a Scrapbox table is one
/// source line per row), so the viewer's cursor, selection and comments
/// address table rows individually — akapen's tui-markdown attribution
/// model ("wrapped cell lines keep the source-line attribution").
#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    /// How the name row is drawn. Comes from the palette (the notation
    /// label colour `code:` uses), so a table follows the syntax theme
    /// like the rest of the body instead of a hard-coded colour.
    pub name_style: Style,
    /// Source line of the `table:name` opener.
    pub name_src: usize,
    /// rows[r] = (source line, cells); cells[col] = styled spans.
    pub rows: Vec<(usize, Vec<Vec<Span<'static>>>)>,
}

impl Table {
    pub fn column_count(&self) -> usize {
        self.rows.iter().map(|(_, r)| r.len()).max().unwrap_or(0)
    }

    /// Lay the table out within `available` display columns. Each output
    /// line carries the source line it belongs to: the name line and the
    /// top border to the opener, a separator to the row BELOW it (so a
    /// row's visual block includes its top rule), the bottom border to the
    /// last row.
    pub fn layout(&self, available: usize) -> Vec<(Line<'static>, usize)> {
        let cols = self.column_count();
        let mut out: Vec<(Line<'static>, usize)> = Vec::new();
        if !self.name.is_empty() {
            // The name row is also the way OUT of the table — Cosense
            // serves it as CSV and `Enter` saves it — which the underline
            // in `name_style` says, the same way it says it on a link.
            out.push((
                Line::from(Span::styled(
                    format!("table:{}", self.name),
                    self.name_style,
                )),
                self.name_src,
            ));
        }
        if cols == 0 {
            return out;
        }
        let (widths, constrained) = self.column_widths(cols, available);
        let bstyle = Style::default().fg(Color::DarkGray);

        out.push((border(&widths, '┌', '┬', '┐', bstyle), self.name_src));
        let mut last_src = self.name_src;
        for (ri, (src, row)) in self.rows.iter().enumerate() {
            if (constrained && ri > 0) || ri == 1 {
                out.push((border(&widths, '├', '┼', '┤', bstyle), *src));
            }
            let header = ri == 0;
            for line in render_row(row, &widths, header, bstyle) {
                out.push((line, *src));
            }
            last_src = *src;
        }
        out.push((border(&widths, '└', '┴', '┘', bstyle), last_src));
        out
    }

    /// Column widths + whether the table had to shrink to fit.
    fn column_widths(&self, cols: usize, available: usize) -> (Vec<usize>, bool) {
        let mut natural = vec![0usize; cols];
        let mut floors = vec![1usize; cols];
        for (_, row) in &self.rows {
            for (c, cell) in row.iter().enumerate() {
                natural[c] = natural[c].max(cell_width(cell));
                floors[c] = floors[c].max(longest_token(cell));
            }
        }
        for w in &mut natural {
            *w = (*w).max(1);
        }
        let overhead = 3 * cols + 1; // (n+1) borders + 2n padding
        let total: usize = natural.iter().sum();
        if total + overhead <= available {
            return (natural, false);
        }
        let budget = available.saturating_sub(overhead);
        if floors.iter().sum::<usize>() <= budget {
            return (proportional(&natural, &floors, budget), true);
        }
        if cols <= budget {
            let ones = vec![1usize; cols];
            return (proportional(&natural, &ones, budget), true);
        }
        (vec![1; cols], true)
    }
}

/// Distribute `budget` columns: each column keeps its floor, the remainder
/// is shared proportionally to natural widths, no column exceeds natural.
fn proportional(natural: &[usize], floors: &[usize], budget: usize) -> Vec<usize> {
    let sum_natural: usize = natural.iter().sum();
    if budget >= sum_natural {
        return natural.to_vec();
    }
    let sum_floor: usize = floors.iter().sum();
    let mut widths = floors.to_vec();
    let span = sum_natural.saturating_sub(sum_floor);
    if span == 0 || budget <= sum_floor {
        return widths;
    }
    let extra = budget - sum_floor;
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(natural[i].saturating_sub(floors[i])));
    let mut remainder = extra;
    for &i in &order {
        let share = natural[i].saturating_sub(floors[i]) * extra / span;
        widths[i] += share;
        remainder -= share;
    }
    for &i in &order {
        if remainder == 0 || widths[i] >= natural[i] {
            continue;
        }
        widths[i] += 1;
        remainder -= 1;
    }
    widths
}

fn cell_width(cell: &[Span]) -> usize {
    cell.iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Longest unsplittable token: a run of narrow non-space chars is one token;
/// each wide (CJK/emoji) char is its own breakable token.
fn longest_token(cell: &[Span]) -> usize {
    let mut longest = 0usize;
    let mut run = 0usize;
    for s in cell {
        for ch in s.content.chars() {
            let w = ch.width().unwrap_or(0);
            if ch.is_whitespace() {
                run = 0;
            } else if w >= 2 {
                longest = longest.max(run).max(w);
                run = 0;
            } else if w == 1 {
                run += 1;
            }
        }
    }
    longest.max(run)
}

fn border(widths: &[usize], l: char, mid: char, r: char, style: Style) -> Line<'static> {
    let mut s = String::new();
    s.push(l);
    for (i, w) in widths.iter().enumerate() {
        for _ in 0..(w + 2) {
            s.push(H);
        }
        if i + 1 < widths.len() {
            s.push(mid);
        }
    }
    s.push(r);
    Line::from(Span::styled(s, style))
}

/// Render one row (possibly multi-line when cells wrap) framed by borders.
fn render_row(
    row: &[Vec<Span<'static>>],
    widths: &[usize],
    header: bool,
    bstyle: Style,
) -> Vec<Line<'static>> {
    let empty: Vec<Span<'static>> = Vec::new();
    let wrapped: Vec<Vec<Vec<Span<'static>>>> = widths
        .iter()
        .enumerate()
        .map(|(c, &w)| {
            let cell = row.get(c).unwrap_or(&empty);
            wrap_cell(cell, w, header)
        })
        .collect();
    let height = wrapped.iter().map(|c| c.len()).max().unwrap_or(1);

    (0..height)
        .map(|li| {
            let mut spans: Vec<Span<'static>> = vec![Span::styled(V, bstyle)];
            for (c, &w) in widths.iter().enumerate() {
                let blank = vec![Span::raw(" ".repeat(w))];
                let content = wrapped[c].get(li).cloned().unwrap_or(blank);
                let used: usize = content
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                spans.push(Span::raw(" "));
                spans.extend(content);
                let pad = w.saturating_sub(used);
                spans.push(Span::raw(format!("{} ", " ".repeat(pad))));
                spans.push(Span::styled(V, bstyle));
            }
            Line::from(spans)
        })
        .collect()
}

/// CJK-aware greedy word wrap of a cell to `width` columns, preserving span
/// styles. Whitespace and wide chars are break opportunities; a token wider
/// than the column is split char-by-char (wide chars never split).
fn wrap_cell(spans: &[Span<'static>], width: usize, header: bool) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    // Flatten to (char, style).
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in spans {
        let st = if header {
            s.style.add_modifier(Modifier::BOLD)
        } else {
            s.style
        };
        for ch in s.content.chars() {
            chars.push((ch, st));
        }
    }

    let mut lines: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut cur_w = 0usize;
    let mut word: Vec<(char, Style)> = Vec::new();
    let mut word_w = 0usize;
    let mut pending_space: Option<(char, Style)> = None;

    let commit_word = |cur: &mut Vec<(char, Style)>,
                       cur_w: &mut usize,
                       word: &mut Vec<(char, Style)>,
                       word_w: &mut usize,
                       pending_space: &mut Option<(char, Style)>,
                       lines: &mut Vec<Vec<(char, Style)>>| {
        if word.is_empty() {
            return;
        }
        let w = std::mem::take(word);
        let ww = std::mem::take(word_w);
        let sp = pending_space.take();
        if cur.is_empty() {
            if ww <= width {
                *cur = w;
                *cur_w = ww;
            } else {
                for (ch, st) in w {
                    put(cur, cur_w, lines, width, ch, st);
                }
            }
            return;
        }
        let sp_w = usize::from(sp.is_some());
        if *cur_w + sp_w + ww <= width {
            if let Some(s) = sp {
                cur.push(s);
                *cur_w += 1;
            }
            cur.extend(w);
            *cur_w += ww;
        } else if ww <= width {
            lines.push(std::mem::take(cur));
            *cur_w = 0;
            *cur = w;
            *cur_w = ww;
        } else {
            lines.push(std::mem::take(cur));
            *cur_w = 0;
            for (ch, st) in w {
                put(cur, cur_w, lines, width, ch, st);
            }
        }
    };

    let mut i = 0;
    while i < chars.len() {
        let (ch, st) = chars[i];
        let w = ch.width().unwrap_or(0);
        if ch.is_whitespace() {
            commit_word(
                &mut cur,
                &mut cur_w,
                &mut word,
                &mut word_w,
                &mut pending_space,
                &mut lines,
            );
            pending_space = Some((ch, st));
            while i < chars.len() && chars[i].0.is_whitespace() {
                i += 1;
            }
            continue;
        }
        if w >= 2 {
            commit_word(
                &mut cur,
                &mut cur_w,
                &mut word,
                &mut word_w,
                &mut pending_space,
                &mut lines,
            );
            word.push((ch, st));
            word_w = w;
            commit_word(
                &mut cur,
                &mut cur_w,
                &mut word,
                &mut word_w,
                &mut pending_space,
                &mut lines,
            );
            i += 1;
            continue;
        }
        word.push((ch, st));
        word_w += w;
        i += 1;
    }
    commit_word(
        &mut cur,
        &mut cur_w,
        &mut word,
        &mut word_w,
        &mut pending_space,
        &mut lines,
    );
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }

    // Re-merge consecutive same-style chars into spans.
    lines
        .into_iter()
        .map(|line| {
            let mut out: Vec<Span<'static>> = Vec::new();
            let mut acc: Option<(Style, String)> = None;
            for (ch, st) in line {
                match &mut acc {
                    Some((s, t)) if *s == st => t.push(ch),
                    _ => {
                        if let Some((s, t)) = acc.take() {
                            out.push(Span::styled(t, s));
                        }
                        acc = Some((st, ch.to_string()));
                    }
                }
            }
            if let Some((s, t)) = acc {
                out.push(Span::styled(t, s));
            }
            out
        })
        .collect()
}

/// Append one char to the current line, wrapping when it does not fit.
fn put(
    cur: &mut Vec<(char, Style)>,
    cur_w: &mut usize,
    lines: &mut Vec<Vec<(char, Style)>>,
    width: usize,
    ch: char,
    st: Style,
) {
    let w = ch.width().unwrap_or(0);
    if w > 0 && *cur_w + w > width {
        lines.push(std::mem::take(cur));
        *cur_w = 0;
    }
    *cur_w += w;
    cur.push((ch, st));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(s: &str) -> Vec<Span<'static>> {
        vec![Span::raw(s.to_string())]
    }
    fn line_str(l: &(Line, usize)) -> String {
        l.0.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn natural_width_when_fits() {
        let t = Table {
            name_style: Style::default(),
            name: String::new(),
            name_src: 0,
            rows: vec![
                (1, vec![cell("abc"), cell("def")]),
                (2, vec![cell("12345"), cell("6789")]),
            ],
        };
        let lines: Vec<String> = t.layout(200).iter().map(line_str).collect();
        assert_eq!(lines[0], "┌───────┬──────┐");
        assert_eq!(lines[1], "│ abc   │ def  │");
    }

    #[test]
    fn rows_carry_their_source_lines() {
        let t = Table {
            name_style: Style::default(),
            name: "t".into(),
            name_src: 5,
            rows: vec![
                (6, vec![cell("h1"), cell("h2")]),
                (7, vec![cell("a"), cell("b")]),
                (8, vec![cell("c"), cell("d")]),
            ],
        };
        let out = t.layout(200);
        // name line + top border → opener; each row (and its separator
        // above) → that row's line; bottom border → last row
        let srcs: Vec<usize> = out.iter().map(|(_, s)| *s).collect();
        assert_eq!(srcs, vec![5, 5, 6, 7, 7, 8, 8]);
    }

    #[test]
    fn cjk_columns_align() {
        let t = Table {
            name_style: Style::default(),
            name: String::new(),
            name_src: 0,
            rows: vec![(1, vec![cell("長い長い文字列"), cell("短い文字列")])],
        };
        let lines: Vec<String> = t.layout(200).iter().map(line_str).collect();
        // every rendered line has equal display width
        let w0 = UnicodeWidthStr::width(lines[0].as_str());
        assert!(lines
            .iter()
            .all(|l| UnicodeWidthStr::width(l.as_str()) == w0));
    }

    #[test]
    fn shrinks_and_wraps_when_narrow() {
        let t = Table {
            name_style: Style::default(),
            name: String::new(),
            name_src: 0,
            rows: vec![
                (1, vec![cell("col"), cell("value")]),
                (2, vec![cell("これはとても長い日本語のセルです"), cell("x")]),
            ],
        };
        let lines: Vec<String> = t.layout(20).iter().map(line_str).collect();
        // no rendered line exceeds the available width
        assert!(lines
            .iter()
            .all(|l| UnicodeWidthStr::width(l.as_str()) <= 20));
        // content is preserved somewhere in the output
        let joined: String = lines.join("");
        assert!(joined.contains("日本語"));
    }
}
