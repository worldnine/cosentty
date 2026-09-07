//! Line splitting/joining, block indentation and opening a line.

use super::*;

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
    let Some(s) = app.session.as_ref() else {
        return;
    };
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
            // Enter at the head of a `code:` header inserts a blank line
            // ABOVE instead of opening the body below: splitting here
            // would erase the header (`Replace` with `""`), and there
            // must be a way to put air above a block. The header below is
            // untouched, so the caret stays at its head.
            if caret == 0 && code.is_some() {
                let id = app.lines[line].id.clone();
                let ops = vec![EditOp::Insert {
                    anchor: id,
                    lines: vec![(new_line_id(), String::new())],
                }];
                do_edit(app, ctx, &t!("改行", "new line"), ops);
                if let Some(s) = app.session.as_mut() {
                    s.line = line + 1;
                    s.input.cur = 0;
                    s.want_col = None;
                }
                app.cursor = line + 1;
                app.follow = true;
                app.laid_width = 0;
                return;
            }
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
        // Enter with the caret INSIDE the leading whitespace: web
        // inherits the indent on the fresh line, so the line above stays
        // blank CODE — a truly blank line here would end the block (the
        // parser, and web, read indent 0 as the exit). A caret at the
        // indent's END is the ordinary EOL case (blank code stacks below).
        if caret < indent.len() && code.is_some() {
            let anchor = app.lines[line].id.clone();
            let ops = vec![EditOp::Insert {
                anchor,
                lines: vec![(new_line_id(), indent.clone())],
            }];
            do_edit(app, ctx, &t!("改行", "new line"), ops);
            if let Some(s) = app.session.as_mut() {
                s.line = line; // the fresh line sits above
                s.input = Input {
                    buf: indent.clone(),
                    cur: indent.len(),
                };
                s.orig = indent.clone();
                s.want_col = None;
                s.sel_from = None;
            }
            app.cursor = line;
            app.follow = true;
            app.laid_width = 0;
            return;
        }
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
            EditOp::Replace {
                id,
                text: String::new(),
            },
            EditOp::Insert {
                anchor,
                lines: vec![(new_line_id(), String::new())],
            },
        ];
        do_edit(app, ctx, &t!("改行", "new line"), ops);
        if let Some(s) = app.session.as_mut() {
            s.line = line + 1;
            s.input = Input {
                buf: String::new(),
                cur: 0,
            };
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
    let indent = if caret < indent.len() {
        &buf[..caret]
    } else {
        indent
    };
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
        EditOp::Insert {
            anchor,
            lines: vec![(new_line_id(), tail.clone())],
        },
    ];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input {
            buf: tail.clone(),
            cur: caret_new,
        };
        s.orig = tail;
        s.want_col = None;
    }
    app.cursor = line + 1;
    app.follow = true;
}

/// Add a line right below `line`, seat the session on it with the caret
/// after `indent`, and leave the current line as it was. Used where Enter
/// must place a line at a specific indent rather than split the text.
pub(crate) fn session_open_below(
    app: &mut App,
    ctx: &Ctx,
    line: usize,
    indent: String,
    text: String,
) {
    let text = if text.is_empty() {
        indent.clone()
    } else {
        text
    };
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
            ops.push(EditOp::Replace {
                id: app.lines[line].id.clone(),
                text: s.input.buf.clone(),
            });
        }
    }
    ops.push(EditOp::Insert {
        anchor,
        lines: vec![(new_line_id(), text.clone())],
    });
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line + 1;
        s.input = Input {
            buf: text.clone(),
            cur: indent.len().min(text.len()),
        };
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line == 0 {
        app.toast(t!("ページの先頭です", "top of page"));
        return;
    }
    let prev_text = app.lines[line - 1].text.clone();
    let merged = format!("{prev_text}{buf}");
    let ops = vec![
        EditOp::Replace {
            id: app.lines[line - 1].id.clone(),
            text: merged.clone(),
        },
        EditOp::Delete {
            id: app.lines[line].id.clone(),
        },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = line - 1;
        s.input = Input {
            buf: merged.clone(),
            cur: prev_text.len(),
        };
        s.orig = merged;
        s.want_col = None;
    }
    app.cursor = line - 1;
    app.follow = true;
}

/// Delete at EOL: join the NEXT line into this one (forward join).
pub(crate) fn session_join_down(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let (line, buf) = (s.line, s.input.buf.clone());
    if line + 1 >= app.lines.len() {
        app.toast(t!("ページの末尾です", "end of page"));
        return;
    }
    let next_text = app.lines[line + 1].text.clone();
    let merged = format!("{buf}{next_text}");
    let ops = vec![
        EditOp::Replace {
            id: app.lines[line].id.clone(),
            text: merged.clone(),
        },
        EditOp::Delete {
            id: app.lines[line + 1].id.clone(),
        },
    ];
    do_edit(app, ctx, &t!("行の結合", "join"), ops);
    if let Some(s) = app.session.as_mut() {
        s.input = Input {
            buf: merged.clone(),
            cur: buf.len(),
        };
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
        session_indent_block(
            app,
            ctx,
            span,
            delta,
            Some(cosense::render::MERMAID_MAX_INDENT as i32),
            t!("図を字下げ", "indent diagram"),
            t!("図の字下げを戻す", "outdent diagram"),
        );
        return;
    }
    // A plain `code:` line's indent is the BLOCK's nesting level too: a
    // Python block stays a program only if every line moves together.
    // Per-line Tab here only ever broke things — indenting the header
    // orphaned the body, outdenting a body dropped it out of the block —
    // so the whole block moves, from whichever line the caret is on.
    // (Mermaid bodies stay per-line: there the indent is content. Math
    // blocks stay per-line too: nesting one past level 0 would unmake
    // the formula.)
    let plain = app
        .session
        .as_ref()
        .and_then(|s| plain_code_span(app, s.line));
    if let Some(span) = plain {
        session_indent_block(
            app,
            ctx,
            span,
            delta,
            None,
            t!("コードを字下げ", "indent code block"),
            t!("コードの字下げを戻す", "outdent code block"),
        );
        return;
    }
    // In a table a TAB is what separates one cell from the next, so that
    // is what Tab types there. (Shift+Tab takes the separator back, and
    // once there is none left it outdents — which is how a row leaves the
    // table, the same way a line leaves a code block.)
    let table = app
        .session
        .as_ref()
        .and_then(|s| app.table_span_at_line(s.line));
    if let Some(span) = table {
        let in_body = app
            .session
            .as_ref()
            .map(|s| s.line > span.header)
            .unwrap_or(false);
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
    let Some(s) = app.session.as_mut() else {
        return;
    };
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
pub(super) fn caret_on_blank_code_bol(app: &App) -> bool {
    let Some(s) = app.session.as_ref() else {
        return false;
    };
    s.input.cur == 0
        && !s.input.buf.is_empty()
        && s.input.buf.chars().all(char::is_whitespace)
        && app.code_span_at_line(s.line).is_some()
}

/// The head of a plain `code:` header: a typed space nests the whole
/// block there too (a header's leading whitespace is structure, never content).
pub(super) fn caret_on_plain_code_header_start(app: &App) -> bool {
    let Some(s) = app.session.as_ref() else {
        return false;
    };
    let Some(span) = plain_code_span(app, s.line) else {
        return false;
    };
    s.line == span.header && s.sel_from.is_none() && s.input.cur <= indent_of(&s.input.buf).len()
}

/// Is the caret inside (or just after) the leading whitespace of a Mermaid
/// `code:` header — the place where a space or a Backspace means "nest",
/// not "type a character"?
pub(super) fn caret_on_mermaid_indent_or_start(app: &App) -> bool {
    let Some(s) = app.session.as_ref() else {
        return false;
    };
    if !app
        .code_span_at_line(s.line)
        .is_some_and(|span| span.mermaid_header)
    {
        return false;
    }
    s.sel_from.is_none() && s.input.cur <= indent_of(&s.input.buf).len()
}

/// The same spot on a plain `code:` header: Backspace there has an
/// indent in front of it to take, block and all.
pub(super) fn caret_on_plain_code_header_indent(app: &App) -> bool {
    let cur = app.session.as_ref().map(|s| s.input.cur).unwrap_or(0);
    cur > 0 && caret_on_plain_code_header_start(app)
}

/// The same spot, minus the very start of the line: Backspace there has an
/// indent in front of it to take.
pub(super) fn caret_on_mermaid_indent(app: &App) -> bool {
    let cur = app.session.as_ref().map(|s| s.input.cur).unwrap_or(0);
    cur > 0 && caret_on_mermaid_indent_or_start(app)
}

/// A `code:` line whose header is neither Mermaid nor math: moving it
/// means moving its whole block (see `session_indent`).
pub(super) fn plain_code_span(app: &App, line: usize) -> Option<CodeSpan> {
    let span = app.code_span_at_line(line)?;
    if span.mermaid_header {
        return None;
    }
    let text = &app.lines.get(span.header)?.text;
    let (_, _, body) = cosense::render::indent_info(text);
    let lang = body.strip_prefix("code:")?.trim();
    if cosense::render::mermaid_lang(lang) || cosense::render::math_lang(lang) {
        return None;
    }
    Some(span)
}

/// Tab / Shift+Tab on a `code:` block: move the header AND every line of
/// its block by one level, in one edit, so the block never spends a
/// keystroke in a broken shape. `max_level` caps diagrams (Cosense draws
/// at most [`cosense::render::MERMAID_MAX_INDENT`] levels of them); plain
/// code has no ceiling.
pub(super) fn session_indent_block(
    app: &mut App,
    ctx: &Ctx,
    span: CodeSpan,
    delta: i32,
    max_level: Option<i32>,
    indent_label: String,
    outdent_label: String,
) {
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let (line, cur, buf) = (s.line, s.input.cur, s.input.buf.clone());
    let level = span.header_indent as i32 + delta;
    if level < 0 || max_level.is_some_and(|max| level > max) {
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
    let mut ops = vec![];
    for i in span.header..end {
        // The caret line's text is still uncommitted: shift the buffer,
        // not the stale committed line.
        let text = if i == line {
            buf.clone()
        } else {
            app.lines[i].text.clone()
        };
        if text.trim().is_empty() {
            continue;
        }
        let Some(text) = shift(&text) else { return };
        ops.push(EditOp::Replace {
            id: app.lines[i].id.clone(),
            text,
        });
    }
    let label = if delta > 0 {
        indent_label
    } else {
        outdent_label
    };
    do_edit(app, ctx, &label, ops);
    if let Some(s) = app.session.as_mut() {
        let cur = if delta > 0 {
            cur + 1
        } else {
            cur.saturating_sub(buf.len() - head.len())
        };
        s.input = Input {
            buf: head.clone(),
            cur: cur.min(head.len()),
        };
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
        app.toast_err(t!(
            "履歴を表示中 — 読み取り専用（Esc で最新へ）",
            "viewing history — read-only (Esc → NOW)"
        ));
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
    let ops = vec![EditOp::Insert {
        anchor,
        lines: vec![(new_id.clone(), indent.clone())],
    }];
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
        enter_session(app, ctx, idx, indent.len());
    }
}
