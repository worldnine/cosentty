//! Width-aware text helpers and comment/composer cards.

use super::*;

/// Truncate `s` to at most `w` columns, appending `…` when cut.
pub(crate) fn truncate_width(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if str_width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// One related-pages row: `title · value  description` (the value being
/// the one the section is sorted on, see `RelEntry::sort_meta`), the title in
/// `title_style` (see `related_rows`: blue while unread, plain once seen)
/// and the rest dim, truncated to the pane width — related rows do not
/// wrap, being a scannable list rather than body text.
pub(crate) fn related_row(
    e: &RelEntry,
    text_w: usize,
    title_style: Style,
    sort: cosense::index::SortKey,
) -> Line<'static> {
    let dim = Style::default().fg(CHROME_DIM);
    let title = truncate_width(&e.title, text_w);
    let mut spans = vec![Span::styled(title.clone(), title_style)];
    let mut used = str_width(&title);
    let value = e.sort_meta(sort);
    if !value.is_empty() {
        let meta = format!(" · {value}");
        if used + str_width(&meta) <= text_w {
            used += str_width(&meta);
            spans.push(Span::styled(meta, dim));
        }
    }
    if !e.desc.is_empty() && used + 2 < text_w {
        let desc = truncate_width(&e.desc, text_w - used - 2);
        spans.push(Span::styled(format!("  {desc}"), dim));
    }
    Line::from(spans)
}

/// The block a comment (and the composer) is drawn as. Flat, like the
/// rest of this viewer's chrome: no rules, no borders — a band of this
/// background under the lines, with the title as a badge in the top-left
/// corner (black text on the badge colour, the menu panels' title style).
pub(crate) const CARD_BG: Color = Color::Black;

/// A saved comment as a flat block under its lines: the title badge
/// (` comment · 12-14 `, black on yellow) on the first row, the wrapped
/// text, and one blank row of the band below, so the block has the same
/// shape the composer had — Enter turns one into the other with only the
/// badge colour changing. akapen's placement (under the range) with this
/// viewer's flat look instead of akapen's rules.
pub(crate) fn card_lines(c: &Comment, width: usize) -> Vec<Line<'static>> {
    let title = Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD);
    // A comment on a past revision says which one on its title, so a
    // reader stepping through history knows the card is about THIS version.
    let label = match c.revision_label() {
        Some(at) => format!(" comment · {} · {at} ", c.range_label()),
        None => format!(" comment · {} ", c.range_label()),
    };
    bar_lines(&label, title, c.text.lines().flat_map(|l| wrap_plain(l, width)).collect(), width)
}

/// The block shape both the card and the composer use: the badge on the
/// first row, `body` rows, one blank row — every row filled to `width`
/// with the block's background.
fn bar_lines(label: &str, title: Style, body: Vec<String>, width: usize) -> Vec<Line<'static>> {
    let band = Style::default().bg(CARD_BG);
    let pad = |used: usize| " ".repeat(width.saturating_sub(used));
    let mut out = vec![Line::from(vec![
        Span::styled(label.to_string(), title),
        Span::styled(pad(str_width(label)), band),
    ])];
    out.extend(body.into_iter().map(|row| {
        let used = str_width(&row);
        Line::from(vec![Span::styled(row, band), Span::styled(pad(used), band)])
    }));
    out.push(Line::from(Span::styled(pad(0), band)));
    out
}

/// The comment composer as rows under the commented range: the card's
/// block with a cyan badge, ` comment · 12-14 ` (or ` edit · 12-14 ` when replacing
/// an existing comment), the input wrapped to the column, and the
/// insertion point's cell recorded so the hardware cursor — and with it
/// the IME's composition window — sits in the bar.
pub(crate) fn composer_rows(
    input: &Input,
    range: (usize, usize),
    editing: bool,
    revision: Option<String>,
    width: usize,
    max_body: usize,
) -> Vec<Row> {
    let width = width.max(1);
    let title = Style::default().fg(Color::Black).bg(CHROME_ACCENT).add_modifier(Modifier::BOLD);
    let label = if range.0 == range.1 {
        format!("{} {}", if editing { " edit ·" } else { " comment ·" }, range.0 + 1)
    } else {
        format!("{} {}-{}", if editing { " edit ·" } else { " comment ·" }, range.0 + 1, range.1 + 1)
    };
    let label = match revision {
        Some(at) => format!("{label} · {at} "),
        None => format!("{label} "),
    };
    let (before, _) = input.parts();
    let (mut body, (mut crow, ccol)) = wrap_with_caret(&input.buf, before.chars().count(), width);
    // A draft taller than the pane would push its own caret off screen
    // (the whole bar is kept visible, and there is no "whole" that fits).
    // Show a window of `max_body` rows around the caret instead, and say
    // how many rows are folded away above and below on the band rows.
    let (mut above, mut below) = (0usize, 0usize);
    if body.len() > max_body {
        let first = crow.saturating_sub(max_body / 2).min(body.len() - max_body);
        above = first;
        below = body.len() - (first + max_body);
        body = body[first..first + max_body].to_vec();
        crow -= first;
    }
    let mut rows: Vec<Row> = bar_lines(&label, title, body, width)
        .into_iter()
        .map(|line| Row::Composer { line, caret: None })
        .collect();
    if let Some(Row::Composer { caret, .. }) = rows.get_mut(1 + crow) {
        *caret = Some(ccol.min(width - 1) as u16);
    }
    let dim = Style::default().fg(CHROME_DIM).bg(CARD_BG);
    let fold = |n: usize, arrow: &str| -> Line<'static> {
        let text = t!("{arrow} あと {n} 行", "{arrow} {n} more line(s)");
        let pad = " ".repeat(width.saturating_sub(str_width(&text)));
        Line::from(vec![Span::styled(text, dim), Span::styled(pad, dim)])
    };
    if above > 0 {
        // The badge row keeps the badge; the fold note takes its tail.
        if let Some(Row::Composer { line, .. }) = rows.first_mut() {
            let badge = line.spans[0].clone();
            let note = t!("↑ あと {above} 行 ", "↑ {above} more line(s) ");
            let used = str_width(badge.content.as_ref()) + str_width(&note);
            let band = Style::default().bg(CARD_BG);
            *line = Line::from(vec![
                badge,
                Span::styled(" ".repeat(width.saturating_sub(used)), band),
                Span::styled(note, dim),
            ]);
        }
    }
    if below > 0 {
        if let Some(Row::Composer { line, .. }) = rows.last_mut() {
            *line = fold(below, "↓");
        }
    }
    rows
}

/// `wrap_plain`, also reporting the cell the caret (after `caret_chars`
/// characters) lands in: `(row, display column)`. A caret between a full
/// row and the next character sits where that character goes — the start
/// of the next row. Only at the very end of the text can it sit on a full
/// row's right edge; the drawer clamps that one cell.
pub(crate) fn wrap_with_caret(s: &str, caret_chars: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    use unicode_width::UnicodeWidthChar;
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    let mut caret: Option<(usize, usize)> = None;
    for (i, ch) in s.chars().enumerate() {
        // A line break in the draft (^j) ends the row where it stands; a
        // caret sitting on it belongs to the row it ends.
        if ch == '\n' {
            if i == caret_chars {
                caret = Some((out.len(), w));
            }
            out.push(std::mem::take(&mut cur));
            w = 0;
            continue;
        }
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        if i == caret_chars {
            caret = Some((out.len(), w));
        }
        cur.push(ch);
        w += cw;
    }
    let caret = caret.unwrap_or((out.len(), w));
    out.push(cur);
    (out, caret)
}

pub(crate) fn wrap_plain(s: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

