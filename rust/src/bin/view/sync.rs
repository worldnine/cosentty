use super::*;

/// Optional diagnostic trace for reproducing sync/undo failures. It is off
/// by default; set `COSENTTY_DEBUG_LOG=/path/to/file` to enable it.
pub(crate) fn debug_log(message: impl AsRef<str>) {
    let Some(path) = std::env::var_os("COSENTTY_DEBUG_LOG") else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{} {}", chrono_like_timestamp(), message.as_ref());
    }
}

fn chrono_like_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs().to_string(),
        Err(_) => "0".into(),
    }
}

/// A commit came back from the worker (drained per frame).
pub(crate) fn handle_commit_outcome(app: &mut App, ctx: &Ctx, outcome: CommitOutcome) {
    match &outcome {
        CommitOutcome::Done { job, label, .. } => {
            debug_log(format!("commit done job={job} label={label}"))
        }
        CommitOutcome::Conflict { job } => debug_log(format!("commit conflict job={job}")),
        CommitOutcome::Skipped { job } => debug_log(format!("commit skipped job={job}")),
        CommitOutcome::Failed {
            job, label, msg, ..
        } => {
            debug_log(format!("commit failed job={job} label={label} error={msg}"));
        }
    }
    app.inflight = app.inflight.saturating_sub(1);
    let origin = app.commit_origins.remove(&outcome.job());
    // Take the structural gate only for the job it is actually waiting on.
    // An ordinary commit may have been in flight when the action started, or
    // been queued behind it, and either could come back first — arrival
    // order says nothing about ownership, the job id does.
    let outline = match app.outline_pending.as_ref() {
        Some(pending) if pending.job == outcome.job() => {
            app.outline_pending.take().map(|pending| pending.snapshot)
        }
        _ => None,
    };
    // Structural jobs have their own snapshot recovery below. Ordinary
    // jobs also outlive navigation: their result must never rename the new
    // page or run conflict recovery against that page's local text.
    if outline.is_none() {
        if let Some(origin) = origin {
            let same_page = app.project == origin.project && app.page_id == origin.page_id;
            if !same_page || app.gen_now() != origin.install_gen {
                match &outcome {
                    CommitOutcome::Done { .. } if same_page => {
                        // Away and back: invalidate an older poll and ask
                        // for a fresh snapshot. The normal remote gate
                        // protects any new local edits until it can land.
                        app.bump_server_epoch();
                        app.ws_resync_pending = true;
                    }
                    CommitOutcome::Failed { msg, .. } => app.toast_err(t!(
                        "移動前のページ /{}/{} の保存に失敗しました: {msg}",
                        "save failed for previous page /{}/{}: {msg}",
                        origin.project,
                        origin.title
                    )),
                    CommitOutcome::Conflict { .. } => app.toast_err(t!(
                        "移動前のページ /{}/{} の保存が競合しました",
                        "save conflicted for previous page /{}/{}",
                        origin.project,
                        origin.title
                    )),
                    _ => {}
                }
                return;
            }
        }
    }
    match outcome {
        CommitOutcome::Done {
            job: _,
            label,
            title,
            commit_id,
        } => {
            // Navigation may still happen while the gate is up. In that case
            // this result belongs wholly to the old page: clearing the gate
            // is the only current-app state it may touch.
            if let Some(pending) = outline.as_ref() {
                let same_page = app.project == pending.project && app.page_id == pending.page_id;
                let same_install = same_page && app.gen_now() == pending.install_gen;
                if !same_install {
                    // Navigation is allowed while saving. Even an away/back
                    // trip to the same page installed a snapshot from before
                    // this commit, so the old outcome must not rename or
                    // otherwise mutate that installation. If it is the same
                    // page, reload once to reveal the successful move.
                    if same_page {
                        // The commit succeeded after this installation was
                        // fetched. Until an authoritative reload lands, its
                        // line IDs are stale and neither edits nor web
                        // rendering may trust them.
                        app.bump_server_epoch();
                        app.mark_desynced();
                        app.outline_refresh_needed = true;
                        if reload_page(app, ctx) {
                            if !commit_id.is_empty() {
                                app.own_commits.push_back(commit_id);
                                while app.own_commits.len() > OWN_COMMIT_MEMORY {
                                    app.own_commits.pop_front();
                                }
                            }
                            app.note(format!("✓ {label}"));
                        }
                    }
                    return;
                }
            }
            if !commit_id.is_empty() {
                app.own_commits.push_back(commit_id);
                while app.own_commits.len() > OWN_COMMIT_MEMORY {
                    app.own_commits.pop_front();
                }
            }
            // The server now holds something a poll started before this may
            // not know about.
            app.bump_server_epoch();
            // Title-line edits rename the page (auto-suffix included).
            if !title.is_empty() && title != app.title {
                app.title = title;
                if let Ok(mut t) = app.poll_target.lock() {
                    *t = (app.project.clone(), app.title.clone());
                }
            }
            // Local activity makes collaboration likely again. A fallback
            // poll that had reached its idle minute must return to the fast
            // edge; a live websocket needs no extra request.
            app.reset_fallback_poll();
            // A toast never displaces the standing status (a selection
            // hint, an upload in flight), so the tick can always be said.
            app.note(format!("✓ {label}"));
            // A page that just came into being has an id we do not know
            // yet, and every edit until we do has to wait. An idle fallback
            // poll and websocket insurance can both be 60 s away; a quit in
            // that gap would lose held edits, so fetch it now.
            if page_is_uncreated(app) && app.create_state == CreateState::Sent {
                adopt_after_create(app, ctx);
            }
        }
        CommitOutcome::Skipped { job: _ } => {
            if let Some(pending) = outline {
                recover_outline_action(app, ctx, pending, t!("処理が失効", "invalidated"));
            }
        }
        CommitOutcome::Failed {
            job: _,
            label,
            msg,
            structural,
        } => {
            if let Some(pending) = outline {
                recover_outline_action(
                    app,
                    ctx,
                    pending,
                    t!(
                        "送信失敗: {label} — {msg}",
                        "commit failed: {label} — {msg}"
                    ),
                );
            } else {
                let failure = t!(
                    "コミットに失敗しました: {label} — {msg}",
                    "commit failed: {label} — {msg}"
                );
                // Keep the exact server response in the standing footer as
                // well as the short-lived toast. A failed optimistic edit
                // can otherwise look like a successful undo after the toast
                // disappears, while the local model is already desynced.
                app.status = failure.clone();
                app.toast_err(failure);
                app.mark_desynced();
                if app.create_state == CreateState::Sent && page_is_uncreated(app) {
                    // The page was never made. Let the next edit try again
                    // rather than retrying in a loop against a dead network.
                    app.create_state = CreateState::Idle;
                } else if structural {
                    // Lines this job made or removed never reached the
                    // server, and every job queued behind it names them.
                    // The worker is holding those back; re-basing on the
                    // server's page (new `gen`) is what lets saving resume.
                    // Without this, one failed split left the page unable
                    // to save anything more until a 409 happened along.
                    recover_by_reload(app, ctx, Rebase::FailedSave);
                }
            }
        }
        CommitOutcome::Conflict { job: _ } => {
            if let Some(pending) = outline {
                recover_outline_action(app, ctx, pending, t!("競合", "conflict"));
            } else {
                recover_conflict(app, ctx);
            }
        }
    }
}

/// Fetch the page we just created and take its id. One blocking request,
/// once per created page: the alternative is holding every later edit
/// until a poll happens to notice, and losing them if the viewer quits
/// first. A failure is not fatal — the poller adopts it later.
pub(crate) fn adopt_after_create(app: &mut App, ctx: &Ctx) {
    if let Ok(page) = ctx.client.get_page_in(&app.project, &app.title) {
        adopt_created_page(app, ctx, &page);
    }
}

/// The page we typed now exists on the server. Take its id and its line
/// ids as the base, then commit whatever was typed after the create left —
/// those keystrokes were never part of it.
///
/// The lines come back with the ids WE generated (the create names its own
/// lines), so the cursor, the session and the telomere all survive this.
pub(crate) fn adopt_created_page(app: &mut App, ctx: &Ctx, page: &cosense::api::Page) {
    if !page.persistent {
        return; // a provisional id is not a page (see `live_page_id`)
    }
    let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
    let session_id = app
        .session
        .as_ref()
        .and_then(|s| app.lines.get(s.line))
        .map(|l| l.id.clone());
    let local: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
    let server: Vec<(String, String)> = page
        .lines
        .iter()
        .map(|l| (l.id.clone(), l.text.clone()))
        .collect();
    app.page_id = page.id.clone();
    app.lines = page.lines.clone();
    app.create_state = CreateState::Idle;
    // It exists now. Pages that link here are drawing it as uncreated on
    // the strength of a reading that is one commit out of date.
    app.links.learn(&app.title.clone(), true);
    app.bump_server_epoch();
    app.mark_synced();
    let ops = cosense::editops::diff_to_ops(&server, &local);
    if ops.is_empty() {
        rerender(app, ctx);
        app.note(t!("✓ ページを作成しました", "✓ page created"));
    } else {
        // `do_edit` applies locally, stacks the undo and queues the commit
        // — the same path any other edit takes, now that there is a page.
        do_edit(app, ctx, &t!("新規ページの同期", "sync new page"), ops);
        app.note(t!("✓ ページを作成しました", "✓ page created"));
    }
    reanchor_cursor_session(app, cursor_id, session_id);
}

/// The shared apply gate: never install remote state while the local
/// model is at stake — viewing history, a commit in flight, a composer
/// open, or a dirty caret line. Polling drops the page when gated (it
/// refetches soon anyway); websocket commits are BUFFERED instead and
/// flushed the moment the gate drops (`ws_flush_pending`).
pub(crate) fn remote_gate_clear(app: &App) -> bool {
    app.time.is_none()
        && app.inflight == 0
        && app.composing.is_none()
        // A grabbed block is uncommitted local work exactly like a dirty
        // caret line: installing someone else's page under it would drag
        // the block against lines the reader never saw.
        && app.move_mode.is_none()
        && !app.session.as_ref().map(|s| s.input.buf != s.orig).unwrap_or(false)
}

/// Replace the local page model with `page`'s lines, keeping the cursor and
/// a clean session on THEIR line ids (a vanished line closes the session),
/// clearing the local undo lineage (its anchors came from the old state),
/// and re-rendering. Polled pages and websocket resyncs both land here.
/// `status: None` installs silently (the periodic insurance resync says
/// nothing when it changed nothing).
pub(crate) fn install_remote_lines(
    app: &mut App,
    ctx: &Ctx,
    page: &cosense::api::Page,
    status: Option<&str>,
) {
    // A full page from the server replaces the local lines wholesale, so
    // whatever a failed commit had left diverging is resolved here. This
    // and `set_page` are the only places the desync flag clears.
    app.bump_server_epoch();
    app.mark_synced();
    let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
    let session_id = app
        .session
        .as_ref()
        .and_then(|s| app.lines.get(s.line))
        .map(|l| l.id.clone());

    // The related list is deliberately NOT rebuilt here. A resync brings
    // the body (v2), which carries no `relatedPages` at all — rebuilding
    // from it would silently empty the sections every time a remote commit
    // landed. The graph below the page is the one already fetched at page
    // install (`App::start_related_load`), and a line edited on the web
    // does not change who links here.
    app.lines = page.lines.clone();
    // Remote lineage: an entry whose anchor line the server no longer has
    // cannot be replayed, but the rest still can. Dropping the WHOLE
    // history here is what made `^r` look dead: a single web-side edit (or
    // one fallback poll that differed) silently took the redo stack with it.
    let dropped = retain_replayable_history(&mut app.undo_stack, &app.lines)
        + retain_replayable_history(&mut app.redo_stack, &app.lines);
    if dropped > 0 {
        app.history_dropped = true;
    }
    rerender(app, ctx);
    reanchor_cursor_session(app, cursor_id, session_id);
    app.follow = true;
    if let Some(s) = status {
        app.note(s);
    }
}

/// Re-anchor the cursor and a clean session onto their line ids after the
/// page model changed (full install or remote diff); a vanished session
/// line closes the session.
pub(crate) fn reanchor_cursor_session(
    app: &mut App,
    cursor_id: Option<String>,
    session_id: Option<String>,
) {
    // Keep the cursor on ITS line (by id), not its number.
    if let Some(id) = cursor_id {
        if let Some(i) = app.lines.iter().position(|l| l.id == id) {
            app.cursor = i;
        }
    }
    // Re-anchor the session the same way; a vanished line closes it.
    if let Some(sid) = session_id {
        match app.lines.iter().position(|l| l.id == sid) {
            Some(i) => {
                let text = app.lines[i].text.clone();
                if let Some(s) = app.session.as_mut() {
                    s.line = i;
                    let cur = s.input.cur.min(text.len());
                    let cur = (0..=cur)
                        .rev()
                        .find(|&b| text.is_char_boundary(b))
                        .unwrap_or(0);
                    s.input = Input {
                        buf: text.clone(),
                        cur,
                    };
                    s.orig = text;
                    s.want_col = None;
                }
                app.cursor = i;
            }
            None => {
                app.session = None;
                app.ime_guard = None;
            }
        }
    }
}

/// Reflect a polled page: WEB EDITS APPEAR ON SCREEN within seconds.
/// Applied only when nothing local is at stake — the page matches, no
/// commit is in flight, no line is dirty, no composer is open — so the
/// local-authoritative model is never overwritten mid-thought. Remote
/// lines keep their per-line `updated`, so the telomere shows the new
/// lines as unread, exactly like a browser revisit.
pub(crate) fn apply_remote(app: &mut App, ctx: &Ctx, polled: PolledPage) {
    if polled.project != app.project || polled.title != app.title {
        return;
    }
    if page_is_uncreated(app) {
        // Until the create lands, every poll is the same empty template.
        // Installing it would wipe the page being typed.
        adopt_created_page(app, ctx, &polled.page);
        return;
    }
    // The snapshot was taken before something newer landed — a commit of
    // ours, a websocket commit, a page install. Installing it now would
    // undo that on screen. It is not an error; the next poll is seconds
    // away and will carry the newer state.
    if polled.epoch != app.server_epoch_now() {
        return;
    }
    if !remote_gate_clear(app) {
        return;
    }
    let same = polled.page.lines.len() == app.lines.len()
        && polled
            .page
            .lines
            .iter()
            .zip(&app.lines)
            .all(|(a, b)| a.id == b.id && a.text == b.text);
    if same {
        // Identical — nothing to install. But this IS a fresh, authoritative
        // full snapshot (the epoch guard above proved it is not stale), and
        // it agrees with the screen line for line. So if a failed commit had
        // left us believing the page had drifted, that belief is now
        // demonstrably wrong: an undo, a retry or a web-side revert brought
        // the two back together. Clearing it here is what lets diagrams
        // render again — without it the flag is sticky for the session.
        //
        // Only the flag is touched: no lines, no cursor, no undo lineage.
        if app.web_unsynced {
            app.mark_synced();
        }
        return;
    }
    install_remote_lines(
        app,
        ctx,
        &polled.page,
        Some(&t!("⟳ web側の編集を反映", "⟳ applying a web edit")),
    );
}

// -------------------------------------------------------------------------
// Websocket push sync (NOTE-websocket-sync.md)
// -------------------------------------------------------------------------

/// One event from the ws thread → the event loop.
pub(crate) fn handle_ws_event(app: &mut App, ctx: &Ctx, ev: WsEvent) {
    match ev {
        // A state for the room the reader has already left says nothing
        // about the one they are on — and a late `Live` from the old room
        // would hold the NEW page on the 60 s poll.
        WsEvent::State {
            project,
            title,
            state,
        } => {
            if project == app.project && title == app.title {
                app.set_sync_state(state);
            }
        }
        WsEvent::Status(s) => {
            // Connection notes are the quiet level: the typed sync state
            // in the footer is the durable word on it.
            app.note(s);
        }
        WsEvent::Resynced(res) => ws_on_resync(app, ctx, res),
        WsEvent::Commit(c) => ws_on_commit(app, ctx, c),
    }
}

/// The ws thread (re)joined the room, served a resync request, or hit its
/// periodic catch-up: install the fetched page (fills any commits missed
/// while away), resume the commit chain at `head`, and — when gated — hold
/// the LATEST page for the moment the gate drops instead of dropping it.
pub(crate) fn ws_on_resync(app: &mut App, ctx: &Ctx, res: ws::ResyncPage) {
    if res.page.id != app.page_id {
        return; // stale room
    }
    if resync_is_stale(app, &res) {
        return;
    }
    if !remote_gate_clear(app) {
        // Buffered commits so far precede this full page (their effects are
        // inside it); commits received after it are in the channel still.
        app.ws_held_resync = Some((res, app.ws_pending.len()));
        return;
    }
    app.ws_pending.clear();
    app.ws_head = res.head;
    let note = resync_note(app, &res.page);
    install_remote_lines(app, ctx, &res.page, note.as_deref());
}

/// resync の status 文言 — ただし本文が実際に変わったときだけ。定期の
/// 保険同期(60秒ごと)は何も変わっていなくてもページを差し替えるので、
/// 毎分「全同期」と言い続けると異常のサインに見えてしまう。
fn resync_note(app: &App, page: &cosense::api::Page) -> Option<String> {
    let changed = page.lines.len() != app.lines.len()
        || page
            .lines
            .iter()
            .zip(&app.lines)
            .any(|(a, b)| a.id != b.id || a.text != b.text);
    changed.then(|| t!("⟳ websocket 全同期", "⟳ full websocket resync"))
}

/// A held resync (arrived while a gate was up) applies now that the gate is
/// down. Buffered commits that PRECEDED it are superseded by the page and
/// dropped; commits after it stay buffered and apply against the new state.
pub(crate) fn ws_apply_held_resync(app: &mut App, ctx: &Ctx) {
    let Some((res, pre)) = app.ws_held_resync.take() else {
        return;
    };
    if !remote_gate_clear(app) {
        app.ws_held_resync = Some((res, pre)); // still gated — keep holding
        return;
    }
    // Waiting for the gate is exactly when the page moves on underneath.
    if resync_is_stale(app, &res) {
        return;
    }
    for _ in 0..pre.min(app.ws_pending.len()) {
        app.ws_pending.pop_front();
    }
    app.ws_head = res.head;
    let note = resync_note(app, &res.page);
    install_remote_lines(app, ctx, &res.page, note.as_deref());
}

/// A remote commit arrived — from ANY user, including ourselves. Commit
/// events are applied through the same gate as polling: our own echoes are
/// just idempotent no-ops (the local model already has them), and a commit
/// from the same account edited in the browser applies like any other.
pub(crate) fn ws_on_commit(app: &mut App, ctx: &Ctx, c: RemoteCommit) {
    if c.page_id != app.page_id {
        return; // stale room (navigation is one step ahead of the events)
    }
    if !remote_gate_clear(app) {
        app.ws_pending.push_back(c);
        return;
    }
    ws_flush_pending(app, ctx);
    ws_apply_one(app, ctx, c);
}

/// Apply buffered commits now that the gates are clear, oldest first.
pub(crate) fn ws_flush_pending(app: &mut App, ctx: &Ctx) {
    while !app.ws_pending.is_empty() {
        let next = app.ws_pending.pop_front().expect("non-empty");
        if !ws_apply_one(app, ctx, next) {
            break; // a gate came up mid-flush — the rest stay buffered
        }
    }
}

/// Apply ONE remote commit. Returns false if it had to be re-buffered (a
/// gate is up); true if it was applied, dropped, or queued a resync.
pub(crate) fn ws_apply_one(app: &mut App, ctx: &Ctx, c: RemoteCommit) -> bool {
    if c.page_id != app.page_id {
        return true; // stale room
    }
    if !remote_gate_clear(app) {
        app.ws_pending.push_front(c);
        return false;
    }
    // Our own commit, coming back to us. Its ops are already in the local
    // model — that is where they came from — and re-applying them is not
    // harmless: an echo can arrive AFTER we have edited past it, and a
    // `Replace` then puts the older text back. (Type, press Enter to
    // split, and let the first commit's echo land afterwards: the line
    // grows its old tail back and the caret ends up a line below the
    // text. Reported from the IME, where confirming and splitting happen
    // a keystroke apart.) So it only moves the head along.
    if let Some(i) = app.own_commits.iter().position(|id| *id == c.commit_id) {
        app.own_commits.remove(i);
        app.ws_head = Some(c.commit_id);
        return true;
    }
    if app.ws_head.as_deref() == Some(c.parent_id.as_str()) {
        // Meta-only commit (linesCount / charsCount / title …): nothing to
        // apply, but the chain moves — following it HERE is what keeps the
        // next text commit contiguous. Dropping these used to force a
        // full-page resync per meta commit.
        if c.ops.is_empty() {
            app.ws_head = Some(c.commit_id);
            return true;
        }
        // Contiguous: this commit extends the state we are known to be at.
        // Apply its ops as a diff (idempotent: our own echo changes
        // nothing) and mark the new head.
        let cursor_id = app.lines.get(app.cursor).map(|l| l.id.clone());
        let session_id = app
            .session
            .as_ref()
            .and_then(|s| app.lines.get(s.line))
            .map(|l| l.id.clone());
        cosense::ws::apply_remote_ops(&mut app.lines, &c.ops, &c.user_id);
        // The screen now holds a state newer than any poll already out.
        app.bump_server_epoch();
        app.ws_head = Some(c.commit_id.clone());
        rerender(app, ctx);
        reanchor_cursor_session(app, cursor_id, session_id);
        app.follow = true;
        app.note(t!(
            "⟳ websocket で更新を反映",
            "⟳ applying a websocket update"
        ));
        return true;
    }
    // Chain broke (reconnect gap, join replay): the event is NOT applied
    // and NOT silently dropped — a background full-page resync is
    // requested, and the fetched page will carry this commit's effect.
    app.ws_resync_pending = true;
    app.note(t!(
        "⟳ websocket 差分に欠落 — 再同期します",
        "⟳ a websocket diff was missing — resyncing"
    ));
    true
}

/// Was this full page fetched before something newer landed here?
///
/// A resync replaces the local lines wholesale, so a page fetched before
/// our last commit would delete the line we are typing on — and deleting
/// the session's line closes the session, dropping the reader out of EDIT
/// mid-word. (Reported as \"pressing Enter twice quickly throws me back to
/// view mode\".) Polls have carried this guard from the start; the
/// websocket's resync had not.
///
/// A stale page is not an error and not a loss: another resync is asked
/// for at once, and the ws thread's own backoff keeps that from becoming
/// a storm while someone is typing.
pub(crate) fn resync_is_stale(app: &mut App, res: &ws::ResyncPage) -> bool {
    if res.epoch == app.server_epoch_now() {
        return false;
    }
    app.ws_resync_pending = true;
    true
}

/// Ship one outstanding resync request to the ws thread (which is the only
/// side that talks to the network). The flag is one-shot per frame; the
/// result arrives back as a `WsEvent::Resynced`.
pub(crate) fn ws_send_resync_request(app: &mut App) {
    if app.ws_resync_pending {
        app.ws_resync_pending = false;
        let _ = app.ws_req_tx.send(ws::WsRequest::Resync);
    }
}

/// 409 NotFastForward: someone else moved the page. Invalidate the queue,
/// reload the server truth, and re-anchor the session by line id — the
/// caret text is NEVER lost: if its line is gone, it becomes a fresh line
/// at the end and the session continues there.
pub(crate) fn recover_conflict(app: &mut App, ctx: &Ctx) {
    recover_by_reload(app, ctx, Rebase::Conflict);
}

/// Why the page is being re-based on the server's copy: the wording of
/// the toast is the only difference.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rebase {
    /// 409: someone else moved the page under our edit.
    Conflict,
    /// A structural save failed, so the local lines it made are not on
    /// the server and the queue behind it cannot be trusted.
    FailedSave,
}

pub(crate) fn recover_by_reload(app: &mut App, ctx: &Ctx, why: Rebase) {
    app.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // 409 means the server moved and our ops did not land: local and server
    // disagree from this moment. Marked BEFORE the reload, because if the
    // reload fails we are still divergent and must not render — a browser
    // would screenshot the server's text and file it under ours.
    app.mark_desynced();
    let stash = app.session.as_ref().map(|s| {
        (
            app.lines
                .get(s.line)
                .map(|l| l.id.clone())
                .unwrap_or_default(),
            s.input.buf.clone(),
            s.input.cur,
        )
    });
    if !reload_page(app, ctx) {
        // `reload_page` already said why on the status line; saying anything
        // else here would bury it. The reader keeps their text, the flag
        // stays set, and the next successful install clears it.
        return;
    }
    // set_page cleared session + undo lineage and marked us synced again.
    let changed = match why {
        Rebase::Conflict => t!(
            "他の人がページを更新しました",
            "page changed by someone else"
        ),
        Rebase::FailedSave => t!("保存に失敗しました", "a save failed"),
    };
    match stash {
        None => {
            app.toast(t!("{changed} — 読み直しました", "{changed} — reloaded"));
        }
        Some((id, buf, caret)) => {
            if let Some(idx) = app.lines.iter().position(|l| l.id == id) {
                enter_session(app, ctx, idx, 0);
                if let Some(s) = app.session.as_mut() {
                    s.input = Input {
                        buf: buf.clone(),
                        cur: caret.min(buf.len()),
                    };
                }
                app.toast(t!(
                    "{changed} — 読み直し、編集中の行はそのままです",
                    "{changed} — reloaded, your line kept"
                ));
            } else if !buf.trim().is_empty() {
                // The line is gone: rescue the text as a fresh last line.
                let new_id = new_line_id();
                let ops = vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![(new_id.clone(), buf.clone())],
                }];
                do_edit(app, ctx, &t!("退避", "rescue"), ops);
                if let Some(idx) = app.lines.iter().position(|l| l.id == new_id) {
                    enter_session(app, ctx, idx, buf.len());
                }
                app.toast(match why {
                    Rebase::Conflict => t!(
                        "編集中の行が他の人に削除されました — 内容はページ末尾に退避しました",
                        "your line was deleted by someone else — text rescued at the end"
                    ),
                    Rebase::FailedSave => t!(
                        "編集中の行はサーバーに届いていませんでした — 内容はページ末尾に退避しました",
                        "your line never reached the server — text rescued at the end"
                    ),
                });
            } else {
                app.toast(t!("{changed} — 読み直しました", "{changed} — reloaded"));
            }
        }
    }
}

/// ←/→: travel the page's snapshot history (dir = -1 older, +1 newer).
/// First ← enters the machine at the newest snapshot; → past the newest
/// exits back to NOW (live page, refetched). The timeline is fetched once
/// per visit and snapshots are cached, so scrubbing is instant.
pub(crate) fn travel(app: &mut App, ctx: &Ctx, dir: i32) {
    // Timeline navigation must never run underneath the modeless editor.
    // Keeping the session alive while swapping in a snapshot makes an undo
    // look like it entered page history and can leave the next commit
    // targeting the wrong revision.
    if app.session.is_some() {
        return;
    }
    if app.time.is_none() {
        if dir > 0 {
            app.toast(t!("すでに最新です", "already at NOW"));
            return;
        }
        // The list fetched with the page is used as it stands; only when
        // it is not known (yet) does ← ask the server itself.
        let listed = match app.snapshots.clone() {
            Some(points) => Ok(points),
            None => ctx.client.list_snapshots(&app.project, &app.page_id),
        };
        match listed {
            Ok(points) if !points.is_empty() => {
                let last = points.len() - 1;
                app.capture_present();
                app.time = Some(TimeMachine {
                    points,
                    pos: last,
                    cache: HashMap::new(),
                });
                show_snapshot(app, ctx, last);
            }
            Ok(_) => app.toast(t!(
                "このページに履歴はありません",
                "no snapshots for this page"
            )),
            Err(e) => app.toast_err(t!(
                "履歴一覧を取得できません: {e}",
                "snapshot list failed: {e}"
            )),
        }
        return;
    }
    let (pos, len) = {
        let tm = app.time.as_ref().unwrap();
        (tm.pos, tm.points.len())
    };
    if dir < 0 {
        if pos == 0 {
            app.toast(t!("最も古い履歴です", "oldest snapshot"));
        } else {
            show_snapshot(app, ctx, pos - 1);
        }
    } else if pos + 1 >= len {
        // Past the newest snapshot is NOW — but only if NOW can be fetched.
        // See the Esc path: a failed reload keeps the snapshot, read-only.
        if reload_page(app, ctx) {
            app.status.clear(); // the history position hint
        }
    } else {
        show_snapshot(app, ctx, pos + 1);
    }
}

/// Open the time machine at snapshot `id` (a comment's revision). The
/// timeline comes from the page's own list when known, else the server;
/// a snapshot no longer listed leaves the page as it is and says so.
pub(crate) fn show_revision(app: &mut App, ctx: &Ctx, id: &str) {
    if app
        .time
        .as_ref()
        .is_some_and(|tm| tm.points.get(tm.pos).is_some_and(|p| p.id == id))
    {
        return; // already showing it
    }
    let points = match app
        .time
        .as_ref()
        .map(|tm| tm.points.clone())
        .or_else(|| app.snapshots.clone())
    {
        Some(p) => p,
        None => match ctx.client.list_snapshots(&app.project, &app.page_id) {
            Ok(p) => p,
            Err(e) => {
                app.toast_err(t!(
                    "履歴一覧を取得できません: {e}",
                    "snapshot list failed: {e}"
                ));
                return;
            }
        },
    };
    let Some(idx) = points.iter().position(|p| p.id == id) else {
        app.toast_err(t!(
            "その版は履歴に見つかりません",
            "that revision is not in the page's history"
        ));
        return;
    };
    if app.time.is_none() {
        app.capture_present();
        app.time = Some(TimeMachine {
            points,
            pos: idx,
            cache: HashMap::new(),
        });
    }
    show_snapshot(app, ctx, idx);
}

/// Install snapshot `idx` of the time machine: swap the page body for the
/// historical lines (rendered normally — the UI always consumes complete
/// documents, akapen's history model), drop the related list (it describes
/// the PRESENT graph), and keep the cursor's line number.
pub(crate) fn show_snapshot(app: &mut App, ctx: &Ctx, idx: usize) {
    let (ts_id, created, len) = {
        let tm = app.time.as_ref().unwrap();
        (
            tm.points[idx].id.clone(),
            tm.points[idx].created,
            tm.points.len(),
        )
    };
    let cached = app.time.as_ref().unwrap().cache.get(&ts_id).cloned();
    let snap = match cached {
        Some(s) => s,
        None => match ctx.client.get_snapshot(&app.project, &app.page_id, &ts_id) {
            Ok(s) => {
                app.time
                    .as_mut()
                    .unwrap()
                    .cache
                    .insert(ts_id.clone(), s.clone());
                s
            }
            Err(e) => {
                app.toast_err(t!(
                    "履歴を取得できません: {e}",
                    "snapshot fetch failed: {e}"
                ));
                return;
            }
        },
    };
    let texts: Vec<String> = snap.lines.iter().map(|l| l.text.clone()).collect();
    // Which rows the NEXT version deletes: web's `.will-delete-next`.
    // The comparison version is the next-newer snapshot when it is already
    // cached (scrubbing always arrives from it), else NOW's captured ids
    // for the newest snapshot. A next version nobody has yet is skipped —
    // marking deletions is not worth an extra request.
    let next_ids: Option<HashSet<String>> = {
        let tm = app.time.as_ref().unwrap();
        if idx + 1 < tm.points.len() {
            tm.cache.get(&tm.points[idx + 1].id).map(|s| {
                s.lines
                    .iter()
                    .filter(|l| !l.id.is_empty())
                    .map(|l| l.id.clone())
                    .collect()
            })
        } else {
            app.present_ids.clone()
        }
    };
    app.deleted_next = match next_ids {
        Some(next) => snap
            .lines
            .iter()
            .filter(|l| !l.id.is_empty() && !next.contains(&l.id))
            .map(|l| l.id.clone())
            .collect(),
        None => HashSet::new(),
    };
    // A snapshot is the page as it was; which of its links exist is only
    // known for NOW, so an old revision says nothing about it. Links still
    // wear the project theme's colours (cached lookup, not a fetch).
    let palette = cosense::theme::tinted_page_palette(
        &ctx.palette(),
        ctx.project_theme(&app.project).as_deref(),
    );
    let rendered = render_lines_with(
        &texts,
        Some(ctx.hl().as_ref()),
        &palette,
        &LinkTruth::default(),
    );
    app.lines = snap.lines;
    app.blocks = rendered.blocks;
    app.srcs = rendered.srcs;
    app.hits = rendered.hits;
    app.related = Vec::new();
    app.virtual_items = Vec::new();
    app.images.clear();
    app.image_errors.clear();
    app.pending.clear();
    app.web_pending.clear();
    app.web_errors.clear();
    app.selection = None;
    app.laid_width = 0; // rebuild (clamps the cursor)
    app.follow = true;
    app.start_image_loads(ctx);
    app.time.as_mut().unwrap().pos = idx;
    // NOW is the last position, as on the header: three snapshots make 4.
    app.status = t!(
        "履歴 {}/{} · {}（{}前）· ← 古い · → 新しい · Esc 最新",
        "history {}/{} · {} ({} ago) · ← older · → newer · Esc NOW",
        idx + 1,
        len + 1,
        cosense::theme::format_local(created),
        relative_age(created),
    );
}

/// Refetch the current page in place, keeping the cursor's line number and
/// following it (used after edits, edit conflicts, and ws re-sync).
/// Returns whether the refetch succeeded.
pub(crate) fn reload_page(app: &mut App, ctx: &Ctx) -> bool {
    let cur = app.cursor;
    match load_page(ctx, &app.project.clone(), &app.title.clone()) {
        Ok(l) => {
            app.set_page(l, ctx);
            app.cursor = cur; // clamped on the next rebuild
            app.follow = true;
            true
        }
        Err(e) => {
            app.toast_err(t!("読み直しに失敗しました: {e}", "reload failed: {e}"));
            false
        }
    }
}

impl App {
    /// Adopt the push channel's new state: retune the poller and, when the
    /// status line is showing the session summary, refresh its `sync:` tag.
    ///
    /// The retune is what makes a stale sid cheap. The old code picked 60 s
    /// from the mere existence of a cookie; now the interval is a function
    /// of a state the websocket thread has actually demonstrated.
    pub(crate) fn set_sync_state(&mut self, st: SyncState) {
        if self.sync_state == st {
            return;
        }
        self.sync_state = st;
        // A dead channel just means the poller is gone (shutdown).
        let _ = self.poll_ctrl_tx.send(st.poll_interval());
        self.refresh_sync_tag();
    }

    /// Restart an adaptive fallback poll after local activity or navigation.
    /// A live room already carries changes and keeps its fixed 60 s insurance
    /// poll, so waking it would only add a redundant GET.
    pub(crate) fn reset_fallback_poll(&self) {
        if self.sync_state != SyncState::Live {
            let _ = self.poll_ctrl_tx.send(SyncState::Polling.poll_interval());
        }
    }

    /// The `sync:` word inside the session summary, kept truthful as the
    /// state moves. Only that summary is rewritten — a live status message
    /// (a commit result, an auth note) is left alone.
    pub(crate) fn refresh_sync_tag(&mut self) {
        // Either wording may be on screen: the status was written in the
        // reader's language, and a test may have seeded the other one.
        let Some((at, tag)) = ["同期: ", "sync: "]
            .iter()
            .find_map(|p| self.status.find(p).map(|i| (i, *p)))
        else {
            return;
        };
        let Some(rest) = self.status.get(at + tag.len()..) else {
            return;
        };
        let end = rest
            .find(' ')
            .map(|i| at + tag.len() + i)
            .unwrap_or(self.status.len());
        self.status
            .replace_range(at + tag.len()..end, self.sync_label());
    }

    /// What to call the live-update channel. A session with no sid is not
    /// "reconnecting" — it never had a push channel to lose.
    pub(crate) fn sync_label(&self) -> &'static str {
        if self.ws_attempted {
            self.sync_state.label()
        } else {
            SyncState::Polling.label()
        }
    }

    /// What the bottom row should say, in priority order: the edit
    /// session's keys, then the links on the cursor line, then whatever
    /// wrote to `status` (commits, auth, resync — the things a reader must
    /// not miss), then a diagram note, and finally the key hints.
    /// Why the page might not be showing the newest state, in a few
    /// characters — or nothing at all when it is.
    ///
    /// Live sync with nothing held is the quiet, normal case. Everything
    /// else is worth a word: `poll` means web-side edits take up to three
    /// seconds, and `held` means they have ARRIVED and are waiting for the
    /// caret line to be committed (remote state is never installed over an
    /// unsaved line). Without this, both look like "the viewer got slow".
    pub(crate) fn sync_notice(&self) -> Option<String> {
        if !self.ws_pending.is_empty() {
            let dirty = self
                .session
                .as_ref()
                .map(|s| s.input.buf != s.orig)
                .unwrap_or(false);
            let why = if self.move_mode.is_some() {
                // A drag holds the whole page's order, so remote edits wait
                // for it exactly as they wait for an unsaved line.
                ts!(
                    "つかんでいるブロックを待っています",
                    "waiting on the grabbed block"
                )
            } else if dirty {
                ts!(
                    "編集中の行を待っています",
                    "waiting on the line being edited"
                )
            } else if self.inflight > 0 {
                // Typing saves as it goes, so while the keys keep coming
                // there is always a save on its way; remote edits queue
                // behind it and land the moment the queue drains.
                ts!("保存を待っています", "waiting on the save")
            } else {
                ts!("適用待ち", "waiting to apply")
            };
            return Some(t!(
                "⟳ {} 件{}",
                "⟳ {} update(s) — {}",
                self.ws_pending.len(),
                why
            ));
        }
        match self.sync_state {
            capability::SyncState::Live => None,
            capability::SyncState::Polling => Some(t!("同期: poll", "sync: poll")),
            capability::SyncState::Reconnecting => Some(t!("同期: 再接続中", "sync: reconnecting")),
        }
    }

    /// The local model may have drifted from the server. Deliberately
    /// sticky: a later commit succeeding says nothing about the edit that
    /// did not, so only an authoritative page install clears it.
    pub(crate) fn mark_desynced(&mut self) {
        self.web_unsynced = true;
    }

    /// A whole page arrived from the server and replaced the local lines,
    /// so whatever had drifted is gone. This is the ONLY way the flag
    /// clears; it must never be called on a partial or local-only update,
    /// which would re-open the window it exists to close.
    pub(crate) fn mark_synced(&mut self) {
        self.web_unsynced = false;
    }

    /// The page source has changed: any render queued before this moment
    /// would be filed under a hash it no longer matches.
    pub(crate) fn bump_src_epoch(&self) {
        self.src_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn src_epoch_now(&self) -> u64 {
        self.src_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The server's state has moved on: any poll response fetched before
    /// this moment is stale.
    pub(crate) fn bump_server_epoch(&self) {
        self.server_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn server_epoch_now(&self) -> u64 {
        self.server_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn gen_now(&self) -> u64 {
        self.web_gen.load(std::sync::atomic::Ordering::SeqCst)
    }
}
