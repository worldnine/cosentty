use super::*;

/// Keys while the index owns the screen.
///
/// A picker's keys: move, `/` to narrow, Enter to go. The filter is a
/// LINE YOU OPEN rather than something every printable key falls into
/// (see `Index::filter_editing`), so the letters stay available as
/// commands — which is how `q` quits from here at all.
pub(crate) fn handle_index_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    app.disarm_quit_unless_q(&k);
    let action = index_key(app, ctx, k);
    // The filter line is where Japanese gets typed, so it holds the IME
    // guard: the input source switches on open and returns to ASCII on
    // close, exactly as the comment composer does. Without this, `/` would
    // still mean "toggle the IME by hand, twice" — which is the friction it
    // exists to remove. Driven off the resulting state rather than set in
    // each arm, so no way of opening or closing the line can forget it.
    let editing = app.index.as_ref().is_some_and(|ix| ix.filter_editing);
    match (editing, app.ime_guard.is_some()) {
        (true, false) => {
            app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
        }
        (false, true) => app.ime_guard = None,
        _ => {}
    }
    action
}

fn index_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    use cosense::index::{Pane, Row};
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let page_rows = app.index_list_rect.height.max(1) as i32;
    let preview_on = app.index_preview_rect.width > 0 && app.index_preview_rect.height > 0;
    // ---- the sort menu, while it is open ------------------------------
    //
    // Six orders is more than a cycle key can offer without counting
    // presses, and a menu also answers "which one am I in" by showing the
    // mark next to it.
    if let Some(cursor) = app.index_sort_menu {
        use cosense::index::SortKey;
        let last = SortKey::ALL.len() - 1;
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => return Action::Quit,
            (KeyCode::Esc, _) | (KeyCode::Char('q'), false) => app.index_sort_menu = None,
            (KeyCode::Down, _) | (KeyCode::Char('j'), false) | (KeyCode::Char('n'), true) => {
                app.index_sort_menu = Some((cursor + 1).min(last));
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), false) | (KeyCode::Char('p'), true) => {
                app.index_sort_menu = Some(cursor.saturating_sub(1));
            }
            (KeyCode::Enter, _) => {
                app.index_sort_menu = None;
                resort_index(app, ctx, SortKey::ALL[cursor.min(last)]);
            }
            _ => {}
        }
        return Action::Continue;
    }
    let Some(ix) = app.index.as_mut() else { return Action::Continue };
    // ---- the filter line, while it is open ----------------------------
    //
    // Everything printable is text here. Only the keys that cannot be
    // text act: Enter keeps it, Esc drops it, and `^c` still quits — the
    // interrupt habit must not be a dead key just because a line is open
    // (ashiato's on_filter_key makes the same exception).
    if ix.filter_editing {
        use cosense::index::FilterMode;
        match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => return Action::Quit,
            (KeyCode::Esc, _) => ix.cancel_filter(),
            // Tab swaps which question the line is asking — the titles on
            // screen, or every page's body. The typed word carries over.
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => ix.toggle_filter_mode(),
            (KeyCode::Enter, _) => match ix.filter_mode {
                FilterMode::Title => ix.commit_filter(),
                FilterMode::FullText => {
                    // The line closes either way: a search is a request,
                    // and its answer is a different list.
                    let query = ix.filter.clone();
                    ix.commit_filter();
                    search_index(app, ctx, &query);
                }
            },
            (KeyCode::Backspace, _) => ix.pop_filter(),
            (KeyCode::Char('u'), true) => ix.set_filter(String::new()),
            // The cursor may still be moved while typing: the list under a
            // filter is a list, and the excerpt follows it. Only the arrows
            // and their `^n`/`^p` twins do it — the letters are text.
            (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
                ix.move_cursor(1);
            }
            (KeyCode::Up, _) | (KeyCode::Char('p'), true) => {
                ix.move_cursor(-1);
            }
            (KeyCode::Char(c), false) => ix.push_filter(c),
            _ => {}
        }
        return Action::Continue;
    }
    match (k.code, ctrl) {
        // ---- quit ---- (as on the page: `q` asks once, and `^c` just goes)
        (KeyCode::Char('q'), false) => {
            return if app.confirm_quit() { Action::Quit } else { Action::Continue };
        }
        (KeyCode::Char('c'), true) => return Action::Quit,
        (KeyCode::Char('/'), false) => ix.begin_filter(),
        // `s` names the order, as it names the display on the page.
        (KeyCode::Char('s'), false) => {
            if ix.is_search() {
                // Re-ordering hits by date would answer a question nobody
                // asked, and quietly re-listing the project would lose the
                // ones found. So say what the state is.
                app.toast(t!(
                    "検索結果は関連度順です（^u で一覧へ戻る）",
                    "hits are in relevance order (^u for the list)"
                ));
            } else {
                let at = cosense::index::SortKey::ALL.iter().position(|&k| k == ix.sort);
                app.index_sort_menu = Some(at.unwrap_or(0));
            }
        }
        (KeyCode::Char('['), false) => go_history(app, ctx, true),
        (KeyCode::Char(']'), false) => go_history(app, ctx, false),
        (KeyCode::Esc, _) => {
            if app.history.is_empty() {
                // A title-less launch starts at the site top and therefore
                // has no earlier route. The latest page is already loaded
                // underneath, which remains a useful fallback.
                app.index = None;
                app.status = String::new();
                app.dismiss_toast();
            } else {
                go_history(app, ctx, true);
            }
        }
        // The excerpt is a second place to be, so Tab moves between it and
        // the list — but only when there IS an excerpt dock.
        (KeyCode::Tab, _) if preview_on => {
            ix.focus = match ix.focus {
                Pane::List => Pane::Preview,
                Pane::Preview => Pane::List,
            };
        }
        (KeyCode::Enter, _) => {
            let target = match ix.rows().get(ix.cursor) {
                Some(Row::Page(e)) => Some((e.title.clone(), false)),
                Some(Row::Create(name)) => Some((name.to_string(), true)),
                None => None,
            };
            open_from_index(app, ctx, target);
        }
        // Scrolling the preview when it has the focus; moving the list
        // otherwise. Same fingers either way.
        (KeyCode::Down, _) | (KeyCode::Char('j'), false) if ix.focus == Pane::Preview => {
            ix.preview_scroll = ix.preview_scroll.saturating_add(1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), false) if ix.focus == Pane::Preview => {
            ix.preview_scroll = ix.preview_scroll.saturating_sub(1);
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
            ix.move_cursor(1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), true) => {
            ix.move_cursor(-1);
        }
        (KeyCode::Char('j'), false) => {
            ix.move_cursor(1);
        }
        (KeyCode::Char('k'), false) => {
            ix.move_cursor(-1);
        }
        (KeyCode::Char('g'), false) => {
            ix.move_cursor(i32::MIN / 2);
        }
        (KeyCode::Char('G'), false) => {
            ix.move_cursor(i32::MAX / 2);
        }
        (KeyCode::PageDown, _) | (KeyCode::Char('d'), true) => {
            ix.move_cursor(page_rows);
        }
        (KeyCode::PageUp, _) => {
            ix.move_cursor(-page_rows);
        }
        // `^u` clears the filter, as it clears a line everywhere else —
        // without having to open the line again to empty it. Over a set of
        // full-text hits it means the same thing one step up: put the
        // project's own list back.
        (KeyCode::Char('u'), true) => {
            if ix.is_search() {
                clear_index_search(app, ctx);
            } else {
                ix.set_filter(String::new());
            }
        }
        _ => {}
    }
    Action::Continue
}

/// One key press.
pub(crate) fn handle_key(app: &mut App, ctx: &Ctx, k: event::KeyEvent) -> Action {
    app.disarm_quit_unless_q(&k);
    if app.index.is_some() {
        return handle_index_key(app, ctx, k);
    }
    // Comment composer captures all keys. Emacs/readline-style line
    // editing, so a Japanese sentence can be corrected mid-line.
    if app.composing.is_some() {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // The bar is part of the layout (it sits under the commented
        // range and grows as the text wraps), so every key re-lays out.
        app.laid_width = 0;
        match (k.code, ctrl) {
            (KeyCode::Esc, _) => {
                app.composing = None;
                app.ime_guard = None; // back to ASCII for command mode
            }
            (KeyCode::Enter, _) => {
                let input = app.composing.take().unwrap();
                app.ime_guard = None; // back to ASCII for command mode
                finish_composer(app, input);
            }
            (KeyCode::Backspace, _) => in_input(app, Input::backspace),
            (KeyCode::Delete, _) | (KeyCode::Char('d'), true) => in_input(app, Input::delete),
            (KeyCode::Left, _) | (KeyCode::Char('b'), true) => in_input(app, Input::left),
            (KeyCode::Right, _) | (KeyCode::Char('f'), true) => in_input(app, Input::right),
            (KeyCode::Home, _) | (KeyCode::Char('a'), true) => in_input(app, Input::home),
            (KeyCode::End, _) | (KeyCode::Char('e'), true) => in_input(app, Input::end),
            (KeyCode::Char('w'), true) => in_input(app, Input::delete_word),
            (KeyCode::Char('u'), true) => in_input(app, Input::kill_to_start),
            (KeyCode::Char(ch), false) => {
                if let Some(c) = app.composing.as_mut() {
                    c.insert_char(ch);
                }
            }
            _ => {}
        }
        return Action::Continue;
    }

    // The modeless edit session owns everything next (SPEC §3).
    if app.session.is_some() {
        handle_session_key(app, ctx, k);
        return Action::Continue;
    }

    // Overlays capture keys while open.
    if app.overlay.is_some() {
        handle_overlay_key(app, ctx, k.code, k.modifiers);
        return Action::Continue;
    }

    // The sticky move mode owns the plain keys while it is up: h/j/k/l and
    // the bare arrows drag the grabbed block, Esc/Enter/m let go. Every
    // OTHER key lets go first — the drag's one commit goes out — and then
    // does its ordinary job, so navigating away can never leave the block
    // half moved.
    //
    // The arrows are bound because they are what a reader reaches for when
    // a block is visibly held: having them silently drop it and move the
    // cursor instead would be the one surprise this mode must not have.
    if app.move_mode.is_some() {
        // SHIFT counts as plain here: a terminal reports `H` as Shift+`h`,
        // and caps lock or the habit of the `^g H/J/K/L` block bindings
        // must not commit a half-finished drag.
        let plain = (k.modifiers - KeyModifiers::SHIFT) == KeyModifiers::NONE;
        // Lowercase and the bare arrows step one line; uppercase steps over
        // a whole sibling. Caps lock therefore gives a bigger jump, never a
        // commit.
        let drag = match k.code {
            KeyCode::Char('j') if plain => Some((OutlineDirection::Down, false)),
            KeyCode::Char('k') if plain => Some((OutlineDirection::Up, false)),
            KeyCode::Char('J') if plain => Some((OutlineDirection::Down, true)),
            KeyCode::Char('K') if plain => Some((OutlineDirection::Up, true)),
            KeyCode::Char('h' | 'H') if plain => Some((OutlineDirection::Left, false)),
            KeyCode::Char('l' | 'L') if plain => Some((OutlineDirection::Right, false)),
            KeyCode::Down if plain => Some((OutlineDirection::Down, false)),
            KeyCode::Up if plain => Some((OutlineDirection::Up, false)),
            KeyCode::Left if plain => Some((OutlineDirection::Left, false)),
            KeyCode::Right if plain => Some((OutlineDirection::Right, false)),
            _ => None,
        };
        if let Some((direction, whole_sibling)) = drag {
            move_mode_step(app, ctx, direction, whole_sibling);
            return Action::Continue;
        }
        match k.code {
            // `m` grabbed the block, so `m` puts it down again.
            KeyCode::Esc | KeyCode::Enter => {
                leave_move_mode(app, ctx);
                return Action::Continue;
            }
            KeyCode::Char('m' | 'M') if plain => {
                leave_move_mode(app, ctx);
                return Action::Continue;
            }
            _ => leave_move_mode(app, ctx),
        }
    }

    // ^g is a one-shot prefix: whatever follows is consumed, including Esc
    // and unknown keys, so the second key never leaks into another command.
    if app.outline_prefix {
        app.outline_prefix = false;
        if let Some((block, direction)) = outline_prefix_command(k) {
            edit_outline(app, ctx, block, direction);
        } else {
            app.note(t!("アウトライン操作を取り消しました", "outline action cancelled"));
        }
        return Action::Continue;
    }
    if k.code == KeyCode::Char('g') && k.modifiers == KeyModifiers::CONTROL {
        app.outline_prefix = true;
        app.status.clear();
        return Action::Continue;
    }
    if let Some((block, direction)) = outline_arrow(k) {
        edit_outline(app, ctx, block, direction);
        return Action::Continue;
    }

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match (k.code, ctrl) {
        // ---- quit ---- (akapen default: q quits, Esc only cancels; here
        //      the first q asks and the second answers — toast.rs)
        (KeyCode::Char('q'), false) => {
            return if app.confirm_quit() { Action::Quit } else { Action::Continue };
        }
        // `^c` too. In raw mode the terminal hands it over as a key rather
        // than a signal, so without this arm the interrupt habit is a dead
        // key. NOT wired into the edit session or the composer, where the
        // reflex is "cancel what I am typing", not "leave".
        (KeyCode::Char('c'), true) => return Action::Quit,
        (KeyCode::Esc, false) => {
            if app.time.is_some() {
                // Leaving history means the live page replaces the snapshot.
                // If that fetch fails there is no live page to show, so we
                // stay in history: `set_page` (which clears `time`) runs
                // only on success. Clearing `time` first would relabel the
                // SNAPSHOT's lines as NOW and editable, and let the renderer
                // screenshot today's page under a historical source's hash.
                if reload_page(app, ctx) {
                    app.status.clear();
                }
            } else if app.selection.is_some() {
                app.selection = None;
                app.status.clear();
            } else {
                app.status.clear();
                app.dismiss_toast();
            }
        }

        // ---- time machine (←/→, akapen's timeline; server snapshots) ----
        (KeyCode::Left, false) if k.modifiers == KeyModifiers::NONE => travel(app, ctx, -1),
        (KeyCode::Right, false) if k.modifiers == KeyModifiers::NONE => travel(app, ctx, 1),

        // ---- move (akapen parity) ----
        // Shift+↑/↓ selects, in READ as in EDIT: the same fingers, the same
        // result. `v` then j/k (akapen) still works — this is the version
        // people try first. J/K are the same act for j/k hands: Shift is
        // already the "and select" modifier here, so the shifted letter
        // must not mean something else. (In the move mode J/K step over a
        // whole sibling — that state is handled before this match.)
        (KeyCode::Down, false) if k.modifiers == KeyModifiers::SHIFT => read_select_line(app, true),
        (KeyCode::Up, false) if k.modifiers == KeyModifiers::SHIFT => read_select_line(app, false),
        (KeyCode::Char('J'), false) => read_select_line(app, true),
        (KeyCode::Char('K'), false) => read_select_line(app, false),
        (KeyCode::Char('j'), false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(true),
        (KeyCode::Down, false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(true),
        (KeyCode::Char('k'), false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(false),
        (KeyCode::Up, false) if k.modifiers == KeyModifiers::NONE => app.move_cursor(false),
        (KeyCode::Char('g'), false) => {
            if let Some(s) = app.first_src() {
                app.goto_src(s);
            }
        }
        (KeyCode::Char('G'), false) => {
            if let Some(s) = app.last_body_src() {
                app.goto_src(s);
            }
            // G means "bottom of the boxed page", not the related sections
            // below it. j/↓ can still continue from there into Links.
            if app.view_h > 0 {
                app.scroll = app.frame_end_scroll(app.view_h);
                app.follow = false;
            }
        }
        // Paging moves the CURSOR by a screenful / half (akapen): the
        // viewport follows it, so the eye keeps its place on the line.
        (KeyCode::PageDown, _) => app.move_cursor_display(app.view_h.max(1) as isize),
        (KeyCode::PageUp, _) => app.move_cursor_display(-(app.view_h.max(1) as isize)),
        (KeyCode::Char('d'), true) => app.move_cursor_display((app.view_h / 2).max(1) as isize),
        (KeyCode::Char('u'), true) => app.move_cursor_display(-((app.view_h / 2).max(1) as isize)),

        // ---- links & history (akapen's file ]/[ slot) ----
        (KeyCode::Enter, false) | (KeyCode::Char('f'), false) => {
            let mut links = app.cursor_line_links();
            match links.len() {
                0 => app.toast(t!("この行にリンクはありません", "no link on this line")),
                1 => activate_link(app, ctx, links.remove(0)),
                _ => app.overlay = Some(Overlay::Links { items: links, cursor: 0 }),
            }
        }
        (KeyCode::Char('['), false) => go_history(app, ctx, true),
        (KeyCode::Char(']'), false) => go_history(app, ctx, false),

        // ---- link cycling (the browser's Tab: the next link line) ----
        (KeyCode::Tab, _) => cycle_link_line(app, true),
        (KeyCode::BackTab, _) => cycle_link_line(app, false),

        // ---- raw source view (akapen's `Tab view⇄source`, demoted from a
        //      mode on Tab to a display option: what only it can do is show
        //      raw text with line numbers). On `z` — a "how it looks" key
        //      in vim's family — so `s` can be akapen's send. ----
        (KeyCode::Char('z'), false) => {
            // The cursor is a source line, so it carries over as is; the
            // re-layout additionally keeps that line on the same physical
            // screen row (akapen's `replace_view_preserving_cursor`).
            app.mode = match app.mode {
                Mode::View => Mode::Source,
                Mode::Source => Mode::View,
            };
            let (w, h) = (app.laid_width.max(1), app.view_h);
            app.relayout_preserving_screen_row(w, h);
            app.selection = None;
        }

        // ---- edit (akapen's `e edit` slot, back to its true meaning:
        //      the modeless session, SPEC-edit-session.md) ----
        (KeyCode::Char('e'), false) => {
            // caret at the END of the cursor line (続きを書く)
            let caret = app
                .cursor_src()
                .and_then(|s| app.lines.get(s))
                .map(|l| l.text.len())
                .unwrap_or(0);
            enter_session(app, ctx, app.cursor, caret);
        }
        (KeyCode::Char('i'), false) => {
            // caret at the start of the text (right after the indent)
            let caret = app
                .cursor_src()
                .and_then(|s| app.lines.get(s))
                .map(|l| indent_of(&l.text).len())
                .unwrap_or(0);
            enter_session(app, ctx, app.cursor, caret);
        }

        // ---- the sticky outline move mode (**m**ove) ----
        // One block, dragged with plain keys for as long as it takes, then
        // one commit. The modified arrows still do single steps from here
        // and from EDIT; this is the route that needs no modifier at all.
        (KeyCode::Char('m'), false) if k.modifiers == KeyModifiers::NONE => {
            enter_move_mode(app, ctx)
        }

        // ---- render this page's missing web artifacts ----
        // Rendering is manual by default: a page load only shows artifacts
        // it already has on disk, because launching a browser is by far the
        // most expensive thing this viewer does. Uppercase `R` is generic:
        // Mermaid today, and TeX / icons / ProjectCSS-backed views later.
        (KeyCode::Char('R'), false) => {
            if app.start_web_renders(capability::Trigger::Manual) {
                // Drawing; the blocks pulse and say the rest themselves.
            } else if app.web_pending.iter().any(|k| !app.images.contains_key(k)) {
                // The page-load cache probe still owns these keys, so the
                // request cannot be queued yet. Remember it and serve it the
                // moment the probe reports its misses — pressing `R` twice
                // should not be part of the interface.
                app.web_manual_wanted = true;
            } else if app.note.is_none() {
                app.note_web_failure(match app.render_policy {
                    capability::RenderPolicy::Off => {
                        t!("diagram: レンダラは off です (COSENSE_WEB_RENDER)", "diagram: renderer is off (COSENSE_WEB_RENDER)")
                    }
                    _ => t!("diagram: 描画するものはありません", "diagram: nothing to draw"),
                });
            }
        }

        // ---- open in browser (moved from `e`: **w**eb) ----
        (KeyCode::Char('w'), false) => {
            let url = app.cursor_url();
            if open_in_browser(&url) {
                app.note(t!("開きました {url}", "opened {url}"));
            } else {
                app.toast_err(t!("ブラウザを開けません", "failed to open browser"));
            }
        }

        // ---- undo / redo (the safety net; no confirmation gates) ----
        // `u` is the akapen/vim seat; `^z` is the same key the session
        // uses, so the reflex works whichever mode you happen to be in.
        (KeyCode::Char('u'), false) | (KeyCode::Char('z'), true) => {
            undo(app, ctx);
        }
        (KeyCode::Char('v'), true) => app.paste_clipboard_image(ctx),
        (KeyCode::Char('r'), true) => {
            redo(app, ctx);
        }

        // ---- comments ----
        // READ keeps two keys for them: `c` writes one, `l` lists them
        // (and is where delete / copy / send live). `s` is the shortcut
        // for sending. The keys that used to sit here (`v` select, `d`
        // delete, `^n`/`^p` jump) say where their job went — a key that
        // silently stops working is worse than one that explains itself.
        (KeyCode::Char('v'), false) => {
            app.toast(t!("選択は Shift+↑↓ か J/K で（v は廃止）", "select with Shift+↑↓ or J/K (v is gone)"));
        }
        (KeyCode::Char('c'), false) => {
            // In history too: the comment is pinned to the snapshot on
            // screen ("this version had it right — put it back"), shown
            // only there, and exported with the command that reads it.
            // The same range again means "edit that comment": the
            // composer opens on its text and Enter replaces it.
            let existing = app.comment_for_range().map(|i| app.comments[i].text.clone());
            let editing = existing.is_some();
            app.composing = Some(Input::new(existing.unwrap_or_default()));
            app.laid_width = 0; // the bar opens under the range
            app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode));
            app.status = if editing {
                t!("コメントを編集 · Enter 置き換え · Esc 取消", "edit comment · Enter replace · Esc cancel")
            } else {
                t!("コメントを入力 · Enter 保存 · Esc 取消", "type comment · Enter save · Esc cancel")
            };
        }
        (KeyCode::Char('s'), false) => send_comments(app, ctx),

        // ---- new lines (vim's o/O; the session opens on the new line) ----
        (KeyCode::Char('o'), false) => open_line(app, ctx, false),
        (KeyCode::Char('O'), false) => open_line(app, ctx, true),
        (KeyCode::Char('e'), true) => {
            // Whole-page edit in $EDITOR (the “big edit” path: multi-line,
            // IME-free — the editor is the user's own environment).
            if outline_mutation_blocked(app) || !ensure_editable(app) {
                return Action::Continue;
            }
            if app.time.is_some() {
                app.toast_err(t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)"));
                return Action::Continue;
            }
            return Action::Editor;
        }
        // `x` used to delete the cursor line (or the selection) right
        // here, in the mode meant for reading. Deletion now lives where
        // editing does: `^k` for a line, Shift+↑↓ then ⌫ for a range. The
        // key is kept only to say so — a key that silently stops working
        // is worse than one that explains itself.
        (KeyCode::Char('x'), false) => {
            app.toast(t!("削除は編集中に — e で入って ^k（行）/ Shift+↑↓ と ⌫（範囲）", "delete while editing — e, then ^k (line) or Shift+↑↓ and ⌫ (range)"));
        }
        (KeyCode::Char('d'), false) => {
            app.toast(t!("コメントの削除は l の一覧で d", "delete a comment in the l list with d"));
        }
        (KeyCode::Char('n'), true) | (KeyCode::Char('p'), true) => {
            app.toast(t!("コメントへは l の一覧から Enter で", "reach a comment from the l list with Enter"));
        }

        // ---- output ----
        // `y` copies what is under the cursor (or the selection); `Y` the
        // whole page. Copying the COMMENTS moved to the comments overlay
        // (`l` then `y`), where the comments are.
        (KeyCode::Char('y'), false) => {
            let payload = copy_payload(app, false);
            copy_and_report(app, payload);
        }
        (KeyCode::Char('Y'), false) => {
            let payload = copy_payload(app, true);
            copy_and_report(app, payload);
        }

        // ---- lists / help ----
        (KeyCode::Char('l'), false) => {
            app.overlay = Some(Overlay::Comments { cursor: 0 });
        }
        // The project index (akapen's `^o files` slot): a full-width list
        // with a short excerpt from the selected page docked below. Typing
        // filters it, so the old picker's fingers still work — they now
        // have a screen to work in.
        (KeyCode::Char('o'), true) => {
            let from = app.here();
            let project = app.project.clone();
            open_index(app, ctx, &project, String::new());
            if app.index.is_some() {
                // Opening the index is going somewhere, so it goes on the
                // stack: `[` from here returns to the page it was opened
                // from, and `[` from a page opened out of it returns here.
                app.history.push(from);
                app.forward.clear();
            }
        }
        // line detail: who edited the cursor line and when (toggle)
        (KeyCode::Char('t'), false) => {
            app.overlay = if matches!(app.overlay, Some(Overlay::LineInfo)) {
                None
            } else {
                // resolve the updater's name for THIS project before showing
                let want = app.cursor_src().and_then(|s| app.lines.get(s)).map(|l| l.user_id.clone());
                app.ensure_members(ctx, want.as_deref());
                Some(Overlay::LineInfo)
            };
        }
        (KeyCode::Char('?'), false) => app.overlay = Some(Overlay::Help),

        // Unbound: show what arrived. A NON-ASCII char here almost always
        // means the IME is on — say so in Japanese instead of a cryptic
        // "unbound key" (the ime helper normally prevents this state).
        _ => {
            if let KeyCode::Char(c) = k.code {
                if !c.is_ascii() {
                    // Reading is done in ASCII — j/k/e/q are the language
                    // here — so put the input source back rather than only
                    // complaining about it. (Typing Japanese belongs to the
                    // edit session, which turns the IME on by itself.)
                    let fixed = app.session_ime.force_ascii();
                    if fixed {
                        app.toast(t!("英数に戻しました（日本語は e / i で編集に入ってから）", "switched back to ASCII (Japanese needs an edit session: e / i)"));
                    } else {
                        app.toast(t!("IMEがONのようです — 英数に切り替えてください（編集中は自動で日本語になります）", "the IME looks ON — switch to ASCII (an edit session turns it on for you)"));
                    }
                    return Action::Continue;
                }
            }
            app.toast(t!("割り当てのないキー: {:?} {:?}", "unbound key: {:?} {:?}", k.code, k.modifiers));
        }
    }
    Action::Continue
}

/// Key handling while an overlay is open.
///
/// The Pages picker is a filter box: printable keys type into the filter, so
/// movement there uses arrows / ^n / ^p. The other overlays are plain lists
/// and keep j/k.
pub(crate) fn handle_overlay_key(app: &mut App, ctx: &Ctx, code: KeyCode, mods: KeyModifiers) {
    enum Act {
        None,
        Close,
        Up,
        Down,
        Activate,
    }
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let is_line_info = matches!(app.overlay, Some(Overlay::LineInfo));
    let act = match code {
        KeyCode::Esc => Act::Close,
        // `t` toggles the line-detail overlay back off.
        KeyCode::Char('t') if is_line_info => Act::Close,
        KeyCode::Enter => Act::Activate,
        KeyCode::Down => Act::Down,
        KeyCode::Up => Act::Up,
        KeyCode::Char('n') if ctrl => Act::Down,
        KeyCode::Char('p') if ctrl => Act::Up,
        // The comments list is where the comments are worked on: `y`
        // copies them all, `s` sends them (as `S` does outside), `d`
        // deletes the one under the cursor.
        KeyCode::Char('s') if matches!(app.overlay, Some(Overlay::Comments { .. })) => {
            send_comments(app, ctx);
            if app.comments.is_empty() {
                app.overlay = None;
            }
            Act::None
        }
        KeyCode::Char('d') if matches!(app.overlay, Some(Overlay::Comments { .. })) => {
            if let Some(Overlay::Comments { cursor }) = app.overlay.as_mut() {
                if *cursor < app.comments.len() {
                    app.comments.remove(*cursor);
                    app.laid_width = 0;
                    *cursor = (*cursor).min(app.comments.len().saturating_sub(1));
                    app.note(t!("コメントを削除しました", "comment deleted"));
                }
                if app.comments.is_empty() {
                    app.overlay = None;
                }
            }
            Act::None
        }
        KeyCode::Char('y') if matches!(app.overlay, Some(Overlay::Comments { .. })) => {
            let text = format_all(&app.comments);
            if app.comments.is_empty() {
                app.toast(t!("コピーするコメントがありません", "no comments to copy"));
            } else if copy_to_clipboard(&text) {
                app.note(t!("✓ コメント {} 件をコピーしました", "✓ copied {} comment(s)", app.comments.len()));
            } else {
                app.toast_err(t!("コピーできません — クリップボードのコマンドが無く、端末も OSC 52 を拒否しました", "copy failed — no clipboard tool and the terminal refused OSC 52"));
            }
            Act::None
        }
        KeyCode::Char('q') => Act::Close,
        KeyCode::Char('j') => Act::Down,
        KeyCode::Char('k') => Act::Up,
        _ => Act::None,
    };

    let len = match &app.overlay {
        Some(Overlay::Comments { .. }) => app.comments.len(),
        Some(Overlay::Links { items, .. }) => items.len(),
        _ => 0,
    };

    match act {
        Act::Close => app.overlay = None,
        Act::Down => {
            if let Some(
                Overlay::Comments { cursor } | Overlay::Links { cursor, .. },
            ) = app.overlay.as_mut()
            {
                if len > 0 {
                    *cursor = (*cursor + 1).min(len - 1);
                }
            }
        }
        Act::Up => {
            if let Some(
                Overlay::Comments { cursor } | Overlay::Links { cursor, .. },
            ) = app.overlay.as_mut()
            {
                *cursor = cursor.saturating_sub(1);
            }
        }
        Act::Activate => {
            // The link picker mixes pages and files: hand the item over.
            if let Some(Overlay::Links { items, cursor }) = &app.overlay {
                let item = items.get(*cursor).cloned();
                app.overlay = None;
                if let Some(item) = item {
                    activate_link(app, ctx, item);
                }
                return;
            }
            let create = false;
            let target: Option<(String, String, Option<usize>)> = match &app.overlay {
                Some(Overlay::Comments { cursor }) => app
                    .comments
                    .get(*cursor)
                    .map(|c| (c.project.clone(), c.title.clone(), Some(c.start))),
                _ => None,
            };
            let revision = match &app.overlay {
                Some(Overlay::Comments { cursor }) => {
                    app.comments.get(*cursor).and_then(|c| c.snapshot_id().map(str::to_string))
                }
                _ => None,
            };
            app.overlay = None;
            if let Some((project, title, line)) = target {
                if project != app.project || title != app.title {
                    navigate_to(app, ctx, &project, &title);
                }
                // A comment lives on one revision: go there before landing
                // on its line (akapen: the timeline rewinds to the draft
                // the comment was written on; a NOW comment brings you
                // back to NOW).
                match revision {
                    Some(id) => show_revision(app, ctx, &id),
                    None if app.time.is_some() => {
                        if reload_page(app, ctx) {
                            app.status.clear();
                        }
                    }
                    None => {}
                }
                if create {
                    // Land in EDIT on a fresh body line. Nothing is sent
                    // yet: an empty page is not created until it has
                    // something in it (see `dispatch_create`).
                    app.cursor = 0;
                    open_line(app, ctx, false);
                }
                if let Some(src) = line {
                    // The page may have just been loaded and not laid out
                    // yet; `rebuild` clamps the cursor onto a rendered line.
                    app.cursor = src;
                    app.follow = true;
                }
            }
        }
        Act::None => {}
    }
}
