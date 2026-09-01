use super::*;

/// One batch of web renders: everything on ONE page, so the worker can serve
/// them with a single browser navigation. `gen` is the App's page generation
/// at the time of the request.
pub(crate) enum WebJob {
    /// Draw these in a browser (or serve them from the disk cache), encoded
    /// for a pane whose text area is `max_cols` wide.
    Render {
        gen: u64,
        reqs: Vec<WebRequest>,
        max_cols: u16,
        /// The source epoch this batch was built from. Re-checked on the
        /// worker before the browser is asked and again before anything is
        /// written to the cache.
        src_epoch: u64,
        /// `Some` lets the batch reach a browser, in that credential state.
        /// `None` is cache-only: serve what is on disk and answer every
        /// miss with `Missing`. That is the default page-load pass, and it
        /// is also what a project we may not render gets.
        auth: Option<RenderCapability>,
    },
    /// Re-encode an artifact already on disk at a new column cap, because
    /// the pane was resized. No browser is involved: this is a decode plus
    /// a resize, done on the worker so the UI thread never stalls on it.
    Rescale { gen: u64, key: String, max_cols: u16 },
    /// Quit. Sent once, on the way out, so the worker can be joined.
    Stop,
}

impl WebJob {
    /// The page generation this job belongs to. `Stop` belongs to none.
    pub(crate) fn gen(&self) -> Option<u64> {
        match self {
            WebJob::Render { gen, .. } | WebJob::Rescale { gen, .. } => Some(*gen),
            WebJob::Stop => None,
        }
    }
}

/// A finished web render, already decoded and protocol-encoded on the
/// worker. `gen` and `key` together decide whether it is still wanted.
pub(crate) struct WebMsg {
    pub(crate) gen: u64,
    pub(crate) key: String,
    /// True when this answers a `Rescale`. A failed rescale is not a failed
    /// diagram: the artifact already on screen stays, and the reader is
    /// told nothing.
    pub(crate) rescale: bool,
    /// What this reply was produced with. A refusal has to be attributed to
    /// a credential state, not guessed from session flags: one browser
    /// batch refuses EVERY key in it at once, and the whole batch has to be
    /// judged as the single event it was.
    pub(crate) attempted: Option<RenderCapability>,
    pub(crate) res: WebOutcome,
}

/// What became of one request. Typed because the three failures are NOT
/// interchangeable: a cache miss must leave the artifact renderable later,
/// a refusal must retune the session's capabilities, and only a genuine
/// failure is worth telling the reader about.
pub(crate) enum WebOutcome {
    Drawn(ImageInfo),
    /// Nothing on disk, and this pass was not allowed to draw. Not an
    /// error: it must not land in `web_errors`, or `R` could never render it.
    Missing,
    /// The browser was refused (login wall). The session decides what to do
    /// with that — it says nothing about the REST credential.
    Denied,
    Failed(String),
    /// The page source moved while this was in flight, so the picture (if
    /// there is one) describes different text than the key naming it. The
    /// block simply goes back to source: it is NOT a miss and NOT a failure,
    /// and the pass that follows the new source picks it up.
    Stale,
}

impl WebOutcome {
    /// Did this produce a picture?
    #[cfg(test)]
    pub(crate) fn is_drawn(&self) -> bool {
        matches!(self, WebOutcome::Drawn(_))
    }
}

/// The web-render worker: the ONLY thread that talks to a browser. It takes
/// whole batches, answers the on-disk cache without launching anything, and
/// hands back terminal-ready images. The UI thread never waits on it — it
/// only sends and later drains.
pub(crate) fn spawn_web_worker(
    jobs: mpsc::Receiver<WebJob>,
    out: mpsc::Sender<WebMsg>,
    backend: Arc<dyn WebBackend>,
    picker: Picker,
    // `current_gen` is the App's page generation: work queued for a page the
    // reader has already left is dropped BEFORE it costs a decode or a
    // browser navigation.
    cache: ArtifactCache,
    current_gen: Arc<std::sync::atomic::AtomicU64>,
    // The App's source epoch: a render whose source has moved is discarded
    // rather than written to the cache under a key it no longer matches.
    current_src: Arc<std::sync::atomic::AtomicU64>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Idle window. The backend keeps a browser warm between batches (a
        // re-render in a warm browser is roughly twice as fast), but a
        // viewer nobody is editing should not hold a browser process.
        // 15 s is short enough that a reader who has finished with a page
        // is not paying for a resident Chrome, and long enough to cover the
        // pause between "the diagram appeared" and "I edited it again".
        let idle = idle_window();
        let reap_at_once = idle.is_zero();
        let live = |gen: u64| gen == current_gen.load(std::sync::atomic::Ordering::SeqCst);
        loop {
            let first = match jobs.recv_timeout(if reap_at_once {
                // A zero window still needs a real block here; the browser
                // is already gone (reaped below), so waking is free.
                Duration::from_secs(3600)
            } else {
                idle
            }) {
                Ok(job) => job,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    backend.idle();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            // Take everything already queued behind it. A burst of jobs for
            // the page the reader has moved on from must not make the
            // current page wait through several browser navigations.
            let mut batch = vec![first];
            let mut stop = false;
            while let Ok(job) = jobs.try_recv() {
                batch.push(job);
            }
            // Stale-generation jobs are never ACCEPTED, so they owe no
            // reply: `set_page` clears `web_pending`/`web_rescaling` for the
            // page being left, which is what keeps this from leaking. See
            // `set_page_clears_the_state_a_dropped_job_would_have_answered`.
            batch.retain(|job| match job.gen() {
                None => {
                    stop = true;
                    false
                }
                Some(g) => live(g),
            });

            // Every same-generation render is merged into ONE batch, so a
            // page's diagrams still cost a single navigation however many
            // passes queued them.
            let mut merged: Vec<WebRequest> = Vec::new();
            let mut merged_cols: Option<u16> = None;
            let mut merged_auth: Option<RenderCapability> = None;
            let mut merged_src = 0u64;
            let mut merged_gen = 0u64;
            for job in batch {
                match job {
                    WebJob::Stop => {}
                    WebJob::Rescale { gen, key, max_cols } => {
                        // A rescale is answered from the disk cache. If the
                        // PNG is gone, the answer is an error so the viewer
                        // stops waiting — it keeps the size it has.
                        let res = match cache.get(&key) {
                            Some(png) => match decode_web_png(&picker, &png, max_cols) {
                                Ok(info) => WebOutcome::Drawn(info),
                                Err(e) => WebOutcome::Failed(e),
                            },
                            None => WebOutcome::Failed(t!("作り直せる図はありません", "no cached artifact to resize")),
                        };
                        let _ = out.send(WebMsg { gen, key, rescale: true, attempted: None, res });
                    }
                    WebJob::Render { gen, src_epoch, reqs, max_cols, auth } => {
                        // Freshness is judged PER JOB, before merging. The
                        // checks around the browser below are per batch, so
                        // a job built against the old source that merged
                        // with a fresh one would ride in on its freshness —
                        // and be filed under a hash it no longer matches.
                        if src_epoch
                            != current_src.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            for req in &reqs {
                                let _ = out.send(WebMsg {
                                    gen,
                                    key: req.cache_key(),
                                    rescale: false,
                                    attempted: None,
                                    res: WebOutcome::Stale,
                                });
                            }
                            continue;
                        }
                        merged_gen = gen;
                        merged_src = src_epoch;
                        merged_cols = Some(max_cols);
                        // Coalescing several passes: the most permissive
                        // one wins, so an explicit `R` arriving behind a
                        // page-load probe is not silently downgraded to
                        // cache-only.
                        merged_auth = merged_auth.or(auth);
                        for req in reqs {
                            if !merged.iter().any(|r| r.cache_key() == req.cache_key()) {
                                merged.push(req);
                            }
                        }
                    }
                }
            }

            if let Some(max_cols) = merged_cols {
                run_render_batch(
                    &*backend,
                    &picker,
                    &cache,
                    &out,
                    merged_gen,
                    merged_src,
                    &current_src,
                    merged,
                    max_cols,
                    merged_auth,
                );
            }
            if reap_at_once {
                // `COSENSE_WEB_IDLE_SECS=0`: hold no browser between
                // batches at all. Every page then pays the cold-start cost.
                backend.idle();
            }
            if stop {
                break;
            }
        }
    })
}

/// How long a browser is kept warm between batches.
/// `COSENSE_WEB_IDLE_SECS`, clamped to 0..=300; 0 reaps after every batch.
pub(crate) fn idle_window() -> Duration {
    let secs = std::env::var("COSENSE_WEB_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s <= 300)
        .unwrap_or(15);
    Duration::from_secs(secs)
}

/// Serve one page's worth of requests: the disk cache first, the browser for
/// whatever is left. EVERY request gets exactly one reply, including cache
/// misses and decode failures — a request with no reply would leave the
/// viewer pulsing a diagram forever.
pub(crate) fn run_render_batch(
    backend: &dyn WebBackend,
    picker: &Picker,
    cache: &ArtifactCache,
    out: &mpsc::Sender<WebMsg>,
    gen: u64,
    // The source this batch was built from. A render captures what the
    // SERVER shows when the browser looks, so if the source has moved since,
    // the picture belongs to different text than the key naming it.
    src_epoch: u64,
    current_src: &std::sync::atomic::AtomicU64,
    reqs: Vec<WebRequest>,
    max_cols: u16,
    // `None` means this pass may not launch a browser: hits are served,
    // misses are answered `Missing`.
    auth: Option<RenderCapability>,
) {
    let mut to_render: Vec<WebRequest> = Vec::new();
    for req in reqs {
        let key = req.cache_key();
        match cache.get(&key).map(|png| decode_web_png(picker, &png, max_cols)) {
            Some(Ok(info)) => {
                let _ = out.send(WebMsg {
                    gen,
                    key,
                    rescale: false,
                    attempted: None, // served from disk; no credential was used
                    res: WebOutcome::Drawn(info),
                });
            }
            // On disk but unreadable: truncated by a crash, or corrupted
            // underneath us. That must not become a permanent failure for
            // this artifact — drop the entry and let the browser produce
            // it again.
            Some(Err(_)) => {
                cache.remove(&key);
                to_render.push(req);
            }
            None => to_render.push(req),
        }
    }
    if to_render.is_empty() {
        return;
    }
    let fresh = || src_epoch == current_src.load(std::sync::atomic::Ordering::SeqCst);
    let Some(auth) = auth else {
        // Cache-only pass. Every miss still owes exactly one reply, or the
        // viewer would pulse those blocks forever.
        for req in &to_render {
            let key = req.cache_key();
            let _ = out.send(WebMsg {
                gen,
                key,
                rescale: false,
                attempted: None,
                res: WebOutcome::Missing,
            });
        }
        return;
    };
    // Checked here, before the browser is asked: the source may have moved
    // while this job sat in the queue behind another batch.
    if !fresh() {
        for req in &to_render {
            let key = req.cache_key();
            let _ = out.send(WebMsg {
                gen,
                key,
                rescale: false,
                attempted: None,
                res: WebOutcome::Stale,
            });
        }
        return;
    }
    let results = backend.render_batch(&to_render, auth);
    // ...and again here. A page of diagrams takes seconds to draw, which is
    // exactly long enough for a commit to land underneath it. Writing that
    // PNG under this key would poison the cache for a week.
    let still_fresh = fresh();
    for (req, res) in to_render.iter().zip(results) {
        let key = req.cache_key();
        let res = match res {
            _ if !still_fresh => WebOutcome::Stale,
            Ok(png) => match decode_web_png(picker, &png, max_cols) {
                Ok(info) => {
                    cache.put(&key, &png);
                    WebOutcome::Drawn(info)
                }
                Err(e) => WebOutcome::Failed(e),
            },
            Err(WebError::NotAuthorized) => WebOutcome::Denied,
            Err(e) => WebOutcome::Failed(e.to_string()),
        };
        let _ = out.send(WebMsg { gen, key, rescale: false, attempted: Some(auth), res });
    }
}

/// A freshly polled page: how web-side edits reach the screen (~3 s).
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

/// The web-edit poller: refetches the CURRENT page every few seconds on
/// its own thread and ships it to the event loop, which applies it only
/// when nothing local is in flight (see `apply_remote`). Cosense proper
/// uses a websocket; polling one small JSON keeps this dependency-free
/// and is plenty "live" for a wiki.
pub(crate) fn spawn_web_poller(
    client: Client,
    target: Arc<std::sync::Mutex<(String, String)>>,
    tx: mpsc::Sender<PolledPage>,
    ctrl: mpsc::Receiver<Duration>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    interval: Duration,
) {
    std::thread::spawn(move || {
        let mut interval = interval;
        loop {
            // The sleep IS the control channel: a push channel that dies
            // 2 s into a 60 s nap must not leave the reader waiting out the
            // other 58. `recv_timeout` wakes on either.
            let fetch_now = match absorb_interval(&ctrl, interval) {
                Some((next, urgent)) => {
                    interval = next;
                    urgent
                }
                None => return, // app gone
            };
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
                if tx.send(PolledPage { project, title, page, epoch: started_at }).is_err() {
                    return; // app gone
                }
            }
        }
    });
}

/// Ask, off the UI thread, whether a project is readable with no credential
/// at all. One anonymous request per project, and only when the answer could
/// change a decision.
pub(crate) fn spawn_visibility_probe(
    client: Client,
    project: String,
    tx: mpsc::Sender<(String, capability::Visibility)>,
) {
    std::thread::spawn(move || {
        let verdict = client.probe_visibility(&project);
        let _ = tx.send((project, verdict));
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
