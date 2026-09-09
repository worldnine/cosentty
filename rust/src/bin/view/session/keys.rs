//! EDIT key dispatch.

use super::*;

/// One key while the session is open — the modeless core: printable keys
/// type, arrows move the caret, Enter makes lines, Esc leaves.
pub(crate) fn handle_session_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
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
        return Action::Continue;
    }
    // ^o opens the index from inside the session too: the dirty line is
    // committed first (clicking away commits too), and the session stays
    // open — it resumes when the index closes.
    if k.code == KeyCode::Char('o') && k.modifiers.contains(KeyModifiers::CONTROL) {
        open_page_index(app, ctx);
        return Action::Continue;
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
        // Commit the dirty line NOW. Usually the commit rides along when
        // the caret leaves the line; sometimes the writer wants the ✓
        // before moving on — and a ws race window closes with it.
        (KeyCode::Char('s'), true) => {
            if app.session.as_ref().is_some_and(|s| s.input.buf != s.orig) {
                session_commit_dirty(app, ctx);
                app.note(t!("✓ コミットしました", "✓ committed"));
            }
        }
        // A pageful at a time, caret and all: reading the context around
        // the line being written without leaving the session. The caret
        // moves (a viewport that moves alone would snap back on the next
        // keystroke), and the line crossed commits first.
        (KeyCode::PageDown, _) => session_move_line(app, ctx, app.view_h.max(1) as i32),
        (KeyCode::PageUp, _) => session_move_line(app, ctx, -(app.view_h.max(1) as i32)),
        // ^j breaks the line exactly like Enter — the composer's finger
        // works here too.
        (KeyCode::Char('j'), true) => session_split(app, ctx),
        // Readline's backspace alias. At the head it does nothing — the
        // join-up is ⌫'s own gesture there.
        (KeyCode::Char('h'), true) => edit_input(app, Input::backspace),
        // A full repaint, for a terminal that has garbled itself.
        (KeyCode::Char('l'), true) => return Action::Repaint,
        (KeyCode::Backspace, _) | (KeyCode::Delete, _)
            if app.session.as_ref().and_then(|s| s.sel_ends()).is_some() =>
        {
            session_replace_selection(app, ctx, "");
        }
        // Backspacing the indent of a Mermaid header takes the level off,
        // block and all — the same move Shift+Tab makes. Deleting the
        // header's lone space by itself would only orphan the body.
        // A plain `code:` header answers the same way; body lines keep
        // character-wise Backspace (their indent may be content).
        (KeyCode::Backspace, _)
            if caret_on_mermaid_indent(app) || caret_on_plain_code_header_indent(app) =>
        {
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
            let at_bol = app
                .session
                .as_ref()
                .map(|s| s.input.cur == 0)
                .unwrap_or(false);
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
    // Whatever the key did to the caret line goes up now (a no-op when
    // the text is unchanged) — see `session_commit_dirty`.
    session_commit_dirty(app, ctx);
    Action::Continue
}
