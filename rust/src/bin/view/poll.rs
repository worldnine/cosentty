use super::*;

/// A freshly polled page: how web-side edits reach the screen without push.
pub(crate) struct PolledPage {
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) page: cosense::api::Page,
    /// The server-state epoch when this fetch was STARTED. If anything has
    /// advanced the epoch since — a local commit, an applied websocket
    /// commit, a page install — then this snapshot predates it and would
    /// roll the reader back. `commitId` cannot be used for this: it is not
    /// safely ordered from the client's side.
    pub(crate) epoch: u64,
}

/// The fallback poll's idle cadence. It starts quickly so a web edit made
/// while this page is active lands promptly, then relaxes while the server's
/// revision stays still. One PAT-only viewer used to issue about 20 GETs per
/// minute forever; the full sequence costs only four in its first minute and
/// one per minute thereafter.
const IDLE_POLL_STEPS: [Duration; 5] = [
    capability::FAST_POLL,
    Duration::from_secs(6),
    Duration::from_secs(12),
    Duration::from_secs(30),
    capability::LIVE_POLL,
];

/// Timing and last-seen state for the page poller. Pure so the cadence can be
/// tested without a clock or a live server.
#[derive(Debug)]
pub(crate) struct PollCadence {
    interval: Duration,
    adaptive: bool,
    step: usize,
    target: Option<(String, String)>,
    revision: Option<u64>,
}

impl PollCadence {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            adaptive: interval == capability::FAST_POLL,
            step: 0,
            target: None,
            revision: None,
        }
    }

    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    /// A push-state change woke the sleeper. Three seconds means fallback
    /// polling and restarts its idle ramp; sixty means a proven-live room and
    /// remains a fixed insurance poll rather than an adaptive one.
    pub(crate) fn retune(&mut self, interval: Duration) {
        self.interval = interval;
        self.adaptive = interval == capability::FAST_POLL;
        if self.adaptive {
            self.step = 0;
        }
    }

    /// Learn one successful response and choose the delay before the next.
    /// A new target establishes a baseline; a changed revision returns to the
    /// fast edge; an unchanged revision advances one idle step.
    pub(crate) fn observed(&mut self, project: &str, title: &str, revision: u64) {
        let target = (project.to_string(), title.to_string());
        let same_target = self.target.as_ref() == Some(&target);
        let changed = same_target && self.revision.is_some_and(|r| r != revision);
        self.target = Some(target);
        self.revision = Some(revision);

        if !self.adaptive {
            return;
        }
        self.step = if !same_target {
            1
        } else if changed {
            0
        } else {
            (self.step + 1).min(IDLE_POLL_STEPS.len() - 1)
        };
        self.interval = IDLE_POLL_STEPS[self.step];
    }
}

/// `commitId` is the ordinary revision. Templates and unusual replies may
/// omit it, so hash the page's stable body fields as a fallback instead of
/// mistaking every empty id for "unchanged".
fn poll_revision(page: &cosense::api::Page) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut h = std::collections::hash_map::DefaultHasher::new();
    if !page.commit_id.is_empty() {
        0u8.hash(&mut h);
        page.commit_id.hash(&mut h);
    } else {
        1u8.hash(&mut h);
        page.id.hash(&mut h);
        page.persistent.hash(&mut h);
        page.title.hash(&mut h);
        page.updated.hash(&mut h);
        page.lines_count.hash(&mut h);
        page.links.hash(&mut h);
        page.project_links.hash(&mut h);
        for line in &page.lines {
            line.id.hash(&mut h);
            line.text.hash(&mut h);
            line.user_id.hash(&mut h);
            line.created.hash(&mut h);
            line.updated.hash(&mut h);
        }
    }
    h.finish()
}

/// The web-edit poller: refetches the CURRENT page on its own thread and
/// ships it to the event loop, which applies it only when nothing local is
/// in flight (see `apply_remote`). Without a live push room it begins at 3 s
/// and backs off through [`IDLE_POLL_STEPS`] while the page is unchanged.
pub(crate) fn spawn_web_poller(
    client: Client,
    target: Arc<std::sync::Mutex<(String, String)>>,
    tx: mpsc::Sender<PolledPage>,
    ctrl: mpsc::Receiver<Duration>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    interval: Duration,
) {
    std::thread::spawn(move || {
        let mut cadence = PollCadence::new(interval);
        loop {
            // The sleep IS the control channel: a push channel that dies
            // during a 60 s nap must wake the reader. A control reply is
            // recognisable even when it names the current delay: timeouts
            // always fetch, while an equal retune does not.
            let previous = cadence.interval();
            let (next, fetch_now) = match absorb_interval(&ctrl, previous) {
                Some(v) => v,
                None => return, // app gone
            };
            let controlled = !fetch_now || next != previous;
            if controlled {
                cadence.retune(next);
            }
            if !fetch_now {
                continue;
            }
            let (project, title) = match target.lock() {
                Ok(t) => t.clone(),
                Err(_) => return,
            };
            if project.is_empty() || title.is_empty() {
                continue;
            }
            // Stamped BEFORE the request goes out: everything that happens
            // while it is in flight makes the answer stale.
            let started_at = epoch.load(std::sync::atomic::Ordering::SeqCst);
            if let Ok(page) = client.get_page_in(&project, &title) {
                cadence.observed(&project, &title, poll_revision(&page));
                if tx
                    .send(PolledPage {
                        project,
                        title,
                        page,
                        epoch: started_at,
                    })
                    .is_err()
                {
                    return; // app gone
                }
            }
        }
    });
}

/// Wait out `current`, or wake early on a new interval from the control
/// channel. Returns the interval to use next and whether to fetch right now;
/// `None` means the channel is gone and the poller should stop.
///
/// Speeding up fetches immediately — that transition means "the push channel
/// just stopped being trustworthy", and the point of the switch is to close
/// the gap, not to open a fresh one. Slowing down does not: the push channel
/// went live and just delivered a catch-up, so there is nothing to catch.
pub(crate) fn absorb_interval(
    ctrl: &mpsc::Receiver<Duration>,
    current: Duration,
) -> Option<(Duration, bool)> {
    match ctrl.recv_timeout(current) {
        // Timed out: an ordinary tick.
        Err(mpsc::RecvTimeoutError::Timeout) => Some((current, true)),
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
        Ok(first) => {
            // Several state changes can pile up while we slept; only the
            // last one describes the world.
            let mut next = first;
            loop {
                match ctrl.try_recv() {
                    Ok(d) => next = d,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return None,
                }
            }
            Some((next, next < current))
        }
    }
}

impl App {
    /// The poller's end of the control channel, for tests: `main` normally
    /// takes it when the poller is spawned.
    #[cfg(test)]
    pub(crate) fn poll_ctrl_rx_for_test(&mut self) -> &mpsc::Receiver<Duration> {
        self.poll_ctrl_rx.as_ref().expect("still held in tests")
    }
}
