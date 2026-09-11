//! Scrapbox / Cosense REST API client.
//! Ported from api.ts. Public projects need no auth; private ones use the
//! official CLI's stored credentials (`cosense login`) or a connect.sid
//! cookie as a fallback.

use serde::Deserialize;
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// `send()` that takes "429 Too Many Requests" as the instruction it is:
/// wait — for `Retry-After` when the server names a time, a doubling
/// second or so otherwise — and ask again, a few times. The site rates
/// everything under one roof, so a burst of picture fetches can leave the
/// next page list refused; the reader should see the page a moment later,
/// not an error. Any other status comes back as it is.
pub trait SendPolite {
    fn send_polite(self) -> reqwest::Result<reqwest::blocking::Response>;
    fn send_edit_polite(self) -> reqwest::Result<reqwest::blocking::Response>;
}

/// How long to wait before attempt `attempt` (1-based) after a 429, given
/// the `Retry-After` header if any: the header's seconds, capped so a
/// misconfigured server cannot park the viewer; else 1s, 2s, 4s….
pub fn retry_backoff(retry_after: Option<&str>, attempt: u32) -> Duration {
    const CAP: u64 = 15;
    match retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) => Duration::from_secs(secs.clamp(1, CAP)),
        None => Duration::from_secs((1u64 << attempt.saturating_sub(1)).min(CAP)),
    }
}

const POLITE_ATTEMPTS: u32 = 4;

/// The server's rate limit applies across the viewer's concurrent requests.
/// Keep one cooldown deadline per origin so a 429 from a page request also
/// holds back an image or list request, without delaying unrelated hosts.
static RATE_LIMIT_COOLDOWNS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn rate_limit_cooldowns() -> &'static Mutex<HashMap<String, Instant>> {
    RATE_LIMIT_COOLDOWNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn rate_limit_key(req: &reqwest::blocking::RequestBuilder) -> String {
    let Some(request) = req.try_clone().and_then(|builder| builder.build().ok()) else {
        return "*".to_owned();
    };
    let url = request.url();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        port
    )
}

fn wait_for_rate_limit(key: &str) {
    loop {
        let wait = {
            let mut cooldowns = rate_limit_cooldowns()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            match cooldowns.get(key).copied() {
                Some(deadline) if deadline > now => deadline - now,
                Some(_) => {
                    cooldowns.remove(key);
                    Duration::ZERO
                }
                None => Duration::ZERO,
            }
        };
        if wait.is_zero() {
            return;
        }
        std::thread::sleep(wait);
    }
}

fn extend_rate_limit(key: &str, delay: Duration) {
    let deadline = Instant::now() + delay;
    let mut cooldowns = rate_limit_cooldowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = cooldowns.entry(key.to_owned()).or_insert(deadline);
    if *current < deadline {
        *current = deadline;
    }
}

/// How long a TCP + TLS handshake may take before a request is given up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one whole request (headers and body) may take. A page or a
/// list is a few hundred KB at most; a link that sits silently for longer
/// than this is dead for the reader's purposes, and the viewer must get
/// its keys back rather than wait on it forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl SendPolite for reqwest::blocking::RequestBuilder {
    fn send_polite(self) -> reqwest::Result<reqwest::blocking::Response> {
        send_with_attempts(self, POLITE_ATTEMPTS)
    }

    fn send_edit_polite(self) -> reqwest::Result<reqwest::blocking::Response> {
        // Keep the serial edit queue on this exact request through a burst
        // limit. In particular, retry submit with the SAME preview token.
        send_with_attempts(self, 12)
    }
}

fn send_with_attempts(
    req: reqwest::blocking::RequestBuilder,
    attempts: u32,
) -> reqwest::Result<reqwest::blocking::Response> {
    let mut req = req;
    let key = rate_limit_key(&req);
    for attempt in 1..=attempts {
        wait_for_rate_limit(&key);
        // A body that cannot be cloned (a stream) can only be sent once.
        let Some(again) = req.try_clone() else {
            let res = req.send()?;
            if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let after = res
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok());
                extend_rate_limit(&key, retry_backoff(after, attempt));
            }
            return Ok(res);
        };
        let res = req.send()?;
        if res.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(res);
        }
        let after = res
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        extend_rate_limit(&key, retry_backoff(after.as_deref(), attempt));
        if attempt == attempts {
            return Ok(res);
        }
        req = again;
    }
    unreachable!("the loop returns on its last attempt")
}

/// One way to authenticate a request, and the header that carries it.
/// PAT / Service Account are what `cosense login` stores; the sid cookie
/// is the legacy fallback (`--sid` / `COSENSE_SID`).
#[derive(Clone, Debug)]
pub enum Credential {
    Pat(String),
    ServiceAccount(String),
    Sid(String),
}

impl Credential {
    /// `(header name, header value)` for this credential.
    pub fn header(&self) -> (&'static str, String) {
        match self {
            Credential::Pat(t) => ("x-personal-access-token", t.clone()),
            Credential::ServiceAccount(k) => ("x-service-account-access-key", k.clone()),
            Credential::Sid(s) => ("Cookie", format!("connect.sid={s}")),
        }
    }

    /// Short human tag for the status line ("pat" / "sa" / "sid").
    pub fn kind(&self) -> &'static str {
        match self {
            Credential::Pat(_) => "pat",
            Credential::ServiceAccount(_) => "sa",
            Credential::Sid(_) => "sid",
        }
    }
}

/// Credentials from the official CLI's `~/.cosense/settings.json`, resolved
/// with the CLI's own precedence (see its `settings.ts`):
///   1. `COSENSE_PAT` env var
///   2. `projects[]` service account matching (origin, projectNameLc)
///   3. `users[]` personal access token matching origin
///   4. the sid cookie fallback (ours; the CLI has no such concept)
#[derive(Clone, Debug, Default)]
pub struct AuthStore {
    env_pat: Option<String>,
    /// (origin, projectNameLc, serviceAccount)
    projects: Vec<(String, String, String)>,
    /// (origin, token)
    users: Vec<(String, String)>,
    sid: Option<String>,
}

impl AuthStore {
    /// Load `~/.cosense/settings.json` (missing or malformed → empty store)
    /// plus the env fallbacks. `sid` comes from the caller (`--sid` /
    /// `COSENSE_SID`).
    pub fn load(sid: Option<String>) -> Self {
        let env_pat = std::env::var("COSENSE_PAT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let mut store = AuthStore {
            env_pat,
            sid,
            ..Default::default()
        };
        let Some(home) = std::env::var_os("HOME") else {
            return store;
        };
        let path = std::path::PathBuf::from(home)
            .join(".cosense")
            .join("settings.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return store;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return store;
        };
        // origin of an URL string: scheme://host[:port] — lenient, like the
        // CLI it only needs to match what the CLI itself wrote.
        fn origin_of(url: &str) -> Option<String> {
            let (scheme, rest) = url.split_once("://")?;
            let host = rest.split('/').next()?;
            if host.is_empty() {
                return None;
            }
            Some(format!("{scheme}://{host}"))
        }
        if let Some(projects) = v.get("projects").and_then(|p| p.as_array()) {
            for p in projects {
                let (Some(url), Some(sa)) = (
                    p.get("url").and_then(|s| s.as_str()),
                    p.get("serviceAccount").and_then(|s| s.as_str()),
                ) else {
                    continue;
                };
                let Some(origin) = origin_of(url) else {
                    continue;
                };
                let name = url
                    .split("://")
                    .nth(1)
                    .and_then(|r| r.split('/').nth(1))
                    .unwrap_or("");
                if !name.is_empty() && !sa.trim().is_empty() {
                    store
                        .projects
                        .push((origin, name.to_lowercase(), sa.to_string()));
                }
            }
        }
        if let Some(users) = v.get("users").and_then(|u| u.as_array()) {
            for u in users {
                let (Some(url), Some(token)) = (
                    u.get("url").and_then(|s| s.as_str()),
                    u.get("token").and_then(|s| s.as_str()),
                ) else {
                    continue;
                };
                if let Some(origin) = origin_of(url) {
                    if !token.trim().is_empty() {
                        store.users.push((origin, token.to_string()));
                    }
                }
            }
        }
        store
    }

    /// The credential for (origin, project), CLI precedence + sid fallback.
    pub fn resolve(&self, origin: &str, project: &str) -> Option<Credential> {
        if let Some(pat) = &self.env_pat {
            return Some(Credential::Pat(pat.clone()));
        }
        let plc = project.to_lowercase();
        for (o, p, sa) in &self.projects {
            if o == origin && *p == plc {
                return Some(Credential::ServiceAccount(sa.clone()));
            }
        }
        for (o, token) in &self.users {
            if o == origin {
                return Some(Credential::Pat(token.clone()));
            }
        }
        self.sid.clone().map(Credential::Sid)
    }

    /// The user-level credential for an origin (no project → no service
    /// account), for endpoints that are not project-scoped (file downloads).
    pub fn resolve_user(&self, origin: &str) -> Option<Credential> {
        if let Some(pat) = &self.env_pat {
            return Some(Credential::Pat(pat.clone()));
        }
        for (o, token) in &self.users {
            if o == origin {
                return Some(Credential::Pat(token.clone()));
            }
        }
        self.sid.clone().map(Credential::Sid)
    }

    /// The sid cookie this store knows (`--sid` / `COSENSE_SID`), REGARDLESS
    /// of which credential actually resolves for a project. REST keeps the
    /// normal precedence (env PAT → service account → users PAT → sid), but
    /// the websocket push channel authenticates with the sid even when REST
    /// uses a PAT — the two transports do not see eye to eye (Note:
    /// NOTE-websocket-sync.md).
    pub fn sid(&self) -> Option<&str> {
        self.sid.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub project: String,
    pub auth: AuthStore,
    pub api_domain: String,
}

impl Config {
    fn base(&self) -> String {
        format!("https://{}/api", self.api_domain)
    }
    fn origin(&self) -> String {
        format!("https://{}", self.api_domain)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PageSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub descriptions: Vec<String>,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub created: i64,
    /// When this page was last OPENED by anyone in the project (not the
    /// requesting user — that is `Page::last_accessed`, which the list
    /// endpoint does not carry). It is what `sort=accessed` orders by.
    #[serde(default)]
    pub accessed: i64,
    #[serde(default)]
    pub views: i64,
    #[serde(default)]
    pub linked: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PageLine {
    #[serde(default)]
    pub id: String,
    pub text: String,
    /// Who last touched this line, and when. Cosense tracks these per line,
    /// so a git-blame style view needs no history API at all.
    #[serde(default, rename = "userId")]
    pub user_id: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub updated: i64,
}

/// A project member (for resolving `PageLine.user_id` to a name).
#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "displayName")]
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MembersResponse {
    Bare(Vec<Member>),
    Wrapped { users: Vec<Member> },
}

/// One entry of the related-pages list (1-hop or 2-hop). `links_lc` is the
/// lowercased links ON that page — for a 2-hop entry it tells which of the
/// current page's links it shares (the "hub" the browser groups it under).
#[derive(Debug, Clone, Deserialize)]
pub struct RelatedPage {
    pub id: String,
    pub title: String,
    #[serde(default, rename = "titleLc")]
    pub title_lc: String,
    #[serde(default)]
    pub descriptions: Vec<String>,
    #[serde(default, rename = "linksLc")]
    pub links_lc: Vec<String>,
    #[serde(default)]
    pub linked: i64,
    #[serde(default)]
    pub updated: i64,
    /// Last opened by anyone in the project (epoch seconds). The related
    /// block carries these two stamps as the list endpoint does, which is
    /// what lets the viewer impose the list's sort order on it. `views` it
    /// does NOT carry — see `index::SortKey` for what that order falls
    /// back to here.
    #[serde(default)]
    pub accessed: i64,
    #[serde(default)]
    pub created: i64,
}

/// `/api/projects/<name>`: the settings the viewer reads off it. Fields
/// absent from the response stay `None`; the site's `uploadImageTo`
/// vocabulary is `gcs` / `gyazo`, and `gyazoTeamsName` is `null` for
/// personal gyazo.com.
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
pub struct ProjectSettings {
    /// The project's proper name (the site header's), as opposed to the
    /// URL slug. Empty when the response has none.
    #[serde(default, rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default, rename = "uploadImageTo")]
    pub upload_image_to: Option<String>,
    #[serde(default, rename = "gyazoTeamsName")]
    pub gyazo_teams_name: Option<String>,
}

/// One row of `/api/projects`: a project the credential is a member of.
/// Timestamps are epoch seconds, as everywhere else in the API.
#[derive(Debug, Default, Clone, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    /// The URL slug.
    pub name: String,
    /// The proper name (the site header's). Empty when the API has none.
    #[serde(default, rename = "displayName")]
    pub display_name: String,
    #[serde(default, rename = "publicVisible")]
    pub public_visible: bool,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub created: i64,
    #[serde(default, rename = "usersCount")]
    pub users_count: i64,
}

/// `relatedPages` of a page response. 1-hop = direct links + backlinks
/// (existing pages only), 2-hop = pages sharing a link target with this
/// page. Cross-project ("External links") entries are NOT here: the API's
/// `projectLinks1hop` is empty in practice, so the viewer builds that
/// section from the page-level `projectLinks` instead.
#[derive(Debug, Default, Deserialize)]
pub struct RelatedPages {
    #[serde(default)]
    pub links1hop: Vec<RelatedPage>,
    #[serde(default)]
    pub links2hop: Vec<RelatedPage>,
    #[serde(default, rename = "hasBackLinksOrIcons")]
    pub has_back_links_or_icons: bool,
}

#[derive(Debug, Deserialize)]
pub struct Page {
    /// Empty for a page that does not exist yet: Cosense answers 200 for
    /// any title, with `persistent: false` and no id. That is how the web
    /// opens a link to an uncreated page, and how this viewer does too.
    #[serde(default)]
    pub id: String,
    /// Does this page exist on the server? `false` = a template the first
    /// commit will create.
    #[serde(default)]
    pub persistent: bool,
    pub title: String,
    /// The page's current commit. Reported by the `web_smoke` binary as
    /// page metadata. NOT part of any diagram's cache key: Cosense commits
    /// on every keystroke-level edit, so keying on it re-rendered every
    /// diagram on a page whenever any line was touched (see NOTE-webrender-handoff.md §2).
    #[serde(default, rename = "commitId")]
    pub commit_id: String,
    #[serde(default)]
    pub lines: Vec<PageLine>,
    #[serde(default)]
    pub links: Vec<String>,
    /// Outgoing cross-project links (`[/project/title]`), as `/project/title`.
    #[serde(default, rename = "projectLinks")]
    pub project_links: Vec<String>,
    /// Related-pages list as scrapbox.io computes it (see `RelatedPages`).
    ///
    /// `None` for anything read through `get_page_in`: the v2 page endpoint
    /// does not carry this block. Fill it in from `get_related_in` (that is
    /// what `sync::merge_related` does) — `Option` is the "not fetched yet"
    /// state, and code that colors links by it must treat `None` as "don't
    /// know", never as "no backlinks".
    #[serde(default, rename = "relatedPages")]
    pub related: Option<RelatedPages>,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub created: i64,
    #[serde(default, rename = "linesCount")]
    pub lines_count: i64,
    /// When the REQUESTING user last opened this page in the browser (epoch
    /// seconds); absent for a page they have never opened. Plain API reads
    /// do not bump it (verified), so it is a stable "last seen" mark.
    #[serde(default, rename = "lastAccessed")]
    pub last_accessed: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    #[serde(default)]
    count: i64,
    #[serde(default)]
    pages: Vec<PageSummary>,
}

/// One hit of the full-text search. Carries the same page metadata the
/// list endpoint does (verified against a live reply) EXCEPT `accessed`,
/// plus the two things only a search can say: the `words` it matched and
/// the `lines` they were found on.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    pub title: String,
    #[serde(default)]
    pub words: Vec<String>,
    /// The matched lines, in page order — a better excerpt than a page's
    /// opening lines, because it is the part that answered the question.
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub views: i64,
    #[serde(default)]
    pub linked: i64,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    count: i64,
    /// How many hits this reply was allowed to carry (100 in practice).
    /// `count` is capped at it too — measured: villagepump/"Scrapbox"
    /// answers `count: 100, limit: 100` with 100 pages, and there are
    /// certainly more. So a full page of hits means "at least this many",
    /// never "exactly this many".
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    pages: Vec<SearchResult>,
}

/// One page edit operation, in the official edit API's vocabulary
/// (`page-edit-for-ai`). Anchors and targets are LINE IDS — never line
/// numbers — which is what makes concurrent-edit rebasing tractable.
///
/// Inserted lines carry CLIENT-GENERATED ids (the API accepts them, same
/// as the official CLI). That makes the ids known the moment an op is
/// built — the local page model can apply the op immediately and keep
/// editing without waiting for (or reloading from) the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    /// Insert lines before `anchor` (`"_end"` = append). `lines` are
    /// `(id, text)` pairs in order; build them with [`EditOp::insert`].
    Insert {
        anchor: String,
        lines: Vec<(String, String)>,
    },
    /// Replace the body of line `id` (single line only).
    Replace { id: String, text: String },
    /// Delete line `id`.
    Delete { id: String },
}

impl EditOp {
    /// An Insert op for `text` (embedded `\n` = several lines), with fresh
    /// client-generated line ids.
    pub fn insert(anchor: impl Into<String>, text: &str) -> Self {
        EditOp::Insert {
            anchor: anchor.into(),
            lines: text
                .split('\n')
                .map(|t| (new_line_id(), t.to_string()))
                .collect(),
        }
    }
}

/// A successful dry-run: what the page will look like, and the one-shot
/// token that commits it (5-minute expiry, consume-on-submit).
#[derive(Debug)]
pub struct EditPreview {
    pub preview_id: String,
    pub expire_at: String,
    pub title: String,
    /// Whole page after applying the ops.
    pub lines: Vec<PreviewLine>,
    /// Ids of lines the ops inserted (generated client-side).
    pub new_ids: Vec<String>,
    /// Ids of lines the ops replaced.
    pub updated_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PreviewLine {
    pub id: String,
    pub text: String,
}

/// What a submit actually did.
#[derive(Debug)]
pub struct EditCommit {
    pub commit_id: String,
    /// Title as written (the server may auto-suffix on a duplicate).
    pub title: String,
}

/// Why an edit failed, separated where the caller reacts differently.
#[derive(Debug)]
pub enum EditError {
    /// 409 NotFastForward: the page changed after the preview — refetch
    /// and rebuild the ops.
    NotFastForward,
    /// 409 DuplicateTitle: another page took the title between preview
    /// and submit.
    DuplicateTitle,
    /// 404 on submit: expired (5 min), already consumed, or not yours.
    PreviewGone,
    Other(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NotFastForward => write!(f, "page changed since preview (NotFastForward)"),
            EditError::DuplicateTitle => write!(f, "another page took this title (DuplicateTitle)"),
            EditError::PreviewGone => write!(f, "preview expired or already used"),
            EditError::Other(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for EditError {}

/// A fresh 24-hex line id, like the CLI's `randomBytes(12).toString('hex')`.
pub fn new_line_id() -> String {
    let mut bytes = [0u8; 12];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        // Fallback: time-derived (uniqueness only matters within one page).
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..12].copy_from_slice(&t.to_le_bytes()[..12]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::blocking::Client,
    cfg: Config,
}

impl Client {
    pub fn new(cfg: Config) -> Result<Self, Box<dyn Error>> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("cosentty")
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// The credential used for requests in `project` (status-line display).
    pub fn credential_for(&self, project: &str) -> Option<Credential> {
        self.cfg.auth.resolve(&self.cfg.origin(), project)
    }

    /// GET `url` as JSON, authenticated for `project` (credentials are
    /// per-project: a service account is bound to one project, and the
    /// viewer hops projects via `[/project/title]` links).
    fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        project: &str,
    ) -> Result<T, Box<dyn Error>> {
        let mut req = self.http.get(url).header("Accept", "application/json");
        if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        Ok(res.json::<T>()?)
    }

    /// List pages. sort: updated|created|accessed|linked|views|title
    pub fn list_pages(
        &self,
        limit: u32,
        skip: u32,
        sort: &str,
    ) -> Result<(i64, Vec<PageSummary>), Box<dyn Error>> {
        self.list_pages_in(&self.cfg.project, limit, skip, sort)
    }

    /// `list_pages` for any project the session's SID can read (cross-project
    /// links `[/project/title]` land the viewer in other projects).
    pub fn list_pages_in(
        &self,
        project: &str,
        limit: u32,
        skip: u32,
        sort: &str,
    ) -> Result<(i64, Vec<PageSummary>), Box<dyn Error>> {
        let url = format!(
            "{}/pages/{}?limit={}&skip={}&sort={}",
            self.cfg.base(),
            urlencoding(project),
            limit,
            skip,
            sort
        );
        let data: ListResponse = self.get_json(&url, project)?;
        Ok((data.count, data.pages))
    }

    pub fn get_page(&self, title: &str) -> Result<Page, Box<dyn Error>> {
        self.get_page_in(&self.cfg.project, title)
    }

    /// `get_page` for any project (see `list_pages_in`).
    ///
    /// Reads through **v2** (`/api/pages/v2/<project>/<title>`), the same
    /// generation the edit API lives on. v2 answers with the identical page
    /// body as v1 — same fields, same `lines`, same `persistent: false`
    /// template for a title nobody has written yet (measured; both even mint
    /// the same kind of throwaway id) — with ONE difference that matters:
    /// **it omits `relatedPages`**, which is most of v1's payload (井戸端 on
    /// villagepump: 13.7 KB against 364 KB). So `Page::related` is always
    /// `None` here and the related list is a second, separate request
    /// (`get_related_in`), which is what lets the body paint before the
    /// backlinks are in. PAT reads v2 on a private project fine (verified).
    pub fn get_page_in(&self, project: &str, title: &str) -> Result<Page, Box<dyn Error>> {
        let url = format!(
            "{}/pages/v2/{}/{}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(title)
        );
        self.get_json(&url, project)
    }

    /// The related-pages block for `project/title` — the half of the page
    /// response v2 does not carry (see `get_page_in`).
    ///
    /// Still asked of the **v1** page endpoint, which is the only thing that
    /// reports it; the body that comes back with it is thrown away. A title
    /// nothing points at answers 404, and that is an ANSWER (no back links),
    /// so it degrades to an empty block rather than an error — same
    /// convention as `backlink_ids`.
    pub fn get_related_in(
        &self,
        project: &str,
        title: &str,
    ) -> Result<RelatedPages, Box<dyn Error>> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, rename = "relatedPages")]
            related: RelatedPages,
        }
        let url = format!(
            "{}/pages/{}/{}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(title)
        );
        let mut req = self.http.get(&url).header("Accept", "application/json");
        if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(RelatedPages::default());
        }
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        Ok(res.json::<Wrapper>()?.related)
    }

    /// Has anyone written `project/title`?
    ///
    /// Asked as a HEAD on the page's `/text`, which is the cheapest
    /// question the API answers and the only one that splits the three
    /// cases the right way. The page endpoint does not: it returns 200 for
    /// a title that merely has links pointing at it (with
    /// `persistent: false`) and 404 only for a title nobody has ever
    /// mentioned — and it ships the whole page to say so. `/text` calls
    /// both of those 404, which is the answer wanted here, and HEAD makes
    /// it a header exchange: measured on villagepump's `井戸端`, 365 KB
    /// and 0.69 s as a page GET against 0 bytes and 0.22 s this way.
    ///
    /// A 404 is therefore an ANSWER, not a failure. Any other non-success
    /// (403 on a project this credential cannot read, a 5xx) is an error,
    /// and the caller is expected to keep saying nothing rather than
    /// guess.
    pub fn page_exists(&self, project: &str, title: &str) -> Result<bool, Box<dyn Error>> {
        let url = format!(
            "{}/pages/{}/{}/text",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(title)
        );
        let mut req = self.http.head(&url);
        if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        Ok(true)
    }

    /// The ids of the pages that link to `project/title`.
    ///
    /// For a title nobody has written, the v1 page endpoint still answers
    /// with its back links (`persistent: false` and a `relatedPages`
    /// block), which is how "is this word used anywhere else?" is
    /// answered without an index of the whole project. A title nothing
    /// points at answers 404 — no back links, and no error.
    pub fn backlink_ids(&self, project: &str, title: &str) -> Result<Vec<String>, Box<dyn Error>> {
        let related = self.get_related_in(project, title)?;
        Ok(related.links1hop.into_iter().map(|p| p.id).collect())
    }

    /// Project members, for resolving line author ids to display names.
    pub fn list_members(&self) -> Result<Vec<Member>, Box<dyn Error>> {
        self.list_members_in(&self.cfg.project)
    }

    /// The authenticated user's id. Used to distinguish a project member
    /// (can edit) from an authenticated visitor of a public project.
    pub fn get_me(&self) -> Result<String, Box<dyn Error>> {
        let url = format!("{}/users/me", self.cfg.base());
        let mut req = self.http.get(&url).header("Accept", "application/json");
        if let Some(cred) = self.cfg.auth.resolve_user(&self.cfg.origin()) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        let value: serde_json::Value = res.json()?;
        value
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "users/me: no id in response".into())
    }

    /// The sid cookie for the websocket push channel, when the session has
    /// one (`--sid` / `COSENSE_SID`) — independent of the project credential
    /// (which may well be a PAT).
    pub fn sid(&self) -> Option<&str> {
        self.cfg.auth.sid()
    }

    /// The HTTP client, for requests to hosts other than Cosense that want
    /// to share its pool (Gyazo uploads).
    pub fn http(&self) -> &reqwest::blocking::Client {
        &self.http
    }

    /// The project's selected Cosense site theme (`blue`, `paper-dark`, …).
    /// See `get_project_settings`, of which this is the one field.
    pub fn get_project_theme(&self, project: &str) -> Result<String, Box<dyn Error>> {
        self.get_project_settings(project)?
            .theme
            .ok_or_else(|| "projects/<name>: no theme in response".into())
    }

    /// `/api/projects`: the projects this credential belongs to, in the
    /// order the server hands them over (NOT by date — the caller sorts).
    ///
    /// A user-level endpoint, so it takes the user credential (PAT / sid),
    /// never a service account: an SA is one project's key and has no
    /// membership list to answer with. Projects the user merely CAN read
    /// (public ones they are not in) are not listed.
    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>, Box<dyn Error>> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            projects: Vec<ProjectSummary>,
        }
        let url = format!("{}/projects", self.cfg.base());
        let mut req = self.http.get(&url).header("Accept", "application/json");
        if let Some(cred) = self.cfg.auth.resolve_user(&self.cfg.origin()) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        Ok(res.json::<Wrapper>()?.projects)
    }

    /// The project's settings page as `/api/projects/<name>` reports it:
    /// the site theme and the Upload tab (where the browser sends pasted
    /// images). The endpoint is public for public projects but refuses PAT
    /// on private ones; when available, the browser `connect.sid` is
    /// therefore used for this lookup and no other REST call. Page/edit
    /// calls keep their normal PAT / service-account precedence.
    pub fn get_project_settings(&self, project: &str) -> Result<ProjectSettings, Box<dyn Error>> {
        let url = format!("{}/projects/{}", self.cfg.base(), urlencoding(project));
        let mut req = self.http.get(&url).header("Accept", "application/json");
        let mut authenticated = false;
        if let Some(sid) = self.sid() {
            req = req.header("Cookie", format!("connect.sid={sid}"));
            authenticated = true;
        } else if let Some(cred @ Credential::ServiceAccount(_)) =
            self.cfg.auth.resolve(&self.cfg.origin(), project)
        {
            let (name, value) = cred.header();
            req = req.header(name, value);
            authenticated = true;
        }
        let mut res = req.send_polite()?;
        // A stale sid must not hide a public project's appearance: retry the
        // public endpoint without credentials before falling back in the UI.
        if !res.status().is_success() && authenticated {
            res = self
                .http
                .get(&url)
                .header("Accept", "application/json")
                .send_polite()?;
        }
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        Ok(res.json::<ProjectSettings>()?)
    }

    /// Upload a file to the project's own storage and return the URL to
    /// embed. The official CLI's `uploadFile` route, three requests:
    /// `upload-request` (a file already there answers with `embedUrl` here
    /// and the rest is skipped) → `PUT` to the signed URL (Content-Type
    /// only; NO credential — it is a Google-signed URL) → `verify`.
    /// Needs the project id, which `/api/projects/<name>/users` gives a PAT.
    pub fn upload_gcs(
        &self,
        project: &str,
        bytes: &[u8],
        name: &str,
        content_type: &str,
    ) -> Result<String, Box<dyn Error>> {
        #[derive(Deserialize)]
        struct Requested {
            #[serde(default, rename = "embedUrl")]
            embed_url: Option<String>,
            #[serde(default, rename = "signedUrl")]
            signed_url: Option<String>,
            #[serde(default, rename = "fileId")]
            file_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct Verified {
            #[serde(default, rename = "embedUrl")]
            embed_url: Option<String>,
        }
        let project_id = self.get_project_id(project)?;
        let md5 = crate::upload::md5_hex(bytes);
        let auth = |mut req: reqwest::blocking::RequestBuilder| {
            if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
                let (name, value) = cred.header();
                req = req.header(name, value);
            }
            req.header("Accept", "application/json")
        };
        let url = format!("{}/gcs/{project_id}/upload-request", self.cfg.base());
        let res = auth(self.http.post(&url))
            .json(&serde_json::json!({
                "md5": md5, "size": bytes.len(), "contentType": content_type, "name": name,
            }))
            .send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for upload-request", res.status()).into());
        }
        let req: Requested = res.json()?;
        if let Some(u) = req.embed_url {
            return Ok(u);
        }
        let (Some(signed), Some(file_id)) = (req.signed_url, req.file_id) else {
            return Err("upload-request: neither embedUrl nor signedUrl".into());
        };
        let put = self
            .http
            .put(&signed)
            .header("Content-Type", content_type)
            .body(bytes.to_vec())
            .send_polite()?;
        if !put.status().is_success() {
            let hint = if put.status().as_u16() == 403 {
                " (retrying a little later often works)"
            } else {
                ""
            };
            return Err(format!("HTTP {} for signed PUT{hint}", put.status()).into());
        }
        let url = format!("{}/gcs/{project_id}/verify", self.cfg.base());
        let res = auth(self.http.post(&url))
            .json(&serde_json::json!({ "md5": md5, "fileId": file_id }))
            .send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for verify", res.status()).into());
        }
        res.json::<Verified>()?
            .embed_url
            .ok_or_else(|| "verify: no embedUrl in response".into())
    }

    /// Whether the project can be read with no credential at all.
    ///
    /// Deliberately ANONYMOUS: no cookie, no PAT, no service account. The
    /// question is "what would a browser with no session see", so attaching
    /// one of our credentials would answer a different question — and would
    /// hand a credential to a call that has no need of it.
    ///
    /// Measured against the live API: 200 on a public project, 401 on a
    /// private one, 404 for a name that is not there. 404 and transport
    /// failures both stay `Unknown`, which every caller treats as "do not
    /// assume".
    pub fn probe_visibility(&self, project: &str) -> crate::capability::Visibility {
        let url = format!("{}/projects/{}", self.cfg.base(), urlencoding(project));
        let status = self
            .http
            .get(&url)
            .header("Accept", "application/json")
            .send_polite()
            .ok()
            .map(|r| r.status().as_u16());
        crate::capability::Visibility::from_anonymous_status(status)
    }

    /// The project's immutable id, for the websocket room (the page API's
    /// `projectId`). Read from `/api/projects/<name>/users` — the same
    /// endpoint the CLI uses, because `/api/projects/<name>` itself refuses
    /// PAT (401, verified).
    pub fn get_project_id(&self, project: &str) -> Result<String, Box<dyn Error>> {
        let url = format!(
            "{}/projects/{}/users",
            self.cfg.base(),
            urlencoding(project)
        );
        let mut req = self.http.get(&url).header("Accept", "application/json");
        if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req.send_polite()?;
        if !res.status().is_success() {
            return Err(format!("HTTP {} for {}", res.status(), url).into());
        }
        let v: serde_json::Value = res.json()?;
        v.get("projectId")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .ok_or_else(|| "projects/<name>/users: no projectId in response".into())
    }

    /// `list_members` for any project (see `list_pages_in`).
    pub fn list_members_in(&self, project: &str) -> Result<Vec<Member>, Box<dyn Error>> {
        let url = format!(
            "{}/projects/{}/users",
            self.cfg.base(),
            urlencoding(project)
        );
        let data: MembersResponse = self.get_json(&url, project)?;
        Ok(match data {
            MembersResponse::Bare(v) => v,
            MembersResponse::Wrapped { users } => users,
        })
    }

    pub fn search_pages(
        &self,
        query: &str,
    ) -> Result<(i64, bool, Vec<SearchResult>), Box<dyn Error>> {
        let project = self.cfg.project.clone();
        self.search_pages_in(&project, query)
    }

    /// `search_pages` for any project — the index may be listing one this
    /// page is not in (a `[/other-project]` link opens its site top).
    ///
    /// Returns `(count, capped, pages)`. `capped` says the reply hit the
    /// endpoint's limit, so `count` is a floor and not the total (see
    /// `SearchResponse::limit`); a caller showing the number has to say so.
    pub fn search_pages_in(
        &self,
        project: &str,
        query: &str,
    ) -> Result<(i64, bool, Vec<SearchResult>), Box<dyn Error>> {
        let url = format!(
            "{}/pages/{}/search/query?q={}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(query)
        );
        let data: SearchResponse = self.get_json(&url, project)?;
        let capped = data.limit > 0 && data.pages.len() as i64 >= data.limit;
        Ok((data.count, capped, data.pages))
    }
}

/// One point on a page's server-side history: Cosense snapshots pages
/// periodically while they are edited and keeps ALL of them (Page history).
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotStamp {
    pub id: String,
    #[serde(default)]
    pub created: i64,
}

#[derive(Debug, Deserialize)]
struct SnapshotListResponse {
    #[serde(default)]
    timestamps: Vec<SnapshotStamp>,
}

/// A full historical version of a page. Lines carry the same per-line
/// author/time metadata as the live page, so blame works in the past too.
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub lines: Vec<PageLine>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created: i64,
}

#[derive(Debug, Deserialize)]
struct SnapshotResponse {
    snapshot: Snapshot,
}

impl Client {
    /// All snapshot timestamps of a page, sorted OLDEST → NEWEST (the API
    /// returns newest first). Member-only on private projects.
    pub fn list_snapshots(
        &self,
        project: &str,
        page_id: &str,
    ) -> Result<Vec<SnapshotStamp>, Box<dyn Error>> {
        let url = format!(
            "{}/page-snapshots/{}/{}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(page_id)
        );
        let mut data: SnapshotListResponse = self.get_json(&url, project)?;
        data.timestamps.sort_by_key(|t| t.created);
        Ok(data.timestamps)
    }

    /// One historical version (see `list_snapshots` for the ids).
    pub fn get_snapshot(
        &self,
        project: &str,
        page_id: &str,
        timestamp_id: &str,
    ) -> Result<Snapshot, Box<dyn Error>> {
        let url = format!(
            "{}/page-snapshots/{}/{}/{}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(page_id),
            urlencoding(timestamp_id)
        );
        let data: SnapshotResponse = self.get_json(&url, project)?;
        Ok(data.snapshot)
    }

    /// POST JSON with per-project auth, classifying the edit-API errors.
    fn post_edit_json(
        &self,
        project: &str,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, EditError> {
        let mut req = self
            .http
            .post(url)
            .header("Accept", "application/json")
            .json(body);
        if let Some(cred) = self.cfg.auth.resolve(&self.cfg.origin(), project) {
            let (name, value) = cred.header();
            req = req.header(name, value);
        }
        let res = req
            .send_edit_polite()
            .map_err(|e| EditError::Other(e.to_string()))?;
        let status = res.status();
        let text = res.text().map_err(|e| EditError::Other(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&text).map_err(|e| EditError::Other(e.to_string()));
        }
        let code = status.as_u16();
        let api_err = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)));
        match (code, api_err.as_deref()) {
            (409, Some("NotFastForward")) => Err(EditError::NotFastForward),
            (409, Some("DuplicateTitle")) => Err(EditError::DuplicateTitle),
            (404, _) if url.ends_with("/submit") => Err(EditError::PreviewGone),
            _ => Err(EditError::Other(format!(
                "HTTP {code}: {}",
                text.chars().take(200).collect::<String>()
            ))),
        }
    }

    /// Dry-run `ops` against page `page_id` in `project` (both from the
    /// page API). Nothing is written; the returned preview commits via
    /// `submit_edit`. A changed page fails with `NotFastForward` here
    /// already, not only at submit.
    /// Dry-run an edit. An EMPTY `page_id` means "this page does not exist
    /// yet": the request goes out without `pageId`, which is how the API
    /// creates a page (the first inserted line becomes its title). Same
    /// shape the official CLI uses for `previewEdit --new`.
    pub fn preview_edit(
        &self,
        project: &str,
        page_id: &str,
        ops: &[EditOp],
    ) -> Result<EditPreview, EditError> {
        let mut changes: Vec<serde_json::Value> = Vec::new();
        let mut new_ids: Vec<String> = Vec::new();
        let mut updated_ids: Vec<String> = Vec::new();
        for op in ops {
            match op {
                EditOp::Insert { anchor, lines } => {
                    for (id, line) in lines {
                        changes.push(serde_json::json!({
                            "_insert": anchor,
                            "lines": { "id": id, "text": line },
                        }));
                        new_ids.push(id.clone());
                    }
                }
                EditOp::Replace { id, text } => {
                    changes.push(serde_json::json!({
                        "_update": id,
                        "lines": { "text": text },
                    }));
                    updated_ids.push(id.clone());
                }
                EditOp::Delete { id } => {
                    changes.push(serde_json::json!({ "_delete": id }));
                }
            }
        }
        let url = format!(
            "{}/pages/v2/{}/page-edit-for-ai/preview",
            self.cfg.base(),
            urlencoding(project)
        );
        let body = if page_id.is_empty() {
            serde_json::json!({ "changes": changes })
        } else {
            serde_json::json!({ "pageId": page_id, "changes": changes })
        };
        let v = self.post_edit_json(project, &url, &body)?;
        let preview_id = v
            .get("previewId")
            .and_then(|s| s.as_str())
            .ok_or_else(|| EditError::Other("preview response missing previewId".into()))?
            .to_string();
        let expire_at = v
            .get("expireAt")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let page = v.get("pagePreview");
        let title = page
            .and_then(|p| p.get("title"))
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let lines: Vec<PreviewLine> = page
            .and_then(|p| p.get("lines"))
            .and_then(|l| serde_json::from_value(l.clone()).ok())
            .unwrap_or_default();
        Ok(EditPreview {
            preview_id,
            expire_at,
            title,
            lines,
            new_ids,
            updated_ids,
        })
    }

    /// Commit a preview. One-shot: success or failure, the previewId is
    /// spent.
    pub fn submit_edit(&self, project: &str, preview_id: &str) -> Result<EditCommit, EditError> {
        let url = format!(
            "{}/pages/v2/{}/page-edit-for-ai/submit",
            self.cfg.base(),
            urlencoding(project)
        );
        let body = serde_json::json!({ "previewId": preview_id });
        let v = self.post_edit_json(project, &url, &body)?;
        let commit_id = v
            .get("commitId")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let title = v
            .get("page")
            .and_then(|p| p.get("title"))
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(EditCommit { commit_id, title })
    }
}

use crate::url::encode_component as urlencoding;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_429_waits_for_retry_after_or_doubles() {
        assert_eq!(retry_backoff(Some("3"), 1), Duration::from_secs(3));
        assert_eq!(
            retry_backoff(Some("999"), 1),
            Duration::from_secs(15),
            "capped"
        );
        assert_eq!(
            retry_backoff(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 2),
            Duration::from_secs(2),
            "a date is not parsed: fall back"
        );
        assert_eq!(retry_backoff(None, 1), Duration::from_secs(1));
        assert_eq!(retry_backoff(None, 3), Duration::from_secs(4));
    }

    #[test]
    fn a_shared_cooldown_keeps_the_longest_deadline() {
        let key = "test://shared-cooldown";
        extend_rate_limit(key, Duration::from_millis(30));
        let first = rate_limit_cooldowns()
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap();
        extend_rate_limit(key, Duration::from_millis(1));
        let second = rate_limit_cooldowns()
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap();
        assert!(second >= first, "a shorter 429 must not shorten the wait");
        wait_for_rate_limit(key);
    }
}
