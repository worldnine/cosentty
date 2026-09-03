use super::*;

/// Mouse handling (akapen parity).
///
/// - Wheel: the viewport alone moves, one row per event; the cursor keeps
///   its line (scrolling back finds it where it was). Over an overlay the
///   wheel moves the overlay's cursor instead.
/// - Left click on an underlined link: activate it on button-up. Elsewhere,
///   move the cursor and drop selection. Dragging selects through lines.
/// - Scrollbar: pressing the track jumps the viewport so the thumb starts
///   at that row and grabs it; dragging scrubs (the pointer may leave the
///   column). Viewport-only, like the wheel.
/// - While the composer is open only the wheel works: the commented range
///   is pinned and a click must not rewrite it mid-typing.
pub(crate) fn handle_mouse(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    // Clicking and dragging move the cursor and the selection, which are
    // the move mode's own hands: let go of the block (committing the drag)
    // before they mean something else.
    //
    // Only a BUTTON does that. Mouse capture asks for any-event tracking,
    // so bare pointer motion arrives here too, and letting that go would
    // mean a trackpad brushed in passing commits a half-finished drag and
    // spends a set of line ids on it. The wheel only moves the viewport.
    if app.move_mode.is_some()
        && matches!(
            m.kind,
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_)
        )
    {
        leave_move_mode(app, ctx);
    }
    if app.index.is_some() {
        handle_mouse_index(app, ctx, m);
        return;
    }
    if app.overlay.is_some() {
        match m.kind {
            MouseEventKind::ScrollDown => handle_overlay_key(app, ctx, KeyCode::Down, KeyModifiers::NONE),
            MouseEventKind::ScrollUp => handle_overlay_key(app, ctx, KeyCode::Up, KeyModifiers::NONE),
            _ => {}
        }
        return;
    }
    handle_mouse_content(app, ctx, m);
}

/// Mouse in the index: the wheel moves the list (or scrolls the excerpt
/// when the pointer is over it), and a click on a row OPENS that page.
///
/// One click, not two. The list is a picker — every row is a link, and a
/// row is a big target — so the second click of a select-then-open dance
/// would only be a way of asking "are you sure" about a page that can be
/// left again with `[`.
pub(crate) fn handle_mouse_index(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    use cosense::index::{Pane, Row};
    let list = app.index_list_rect;
    let preview = app.index_preview_rect;
    let inside = |r: Rect| {
        m.column >= r.x
            && m.column < r.x + r.width
            && m.row >= r.y
            && m.row < r.y + r.height
    };
    let over_list = inside(list);
    let over_preview = inside(preview);
    let row_at = |m: &MouseEvent, ix: &cosense::index::Index| -> Option<usize> {
        if m.row < list.y || m.row >= list.y + list.height || !over_list {
            return None;
        }
        let i = ix.scroll + (m.row - list.y) as usize;
        (i < ix.len()).then_some(i)
    };
    match m.kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let down = matches!(m.kind, MouseEventKind::ScrollDown);
            let height = list.height as usize;
            let Some(ix) = app.index.as_mut() else { return };
            // The wheel belongs to whatever it is pointing at, whichever
            // pane has the keys. Over the list it moves the WINDOW and
            // leaves the selection alone — the same bargain the page body
            // makes, where the wheel never carries the cursor off its line.
            if over_list {
                if ix.scroll_by(if down { 1 } else { -1 }, height) {
                    app.index_scrolled_at = Some(Instant::now());
                }
            } else if over_preview && down {
                ix.preview_scroll = ix.preview_scroll.saturating_add(1);
            } else if over_preview {
                ix.preview_scroll = ix.preview_scroll.saturating_sub(1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(ix) = app.index.as_mut() else { return };
            if over_preview {
                // Clicking the excerpt is how you get to scroll it with
                // the keys, the way Tab does.
                ix.focus = Pane::Preview;
                return;
            }
            if !over_list {
                return;
            }
            ix.focus = Pane::List;
            let Some(i) = row_at(&m, ix) else { return };
            ix.cursor = i;
            let target = match ix.rows().get(i) {
                Some(Row::Page(e)) => Some((e.title.clone(), false)),
                Some(Row::Create(name)) => Some((name.to_string(), true)),
                None => None,
            };
            open_from_index(app, ctx, target);
        }
        _ => {}
    }
}

/// Caret byte for a click at display column `col` of body line `line`:
/// exact on the session's caret line (it shows raw source), best-effort on
/// rendered lines (raw and rendered columns differ where notation hides).
pub(crate) fn click_caret(app: &App, line: usize, col: usize, screen_row: i32) -> usize {
    let text = app
        .session
        .as_ref()
        .filter(|s| s.line == line)
        .map(|s| s.input.buf.clone())
        .or_else(|| app.lines.get(line).map(|l| l.text.clone()))
        .unwrap_or_default();
    // Map through the bullet display: what the eye clicked is the display
    // column, which the view (bullets at indent) also approximates.
    let code = app.raw_span_at_line(line);
    let disp = session_display(&text, code);
    // A wrapped line owns SEVERAL display rows. The column alone cannot say
    // where in the text the click landed — without the row, every
    // continuation row maps onto the first one, and the caret jumps to the
    // wrong place (and a drag selects the wrong run).
    let width = app.session_wrap_width();
    let wrapped = SessionWrap::new(&disp, width, session_hang(&text, code));
    let seg_index = app
        .src_rows(line)
        .map(|(first, _)| {
            let clicked = (app.scroll as i32 + screen_row).max(0) as usize;
            clicked.saturating_sub(first).min(wrapped.segs.len().saturating_sub(1))
        })
        .unwrap_or(0);
    raw_caret_from_display(&text, wrapped.offset_at(seg_index, col), code)
}

/// One button-press, counted: a press within the double-click window of
/// the previous one at (almost) the same cell continues its gesture —
/// word on the second click, line on the third — and the count restarts
/// after. Records the press in `last_click`, answers with this press's
/// place in the gesture: 1, 2 or 3.
pub(crate) fn register_click(
    last: &mut Option<(Instant, u16, u16, u8)>,
    now: Instant,
    column: u16,
    row: u16,
) -> u8 {
    let count = match *last {
        Some((t, cx, cy, c))
            if now.duration_since(t) < Duration::from_millis(450)
                && cx.abs_diff(column) <= 1
                && cy.abs_diff(row) <= 1 =>
        {
            if c >= 3 {
                1
            } else {
                c + 1
            }
        }
        _ => 1,
    };
    *last = Some((now, column, row, count));
    count
}

/// The byte range of the word `caret` sits in: a run of letters and
/// digits — CJK counts as letters, so a click inside `こんにちは` takes
/// the whole run up to the first space or punctuation mark. A caret
/// exactly at a word's edge still takes that word; a click in the
/// whitespace or punctuation between two words takes nothing (`None`).
pub(crate) fn word_span(buf: &str, caret: usize) -> Option<(usize, usize)> {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '#';
    let caret = floor_boundary(buf, caret);
    let mut start: Option<usize> = None;
    let mut found: Option<(usize, usize)> = None;
    for (i, c) in buf.char_indices() {
        match (start, word(c)) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                if found.is_none() && s <= caret && caret <= i {
                    found = Some((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    if found.is_none() {
        if let Some(s) = start.filter(|s| caret >= *s) {
            found = Some((s, buf.len()));
        }
    }
    found.filter(|(a, b)| a != b)
}

/// Mouse over the page body (no overlay open): wheel, click, drag,
/// scrollbar. Uses the geometry the last frame recorded in
/// `App::text_rect` / `App::bar_rect`.
pub(crate) fn handle_mouse_content(app: &mut App, ctx: &Ctx, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => {
            app.drag_anchor = None;
            app.pressed_link = None;
            app.wheel_scroll(1, app.view_h);
            return;
        }
        MouseEventKind::ScrollUp => {
            app.drag_anchor = None;
            app.pressed_link = None;
            app.wheel_scroll(-1, app.view_h);
            return;
        }
        _ => {}
    }
    if app.composing.is_some() {
        return;
    }

    let text = app.text_rect;
    let bar = app.bar_rect;
    // `text.y` is the unscrolled content anchor below the top rule. The
    // visible band also includes one row above it: that row is the rule at
    // scroll=0 and becomes content as soon as the rule scrolls away.
    let viewport_top = text.y.saturating_sub(1);
    let viewport_bottom = text.y.saturating_add(text.height);
    let in_rows = m.row >= viewport_top && m.row <= viewport_bottom;
    let in_text_col = m.column >= text.x && m.column < text.x + text.width;
    let screen_row = m.row as i32 - text.y as i32;
    let clamped_row = m.row.clamp(viewport_top, viewport_bottom) as i32 - text.y as i32;
    let in_track = m.row >= bar.y && m.row < bar.y + bar.height;
    let track_row = m.row.saturating_sub(bar.y);
    // The pointer may leave the scrollbar while dragging: clamp it back.
    let clamped_track_row =
        m.row.clamp(bar.y, (bar.y + bar.height).saturating_sub(1)) - bar.y;
    // The scrollable extent includes the external top rule; FrameEnd is
    // already part of the rows. The track deliberately starts below the
    // frame's top rule, so the top-position thumb cannot overwrite it.
    let total = app.total_height().saturating_add(1) as usize;
    let viewport = app.view_h as usize;
    let track_len = bar.height as usize;
    let on_track = m.column == bar.x
        && in_track
        && cosense::theme::scroll_thumb_in_track(
            total,
            viewport,
            track_len,
            app.scroll as usize,
        )
        .is_some();

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if on_track => {
            if let Some(off) = cosense::theme::scroll_offset_at_in_track(
                total,
                viewport,
                track_len,
                track_row as usize,
            ) {
                app.scroll = off as u16;
                app.follow = false;
                app.scrollbar_drag = Some((track_row, off as u16));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            let (start_row, start_off) = app.scrollbar_drag.unwrap();
            if let Some(off) = cosense::theme::scroll_offset_drag_in_track(
                total,
                viewport,
                track_len,
                start_row as usize,
                start_off as usize,
                clamped_track_row as usize,
            ) {
                app.scroll = off as u16;
                app.follow = false;
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.scrollbar_drag.is_some() => {
            app.scrollbar_drag = None;
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.pressed_link = None;
            // Any column left of the scrollbar counts (gutter included):
            // the row is what selects the line. Only cells inside the text
            // column can activate an underlined link.
            if in_rows && m.column < bar.x {
                if let Some(src) = app.src_at_screen_row(screen_row) {
                    let col = (m.column.saturating_sub(text.x)) as usize;
                    // Session open: a click moves the caret and arms a
                    // selection for a drag to grow; a double-click takes
                    // the word under the pointer, a triple-click the whole
                    // line — cosense web's gestures, in the unit EDIT
                    // selects in (a character range on the caret line).
                    if app.session.is_some() {
                        if src < app.lines.len() {
                            session_commit_dirty(app, ctx);
                            let count = register_click(
                                &mut app.last_click,
                                Instant::now(),
                                m.column,
                                m.row,
                            );
                            let caret = click_caret(app, src, col, screen_row);
                            app.selection = None;
                            let mut anchor_caret = None;
                            if let Some(s) = app.session.as_mut() {
                                if s.line != src {
                                    let t = app.lines[src].text.clone();
                                    s.line = src;
                                    s.orig = t.clone();
                                    s.input = Input { buf: t, cur: 0 };
                                }
                                s.input.cur = caret.min(s.input.buf.len());
                                s.want_col = None;
                                // A plain click only arms the anchor: a
                                // click that does not drag selects nothing
                                // (see the Up arm). The gestures select.
                                s.sel_from = match count {
                                    2 => match word_span(&s.input.buf, s.input.cur) {
                                        Some((a, b)) => {
                                            s.input.cur = b;
                                            Some((src, a))
                                        }
                                        None => None,
                                    },
                                    3 => {
                                        s.input.cur = s.input.buf.len();
                                        Some((src, 0))
                                    }
                                    _ => Some((src, s.input.cur)),
                                };
                                if s.sel_ends().is_none() {
                                    s.sel_from = None;
                                }
                                anchor_caret = s.sel_from.map(|(_, b)| b).or(Some(s.input.cur));
                            }
                            app.drag_anchor = Some((src, anchor_caret));
                            app.cursor = src;
                            app.follow = true;
                            app.laid_width = 0;
                        }
                        return;
                    }
                    // A link activates on button-up over the same target.
                    // Waiting for release preserves click-drag selection.
                    if in_text_col {
                        if let Some((link_src, item)) =
                            app.link_at_screen_position(screen_row, col)
                        {
                            app.selection = None;
                            app.drag_anchor = Some((link_src, None));
                            app.goto_src(link_src);
                            app.pressed_link = Some((link_src, item));
                            app.last_click = None;
                            return;
                        }
                    }
                    // READ: a double-click ENTERS the session at the
                    // clicked character — the view→edit transition — and
                    // nothing more: it just parks the caret where the
                    // pointer was, with no selection. (Taking the word on
                    // the gesture that only meant to open EDIT surprised
                    // the reader; selecting is a gesture for once you are
                    // already IN edit — see the session branch above, where
                    // a double-click takes the word and a triple the line.)
                    // A single click moves the line cursor as before.
                    let count =
                        register_click(&mut app.last_click, Instant::now(), m.column, m.row);
                    if count >= 2 && src < app.lines.len() && app.time.is_none() {
                        let caret = click_caret(app, src, col, screen_row);
                        enter_session(app, ctx, src, caret);
                        return;
                    }
                    app.selection = None;
                    app.drag_anchor = Some((src, None));
                    app.goto_src(src);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.pressed_link = None;
            // Dragging past the edge scrolls, so a selection can reach
            // beyond one screenful.
            if in_rows {
                if m.row <= viewport_top {
                    app.wheel_scroll(-1, app.view_h);
                } else if m.row >= viewport_bottom {
                    app.wheel_scroll(1, app.view_h);
                }
            }
            let Some((anchor, anchor_caret)) = app.drag_anchor else { return };
            let Some(end) = app.src_at_screen_row(clamped_row) else { return };
            if app.session.is_some() {
                let col = m.column.saturating_sub(text.x) as usize;
                if end < app.lines.len() {
                    // Characters, across lines: the caret — the range's
                    // moving end — follows the pointer wherever it
                    // wanders, and the anchor stays exactly where the
                    // button came down. Drifting back onto the anchor
                    // line collapses the range right back into the
                    // character selection it started as: there is no line
                    // mode to get stuck in, because there is no line
                    // mode. Seat the session on the pointer's line FIRST:
                    // the caret below belongs to THAT line's text, and
                    // writing a foreign buffer's offset is the
                    // mid-character panic the draws used to die of. (A
                    // drag types nothing, so the seat's commit is a
                    // no-op.)
                    session_move_to_line(app, ctx, end);
                    let caret = click_caret(app, end, col, clamped_row);
                    if let Some(s) = app.session.as_mut() {
                        s.input.cur = caret.min(s.input.buf.len());
                        s.sel_from = anchor_caret.map(|b| (anchor, b));
                        if s.sel_ends().is_none() {
                            s.sel_from = None;
                        }
                        s.want_col = None;
                    }
                }
                app.selection = None;
                app.laid_width = 0;
                app.follow = true;
                return;
            }
            app.selection = Some(Selection { anchor, cursor: end });
            app.goto_src(end);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some((src, target)) = app.pressed_link.take() {
                let same_target = in_rows
                    && in_text_col
                    && app
                        .link_at_screen_position(
                            screen_row,
                            m.column.saturating_sub(text.x) as usize,
                        )
                        .map(|(_, item)| item == target)
                        .unwrap_or(false);
                app.drag_anchor = None;
                if same_target {
                    app.selection = None;
                    app.goto_src(src);
                    activate_link(app, ctx, target);
                    return;
                }
            }
            app.drag_anchor = None;
            // A plain click arms an anchor but selects nothing.
            if let Some(s) = app.session.as_mut() {
                if s.sel_ends().is_none() {
                    s.sel_from = None;
                }
            }
        }
        _ => {}
    }
}

/// Move the cursor to the next/previous commented line on this page.
pub(crate) fn jump_comment(app: &mut App, forward: bool) {
    let cur_src = app.cursor_src().unwrap_or(0);
    let mut targets: Vec<usize> = app
        .comments
        .iter()
        .filter(|c| c.project == app.project && c.title == app.title)
        .map(|c| c.start)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let target = if forward {
        targets.into_iter().find(|&s| s > cur_src)
    } else {
        targets.into_iter().filter(|&s| s < cur_src).next_back()
    };
    match target {
        Some(src) => {
            app.goto_src(src);
            app.note(t!("{} 行目のコメント", "comment at line {}", src + 1));
        }
        None => app.toast(t!("これ以上コメントはありません", "no more comments")),
    }
}
