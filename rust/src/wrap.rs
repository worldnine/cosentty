//! Width-aware line wrapping for styled ratatui lines, CJK-correct.
//!
//! Ported/adapted from akapen's `highlight::wrap_spans`: wrap a `Line`'s
//! spans into display rows no wider than `width` columns, measuring with
//! unicode-width so full-width CJK characters never straddle the boundary.
//! Span styles are preserved; a span split across rows keeps its style on
//! each fragment. Scrapbox indentation is already space-expanded upstream,
//! so tabs need no special handling here.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Wrap one styled line into rows fitting `width` columns. An empty line
/// yields one empty row (a blank source line stays one row).
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    wrap_line_hanging(line, width, 0)
}

/// Continuation rows narrower than this fall back to a flat wrap: a deep
/// bullet in a narrow pane must not degenerate into a one-word column.
const MIN_HANGING_BODY: usize = 8;

/// Wrap with a hanging indent: the first row uses the full `width`; every
/// continuation row is prefixed with `hang` spaces and wraps within
/// `width - hang`, so wrapped bullet text lines up under its own first
/// character instead of under the bullet:
///
/// ```text
///   • TAOのIDが重複していないかのチェックをするときに使う、頻出
///     文字列の抽出機能
/// ```
///
/// `hang` is usually [`hanging_indent`] of the line. When the body would
/// be narrower than [`MIN_HANGING_BODY`], the indent is dropped.
pub fn wrap_line_hanging(line: &Line<'static>, width: usize, hang: usize) -> Vec<Line<'static>> {
    let prefix = if hang > 0 { vec![Span::raw(" ".repeat(hang))] } else { Vec::new() };
    wrap_line_continued(line, width, &prefix)
}

/// Wrap with an arbitrary continuation `prefix` (usually
/// [`hanging_prefix`] of the line): every row after the first starts with
/// these spans and wraps within the remaining width. A blank prefix hangs
/// (bullets); a quote's prefix repeats its `┃ ` bar so the whole wrapped
/// quote reads as one quoted block:
///
/// ```text
/// ┃ 引用の一行目がここで折り返されると
/// ┃ 二行目にも縦棒が続く
/// ```
///
/// When the body would be narrower than [`MIN_HANGING_BODY`], the prefix
/// is dropped and the line wraps flat.
pub fn wrap_line_continued(line: &Line<'static>, width: usize, prefix: &[Span<'static>]) -> Vec<Line<'static>> {
    let width = width.max(1);
    let hang: usize = prefix.iter().map(|s| UnicodeWidthStr::width(s.content.as_ref())).sum();
    let hang = if hang > 0 && width.saturating_sub(hang) >= MIN_HANGING_BODY { hang } else { 0 };
    // Flatten to (char, style), remembering nothing else — we re-merge by
    // consecutive equal style at the end.
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        for ch in span.content.chars() {
            chars.push((ch, span.style));
        }
    }
    if chars.is_empty() {
        return vec![Line::from("")];
    }

    let mut rows: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    let mut col = 0usize;
    for (ch, st) in chars {
        let cw = ch.width().unwrap_or(0);
        // the first row owns the whole width; continuations leave room for
        // the hanging indent
        let limit = if rows.is_empty() { width } else { width - hang };
        if cw > 0 && col + cw > limit && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            col = 0;
        }
        cur.push((ch, st));
        col += cw;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }

    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut line = merge_row(row);
            if i > 0 && hang > 0 {
                let mut spans = prefix.to_vec();
                spans.append(&mut line.spans);
                line.spans = spans;
            }
            line
        })
        .collect()
}

/// Leading markers a wrapped continuation should hang under, each followed
/// by a space in the rendered line, and whether the marker itself is
/// REPEATED on continuation rows (a quote bar is; a bullet is replaced by
/// blank space of the same width).
const HANG_MARKERS: [(char, bool); 2] = [('•', false), ('┃', true)];

/// The continuation prefix of a rendered line (see
/// [`wrap_line_continued`]): its leading ASCII spaces, then — if a marker
/// follows — either that marker in its own style (`┃ `, repeated) or blank
/// space of the marker's width (`• `, hung under). A line without such a
/// prefix hangs by its spaces alone (code rows), a plain paragraph by
/// nothing (empty prefix). Only ASCII spaces count: the renderer has
/// already normalized source indentation (tabs, full-width spaces).
pub fn hanging_prefix(line: &Line<'static>) -> Vec<Span<'static>> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        for ch in span.content.chars() {
            chars.push((ch, span.style));
        }
    }
    let spaces = chars.iter().take_while(|(c, _)| *c == ' ').count();
    let mut prefix: Vec<Span<'static>> = Vec::new();
    if spaces > 0 {
        prefix.push(Span::raw(" ".repeat(spaces)));
    }
    if let (Some(&(m, style)), Some(&(' ', _))) = (chars.get(spaces), chars.get(spaces + 1)) {
        if let Some(&(_, repeat)) = HANG_MARKERS.iter().find(|(c, _)| *c == m) {
            if repeat {
                prefix.push(Span::styled(format!("{m} "), style));
            } else {
                prefix.push(Span::raw(" ".repeat(m.width().unwrap_or(1) + 1)));
            }
        }
    }
    prefix
}

/// The display width of [`hanging_prefix`]: how far continuation rows hang.
pub fn hanging_indent(line: &Line<'static>) -> usize {
    hanging_prefix(line)
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Re-merge consecutive characters that share a style into one span.
fn merge_row(row: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut acc: Option<(Style, String)> = None;
    for (ch, st) in row {
        match &mut acc {
            Some((s, t)) if *s == st => t.push(ch),
            _ => {
                if let Some((s, t)) = acc.take() {
                    spans.push(Span::styled(t, s));
                }
                acc = Some((st, ch.to_string()));
            }
        }
    }
    if let Some((s, t)) = acc {
        spans.push(Span::styled(t, s));
    }
    if spans.is_empty() {
        Line::from("")
    } else {
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line<'static> {
        Line::from(s.to_string())
    }
    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wraps_ascii_within_width() {
        let rows = wrap_line(&line("abcdefgh"), 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[0]), "abcd");
        assert_eq!(text(&rows[1]), "efgh");
    }

    #[test]
    fn never_splits_wide_char_and_stays_within_width() {
        let rows = wrap_line(&line("あいうえお"), 4);
        let joined: String = rows.iter().map(|r| text(r)).collect();
        assert_eq!(joined, "あいうえお");
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(text(r).as_str()) <= 4));
    }

    #[test]
    fn mixed_ascii_cjk() {
        let rows = wrap_line(&line("ab日本語cd"), 6);
        let joined: String = rows.iter().map(|r| text(r)).collect();
        assert_eq!(joined, "ab日本語cd");
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(text(r).as_str()) <= 6));
    }

    #[test]
    fn empty_line_stays_one_row() {
        let rows = wrap_line(&line(""), 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(text(&rows[0]), "");
    }

    #[test]
    fn hanging_indent_measures_whitespace_and_marker() {
        assert_eq!(hanging_indent(&line("plain paragraph")), 0);
        assert_eq!(hanging_indent(&line("• top-level bullet")), 2);
        assert_eq!(hanging_indent(&line("    • nested bullet")), 6);
        assert_eq!(hanging_indent(&line("┃ quoted")), 2);
        assert_eq!(hanging_indent(&line("    code row")), 4, "code hangs by its indent");
        assert_eq!(hanging_indent(&line("•no space is not a bullet")), 0);
        // the marker may sit in its own styled span, as render emits it
        let l = Line::from(vec![Span::raw("  "), Span::raw("• "), Span::raw("x")]);
        assert_eq!(hanging_indent(&l), 4);
    }

    #[test]
    fn continuation_rows_hang_under_the_bullet_text() {
        // "  • " + 12 chars in width 12: first row "  • abcdefgh", then the
        // rest hangs 4 columns in: "    ijkl"
        let rows = wrap_line_hanging(&line("  • abcdefghijkl"), 12, 4);
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(texts, vec!["  • abcdefgh", "    ijkl"]);
        // CJK: continuation width is measured, never split mid-character
        let rows = wrap_line_hanging(&line("• あいうえおかきくけこ"), 10, 2);
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(texts, vec!["• あいうえ", "  おかきく", "  けこ"]);
        assert!(rows.iter().all(|r| UnicodeWidthStr::width(text(r).as_str()) <= 10));
        // the joined body is unchanged apart from the inserted indent
        let body: String = texts.iter().map(|t| t.trim_start()).collect();
        assert_eq!(body, "• あいうえおかきくけこ");
    }

    #[test]
    fn quote_bar_repeats_on_continuation_rows() {
        use ratatui::style::Color;
        let bar = Style::default().fg(Color::Gray);
        let l = Line::from(vec![
            Span::raw("  "),
            Span::styled("┃ ", bar),
            Span::raw("引用の一行目がここで折り返されると二行目にも縦棒が続く"),
        ]);
        // the prefix is the indent plus the bar itself, in the bar's style
        let prefix = hanging_prefix(&l);
        let ptext: String = prefix.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(ptext, "  ┃ ");
        assert_eq!(prefix[1].style, bar);
        assert_eq!(hanging_indent(&l), 4);
        let rows = wrap_line_continued(&l, 20, &prefix);
        assert!(rows.len() >= 3);
        for (i, r) in rows.iter().enumerate() {
            let t = text(r);
            assert!(t.starts_with("  ┃ "), "row {i}: {t:?}");
            assert!(UnicodeWidthStr::width(t.as_str()) <= 20);
            // the repeated bar keeps the quote-bar style
            assert_eq!(r.spans.iter().find(|s| s.content.contains('┃')).unwrap().style, bar);
        }
        // a bullet's prefix is blank space of the same width instead
        let b = Line::from(vec![Span::raw("  "), Span::styled("• ", bar), Span::raw("x")]);
        let bp: String = hanging_prefix(&b).iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(bp, "    ");
        // a plain paragraph has no prefix at all
        assert!(hanging_prefix(&line("plain")).is_empty());
    }

    #[test]
    fn hanging_indent_is_dropped_when_the_body_would_be_too_narrow() {
        // width 12 with hang 6 leaves 6 < MIN_HANGING_BODY: flat wrap
        let rows = wrap_line_hanging(&line("      • abcdefghijklmnop"), 12, 6);
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert_eq!(texts, vec!["      • abcd", "efghijklmnop"]);
        // hang 0 is exactly wrap_line
        let l = line("abcdefgh");
        assert_eq!(
            wrap_line_hanging(&l, 4, 0).iter().map(text).collect::<Vec<_>>(),
            wrap_line(&l, 4).iter().map(text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn preserves_span_styles_across_wrap() {
        use ratatui::style::{Color, Stylize};
        let l = Line::from(vec![
            Span::raw("aa"),
            Span::styled("bbbb", Style::default().fg(Color::Red)),
        ]);
        let rows = wrap_line(&l, 3);
        // "aab" | "bbb"
        let joined: String = rows.iter().map(|r| text(r)).collect();
        assert_eq!(joined, "aabbbb");
        // the red style survives on the fragments
        let red_found = rows.iter().any(|r| {
            r.spans.iter().any(|s| s.style.fg == Some(Color::Red))
        });
        assert!(red_found);
    }
}
