//! Scrapbox / Cosense REST API client (read-only).
//! Ported from api.ts. Public projects need no auth; private ones use a
//! connect.sid cookie.

use serde::Deserialize;
use std::error::Error;

#[derive(Clone, Debug)]
pub struct Config {
    pub project: String,
    pub sid: Option<String>,
    pub api_domain: String,
}

impl Config {
    fn base(&self) -> String {
        format!("https://{}/api", self.api_domain)
    }
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct Page {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub lines: Vec<PageLine>,
    #[serde(default)]
    pub links: Vec<String>,
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

#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub title: String,
    #[serde(default)]
    pub words: Vec<String>,
    #[serde(default)]
    pub lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    count: i64,
    #[serde(default)]
    pages: Vec<SearchResult>,
}

pub struct Client {
    http: reqwest::blocking::Client,
    cfg: Config,
}

impl Client {
    pub fn new(cfg: Config) -> Result<Self, Box<dyn Error>> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("cosense-tui")
            .build()?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, Box<dyn Error>> {
        let mut req = self.http.get(url).header("Accept", "application/json");
        if let Some(sid) = &self.cfg.sid {
            req = req.header("Cookie", format!("connect.sid={sid}"));
        }
        let res = req.send()?;
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
        let data: ListResponse = self.get_json(&url)?;
        Ok((data.count, data.pages))
    }

    pub fn get_page(&self, title: &str) -> Result<Page, Box<dyn Error>> {
        self.get_page_in(&self.cfg.project, title)
    }

    /// `get_page` for any project (see `list_pages_in`).
    pub fn get_page_in(&self, project: &str, title: &str) -> Result<Page, Box<dyn Error>> {
        let url = format!(
            "{}/pages/{}/{}",
            self.cfg.base(),
            urlencoding(project),
            urlencoding(title)
        );
        self.get_json(&url)
    }

    /// Project members, for resolving line author ids to display names.
    pub fn list_members(&self) -> Result<Vec<Member>, Box<dyn Error>> {
        self.list_members_in(&self.cfg.project)
    }

    /// `list_members` for any project (see `list_pages_in`).
    pub fn list_members_in(&self, project: &str) -> Result<Vec<Member>, Box<dyn Error>> {
        let url = format!(
            "{}/projects/{}/users",
            self.cfg.base(),
            urlencoding(project)
        );
        let data: MembersResponse = self.get_json(&url)?;
        Ok(match data {
            MembersResponse::Bare(v) => v,
            MembersResponse::Wrapped { users } => users,
        })
    }

    pub fn search_pages(
        &self,
        query: &str,
    ) -> Result<(i64, Vec<SearchResult>), Box<dyn Error>> {
        let url = format!(
            "{}/pages/{}/search/query?q={}",
            self.cfg.base(),
            urlencoding(&self.cfg.project),
            urlencoding(query)
        );
        let data: SearchResponse = self.get_json(&url)?;
        Ok((data.count, data.pages))
    }
}

/// Minimal percent-encoding for path/query components (RFC 3986 unreserved kept).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b;
        let unreserved = c.is_ascii_alphanumeric()
            || matches!(c, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}
