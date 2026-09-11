//! What this session can actually do.
//!
//! Auth here is deliberately **not one boolean**. A session holding a PAT but
//! no (or a stale) `connect.sid` still reads, edits and commits perfectly
//! well — it simply cannot push-sync. Collapsing those into a single
//! "authenticated" flag is what turns a missing cookie into an app-wide
//! failure, so the sid is tracked as its own fact and consulted only where
//! the push channel is concerned.
//!
//! Everything in this module is pure: no network, no clock, no I/O.

use std::time::Duration;

/// The first fallback-poll delay while the push channel is not proven. The
/// viewer backs off from here while the page stays unchanged.
pub const FAST_POLL: Duration = Duration::from_secs(3);
/// How fast it runs once a room is joined and caught up, and the ceiling for
/// an idle fallback poll.
pub const LIVE_POLL: Duration = Duration::from_secs(60);

/// Whether the project can be read with no credential at all.
///
/// `Unknown` is a real state, not a placeholder: Cosense answers 404 both for
/// a project that does not exist and for some it will not talk about, and a
/// network error tells us nothing either. Everything that consumes this must
/// stay safe when it cannot tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

impl Visibility {
    /// Map an anonymous `GET /api/projects/<name>` outcome. `None` is a
    /// transport failure (no response at all). Measured live: 200 on
    /// `help-jp`, 401 on a private project, 404 on a name that is not there.
    pub fn from_anonymous_status(status: Option<u16>) -> Self {
        match status {
            Some(200) => Visibility::Public,
            Some(401) | Some(403) => Visibility::Private,
            _ => Visibility::Unknown,
        }
    }
}

/// The live-update channel's state. Typed on purpose: the poller's interval
/// and the status line both read THIS, never the human-readable status text
/// the websocket thread also emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// No sid at all, or a sid whose room is not joined yet (startup).
    Polling,
    /// Room joined AND the post-join catch-up fetch succeeded.
    Live,
    /// Connect / join / catch-up failed, the socket died, or we are between
    /// attempts. Indistinguishable from `Polling` in effect — kept separate
    /// so the user can see that a push channel exists and is struggling.
    Reconnecting,
}

impl SyncState {
    /// The poller's starting interval for this state. Only a proven-live
    /// push channel starts at the slow insurance interval; fallback polling
    /// begins fast, then backs off while the page remains unchanged.
    pub fn poll_interval(self) -> Duration {
        match self {
            SyncState::Live => LIVE_POLL,
            SyncState::Polling | SyncState::Reconnecting => FAST_POLL,
        }
    }

    /// The `sync:` tag in the status line.
    pub fn label(self) -> &'static str {
        match self {
            SyncState::Polling => "poll",
            SyncState::Live => "ws",
            SyncState::Reconnecting => crate::ts!("再接続中", "reconnecting"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_sid_never_buys_the_slow_poll() {
        // The whole point: only a PROVEN room relaxes the interval.
        assert_eq!(SyncState::Polling.poll_interval(), FAST_POLL);
        assert_eq!(SyncState::Reconnecting.poll_interval(), FAST_POLL);
        assert_eq!(SyncState::Live.poll_interval(), LIVE_POLL);
    }
}
