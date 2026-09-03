use crate::*;
use super::support::*;

    #[test]
    fn a_failed_reload_at_the_newest_snapshot_also_stays_put() {
        let ctx = offline_ctx();
        let mut app = mermaid_page();
        app.render_policy = capability::RenderPolicy::Auto;
        in_history(&mut app);
        app.rebuild(80);
        // Right from the newest snapshot is the other way out of history.
        travel(&mut app, &ctx, 1);
        assert!(app.time.is_some());
        assert_ne!(app.toast_text(), "最新");
        assert!(app.web_jobs_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn a_poll_that_started_before_a_websocket_commit_never_rolls_it_back() {
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        // A GET goes out now, seeing "a"/"b".
        let in_flight =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        // While it is out, a websocket commit lands and the screen moves on.
        app.lines[1].text = "b2".into();
        app.bump_server_epoch();
        // Now the old response arrives.
        apply_remote(&mut app, &ctx, in_flight);
        assert_eq!(
            app.lines[1].text, "b2",
            "a snapshot older than the applied commit must not be installed"
        );
    }

    #[test]
    fn a_poll_that_started_before_a_local_commit_never_rolls_it_back() {
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        let in_flight =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        // The reader's own edit commits while the GET is out.
        app.lines[1].text = "mine".into();
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Done {
                job: UNRELATED_JOB,
                label: "line 2".into(),
                title: String::new(),
                commit_id: String::new(),
            },
        );
        apply_remote(&mut app, &ctx, in_flight);
        assert_eq!(app.lines[1].text, "mine", "the server has our commit; the snapshot predates it");
    }

    #[test]
    fn an_undo_that_restores_agreement_lets_diagrams_render_again() {
        // A failed commit leaves local and server divergent, and diagrams
        // stop rendering so the browser cannot screenshot the server's old
        // text and file it under the local source. Undoing the edit makes
        // the two agree again — but nothing was telling the app so, and the
        // flag stayed set for the rest of the session.
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        app.lines[1].text = "typo".into();
        app.inflight = 1;
        handle_commit_outcome(
            &mut app,
            &ctx,
            CommitOutcome::Failed { job: UNRELATED_JOB, label: "line 2".into(), msg: "500".into() },
        );
        assert!(app.web_unsynced, "the page is known to have drifted");

        // The reader undoes it: the screen is back to what the server has.
        app.lines[1].text = "b".into();
        // A fresh poll now agrees with the screen, line for line.
        let agreeing =
            polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        apply_remote(&mut app, &ctx, agreeing);
        assert!(
            !app.web_unsynced,
            "an authoritative snapshot that MATCHES proves the drift is over"
        );
        assert_eq!(app.lines[1].text, "b", "and it changed nothing else");
        assert_eq!(app.cursor, 0);

        // ...so the next frame can render again.
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
    }

    #[test]
    fn a_stale_equal_poll_does_not_clear_the_drift() {
        // Agreement only counts when the snapshot is CURRENT. An old
        // response that happens to match says nothing about now.
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        app.web_unsynced = true;
        let stale = polled_at(&[("id0", "a"), ("id1", "b")], app.server_epoch_now());
        app.bump_server_epoch();
        apply_remote(&mut app, &ctx, stale);
        assert!(app.web_unsynced, "a photograph of the past proves nothing");
    }

    #[test]
    fn a_fresh_poll_still_applies_web_edits() {
        // The guard must not turn into "polling stopped working".
        let ctx = test_ctx();
        let mut app = page(&["a", "b"]);
        app.page_id = "pid".into();
        let fresh =
            polled_at(&[("id0", "a"), ("id1", "web edit")], app.server_epoch_now());
        apply_remote(&mut app, &ctx, fresh);
        assert_eq!(app.lines[1].text, "web edit");
    }

    #[test]
    fn a_live_from_the_room_the_reader_left_is_ignored() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "other".into();
        app.ws_attempted = true;
        // Still in the channel from before the navigation.
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "t".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(
            app.sync_state,
            SyncState::Polling,
            "a Live for the old page must not hold the new one on the 60 s poll"
        );
        // The new room's own Live is honoured.
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "other".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(app.sync_state, SyncState::Live);
    }

    #[test]
    fn a_rejoin_that_stalls_leaves_the_reader_on_the_fast_poll() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "other".into();
        app.ws_attempted = true;
        app.status = "認証: sid · 編集可 · 同期: ws · ? ヘルプ".into();
        app.sync_state = SyncState::Live;
        // The rejoin fails, repeatedly. Every failure edge says so.
        for _ in 0..3 {
            handle_ws_event(
                &mut app,
                &ctx,
                WsEvent::State {
                    project: "proj".into(),
                    title: "other".into(),
                    state: SyncState::Reconnecting,
                },
            );
        }
        assert_eq!(app.sync_state.poll_interval(), Duration::from_secs(3));
        assert!(app.status.contains("同期: 再接続中"));
    }

    /// The bug that made a new page look saved and arrive empty: Cosense
    /// hands out an id for a page it has NOT made, and committing against
    /// that id writes into nothing. Only `persistent` says a page is real.
    #[test]
    fn a_provisional_id_is_not_a_page_id() {
        let mut page = polled(&[("prov0", "a title")]).page;
        page.id = "6a92b6a60000000000000c05".into(); // the API really does send one
        page.persistent = false;
        assert_eq!(live_page_id(&page), "", "nothing to edit against yet");

        page.persistent = true;
        assert_eq!(live_page_id(&page), "6a92b6a60000000000000c05");

        // And a non-persistent page is never adopted, however real its id
        // looks: adopting it would resume committing into nothing.
        let ctx = test_ctx();
        let mut app = page_uncreated(&["a title", "typed"]);
        let mut ghost = polled(&[("prov0", "a title")]).page;
        ghost.id = "6a92b6a60000000000000c05".into();
        ghost.persistent = false;
        adopt_created_page(&mut app, &ctx, &ghost);
        assert_eq!(app.page_id, "", "still uncreated");
        assert_eq!(app.lines[1].text, "typed", "and the local text is untouched");
    }

    /// Once the page exists, the viewer adopts its id and commits whatever
    /// was typed after the create left — those keystrokes were never in it.
    #[test]
    fn adopting_a_created_page_commits_what_the_create_missed() {
        let ctx = test_ctx();
        let mut app = page(&["new title", "body"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);
        // Typed while the create was in flight.
        app.lines[1].text = "body, and more".into();

        let mut server = polled(&[("id0", "new title"), ("id1", "body")]).page;
        server.id = "PID".into();
        adopt_created_page(&mut app, &ctx, &server);

        assert_eq!(app.page_id, "PID", "later commits have something to name");
        assert_eq!(app.lines[1].text, "body, and more", "the local text wins");
        let jobs = drain_jobs(&mut app);
        assert_eq!(jobs.len(), 1);
        assert!(matches!(&jobs[0].1[0], EditOp::Replace { id, .. } if id == "id1"));
    }

    /// While the page does not exist, every poll returns the same empty
    /// template. Installing it would wipe the page being typed.
    #[test]
    fn polls_never_overwrite_a_page_being_typed_into_existence() {
        let ctx = test_ctx();
        let mut app = page(&["new title", "body"]);
        app.title = "new title".into();
        app.page_id = String::new();
        app.rebuild(40);

        let mut template = polled(&[("", "new title")]);
        template.page.id = String::new();
        template.page.persistent = false;
        apply_remote(&mut app, &ctx, template);
        assert_eq!(app.lines.len(), 2, "the template did not replace anything");
        assert_eq!(app.lines[1].text, "body");
    }

    #[test]
    fn remote_edits_apply_when_idle_and_keep_the_cursor_line() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.cursor = 2; // on "two" (id2)
        // remote inserted a line above and edited "one"
        apply_remote(
            &mut app,
            &ctx,
            polled(&[("id0", "t"), ("idN", "new!"), ("id1", "ONE"), ("id2", "two")]),
        );
        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["t", "new!", "ONE", "two"]);
        assert_eq!(app.cursor, 3, "cursor followed its line id, not its number");
        assert!(app.note_text().contains("web"));
    }

    #[test]
    fn remote_apply_waits_for_inflight_commits() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.inflight = 1;
        apply_remote(&mut app, &ctx, polled(&[("id0", "t"), ("id1", "REMOTE")]));
        assert_eq!(app.lines[1].text, "one", "own commits must land first");
    }

    // ---- websocket push (NOTE-websocket-sync.md) ----
    /// meta-only コミット(linesCount / charsCount / タイトル変更など)は
    /// 適用するものが無いが、チェーンの頭は進める。これを落とすと、次の
    /// 本文コミットが必ず「親不一致=チェーン切れ」になり、meta コミット
    /// 1つごとに全ページ再取得(full resync)を払わされる。
    #[test]
    fn ws_meta_only_commit_advances_the_head_without_a_resync() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        app.status = "quiet".into();

        // meta コミット: ops は空。頭だけ進み、resync は要求されず、
        // status も騒がない。
        ws_on_commit(&mut app, &ctx, commit("c1", "c0", &pid, "other", vec![]));
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(!app.ws_resync_pending, "no resync for a meta commit");
        assert_eq!(app.status, "quiet", "and nothing to announce");
        assert!(app.toast_text().is_empty(), "not even a toast: {}", app.toast_text());

        // その直後の本文コミットは contiguous のまま差分適用される。
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "c1",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "ONE!".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "ONE!", "the text commit still rides the chain");
        assert_eq!(app.ws_head.as_deref(), Some("c2"));
        assert!(!app.ws_resync_pending);
    }

    /// 定期の保険 resync(60秒ごと)は、何も変わっていなければ黙って
    /// 差し替える。毎分「⟳ websocket 全同期」と言い続けると異常のサイン
    /// に見えてしまう。変わっていれば従来どおり告げる。
    #[test]
    fn a_resync_that_changes_nothing_says_nothing() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.status = "quiet".into();
        let pid = app.page_id.clone();

        // 同一内容のページ(id・text とも現状と一致)。
        let mut same = polled(&[("id0", "t"), ("id1", "one")]).page;
        same.id = pid.clone();
        ws_on_resync(&mut app, &ctx, resync(same, Some("h1")));
        assert_eq!(app.ws_head.as_deref(), Some("h1"), "the head still resumes");
        assert_eq!(app.status, "quiet", "an unchanged install is silent");
        assert!(app.toast_text().is_empty(), "not even a toast: {}", app.toast_text());

        // 内容が変わっていれば従来どおり告げる(epoch は現在値を運ぶ)。
        let mut differs = polled(&[("id0", "t"), ("id1", "CHANGED")]).page;
        differs.id = pid;
        let now = app.server_epoch_now();
        ws_on_resync(&mut app, &ctx, resync_at(differs, Some("h2"), now));
        assert_eq!(app.lines[1].text, "CHANGED");
        assert!(app.note_text().contains("全同期"), "note: {}", app.note_text());
    }

    #[test]
    fn ws_contiguous_commit_applies_a_diff_and_advances_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "ONE!".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "ONE!", "diff applied in place");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.note_text().contains("websocket"));
    }

    #[test]
    fn ws_same_user_browser_commit_applies() {
        // A commit from the SAME account is the primary use case (edit in
        // the browser, watch the TUI follow) — it must apply, not be
        // dropped as a "self-echo" (idempotency protects against real
        // echoes instead). There is no me_id concept anymore.
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "me", // same user that runs the TUI
                vec![EditOp::Replace { id: "id1".into(), text: "SELF".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "SELF", "same-user browser edit reflects");
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert_eq!(app.lines[1].user_id, "me", "the committer is stamped for blame");
    }

    #[test]
    fn ws_own_echo_does_not_double_insert() {
        // OUR commit came back as an event: applying it again must not
        // duplicate the inserted line (insert-by-id skip).
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        let echo = commit(
            "c1",
            "c0",
            &pid,
            "me",
            vec![EditOp::Insert {
                anchor: "_end".into(),
                lines: vec![("n1".into(), "new line".into())],
            }],
        );
        ws_on_commit(&mut app, &ctx, echo.clone());
        assert_eq!(app.lines.len(), 3);
        // the echo reapplies (it is just an idempotent no-op now)
        ws_on_commit(&mut app, &ctx, echo);
        assert_eq!(app.lines.len(), 3, "no duplicate from the echo");
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "one", "new line"]);
    }

    /// The reported bug: confirm an IME composition (Enter), then press
    /// Enter again straight away. The caret ends up a line lower, or the
    /// session drops out to view mode, and the status says the websocket
    /// applied an update.
    ///
    /// The sequence is: the text commit goes out, the split commit goes
    /// out, both are acknowledged — and only THEN does the websocket echo
    /// of the first one arrive. It carries the line as it was before the
    /// split, which is older than what is on screen.
    #[test]
    fn a_late_echo_of_our_own_commit_does_not_undo_the_split_after_it() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();

        // Type into the line (as an IME commit does), then Enter: the
        // dirty text commits, and the line splits at the caret.
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "日本語");
        handle_session_key(&mut app, &ctx, key(KeyCode::Left));
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        let after_split: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        let caret_line = app.session.as_ref().map(|s| s.line).unwrap();
        let caret_id = app.lines[caret_line].id.clone();
        assert_eq!(after_split, vec!["t", "one日本", "語"]);

        // Both commits are acknowledged: nothing is in flight, so the
        // gates are down.
        while app.inflight > 0 {
            handle_commit_outcome(
                &mut app,
                &ctx,
                CommitOutcome::Done {
                    job: UNRELATED_JOB,
                    label: "line".into(),
                    title: String::new(),
                    commit_id: "c1".into(),
                },
            );
        }

        // Now the echo of the FIRST commit turns up.
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "me",
                vec![EditOp::Replace { id: "id1".into(), text: "one日本語".into() }],
            ),
        );
        let now: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(now, after_split, "our own echo must not roll the page back");
        assert!(app.session.is_some(), "and must not drop the reader out of EDIT");
        assert_eq!(
            app.session.as_ref().map(|s| app.lines[s.line].id.clone()),
            Some(caret_id),
            "the caret stays on the line it was typing"
        );
    }

    #[test]
    fn ws_commits_buffer_while_gated_and_flush_in_order() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one", "two"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        app.inflight = 1; // our own commit is landing — gate up
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "FIRST".into() }],
            ),
        );
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "c1",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("n1".into(), "tail".into())],
                }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "nothing applied while gated");
        assert_eq!(app.ws_pending.len(), 2);
        // the commit lands: gate drops, buffered events apply oldest first
        app.inflight = 0;
        ws_flush_pending(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "FIRST", "two", "tail"]);
        assert_eq!(app.ws_head.as_deref(), Some("c2"));
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_dirty_caret_line_buffers_the_commit() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        enter_session(&mut app, &ctx, 1, 3);
        type_str(&mut app, &ctx, "!"); // dirty caret line — gate up
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "REMOTE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "buffered, not applied");
        assert_eq!(app.ws_pending.len(), 1);
        // leaving the line commits it and drops the gate; flush applies
        handle_session_key(&mut app, &ctx, key(KeyCode::Esc));
        let _ = drain_jobs(&mut app);
        app.inflight = 0; // the queued commit is "done" in this test harness
        ws_flush_pending(&mut app, &ctx);
        assert_eq!(app.lines[1].text, "REMOTE");
    }

    #[test]
    fn ws_different_page_events_are_dropped() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                "some-other-page",
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NOPE".into() }],
            ),
        );
        assert_eq!(app.lines[1].text, "one", "stale-room events go nowhere");
        assert!(app.ws_pending.is_empty());
    }

    /// The other half of the report: pressing Enter twice quickly could
    /// throw the reader out of EDIT into view mode.
    ///
    /// A resync fetch that STARTED before our commit landed comes back
    /// without the line we just made. Installing it deletes that line, and
    /// a session whose line has vanished closes — mid-word. The page is a
    /// photograph of the past, so it is refused and another one asked for,
    /// which is what polls have always done.
    #[test]
    fn a_resync_fetched_before_our_own_commit_is_refused() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();

        // The ws thread starts a resync fetch: it stamps the epoch it saw.
        let fetched_at = app.server_epoch_now();

        // Meanwhile we split the line and the commit lands, which moves the
        // server state on (and the epoch with it).
        enter_session(&mut app, &ctx, 1, 3);
        handle_session_key(&mut app, &ctx, key(KeyCode::Enter));
        while app.inflight > 0 {
            handle_commit_outcome(
                &mut app,
                &ctx,
                CommitOutcome::Done {
                    job: UNRELATED_JOB,
                    label: "line".into(),
                    title: String::new(),
                    commit_id: "c1".into(),
                },
            );
        }
        let lines_now = app.lines.len();
        let caret_id = app.session.as_ref().map(|s| app.lines[s.line].id.clone());
        assert!(caret_id.is_some());

        // Now the stale page arrives: it predates the split.
        let mut p = polled(&[("id0", "t"), ("id1", "one")]).page;
        p.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync_at(p, Some("c1"), fetched_at)));

        assert_eq!(app.lines.len(), lines_now, "the new line survives");
        assert!(app.session.is_some(), "and the reader stays in EDIT");
        assert_eq!(
            app.session.as_ref().map(|s| app.lines[s.line].id.clone()),
            caret_id,
            "on the same line"
        );
        assert!(app.ws_resync_pending, "a fresh page is asked for instead");
    }

    #[test]
    fn ws_resync_installs_the_page_and_resumes_head() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c9".into());
        let pid = app.page_id.clone();
        let mut p = polled(&[("id0", "t"), ("idX", "fresh!"), ("id1", "one")]).page;
        p.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c12"))));
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "fresh!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c12"), "chain resumes at head");
        // …and the next commit with that parent applies as a contiguous diff
        let pid2 = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c13",
                "c12",
                &pid2,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "NEXT".into() }],
            ),
        );
        // "id1" is at index 2 now (the resync inserted "idX" above it)
        assert_eq!(app.lines[2].text, "NEXT", "the replace landed on id1");
        assert_eq!(app.ws_head.as_deref(), Some("c13"));
    }

    #[test]
    fn ws_resync_while_gated_is_held_and_applied_afterwards() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        app.inflight = 1; // gate up
        // a commit that must be superseded by the resync
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c1",
                "c0",
                &pid,
                "other",
                vec![EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("o1".into(), "old".into())],
                }],
            ),
        );
        let mut p = polled(&[("id0", "t"), ("idN", "fresh!"), ("id1", "one")]).page;
        p.id = pid.clone();
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p, Some("c1"))));
        // still gated: the resync is HELD, the commit is buffered before it
        assert!(app.ws_held_resync.is_some(), "the latest resync is kept");
        assert_eq!(app.lines[1].text, "one");
        // a second resync while gated REPLACES the held one (latest wins)
        let mut p2 = polled(&[("id0", "t"), ("idN2", "newer!"), ("id1", "one")]).page;
        p2.id = pid;
        handle_ws_event(&mut app, &ctx, WsEvent::Resynced(resync(p2, Some("c1"))));
        // gate drops: latest held resync applies, the pre-resync commit is
        // superseded (its effect is inside the fetched page)
        app.inflight = 0;
        ws_apply_held_resync(&mut app, &ctx);
        let texts: Vec<String> = app.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["t", "newer!", "one"]);
        assert_eq!(app.ws_head.as_deref(), Some("c1"));
        assert!(app.ws_held_resync.is_none());
        assert!(app.ws_pending.is_empty());
    }

    #[test]
    fn ws_chain_break_requests_a_background_resync() {
        let ctx = test_ctx();
        let mut app = page(&["t", "one"]);
        app.rebuild(40);
        app.ws_head = Some("c0".into());
        let pid = app.page_id.clone();
        ws_on_commit(
            &mut app,
            &ctx,
            commit(
                "c2",
                "cNOPE",
                &pid,
                "other",
                vec![EditOp::Replace { id: "id1".into(), text: "??".into() }],
            ),
        );
        // the stray commit is not applied and NOT silently dropped: a
        // background resync is requested (no network on the UI thread)
        assert_eq!(app.lines[1].text, "one");
        assert!(app.ws_resync_pending, "gap event requests a resync");
        ws_send_resync_request(&mut app);
        let req_rx = app.ws_req_rx.as_ref().unwrap();
        assert_eq!(req_rx.try_recv(), Ok(ws::WsRequest::Resync));
        assert!(!app.ws_resync_pending, "one request per gap flag");
    }
