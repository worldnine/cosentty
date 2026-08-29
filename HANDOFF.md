# Cosense Web renderer — MVP handoff

`code:mmd` / `code:mermaid` / `code:<name>.mmd` blocks now display as pictures in the
TUI. The pictures are not rendered locally: headless Chrome opens the **real Cosense
page** and screenshots the element **Cosense itself drew**. This is deliberately built
as a general `request -> artifact` boundary so TeX, `.icon` rows and ProjectCSS-styled
blocks can be added later without touching the viewer.

Branch `feat/cosense-web-render`, based on `ec91fdc`.

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

struct WebRequest { kind, project, title, page_id, revision, line_id,
                    code_hash, width_px, dark }
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
struct ArtifactCache                     // ~/.cache/cosense-tui/webrender/<key>.png
```

`render_batch` takes a **whole page's worth** of requests: one navigation serves every
diagram on the page. The credential is **not** in `WebRequest` — it is constructor
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
| `rust/src/webrender.rs` | **new** — the contract above, `cache_key`, `FakeBackend`, `ArtifactCache`. |
| `rust/src/chrome.rs` | **new** — CDP client, Chrome discovery/launch/reaping, element capture, base64. |
| `rust/src/bin/web_smoke.rs` | **new** — live smoke binary (not in `cargo test`). |
| `rust/src/render.rs` | `mermaid_lang()`; `code:` blocks whose language is Mermaid emit `Block::WebRender` carrying the code plus the unchanged code rows. Everything else is byte-for-byte as before. |
| `rust/src/bin/view.rs` | `WebJob`/`WebMsg`/`spawn_web_worker`; App fields (`revision`, `web_gen`, `web_width_px`, `web_dark`, `web_pending`, `web_errors`, channels); `web_request`, `start_web_renders`, `drain_web_renders`; layout arm for `Block::WebRender`; backend construction + `shutdown()` on the quit path; `?` help entry. |
| `rust/src/api.rs` | `Page.commit_id` (`commitId`) — the render revision. |
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

## 6. Tests and build

* `cargo test`: **145 green** — lib **92** (78 baseline + 14 new) and view **53**
  (47 baseline + 6 new). No test launches a browser or touches the network.
* `cargo build --release`: succeeds, **no new warnings**.

New coverage, against the acceptance list:

| requirement | test |
| --- | --- |
| Mermaid filename/language detection | `render::mermaid_is_recognised_by_language_and_by_filename` (`mmd`, `mermaid`, `MMD`, `flow.mmd`, `図.mermaid` vs `js`, `mmdx`, `mermaidjs`, `readme.md`) |
| block → lineId, multiple blocks | `render::a_mermaid_block_becomes_one_web_render_keyed_on_its_last_line`, `render::several_mermaid_blocks_stay_separate_and_other_languages_are_untouched`, `view::each_mermaid_block_is_requested_against_its_own_cosense_line_id` |
| stale generation / page / revision rejected | `view::a_result_for_an_older_page_generation_is_dropped`, `view::a_commit_or_a_resize_makes_a_new_artifact_key`, `webrender::a_new_revision_page_or_width_is_a_different_artifact` |
| renderer failure → code fallback | `view::a_renderer_failure_leaves_the_code_block_on_screen`, `webrender::unavailable_backend_fails_every_request_without_a_browser` |
| UI thread does not block | `view::the_ui_thread_never_waits_for_the_browser` (the fake backend is pinned mid-render; the UI still queues, lays out and reports pending in <200 ms, and the artifact arrives after the gate is released) |
| selector / artifact correspondence | `webrender::selector_addresses_the_preview_by_line_id`, `webrender::fake_backend_answers_by_key`, `view::an_artifact_replaces_the_code_block_and_edit_puts_it_back` |
| credential never leaks | `webrender::cache_key_never_carries_a_credential` |
| edit contract preserved | `view::an_artifact_replaces_the_code_block_and_edit_puts_it_back` (second half) |

## 7. Live smoke results

`cargo run --bin web_smoke -- <project> <title>`; PNGs land in `$TMPDIR/cosense-web-smoke/`.

**Public, no credential needed — `https://scrapbox.io/help-jp/Mermaid`**

| line id (selector `#mermaid-preview-<id>`) | result | time |
| --- | --- | --- |
| `65695bc797c2910000c699b2` | 1332×124 PNG | |
| `65695d8097c2910000c699e8` | 1332×314 PNG | |
| `65695ac297c2910000c699a0` | 1332×788 PNG (sequence diagram, visually verified) | |
| **batch of 3, one navigation** | | **5.1 s** |

**Private, with `COSENSE_SID` — `https://scrapbox.io/my-sandbox/テスト`**

| line id | result | time |
| --- | --- | --- |
| `056f612a7f428aa9f83223e2` (flowchart) | 1374×180 PNG, visually verified | |
| `9a3c759df691479055deb208` (pie) | 1374×918 PNG | |
| `7bbaac0d02c5590651d0d957` (deliberately broken) | `diagram not drawn by Cosense` → code fallback | |
| **batch of 3** | | **10.3 s** |

Failure paths exercised live: `COSENSE_CHROME=/nonexistent/chrome` → `no Chrome found`;
private project with no credential → REST 401 before the browser is ever reached.

### What was written to the smoke page

Two commits were appended to `https://scrapbox.io/my-sandbox/テスト` (a throwaway
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

## 8. Known limits, security, distribution

* **Renders the committed page, never the buffer.** The browser shows what Cosense
  has. A diagram edited but not yet committed re-renders on the next commit (the
  revision moves) — until then the *previous* picture stands, and the block's own
  `code_hash` is in the key so an edited block re-requests rather than silently
  showing the old image under a matching key.
* **Time-machine snapshots are never rendered** (`web_request` returns `None` when
  `app.time` is set): the browser can only show the current page, and a current
  diagram on a historical snapshot would be a lie. Snapshots show code.
* **Cost.** ~4–10 s and one Chrome process per page-load/resize/commit that has an
  uncached diagram. Results are cached to `~/.cache/cosense-tui/webrender/` and keyed
  so a revisit is instant. The width is bucketed to 80 CSS px so ordinary resizes
  do not re-render.
* **Process hygiene.** The child is killed *and waited* at the end of every batch, its
  profile directory removed, and `shutdown()` on the quit path SIGKILLs a batch still
  in flight. Verified: no orphan Chrome and no leftover `cosense-tui-chrome-*` profile
  after the smoke runs.
* **Nothing from the browser is executed in-process.** Only PNG bytes cross back; no
  SVG, no HTML, no page script. The credential's only exit is `Network.setCookie`.
* **Distribution.** Chrome/Chromium/Edge/Brave must be installed. Auto-detected on
  macOS (`/Applications/…`) and via `PATH` on Linux; `COSENSE_CHROME` overrides, and an
  explicit-but-missing setting reports "no browser" rather than silently launching a
  different one. No browser → diagrams simply stay code blocks.
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

Batching is per (page, revision), so several kinds on one page still cost one
navigation as long as they are queued in the same `start_web_renders` pass.
