# Cosense Web renderer — MVP handoff

`code:mmd` / `code:mermaid` / `code:<name>.mmd` blocks now display as pictures in the
TUI. The pictures are not rendered locally: headless Chrome opens the **real Cosense
page** and screenshots the element **Cosense itself drew**. This is deliberately built
as a general `request -> artifact` boundary so TeX, `.icon` rows and ProjectCSS-styled
blocks can be added later without touching the viewer.

Branch `feat/cosense-web-render`, based on `05aefad`.

---

## 1. Browser automation: raw CDP, and why

**Chosen: raw Chrome DevTools Protocol over the dependencies the crate already has.**

| option | verdict |
| --- | --- |
| **raw CDP** (chosen) | `tungstenite` (already in `Cargo.toml` for the Cosense push channel) + `reqwest` (already there) + `std::process`. **Zero new dependencies.** The whole backend is four commands. |
| Playwright / Puppeteer | Needs a Node runtime and a `node_modules` tree at install time for what is a single self-contained Rust binary. Rejected on distribution cost. |
| `headless_chrome` crate | Sync API, would have fit — but pulls its own CDP codegen + a large tree for four commands we can spell out. |
| `chromiumoxide` | Requires an async runtime (tokio); the viewer is entirely synchronous threads-and-channels. Rejected. |
| Chrome CLI `--screenshot` | Cannot wait for a selector and cannot clip to an element. Would force full-page-screenshot-and-guess-the-crop, which the brief rules out. |

The capture flow mirrors what Puppeteer's `elementHandle.screenshot` does internally:
navigate → poll until the element has been drawn → read `getBoundingClientRect()` +
scroll offsets → `Page.captureScreenshot` with an explicit `clip`. No full-page
screenshot is cropped by guesswork.

Notable details:

* Chrome is launched with `--remote-debugging-port=0` into a **throwaway
  `--user-data-dir`** under `$TMPDIR`; the port it actually bound is read back from
  `DevToolsActivePort`. stdout/stderr are `Stdio::null()` — anything Chrome printed
  would land in the alternate screen and wreck the TUI.
* **The browser is kept alive between batches** (`Mutex<Option<Session>>`). Relaunching
  per batch cost ~1.4 s of startup *and* discarded Chrome's warm HTTP/V8 caches, which
  is most of what makes a Cosense page slow to draw. Measured on help-jp/Mermaid:
  navigate+draw is ~3.5 s cold and ~2.3 s warm, so a whole batch goes 5.8 s → 2.6 s.
  A session that returns *any* error is reaped and never reused (a CDP error can leave
  the socket half-consumed); if the failed session was an inherited one, the batch is
  retried once on a fresh browser. The worker closes the browser after its idle window
  (`COSENSE_WEB_IDLE_SECS`, default 15 s), so a viewer left open holds nothing.
* `clip.scale = 2` gives a crisp 2× PNG; `build_image` downsizes it to cells anyway.
* **Before measuring we `window.scrollTo(0, 0)`.** Cosense's navbar is `position:
  sticky`, and with `captureBeyondViewport` it paints wherever the viewport happens to
  be — `scrollIntoView` put it straight over the diagram. (Caught in live smoke; the
  first capture had the navbar across the top of a sequence diagram.)

## 2. Module boundary and contract

### `cosense::webrender` — the contract (no browser code)

```rust
enum WebKind { Mermaid }                 // future: Tex, Icon, ProjectCss…
    fn selector(&self, line_id) -> String   // "#mermaid-preview-<lineId>"
    fn ready_child(&self) -> &'static str   // "svg" — drawn, not merely present

struct WebRequest { kind, project, title, page_id, line_id, code_hash, dark }
    fn page_url(&self) -> String
    fn cache_key(&self) -> String        // "web:mermaid:<16 hex>"  — identity
    fn same_artifact(&self, other) -> bool

enum WebError { NoBrowser, Timeout{secs}, NotRendered, NotAuthorized, Backend(String) }
struct WebArtifact { key, png }

trait WebBackend: Send + Sync {
    fn render_batch(&self, reqs: &[WebRequest]) -> Vec<Result<Vec<u8>, WebError>>;
    fn shutdown(&self);
}

struct UnavailableBackend(WebError)      // installed when no browser is found
struct FakeBackend                       // tests: scripted answers + a gate
struct ArtifactCache                     // ~/.cache/cosentty/webrender/<key>.png
```

`render_batch` takes a **whole page's worth** of requests: one navigation serves every
diagram on the page.

### Invalidation: `code_hash`, deliberately NOT `commitId` or the pane width

The brief asked for "`project/page identity + commitId + block lineId + width/theme`
相当". **The `commitId` half was a trap and is not used.** Cosense commits on every
keystroke-level edit — pressing Enter for a new line is a commit — so keying on it made
*every* diagram on a page re-render whenever *any* line was touched. That is not a
theoretical concern: the first user to open the viewer hit it within minutes
("edit モードに入ると一旦また図のレンダリングが始まってしまう").

The key is instead the block's **own source hash** plus page identity and theme.
A block's text changes exactly when its picture does — strictly more precise than the
page commit, and it still invalidates on a *remote* edit to the diagram, because the
websocket apply rewrites `app.lines` and therefore the hash.

**The pane width is not in the key either.** The browser's viewport decides raster
quality and nothing else — what the reader sees is set by the cell size the artifact is
*encoded* at, which is a local decision (see §7c). Keying on a bucketed pane width meant
a ten-column resize cost a 3–6 s re-render, and dragging a window edge queued one batch
per bucket crossed, which the serial worker then ground through while the batch for the
width the reader actually ended on waited at the back. That read as "resizing
re-renders, and sometimes never loads". The backend now renders at a fixed
`RENDER_WIDTH_PX = 1000`, and resizing never involves the browser.

Page identity is still guarded, twice over: `project/title/page_id/line_id` are in the
key, and `web_gen` (bumped on every page install) drops any result that arrives for a
page the reader has already left.

Because the browser can only ever show what the **server** has, a render is only issued
for a block whose text the server already has:

* `app.inflight > 0` gates the **whole page** — any queued commit could be the one that
  changes a diagram.
* An open edit session gates **only the block under the caret** (`caret_is_inside`).
  That one line's buffer is uncommitted; every other block on the page is committed and
  renders normally. Since every caret move calls `session_commit_dirty` first, moving
  off a diagram is what releases it — the reader edits a diagram, moves away, and it
  starts rendering while the session is still open.

Typing itself therefore never launches a browser, and when a picture does land
mid-session the cursor keeps its screen row (`relayout_preserving_screen_row`, which
already runs on every layout invalidation). The credential is **not** in `WebRequest` — it is constructor
state on the backend, so it cannot reach a key, a log or an error string.

### `cosense::chrome` — the one implementation

`ChromeBackend::detect(sid) -> Option<Self>` (`None` = no browser → the viewer
installs `UnavailableBackend`). Everything CDP lives here: `Cdp` (one websocket,
request/response, skips the event stream), `find_chrome`, `capture`, `b64_decode`.

### Viewer wiring

`render::Block::WebRender { kind, code, rows, last_src }` is a single block that
**carries its own fallback**: `rows` is the ordinary highlighted code block,
`(source line, styled line)` pairs. The layout draws the artifact if it has one and
`rows` otherwise — so "no browser" is not a special case, it is the default.

One worker thread (`spawn_web_worker`) owns the backend, answers the disk cache
without launching anything, decodes PNG → `SlicedProtocol`, and sends back
`(gen, key, Result<ImageInfo, String>)`. Artifacts land in the App's existing `images`
map, so drawing, partial scroll, resize and cursor handling need **no** new code path.

## 3. Files changed

| file | change |
| --- | --- |
| `rust/src/capability.rs` | **new** — the split capability state (`Visibility`, `SyncState`, `RenderPolicy`, `Capabilities`, `decide`, `on_not_authorized`). Pure: no network, no clock, no I/O. §5b is this file. |
| `rust/src/webrender.rs` | **new** — the contract above, `cache_key`, `FakeBackend`, `ArtifactCache`. |
| `rust/src/chrome.rs` | **new** — CDP client, Chrome discovery/launch/reaping, element capture, base64. |
| `rust/src/bin/web_smoke.rs` | **new** — live smoke binary (not in `cargo test`). `--twice` exercises the warm-browser path; it also reports Chrome process counts across `idle()`/`shutdown()`. |
| `rust/src/render.rs` | `mermaid_lang()`; `code:` blocks whose language is Mermaid emit `Block::WebRender` carrying the code plus the unchanged code rows. Everything else is byte-for-byte as before. |
| `rust/src/theme.rs` | `shimmer_level` / `shimmer_style` — the pure "still rendering" brightness wave, mixing an RGB foreground toward the terminal background (DIM attribute for named colors). |
| `rust/src/bin/view.rs` | `WebJob` (`Render` / `Rescale`), `WebMsg`, `spawn_web_worker`; App fields (`web_gen`, `web_dark`, `web_pending`, `web_errors`, `web_shimmer`, `web_cols`, `web_rescaling`, `web_unsynced`, `web_notice`, `caps`, `render_policy`, `web_missing`, `sync_state`, channels); `web_request`, `start_web_renders(Trigger)`, `rescale_diagrams`, `drain_web_renders`, `note_web_failure` / `expire_web_notice`, `hint_text`, `mark_desynced` / `mark_synced`, `set_sync_state` / `sync_label`, `absorb_interval` (the poller's control channel), `ensure_visibility` / `drain_visibility`, `note_denied`; the generic `R` render key; `diagram_max_cols` + a `max_cols` argument on `build_image`; layout arm for `Block::WebRender`; backend construction + `shutdown()` on the quit path; `?` help entry. |
| `rust/src/api.rs` | `Page.commit_id` (`commitId`), reported by the smoke binary; `probe_visibility` — an anonymous `GET /api/projects/<name>` that attaches **no** credential. |
| `rust/src/ws.rs` | `WsEvent::State { project, title, state }` emitted unthrottled on every failure edge and once the post-join catch-up lands; `sync_plan` → `initial_plan` (a sid no longer buys the slow poll). |
| `rust/src/bin/probe.rs` | prints the new block kind. |
| `rust/KEYMAP.md` | user-facing section: dependency, auth, fallback, notation, env vars. |
| `rust/Cargo.toml` | registers the `web_smoke` bin. **No new dependencies.** |

## 4. Live DOM investigation (the part that could not be guessed)

Verified on `https://scrapbox.io/help-jp/Mermaid` in a real browser:

```
div.line#L<lineId>
  └─ div.mermaid-preview#mermaid-preview-<lineId>
       └─ svg#mermaid-preview-svg-<lineId>     (width="100%", viewBox set)
```

**The critical finding: `<lineId>` is the code block's LAST content line, not its
`code:` header.** On help-jp/Mermaid the three previews hang off source lines 13, 25
and 38 — `A-- hello -->B`, `another task :24d`, `S->>B: HTTPS Response with HTML` —
each the last line of its block. Deriving the id from the header would have produced
a selector that never matches, and every diagram would have silently timed out.

`render.rs` already strips trailing blanks from a code block, so its last `raws` entry
is exactly that line; `Block::WebRender.last_src` carries it, and the viewer resolves
it to `app.lines[last_src].id`.

Second finding: `.mermaid-preview` exists in the DOM from first paint and is filled in
later, so presence is not readiness — we poll for a `<svg>` child with a non-zero box.

## 5. Authentication

* **public** projects render with no credential at all (verified on help-jp/Mermaid).
* **private** projects: the session's browser cookie (`--sid` / `COSENSE_SID`) is sent
  once via CDP `Network.setCookie` (`.scrapbox.io`, `secure`, `httpOnly`) into the
  throwaway profile, which is deleted with the batch. It is **never** in argv, a log,
  an error message or a cache key — `webrender::tests::cache_key_never_carries_a_credential`
  pins the key's shape (`web:mermaid:` + 16 hex digits, nothing else).
* A PAT is deliberately **not** used: it is a REST credential the browser cannot
  present. Same split as the websocket channel (see `NOTE-websocket-sync.md`).
* If the browser is redirected to a login URL the batch reports `NotAuthorized`;
  the viewer shows the code block and a one-line status.

## 5b. Capability state machine

Auth is **not one boolean**. A session that has a PAT but no (or a stale) `connect.sid`
can still read, edit and commit — it just cannot push-sync and cannot render a *private*
diagram. The four capabilities are tracked separately so a missing sid never degrades
into an app-wide "auth failure":

| capability | source | what it gates |
|---|---|---|
| REST | `AuthStore::resolve` (none / PAT / SA / sid) | reads, edits, commits |
| push sync | `connect.sid` **plus** a joined room | live updates vs. polling |
| browser render | sid **and/or** project visibility | whether Chrome may launch |
| visibility | anonymous `GET /api/projects/<name>` | Public / Private / Unknown |

### Live updates (`capability::SyncState`)

The state is typed and comes from the websocket thread as `WsEvent::State`; nothing
parses the human-readable status string. The poller's interval follows the state
through a control channel, so it switches **during** a 60 s sleep.

| state | when | poll interval |
|---|---|---|
| `Polling` | no sid, or sid present but not yet joined (**startup**) | 3 s |
| `Live` | room joined **and** the post-join catch-up fetch succeeded | 60 s (insurance) |
| `Reconnecting` | connect / join / catch-up failure, disconnect, half-open socket | 3 s |

A stale sid therefore costs 0 s, not 60 s: the fast poll runs from startup and is only
relaxed once the push channel has actually proven itself. Every failure edge sends
`Reconnecting` **unthrottled** — a swallowed one would leave the poller at 60 s, which
is the exact silence this replaces. Switching slow→fast fetches immediately.

### Browser render (`capability::decide`)

`Trigger::Auto` is a page load or source update; `Trigger::Manual` is the `R` key. `browser_denied` is
set for the rest of the session (per project) once the renderer has been refused.

| policy | trigger | sid | visibility | decision |
|---|---|---|---|---|
| `text` | — | — | — | `Nothing` (no cache I/O, the text tier / source) |
| `image` | Auto/Manual | yes (accepted) | any | `Render(Authenticated)` |
| `image` | Auto/Manual | no | Public | `Render(Anonymous)` |
| `image` | Auto/Manual | no | Private | `CacheOnly` + notice「非公開の図を描画するには connect.sid が必要です」(once per page) |
| `image` | Auto | no | Unknown | `CacheOnly`, silent — conservative |

(2026-09-11: the policy became `text | image`; `off` reads as `text`, `manual` and
`auto` as `image`. The `manual` rows — cache-only on load, browser on `R` — are gone.)
| `auto` | Manual | no | Unknown | `Render(Anonymous)`, one attempt |
| `auto` | Auto/Manual | yes (**rejected**) | Public | `Render(Anonymous)` — a rejected cookie counts as none |
| `auto` | Auto/Manual | yes (**rejected**) | Private | `CacheOnly` + notice「connect.sid が失効しています」 |
| any | — | — | — | `browser_denied` ⇒ `CacheOnly` |

`NotAuthorized` from the browser never touches the REST credential:

| sid | visibility | on `NotAuthorized` |
|---|---|---|
| yes | Public | mark the cookie rejected, drop the session (Chrome dies with its profile), retry **once** with no cookie |
| yes | Private | `browser_denied` — renderer only; edits keep working |
| no | any | `browser_denied` |

A private PNG already in the cache is still shown: reaching it required reading the
page over REST, and the cache is 0700/0600 (§7e).

### Visibility, and why `Unknown` is a real state

Measured anonymously against the live API:

| response | meaning |
|---|---|
| 200 | `Public` (`help-jp`) |
| 401 / 403 | `Private` (手元の非公開プロジェクト) |
| 404 | `Unknown` — Cosense returns it both for a missing project and for some hidden ones |
| network error | `Unknown` |

A session whose REST reads resolved **no credential** and succeeded is `Public` with
zero extra requests. The probe is anonymous by construction (no header is attached),
so neither the sid nor the PAT is ever handed to a visibility lookup, and it runs on
the render worker — never on the UI thread.

## 6. Tests and build

* `cargo test`: **228 green** — lib **120** and view **108**. No test launches a browser or touches the network, and none
  of them sleeps: the job channel and the fake backend's gate are the synchronisation.
* `cargo build --release`: succeeds, **no new warnings**.

New coverage, against the acceptance list:

| requirement | test |
| --- | --- |
| Mermaid filename/language detection | `render::mermaid_is_recognised_by_language_and_by_filename` (`mmd`, `mermaid`, `MMD`, `flow.mmd`, `図.mermaid` vs `js`, `mmdx`, `mermaidjs`, `readme.md`) |
| block → lineId, multiple blocks | `render::a_mermaid_block_becomes_one_web_render_keyed_on_its_last_line`, `render::several_mermaid_blocks_stay_separate_and_other_languages_are_untouched`, `view::each_mermaid_block_is_requested_against_its_own_cosense_line_id` |
| stale generation / page rejected | `view::a_result_for_an_older_page_generation_is_dropped`, `webrender::a_new_source_or_page_is_a_different_artifact` |
| an unrelated edit does NOT re-render | `view::only_the_diagrams_own_source_invalidates_it`, `view::typing_never_launches_a_browser` |
| an open session only blocks its own diagram | `view::an_open_session_only_holds_back_the_block_under_the_caret` |
| resizing never re-renders | `view::only_the_diagrams_own_source_invalidates_it` (resizes to 60/100/200/37 columns queue nothing), `webrender::resizing_the_pane_is_not_a_new_artifact` |
| the reader sees the renderer working | `theme::shimmer_*` (4), `view::a_rendering_diagram_pulses_its_code_and_stops_when_it_lands` |
| a diagram never overflows the pane | `view::a_diagram_is_never_encoded_wider_than_the_pane` (the cap, and the encoder honouring it at 64/35/14/5/1 columns), `view::narrowing_the_pane_re_encodes_the_diagram_from_its_cached_png` (40- and 20-column panes: a `Rescale` job — never a browser render — is queued once, and the installed artifact fits `text_rect`) |
| the cache is private, atomic and self-repairing | `webrender::the_cache_is_private_and_repairs_what_it_finds`, `webrender::a_reader_never_sees_a_half_written_artifact`, `webrender::empty_and_expired_entries_are_dropped_rather_than_served`, `webrender::a_corrupt_entry_can_be_dropped_so_it_is_re_rendered`, `webrender::the_ttl_is_configurable_and_bounded`, `webrender::a_schema_bump_orphans_old_artifacts_instead_of_serving_them` |
| shutdown cannot start a browser | `chrome::a_shut_down_backend_never_starts_another_browser`, `chrome::a_running_backend_reports_the_real_launch_failure` |
| the worker joins, and always replies | `view::the_worker_joins_on_stop_and_answers_every_accepted_job`, `view::a_rescale_that_cannot_be_served_keeps_the_diagram_it_has`, `view::a_send_to_a_dead_worker_stops_the_pulse_instead_of_hanging_it` |
| stale work costs nothing and breaks nothing | `view::work_queued_for_a_page_the_reader_left_never_reaches_the_browser`, `view::a_stale_result_never_cancels_the_live_request_for_the_same_key`, `view::set_page_clears_the_state_a_dropped_job_would_have_answered`, `view::several_passes_over_one_page_cost_a_single_browser_batch` |
| a failed commit keeps diagrams as source | `view::a_failed_commit_stops_the_server_s_old_diagram_being_filed_under_the_new_source`, `view::an_edit_that_never_reached_the_commit_worker_also_desyncs` |
| a failure note does not eat the key hints | `view::a_diagram_failure_gives_the_key_hints_back`, `view::a_diagram_note_never_hides_or_erases_a_commit_or_auth_message` |
| renderer failure → code fallback | `view::a_renderer_failure_leaves_the_code_block_on_screen`, `webrender::unavailable_backend_fails_every_request_without_a_browser` |
| UI thread does not block | `view::the_ui_thread_never_waits_for_the_browser` (the fake backend is pinned mid-render; the UI still queues, lays out and reports pending in <200 ms, and the artifact arrives after the gate is released) |
| selector / artifact correspondence | `webrender::selector_addresses_the_preview_by_line_id`, `webrender::fake_backend_answers_by_key`, `view::an_artifact_replaces_the_code_block_and_edit_puts_it_back` |
| credential never leaks | `webrender::cache_key_never_carries_a_credential` |
| edit contract preserved | `view::an_artifact_replaces_the_code_block_and_edit_puts_it_back` (second half) |
| a stale sid never costs 60 s of silence | `capability::a_stale_sid_never_buys_the_slow_poll`, `ws::a_sid_alone_does_not_earn_the_slow_poll`, `view::a_stale_sid_polls_fast_from_the_first_second`, `view::only_a_proven_room_relaxes_the_poll_and_a_drop_tightens_it_again` |
| the poller retunes mid-sleep | `view::speeding_up_the_poll_fetches_at_once_slowing_down_does_not`, `view::a_pile_of_state_changes_collapses_to_the_last_one`, `view::the_poller_stops_when_the_app_is_gone` |
| sync state is typed, not parsed | `view::a_session_without_a_sid_is_never_called_reconnecting` (the label comes from `SyncState`; nothing reads the status text) |
| no sid + public still renders | `capability::no_sid_on_a_public_project_still_renders`, `view::no_sid_on_a_public_project_renders_anonymously_and_leaves_rest_alone` |
| no sid + private: cache only, REST intact | `capability::no_sid_on_a_private_project_falls_back_to_source_and_says_why`, `view::no_sid_on_a_private_project_serves_the_cache_and_never_launches_a_browser` (backend calls = 0, the cached diagram still shows, `editable` untouched) |
| the notice is per page, not per diagram | `view::the_private_notice_is_said_once_per_page_not_once_per_diagram` |
| unknown visibility is conservative | `capability::unknown_visibility_is_automatic_only_when_explicitly_asked`, `view::unknown_visibility_waits_for_an_explicit_r` |
| a rejected cookie is retried anonymously | `capability::a_rejected_cookie_counts_as_no_cookie_at_all`, `capability::a_stale_cookie_on_a_public_page_is_retried_without_it`, `view::a_stale_cookie_on_a_public_page_retries_without_it` (asserts the retry job's `auth`) |
| a refused private render keeps edits working | `view::a_refused_private_render_falls_back_to_source_without_disabling_edits` |
| visibility resets across projects | `capability::moving_project_forgets_that_project_s_findings_but_not_the_sid`, `view::moving_to_another_project_forgets_the_old_one_s_verdict` |
| visibility mapping matches the live API | `capability::visibility_reads_the_measured_status_codes` |
| manual is the default and costs no browser | `capability::manual_shows_what_it_has_and_draws_only_when_asked`, `view::by_default_opening_a_page_shows_cached_diagrams_and_starts_no_browser` |
| `R` renders the missing artifacts in one batch | `view::r_renders_the_artifacts_the_page_load_could_not` |
| a cache miss is not a failure | `view::a_cache_miss_never_blocks_the_later_r` |
| auto / off | `capability::off_touches_nothing_at_all`, `view::auto_draws_on_load_and_off_draws_never` |
| a manual edit waits for explicit rendering | `view::editing_a_drawn_artifact_in_manual_mode_waits_for_explicit_render` |
| `R` types a character while editing | `view::r_is_an_ordinary_character_while_editing` |
| a refused batch is retried as one | `view::a_whole_refused_batch_is_retried_anonymously_in_one_go` (two diagrams; one anonymous batch carries both, and only an anonymous refusal sets `browser_denied`), `capability::a_stale_cookie_on_a_public_page_is_retried_without_it` |
| a stale room never holds the new page slow | `view::navigating_away_from_a_live_room_goes_back_to_the_fast_poll`, `view::a_live_from_the_room_the_reader_left_is_ignored`, `view::a_rejoin_that_stalls_leaves_the_reader_on_the_fast_poll` |
| a late poll never rolls the page back | `view::a_poll_that_started_before_a_websocket_commit_never_rolls_it_back`, `view::a_poll_that_started_before_a_local_commit_never_rolls_it_back`, `view::a_fresh_poll_still_applies_web_edits` |
| blankness alone is not a refusal | `chrome::a_readable_page_is_never_called_unauthorised` |
| a title called `auth` is not a login wall | `chrome::an_ordinary_page_whose_title_is_auth_is_not_a_login_wall` (`auth`, `login`, `authentication`, `my-login`, and a project named `auth`), `chrome::a_real_redirect_to_an_auth_route_is_a_login_wall`, `chrome::a_slow_page_titled_auth_times_out_rather_than_blaming_the_cookie` (the reported path end to end) |
| a render whose source moved is discarded | `view::a_render_whose_source_moved_is_never_filed_under_the_old_hash` (gated backend, A→B commit; nothing is written under A's key and B is queued), `view::a_stale_job_merged_with_a_fresh_one_is_still_thrown_away` (two jobs drained together; the older is abandoned rather than carried in on the newer's freshness) |
| a failed reload is not a successful one | `view::a_failed_reload_keeps_the_reader_in_history`, `view::a_failed_reload_at_the_newest_snapshot_also_stays_put`, `view::a_conflict_whose_reload_fails_stops_rendering_until_it_is_resolved`, `view::a_snapshot_never_renders_diagrams` |
| agreement clears the drift, staleness does not | `view::an_undo_that_restores_agreement_lets_diagrams_render_again`, `view::a_stale_equal_poll_does_not_clear_the_drift` |
| `R` during the cache probe is not lost | `view::r_pressed_while_the_cache_probe_is_out_is_served_when_it_answers` |
| a diagram keeps the cursor it was edited with | `view::leaving_an_edit_inside_a_diagram_lands_on_the_picture_not_before_it` (header / interior / last), `view::a_cursor_before_a_diagram_is_left_where_it_is`, `view::an_undrawn_diagram_block_keeps_its_source_lines_addressable` |

## 7. Live smoke results

`cargo run --bin web_smoke -- <project> <title>`; PNGs land in `$TMPDIR/cosense-web-smoke/`.

**Public, no credential needed — `https://scrapbox.io/help-jp/Mermaid`**

| line id (selector `#mermaid-preview-<id>`) | result |
| --- | --- |
| `65695bc797c2910000c699b2` | 1332×124 PNG |
| `65695d8097c2910000c699e8` | 1332×314 PNG |
| `65695ac297c2910000c699a0` | 1332×788 PNG (sequence diagram, visually verified) |

| batch of 3, one navigation | time |
| --- | --- |
| cold (browser launched) | **5.8 s** — launch+attach 1.5 s · navigate 0.6 s · **wait-for-draw 3.5 s** · capture 0.16 s |
| warm (browser reused) | **2.6 s** — navigate 0.2 s · wait-for-draw 2.3 s · capture 0.13 s |
| already in the disk cache | instant, no browser |

The 3.5 s is Cosense's own page boot, not ours; it is the floor for a first render.

**Private, with `COSENSE_SID` — `<sandbox>/テスト`**

| line id | result | time |
| --- | --- | --- |
| `056f612a7f428aa9f83223e2` (flowchart) | 1374×180 PNG, visually verified | |
| `9a3c759df691479055deb208` (pie) | 1374×918 PNG | |
| `7bbaac0d02c5590651d0d957` (deliberately broken) | `diagram not drawn by Cosense` → code fallback | |
| **batch of 3** | | **10.2 s cold, 8.5 s warm** (the broken block spends the 6 s quiet-cutoff either way) |

Browser lifecycle, measured in the same run: **10** Chrome processes alive while the
session is warm, **0** after `idle()`, **0** after `shutdown()`, **0** after the process
exits, and no leftover `cosentty-chrome-*` profile directory.

Failure paths exercised live: `COSENSE_CHROME=/nonexistent/chrome` → `no Chrome found`;
private project with no credential → REST 401 before the browser is ever reached.

### What was written to the smoke page

Two commits were appended to `<sandbox>/テスト` (a throwaway
page). **They are still there** — deliberately, so the smoke keeps something to render:

```
code:tui-smoke.mmd          ← valid flowchart
 flowchart LR
  TUI-- CDP -->Chrome
  Chrome-- PNG -->TUI
code:mermaid                ← valid pie chart
 pie title smoke
  "rendered" : 3
  "fallback" : 1
code:broken.mmd             ← DELIBERATELY invalid, exercises the fallback
 flowchart LR
  A -->]]] B (((
```

Delete them with `cosense previewEdit` + `submitEdit` if the page is wanted clean.

### Two bugs the live smoke caught (neither was visible in unit tests)

1. **The sticky navbar in the capture** — fixed by parking the page at `y=0` before
   measuring (§1).
2. **One broken diagram failed the whole page.** The wait loop consumed the entire
   budget on the block Cosense refuses to draw, and the capture phase then started
   *past* its deadline, so even the diagrams that had rendered came back as timeouts.
   Fixed two ways: capture gets its own 10 s allowance after the wait, and the wait
   ends early once the page has been quiet for 6 s. Broken-block page went 25 s → 10 s,
   and the two good diagrams now render. A block that never draws now reports
   `NotRendered` ("diagram not drawn by Cosense"), not a misleading timeout.

## 7b. Feedback while a diagram is rendering

A render takes seconds, so the block it will replace shows that work is happening: the
code block dims and a band of brightness runs down its rows (`theme::shimmer_level` /
`shimmer_style`, `App::web_shimmer`). Design constraints that shaped it:

* **The text must not move.** A spinner or a "rendering…" line would reflow the page
  under a reader who is mid-sentence. Only brightness changes.
* **Brightness is made by mixing toward the terminal background**, never by inventing a
  color — the theme still owns every hue. A named/indexed color has no components to
  mix, so it falls back to the DIM attribute (in practice code rows are
  syntax-highlighted and therefore RGB).
* **It stays readable**: the wave spans 0.55–1.0 of the original color, and the floor is
  a dimmed version of the real text, not a placeholder.
* **It costs nothing when idle.** The map is empty unless something is pending; the
  event loop's input tick tightens from 120 ms to 60 ms only while it is not.
* Source mode (`Tab`) never pulses — there is no picture there for it to be about.

## 7c. Fitting the pane

The draw clips an image to the pane's text column, so an artifact encoded wider than the
pane silently loses its right-hand side — a clipped photo is still a photo, a clipped
flowchart has lost half its meaning. Diagrams are therefore encoded at
`diagram_max_cols(text_w) = min(text_w, 64)`.

When the pane crosses that cap, `rescale_diagrams` queues a `WebJob::Rescale` for each
affected key. The worker re-reads the artifact's PNG **from the disk cache** and
re-encodes it — a decode plus a resize, no browser, no network. The old (wrongly sized)
picture stays on screen until the new encoding lands, which reads better than flickering
back to source, and the draw clips it in the meantime. Duplicate work is prevented by
`web_rescaling`; if the PNG is not in the cache the diagram simply keeps the size it has.

Ordinary images keep their long-standing fixed 64-column cap and are not rescaled — that
is the shared image path and is deliberately left alone here.

## 7d. The status line is also the key-hint bar

The bottom row shows key hints only while `app.status` is empty, so a message parked
there costs the reader their hints for the rest of the session. A diagram failure gets its **own slot** (`App::web_notice`) rather than writing to
`app.status`, with a fixed priority in `App::hint_text`:

    edit-session keys  >  cursor-line links  >  status  >  diagram notice  >  key hints

Ranking below `status` is the point: a commit failure, an auth error or a resync notice
must never be overwritten by a picture that did not draw — nor erased when the diagram
note times out, which is what an earlier version could do. The notice gives its row back
after six seconds so the hints return. No other status message's lifecycle is touched;
the general status line still has no fade, and changing that belongs on `main`.

`COSENSE_WEB_DEBUG` is a **file path**, not a flag. The render worker runs while the TUI
owns the alternate screen, so phase timings are appended to that file and nothing is
ever written to stdout or stderr. Verified: a run with it set produces zero bytes on
stderr.

## 7e. The artifact cache is credential-bearing storage

The cached PNGs are renders of the user's own pages, private ones included, and the
Chrome profile holds the `connect.sid` cookie for as long as the browser runs. Both are
handled accordingly:

| | |
| --- | --- |
| cache directory | `0700`, created and re-asserted on every open |
| artifacts | `0600`, set at creation via `OpenOptions::mode` — no window at 0644 |
| repair | opening the cache walks it: anything an earlier version left group/other-readable is tightened, empty and expired files are removed, and temp files older than an hour are cleaned up |
| writes | `create_new` 0600 temp file **in the same directory**, `write_all`, `sync_all`, then `rename` — a reader (this process, another viewer, the next run after a crash) sees the old bytes or the new ones, never a prefix |
| corrupt entry | dropped, and the request goes back to the browser — a truncated file must never become a permanently cached failure |
| Chrome profile | `tempfile`-created unique `0700` directory; a creation failure is an error. Never a predictable `$TMPDIR` name plus `create_dir_all`, which a local attacker could pre-create or aim elsewhere |

Measured on this machine after a live run: `drwx------` on the directory, `-rw-------`
on every artifact.

### Cache identity: schema and TTL

`ARTIFACT_SCHEMA` (currently **1**) is part of the key. Bump it whenever *our* capture
pipeline changes the pixels for unchanged source — the viewport width, the capture
scale, the selector, the readiness condition. A bump orphans old artifacts rather than
serving them; the sweep reclaims the files.

It cannot cover the other half of the rendering environment: **Cosense's own Mermaid
version and the project's CSS change without telling us, and nothing in the page
identifies them**, so a complete identity is not available. Artifacts therefore carry a
**7-day TTL** (`ARTIFACT_TTL_DAYS`, overridable with `COSENSE_WEB_CACHE_TTL_DAYS`; `0`
disables reuse, an out-of-range value falls back to the default). Past the TTL an entry
is a miss and is deleted, so a Cosense-side change works itself out within a week
without the reader ever knowing there was a cache.

## 7f. Shutdown contract

`spawn_web_worker` returns a `JoinHandle`, and `main` ends the feature in this order:

1. `WebBackend::shutdown()` — refuses further work (checked on entry to `run_batch` and
   again immediately before a browser is spawned, so a shutdown cannot be raced into
   starting a Chrome) and SIGKILLs any browser in flight. That kill is also what
   unblocks the worker: its DevTools socket dies, so a batch mid-navigation errors out
   promptly instead of waiting out its 25-second budget. `page_target` — the one loop
   that watched only its deadline — honours the stop flag too.
2. The terminal is restored, so a slow reap is never a black screen.
3. `WebJob::Stop`, then `join()`.

After that, "no browser and no worker outlives this process" is a fact rather than a
hope. The smoke binary asserts it against the pid and profile **it** started
(`ChromeBackend::last_owned`), not against every Chrome on the machine, and exits
nonzero if either survives or if any requested diagram failed.

### Generations and replies

`web_gen` is a shared atomic. The worker drains everything queued, drops jobs whose
generation is not current **before** spending a decode or a navigation on them, and
merges the remaining same-page renders into one batch — so a page the reader has left
can no longer make the current page wait through several browser navigations.

Two invariants hold this together:

* **Stale jobs are never accepted and owe no reply.** That is only safe because
  `set_page` clears `web_pending`/`web_rescaling` for the page being left, pinned by
  `view::set_page_clears_the_state_a_dropped_job_would_have_answered`.
* **Every accepted job gets exactly one reply**, cache misses and decode failures
  included. A reply says whether it answers a `Rescale`: a failed resize keeps the
  picture already on screen rather than reporting a failed diagram. A send to a dead
  worker rolls back what it optimistically marked, so the shimmer stops.

A stale result touches **no** state. It used to clear pending by key, which after
navigating away and back would cancel the live request for the same key and leave the
diagram pulsing forever.

## 7g. Local/server divergence

`inflight == 0` means nothing is in flight; it does not mean everything landed. After a
refused commit (`CommitOutcome::Failed`) or an edit that never reached the commit worker
(`commit_tx.send` failed), the local lines and the server have parted ways — and a
render would then screenshot the **server's** version of a block and file it under the
**local** text's hash, quietly attaching the wrong picture to the source the reader is
looking at.

`App::web_unsynced` blocks all rendering while that is true. It is deliberately sticky:
a later commit succeeding says nothing about the edit that did not, so only an
authoritative page install clears it — `set_page`, or `install_remote_lines` on a
websocket resync. The conflict path already reloads through `set_page`, so it clears
too, and nothing about the existing "never lose what you typed" contract changes. The
~3 s poller is deliberately **not** a clear point: its apply gate is conditional, so
treating it as authoritative would re-open the window this exists to close.

## 7h. Render policy: a browser starts when you ask

Launching Chrome is by a wide margin the most expensive thing this viewer does
(~1.4 s of startup, ~250 MB resident, 8–17 s for a page of diagrams). Doing that on
every page load, for a reader who may only be passing through, is not a good trade.

`[view] diagrams` / `COSENSE_WEB_RENDER`, default **`text`** (2026-09-11; was
`off | manual | auto`, still read as aliases):

| value | page load / source update | `R` |
|---|---|---|
| `image` | renders every renderable miss in **one** batch; the text tier shows until the picture lands, and whenever it failed | clears the failures and renders again |
| `text` | nothing: no browser, no cache directory is created or swept. The worker thread still waits, so switching to `image` on the settings screen needs no restart | points at the setting |

`R` means **Render**, not Mermaid. It is shared by every `WebKind`, so future TeX,
`.icon` and ProjectCSS-backed artifacts do not need notation-specific keys. Uppercase
also makes an operation that can launch a ~500 MiB browser harder to trigger by accident.
Inside an edit session it types the letter because session keys are routed before the
reader's keymap (`handle_key` → `handle_session_key`).

Manual mode has no hidden browser start. When source changes, the artifact key changes;
a matching cached artifact appears automatically, otherwise the ordinary source fallback
stays until `R`. Auto mode uses the same source-change path but may launch the browser.
This removes the former `web_drawn_blocks` edit-follow exception and keeps the contract
literal: **cache is automatic, Chrome is explicit**.

A cache miss is recorded in `web_missing`, **not** `web_errors`: a miss is not a failure,
it stops the block pulsing, and it must leave the artifact renderable by a later `R`.

`COSENSE_WEB_IDLE_SECS` (0..=300, default **15**) is how long a browser is kept warm
between batches — a re-render in a warm browser is roughly twice as fast. `0` reaps it
after every batch.

## 7i. What a private page looks like to a browser with no cookie

Found by running the smoke against 手元の非公開プロジェクト with `COSENSE_SID` unset.

An anonymous request for a private page returns **HTTP 401** and the SPA shell renders
with no `.lines` and no `.page` — and **the URL does not change**, so the existing
`/login` redirect check never fired. The batch therefore spent its entire 25 s budget
and reported `browser timed out`, which names the wrong cause entirely.

The wait loop now also gives up when `document.readyState === 'complete'` and the page
body has still not appeared after 4 s, reporting `NotAuthorized`. Measured on that page:
**27.5 s → 8.3 s**, with the right reason. This is what makes the anonymous-retry branch
of §5b reachable in practice rather than theoretical.

The conservative gate still does the real work: a project known to be `Private` with no
usable cookie never launches a browser at all. `Unknown` + an explicit `R` is the one
place a wasted launch is possible, and it costs ~8 s once per project per session.

## 7j. Two epochs, and why they are not one

Both guard against "work started before the world moved", but they guard
different work and must never be merged — one counter would either throw away
good poll snapshots on every keystroke, or good renders on every poll.

| | `server_epoch` | `src_epoch` |
|---|---|---|
| guards | a poll response already in flight | a render already in flight |
| stamped by | the poller, immediately before its GET | `start_web_renders`, onto the job |
| advanced by | commit `Done`, an applied websocket commit, any page install | `rerender()` — i.e. any change to the local source |
| checked | `apply_remote`, before anything else | the worker: per job before merging, then per batch before the browser AND before `cache.put` |
| on mismatch | drop the snapshot; the next poll carries the newer state | reply `Stale`; free the pending mark, record nothing |

`commitId` is deliberately not used for either: it is not safely ordered from
the client's side.

The per-job check matters as much as the two batch checks. Coalescing merges
everything drained together into one browser navigation, so a job built against
the old source that arrives alongside a fresh one would be judged by the batch's
freshness and ride in — filing the new text's screenshot under the old text's
hash, which is the exact poisoning this guards against.

`WebOutcome::Stale` is neither a miss nor a failure. It must not enter
`web_missing` (that would muddy the manual policy's cache-only pass and block
the re-queue) nor `web_errors` (that is for diagrams that genuinely cannot be
drawn). The block simply returns to source, and the next pass — which sees the
new text — asks for the right artifact.

### Where rendering is refused outright

Every one of these means "a screenshot taken now would be filed under a hash it
does not match":

* `inflight > 0` — a commit is on its way to the server.
* `web_unsynced` — a commit is known to have failed, or a 409 recovery could not
  refetch. Cleared only by an authoritative install, or by a **fresh** poll whose
  lines match the screen exactly (§7g).
* `time.is_some()` — a historical snapshot is not what the server is showing.

## 7k. A refusal is a property of a batch, not of a key

Chrome refuses a *navigation*: one login wall, N replies. Judging those one at a
time meant the first key started the anonymous retry and the second key, reading
the bookkeeping the first had just written, concluded the browser was dead — so
on a public page with a stale sid and two diagrams, one diagram silently lost
its retry.

Replies therefore carry `attempted: Option<RenderCapability>` — what the batch
actually used, not something inferred afterwards — and the drain collects every
`Denied` and judges them together, once:

| attempted | visibility | outcome |
|---|---|---|
| `Authenticated` | Public | `cookie_rejected`, and **one** anonymous batch carrying every key the refusal lost |
| `Authenticated` | Private / Unknown | `cookie_rejected`, source fallback, one notice |
| `Anonymous` | any | `browser_denied` — the only thing that sets it |

`cookie_rejected` is set for any refused cookie regardless of branch: without it
a private page re-presents the same dead cookie on every `R`, forever.
`anonymous_spent` is now only about the one Unknown-visibility gamble — using it
to infer refusals was what broke the batch case.

## 7l. Blankness is not evidence

`readyState === 'complete'` with no page body looks identical for a slow SPA, a
throttled machine and a page we may not read. Calling all three `NotAuthorized`
was wrong in the two cases that matter to a reader on a bad connection.

When the blank grace expires the browser is asked, **once**, what the page API
says in its own cookie state (an in-page `fetch` with `credentials: 'include'`).

The same applies to the address bar. `href.contains("/login") || href.contains("/auth")`
is a substring test on a whole URL, and it condemns perfectly ordinary Cosense
pages — `/<project>/auth`, `/<project>/login`, `/<project>/my-login` all match.
On a page like that, a slow SPA or a single broken Mermaid block was reported as
an authentication failure.

A Cosense page is `/<project>/<title>`; an auth route lives at the **root**. So
`chrome::redirected_to_auth` compares the requested `page_url()` with the current
location as URLs — host, then first path segment against the project actually
requested — and never looks at the title, which the reader may call anything:

| requested | current | verdict |
|---|---|---|
| `/proj/auth`, `/proj/login`, `/proj/my-login` | unchanged | page (no redirect) |
| `/auth/index` | `/auth/other-page` | page (the project is named `auth`) |
| `/proj/Mermaid` | `/login`, `/login/google`, `/auth/…` | **wall** |
| `/auth/Mermaid` | `/login` | **wall** (root route, whatever the project) |
| any | another host with an auth segment | **wall** |

The two signals are combined by the pure `chrome::auth_verdict(api, redirected)`,
and **the API outranks the URL**: a 200 means this browser can read this page, so
whatever the address looks like, the missing picture is not an authentication
problem — and calling it one would send the viewer down the cookie-retry path for
a page that is merely slow. Only 401/403 is a refusal; anything else — including
no answer at all, with no redirect — falls back to the ordinary budget and
reports `Timeout`/`NotRendered`. A redirect with no API evidence still counts.

## 8. Known limits, security, distribution

* **Renders the committed page, never the buffer** — see §7g for what happens when the
  two are known to disagree.
* **`tempfile` is the one dependency this feature adds**, for the Chrome profile
  directory. The browser automation itself still adds none — see §1.
* **Chrome's sandbox is left on**: this browser holds the live `connect.sid` and loads
  remote content (ProjectCSS can pull third-party resources). A container that cannot
  sandbox opts out with `COSENSE_CHROME_NO_SANDBOX=1`; without it Chrome simply exits
  early there and the code block stays.
* **Time-machine snapshots are never rendered** (`web_request` returns `None` when
  `app.time` is set): the browser can only show the current page, and a current
  diagram on a historical snapshot would be a lie. Snapshots show code.
* **Cost.** ~5.8 s for the first uncached page, ~2.6 s for a re-render in a warm
  browser; ~3.5 s of the first is Cosense's own page boot and cannot be removed from
  this design. Results are cached to `~/.cache/cosentty/webrender/`, so a revisit is
  instant and launches nothing. Resizing never involves the browser at all (§7c), and
  renders never fire for the block under the edit caret or while the page is desynced.
* **Process hygiene.** `Session::drop` kills *and waits* the child and removes its
  profile (retried briefly — Chrome's helpers outlive the SIGKILL on their parent and
  hold files open in there); it runs when a batch fails, when the worker goes idle
  (`COSENSE_WEB_IDLE_SECS`, default 15 s), and on quit. See §7f for the full shutdown contract.
* **Nothing from the browser is executed in-process.** Only PNG bytes cross back; no
  SVG, no HTML, no page script. The credential's only exit is `Network.setCookie`.
* **Distribution.** Chrome/Chromium/Edge/Brave must be installed. Auto-detected on
  macOS (`/Applications/…`) and via `PATH` on Linux; `COSENSE_CHROME` overrides, and an
  explicit-but-missing setting reports "no browser" rather than silently launching a
  different one. No browser → diagrams simply stay code blocks.
* **Narrow panes still clip ordinary images.** Diagrams are fitted to the pane (§7c);
  gyazo and other inline images keep the pre-existing fixed 64-column cap and are clipped
  in a pane narrower than that. Giving them the same treatment means touching the shared
  image path, which is out of scope for this branch.
* **A rescale needs the PNG on disk.** If the artifact cache entry is missing (cache
  cleared mid-session, or the write failed), the diagram keeps whatever size it has
  rather than re-rendering in a browser.
* **DOM coupling.** `#mermaid-preview-<lineId>` is Cosense's markup and can change.
  When it does, the wait times out and the viewer falls back to code — a rename of the
  selector is a one-line change in `WebKind::selector`.
* **Not covered:** `NotAuthorized` was only reached synthetically (a private page with
  no credential fails REST first, before the browser runs). A REST-PAT-with-no-sid
  session is the real case for it and was not reproducible here.

## 9. Extending to TeX / `.icon` / link coordinates

The boundary was built for this; adding a kind is four steps and touches no viewer logic:

1. **`WebKind`**: add the variant, its `selector(line_id)` and its `ready_child()`.
   Verify both against the real DOM first — the Mermaid header-vs-last-line trap is
   exactly the sort of thing that is invisible from the source.
2. **`render.rs`**: emit `Block::WebRender { kind, … }` for the notation, carrying the
   rows that should show when there is no picture. If the construct is inline rather
   than a block, the pattern generalises the same way `Block::Table` does — rows carry
   their own source lines.
3. Nothing in `view.rs` changes. The key, the worker, the cache, the staleness rules,
   the edit fallback and the drawing path are all kind-agnostic.
4. If a kind needs *data* rather than an image (link hit-boxes for mouse targeting,
   say), add a sibling to `WebArtifact` — a `Vec<u8>` PNG today, an enum tomorrow —
   and a matching `render_batch` return. `Cdp::call` already returns arbitrary JSON,
   so `getClientRects()` per link is one more `Runtime.evaluate`.

Batching is per page, so several kinds on one page still cost one navigation as long as
they are queued in the same `start_web_renders` pass.
