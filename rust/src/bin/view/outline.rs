use super::*;

/// The pre-action model one structural action can be rolled back to.
#[derive(Clone)]
pub(crate) struct OutlineSnapshot {
    pub(crate) project: String,
    pub(crate) page_id: String,
    /// Page-install generation, not the commit-queue generation. Navigating
    /// away and back to the same immutable page id is still a different
    /// installation whose lines may predate this commit.
    pub(crate) install_gen: u64,
    pub(crate) lines: Vec<PageLine>,
    pub(crate) cursor: usize,
    pub(crate) selection: Option<Selection>,
    pub(crate) undo_stack: Vec<(String, Vec<EditOp>)>,
    pub(crate) redo_stack: Vec<(String, Vec<EditOp>)>,
    pub(crate) history_dropped: bool,
}

/// The one outstanding structural action: which commit job decides its
/// fate, and what to put back if that job does not land. The job id is what
/// identifies the outcome — ordering and in-flight counts cannot, because
/// an ordinary commit may still be on its way when the action starts.
#[derive(Clone)]
pub(crate) struct OutlinePending {
    pub(crate) job: CommitJobId,
    pub(crate) snapshot: OutlineSnapshot,
}

/// The sticky outline move mode (NOTE-outline-editing.md §移動モード): the
/// reader grabs one block with `m` and keeps dragging it with plain keys.
/// Every step is LOCAL — the existing line values are reordered and
/// re-indented in place, ids and all, nothing is sent and nothing is
/// pushed onto the undo stack — and leaving sends ONE commit for the whole
/// drag. Five presses are one commit, one new set of ids, one undo step.
#[derive(Clone)]
pub(crate) struct MoveMode {
    /// Line ids of the grabbed block, frozen at entry. Outdenting inside
    /// the mode makes the lines below read as children; the reader still
    /// picked up THESE lines, so the set never grows.
    pub(crate) block: Vec<String>,
    /// The page as it stood before the grab. The one exit commit is the
    /// diff from here to wherever the block ended up.
    pub(crate) before: Vec<PageLine>,
    pub(crate) cursor: usize,
    pub(crate) selection: Option<Selection>,
}

/// Structural arrows use exact modifiers. This deliberately excludes
/// Shift+Alt, Ctrl+Alt, and every other combination.
pub(crate) fn outline_arrow(k: event::KeyEvent) -> Option<(bool, OutlineDirection)> {
    let block = match k.modifiers {
        KeyModifiers::CONTROL => false,
        KeyModifiers::ALT => true,
        _ => return None,
    };
    let direction = match k.code {
        KeyCode::Left => OutlineDirection::Left,
        KeyCode::Right => OutlineDirection::Right,
        KeyCode::Up => OutlineDirection::Up,
        KeyCode::Down => OutlineDirection::Down,
        _ => return None,
    };
    Some((block, direction))
}

/// The portable ^g follow-up. Uppercase characters may arrive with or
/// without an explicit SHIFT flag depending on the terminal protocol.
pub(crate) fn outline_prefix_command(k: event::KeyEvent) -> Option<(bool, OutlineDirection)> {
    let plain = k.modifiers == KeyModifiers::NONE;
    let upper = plain || k.modifiers == KeyModifiers::SHIFT;
    match k.code {
        KeyCode::Char('h') if plain => Some((false, OutlineDirection::Left)),
        KeyCode::Char('j') if plain => Some((false, OutlineDirection::Down)),
        KeyCode::Char('k') if plain => Some((false, OutlineDirection::Up)),
        KeyCode::Char('l') if plain => Some((false, OutlineDirection::Right)),
        KeyCode::Char('H') if upper => Some((true, OutlineDirection::Left)),
        KeyCode::Char('J') if upper => Some((true, OutlineDirection::Down)),
        KeyCode::Char('K') if upper => Some((true, OutlineDirection::Up)),
        KeyCode::Char('L') if upper => Some((true, OutlineDirection::Right)),
        _ => None,
    }
}

pub(crate) fn outline_mutation_blocked(app: &mut App) -> bool {
    if app.move_mode.is_some() {
        app.status = t!(
            "移動モード中 — Esc/Enter で確定してから",
            "in move mode — commit it first with Esc/Enter"
        );
        return true;
    }
    if app.outline_refresh_needed {
        app.status = t!(
            "アウトライン移動後の再読み込み待ち — ページを開き直してください",
            "outline refresh required — reopen the page before editing"
        );
        return true;
    }
    if app.outline_pending.is_some() {
        app.status = t!(
            "アウトライン操作の完了待ち — 編集・取り消し・やり直しは待ってください",
            "waiting for outline action — edit, undo, and redo are temporarily blocked"
        );
        return true;
    }
    false
}

pub(crate) fn outline_snapshot(app: &App) -> OutlineSnapshot {
    OutlineSnapshot {
        project: app.project.clone(),
        page_id: app.page_id.clone(),
        install_gen: app.gen_now(),
        lines: app.lines.clone(),
        cursor: app.cursor,
        selection: app.selection,
        undo_stack: app.undo_stack.clone(),
        redo_stack: app.redo_stack.clone(),
        history_dropped: app.history_dropped,
    }
}

pub(crate) fn restore_outline_snapshot(app: &mut App, snapshot: &OutlineSnapshot) {
    app.lines = snapshot.lines.clone();
    app.cursor = snapshot.cursor;
    app.selection = snapshot.selection;
    app.undo_stack = snapshot.undo_stack.clone();
    app.redo_stack = snapshot.redo_stack.clone();
    app.history_dropped = snapshot.history_dropped;
}

pub(crate) fn outline_error(app: &mut App, error: PlanError) {
    app.status = match error {
        PlanError::InvalidTarget => t!("カーソルの下にソース行がありません", "no source line under the cursor"),
        PlanError::TitleProtected => t!("タイトル行はアウトライン操作できません", "the title line is protected"),
        PlanError::CannotOutdent => t!(
            "字下げを戻せません — 対象の全行に字下げが必要です",
            "cannot outdent — every target line must be indented"
        ),
        PlanError::Boundary => t!("これ以上移動できません", "cannot move any farther"),
        PlanError::NotSibling => t!(
            "同じ親を持つ兄弟ブロックがありません",
            "no sibling block with the same parent"
        ),
    };
}

/// The refusals every outline action shares, in the order the reader
/// meets them. `false` means the reason is already on the status line.
pub(crate) fn outline_action_allowed(app: &mut App) -> bool {
    // An ordinary commit may still be in flight: the gate waits for its own
    // job id, and the serial worker keeps the ops in order. Only a second
    // structural action has to wait, because there is one snapshot to roll
    // back to and one gate to release.
    if outline_mutation_blocked(app) {
        return false;
    }
    if app.web_unsynced {
        app.status = t!(
            "サーバーの内容を読み直すまでアウトライン操作できません",
            "outline actions require a fresh server copy"
        );
        return false;
    }
    if app.time.is_some() {
        app.status = t!("履歴を表示中 — 読み取り専用（Esc で最新へ）", "viewing history — read-only (Esc → NOW)");
        return false;
    }
    if !ensure_editable(app) {
        return false;
    }
    if app.cursor >= app.lines.len() {
        outline_error(app, PlanError::InvalidTarget);
        return false;
    }
    if page_is_uncreated(app) {
        app.status = t!(
            "未作成ページではアウトライン操作できません",
            "outline actions are unavailable until the page is created"
        );
        return false;
    }
    true
}

/// Hand one structural op list to the single write path, under the gate
/// and the rollback every outline action shares: the snapshot is the page
/// as it stands NOW, so a job the worker never took leaves no screen-only
/// fact behind.
pub(crate) fn queue_outline_action(app: &mut App, ctx: &Ctx, label: &str, done: String, ops: Vec<EditOp>) {
    let snapshot = outline_snapshot(app);
    if let Some(job) = do_edit(app, ctx, label, ops) {
        app.outline_pending = Some(OutlinePending { job, snapshot });
        app.follow = true;
        app.status = done;
    } else {
        // The worker never took the structural job. Put the clean
        // pre-action model back immediately; an optimistic move must never
        // become a screen-only fact.
        restore_outline_snapshot(app, &snapshot);
        rerender(app, ctx);
    }
}

/// Lower one pure outline plan into the existing edit queue. Replacements
/// keep IDs. Moves delete and recreate only the user's target.
pub(crate) fn edit_outline(app: &mut App, ctx: &Ctx, block: bool, direction: OutlineDirection) {
    if !outline_action_allowed(app) {
        return;
    }
    if block && app.selection.is_some() {
        app.status = t!(
            "選択中はブロック操作できません — Esc で選択を解除",
            "block actions are unavailable with a selection — Esc clears it"
        );
        return;
    }

    let scope = if block {
        OutlineScope::Block(app.cursor)
    } else {
        let (start, end) = app.selection.map(|selection| selection.range()).unwrap_or((app.cursor, app.cursor));
        OutlineScope::Lines(LineRange::new(start, end))
    };
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    let plan = match cosense::outline::plan(&source, scope, direction) {
        Ok(plan) => plan,
        Err(error) => {
            outline_error(app, error);
            return;
        }
    };

    let (label, done, ops) = match plan {
        OutlinePlan::Replace { range, texts } => {
            let ops = (range.start..=range.end)
                .zip(texts)
                .map(|(line, text)| EditOp::Replace { id: app.lines[line].id.clone(), text })
                .collect();
            (
                t!("アウトラインの字下げ", "outline indent"),
                t!("…字下げを保存中", "…saving indentation"),
                ops,
            )
        }
        OutlinePlan::Move { range, destination, destination_start: _ } => {
            let anchor = match destination {
                Destination::Before(line) => app.lines[line].id.clone(),
                Destination::End => "_end".to_string(),
            };
            let inserted: Vec<(String, String)> = app.lines[range.start..=range.end]
                .iter()
                .map(|line| (new_line_id(), line.text.clone()))
                .collect();
            let mut ops: Vec<EditOp> = app.lines[range.start..=range.end]
                .iter()
                .map(|line| EditOp::Delete { id: line.id.clone() })
                .collect();
            ops.push(EditOp::Insert { anchor, lines: inserted });
            (
                t!("アウトラインの移動", "outline move"),
                t!("…移動を保存中", "…saving move"),
                ops,
            )
        }
    };

    queue_outline_action(app, ctx, &label, done, ops);
}

/// Where the grabbed block sits right now, found by its frozen ids. `None`
/// means the run is no longer there, which the mode itself cannot cause.
pub(crate) fn move_block_range(app: &App) -> Option<(usize, usize)> {
    let block = &app.move_mode.as_ref()?.block;
    let first = block.first()?;
    let start = app.lines.iter().position(|line| line.id == *first)?;
    let end = start + block.len() - 1;
    let run = app.lines.get(start..=end)?;
    run.iter()
        .map(|line| line.id.as_str())
        .eq(block.iter().map(|id| id.as_str()))
        .then_some((start, end))
}

/// `m`: grab the block under the cursor and stay in the mode until
/// `Esc`/`Enter`. Nothing is sent yet — the grab is a promise to send one
/// commit later.
pub(crate) fn enter_move_mode(app: &mut App, ctx: &Ctx) {
    if !outline_action_allowed(app) {
        return;
    }
    // A block operation with a selection open has no single answer — the
    // same refusal the Alt and ^g block bindings give. Refusing is better
    // than carrying a selection that the drag would silently drop.
    if app.selection.is_some() {
        app.status = t!(
            "選択中はブロック操作できません — Esc で選択を解除",
            "block actions are unavailable with a selection — Esc clears it"
        );
        return;
    }
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    let range = match cosense::outline::grab(&source, app.cursor) {
        Ok(range) => range,
        Err(error) => {
            outline_error(app, error);
            return;
        }
    };
    app.move_mode = Some(MoveMode {
        block: app.lines[range.start..=range.end]
            .iter()
            .map(|line| line.id.clone())
            .collect(),
        before: app.lines.clone(),
        cursor: app.cursor,
        selection: app.selection,
    });
    app.follow = true;
    app.status.clear();
    rerender(app, ctx);
}

/// One press inside the mode. Purely local: the same `PageLine` values are
/// reordered or re-indented, so every id — and every permalink, telomere
/// and comment hanging off it — stays exactly where it was. Nothing goes
/// to the server, nothing goes onto the undo stack.
pub(crate) fn move_mode_step(app: &mut App, ctx: &Ctx, direction: OutlineDirection, whole_sibling: bool) {
    let Some((start, end)) = move_block_range(app) else {
        // The grabbed lines are gone. Nothing in the mode can do that, and
        // remote application is held while it is up, so this is the
        // unexpected case — which is exactly why it must not keep the
        // half-dragged arrangement. The page goes back to what it was when
        // the block was picked up, and the disagreement is recorded.
        let Some(mode) = app.move_mode.take() else { return };
        app.lines = mode.before;
        app.cursor = mode.cursor.min(app.lines.len().saturating_sub(1));
        app.selection = mode.selection;
        app.mark_desynced();
        app.follow = true;
        rerender(app, ctx);
        app.status = t!(
            "つかんでいたブロックが見つからないので、つかむ前の並びに戻しました",
            "the grabbed block is gone — restored the arrangement from before the grab"
        );
        return;
    };
    let source: Vec<String> = app.lines.iter().map(|line| line.text.clone()).collect();
    // Two vertical steps, and the reader picks which one. `Lines` moves the
    // block past ONE source line, keeping its depth: it never refuses (bar
    // the title), and Up and Down are exact inverses. `Grabbed` steps over
    // a whole sibling subtree at once, which is the fast way to reorder
    // sections but has nowhere to go when there is no sibling.
    //
    // Requiring the sibling step was the mistake this replaces: it refused
    // moves that were reachable anyway by going out a level, stepping, and
    // coming back in — so the rule cost keystrokes without protecting any
    // arrangement.
    let range = LineRange::new(start, end);
    let scope = if whole_sibling && matches!(direction, OutlineDirection::Up | OutlineDirection::Down)
    {
        OutlineScope::Grabbed(range)
    } else {
        OutlineScope::Lines(range)
    };
    let plan = match cosense::outline::plan(&source, scope, direction) {
        Ok(plan) => plan,
        Err(error) => {
            // Only the whole-sibling step can refuse, and the answer is
            // one press away: `j` steps a single line and never refuses.
            outline_error(app, error);
            return;
        }
    };
    // Ids survive every step, so the cursor and the selection travel by id.
    let cursor_id = app.lines.get(app.cursor).map(|line| line.id.clone());
    let selection_ids = app.selection.and_then(|selection| {
        let anchor = app.lines.get(selection.anchor)?.id.clone();
        let cursor = app.lines.get(selection.cursor)?.id.clone();
        Some((anchor, cursor))
    });
    match plan {
        OutlinePlan::Replace { range, texts } => {
            for (line, text) in (range.start..=range.end).zip(texts) {
                app.lines[line].text = text;
            }
        }
        OutlinePlan::Move { range, destination: _, destination_start } => {
            let block: Vec<PageLine> = app.lines.drain(range.start..=range.end).collect();
            let at = destination_start.min(app.lines.len());
            app.lines.splice(at..at, block);
        }
    }
    if let Some(id) = cursor_id {
        if let Some(line) = app.lines.iter().position(|line| line.id == id) {
            app.cursor = line;
        }
    }
    app.selection = selection_ids.and_then(|(anchor, cursor)| {
        let anchor = app.lines.iter().position(|line| line.id == anchor)?;
        let cursor = app.lines.iter().position(|line| line.id == cursor)?;
        Some(Selection { anchor, cursor })
    });
    app.follow = true;
    app.status.clear();
    rerender(app, ctx);
}

/// The ONE edit from the pre-mode page to the final arrangement: nothing
/// when the block came home, `Replace` (ids kept) when only its
/// indentation changed, and — exactly as cosense itself does it — delete
/// plus insert of the block ALONE when its position changed.
/// Returns `(commit label, status while saving, ops)`.
pub(crate) fn move_mode_edit(
    before: &[PageLine],
    after: &[PageLine],
    block: &[String],
) -> Option<(String, String, Vec<EditOp>)> {
    let moved = before
        .iter()
        .map(|line| line.id.as_str())
        .ne(after.iter().map(|line| line.id.as_str()));
    if !moved {
        let ops: Vec<EditOp> = before
            .iter()
            .zip(after)
            .filter(|(was, now)| was.text != now.text)
            .map(|(_, now)| EditOp::Replace { id: now.id.clone(), text: now.text.clone() })
            .collect();
        return (!ops.is_empty()).then(|| {
            (
                t!("アウトラインの字下げ", "outline indent"),
                t!("…字下げを保存中", "…saving indentation"),
                ops,
            )
        });
    }
    let first = block.first()?;
    let start = after.iter().position(|line| line.id == *first)?;
    let final_block = after.get(start..start + block.len())?;
    // The block is contiguous wherever it landed, so the line after it is
    // never one of its own — and it still exists in the pre-mode page.
    let anchor = after
        .get(start + block.len())
        .map(|line| line.id.clone())
        .unwrap_or_else(|| "_end".to_string());
    let mut ops: Vec<EditOp> =
        block.iter().map(|id| EditOp::Delete { id: id.clone() }).collect();
    ops.push(EditOp::Insert {
        anchor,
        lines: final_block
            .iter()
            .map(|line| (new_line_id(), line.text.clone()))
            .collect(),
    });
    Some((
        t!("アウトラインの移動", "outline move"),
        t!("…移動を保存中", "…saving move"),
        ops,
    ))
}

/// `Esc`/`Enter` (and anything else the reader asks for): let go of the
/// block and send the whole drag as one commit. The pre-mode arrangement
/// goes back first, so the ops are applied exactly once — by `do_edit`,
/// the same write path every other edit uses, which is also what makes it
/// one undo step and what puts the cursor back on the moved block.
pub(crate) fn leave_move_mode(app: &mut App, ctx: &Ctx) {
    let Some(mode) = app.move_mode.take() else { return };
    let after = std::mem::replace(&mut app.lines, mode.before);
    app.cursor = mode.cursor;
    app.selection = mode.selection;
    let Some((label, done, ops)) = move_mode_edit(&app.lines, &after, &mode.block) else {
        // The block came home: position and depth are what they were, so
        // there is nothing to tell the server.
        rerender(app, ctx);
        app.follow = true;
        app.status = t!(
            "移動モードを抜けました（変更なし）",
            "left move mode (nothing changed)"
        );
        return;
    };
    queue_outline_action(app, ctx, &label, done, ops);
}

#[derive(Clone, Copy)]
pub(crate) struct MoveShape {
    pub(crate) start: usize,
    pub(crate) len: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct MoveRebase {
    pub(crate) cursor: usize,
    pub(crate) selection: Option<(usize, usize)>,
}

/// Recognize the move lowering shared by outline actions and their history:
/// delete one contiguous target, then recreate the same-size run with fresh IDs.
/// Replace-only indentation history deliberately does not pass this gate.
pub(crate) fn move_shape(app: &App, ops: &[EditOp]) -> Option<MoveShape> {
    let (last, deletes) = ops.split_last()?;
    let EditOp::Insert { lines: inserted, .. } = last else { return None };
    if deletes.is_empty() || deletes.len() != inserted.len() {
        return None;
    }
    let deleted: Vec<&str> = deletes
        .iter()
        .map(|op| match op {
            EditOp::Delete { id } => Some(id.as_str()),
            EditOp::Insert { .. } | EditOp::Replace { .. } => None,
        })
        .collect::<Option<_>>()?;
    let start = app.lines.iter().position(|line| line.id == deleted[0])?;
    let source = app.lines.get(start..start + deleted.len())?;
    if source.iter().map(|line| line.id.as_str()).ne(deleted.iter().copied())
        || inserted.iter().any(|(id, _)| app.lines.iter().any(|line| line.id == *id))
    {
        return None;
    }
    Some(MoveShape { start, len: deleted.len() })
}

/// If these ops are a source move, remember offsets inside the logical
/// target before its old IDs disappear. Duplicate line text is irrelevant:
/// rebasing uses the fresh IDs already carried by the insert op.
pub(crate) fn prepare_move_rebase(app: &App, ops: &[EditOp]) -> Option<MoveRebase> {
    let shape = move_shape(app, ops)?;
    let cursor = app.cursor.checked_sub(shape.start)?.min(shape.len - 1);
    let selection = app.selection.and_then(|selection| {
        let (a, b) = selection.range();
        (a == shape.start && b + 1 == shape.start + shape.len)
            .then(|| (selection.anchor - shape.start, selection.cursor - shape.start))
    });
    Some(MoveRebase { cursor, selection })
}

pub(crate) fn apply_move_rebase(app: &mut App, ops: &[EditOp], rebase: MoveRebase) -> bool {
    let Some(EditOp::Insert { lines: inserted, .. }) = ops.last() else { return false };
    let locate = |offset: usize| app.lines.iter().position(|line| line.id == inserted[offset].0);
    let Some(cursor) = locate(rebase.cursor) else { return false };
    app.cursor = cursor;
    app.selection = rebase.selection.and_then(|(anchor, cursor)| {
        locate(anchor).zip(locate(cursor)).map(|(anchor, cursor)| Selection { anchor, cursor })
    });
    app.follow = true;
    true
}

/// Reject one optimistic structural action, invalidate anything still
/// queued against it, and replace it with server truth. The pre-action
/// snapshot is restored first, so even a failed reload cannot leave the
/// outline move as a screen-only success.
pub(crate) fn recover_outline_action(app: &mut App, ctx: &Ctx, pending: OutlineSnapshot, reason: String) {
    recover_outline_action_with(app, ctx, pending, reason, reload_page);
}

/// Recovery is parameterized at this narrow fetch seam so the successful
/// reload path can be tested without a live Cosense server.
pub(crate) fn recover_outline_action_with(
    app: &mut App,
    ctx: &Ctx,
    pending: OutlineSnapshot,
    reason: String,
    reload: impl FnOnce(&mut App, &Ctx) -> bool,
) {
    app.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let same_page = app.project == pending.project && app.page_id == pending.page_id;
    if !same_page {
        app.status = t!(
            "前のページのアウトライン操作に失敗しました（{reason}）",
            "outline action on the previous page failed ({reason})"
        );
        return;
    }

    let cursor_id = pending.lines.get(pending.cursor).map(|line| line.id.clone());
    let selection_ids = pending.selection.and_then(|selection| {
        pending
            .lines
            .get(selection.anchor)
            .zip(pending.lines.get(selection.cursor))
            .map(|(anchor, cursor)| (anchor.id.clone(), cursor.id.clone()))
    });

    app.mark_desynced();
    restore_outline_snapshot(app, &pending);
    rerender(app, ctx);

    if reload(app, ctx) {
        // A reload normally clears page-local history in `set_page`. Put the
        // pre-action lineage back only if the response is still for the page
        // that owned the rejected action, and discard entries whose IDs no
        // longer exist in the authoritative response.
        if app.project == pending.project && app.page_id == pending.page_id {
            app.undo_stack = pending.undo_stack;
            app.redo_stack = pending.redo_stack;
            let dropped = retain_replayable_history(&mut app.undo_stack, &app.lines)
                + retain_replayable_history(&mut app.redo_stack, &app.lines);
            app.history_dropped = pending.history_dropped || dropped != 0;

            if let Some(id) = cursor_id {
                if let Some(cursor) = app.lines.iter().position(|line| line.id == id) {
                    app.cursor = cursor;
                }
            }
            app.selection = selection_ids.and_then(|(anchor_id, cursor_id)| {
                app.lines
                    .iter()
                    .position(|line| line.id == anchor_id)
                    .zip(app.lines.iter().position(|line| line.id == cursor_id))
                    .map(|(anchor, cursor)| Selection { anchor, cursor })
            });
            app.follow = true;
        }
        app.status = t!(
            "アウトライン操作に失敗しました（{reason}）— サーバーの内容を読み直しました",
            "outline action failed ({reason}) — reloaded server state"
        );
    } else {
        let reload_reason = app.status.clone();
        app.status = t!(
            "アウトライン操作に失敗しました（{reason}）— 変更を戻しました。{reload_reason}",
            "outline action failed ({reason}) — reverted the change. {reload_reason}"
        );
    }
}
