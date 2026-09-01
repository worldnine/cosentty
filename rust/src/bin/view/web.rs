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

impl App {
    /// The render order for one Mermaid block, or `None` when the block
    /// cannot be rendered from the live web page: a historical snapshot (the
    /// browser only ever shows the current page), a line Cosense has not
    /// given an id yet, or a layout that has not happened.
    pub(crate) fn web_request(
        &self,
        kind: cosense::webrender::WebKind,
        code: &str,
        last_src: usize,
    ) -> Option<WebRequest> {
        if self.time.is_some() {
            return None;
        }
        let line_id = self.lines.get(last_src)?.id.clone();
        if line_id.is_empty() {
            return None;
        }
        Some(WebRequest {
            kind,
            project: self.project.clone(),
            title: self.title.clone(),
            page_id: self.page_id.clone(),
            line_id,
            code_hash: cosense::webrender::hash_code(code),
            dark: self.web_dark,
        })
    }

    /// Queue every not-yet-rendered diagram on the page as ONE batch. Returns
    /// immediately: the worker owns the browser, and the code block stays on
    /// screen until an artifact arrives.
    pub(crate) fn start_web_renders(&mut self, trigger: capability::Trigger) -> bool {
        // The browser can only ever show what the SERVER has, so nothing is
        // requested while a commit is still on its way there, or while a
        // commit is known to have failed. Both are whole-page gates: any
        // queued or lost commit could be the one that changes a diagram,
        // and `inflight == 0` alone only means "nothing in flight", not
        // "everything landed".
        if self.inflight > 0 || self.web_unsynced {
            return false;
        }
        // A historical snapshot is not what the server is showing now, so a
        // screenshot of the live page would be filed under the snapshot's
        // source hash — the same poisoning `src_epoch` guards against.
        if self.time.is_some() {
            return false;
        }
        // What this session may do, right now, for this project.
        let decision = capability::decide(&self.caps, self.render_policy, trigger);
        let auth = match decision {
            capability::Decision::Nothing => return false,
            capability::Decision::Render(cap) => Some(cap),
            capability::Decision::CacheOnly { notice } => {
                // Said once per page: repeating it per diagram, per pass,
                // would bury every other status the reader needs.
                if let Some(text) = notice {
                    if !self.web_notice_shown {
                        self.web_notice_shown = true;
                        self.note_web_failure(text.to_string());
                    }
                }
                None
            }
        };
        // The one anonymous attempt on an Unknown project is spent HERE,
        // when it is dispatched — not when it answers, or a second `R`
        // pressed while the first is in flight would spend it twice.
        if auth == Some(RenderCapability::Anonymous)
            && self.caps.visibility != capability::Visibility::Public
        {
            self.caps.anonymous_spent = true;
        }
        let mut reqs: Vec<WebRequest> = Vec::new();
        for b in &self.blocks {
            let Block::WebRender { kind, code, rows, last_src } = b else { continue };
            // An open session does NOT hold up the rest of the page: only
            // the block under the caret waits, because that one line's
            // buffer has not been committed yet (every caret MOVE commits
            // the dirty line first, so leaving a block releases it).
            if self.caret_is_inside(rows) {
                continue;
            }
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            let key = req.cache_key();
            if self.images.contains_key(&key)
                || self.web_errors.contains_key(&key)
                || self.web_pending.contains(&key)
                || reqs.iter().any(|r| r.cache_key() == key)
            {
                continue;
            }
            // A cache-only pass already looked at this one and found
            // nothing. Asking again would just re-answer `Missing`; only a
            // pass that may actually draw is worth sending.
            if self.web_missing.contains(&key) && auth.is_none() {
                continue;
            }
            reqs.push(req);
        }
        if reqs.is_empty() {
            return false;
        }
        let queued = auth.is_some();
        let keys: Vec<String> = reqs.iter().map(|r| r.cache_key()).collect();
        for k in &keys {
            self.web_pending.insert(k.clone());
            self.web_missing.remove(k);
        }
        // Those blocks now shimmer; the map is rebuilt with the layout.
        self.laid_width = 0;
        self.web_anim = std::time::Instant::now();
        if self
            .web_job_tx
            .send(WebJob::Render {
                gen: self.gen_now(),
                src_epoch: self.src_epoch_now(),
                reqs,
                max_cols: self.web_cols,
                auth,
            })
            .is_err()
        {
            // The worker is gone (shutting down). Nothing will ever answer,
            // so un-mark them: the code block stays, and it stops pulsing.
            for k in &keys {
                self.web_pending.remove(k);
            }
            return false;
        }
        queued
    }

    /// The browser hit a login wall. This says something about the COOKIE,
    /// never about the PAT that is reading and committing this page — so
    /// nothing here touches `editable`, the credential, or the status.
    pub(crate) fn note_denied(&mut self, denied: Vec<(String, Option<RenderCapability>)>) {
        // Every key in the batch goes back to being drawable: whatever we
        // decide below, none of these is a FAILURE of the diagram.
        for (key, _) in &denied {
            self.web_missing.insert(key.clone());
        }
        // The batch was one request with one credential state. If any key
        // in it names the state, that is the state that was refused.
        let attempted = denied
            .iter()
            .find_map(|(_, a)| *a)
            .unwrap_or(RenderCapability::Anonymous);
        // A refused cookie is a refused cookie whatever we do next: from
        // here it counts as absent, so no later pass presents it again.
        // (Without this, a private page would retry the same dead cookie on
        // every `R`, forever.)
        if attempted == RenderCapability::Authenticated {
            self.caps.cookie_rejected = true;
        }
        match capability::on_not_authorized(&self.caps, attempted) {
            // A public page that refused a cookie refused a STALE cookie.
            // Anonymous is a different request, and usually works — and it
            // retries EVERY key the batch lost, in one more batch.
            capability::Denial::RetryAnonymous => {
                // `cookie_rejected` above is what makes this retry genuinely
                // cookie-free, and it retries EVERY key the batch lost in
                // one more batch.
                self.start_web_renders(capability::Trigger::Manual);
            }
            capability::Denial::GiveUp => {
                // Only an attempt that carried no cookie can prove the
                // browser has nothing left to offer.
                if attempted == RenderCapability::Anonymous {
                    self.caps.browser_denied = true;
                }
                if !self.web_notice_shown {
                    self.web_notice_shown = true;
                    let msg = if self.caps.sid {
                        capability::sid_rejected()
                    } else {
                        capability::needs_sid()
                    };
                    self.note_web_failure(msg.to_string());
                }
            }
        }
    }

    /// Start the anonymous visibility probe for the current project, once.
    ///
    /// Free evidence first: if this session resolved NO credential for the
    /// project and still read the page, it is public by demonstration and
    /// no request is needed.
    pub(crate) fn ensure_visibility(&mut self, ctx: &Ctx) {
        if self.project.is_empty() || self.vis_asked.as_deref() == Some(&self.project) {
            return;
        }
        self.vis_asked = Some(self.project.clone());
        if ctx.client.credential_for(&self.project).is_none() {
            self.caps.visibility = capability::Visibility::Public;
            return;
        }
        spawn_visibility_probe(ctx.client.clone(), self.project.clone(), self.vis_tx.clone());
    }

    /// Take whatever the visibility probe learned. Pure bookkeeping: the
    /// verdict only ever widens or narrows what a LATER render pass may do.
    pub(crate) fn drain_visibility(&mut self) {
        while let Ok((project, verdict)) = self.vis_rx.try_recv() {
            if project == self.project {
                self.caps.visibility = verdict;
            }
        }
    }

    /// The poller's end of the control channel, for tests: `main` normally
    /// takes it when the poller is spawned.
    #[cfg(test)]
    pub(crate) fn poll_ctrl_rx_for_test(&mut self) -> &mpsc::Receiver<Duration> {
        self.poll_ctrl_rx.as_ref().expect("still held in tests")
    }

    /// Source lines belonging to a diagram that is being rendered right now,
    /// mapped to their position in the block. Derived from the blocks rather
    /// than the laid-out rows, so wrapped continuation rows of one source
    /// line pulse together.
    pub(crate) fn web_shimmer_rows(&self) -> HashMap<usize, (u16, u16)> {
        let mut out = HashMap::new();
        // Source mode shows the raw notation and never a picture, so there
        // is nothing there for the pulse to be about.
        if self.web_pending.is_empty() || self.mode == Mode::Source {
            return out;
        }
        for b in &self.blocks {
            let Block::WebRender { kind, code, rows, last_src } = b else { continue };
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            if !self.web_pending.contains(&req.cache_key()) {
                continue;
            }
            let len = rows.len() as u16;
            for (i, (src, _)) in rows.iter().enumerate() {
                out.insert(*src, (i as u16, len));
            }
        }
        out
    }

    /// Re-encode any diagram that was built for a different column cap than
    /// the pane now has. The picture keeps its place on screen — the old,
    /// wrongly-sized one stays up until the new encoding lands, which beats
    /// flickering back to source. Costs a PNG decode on the worker; the
    /// browser is not involved.
    pub(crate) fn rescale_diagrams(&mut self) {
        let want = self.web_cols;
        let mut keys: Vec<String> = Vec::new();
        for b in &self.blocks {
            let Block::WebRender { kind, code, last_src, .. } = b else { continue };
            let Some(req) = self.web_request(*kind, code, *last_src) else { continue };
            let key = req.cache_key();
            let Some(info) = self.images.get(&key) else { continue };
            if info.built_for != want && !self.web_rescaling.contains(&key) {
                keys.push(key);
            }
        }
        for key in keys {
            self.web_rescaling.insert(key.clone());
            if self
                .web_job_tx
                .send(WebJob::Rescale { gen: self.gen_now(), key: key.clone(), max_cols: want })
                .is_err()
            {
                self.web_rescaling.remove(&key);
            }
        }
    }

    /// Note that a diagram did not draw. This never touches `app.status`,
    /// so it cannot overwrite — or, on expiry, erase — a commit, auth or
    /// resync message.
    pub(crate) fn note_web_failure(&mut self, msg: String) {
        const SHOWN_FOR: std::time::Duration = std::time::Duration::from_secs(6);
        self.web_notice = Some((msg, std::time::Instant::now() + SHOWN_FOR));
    }

    /// Drop the diagram note once it has had its seconds, so the key hints
    /// come back. Returns whether anything changed.
    pub(crate) fn expire_web_notice(&mut self) -> bool {
        let Some((_, until)) = self.web_notice.as_ref() else { return false };
        if std::time::Instant::now() < *until {
            return false;
        }
        self.web_notice = None;
        true
    }

    /// Install finished web renders. A result from an older page generation
    /// is dropped WITHOUT touching any state: the reader has moved on, and
    /// the same key can legitimately be pending again for the current page
    /// (navigate away and back), so clearing by key alone would cancel the
    /// live request and leave the diagram pulsing forever.
    pub(crate) fn drain_web_renders(&mut self) -> bool {
        let mut changed = false;
        // Refusals from this drain, judged together once the loop ends.
        let mut denied: Vec<(String, Option<RenderCapability>)> = Vec::new();
        while let Ok(msg) = self.web_rx.try_recv() {
            if msg.gen != self.gen_now() {
                continue;
            }
            let WebMsg { key, rescale, attempted, res, .. } = msg;
            if rescale {
                // A rescale only ever changes the SIZE of a picture that is
                // already on screen. If it failed, the reader keeps the
                // size they had: no error, no notice, no lost diagram.
                self.web_rescaling.remove(&key);
                if let WebOutcome::Drawn(info) = res {
                    self.images.insert(key, info);
                    changed = true;
                }
                continue;
            }
            self.web_pending.remove(&key);
            match res {
                WebOutcome::Drawn(info) => {
                    self.images.insert(key, info);
                }
                // Not on disk, and this pass could not draw. The block goes
                // back to being source, silently — `R` will pick it up.
                WebOutcome::Missing => {
                    self.web_missing.insert(key);
                }
                // The BROWSER was refused. The REST credential is untouched:
                // edits and reads carry on exactly as before. Collected
                // rather than handled here — one batch refuses every key in
                // it at once, and handling them one at a time made the
                // second key see the first key's bookkeeping and give up.
                WebOutcome::Denied => {
                    denied.push((key, attempted));
                }
                // Nothing is recorded: not an image, not a miss, not an
                // error. The next pass sees the new source and asks again.
                WebOutcome::Stale => {}
                WebOutcome::Failed(e) => {
                    self.note_web_failure(t!("diagram: {e}（ソースを表示します）", "diagram: {e} (showing source)"));
                    self.web_errors.insert(key, e);
                }
            }
            changed = true;
        }
        if !denied.is_empty() {
            self.note_denied(denied);
        }
        // An `R` that arrived while the cache probe was out: now that the
        // misses are known, serve it. Once — the flag is cleared whether or
        // not there turned out to be anything to draw.
        if self.web_manual_wanted && self.web_pending.is_empty() {
            self.web_manual_wanted = false;
            self.start_web_renders(capability::Trigger::Manual);
        }
        changed
    }
}
