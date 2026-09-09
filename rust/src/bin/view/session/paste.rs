//! Multi-line paste and external-editor round trips.

use super::*;

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
        session_commit_dirty(app, ctx);
        return;
    }
    // Multi-line: a live selection goes first — the paste replaces it,
    // as any typed character's would — then the fragments land plain.
    session_replace_selection(app, ctx, "");
    let Some(s) = app.session.as_ref() else {
        return;
    };
    let (line, caret, buf) = (s.line, s.input.cur, s.input.buf.clone());
    let mut parts = clean.split('\n');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let head = format!("{}{}", &buf[..caret], first);
    let tail_of_line = &buf[caret..];
    let last_part = rest.last().copied().unwrap_or("");
    let mut inserted: Vec<(String, String)> = Vec::new();
    for (i, p) in rest.iter().enumerate() {
        let text = if i + 1 == rest.len() {
            format!("{p}{tail_of_line}")
        } else {
            (*p).to_string()
        };
        inserted.push((new_line_id(), text));
    }
    let last_id = inserted.last().map(|(id, _)| id.clone());
    let anchor = app
        .lines
        .get(line + 1)
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "_end".into());
    let ops = vec![
        EditOp::Replace {
            id: app.lines[line].id.clone(),
            text: head,
        },
        EditOp::Insert {
            anchor,
            lines: inserted,
        },
    ];
    do_edit(app, ctx, &t!("貼り付け", "paste"), ops);
    if let (Some(s), Some(last_id)) = (app.session.as_mut(), last_id) {
        if let Some(idx) = app.lines.iter().position(|l| l.id == last_id) {
            s.line = idx;
            let text = app.lines[idx].text.clone();
            s.input = Input {
                buf: text.clone(),
                cur: last_part.len().min(text.len()),
            };
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
/// Hand the terminal to another program: leave the alt screen and raw
/// mode, and undo every mode the viewer switched on — mouse reporting,
/// bracketed paste and, when the terminal took it, the kitty keyboard
/// protocol. Left pushed, the last one turns the editor's Ctrl+Q into a
/// `CSI u` sequence it cannot read (micro shows it and stays open).
pub(crate) fn suspend_tui(ctx: &Ctx) {
    if ctx.keyboard_enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
}

/// Take the terminal back after `suspend_tui`.
pub(crate) fn resume_tui(terminal: &mut ratatui::DefaultTerminal, ctx: &Ctx) {
    *terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    if ctx.keyboard_enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
}

pub(crate) fn editor_roundtrip(terminal: &mut ratatui::DefaultTerminal, app: &mut App, ctx: &Ctx) {
    let original: String = app
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let path =
        std::env::temp_dir().join(format!("cosense-{}-{}.txt", std::process::id(), now_secs()));
    if let Err(e) = std::fs::write(&path, format!("{original}\n")) {
        app.toast_err(t!("一時ファイルを作れません: {e}", "temp file failed: {e}"));
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());

    // Suspend the TUI for the editor, restore it after — whatever happens.
    suspend_tui(ctx);
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} '{}'", path.display()))
        .status();
    resume_tui(terminal, ctx);
    app.laid_width = 0; // re-lay out on the next frame

    let ok = matches!(status, Ok(s) if s.success());
    let edited = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !ok {
        app.toast_err(t!(
            "エディタが中断しました（{editor}）— 何も書き込んでいません",
            "editor aborted ({editor}) — nothing written"
        ));
        return;
    }
    let edited = edited.strip_suffix('\n').unwrap_or(&edited).to_string();
    if edited == original {
        app.toast(t!("変更はありません", "no changes"));
        return;
    }
    let new_lines: Vec<String> = edited.split('\n').map(str::to_string).collect();
    if new_lines.iter().all(|l| l.trim().is_empty()) {
        app.toast_err(t!(
            "ページが空になるため中止しました（ページの削除はブラウザで）",
            "page emptied — refusing (delete pages in the browser)"
        ));
        return;
    }
    let old: Vec<(String, String)> = app
        .lines
        .iter()
        .map(|l| (l.id.clone(), l.text.clone()))
        .collect();
    let ops = diff_to_ops(&old, &new_lines);
    if ops.is_empty() {
        app.toast(t!("変更はありません", "no changes"));
    } else {
        let n = ops.len();
        do_edit(app, ctx, &t!("エディタ", "editor"), ops);
        app.note(t!(
            "✓ エディタの変更を {n} 件コミットしました · u で戻せます",
            "✓ editor: {n} op(s) committed · u to undo"
        ));
    }
}
