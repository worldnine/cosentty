use crate::tests::support::*;
use crate::*;

// ---------------------------------------------------------------
// Live-update plan: the poller's interval follows a TYPED push state,
// and a fetch that fails must not be treated as one that succeeded.
// ---------------------------------------------------------------
#[test]
fn a_failed_reload_keeps_the_reader_in_history() {
    let ctx = offline_ctx();
    let mut app = mermaid_page();
    in_history(&mut app);
    app.rebuild(80);
    // Esc asks for NOW. The fetch fails, so there is no NOW to show.
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(app.time.is_some(), "the snapshot stays on screen");
    assert_ne!(
        app.toast_text(),
        "最新",
        "and it is not labelled as the live page"
    );
    assert!(
        app.toast_text().contains("読み直しに失敗"),
        "the reason is shown: {}",
        app.toast_text()
    );
}

#[test]
fn a_stale_sid_polls_fast_from_the_first_second() {
    let mut app = page(&["a"]);
    app.ws_attempted = true; // a sid exists...
    app.status = "認証: pat · 編集可 · 同期: poll · ? ヘルプ".into();
    // ...but nothing has joined a room, so the plan is still the fast one.
    assert_eq!(app.sync_state, SyncState::Polling);
    assert_eq!(app.sync_state.poll_interval(), Duration::from_secs(3));
    // A sid that never works keeps failing; each failure re-asserts fast.
    app.set_sync_state(SyncState::Reconnecting);
    assert_eq!(
        app.poll_ctrl_rx_for_test().recv().unwrap(),
        Duration::from_secs(3)
    );
    assert!(app.status.contains("同期: 再接続中"));
}

#[test]
fn only_a_proven_room_relaxes_the_poll_and_a_drop_tightens_it_again() {
    let mut app = page(&["a"]);
    app.ws_attempted = true;
    app.status = "認証: sid · 編集可 · 同期: poll · ? ヘルプ".into();
    app.set_sync_state(SyncState::Live);
    assert!(app.status.contains("同期: ws"));
    app.set_sync_state(SyncState::Reconnecting);
    assert!(app.status.contains("同期: 再接続中"));
    let rx = app.poll_ctrl_rx_for_test();
    assert_eq!(rx.recv().unwrap(), Duration::from_secs(60));
    // The drop is delivered as its own message, so the sleeping poller
    // wakes on it instead of finishing a 60 s nap in silence.
    assert_eq!(rx.recv().unwrap(), Duration::from_secs(3));
}

#[test]
fn a_session_without_a_sid_is_never_called_reconnecting() {
    let mut app = page(&["a"]);
    app.ws_attempted = false;
    app.set_sync_state(SyncState::Reconnecting);
    assert_eq!(app.sync_label(), "poll");
}

#[test]
fn local_activity_restarts_fallback_polling_but_not_live_insurance_polling() {
    let mut app = page(&["a"]);
    app.reset_fallback_poll();
    assert_eq!(
        app.poll_ctrl_rx_for_test().recv().unwrap(),
        capability::FAST_POLL
    );

    app.set_sync_state(SyncState::Live);
    assert_eq!(
        app.poll_ctrl_rx_for_test().recv().unwrap(),
        capability::LIVE_POLL
    );
    app.reset_fallback_poll();
    assert!(
        app.poll_ctrl_rx_for_test().try_recv().is_err(),
        "a live room needs no extra GET"
    );
}

/// A picture written beside text has to be FETCHED like any other.
/// Collecting only whole-line pictures left the inline ones with a box
/// reserved and nothing ever put in it.
#[test]
fn pictures_inside_a_line_are_fetched_too() {
    let ctx = test_ctx();
    let a = "https://example.com/a.png";
    let b = "https://example.com/b.png";
    let mut app = page(&["t", &format!("[{a}] と [{b}]"), &format!("[{a}]")]);
    app.rebuild(80);
    app.start_image_loads(&ctx);
    for url in [a, b] {
        assert!(app.pending.contains(url), "{url} was never asked for");
    }
}
