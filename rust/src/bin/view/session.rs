use super::*;

/// The modeless edit session (SPEC-edit-session.md): the caret line shows
/// raw source and takes every printable key; ↑/↓ walk lines, Enter splits
/// or creates lines, BOL-Backspace joins — the session spans any number of
/// lines. The only dirty state is THIS line's working text; everything
/// structural commits immediately, text commits when the caret leaves.
pub(crate) struct EditSession {
    /// Caret line (index into `app.lines`; mirrored to `app.cursor`).
    pub(crate) line: usize,
    /// Working text + caret of the caret line.
    pub(crate) input: Input,
    /// Last committed text of this line (`dirty = input.buf != orig`).
    pub(crate) orig: String,
    /// Sticky display column for ↑/↓ over lines of differing length.
    pub(crate) want_col: Option<usize>,
    /// Character selection: the fixed end of the range as a
    /// (line, byte) position, with the caret as the moving one. EDIT has
    /// no line-range concept at all — a range may cross lines, and every
    /// operation on it (typing, ⌫, Enter, paste, copy) is text-unit: the
    /// lines it spans merge or split the way a textarea's would. `None`,
    /// or an anchor sitting exactly on the caret, selects nothing.
    pub(crate) sel_from: Option<(usize, usize)>,
}

impl EditSession {
    /// The selection as two (line, byte) ends in DOCUMENT order (top
    /// first), `None` when nothing is selected. The caret is always one of
    /// the two ends — the head a drag or a Shift-move last reached.
    pub(crate) fn sel_ends(&self) -> Option<((usize, usize), (usize, usize))> {
        let from = self.sel_from?;
        let head = (self.line, self.input.cur);
        (from != head).then(|| if from <= head { (from, head) } else { (head, from) })
    }

    /// The selected byte range WHEN IT FITS ON THE CARET LINE, normalised
    /// — the unit the local, uncommitted edits (cut, wrap, bracket math)
    /// work in. A range that crossed into another line answers `None`:
    /// touching it is structural, and structural edits commit.
    pub(crate) fn sel_span(&self) -> Option<(usize, usize)> {
        let ((a, x), (b, y)) = self.sel_ends()?;
        (a == b).then_some((x, y))
    }

    /// The covered byte range OF THE CARET LINE for whatever selection
    /// exists: the caret line is always one end of the range, so a range
    /// off this line reaches from the caret to that line's near edge.
    /// This is what draws reversed on the raw caret line; the other lines
    /// the range covers are painted as whole-line bands.
    pub(crate) fn caret_sel_bytes(&self) -> Option<(usize, usize)> {
        let ((tl, tb), (bl, bb)) = self.sel_ends()?;
        if tl == bl {
            Some((tb, bb))
        } else if self.line == tl {
            Some((tb, self.input.buf.len()))
        } else {
            Some((0, bb))
        }
    }
}

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

/// Open the session on body line `line` with the caret at byte `caret`.
pub(crate) fn enter_session(app: &mut App, ctx: &Ctx, line: usize, caret: usize) {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.toast_err(t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)"));
        return;
    }
    if line >= app.lines.len() {
        app.toast_err(t!("関連ページの行は編集できません（o でページ末尾に行を足せます）", "related rows cannot be edited (o adds a line at the end)"));
        return;
    }
    let text = app.lines[line].text.clone();
    let caret = caret.min(text.len());
    // snap to a char boundary
    let caret = (0..=caret).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    app.session = Some(EditSession {
        line,
        input: Input { buf: text.clone(), cur: caret },
        orig: text,
        want_col: None,
        sel_from: None,
    });
    app.cursor = line;
    app.selection = None;
    app.follow = true;
    app.laid_width = 0;
    app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
    // The footer's EDIT badge and key hint say the rest; a stale READ
    // status (a selection hint) must not ride along into the session.
    app.status.clear();
}

/// Commit the caret line's text if it changed (called whenever the caret
/// leaves the line, and on Esc).
pub(crate) fn session_commit_dirty(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    if s.input.buf == s.orig {
        return;
    }
    let (line, buf) = (s.line, s.input.buf.clone());
    let id = app.lines[line].id.clone();
    let label = format!("line {}", line + 1);
    do_edit(app, ctx, &label, vec![EditOp::Replace { id, text: buf.clone() }]);
    if let Some(s) = app.session.as_mut() {
        s.orig = buf;
    }
}

/// Drop the session state and go back to READ + ASCII. Commits nothing —
/// callers decide what to do with the dirty line first.
pub(crate) fn close_session(app: &mut App) {
    app.session = None;
    app.ime_guard = None;
    app.laid_width = 0;
}

/// Leave the session (Esc): commit the dirty line, back to READ + ASCII.
pub(crate) fn leave_session(app: &mut App, ctx: &Ctx) {
    session_commit_dirty(app, ctx);
    close_session(app);
    app.note(t!("✓ 完了", "✓ done"));
}

/// `^z` (undo) / `^r` (redo) inside the session — SPEC §6 says the safety
/// net works in EDIT too, not only in READ.
///
/// The dirty line commits first, so undo means "take back what I just
/// typed" instead of skipping over it to the commit before.
///
/// Re-seating afterwards is the whole difficulty: an undo can move the
/// caret line, rewrite it, or take it away entirely. The line **id** is
/// what survives an undo (indices do not), so that is what we follow. When
/// the line itself is undone away — which is exactly what happens when you
/// keep pressing undo past the point where you created it — the caret
/// falls back to the line above, at its end, the way any editor does.
/// Pressing undo N times must never eject you from EDIT.
pub(crate) fn session_history(app: &mut App, ctx: &Ctx, back: bool) {
    session_commit_dirty(app, ctx);
    let Some(s) = app.session.as_ref() else { return };
    let id = app.lines[s.line].id.clone();
    // Where to land if this very line is undone away.
    let fallback = s.line.checked_sub(1).map(|i| app.lines[i].id.clone());
    let col = s.want_col.unwrap_or_else(|| str_width(&s.input.buf[..s.input.cur]));
    let stack_was_empty = if back { app.undo_stack.is_empty() } else { app.redo_stack.is_empty() };
    let seated = if back { undo(app, ctx) } else { redo(app, ctx) };
    if stack_was_empty {
        return; // nothing happened; the status line already says so
    }
    // undo/redo put the caret on what they changed. Only when that line is
    // gone (nothing to stand on) does the session need placing by hand.
    if seated {
        return;
    }
    let (line, col) = match app.lines.iter().position(|l| l.id == id) {
        Some(line) => (line, col),
        None => {
            let line = fallback
                .and_then(|prev| app.lines.iter().position(|l| l.id == prev))
                .unwrap_or_else(|| app.cursor.min(app.lines.len().saturating_sub(1)));
            (line, str_width(&app.lines[line].text)) // caret at end of line
        }
    };
    let text = app.lines[line].text.clone();
    let caret = byte_at_col(&text, col);
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = Some(col);
    }
    app.cursor = line;
    app.follow = true;
    app.laid_width = 0;
}

/// Move the session's caret to `line`, committing the line it leaves —
/// what ↑/↓ do, addressed by line number instead of by direction.
pub(crate) fn session_move_to_line(app: &mut App, ctx: &Ctx, line: usize) {
    let Some(s) = app.session.as_ref() else { return };
    if s.line == line || line >= app.lines.len() {
        return;
    }
    session_commit_dirty(app, ctx);
    let text = app.lines[line].text.clone();
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: text.len() };
        s.orig = text;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = line;
}

/// What a single-line paste should actually insert.
///
/// * a URL pasted over a selection → `[selected text URL]`, the link form
/// * an image URL pasted on its own → `[URL]`, which is what draws it
/// * anything else → exactly what was pasted
pub(crate) fn paste_shape(app: &App, pasted: &str) -> String {
    let url = pasted.trim();
    let is_url = url.starts_with("http://") || url.starts_with("https://");
    if !is_url || url.split_whitespace().count() != 1 {
        return pasted.to_string();
    }
    if let Some(s) = app.session.as_ref() {
        if let Some((a, b)) = s.sel_span() {
            let label = s.input.buf[a..b].trim();
            if !label.is_empty() && !label.contains('[') && !label.contains(']') {
                return format!("[{label} {url}]");
            }
        }
    }
    let draws = cosense::render::gyazo_permalink(url).is_some()
        || cosense::render::looks_like_image_url(url);
    if draws {
        format!("[{url}]")
    } else {
        pasted.to_string()
    }
}

/// `session_cut_span` for a session already borrowed mutably.
pub(crate) fn session_cut_span_in(s: &mut EditSession) {
    if let Some((a, b)) = s.sel_span() {
        s.input.buf.replace_range(a..b, "");
        s.input.cur = a;
        s.sel_from = None;
    }
}

/// Drop the character selection's text from the caret line. Stays local
/// (the line is simply dirty afterwards), like any other typing: the line
/// commits when the caret leaves it.
pub(crate) fn session_cut_span(app: &mut App) -> bool {
    let Some(s) = app.session.as_mut() else { return false };
    if s.sel_span().is_none() {
        return false;
    }
    session_cut_span_in(s);
    s.want_col = None;
    app.laid_width = 0;
    app.follow = true;
    true
}

/// The one answer to "type over a selection": the range disappears and
/// `text` stands in its place, the caret after it. A range inside one
/// line is just the buffer editing it was made of — local, dirty, it
/// commits when the caret leaves. A range ACROSS lines is structural: it
/// is the anchor line's head, `text`, and the far line's tail, with every
/// line wholly between them deleted — one commit, one undo step, exactly
/// as every split and join already is. (Cosense web's textarea does the
/// same; here a page's lines ARE the units, so "merge" is the honest
/// spelling of "replace".) Returns whether a selection was consumed.
pub(crate) fn session_replace_selection(app: &mut App, ctx: &Ctx, text: &str) -> bool {
    let Some(((tl, tb), (bl, bb))) =
        app.session.as_ref().and_then(|s| s.sel_ends())
    else {
        return false;
    };
    if tl == bl {
        let Some(s) = app.session.as_mut() else { return false };
        let Some((a, b)) = s.sel_span() else { return false };
        s.input.buf.replace_range(a..b, text);
        s.input.cur = a + text.len();
        s.sel_from = None;
        s.want_col = None;
        app.laid_width = 0;
        app.follow = true;
        return true;
    }
    // The endpoints' lines: the caret line's working buffer is the truth
    // for its own end (nothing is typed while a selection lives — every
    // key that makes one commits on the way — but the seat itself can
    // carry an uncommitted split). Commit anyway: it is a no-op unless so.
    if bl >= app.lines.len() {
        // Stale far end (a remote edit reshaped the page mid-selection):
        // the range dies here, unseen, and the keystroke goes unfed.
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
        return false;
    }
    session_commit_dirty(app, ctx);
    let (top_text, bot_text) = (
        app.lines[tl].text.clone(),
        app.lines[bl].text.clone(),
    );
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let merged = format!("{prefix}{text}{suffix}");
    let mut ops = vec![EditOp::Replace {
        id: app.lines[tl].id.clone(),
        text: merged.clone(),
    }];
    ops.extend((tl + 1..=bl).map(|i| EditOp::Delete { id: app.lines[i].id.clone() }));
    do_edit(app, ctx, &t!("選択範囲の置換", "replace selection"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = tl;
        s.input = Input { buf: merged.clone(), cur: prefix.len() + text.len() };
        s.orig = merged;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = tl;
    app.follow = true;
    true
}

/// Enter with a selection across lines: the range is replaced by the
/// line break itself — the anchor line keeps its head, the far line its
/// tail, the lines wholly between are deleted, and the caret opens the
/// tail. (A same-line range answers false: cutting it and splitting at
/// the junction is the very same edit, and `session_split` does that on
/// its way through.)
pub(crate) fn session_enter_selection(app: &mut App, ctx: &Ctx) -> bool {
    let Some(((tl, tb), (bl, bb))) =
        app.session.as_ref().and_then(|s| s.sel_ends())
    else {
        return false;
    };
    if tl == bl {
        return false;
    }
    // A remote edit that reshaped the page can leave the far end standing
    // past its last line. The range was about to die at the next draw
    // anyway; it dies here, unseen.
    if bl >= app.lines.len() {
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
        return false;
    }
    session_commit_dirty(app, ctx);
    let (top_text, bot_text) = (
        app.lines[tl].text.clone(),
        app.lines[bl].text.clone(),
    );
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let mut ops = vec![
        EditOp::Replace { id: app.lines[tl].id.clone(), text: prefix },
        EditOp::Replace { id: app.lines[bl].id.clone(), text: suffix.clone() },
    ];
    ops.extend((tl + 1..bl).map(|i| EditOp::Delete { id: app.lines[i].id.clone() }));
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    // The middle lines are gone: the far line now sits one below the
    // anchor, and it is where the caret goes on.
    let seat = tl + 1;
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input { buf: suffix.clone(), cur: 0 };
        s.orig = suffix;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = seat;
    app.follow = true;
    true
}

/// Shift+↑/↓ in READ: grow a line range from wherever the cursor is.
/// Anchors on the first press; after that the ordinary cursor move carries
/// the far end (`after_cursor_move`).
pub(crate) fn read_select_line(app: &mut App, down: bool) {
    if app.selection.is_none() {
        app.selection = Some(Selection::new(app.cursor));
    }
    app.move_cursor(down);
    if let Some((a, b)) = app.selection.map(|s| s.range()) {
        app.status = t!("{} 行を選択 · y コピー · c コメント · Esc 解除", "selected {} line(s) · y copy · c comment · Esc clear", b - a + 1);
    }
}

/// `^t`: step the caret line's heading up a level, and off the top back
/// to plain text.
///
/// Cosense has no official shortcut for this — the community UserScript
/// uses `Ctrl+8` (`*` is Shift+8), which a terminal cannot offer: Ctrl+8
/// arrives as 0x7F, indistinguishable from Backspace. One key that CYCLES
/// gives both directions without a modifier the terminal mangles, and
/// matches how the levels are used in practice: keep pressing until it
/// looks right.
pub(crate) fn session_cycle_heading(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (buf, cur) = (s.input.buf.clone(), s.input.cur);
    let indent = indent_of(&buf).to_string();
    let body = &buf[indent.len()..];
    let (stars, text) = match heading_body(body) {
        Some((n, t)) => (Some(n), t.to_string()),
        None => (None, body.to_string()),
    };
    // 1 → 2 → 3 → 4 → plain → 1 …
    let next = match stars {
        None => Some(1),
        Some(n) if n < 4 => Some(n + 1),
        _ => None,
    };
    let new_body = match next {
        Some(n) => format!("[{} {text}]", "*".repeat(n)),
        None => text.clone(),
    };
    // Keep the caret on the same character of the TEXT, wherever the
    // notation around it moved to.
    let old_text_at = indent.len() + stars.map(|n| n + 2).unwrap_or(0);
    let new_text_at = indent.len() + next.map(|n| n + 2).unwrap_or(0);
    let within = cur.saturating_sub(old_text_at).min(text.len());
    let new_cur = new_text_at + within;
    if let Some(s) = app.session.as_mut() {
        s.input = Input { buf: format!("{indent}{new_body}"), cur: new_cur };
        s.want_col = None;
        s.sel_from = None;
    }
    app.laid_width = 0;
    app.follow = true;
    let _ = ctx;
    app.note(match next {
        Some(n) => t!("見出し レベル{n}（^t でさらに）", "heading level {n} (^t for more)"),
        None => t!("見出しを解除（^t で再び）", "heading cleared (^t to start again)"),
    });
}

/// `[** text]` → `(2, "text")`. Only a WHOLE line counts: a heading is the
/// line, not a run inside it.
pub(crate) fn heading_body(body: &str) -> Option<(usize, &str)> {
    let inner = body.strip_prefix('[')?.strip_suffix(']')?;
    let stars = inner.chars().take_while(|c| *c == '*').count();
    if stars == 0 {
        return None;
    }
    inner[stars..].strip_prefix(' ').map(|t| (stars, t))
}

/// Is the caret line inside a `code:` block?/// Is the caret line inside a `code:` block?
pub(crate) fn session_in_code(app: &App) -> bool {
    app.session
        .as_ref()
        .map(|s| s.line)
        .map(|line| app.code_span_at_line(line).is_some())
        .unwrap_or(false)
}

/// Is the caret sitting inside an empty `[]`?/// Is the caret sitting inside an empty `[]`?
pub(crate) fn empty_pair_at_caret(app: &App) -> bool {
    app.session
        .as_ref()
        .map(|s| {
            s.input.buf[..s.input.cur].ends_with('[') && s.input.buf[s.input.cur..].starts_with(']')
        })
        .unwrap_or(false)
}

/// One printable key in the session, with the bracket habits cosense web
/// has: `[` opens a pair (or wraps the selection), and `]` typed where one
/// is already waiting steps over it instead of doubling.
///
/// Brackets are how everything is written in Cosense — links, images,
/// decoration — so typing the closing half by hand every time is the
/// single most repeated keystroke there is.
pub(crate) fn session_type_char(app: &mut App, ctx: &Ctx, ch: char) {
    // Not inside a `code:` block. There, notation is off by contract —
    // links, images and quotes all stop working — and brackets are just
    // characters someone is typing on purpose (`[0]`, `[\n`). Helping
    // would be the one place the block leaks.
    let in_code = session_in_code(app);
    // A space typed at the head of a Mermaid header is the other way people
    // nest a line, so it moves the block instead of landing in the text.
    if ch == ' ' && caret_on_mermaid_indent_or_start(app) {
        session_indent(app, ctx, 1);
        return;
    }
    // A typed character replaces the selection — all of it, wherever the
    // range ends. The bracket habits below only make sense inside one
    // line; a range across lines just takes the character the plain way.
    let cross = app
        .session
        .as_ref()
        .and_then(|s| s.sel_ends())
        .is_some_and(|((a, _), (b, _))| a != b);
    match ch {
        '[' if !in_code && !cross => {
            // With a selection, the brackets go AROUND it: selecting a word
            // and pressing `[` is how a link gets made.
            let selected = app.session.as_ref().and_then(|s| {
                let (a, b) = s.sel_span()?;
                Some(s.input.buf[a..b].to_string())
            });
            session_cut_span(app);
            if let Some(s) = app.session.as_mut() {
                match selected {
                    Some(text) => {
                        let wrapped = format!("[{text}]");
                        s.input.insert_str(&wrapped);
                    }
                    None => {
                        s.input.insert_str("[]");
                        s.input.cur -= 1; // between the pair
                    }
                }
                s.want_col = None;
            }
        }
        ']' if !in_code
            && !cross
            && app
                .session
                .as_ref()
                .map(|s| s.input.buf[s.input.cur..].starts_with(']'))
                .unwrap_or(false)
            && app.session.as_ref().and_then(|s| s.sel_span()).is_none() =>
        {
            if let Some(s) = app.session.as_mut() {
                s.input.right();
                s.want_col = None;
            }
        }
        _ => {
            let mut tmp = [0u8; 4];
            if !session_replace_selection(app, ctx, ch.encode_utf8(&mut tmp)) {
                if let Some(s) = app.session.as_mut() {
                    s.input.insert_char(ch);
                    s.want_col = None;
                }
            }
        }
    }
    app.laid_width = 0;
    app.follow = true;
}

/// Shift+←/→: grow (or start) the character selection as the caret moves.
pub(crate) fn session_select_char(app: &mut App, right: bool) {
    if let Some(s) = app.session.as_mut() {
        if s.sel_from.is_none() {
            s.sel_from = Some((s.line, s.input.cur));
        }
        if right {
            s.input.right();
        } else {
            s.input.left();
        }
        s.want_col = None;
        // A selection collapsed back onto its anchor is no selection.
        if s.sel_from == Some((s.line, s.input.cur)) {
            s.sel_from = None;
        }
    }
    app.selection = None;
    app.laid_width = 0;
    app.follow = true;
}

/// Shift+↑/↓ inside the session: carry the caret one line and grow the
/// CHARACTER range with it — the very selection the mouse drags, ended
/// in (line, byte) positions and covering the lines between. The caret
/// line commits on the way, exactly as a plain ↑/↓ does — selecting must
/// never be the thing that loses a keystroke.
pub(crate) fn session_select_line(app: &mut App, ctx: &Ctx, delta: i32) {
    let Some(s) = app.session.as_ref() else { return };
    let anchor = s.sel_from.unwrap_or((s.line, s.input.cur));
    session_move_line(app, ctx, delta);
    let Some(head) = app.session.as_ref().map(|s| (s.line, s.input.cur)) else {
        return;
    };
    if head == anchor {
        // The caret could not move (the page's edge): the press anchors
        // nothing, and whatever the move said about being at the end
        // stands.
        return;
    }
    if let Some(s) = app.session.as_mut() {
        s.sel_from = Some(anchor);
    }
    app.selection = None;
    let n = anchor.0.abs_diff(head.0) + 1;
    app.status = t!("{} 行を選択 · ⌫ 削除 · Esc 解除", "selected {} line(s) · ⌫ delete · Esc clear", n);
}

/// `^k` inside the session — the emacs contract, one line at a time:
/// kill to the end of the line, and when there is nothing left to kill,
/// kill the line itself. So `^k^k` deletes a line without leaving EDIT,
/// which is what makes deletion an editing gesture rather than a READ-mode
/// keystroke.
///
/// Deleting is structural, so it commits at once (as joins and splits do).
/// The dirty text commits first: undo has to give back the line you were
/// looking at, not the last version the server happened to hold.
pub(crate) fn session_kill(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, at_eol) = (s.line, s.input.cur == s.input.buf.len());
    if !at_eol {
        if let Some(s) = app.session.as_mut() {
            s.input.kill_to_end();
            s.want_col = None;
            s.sel_from = None;
        }
        app.laid_width = 0;
        app.follow = true;
        return;
    }
    // Line 0 is the page title: deleting it renames the page (same reason
    // `x` refuses). Emptying it is still allowed — that is a rename you
    // typed on purpose.
    if line == 0 {
        app.toast_err(t!("タイトル行は削除できません", "the title line cannot be deleted"));
        return;
    }
    session_commit_dirty(app, ctx);
    let id = app.lines[line].id.clone();
    do_edit(app, ctx, &t!("行の削除", "delete line"), vec![EditOp::Delete { id }]);
    // The caret takes the place the line left behind: the line that slid
    // up into this index, or the one above when we killed the last line.
    let seat = line.min(app.lines.len().saturating_sub(1));
    let text = app.lines[seat].text.clone();
    let caret = if seat < line { text.len() } else { 0 };
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = None;
    }
    app.cursor = seat;
    app.follow = true;
    app.laid_width = 0;
    app.note(t!("✓ 1行削除 · ^z で戻せます", "✓ deleted line · ^z to undo"));
}

/// ↑/↓ inside the session: commit the dirty line, carry the caret to the
/// next/previous BODY line, keeping the display column (sticky).
pub(crate) fn session_move_line(app: &mut App, ctx: &Ctx, delta: i32) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf, cur) = (s.line, s.input.buf.clone(), s.input.cur);
    let code = app.raw_span_at_line(line);
    let width = app.session_wrap_width();
    let disp = session_display(&buf, code);
    let wrapped = SessionWrap::new(&disp, width, session_hang(&buf, code));
    let (row, col_now) = wrapped.row_col(display_caret(&buf, cur, code));
    let want = s.want_col.unwrap_or(col_now);

    // A wrapped line owns several display rows, and ↑/↓ mean "the row
    // above/below" — the same movement the eye makes. Only at the top or
    // bottom row of a line does the caret cross into the next SOURCE line.
    let target_row = row as i32 + delta;
    if target_row >= 0 && (target_row as usize) < wrapped.segs.len() {
        let off = wrapped.offset_at(target_row as usize, want);
        if let Some(s) = app.session.as_mut() {
            s.input.cur = raw_caret_from_display(&buf, off, code);
            s.want_col = Some(want);
            // A plain move collapses a selection to the caret — the
            // range was the shift-key's, and the shift is gone.
            s.sel_from = None;
        }
        app.follow = true;
        app.laid_width = 0;
        return;
    }

    // Crossing into another line: commit what is here first, as before.
    session_commit_dirty(app, ctx);
    let cur_line = line as i32;
    let last = app.lines.len().saturating_sub(1) as i32;
    let target = (cur_line + delta).clamp(0, last);
    if target == cur_line {
        app.toast(if delta < 0 { t!("ページの先頭です", "top of page") } else { t!("ページの末尾です — Enter で行を足せます", "end of page — Enter adds a line") });
        return;
    }
    let line = target as usize;
    let text = app.lines[line].text.clone();
    // Land on the row nearest where the caret came from: the FIRST row of
    // the line below, the LAST row of the line above.
    let code = app.raw_span_at_line(line);
    let disp = session_display(&text, code);
    let wrapped = SessionWrap::new(&disp, width, session_hang(&text, code));
    let landing = if delta < 0 { wrapped.segs.len().saturating_sub(1) } else { 0 };
    let caret = raw_caret_from_display(&text, wrapped.offset_at(landing, want), code);
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input { buf: text.clone(), cur: caret };
        s.orig = text;
        s.want_col = Some(want);
        s.sel_from = None;
    }
    app.cursor = line;
    app.follow = true;
    app.laid_width = 0;
}

/// Enter inside the session: split at the caret (at EOL this creates a
/// fresh line). The new line inherits the indent; the caret lands after
/// it; the session CONTINUES there — keep typing.
///
/// Exception — the standard list-escape: Enter on an EMPTY bullet (a
/// whitespace-only line) does not chain another empty bullet. The empty
/// bullet dissolves into a true blank line (its spaces are removed) and
/// the fresh line starts flush, with no indent.
pub(crate) fn session_split(app: &mut App, ctx: &Ctx) {
    // Enter replaces the selection the way every typed character does —
    // with the line break this time. Across lines that is the range
    // collapsing into exactly one break; on one line, cutting the range
    // and splitting at the junction left behind is the identical edit,
    // which the ordinary path below then performs.
    if session_enter_selection(app, ctx) {
        return;
    }
    if let Some(s) = app.session.as_mut() {
        if s.sel_span().is_some() {
            session_cut_span_in(s);
        }
    }
    let Some(s) = app.session.as_ref() else { return };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
    // In a `code:` block the indent is content, not list structure. Enter
    // on the HEADER opens the block's first body line (without this there
    // is no way to type a block at all: the new line would start flush and
    // the renderer would not read it as code). Enter on ONE blank line
    // inside keeps the indent — blank code exists — but a second blank in
    // a row is the writer asking out (the same bargain the empty bullet
    // makes, delayed one line for the blank code's sake).
    let texts = app.source_texts();
    let code = cosense::render::code_span_at(&texts, line);
    let table = cosense::render::table_span_at(&texts, line);
    drop(texts);
    if let Some(span) = code.or(table) {
        if line == span.header {
            session_open_below(app, ctx, line, span.body_indent(), String::new());
            return;
        }
        // What continues onto the new line. In code it is the line's OWN
        // leading whitespace — code sits at depths of its own past the
        // block's base (`  if x:` under a 1-space body), and resetting to
        // the base flattened every deeper line on Enter. A table row has
        // no inner depth; it keeps the base.
        let indent = if code.is_some() {
            indent_of(&buf).to_string()
        } else {
            span.body_indent()
        };
        let empty = buf.chars().all(char::is_whitespace);
        // A table ends the way a list does: Enter on a row with nothing
        // in it leaves. In code a blank line is just blank CODE: Enter
        // stacks as many as the writer wants, each keeping its indent.
        // Stepping out is a different gesture — deleting the indent (⌫) —
        // and the only one. (The old bargain "a second blank means leave"
        // fought every diagram that wants a blank line in the middle.)
        if empty && code.is_some() {
            let tail = format!("{indent}{}", buf[caret.min(buf.len())..].trim_start());
            session_open_below(app, ctx, line, indent, tail);
            return;
        }
        if !empty {
            let tail = format!("{indent}{}", buf[caret.min(buf.len())..].trim_start());
            if caret >= buf.trim_end().len() {
                session_open_below(app, ctx, line, indent, tail);
                return;
            }
        }
    }
    if !buf.is_empty() && buf.chars().all(char::is_whitespace) {
        let id = app.lines[line].id.clone();
        let anchor = app
            .lines
            .get(line + 1)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "_end".into());
        let ops = vec![
            EditOp::Replace { id, text: String::new() },
            EditOp::Insert { anchor, lines: vec![(new_line_id(), String::new())] },
        ];
        do_edit(app, ctx, &t!("改行", "new line"), ops);
        if let Some(s) = app.session.as_mut() {
            s.line = line + 1;
            s.input = Input { buf: String::new(), cur: 0 };
            s.orig = String::new();
            s.want_col = None;
            s.sel_from = None;
        }
        app.cursor = line + 1;
        app.follow = true;
        return;
    }
    let head = buf[..caret].to_string();
    let indent = indent_of(&buf);
    let indent = if caret < indent.len() { &buf[..caret] } else { indent };
    let tail = format!("{indent}{}", &buf[caret..]);
    let caret_new = indent.len();
    let id = app.lines[line].id.clone();
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let ops = vec![
        EditOp::Replace { id, text: head },
        EditOp::Insert { anchor, lines: vec![(new_line_id(), tail.clone())] },
    ];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input { buf: tail.clone(), cur: caret_new };
        s.orig = tail;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
}

/// Add a line right below `line`, seat the session on it with the caret
/// after `indent`, and leave the current line as it was. Used where Enter
/// must place a line at a specific indent rather than split the text.
pub(crate) fn session_open_below(app: &mut App, ctx: &Ctx, line: usize, indent: String, text: String) {
    let text = if text.is_empty() { indent.clone() } else { text };
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let mut ops = Vec::new();
    // The header's own text may still be uncommitted (you just typed
    // `code:py`); commit it with the same edit that opens the body line.
    if let Some(s) = app.session.as_ref() {
        if s.input.buf != s.orig {
            ops.push(EditOp::Replace { id: app.lines[line].id.clone(), text: s.input.buf.clone() });
        }
    }
    ops.push(EditOp::Insert { anchor, lines: vec![(new_line_id(), text.clone())] });
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input { buf: text.clone(), cur: indent.len().min(text.len()) };
        s.orig = text;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
    app.laid_width = 0;
}

/// Backspace at BOL: join this line into the previous one; the caret
/// lands on the junction. Joining line 1 into line 0 edits the title.
pub(crate) fn session_join_up(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line == 0 {
        app.toast(t!("ページの先頭です", "top of page"));
        return;
    }
    let prev_text = app.lines[line - 1].text.clone();
    let merged = format!("{prev_text}{buf}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line - 1].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line].id.clone() },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line - 1;
        s.input = Input { buf: merged.clone(), cur: prev_text.len() };
        s.orig = merged;
        s.want_col = None;
    }
    app.cursor = line - 1;
    app.follow = true;
}

/// Delete at EOL: join the NEXT line into this one (forward join).
pub(crate) fn session_join_down(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line + 1 >= app.lines.len() {
        app.toast(t!("ページの末尾です", "end of page"));
        return;
    }
    let next_text = app.lines[line + 1].text.clone();
    let merged = format!("{buf}{next_text}");
    let ops = vec![
        EditOp::Replace { id: app.lines[line].id.clone(), text: merged.clone() },
        EditOp::Delete { id: app.lines[line + 1].id.clone() },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.input = Input { buf: merged.clone(), cur: buf.len() };
        s.orig = merged;
        s.want_col = None;
    }
}

/// Tab / Shift+Tab: indent or outdent by one logical level (one source
/// space, rendered as a two-cell nesting step).
pub(crate) fn session_indent(app: &mut App, ctx: &Ctx, delta: i32) {
    // Indenting is a whole-line act; a live selection would only end up
    // pointing at bytes that moved, so it is dropped first.
    if let Some(s) = app.session.as_mut() {
        s.sel_from = None;
    }
    // A Mermaid header's indent is the DIAGRAM's nesting level: what the
    // reader sees move is the picture, not a `code:` line. So Tab there
    // takes the body with it — otherwise the first keystroke would leave
    // the block behind and stop being a block at all.
    let mermaid = app
        .session
        .as_ref()
        .and_then(|s| app.code_span_at_line(s.line))
        .filter(|span| span.mermaid_header);
    if let Some(span) = mermaid {
        session_indent_mermaid(app, ctx, span, delta);
        return;
    }
    // In a table a TAB is what separates one cell from the next, so that
    // is what Tab types there. (Shift+Tab takes the separator back, and
    // once there is none left it outdents — which is how a row leaves the
    // table, the same way a line leaves a code block.)
    let table = app.session.as_ref().and_then(|s| app.table_span_at_line(s.line));
    if let Some(span) = table {
        let in_body = app.session.as_ref().map(|s| s.line > span.header).unwrap_or(false);
        if in_body {
            if let Some(s) = app.session.as_mut() {
                if delta > 0 {
                    s.input.insert_char('\t');
                    s.want_col = None;
                    app.laid_width = 0;
                    return;
                }
                if s.input.buf[..s.input.cur].ends_with('\t') {
                    s.input.cur -= 1;
                    s.input.buf.remove(s.input.cur);
                    s.want_col = None;
                    app.laid_width = 0;
                    return;
                }
            }
        }
    }
    let Some(s) = app.session.as_mut() else { return };
    if delta > 0 {
        s.input.buf.insert(0, ' ');
        s.input.cur += 1;
    } else if let Some(first) = s.input.buf.chars().next() {
        if first.is_whitespace() {
            let w = first.len_utf8();
            s.input.buf.replace_range(..w, "");
            s.input.cur = s.input.cur.saturating_sub(w);
        }
    }
    s.want_col = None;
    app.laid_width = 0;
}

/// Column 0 of a whitespace-only line inside a `code:` block — where ⌫
/// means "eat one indent character", not "join the line above".
fn caret_on_blank_code_bol(app: &App) -> bool {
    let Some(s) = app.session.as_ref() else { return false };
    s.input.cur == 0
        && !s.input.buf.is_empty()
        && s.input.buf.chars().all(char::is_whitespace)
        && app.code_span_at_line(s.line).is_some()
}

/// Is the caret inside (or just after) the leading whitespace of a Mermaid
/// `code:` header — the place where a space or a Backspace means "nest",
/// not "type a character"?
fn caret_on_mermaid_indent_or_start(app: &App) -> bool {
    let Some(s) = app.session.as_ref() else { return false };
    if !app.code_span_at_line(s.line).is_some_and(|span| span.mermaid_header) {
        return false;
    }
    s.sel_from.is_none() && s.input.cur <= indent_of(&s.input.buf).len()
}

/// The same spot, minus the very start of the line: Backspace there has an
/// indent in front of it to take.
fn caret_on_mermaid_indent(app: &App) -> bool {
    let cur = app.session.as_ref().map(|s| s.input.cur).unwrap_or(0);
    cur > 0 && caret_on_mermaid_indent_or_start(app)
}

/// Tab / Shift+Tab on a Mermaid `code:` header: move the header AND every
/// line of its block by one level, in one edit, so the block never spends a
/// keystroke in a broken shape. Cosense draws at most
/// [`cosense::render::MERMAID_MAX_INDENT`] levels of diagram, and that is
/// where the movement stops.
fn session_indent_mermaid(app: &mut App, ctx: &Ctx, span: CodeSpan, delta: i32) {
    let Some(s) = app.session.as_ref() else { return };
    let (line, cur, buf) = (s.line, s.input.cur, s.input.buf.clone());
    let level = span.header_indent as i32 + delta;
    if level < 0 || level > cosense::render::MERMAID_MAX_INDENT as i32 {
        return;
    }
    // The block is every line that answers with THIS header — not the
    // `code:`-flagged run, which would walk into the next block when two
    // sit back to back.
    let texts = app.source_texts();
    let mut end = line + 1;
    while end < texts.len()
        && cosense::render::code_span_at(&texts, end).map(|s| s.header) == Some(span.header)
    {
        end += 1;
    }
    // Shifting a whitespace-only line by hand is pointless (it has nothing
    // to hold the indent for), so those are left alone.
    let shift = |text: &str| -> Option<String> {
        if delta > 0 {
            return Some(format!(" {text}"));
        }
        let first = text.chars().next().filter(|c| c.is_whitespace())?;
        Some(text[first.len_utf8()..].to_string())
    };
    let Some(head) = shift(&buf) else { return };
    let mut ops = vec![EditOp::Replace { id: app.lines[line].id.clone(), text: head.clone() }];
    for i in line + 1..end {
        let text = app.lines[i].text.clone();
        if text.trim().is_empty() {
            continue;
        }
        let Some(text) = shift(&text) else { return };
        ops.push(EditOp::Replace { id: app.lines[i].id.clone(), text });
    }
    let label = if delta > 0 {
        t!("図を字下げ", "indent diagram")
    } else {
        t!("図の字下げを戻す", "outdent diagram")
    };
    do_edit(app, ctx, &label, ops);
    if let Some(s) = app.session.as_mut() {
        let cur = if delta > 0 {
            cur + 1
        } else {
            cur.saturating_sub(buf.len() - head.len())
        };
        s.input = Input { buf: head.clone(), cur: cur.min(head.len()) };
        s.orig = head;
        s.want_col = None;
    }
    app.laid_width = 0;
}

/// `o` / `O` in READ: create a fresh line below/above the cursor (indent
/// inherited) and open the session on it. On a related row (or an empty
/// spot) `o` appends at the page end.
pub(crate) fn open_line(app: &mut App, ctx: &Ctx, above: bool) {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return;
    }
    if app.time.is_some() {
        app.toast_err(t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)"));
        return;
    }
    let cur = app.cursor_src();
    let indent: String = cur
        .and_then(|s| app.lines.get(s))
        .map(|l| indent_of(&l.text).to_string())
        .unwrap_or_default();
    let anchor = match (cur, above) {
        (Some(s), true) => app.lines[s].id.clone(),
        (Some(s), false) => app
            .lines
            .get(s + 1)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "_end".into()),
        (None, _) => "_end".into(),
    };
    let new_id = new_line_id();
    let ops = vec![EditOp::Insert { anchor, lines: vec![(new_id.clone(), indent.clone())] }];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
        enter_session(app, ctx, idx, indent.len());
    }
}

/// One key while the session is open — the modeless core: printable keys
/// type, arrows move the caret, Enter makes lines, Esc leaves.
pub(crate) fn handle_session_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) {
    // A modified arrow is cosense's outline chord. This viewer answers it
    // in READ, not here, and the arms below match arrows regardless of
    // their modifiers — so without this the chord would quietly move the
    // caret instead. Doing nothing and saying where the block keys live is
    // the honest answer; the selection and the caret are left untouched
    // because nothing happened.
    if matches!(
        k.code,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
    ) && matches!(k.modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        app.toast(t!(
            "ブロックを動かすには Esc で編集を抜けて m（移動モード）",
            "to move a block: Esc to leave EDIT, then m (move mode)"
        ));
        return;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    // Anything that is not extending or acting on the selection drops it:
    // a range that outlives the keystroke it was made for is a trap.
    let keeps_span = matches!(
        (k.code, shift, ctrl),
        (KeyCode::Left, true, _)
            | (KeyCode::Right, true, _)
            | (KeyCode::Up, true, _)
            | (KeyCode::Down, true, _)
            | (KeyCode::Esc, _, _)
            | (KeyCode::Enter, _, _)
            | (KeyCode::Char('y'), _, true)
            | (KeyCode::Backspace, _, _)
            | (KeyCode::Delete, _, _)
    ) || matches!(k.code, KeyCode::Char(_)) && !ctrl;
    if !keeps_span {
        if let Some(s) = app.session.as_mut() {
            s.sel_from = None;
        }
    }
    if app.selection.is_some()
        && !matches!(
            (k.code, shift),
            (KeyCode::Up, true)
                | (KeyCode::Down, true)
                | (KeyCode::Backspace, _)
                | (KeyCode::Delete, _)
                | (KeyCode::Esc, _)
                | (KeyCode::Char('y'), _)
        )
    {
        app.selection = None;
    }
    // Any horizontal edit resets the sticky ↑/↓ column.
    let reset_col = |app: &mut App| {
        if let Some(s) = app.session.as_mut() {
            s.want_col = None;
            s.sel_from = None;
        }
    };
    let edit_input = |app: &mut App, f: fn(&mut Input)| {
        if let Some(s) = app.session.as_mut() {
            f(&mut s.input);
            s.want_col = None;
            s.sel_from = None;
        }
        app.laid_width = 0;
        app.follow = true;
    };
    match (k.code, ctrl) {
        (KeyCode::Esc, _) => {
            // One Esc at a time: a selection is dropped first, so Esc never
            // both un-selects and leaves.
            if app.selection.is_some() {
                app.selection = None;
            } else if app.session.as_ref().and_then(|s| s.sel_ends()).is_some() {
                if let Some(s) = app.session.as_mut() {
                    s.sel_from = None;
                }
            } else {
                leave_session(app, ctx);
            }
        }
        (KeyCode::Enter, _) => session_split(app, ctx),
        // Shift+↑/↓ grows the CHARACTER range from the caret — the same
        // selection the mouse drags, reachable from the keyboard too.
        (KeyCode::Up, _) if shift => session_select_line(app, ctx, -1),
        (KeyCode::Down, _) if shift => session_select_line(app, ctx, 1),
        (KeyCode::Up, _) => session_move_line(app, ctx, -1),
        (KeyCode::Down, _) => session_move_line(app, ctx, 1),
        (KeyCode::Left, _) if shift => session_select_char(app, false),
        (KeyCode::Right, _) if shift => session_select_char(app, true),
        (KeyCode::Left, _) | (KeyCode::Char('b'), true) => edit_input(app, Input::left),
        (KeyCode::Right, _) | (KeyCode::Char('f'), true) => edit_input(app, Input::right),
        (KeyCode::Home, _) | (KeyCode::Char('a'), true) => edit_input(app, Input::home),
        (KeyCode::End, _) | (KeyCode::Char('e'), true) => edit_input(app, Input::end),
        (KeyCode::Char('w'), true) => edit_input(app, Input::delete_word),
        (KeyCode::Char('u'), true) => edit_input(app, Input::kill_to_start),
        (KeyCode::Char('k'), true) => session_kill(app, ctx),
        (KeyCode::Char('t'), true) => session_cycle_heading(app, ctx),
        // The terminal cannot paste a PICTURE (bracketed paste is text), so
        // the clipboard image has a key of its own.
        (KeyCode::Char('v'), true) => app.paste_clipboard_image(ctx),
        // `y` types a letter in a modeless session, and `^c` belongs to the
        // terminal, so copying takes `^y`.
        (KeyCode::Char('y'), true) => {
            let payload = copy_payload(app, false);
            copy_and_report(app, payload);
        }
        // `u` is a printable key in a modeless session, so undo takes the
        // key everyone already presses to undo text: `^z`. Raw mode turns
        // ISIG off, so it never reached the terminal as SIGTSTP anyway,
        // and this viewer has no suspend of its own to lose.
        (KeyCode::Char('z'), true) => session_history(app, ctx, true),
        (KeyCode::Char('r'), true) => session_history(app, ctx, false),
        (KeyCode::Backspace, _) | (KeyCode::Delete, _)
            if app.session.as_ref().and_then(|s| s.sel_ends()).is_some() =>
        {
            session_replace_selection(app, ctx, "");
        }
        // Backspacing the indent of a Mermaid header takes the level off,
        // block and all — the same move Shift+Tab makes. Deleting the
        // header's lone space by itself would only orphan the body.
        (KeyCode::Backspace, _) if caret_on_mermaid_indent(app) => {
            session_indent(app, ctx, -1)
        }
        // At the head of a blank code line, ⌫ eats one indent character —
        // cosense web's way. The indent IS the membership: when the last
        // of it goes, the line is flush and the block is over. That is how
        // a writer steps out, and the only way.
        (KeyCode::Backspace, _) if caret_on_blank_code_bol(app) => {
            if let Some(s) = app.session.as_mut() {
                s.input.buf.remove(0);
                s.want_col = None;
                s.sel_from = None;
            }
            app.laid_width = 0;
        }
        (KeyCode::Backspace, _) => {
            let at_bol = app.session.as_ref().map(|s| s.input.cur == 0).unwrap_or(false);
            if at_bol {
                session_join_up(app, ctx);
            } else if empty_pair_at_caret(app) && !session_in_code(app) {
                // Backspacing out of `[|]` takes the bracket that was put
                // there for you, not just the one you typed.
                edit_input(app, Input::delete);
                edit_input(app, Input::backspace);
            } else {
                edit_input(app, Input::backspace);
            }
        }
        (KeyCode::Delete, _) | (KeyCode::Char('d'), true) => {
            let at_eol = app
                .session
                .as_ref()
                .map(|s| s.input.cur == s.input.buf.len())
                .unwrap_or(false);
            if at_eol {
                session_join_down(app, ctx);
            } else {
                edit_input(app, Input::delete);
            }
        }
        (KeyCode::Tab, _) => session_indent(app, ctx, 1),
        (KeyCode::BackTab, _) => session_indent(app, ctx, -1),
        (KeyCode::Char(ch), false) => session_type_char(app, ctx, ch),
        _ => {
            reset_col(app);
        }
    }
}

/// A multi-line paste while the session is open: the first fragment goes
/// into the caret line; the rest become real lines (one insert op), and
/// the caret lands at the end of the last pasted fragment.
pub(crate) fn session_paste(app: &mut App, ctx: &Ctx, clean: &str) {
    if !clean.contains('\n') {
        // A dragged-in file arrives as its path. If it is an image on this
        // machine, the page wants the picture, not the path.
        if let Some(path) = cosense::upload::image_path_from_paste(clean) {
            app.start_upload(ctx, &path);
            return;
        }
        // A pasted URL usually wants brackets around it — that is what
        // makes an image a picture, and a selection a labelled link. Doing
        // it by hand after every paste is the kind of chore cosense web
        // already spares you.
        let text = paste_shape(app, clean);
        if !session_replace_selection(app, ctx, &text) {
            if let Some(s) = app.session.as_mut() {
                s.input.insert_str(&text);
                s.want_col = None;
                s.sel_from = None;
            }
        }
        app.laid_width = 0;
        return;
    }
    // Multi-line: a live selection goes first — the paste replaces it,
    // as any typed character's would — then the fragments land plain.
    session_replace_selection(app, ctx, "");
    let Some(s) = app.session.as_ref() else { return };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
    let mut parts = clean.split('\n');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let head = format!("{}{}", &buf[..caret], first);
    let tail_of_line = &buf[caret..];
    let last_part = rest.last().copied().unwrap_or("");
    let mut inserted: Vec<(String, String)> = Vec::new();
    for (i, p) in rest.iter().enumerate() {
        let text = if i + 1 == rest.len() { format!("{p}{tail_of_line}") } else { (*p).to_string() };
        inserted.push((new_line_id(), text));
    }
    let last_id = inserted.last().map(|(id, _)| id.clone());
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let ops = vec![
        EditOp::Replace { id: app.lines[line].id.clone(), text: head },
        EditOp::Insert { anchor, lines: inserted },
    ];
    do_edit(app, ctx, &t!("貼り付け", "paste"), ops);
    if let (Some(s), Some(last_id)) = (app.session.as_mut(), last_id) {
        if let Some(idx) = app.lines.iter().position(|l| l.id == last_id) {
            s.line = idx;
            let text = app.lines[idx].text.clone();
            s.input = Input { buf: text.clone(), cur: last_part.len().min(text.len()) };
            s.orig = text;
            s.want_col = None;
            app.cursor = idx;
            app.follow = true;
        }
    }
}

/// `^e`: suspend the TUI, open the WHOLE page in `$EDITOR`, diff the
/// result into minimal lineId ops (unchanged lines keep their ids), and
/// commit — undo (`u`) is the safety net. The editor is the user's own
/// environment — multi-line editing, their keybindings, their IME
/// settings — which makes this the most natural way to write a lot.
pub(crate) fn editor_roundtrip(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) {
    let original: String =
        app.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    let path = std::env::temp_dir().join(format!(
        "cosense-{}-{}.txt",
        std::process::id(),
        now_secs()
    ));
    if let Err(e) = std::fs::write(&path, format!("{original}\n")) {
        app.toast_err(t!("一時ファイルを作れません: {e}", "temp file failed: {e}"));
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());

    // Suspend the TUI for the editor, restore it after — whatever happens.
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} '{}'", path.display()))
        .status();
    *terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    app.laid_width = 0; // re-lay out on the next frame

    let ok = matches!(status, Ok(s) if s.success());
    let edited = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !ok {
        app.toast_err(t!("エディタが中断しました（{editor}）— 何も書き込んでいません", "editor aborted ({editor}) — nothing written"));
        return;
    }
    let edited = edited.strip_suffix('\n').unwrap_or(&edited).to_string();
    if edited == original {
        app.toast(t!("変更はありません", "no changes"));
        return;
    }
    let new_lines: Vec<String> = edited.split('\n').map(str::to_string).collect();
    if new_lines.iter().all(|l| l.trim().is_empty()) {
        app.toast_err(t!("ページが空になるため中止しました（ページの削除はブラウザで）", "page emptied — refusing (delete pages in the browser)"));
        return;
    }
    let old: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let ops = diff_to_ops(&old, &new_lines);
    if ops.is_empty() {
        app.toast(t!("変更はありません", "no changes"));
    } else {
        let n = ops.len();
        do_edit(app, ctx, &t!("エディタ", "editor"), ops);
        app.note(t!("✓ エディタの変更を {n} 件コミットしました · u で戻せます", "✓ editor: {n} op(s) committed · u to undo"));
    }
}

impl App {
    /// Is the edit session's caret on one of these source lines? Such a
    /// line is being typed into, so `app.lines` still holds the committed
    /// text while the session's buffer holds the reader's — the block is not
    /// renderable until the caret leaves and the commit lands.
    pub(crate) fn caret_is_inside(&self, rows: &[(usize, Line<'static>)]) -> bool {
        let Some(s) = &self.session else { return false };
        rows.iter().any(|(src, _)| *src == s.line)
    }

    /// Put the caret on an edit's own location (see `edit_focus`). The
    /// session moves with it, so undo/redo show what changed instead of
    /// leaving the caret wherever it happened to be. Returns whether the
    /// line was still there to stand on.
    pub(crate) fn focus_edit(&mut self, focus: Option<(String, usize)>) -> bool {
        let Some((id, caret)) = focus else { return false };
        let Some(line) = self.lines.iter().position(|l| l.id == id) else { return false };
        let text = self.lines[line].text.clone();
        self.cursor = line;
        self.follow = true;
        self.laid_width = 0;
        if let Some(s) = self.session.as_mut() {
            s.line = line;
            s.input = Input { buf: text.clone(), cur: caret.min(text.len()) };
            s.orig = text;
            s.want_col = None;
            s.sel_from = None;
        }
        true
    }

    /// The width the body rows are wrapped at. After an edit the layout is
    /// stale (`laid_width = 0`) until the next draw; the last text rect is
    /// the honest answer in between.
    pub(crate) fn session_wrap_width(&self) -> usize {
        if self.laid_width > 0 {
            return Self::text_width(self.mode, self.laid_width);
        }
        if self.text_rect.width > 0 {
            return self.text_rect.width as usize;
        }
        // No geometry at all (nothing has been drawn yet): treat lines as
        // unwrapped rather than as one column wide, which would turn every
        // line into a stack of single characters.
        usize::MAX
    }
}
