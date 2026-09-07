//! UTF-8 caret coordinates, code indentation and display wrapping.

use super::*;

/// Display width of a string in terminal columns.
pub(crate) fn str_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
}

/// Hard-wrap `s` into segments of at most `w` display columns, preserving
/// every char in order (no trimming). Both the session line's display and
/// its caret math use THIS function, so the hardware cursor can never
/// disagree with the text it sits on. Always returns ≥ 1 segment.
/// The nearest char boundary at or below `i` — slicing a display segment
/// at a byte that lands mid-character would panic.
pub(crate) fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The caret line, wrapped the way it is drawn: the first row uses the
/// whole width, continuation rows are pushed in by `hang` so the text
/// lines up under its own first character instead of under the bullet.
///
/// READ has always wrapped like this; EDIT wrapped flat, so a long
/// bullet's continuation jumped back to the margin the moment you started
/// editing it. Every piece of caret arithmetic — the hardware cursor, a
/// click, ↑/↓, the selection highlight — has to agree with the drawing,
/// so they all read this one structure.
pub(crate) struct SessionWrap {
    pub(crate) segs: Vec<String>,
    pub(crate) hang: usize,
}

impl SessionWrap {
    pub(crate) fn new(disp: &str, width: usize, hang: usize) -> Self {
        let width = width.max(1);
        // A continuation narrower than this is worse than no hanging at
        // all (`wrap.rs` draws the same line).
        let hang = if hang > 0 && width.saturating_sub(hang) >= 8 { hang } else { 0 };
        let mut segs: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut used = 0usize;
        for ch in disp.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            let limit = if segs.is_empty() { width } else { width - hang };
            if cw > 0 && used + cw > limit && !cur.is_empty() {
                segs.push(std::mem::take(&mut cur));
                used = 0;
            }
            cur.push(ch);
            used += cw;
        }
        segs.push(cur);
        Self { segs, hang }
    }

    /// The indent drawn before row `i`.
    pub(crate) fn indent_of(&self, row: usize) -> usize {
        if row == 0 { 0 } else { self.hang }
    }

    /// (row, column ON SCREEN) of a display byte offset.
    pub(crate) fn row_col(&self, disp_byte: usize) -> (usize, usize) {
        let mut off = 0usize;
        for (i, seg) in self.segs.iter().enumerate() {
            let end = off + seg.len();
            if disp_byte < end || (disp_byte == end && i + 1 == self.segs.len()) {
                let within = str_width(&seg[..floor_boundary(seg, disp_byte - off)]);
                return (i, self.indent_of(i) + within);
            }
            off = end;
        }
        let last = self.segs.len().saturating_sub(1);
        let w = str_width(self.segs.last().map(String::as_str).unwrap_or(""));
        (last, self.indent_of(last) + w)
    }

    /// Display byte offset at a row and an on-screen column.
    pub(crate) fn offset_at(&self, row: usize, col: usize) -> usize {
        let row = row.min(self.segs.len().saturating_sub(1));
        let before: usize = self.segs.iter().take(row).map(String::len).sum();
        let col = col.saturating_sub(self.indent_of(row));
        before + self.segs.get(row).map(|s| byte_at_col(s, col)).unwrap_or(0)
    }
}

/// How far the caret line's continuation rows hang: the column its text
/// starts at, which is the bullet's width for an outline row and the code
/// gutter inside a `code:` block.
pub(crate) fn session_hang(buf: &str, code: Option<CodeSpan>) -> usize {
    match code {
        Some(span) if !span.outline_header() => span.gutter_cols(),
        _ => {
            let n = indent_of(buf).chars().count();
            if n == 0 { 0 } else { bullet_indent_width(n) + 2 }
        }
    }
}

/// Byte offset in `s` whose display column is closest to `col` (used by/// Byte offset in `s` whose display column is closest to `col` (used by
/// sticky-column ↑/↓ and by mouse clicks).
pub(crate) fn byte_at_col(s: &str, col: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > col {
            return i;
        }
        used += cw;
    }
    s.len()
}

/// Leading whitespace (the Scrapbox indent) of a line.
pub(crate) fn indent_of(s: &str) -> &str {
    let end = s.find(|c: char| !c.is_whitespace()).unwrap_or(s.len());
    &s[..end]
}

/// Display form of the session's raw line. One source whitespace character
/// remains one logical level, while each step after level 1 occupies two
/// terminal cells: level 1 `•`, level 2 `  •`, level 3 `    •`.
/// The DATA stays unchanged and the caret conversion below maps between the
/// compact source indent and its expanded display form.
/// How many of the first `n` characters of `buf` are really leading
/// whitespace — what `strip_leading_ws` would take off.
pub(crate) fn leading_ws_taken(buf: &str, n: usize) -> usize {
    buf.chars()
        .take(n)
        .take_while(|c| *c == ' ' || *c == '\t' || *c == '\u{3000}')
        .count()
}

/// The span a caret line is displayed and measured with: the raw span,
/// except a flush `code:` header, which stands without indent. Showing
/// it with the code gutter suggests a nesting level it does not have
/// (and READ shows it flush), so it is shown — and measured — as it is.
/// Nested headers, bodies and tables are untouched.
pub(crate) fn caret_span(app: &App, line: usize) -> Option<CodeSpan> {
    let span = app.raw_span_at_line(line)?;
    // A flush header — a `code:` OR `table:` line at the left margin —
    // edits as itself: READ shows it flush, so the caret row must show it
    // flush too. The block gutter would read as a nesting level the line
    // has not got (the table header used to indent itself on entering).
    let flush_header = line == span.header && span.header_indent == 0;
    if flush_header { None } else { Some(span) }
}

pub(crate) fn session_display(buf: &str, code: Option<CodeSpan>) -> String {
    // A TAB inside the line is the cell separator of a table, and
    // invisible everywhere else: at width zero the cells on either side
    // run together and the caret sits in a gap nobody can see. Show it as
    // one column — one character in, one out, so every caret offset still
    // maps. A tab in the INDENT is a level, not a cell, and is left to the
    // indent handling below.
    let mark = |text: &str| text.replace('\t', TAB_MARK);
    if let Some(span) = code.filter(|span| !span.outline_header()) {
        // Wear the renderer's code gutter, not the raw indent: the block's
        // base indent comes off and the same columns go back on, so the
        // caret line sits in the same column as the code around it.
        let strip = leading_ws_taken(buf, span.strip_chars());
        let rest: String = buf.chars().skip(strip).collect();
        return format!("{}{}", " ".repeat(span.gutter_cols()), mark(&rest));
    }
    let ind = indent_of(buf);
    let n = ind.chars().count();
    if n == 0 {
        return mark(buf);
    }
    let mut out = String::with_capacity(buf.len() + n + 4);
    out.push_str(&" ".repeat(bullet_indent_width(n)));
    out.push('\u{2022}');
    out.push(' ');
    out.push_str(&mark(&buf[ind.len()..]));
    out
}

/// Byte length of the `…• ` prefix in `session_display(buf)` (0 when the
/// line has no indent).
pub(crate) fn display_prefix_bytes(buf: &str) -> usize {
    let n = indent_of(buf).chars().count();
    if n == 0 { 0 } else { bullet_indent_width(n) + '•'.len_utf8() + 1 }
}

/// Byte offset in `session_display(buf, in_code)` for byte offset `caret`.

pub(crate) fn display_caret(buf: &str, caret: usize, code: Option<CodeSpan>) -> usize {
    // A caret that strayed mid-character — a mouse anchor computed against
    // another line's text is the classic way — is floored to the boundary:
    // the slice below would panic on it.
    let caret = floor_boundary(buf, caret);
    let ci = buf[..caret].chars().count();
    if let Some(span) = code.filter(|span| !span.outline_header()) {
        let strip = leading_ws_taken(buf, span.strip_chars());
        let di = if ci < strip { ci } else { span.gutter_cols() + ci - strip };
        let disp = session_display(buf, code);
        return disp.char_indices().nth(di).map(|(b, _)| b).unwrap_or_else(|| disp.len());
    }
    let n = indent_of(buf).chars().count();
    let di = if n == 0 {
        ci
    } else if ci < n {
        ci * 2
    } else {
        // The display prefix has 2n chars; the source prefix has n.
        ci + n
    };
    let disp = session_display(buf, None);
    disp.char_indices().nth(di).map(|(b, _)| b).unwrap_or_else(|| disp.len())
}

/// Inverse: a byte offset in `session_display(buf)` back to `buf`. A click
/// in either cell of one visual indent step maps to the nearest logical
/// source boundary; the injected space after `•` maps to the text start.
pub(crate) fn raw_caret_from_display(buf: &str, disp_byte: usize, code: Option<CodeSpan>) -> usize {
    let disp = session_display(buf, code);
    let di = disp[..floor_boundary(&disp, disp_byte)].chars().count();
    if let Some(span) = code.filter(|span| !span.outline_header()) {
        let strip = leading_ws_taken(buf, span.strip_chars());
        let gutter = span.gutter_cols();
        let ci = if di < gutter { di.min(strip) } else { strip + di - gutter };
        return buf.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(buf.len());
    }
    let n = indent_of(buf).chars().count();
    let ci = if n == 0 {
        di
    } else if di < n * 2 {
        (di + 1) / 2
    } else {
        di - n
    };
    buf.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(buf.len())
}
