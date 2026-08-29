//! What this session can actually do, split into independent axes.
//!
//! Auth here is deliberately **not one boolean**. A session holding a PAT but
//! no (or a stale) `connect.sid` still reads, edits and commits perfectly
//! well — it simply cannot push-sync, and cannot ask a browser to draw a
//! *private* page. Collapsing those into a single "authenticated" flag is
//! what turns a missing cookie into an app-wide failure, so the capabilities
//! are tracked apart and combined only at the point of a decision.
//!
//! Everything in this module is pure: no network, no clock, no I/O. The
//! state machine in HANDOFF.md §5b is these functions.

use std::time::Duration;

/// How fast the insurance poller runs while the push channel is not proven.
pub const FAST_POLL: Duration = Duration::from_secs(3);
/// How fast it runs once a room is joined and caught up.
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
    /// `help-jp`, 401 on `my-sandbox`, 404 on a name that is not there.
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
    /// The insurance poll interval for this state. Only a proven-live push
    /// channel earns the slow poll; a stale sid costs 0 s, not 60 s.
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
            SyncState::Reconnecting => "reconnecting",
        }
    }
}

/// When diagrams may be drawn. `COSENSE_WEB_RENDER`, default `manual`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderPolicy {
    /// Page load consults the disk cache only; `m` draws the rest.
    #[default]
    Manual,
    /// Every renderable miss is drawn as the page loads.
    Auto,
    /// No worker, no backend, no cache I/O — code blocks, always.
    Off,
}

impl RenderPolicy {
    pub fn from_env() -> Self {
        match std::env::var("COSENSE_WEB_RENDER").ok().as_deref() {
            Some("auto") => RenderPolicy::Auto,
            Some("off") => RenderPolicy::Off,
            _ => RenderPolicy::Manual,
        }
    }
}

/// What asked for this render pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A page load, a resize, an edit settling — anything automatic.
    Auto,
    /// The reader pressed `m`.
    Manual,
}

/// How a browser may be used for this project, right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderCapability {
    /// Launch with the session cookie (private pages need it).
    Authenticated,
    /// Launch with no cookie at all.
    Anonymous,
}

/// The outcome of the table in HANDOFF.md §5b.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Ask the backend; a cache miss may launch a browser.
    Render(RenderCapability),
    /// Serve disk-cache hits and nothing else. A miss stays a code block and
    /// is NOT an error — a later `m` must still be able to draw it.
    CacheOnly {
        /// Shown once per page when the reason is worth saying out loud.
        notice: Option<&'static str>,
    },
    /// The renderer is switched off: not even the cache is touched.
    Nothing,
}

/// Said once per page when a private diagram cannot be drawn. Names the
/// cookie, never a value.
pub const NEEDS_SID: &str = "非公開の図を描画するには connect.sid が必要です";
/// The same, when a cookie exists but the server has rejected it.
pub const SID_REJECTED: &str = "connect.sid が失効しています — 非公開の図は描画できません";

/// The session's browser-render standing for one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// A `connect.sid` exists (it may still be stale — that is what
    /// `browser_denied` records after the fact).
    pub sid: bool,
    pub visibility: Visibility,
    /// The browser was already refused for this project this session. Set so
    /// a hopeless relaunch is not attempted on every page.
    pub browser_denied: bool,
    /// The one anonymous attempt allowed on `Unknown` visibility, or as the
    /// public retry after `NotAuthorized`, has been spent.
    pub anonymous_spent: bool,
    /// The server has rejected this cookie. A rejected cookie is WORSE than
    /// no cookie — presenting it again just fails again — so from here on
    /// the session is treated as having none for render purposes. REST is
    /// untouched: the cookie is not what REST authenticates with.
    pub cookie_rejected: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            sid: false,
            visibility: Visibility::Unknown,
            browser_denied: false,
            anonymous_spent: false,
            cookie_rejected: false,
        }
    }
}

impl Capabilities {
    /// Moving to another project resets everything learned about the old one
    /// EXCEPT the session-wide fact of whether a sid exists.
    pub fn for_new_project(&self) -> Self {
        Self { sid: self.sid, ..Default::default() }
    }
}

/// The whole policy in one place. Pure — the caller supplies the state.
pub fn decide(caps: &Capabilities, policy: RenderPolicy, trigger: Trigger) -> Decision {
    if policy == RenderPolicy::Off {
        return Decision::Nothing;
    }
    // Manual is the default because a browser launch is the most expensive
    // thing this app can do. A page load may still SHOW what it has.
    if policy == RenderPolicy::Manual && trigger == Trigger::Auto {
        return Decision::CacheOnly { notice: None };
    }
    if caps.browser_denied {
        return Decision::CacheOnly { notice: None };
    }
    // A cookie the server has rejected buys nothing: sending it again is
    // the failure we just had. This is what makes the anonymous retry
    // actually anonymous.
    let usable_sid = caps.sid && !caps.cookie_rejected;
    match (usable_sid, caps.visibility) {
        // A sid can read a public page too, so it needs no special case.
        (true, _) => Decision::Render(RenderCapability::Authenticated),
        (false, Visibility::Public) => Decision::Render(RenderCapability::Anonymous),
        // Saying "you need a cookie" is only honest when we KNOW it is
        // private; the notice is suppressed for Unknown.
        (false, Visibility::Private) => Decision::CacheOnly {
            notice: Some(if caps.sid { SID_REJECTED } else { NEEDS_SID }),
        },
        // Unknown: automatic passes stay conservative — no browser is spent
        // guessing. An explicit `m` is allowed exactly one anonymous try.
        (false, Visibility::Unknown) => {
            if trigger == Trigger::Manual && !caps.anonymous_spent {
                Decision::Render(RenderCapability::Anonymous)
            } else {
                Decision::CacheOnly { notice: None }
            }
        }
    }
}

/// What to do when the browser reports `NotAuthorized`. Never touches the
/// REST credential: a refused cookie says nothing about a working PAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// Throw the browser session away (its cookie is in it) and try once
    /// more with none.
    RetryAnonymous,
    /// The renderer is out of options for this project this session.
    GiveUp,
}

pub fn on_not_authorized(caps: &Capabilities) -> Denial {
    // A public page that refused a cookie refused the COOKIE, not us: it is
    // stale. Anonymous is a different request and often works.
    if caps.sid && caps.visibility == Visibility::Public && !caps.anonymous_spent {
        Denial::RetryAnonymous
    } else {
        Denial::GiveUp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(sid: bool, visibility: Visibility) -> Capabilities {
        Capabilities { sid, visibility, ..Default::default() }
    }

    #[test]
    fn a_stale_sid_never_buys_the_slow_poll() {
        // The whole point: only a PROVEN room relaxes the interval.
        assert_eq!(SyncState::Polling.poll_interval(), FAST_POLL);
        assert_eq!(SyncState::Reconnecting.poll_interval(), FAST_POLL);
        assert_eq!(SyncState::Live.poll_interval(), LIVE_POLL);
    }

    #[test]
    fn visibility_reads_the_measured_status_codes() {
        assert_eq!(Visibility::from_anonymous_status(Some(200)), Visibility::Public);
        assert_eq!(Visibility::from_anonymous_status(Some(401)), Visibility::Private);
        assert_eq!(Visibility::from_anonymous_status(Some(403)), Visibility::Private);
        // 404 is both "no such project" and "not telling you" — never Public.
        assert_eq!(Visibility::from_anonymous_status(Some(404)), Visibility::Unknown);
        assert_eq!(Visibility::from_anonymous_status(None), Visibility::Unknown);
    }

    #[test]
    fn off_touches_nothing_at_all() {
        for trigger in [Trigger::Auto, Trigger::Manual] {
            let d = decide(&caps(true, Visibility::Public), RenderPolicy::Off, trigger);
            assert_eq!(d, Decision::Nothing);
        }
    }

    #[test]
    fn manual_shows_what_it_has_and_draws_only_when_asked() {
        let c = caps(true, Visibility::Private);
        assert_eq!(
            decide(&c, RenderPolicy::Manual, Trigger::Auto),
            Decision::CacheOnly { notice: None }
        );
        assert_eq!(
            decide(&c, RenderPolicy::Manual, Trigger::Manual),
            Decision::Render(RenderCapability::Authenticated)
        );
    }

    #[test]
    fn no_sid_on_a_public_project_still_renders() {
        assert_eq!(
            decide(&caps(false, Visibility::Public), RenderPolicy::Auto, Trigger::Auto),
            Decision::Render(RenderCapability::Anonymous)
        );
    }

    #[test]
    fn no_sid_on_a_private_project_falls_back_to_source_and_says_why() {
        let d = decide(&caps(false, Visibility::Private), RenderPolicy::Auto, Trigger::Auto);
        assert_eq!(d, Decision::CacheOnly { notice: Some(NEEDS_SID) });
        // The advice names the cookie; it can never name a value.
        assert!(NEEDS_SID.contains("connect.sid"));
    }

    #[test]
    fn unknown_visibility_is_automatic_only_when_explicitly_asked() {
        let c = caps(false, Visibility::Unknown);
        assert_eq!(
            decide(&c, RenderPolicy::Auto, Trigger::Auto),
            Decision::CacheOnly { notice: None }
        );
        assert_eq!(
            decide(&c, RenderPolicy::Auto, Trigger::Manual),
            Decision::Render(RenderCapability::Anonymous)
        );
        // ...and only once.
        let spent = Capabilities { anonymous_spent: true, ..c };
        assert_eq!(
            decide(&spent, RenderPolicy::Auto, Trigger::Manual),
            Decision::CacheOnly { notice: None }
        );
    }

    #[test]
    fn a_refused_browser_is_not_retried_all_session() {
        let c = Capabilities { browser_denied: true, ..caps(true, Visibility::Public) };
        assert_eq!(
            decide(&c, RenderPolicy::Auto, Trigger::Manual),
            Decision::CacheOnly { notice: None }
        );
    }

    #[test]
    fn a_rejected_cookie_counts_as_no_cookie_at_all() {
        // This is the whole point of the retry: presenting the SAME
        // rejected cookie again would just fail again.
        let c = Capabilities { cookie_rejected: true, ..caps(true, Visibility::Public) };
        assert_eq!(
            decide(&c, RenderPolicy::Auto, Trigger::Manual),
            Decision::Render(RenderCapability::Anonymous)
        );
        // On a private project there is nothing anonymous can do, and the
        // advice differs: the cookie is stale, not absent.
        let p = Capabilities { cookie_rejected: true, ..caps(true, Visibility::Private) };
        assert_eq!(
            decide(&p, RenderPolicy::Auto, Trigger::Manual),
            Decision::CacheOnly { notice: Some(SID_REJECTED) }
        );
        assert!(SID_REJECTED.contains("connect.sid"));
    }

    #[test]
    fn a_stale_cookie_on_a_public_page_is_retried_without_it() {
        assert_eq!(on_not_authorized(&caps(true, Visibility::Public)), Denial::RetryAnonymous);
        // Private has nothing to retry WITH, so the renderer alone gives up.
        assert_eq!(on_not_authorized(&caps(true, Visibility::Private)), Denial::GiveUp);
        assert_eq!(on_not_authorized(&caps(false, Visibility::Public)), Denial::GiveUp);
        let spent = Capabilities { anonymous_spent: true, ..caps(true, Visibility::Public) };
        assert_eq!(on_not_authorized(&spent), Denial::GiveUp);
    }

    #[test]
    fn moving_project_forgets_that_project_s_findings_but_not_the_sid() {
        let c = Capabilities {
            sid: true,
            visibility: Visibility::Private,
            browser_denied: true,
            anonymous_spent: true,
            cookie_rejected: true,
        };
        let next = c.for_new_project();
        assert!(next.sid);
        assert_eq!(next.visibility, Visibility::Unknown);
        assert!(!next.browser_denied);
        assert!(!next.anonymous_spent);
        assert!(!next.cookie_rejected, "another project may accept it fine");
    }
}
