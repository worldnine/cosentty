//! Selections, character input, headings and caret movement.

use super::*;

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
    let Some(s) = app.session.as_mut() else {
        return false;
    };
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
    let Some(((tl, tb), (bl, bb))) = app.session.as_ref().and_then(|s| s.sel_ends()) else {
        return false;
    };
    if tl == bl {
        let Some(s) = app.session.as_mut() else {
            return false;
        };
        let Some((a, b)) = s.sel_span() else {
            return false;
        };
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
    let (top_text, bot_text) = (app.lines[tl].text.clone(), app.lines[bl].text.clone());
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let merged = format!("{prefix}{text}{suffix}");
    let mut ops = vec![EditOp::Replace {
        id: app.lines[tl].id.clone(),
        text: merged.clone(),
    }];
    ops.extend((tl + 1..=bl).map(|i| EditOp::Delete {
        id: app.lines[i].id.clone(),
    }));
    do_edit(app, ctx, &t!("選択範囲の置換", "replace selection"), ops);
    if let Some(s) = app.session.as_mut() {
        s.line = tl;
        s.input = Input {
            buf: merged.clone(),
            cur: prefix.len() + text.len(),
        };
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
    let Some(((tl, tb), (bl, bb))) = app.session.as_ref().and_then(|s| s.sel_ends()) else {
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
    let (top_text, bot_text) = (app.lines[tl].text.clone(), app.lines[bl].text.clone());
    let prefix = top_text[..floor_boundary(&top_text, tb)].to_string();
    let suffix = bot_text[floor_boundary(&bot_text, bb)..].to_string();
    let mut ops = vec![
        EditOp::Replace {
            id: app.lines[tl].id.clone(),
            text: prefix,
        },
        EditOp::Replace {
            id: app.lines[bl].id.clone(),
            text: suffix.clone(),
        },
    ];
    ops.extend((tl + 1..bl).map(|i| EditOp::Delete {
        id: app.lines[i].id.clone(),
    }));
    do_edit(app, ctx, &t!("改行", "new line"), ops);
    // The middle lines are gone: the far line now sits one below the
    // anchor, and it is where the caret goes on.
    let seat = tl + 1;
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input {
            buf: suffix.clone(),
            cur: 0,
        };
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
    // A line selection is a selection of BODY lines: the related rows
    // below the frame are other pages, not text of this one, so neither
    // does a selection start on them nor may it grow into them. Coming
    // from a related row, ↑ first climbs back into the body as a plain
    // move and selects from there.
    let body = app.lines.len();
    if app.cursor >= body {
        if down {
            return;
        }
        if let Some(last) = app.last_body_src() {
            app.goto_src(last);
        }
        return;
    }
    if app.selection.is_none() {
        app.selection = Some(Selection::new(app.cursor));
    }
    let before = app.cursor;
    app.move_cursor(down);
    if app.cursor >= body {
        // The step would have left the body: stay on its last line.
        app.cursor = app.last_body_src().unwrap_or(before);
        app.after_cursor_move();
    }
    if let Some((a, b)) = app.selection.map(|s| s.range()) {
        app.status = t!(
            "{} 行を選択 · y コピー · c コメント · Esc 解除",
            "selected {} line(s) · y copy · c comment · Esc clear",
            b - a + 1
        );
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
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
        s.input = Input {
            buf: format!("{indent}{new_body}"),
            cur: new_cur,
        };
        s.want_col = None;
        s.sel_from = None;
    }
    app.laid_width = 0;
    app.follow = true;
    let _ = ctx;
    app.note(match next {
        Some(n) => t!(
            "見出し レベル{n}（^t でさらに）",
            "heading level {n} (^t for more)"
        ),
        None => t!(
            "見出しを解除（^t で再び）",
            "heading cleared (^t to start again)"
        ),
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
    // Same for a plain `code:` header (a body's leading spaces are content
    // and stay typable — only the header's indent is structure).
    if ch == ' ' && (caret_on_mermaid_indent_or_start(app) || caret_on_plain_code_header_start(app))
    {
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
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
    app.status = t!(
        "{} 行を選択 · ⌫ 削除 · Esc 解除",
        "selected {} line(s) · ⌫ delete · Esc clear",
        n
    );
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
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
        app.toast_err(t!(
            "タイトル行は削除できません",
            "the title line cannot be deleted"
        ));
        return;
    }
    session_commit_dirty(app, ctx);
    let id = app.lines[line].id.clone();
    do_edit(
        app,
        ctx,
        &t!("行の削除", "delete line"),
        vec![EditOp::Delete { id }],
    );
    // The caret takes the place the line left behind: the line that slid
    // up into this index, or the one above when we killed the last line.
    let seat = line.min(app.lines.len().saturating_sub(1));
    let text = app.lines[seat].text.clone();
    let caret = if seat < line { text.len() } else { 0 };
    if let Some(s) = app.session.as_mut() {
        s.line = seat;
        s.input = Input {
            buf: text.clone(),
            cur: caret,
        };
        s.orig = text;
        s.want_col = None;
    }
    app.cursor = seat;
    app.follow = true;
    app.laid_width = 0;
    app.note(t!(
        "✓ 1行削除 · ^z で戻せます",
        "✓ deleted line · ^z to undo"
    ));
}

/// ↑/↓ inside the session: commit the dirty line, carry the caret to the
/// next/previous BODY line, keeping the display column (sticky).
pub(crate) fn session_move_line(app: &mut App, ctx: &Ctx, delta: i32) {
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let (line, buf, cur) = (s.line, s.input.buf.clone(), s.input.cur);
    let code = caret_span(app, line);
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
        app.toast(if delta < 0 {
            t!("ページの先頭です", "top of page")
        } else {
            t!(
                "ページの末尾です — Enter で行を足せます",
                "end of page — Enter adds a line"
            )
        });
        return;
    }
    let line = target as usize;
    let text = app.lines[line].text.clone();
    // Land on the row nearest where the caret came from: the FIRST row of
    // the line below, the LAST row of the line above.
    let code = caret_span(app, line);
    let disp = session_display(&text, code);
    let wrapped = SessionWrap::new(&disp, width, session_hang(&text, code));
    let landing = if delta < 0 {
        wrapped.segs.len().saturating_sub(1)
    } else {
        0
    };
    let caret = raw_caret_from_display(&text, wrapped.offset_at(landing, want), code);
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input {
            buf: text.clone(),
            cur: caret,
        };
        s.orig = text;
        s.want_col = Some(want);
        s.sel_from = None;
    }
    app.cursor = line;
    app.follow = true;
    app.laid_width = 0;
}
