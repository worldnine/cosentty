//! EDIT session state and lifecycle. See submodules for each editing concern.

use super::*;

mod display;
mod keys;
mod paste;
mod structure;
mod text;

pub(crate) use display::*;
pub(crate) use keys::*;
pub(crate) use paste::*;
pub(crate) use structure::*;
pub(crate) use text::*;

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
        (from != head).then(|| {
            if from <= head {
                (from, head)
            } else {
                (head, from)
            }
        })
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

/// Open the session on body line `line` with the caret at byte `caret`.
pub(crate) fn enter_session(app: &mut App, ctx: &Ctx, line: usize, caret: usize) {
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
    if line >= app.lines.len() {
        app.toast_err(t!(
            "関連ページの行は編集できません（o でページ末尾に行を足せます）",
            "related rows cannot be edited (o adds a line at the end)"
        ));
        return;
    }
    let text = app.lines[line].text.clone();
    let caret = caret.min(text.len());
    // snap to a char boundary
    let caret = (0..=caret)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    app.session = Some(EditSession {
        line,
        input: Input {
            buf: text.clone(),
            cur: caret,
        },
        orig: text,
        want_col: None,
        sel_from: None,
    });
    app.cursor = line;
    app.selection = None;
    app.follow = true;
    app.laid_width = 0;
    app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode()));
    // The footer's EDIT badge and key hint say the rest; a stale READ
    // status (a selection hint) must not ride along into the session.
    app.status.clear();
}

/// Commit the caret line's text if it changed. This runs after EVERY key
/// the session handles (and after a paste), so typing reaches the server
/// while it is still going on — the way cosense web saves — and it is
/// also the forced flush the structural edits and the caret's line changes
/// call before they act.
///
/// Two things keep "a commit per keystroke" from meaning "a request and an
/// undo step per keystroke":
///
/// * the previous still-queued replace of the same line is marked
///   superseded, so the worker skips it and sends only the newest text
///   (one request per round trip while typing continues);
/// * the undo entries of one typing run on one line fold into the first,
///   so `^z` takes back the run — what "undo what I just typed" meant when
///   the line committed only on leaving it.
pub(crate) fn session_commit_dirty(app: &mut App, ctx: &Ctx) {
    let Some(s) = app.session.as_ref() else {
        return;
    };
    if s.input.buf == s.orig {
        return;
    }
    let (line, buf) = (s.line, s.input.buf.clone());
    let id = app.lines[line].id.clone();
    let label = format!("line {}", line + 1);
    let fold_undo = app.live_undo.as_deref() == Some(id.as_str())
        && app.redo_stack.is_empty()
        && matches!(
            app.undo_stack.last(),
            Some((_, ops)) if matches!(ops.as_slice(), [EditOp::Replace { id: prev, .. }] if *prev == id)
        );
    let depth = app.undo_stack.len();
    let job = do_edit(
        app,
        ctx,
        &label,
        vec![EditOp::Replace {
            id: id.clone(),
            text: buf.clone(),
        }],
    );
    if fold_undo && app.undo_stack.len() == depth + 1 {
        // The older inverse already restores the text from before the
        // run; the one just pushed only restores the previous keystroke.
        app.undo_stack.pop();
    }
    app.live_undo = Some(id.clone());
    app.live_dirty_since = None;
    app.live_due = None;
    if let Some(job) = job {
        if let Some((prev_id, prev_job)) = app.live_pending.take() {
            if prev_id == id {
                if let Ok(mut set) = app.live_superseded.lock() {
                    set.insert(prev_job);
                }
            }
        }
        app.live_pending = Some((id, job));
    }
    if let Some(s) = app.session.as_mut() {
        s.orig = buf;
    }
}

/// The keystroke's way to save: like `session_commit_dirty`, but batched.
/// The text waits for `live_debounce` of quiet, or for `live_max_wait`
/// since it first became dirty while the typing keeps coming — whichever
/// is first. Nothing goes up alone: a short sentence lands in one piece,
/// a long one in pieces `live_max_wait` apart. The deferred save is made
/// by `flush_live_edit` on the event loop's tick, so a pause saves without
/// another key being pressed.
pub(crate) fn session_live_commit(app: &mut App, ctx: &Ctx) {
    if !app.session.as_ref().is_some_and(|s| s.input.buf != s.orig) {
        return;
    }
    let now = Instant::now();
    let since = *app.live_dirty_since.get_or_insert(now);
    let due = (now + app.live_debounce).min(since + app.live_max_wait);
    if due <= now {
        session_commit_dirty(app, ctx);
    } else {
        app.live_due = Some(due);
    }
}

/// Make the deferred save when its time has come (see
/// `session_live_commit`). Called once per event-loop iteration.
pub(crate) fn flush_live_edit(app: &mut App, ctx: &Ctx) {
    flush_live_edit_at(app, ctx, Instant::now());
}

pub(crate) fn flush_live_edit_at(app: &mut App, ctx: &Ctx, now: Instant) {
    let dirty = app.session.as_ref().is_some_and(|s| s.input.buf != s.orig);
    if !dirty {
        app.live_due = None;
        return;
    }
    // The safety net: dirty text with no save scheduled (an edit elsewhere
    // cleared it) still goes up within the max wait.
    let due = app
        .live_due
        .or_else(|| app.live_dirty_since.map(|since| since + app.live_max_wait))
        .unwrap_or(now);
    if due <= now {
        session_commit_dirty(app, ctx);
        app.live_due = None;
    }
}

/// Drop the session state and go back to READ + ASCII. Commits nothing —
/// callers decide what to do with the dirty line first.
pub(crate) fn close_session(app: &mut App) {
    app.session = None;
    app.live_undo = None;
    app.live_due = None;
    app.live_dirty_since = None;
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let id = app.lines[s.line].id.clone();
    // Where to land if this very line is undone away.
    let fallback = s.line.checked_sub(1).map(|i| app.lines[i].id.clone());
    let col = s
        .want_col
        .unwrap_or_else(|| str_width(&s.input.buf[..s.input.cur]));
    let stack_was_empty = if back {
        app.undo_stack.is_empty()
    } else {
        app.redo_stack.is_empty()
    };
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
        s.input = Input {
            buf: text.clone(),
            cur: caret,
        };
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
    let Some(s) = app.session.as_ref() else {
        return;
    };
    if s.line == line || line >= app.lines.len() {
        return;
    }
    session_commit_dirty(app, ctx);
    app.live_undo = None; // leaving the line ends its typing run
    app.live_dirty_since = None;
    let text = app.lines[line].text.clone();
    if let Some(s) = app.session.as_mut() {
        s.line = line;
        s.input = Input {
            buf: text.clone(),
            cur: text.len(),
        };
        s.orig = text;
        s.want_col = None;
        s.sel_from = None;
    }
    app.cursor = line;
}

impl App {
    /// Put the caret on an edit's own location (see `edit_focus`). The
    /// session moves with it, so undo/redo show what changed instead of
    /// leaving the caret wherever it happened to be. Returns whether the
    /// line was still there to stand on.
    pub(crate) fn focus_edit(&mut self, focus: Option<(String, usize)>) -> bool {
        let Some((id, caret)) = focus else {
            return false;
        };
        let Some(line) = self.lines.iter().position(|l| l.id == id) else {
            return false;
        };
        let text = self.lines[line].text.clone();
        self.cursor = line;
        self.follow = true;
        self.laid_width = 0;
        if let Some(s) = self.session.as_mut() {
            s.line = line;
            s.input = Input {
                buf: text.clone(),
                cur: caret.min(text.len()),
            };
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
