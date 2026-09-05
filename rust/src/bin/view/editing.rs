use super::*;

/// A single-line text input with a movable cursor (byte index, always on
/// a char boundary). The composer used to be append-only; Japanese text
/// especially needs mid-line correction without retyping everything.
pub(crate) struct Input {
    pub(crate) buf: String,
    pub(crate) cur: usize,
}

impl Input {
    pub(crate) fn new(buf: String) -> Self {
        let cur = buf.len();
        Input { buf, cur }
    }
    pub(crate) fn parts(&self) -> (&str, &str) {
        self.buf.split_at(self.cur)
    }
    pub(crate) fn insert_char(&mut self, ch: char) {
        self.buf.insert(self.cur, ch);
        self.cur += ch.len_utf8();
    }
    pub(crate) fn insert_str(&mut self, s: &str) {
        self.buf.insert_str(self.cur, s);
        self.cur += s.len();
    }
    pub(crate) fn left(&mut self) {
        if let Some(ch) = self.buf[..self.cur].chars().next_back() {
            self.cur -= ch.len_utf8();
        }
    }
    pub(crate) fn right(&mut self) {
        if let Some(ch) = self.buf[self.cur..].chars().next() {
            self.cur += ch.len_utf8();
        }
    }
    /// Start of the caret's logical line (just after the previous `\n`).
    pub(crate) fn line_start(&self) -> usize {
        self.buf[..self.cur].rfind('\n').map_or(0, |i| i + 1)
    }
    /// End of the caret's logical line (just before the next `\n`).
    pub(crate) fn line_end(&self) -> usize {
        self.buf[self.cur..].find('\n').map_or(self.buf.len(), |i| self.cur + i)
    }
    /// ^a / Home: to the start of the caret's line (a comment may span
    /// lines — see `up` / `down`).
    pub(crate) fn home(&mut self) {
        self.cur = self.line_start();
    }
    /// ^e / End: to the end of the caret's line.
    pub(crate) fn end(&mut self) {
        self.cur = self.line_end();
    }
    /// ↑: the same character column on the previous logical line
    /// (clamped to its length); nothing on the first line.
    pub(crate) fn up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let col = self.buf[start..self.cur].chars().count();
        let prev_start = self.buf[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cur = Self::col_to_byte(&self.buf, prev_start, start - 1, col);
    }
    /// ↓: the same character column on the next logical line.
    pub(crate) fn down(&mut self) {
        let end = self.line_end();
        if end >= self.buf.len() {
            return;
        }
        let col = self.buf[self.line_start()..self.cur].chars().count();
        let next_start = end + 1;
        let next_end = self.buf[next_start..].find('\n').map_or(self.buf.len(), |i| next_start + i);
        self.cur = Self::col_to_byte(&self.buf, next_start, next_end, col);
    }
    fn col_to_byte(buf: &str, start: usize, end: usize, col: usize) -> usize {
        buf[start..end].char_indices().nth(col).map_or(end, |(i, _)| start + i)
    }
    pub(crate) fn backspace(&mut self) {
        if let Some(ch) = self.buf[..self.cur].chars().next_back() {
            self.cur -= ch.len_utf8();
            self.buf.remove(self.cur);
        }
    }
    pub(crate) fn delete(&mut self) {
        if self.cur < self.buf.len() {
            self.buf.remove(self.cur);
        }
    }
    /// ^w: delete the word (or whitespace run) left of the cursor.
    pub(crate) fn delete_word(&mut self) {
        let head = &self.buf[..self.cur];
        let trimmed = head.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        self.buf.replace_range(cut..self.cur, "");
        self.cur = cut;
    }
    /// ^u: kill to the start of the line.
    pub(crate) fn kill_to_start(&mut self) {
        self.buf.replace_range(..self.cur, "");
        self.cur = 0;
    }
    /// ^k: kill to the end of the line.
    pub(crate) fn kill_to_end(&mut self) {
        self.buf.truncate(self.cur);
    }
}

/// Identifies one queued commit for its whole life, outcome included.
pub(crate) type CommitJobId = u64;

/// One queued commit for the serial background worker. `id` names this job
/// so its outcome can be recognised; `gen` invalidates jobs queued before a
/// conflict reload (their base state is gone).
pub(crate) struct CommitJob {
    pub(crate) id: CommitJobId,
    pub(crate) gen: u64,
    pub(crate) project: String,
    pub(crate) page_id: String,
    pub(crate) label: String,
    pub(crate) ops: Vec<EditOp>,
}

/// What a commit attempt came back with. Every variant carries the id of
/// the job it answers: outcomes arrive one at a time but jobs are queued
/// freely, so nothing about arrival order says which job an outcome is for.
pub(crate) enum CommitOutcome {
    /// `commit_id` is the id the server gave this commit — the name our
    /// own edit will come back under on the websocket (see
    /// `App::own_commits`).
    Done { job: CommitJobId, label: String, title: String, commit_id: String },
    Conflict { job: CommitJobId },
    Skipped { job: CommitJobId },
    Failed { job: CommitJobId, label: String, msg: String },
}

impl CommitOutcome {
    /// The job this outcome answers.
    pub(crate) fn job(&self) -> CommitJobId {
        match self {
            CommitOutcome::Done { job, .. }
            | CommitOutcome::Conflict { job }
            | CommitOutcome::Skipped { job }
            | CommitOutcome::Failed { job, .. } => *job,
        }
    }
}

/// The serial commit worker: one job in flight at a time, in queue order —
/// ordering is what keeps every op valid against the server's state.
pub(crate) fn spawn_commit_worker(
    client: Client,
    jobs: mpsc::Receiver<CommitJob>,
    out: mpsc::Sender<CommitOutcome>,
    gen: Arc<std::sync::atomic::AtomicU64>,
) {
    std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            let id = job.id;
            if job.gen < gen.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = out.send(CommitOutcome::Skipped { job: id });
                continue;
            }
            let res = client
                .preview_edit(&job.project, &job.page_id, &job.ops)
                .and_then(|p| client.submit_edit(&job.project, &p.preview_id));
            let outcome = match res {
                Ok(c) => CommitOutcome::Done {
                    job: id,
                    label: job.label,
                    title: c.title,
                    commit_id: c.commit_id,
                },
                Err(EditError::NotFastForward) => CommitOutcome::Conflict { job: id },
                Err(e) => CommitOutcome::Failed { job: id, label: job.label, msg: e.to_string() },
            };
            let _ = out.send(outcome);
        }
    });
}

/// Apply one `Input` editing method to the open composer.
pub(crate) fn in_input(app: &mut App, f: fn(&mut Input)) {
    if let Some(c) = app.composing.as_mut() {
        f(c);
    }
}

/// Enter in the comment composer.
pub(crate) fn finish_composer(app: &mut App, input: Input) {
    let buf = input.buf;
    app.laid_width = 0; // the bar closes either way
    app.status.clear(); // the composer's key hint goes with it
    if buf.trim().is_empty() {
        app.note(t!("空のコメントは破棄しました", "empty comment discarded"));
    } else if let Some(c) = app.make_comment(buf) {
        match app.comment_for_range() {
            Some(i) => {
                app.comments[i] = c;
                app.note(t!("コメントを置き換えました", "comment replaced"));
            }
            None => {
                app.comments.push(c);
                app.note(t!("コメントを保存しました（全 {} 件）", "comment saved ({} total)", app.comments.len()));
            }
        }
        app.selection = None;
        app.laid_width = 0; // force rebuild to weave the card
    } else {
        app.toast_err(t!("コメントを行に結び付けられません", "could not anchor comment"));
    }
}

// ---------------------------------------------------------------------------
// The edit engine (SPEC-edit-session.md §4–§6): the LOCAL model is
// authoritative — every edit applies to `app.lines` immediately (insert ids
// are client-generated, so no reload is ever needed) and a serial worker
// commits the same ops in order in the background. Undo is the diff back.
// ---------------------------------------------------------------------------

/// Re-render the page body from the (just mutated) local model.
pub(crate) fn rerender(app: &mut App, ctx: &Ctx) {
    // The source is about to change shape. Anything already queued against
    // the old text must not come back and be filed under it.
    app.bump_src_epoch();
    let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
    // THE PAGE's palette (theme-tinted at load), not ctx's: the terminal
    // scheme would repaint the links mid-page — the colour of a page the
    // reader just left.
    let r = render_lines_with(&texts, Some(&ctx.hl), &app.palette, &app.links);
    app.blocks = r.blocks;
    app.srcs = r.srcs;
    app.hits = r.hits;
    app.laid_width = 0;
    app.start_image_loads(ctx);
    app.start_web_renders(capability::Trigger::Auto);
}

/// Can these ops still be applied to the page as it now stands? Every id
/// they name has to be there — `_end` always is.
pub(crate) fn ops_replayable(ops: &[EditOp], live: &std::collections::HashSet<&str>) -> bool {
    ops.iter().all(|op| match op {
        EditOp::Insert { anchor, .. } => anchor == "_end" || live.contains(anchor.as_str()),
        EditOp::Replace { id, .. } | EditOp::Delete { id } => live.contains(id.as_str()),
    })
}

/// Keep every history entry that can be reached in the order the user will
/// pop it. Validity is sequential: a newer undo can recreate an id required
/// by the older undo below it, so checking every entry against only the
/// initial live page discards perfectly usable lineage.
pub(crate) fn retain_replayable_history(
    stack: &mut Vec<(String, Vec<EditOp>)>,
    live_lines: &[PageLine],
) -> usize {
    let mut simulated = live_lines.to_vec();
    let mut keep = vec![false; stack.len()];
    for index in (0..stack.len()).rev() {
        let live: std::collections::HashSet<&str> =
            simulated.iter().map(|line| line.id.as_str()).collect();
        if ops_replayable(&stack[index].1, &live) {
            apply_ops(&mut simulated, &stack[index].1);
            keep[index] = true;
        }
    }
    let before = stack.len();
    let mut index = 0usize;
    stack.retain(|_| {
        let kept = keep[index];
        index += 1;
        kept
    });
    before - stack.len()
}

pub(crate) fn ensure_editable(app: &mut App) -> bool {
    if app.editable {
        true
    } else {
        app.toast_err(t!("このプロジェクトでは編集権限がありません", "no edit permission in this project"));
        false
    }
}

/// Apply `ops` locally, push their inverse onto the undo stack, and queue
/// the background commit. The single write path for every edit.
/// Returns the commit job the edit was queued under, when it reached the
/// worker at all (an uncreated page commits nothing yet; see below).
pub(crate) fn do_edit(app: &mut App, ctx: &Ctx, label: &str, ops: Vec<EditOp>) -> Option<CommitJobId> {
    if outline_mutation_blocked(app) || !ensure_editable(app) || ops.is_empty() {
        return None;
    }
    let inverse = invert_ops(&app.lines, &ops);
    let move_rebase = prepare_move_rebase(app, &ops);
    apply_ops(&mut app.lines, &ops);
    if let Some(rebase) = move_rebase {
        apply_move_rebase(app, &ops, rebase);
    }
    app.undo_stack.push((label.to_string(), inverse));
    if app.undo_stack.len() > 200 {
        app.undo_stack.remove(0);
    }
    app.redo_stack.clear();
    // A fresh edit starts a fresh lineage: an older drop no longer
    // explains anything.
    app.history_dropped = false;
    let job = if page_is_uncreated(app) {
        // Nothing to commit against yet. The edit lives locally and the
        // whole page goes up as one create instead — sending these ops
        // would need a pageId that does not exist. Once the create is out,
        // later edits wait for the page to come back rather than sending a
        // second create, which would make a second page.
        if app.create_state != CreateState::Sent {
            app.create_state = CreateState::Needed;
        }
        None
    } else {
        queue_commit(app, label, ops)
    };
    rerender(app, ctx);
    job
}

/// Byte offset just past the text an edit changed: everything before the
/// common prefix and after the common suffix is what moved, so the caret
/// belongs at the end of the new middle. Typing lands after what you
/// typed; taking it away lands where it was.
pub(crate) fn changed_end(old: &str, new: &str) -> usize {
    let mut prefix = 0usize;
    for ((i, a), b) in old.char_indices().zip(new.chars()) {
        if a != b {
            break;
        }
        prefix = i + a.len_utf8();
    }
    let mut suffix = 0usize;
    let mut o = old.char_indices().rev();
    let mut n = new.char_indices().rev();
    while let (Some((oi, a)), Some((ni, b))) = (o.next(), n.next()) {
        if a != b || oi < prefix || ni < prefix {
            break;
        }
        suffix += a.len_utf8();
    }
    new.len().saturating_sub(suffix).max(prefix)
}

/// Where an applied edit leaves the caret: on the line it changed, just
/// after the change — which is the whole point of undo/redo. Being
/// returned to where the caret happened to be says nothing about what
/// moved; being taken to the change shows it.
///
/// `before` is the page as it stood before the ops were applied.
pub(crate) fn edit_focus(
    lines: &[PageLine],
    ops: &[EditOp],
    before: &[(String, String)],
) -> Option<(String, usize)> {
    let old_text = |id: &str| -> &str {
        before.iter().find(|(i, _)| i == id).map(|(_, t)| t.as_str()).unwrap_or("")
    };
    // A line that still exists is the better place to stand.
    for op in ops {
        match op {
            EditOp::Insert { lines: ins, .. } => {
                if let Some((id, text)) = ins.last() {
                    return Some((id.clone(), text.len()));
                }
            }
            EditOp::Replace { id, text } => {
                return Some((id.clone(), changed_end(old_text(id), text)));
            }
            EditOp::Delete { .. } => continue,
        }
    }
    // Nothing but deletions: stand where the first deleted line was — the
    // line that slid up into its place, at its start. When nothing slid up
    // (the page ends there), the deletion happened at the END of the line
    // above, so that is where the caret belongs.
    let first = ops.iter().find_map(|op| match op {
        EditOp::Delete { id } => before.iter().position(|(i, _)| i == id),
        _ => None,
    })?;
    match lines.get(first) {
        Some(l) => Some((l.id.clone(), 0)),
        None => lines.last().map(|l| (l.id.clone(), l.text.len())),
    }
}

/// The page id to edit against, or empty when there is nothing to edit
/// against yet.
///
/// Cosense answers 200 for a title that does not exist AND hands out an
/// id with it — a provisional one, for a page it has not made. Committing
/// against that id writes into nothing: the viewer looks like it saved and
/// the web shows an empty page. `persistent` is the only field that says
/// whether the page is real, so it is the one to read.
pub(crate) fn live_page_id(page: &cosense::api::Page) -> String {
    if page.persistent {
        page.id.clone()
    } else {
        String::new()
    }
}

/// How far an uncreated page has got toward existing.
///
/// The distinction that matters is `Sent`: the create carries the whole
/// page, so once it is out, a second one must never follow. The server
/// answers a second create with a SECOND page (same title, auto-suffixed),
/// and the writing splits between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CreateState {
    /// The page exists, or nothing has been typed into it yet.
    Idle,
    /// Local edits are waiting to bring the page into being.
    Needed,
    /// The create is out. Later edits stay local until the page comes back
    /// with an id, and `adopt_created_page` commits them as a diff.
    Sent,
}

/// Does this page exist on the server yet? Cosense answers 200 for any
/// title, so a link to an uncreated page opens as a template with just its
/// title line; it becomes real on the first commit.
pub(crate) fn page_is_uncreated(app: &App) -> bool {
    app.page_id.is_empty()
}

/// Bring an uncreated page into being: one insert of the whole local page,
/// with no `pageId` — the API reads the first inserted line as the title.
/// Sent once, when nothing else is in flight, so a burst of typing cannot
/// race two creates (which the server would answer with two pages, the
/// second auto-suffixed).
pub(crate) fn dispatch_create(app: &mut App) {
    if app.create_state != CreateState::Needed || !page_is_uncreated(app) || app.inflight > 0 {
        return;
    }
    // A title and nothing else is not a page. Opening a name from the
    // picker lands you in EDIT on an empty line, and renaming that title
    // while you think about it is still just thinking: walking away must
    // leave no trace either way. Content is what makes a page.
    if !app.lines.iter().skip(1).any(|l| !l.text.trim().is_empty()) {
        return;
    }
    app.create_state = CreateState::Sent;
    let lines: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    queue_commit(app, &t!("ページの作成", "create page"), vec![EditOp::Insert { anchor: "_end".into(), lines }]);
    app.status = t!("ページを作成しています…", "creating the page…");
}

/// Send one commit job to the serial worker. Returns the id the job was
/// queued under, so a caller that has to recognise its own outcome (an
/// outline action) can wait for that id and nothing else. `None` means the
/// worker is gone and the edit never left the machine.
pub(crate) fn queue_commit(app: &mut App, label: &str, ops: Vec<EditOp>) -> Option<CommitJobId> {
    let id = app.next_job_id;
    app.next_job_id += 1;
    let job = CommitJob {
        id,
        gen: app.gen.load(std::sync::atomic::Ordering::SeqCst),
        project: app.project.clone(),
        page_id: app.page_id.clone(),
        label: label.to_string(),
        ops,
    };
    if app.commit_tx.send(job).is_ok() {
        app.inflight += 1;
        Some(id)
    } else {
        app.status = t!("コミット処理が停止しました — 編集はこの画面にしか残りません", "commit worker gone — edits are LOCAL ONLY");
        // The edit never left the machine: local lines and the server have
        // parted ways, and every diagram on the page must stay as source.
        app.mark_desynced();
        None
    }
}

/// `u`: revert the newest commit (locally at once, on the server via the
/// queue). Ids of replaced lines survive; re-inserted lines get fresh ids.
pub(crate) fn undo(app: &mut App, ctx: &Ctx) -> bool {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return false;
    }
    let Some((_, next_ops)) = app.undo_stack.last() else {
        app.toast(empty_history_reason(app, t!("取り消せる編集がありません", "nothing to undo")));
        return false;
    };
    let structural = move_shape(app, next_ops).is_some();
    // Capture the pre-action model before either the page or its history
    // changes. An ordinary commit in flight is no obstacle: the gate below
    // waits for this action's own job id.
    let snapshot = structural.then(|| outline_snapshot(app));

    let (label, ops) = app.undo_stack.pop().expect("history was checked above");
    let redo = invert_ops(&app.lines, &ops);
    let before: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let move_rebase = prepare_move_rebase(app, &ops);
    let replace_selection = app.selection.filter(|_| {
        move_rebase.is_none() && ops.iter().all(|op| matches!(op, EditOp::Replace { .. }))
    });
    apply_ops(&mut app.lines, &ops);
    app.redo_stack.push((label.clone(), redo));
    let focus = edit_focus(&app.lines, &ops, &before);
    let job = queue_commit(app, &format!("undo {label}"), ops.clone());
    if let Some(snapshot) = snapshot {
        match job {
            Some(job) => app.outline_pending = Some(OutlinePending { job, snapshot }),
            None => {
                // The worker never took the structural job — nothing will
                // ever answer for it, so undo the optimistic change now.
                restore_outline_snapshot(app, &snapshot);
                rerender(app, ctx);
                return false;
            }
        }
    }
    rerender(app, ctx);
    let seated = if let Some(selection) = replace_selection {
        // Horizontal outline changes keep every line and every endpoint.
        // The selection's cursor is the active end; focusing the first
        // Replace would make an upward selection visibly jump to its anchor.
        app.selection = Some(selection);
        app.cursor = selection.cursor;
        app.follow = true;
        true
    } else {
        move_rebase
            .map(|rebase| apply_move_rebase(app, &ops, rebase))
            .unwrap_or_else(|| app.focus_edit(focus))
    };
    app.note(t!("{label} を取り消しました（あと {} 件）", "undid {label} ({} more)", app.undo_stack.len()));
    seated
}

/// Why is there nothing to undo/redo? "Never had any" and "the server
/// moved and took the lineage with it" are different answers, and silence
/// makes the second look like a broken key.
pub(crate) fn empty_history_reason(app: &App, empty: String) -> String {
    if app.history_dropped {
        // Which of the two stacks is empty does not matter here: the web
        // edit dropped both, and that is the whole answer.
        t!("履歴は web 側の更新で失効しました", "history was dropped by a web edit")
    } else {
        empty
    }
}

/// `^r`: re-apply the newest undone commit.
pub(crate) fn redo(app: &mut App, ctx: &Ctx) -> bool {
    if outline_mutation_blocked(app) || !ensure_editable(app) {
        return false;
    }
    let Some((_, next_ops)) = app.redo_stack.last() else {
        app.toast(empty_history_reason(app, t!("やり直せる編集がありません", "nothing to redo")));
        return false;
    };
    let structural = move_shape(app, next_ops).is_some();
    // As in `undo`: snapshot first, gate on this action's own job id.
    let snapshot = structural.then(|| outline_snapshot(app));

    let (label, ops) = app.redo_stack.pop().expect("history was checked above");
    let undo_ops = invert_ops(&app.lines, &ops);
    let before: Vec<(String, String)> =
        app.lines.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    let move_rebase = prepare_move_rebase(app, &ops);
    let replace_selection = app.selection.filter(|_| {
        move_rebase.is_none() && ops.iter().all(|op| matches!(op, EditOp::Replace { .. }))
    });
    apply_ops(&mut app.lines, &ops);
    app.undo_stack.push((label.clone(), undo_ops));
    let focus = edit_focus(&app.lines, &ops, &before);
    let job = queue_commit(app, &format!("redo {label}"), ops.clone());
    if let Some(snapshot) = snapshot {
        match job {
            Some(job) => app.outline_pending = Some(OutlinePending { job, snapshot }),
            None => {
                restore_outline_snapshot(app, &snapshot);
                rerender(app, ctx);
                return false;
            }
        }
    }
    rerender(app, ctx);
    let seated = if let Some(selection) = replace_selection {
        app.selection = Some(selection);
        app.cursor = selection.cursor;
        app.follow = true;
        true
    } else {
        move_rebase
            .map(|rebase| apply_move_rebase(app, &ops, rebase))
            .unwrap_or_else(|| app.focus_edit(focus))
    };
    app.note(t!("{label} をやり直しました", "redid {label}"));
    seated
}

// ---------------------------------------------------------------------------
// The modeless edit session (SPEC §1–§3).
// ---------------------------------------------------------------------------
