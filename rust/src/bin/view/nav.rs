use super::*;

/// One section of the related-pages list rendered below the page body
/// (scrapbox.io's "Links" / per-hub 2-hop groups / "External links").
pub(crate) struct RelSection {
    pub(crate) heading: String,
    pub(crate) entries: Vec<RelEntry>,
}

/// One related page: where Enter goes, and what the row shows.
pub(crate) struct RelEntry {
    pub(crate) item: LinkItem,
    pub(crate) title: String,
    /// First description line (the page's own first body line), dimmed.
    pub(crate) desc: String,
    /// `updated` epoch seconds (0 = unknown, no age shown). The telomere
    /// reads this whatever the sort order, as the index's does.
    pub(crate) age: i64,
    /// The other two stamps and the count the related block reports, so
    /// the row can show the value it is being sorted ON
    /// (`sort_meta`). 0 = the API did not say.
    pub(crate) accessed: i64,
    pub(crate) created: i64,
    pub(crate) linked: i64,
    /// Two-state telomere for related rows. RelatedPages has no per-user
    /// lastAccessed, so this is based on this TUI's persisted visit record.
    pub(crate) unread: bool,
}

impl RelEntry {
    /// The dim `· …` after the title: the value the section is sorted on,
    /// on the index's principle that sorting by a number the reader cannot
    /// see is no help. Time orders show that stamp's age; `linked` shows
    /// the count; `title` and `views` (which the related block does not
    /// report) fall back to the updated age the row always showed. Empty
    /// when the API did not fill the field.
    pub(crate) fn sort_meta(&self, sort: cosense::index::SortKey) -> String {
        use cosense::index::{relative_age, SortKey};
        match sort {
            SortKey::Accessed => relative_age(self.accessed),
            SortKey::Created => relative_age(self.created),
            SortKey::Linked if self.age > 0 => format!("linked {}", self.linked),
            _ => relative_age(self.age),
        }
    }
}

/// Order one section's pages by `sort`, in place. The related block comes
/// back in the server's own order (updated, newest first), so that key is
/// a no-op and `views`, which the block does not report, keeps it too —
/// the honest answer to "sort by a number I do not have" is "leave it as
/// the server sent it", and the default order is what that is. Sections
/// themselves (Links, one per hub in page order, External links) are not
/// reordered: the grouping IS the information there.
pub(crate) fn sort_related(pages: &mut [&cosense::api::RelatedPage], sort: cosense::index::SortKey) {
    use cosense::index::SortKey;
    match sort {
        SortKey::Updated | SortKey::Views => {}
        SortKey::Title => pages.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        SortKey::Linked => pages.sort_by(|a, b| b.linked.cmp(&a.linked)),
        SortKey::Accessed => pages.sort_by(|a, b| b.accessed.cmp(&a.accessed)),
        SortKey::Created => pages.sort_by(|a, b| b.created.cmp(&a.created)),
    }
}

/// A finished background related-pages fetch: which page it describes, and
/// the block itself — `None` when the request failed, which still has to
/// come back so the page stops waiting on it.
pub(crate) type RelatedMsg = (String, String, Option<cosense::api::RelatedPages>);

/// The page-level facts the related-pages block has to be read against.
///
/// v2 does not ship `relatedPages` with the body (see
/// `api::Client::get_related_in`), so the block lands after the page is
/// already on screen — and by then the `Page` is gone. These four fields
/// are all that `build_related` and `link_truth` ever wanted from it, so
/// the viewer keeps them and throws the rest of the response away as
/// before.
#[derive(Default)]
pub(crate) struct PageFacts {
    pub(crate) title: String,
    pub(crate) persistent: bool,
    pub(crate) links: Vec<String>,
    pub(crate) project_links: Vec<String>,
}

impl PageFacts {
    pub(crate) fn of(page: &cosense::api::Page) -> Self {
        PageFacts {
            title: page.title.clone(),
            persistent: page.persistent,
            links: page.links.clone(),
            project_links: page.project_links.clone(),
        }
    }
}

/// Everything produced by loading one page.
pub(crate) struct Loaded {
    pub(crate) project: String,
    pub(crate) title: String,
    /// Site-theme-derived header colors for this project.
    pub(crate) header_colors: HeaderColors,
    pub(crate) project_display: String,
    /// Immutable page id (edit API / commit log).
    pub(crate) page_id: String,
    pub(crate) lines: Vec<PageLine>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) srcs: Vec<usize>,
    /// See `App::hits`.
    pub(crate) hits: Vec<Vec<cosense::render::Hit>>,
    /// See `App::read_at`.
    pub(crate) read_at: Option<i64>,
    /// Whether this credential may edit the loaded project.
    pub(crate) editable: bool,
    /// Related-pages sections (see `build_related`). EMPTY on arrival: the
    /// related block is a second request that has not landed yet
    /// (`App::start_related_load`).
    pub(crate) related: Vec<RelSection>,
    /// What this page's response said about the pages it links to. Also
    /// waiting on the related block, so this starts out knowing nothing but
    /// the page's own existence.
    pub(crate) links: LinkTruth,
    /// Kept so the related block can be read against this page when it
    /// arrives (see `PageFacts`).
    pub(crate) facts: PageFacts,
}

/// Build the related-pages sections the way scrapbox.io presents them:
///
///   Links            — 1-hop: pages this page links to + pages linking here
///                      (existing pages only; the API precomputes this).
///   <hub> (×N)       — 2-hop: pages sharing the link <hub> with this page,
///                      one group per link of this page, in page order. An
///                      entry appears only under its first hub. A hub may be
///                      a page that does not exist — 2-hop links work through
///                      empty pages, which is exactly what makes a purely
///                      auto-linked wiki hang together.
///   External links   — outgoing `[/project/title]` links (the API exposes
///                      no incoming cross-project list).
pub(crate) fn related_is_unread(
    visits: &HashMap<String, i64>,
    project: &str,
    title: &str,
    updated: i64,
) -> bool {
    visits
        .get(&format!("{project}/{title}"))
        .map_or(true, |&seen_at| updated > 0 && updated > seen_at)
}

pub(crate) fn build_related(
    facts: &PageFacts,
    related: Option<&cosense::api::RelatedPages>,
    project: &str,
    sort: cosense::index::SortKey,
) -> Vec<RelSection> {
    let mut secs: Vec<RelSection> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let visits = load_visits();
    let unread = |p: &str, title: &str, updated: i64| {
        related_is_unread(&visits, p, title, updated)
    };
    let key_of = |p: &cosense::api::RelatedPage| {
        if p.title_lc.is_empty() { p.title.to_lowercase() } else { p.title_lc.clone() }
    };
    let entry_of = |p: &cosense::api::RelatedPage| RelEntry {
        item: LinkItem::Page(p.title.clone()),
        title: p.title.clone(),
        desc: p.descriptions.first().cloned().unwrap_or_default(),
        age: p.updated,
        accessed: p.accessed,
        created: p.created,
        linked: p.linked,
        unread: unread(project, &p.title, p.updated),
    };
    // Each section is ordered on its own; see `sort_related` for why the
    // sections themselves stay put.
    let section = |heading: String, mut group: Vec<&cosense::api::RelatedPage>| {
        sort_related(&mut group, sort);
        RelSection { heading, entries: group.into_iter().map(&entry_of).collect() }
    };
    if let Some(rel) = related {
        if !rel.links1hop.is_empty() {
            for p in &rel.links1hop {
                seen.insert(key_of(p));
            }
            secs.push(section(format!("Links ({})", rel.links1hop.len()), rel.links1hop.iter().collect()));
        }
        // 2-hop groups, one per link of this page (page order). `links_lc`
        // of an entry lists which links it shares.
        for hub in &facts.links {
            let hub_lc = hub.to_lowercase();
            let group: Vec<&cosense::api::RelatedPage> = rel
                .links2hop
                .iter()
                .filter(|p| !seen.contains(&key_of(p)) && p.links_lc.iter().any(|l| *l == hub_lc))
                .collect();
            if group.is_empty() {
                continue;
            }
            for p in &group {
                seen.insert(key_of(p));
            }
            secs.push(section(format!("{hub} ({})", group.len()), group));
        }
        // 2-hop entries whose hubs did not match any current link (rename
        // races and the like) still deserve a place.
        let rest: Vec<&cosense::api::RelatedPage> =
            rel.links2hop.iter().filter(|p| !seen.contains(&key_of(p))).collect();
        if !rest.is_empty() {
            secs.push(section(format!("2 hop links ({})", rest.len()), rest));
        }
    }
    if !facts.project_links.is_empty() {
        let mut entries: Vec<RelEntry> = facts
            .project_links
            .iter()
            .filter_map(|pl| {
                let rest = pl.strip_prefix('/')?;
                let (project, title) = rest.split_once('/')?;
                if project.is_empty() || title.is_empty() {
                    return None;
                }
                Some(RelEntry {
                    item: LinkItem::ProjectPage {
                        project: project.to_string(),
                        title: title.to_string(),
                    },
                    title: pl.clone(),
                    desc: String::new(),
                    age: 0,
                    accessed: 0,
                    created: 0,
                    linked: 0,
                    unread: unread(project, title, 0),
                })
            })
            .collect();
        // Cross-project links come with no stamps and no counts, so the
        // only order that means anything for them is A→Z.
        if sort == cosense::index::SortKey::Title {
            entries.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        }
        if !entries.is_empty() {
            secs.push(RelSection {
                heading: format!("External links ({})", entries.len()),
                entries,
            });
        }
    }
    secs
}

/// Age in seconds for a related row's telomere.
///
/// A cross-project link has no timestamp in the related list (`age == 0`),
/// and neither has anything else the API declined to date. Those report the
/// OLDEST bucket rather than "brand new": the thickness axis means "how
/// recently did this change", and the honest answer is "no idea, so do not
/// claim it is fresh". That is also the hairline those rows already wore.
pub(crate) const UNDATED_AGE: i64 = 100 * 365 * 86_400;

pub(crate) fn related_age(updated: i64) -> i64 {
    match updated {
        0 => UNDATED_AGE,
        t => (now_secs() - t).max(0),
    }
}

/// A line is unread if it was edited after `read_at`, or the page was never
/// seen at all (`None`).
pub(crate) fn unread_since(updated: i64, read_at: Option<i64>) -> bool {
    read_at.map_or(true, |t| updated > t)
}

/// Local record of when this viewer last opened each page, keyed by
/// `project/title` (epoch seconds). Cosense only learns about browser
/// visits, so without this a page read here would stay "unread" forever.
/// Lives in `$XDG_STATE_HOME/cosense-tui/visits.json` (default
/// `~/.local/state`).
pub(crate) fn visits_path() -> Option<std::path::PathBuf> {
    let base = match std::env::var("XDG_STATE_HOME") {
        Ok(x) if !x.is_empty() => std::path::PathBuf::from(x),
        _ => std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".local").join("state"),
    };
    Some(base.join("cosense-tui").join("visits.json"))
}

pub(crate) fn load_visits() -> HashMap<String, i64> {
    visits_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record a visit to `project/title` at `now`, returning the PREVIOUS
/// local visit time (if any). Failures to persist are ignored: the worst
/// case is a page that stays "unread" on the next visit.
pub(crate) fn record_visit(project: &str, title: &str, now: i64) -> Option<i64> {
    let mut visits = load_visits();
    let prev = visits.insert(format!("{project}/{title}"), now);
    if let Some(p) = visits_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string(&visits) {
            let _ = std::fs::write(p, s);
        }
    }
    prev
}

/// Leave the index for one of its rows. The index goes on the back stack
/// (that is what makes `[` come back to it), and a row that is not a page
/// yet lands in EDIT on a fresh line — nothing is sent until something is
/// written (see `dispatch_create`).
pub(crate) fn open_from_index(app: &mut App, ctx: &Ctx, target: Option<(String, bool)>) {
    let from = app.here();
    let project = app.index_project.clone();
    // Held, not dropped: a page that fails to load is not a reason to lose
    // the list you were choosing from.
    let saved = app.index.take();
    let Some((title, create)) = target else { return };
    if !navigate_from(app, ctx, &project, &title, from) {
        app.index = saved;
        return;
    }
    if create {
        app.cursor = 0;
        open_line(app, ctx, false);
    }
}

/// `[` / `]`: step back or forward through the places visited.
///
/// A place is a page OR a project's index, so walking back out of a page
/// you reached from the index lands in the index — where you were — rather
/// than in whatever page happened to precede it.
pub(crate) fn go_history(app: &mut App, ctx: &Ctx, back: bool) {
    let place = if back { app.history.pop() } else { app.forward.pop() };
    let Some(place) = place else {
        app.note(if back { t!("戻る先の履歴はありません", "no history") } else { t!("進む先の履歴はありません", "no forward history") });
        return;
    };
    let here = app.here();
    let mut arrived = false;
    match place {
        Place::Page { project, title } => {
            // Every index keeps the page it was opened over loaded. If that
            // page is the destination, closing the index is both exact and
            // instant: cursor, scroll and images do not have to be rebuilt.
            if app.index.is_some() && app.project == project && app.title == title {
                app.index = None;
                arrived = true;
            } else {
                match load_page(ctx, &project, &title) {
                    Ok(loaded) => {
                        app.index = None;
                        app.set_page(loaded, ctx);
                        arrived = true;
                    }
                    Err(e) => {
                        // A failed back must not eat the destination. The
                        // same key can retry after the connection recovers.
                        let place = Place::Page { project, title };
                        if back {
                            app.history.push(place);
                        } else {
                            app.forward.push(place);
                        }
                        app.toast_err(t!("履歴の移動に失敗しました: {e}", "history failed: {e}"));
                    }
                }
            }
        }
        Place::Index { project, state } => {
            app.index = Some(*state);
            app.index_display = ctx.project_display(&project);
            app.index_project = project;
            app.overlay = None;
            arrived = true;
        }
    }
    if arrived {
        if back {
            app.forward.push(here);
        } else {
            app.history.push(here);
        }
    }
}

/// Open the project index: every page, newest first, with a short excerpt
/// from the one under the cursor docked below the list.
///
/// The list is one request (`/api/pages/<project>?limit=500&sort=<key>`)
/// and the excerpt costs nothing on top of it: the same response carries
/// each page's first lines, which is what the reader is choosing between.
///
/// The order is the session's standing choice (`App::index_sort`), asked
/// of the server so it covers the whole project rather than whichever
/// pages one request happened to bring back.
pub(crate) fn open_index(app: &mut App, ctx: &Ctx, project: &str, filter: String) {
    use cosense::index::{Entry, Index};
    let sort = app.index_sort;
    let (count, pages) =
        match ctx.client.list_pages_in(project, INDEX_PAGE_LIMIT, 0, sort.name()) {
            Ok(v) => v,
            Err(e) => {
                // Both, because this is reached from two places: from a
                // page (no index open — the page's own status line shows
                // it) and from `s`/`^u` with the list already on screen.
                app.toast_err(t!("ページ一覧を取得できません: {e}", "page list failed: {e}"));
                return;
            }
        };
    let visits = load_visits();
    let mut entries: Vec<Entry> = pages
        .into_iter()
        .map(|p| {
            let seen = visits.get(&format!("{project}/{}", p.title)).copied();
            Entry::from_summary(p, seen)
        })
        .collect();
    Index::sort_entries(&mut entries, sort);
    let mut ix = Index::new(entries, count.max(0) as usize, sort);
    ix.filter_mode = carried_filter_mode(app);
    // Cached per project after the first probe, so this is not a request
    // per index open (see `Ctx::can_edit_in`).
    ix.can_create = ctx.can_edit_in(project);
    if !filter.is_empty() {
        ix.set_filter(filter);
    }
    app.index = Some(ix);
    app.index_project = project.to_string();
    app.index_display = ctx.project_display(project);
    app.index_sort_menu = None;
    app.overlay = None;
}

/// Which question the filter line opens with, carried across a rebuilt
/// list. Building a fresh `Index` would otherwise silently drop back to
/// the title filter every time the list was refetched. A `^o` from a page
/// (no index yet) starts at the default, which is the title filter.
pub(crate) fn carried_filter_mode(app: &App) -> cosense::index::FilterMode {
    app.index.as_ref().map(|ix| ix.filter_mode).unwrap_or_default()
}

/// Run a full-text search and put its hits on screen in place of the list.
///
/// The hits keep the server's RELEVANCE order (they are not re-sorted): the
/// question was "where is this word", and the best answer to it is not the
/// most recently edited page. `^u` puts the project's own list back.
pub(crate) fn search_index(app: &mut App, ctx: &Ctx, query: &str) {
    use cosense::index::{Entry, Index};
    let query = query.trim().to_string();
    if query.is_empty() {
        return;
    }
    let project = app.index_project.clone();
    let (count, capped, hits) = match ctx.client.search_pages_in(&project, &query) {
        Ok(v) => v,
        Err(e) => {
            app.toast_err(t!("本文検索に失敗しました: {e}", "full-text search failed: {e}"));
            return;
        }
    };
    let visits = load_visits();
    let entries: Vec<Entry> = hits
        .into_iter()
        .map(|h| {
            let seen = visits.get(&format!("{project}/{}", h.title)).copied();
            Entry::from_search(h, seen)
        })
        .collect();
    let found = entries.len();
    let mut ix = Index::new(entries, count.max(0) as usize, app.index_sort);
    ix.search = Some(query.clone());
    ix.search_capped = capped;
    ix.can_create = ctx.can_edit_in(&project);
    // `/` over a set of hits means "search again", so the line has to open
    // asking the same question it just answered.
    ix.filter_mode = carried_filter_mode(app);
    app.index = Some(ix);
    if found == 0 {
        app.toast(t!("「{}」は本文にありません", "no page's body has \"{}\"", query));
    }
}

/// `^u`: back to the project's own list, whatever was narrowing or
/// searching it.
pub(crate) fn clear_index_search(app: &mut App, ctx: &Ctx) {
    let project = app.index_project.clone();
    open_index(app, ctx, &project, String::new());
}

/// Re-open the list in `sort` order, keeping the filter that is in force.
///
/// A refetch, not a local re-shuffle: the list holds at most
/// `INDEX_PAGE_LIMIT` pages, so re-ordering what is already in hand would
/// silently sort the wrong 500 pages of a bigger project.
pub(crate) fn resort_index(app: &mut App, ctx: &Ctx, sort: cosense::index::SortKey) {
    let project = app.index_project.clone();
    let filter = app.index.as_ref().map(|ix| ix.filter.clone()).unwrap_or_default();
    app.index_sort = sort;
    app.rebuild_related();
    app.laid_width = 0; // related rows may have moved
    open_index(app, ctx, &project, filter);
}

/// Fetch and render one page. Images are NOT downloaded here: they are
/// fetched on background threads (see `App::start_image_loads`) so a page
/// with many images still appears immediately.
pub(crate) fn load_page(ctx: &Ctx, project: &str, title: &str) -> Result<Loaded, Box<dyn Error>> {
    let page = ctx.client.get_page_in(project, title)?;
    let mut lines: Vec<PageLine> = page.lines.clone();
    if !page.persistent {
        // An uncreated page comes back as a template: a title line with no
        // id. Give every line an id here, so editing works exactly as on a
        // real page and the create request can name its own lines.
        for l in lines.iter_mut() {
            if l.id.is_empty() {
                l.id = new_line_id();
            }
        }
    }
    let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
    let facts = PageFacts::of(&page);
    // The related block is not in a v2 response, so nothing is known about
    // the links yet; `App::start_related_load` fills both in.
    let links = link_truth(&facts, None);
    let rendered = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &links);
    // Last seen = later of the browser's and this viewer's previous visit;
    // then stamp this visit so the next open treats today's lines as read.
    let local_prev = record_visit(project, title, now_secs());
    let read_at = match (page.last_accessed, local_prev) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let editable = ctx.can_edit_in(project);
    let site_theme = ctx.project_theme(project);
    let (header_fg, header_bg) =
        cosense::theme::project_header_colors(site_theme.as_deref(), ctx.terminal_bg);
    Ok(Loaded {
        project: project.to_string(),
        title: title.to_string(),
        header_colors: HeaderColors { fg: header_fg, bg: header_bg },
        project_display: ctx.project_display(project),
        page_id: live_page_id(&page),
        lines,
        blocks: rendered.blocks,
        srcs: rendered.srcs,
        hits: rendered.hits,
        read_at,
        editable,
        related: Vec::new(),
        links,
        facts,
    })
}

/// What this page's own response says about the pages it links to — by
/// Cosense's rule, which is not "does the page exist".
///
/// Read out of its client (`compileRelatedPages` in the related-page
/// store, and `isPageExists` where a link is drawn), a link is drawn as
/// live when ANY of these hold:
///
/// * it is a 1-hop or 2-hop neighbour — a page that exists;
/// * it is the page being read;
/// * **another page links to the same title**, written or not.
///
/// That last rule is not an accident: the index the web client builds
/// walks every page's links and marks each target live *unless the only
/// page writing it is the one on screen*
/// (`P !== p.Page.id && n.set(te, !0)`). So the red does not mean "no
/// page here"; it means "nobody but this page has ever said this word".
/// A tag two pages share is already a hub, and Cosense stops calling it
/// empty the moment the second page uses it.
///
/// The same answer is computable from the response already in hand: a
/// page that links to one of OUR targets shares that target with us, so
/// it is in our own 1-hop or 2-hop list with the target in its `linksLc`.
/// Checked against scrapbox.io: on villagepump's `井戸端` the naive "not
/// in links1hop" reading calls four titles empty, Cosense calls three,
/// and the one it spares is `実況` — unwritten, but linked from other
/// pages. This reproduces that exactly.
///
/// It costs no extra request, and it answers every link the page was
/// SAVED with. Links written since — the ones a reader is most likely to
/// be looking at — are not in it at all, and `probe_unknown_links` goes
/// and asks about those one at a time.
pub(crate) fn link_truth(
    facts: &PageFacts,
    related: Option<&cosense::api::RelatedPages>,
) -> LinkTruth {
    // No related-pages block, no reading: every link keeps its ordinary
    // colour rather than all of them turning red at once. That is also the
    // state a page is in for the first moment it is on screen, before the
    // related fetch lands.
    let Some(r) = related else { return LinkTruth::default() };
    let neighbours = || r.links1hop.iter().chain(r.links2hop.iter());
    let mut existing: Vec<&str> = neighbours().map(|p| p.title.as_str()).collect();
    // Titles this page links to that a neighbour ALSO links to: shared
    // words, live whether or not anyone wrote the page.
    let ours: HashSet<String> =
        facts.links.iter().map(|l| cosense::render::title_lc(l)).collect();
    existing.extend(
        neighbours()
            .flat_map(|p| p.links_lc.iter())
            .filter(|c| ours.contains(c.as_str()))
            .map(|c| c.as_str()),
    );
    let mut truth = LinkTruth::seed(facts.links.iter().map(|s| s.as_str()), existing);
    // And this page itself, which is not in its own 1-hop list. Reading a
    // page is the most direct answer there is about it — including the
    // uncreated one opened through a link, which is exactly the title the
    // page we came from is drawing.
    truth.learn(&facts.title, facts.persistent);
    truth
}

/// The lookup worker: one title in, one answer out.
///
/// The question is Cosense's, not "does the page exist" (see
/// `link_truth`): a title is live when somebody wrote it, OR when some
/// page other than the one asking links to it. The cheap half is asked
/// first — a HEAD that says "written" ends it — so only a title nobody
/// wrote costs the second request, which is also the small one (an
/// unwritten page's response is its back links, not a body).
///
/// Serial on purpose. Unknown links are rare (a page arrives with all of
/// its links already answered), so this is a trickle — and a trickle down
/// one thread cannot turn a page full of new links into a burst of
/// requests.
pub(crate) fn spawn_link_prober(
    client: Client,
    rx: mpsc::Receiver<LinkProbe>,
    tx: mpsc::Sender<(LinkProbe, bool)>,
) {
    std::thread::spawn(move || {
        while let Ok(probe) = rx.recv() {
            let LinkProbe { project, title, asked_by } = &probe;
            // A failed lookup answers nothing, and is not retried (see
            // `App::link_pending`): the title stays unknown and keeps its
            // ordinary colour, which is the safe direction.
            let Ok(written) = client.page_exists(project, title) else { continue };
            let live = if written {
                true
            } else {
                match client.backlink_ids(project, title) {
                    // The asking page's own link does not make a word
                    // shared — that is the whole point of the rule.
                    Ok(ids) => ids.iter().any(|id| id != asked_by),
                    Err(_) => continue,
                }
            };
            if tx.send((probe, live)).is_err() {
                return; // app gone
            }
        }
    });
}

/// One question for the lookup worker: is this title live? Asked from a
/// page whose own links must not count towards the answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LinkProbe {
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) asked_by: String,
}

impl App {
    /// Number of cursor-addressable source indices: body lines, plus the
    /// related entries in view mode (virtual lines after the body). The
    /// edit session hides the related section, so its rows are not
    /// addressable then either — exactly like source mode.
    /// Where the reader is right now, for the back stack.
    pub(crate) fn here(&self) -> Place {
        match self.index.as_ref() {
            Some(index) => Place::Index {
                project: self.index_project.clone(),
                state: Box::new(index.clone()),
            },
            None => Place::Page { project: self.project.clone(), title: self.title.clone() },
        }
    }

    /// Read state for a cursor-addressable related row. The flattening order
    /// is exactly the same as `virtual_items`.
    pub(crate) fn related_telomere(&self, src: usize) -> Option<(i64, bool)> {
        let index = src.checked_sub(self.lines.len())?;
        let entry = self
            .related
            .iter()
            .flat_map(|section| section.entries.iter())
            .nth(index)?;
        Some((related_age(entry.age), entry.unread))
    }

    /// Install a freshly loaded page, resetting view state (keeps comments).
    /// Install a freshly loaded page and immediately kick off its image
    /// downloads. Loading is folded in here so no navigation path can forget
    /// it (back/forward included).
    pub(crate) fn set_page(&mut self, l: Loaded, ctx: &Ctx) {
        self.bump_server_epoch();
        self.time = None; // installing a live page always exits history
        self.outline_prefix = false;
        self.move_mode = None;
        self.outline_refresh_needed = false;
        // A page install ends any edit session and cuts the undo lineage:
        // undo ops reference THIS page's line ids.
        self.session = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        // Echoes of the last page's commits say nothing about this one.
        self.own_commits.clear();
        self.history_dropped = false;
        // Whatever the last page was waiting to become, it is not this one.
        self.create_state = CreateState::Idle;
        // A different project is a different set of capabilities: what we
        // learned about the old one (visibility, a refused browser) says
        // nothing here. The sid is a property of the SESSION and survives.
        if self.project != l.project {
            self.caps = self.caps.for_new_project();
            self.vis_asked = None;
        }
        self.project = l.project;
        self.title = l.title;
        self.header_colors = l.header_colors;
        self.project_display = l.project_display;
        self.page_id = l.page_id;
        self.web_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Navigating leaves the joined room. Until the thread has re-joined
        // the new one AND caught it up, there is no push channel here, so
        // the fast poll covers the gap — and `absorb_interval` fetches at
        // once on the way down rather than waiting out a 60 s nap.
        self.set_sync_state(SyncState::Polling);
        self.lines = l.lines;
        self.blocks = l.blocks;
        self.srcs = l.srcs;
        self.hits = l.hits;
        self.read_at = l.read_at;
        self.editable = l.editable;
        // Answers that were only true of the page we are leaving go now;
        // the page arriving may be the second one writing that word.
        self.links.forget_dead();
        self.links.absorb(l.links);
        // ...so those titles must be askable again.
        self.link_pending.clear();
        if let Ok(mut t) = self.poll_target.lock() {
            *t = (self.project.clone(), self.title.clone());
        }
        // A fresh page install severs the websocket commit lineage: the
        // room re-joins on the new target, and any remote head we tracked,
        // events buffered, or resync held for the OLD page are meaningless.
        self.ws_head = None;
        self.ws_pending.clear();
        self.ws_resync_pending = false;
        self.ws_held_resync = None;
        self.related = l.related;
        self.related_block = None;
        self.facts = l.facts;
        self.virtual_items = self
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
        self.images.clear();
        self.image_errors.clear();
        self.pending.clear();
        self.web_pending.clear();
        self.web_errors.clear();
        self.web_rescaling.clear();
        self.web_missing.clear();
        self.web_manual_wanted = false;
        // Once per page, not once per diagram.
        self.web_notice_shown = false;
        // A freshly fetched page IS the server's state.
        self.mark_synced();
        self.laid_width = 0; // force rebuild
        self.scroll = 0;
        self.cursor = 0;
        self.selection = None;
        self.follow = true;
        self.web_dark = !ctx.light;
        self.start_image_loads(ctx);
        self.start_web_renders(capability::Trigger::Auto);
        self.start_related_load(ctx);
    }

    /// Go and get this page's related-pages block, which the v2 body does
    /// not carry (see `api::Client::get_related_in`).
    ///
    /// On a background thread on purpose: it is the request the old v1 read
    /// spent most of its bytes and most of its wait on (井戸端: 364 KB and
    /// 0.7 s against 13.7 KB and 0.2 s), and none of it is needed to put
    /// the page on screen. The sections appear a beat later, from the
    /// bottom of the page where nobody is looking yet.
    pub(crate) fn start_related_load(&mut self, ctx: &Ctx) {
        if !self.related_fetch {
            return; // tests: no page install goes to the network
        }
        self.related_pending = true;
        let tx = self.related_tx.clone();
        let client = ctx.client.clone();
        let project = self.project.clone();
        let title = self.title.clone();
        std::thread::spawn(move || {
            let rel = client.get_related_in(&project, &title).ok();
            let _ = tx.send((project, title, rel));
        });
    }

    /// Install a related block that arrived. `true` = the page has to be
    /// laid out and coloured again.
    ///
    /// A failed fetch (`None`) only lowers the gate: the related list stays
    /// empty and every link keeps its ordinary colour, and the link prober
    /// is then free to ask about them one at a time — the same degradation
    /// as a page whose response never said anything about its links.
    pub(crate) fn drain_related(&mut self) -> bool {
        let mut changed = false;
        while let Ok((project, title, rel)) = self.related_rx.try_recv() {
            // A block for a page the reader has already left says nothing
            // about the one on screen.
            if project != self.project || title != self.title {
                continue;
            }
            self.related_pending = false;
            // Parked on a snapshot: the related list describes the PRESENT
            // graph, and `show_snapshot` cleared it for that reason. Leaving
            // the time machine reloads the page, which asks again.
            if self.time.is_some() {
                continue;
            }
            // The gate is down: whatever links this block does not answer
            // are asked about now, not on the next slow beat.
            self.link_scan_at = Instant::now() - LINK_SCAN_EVERY;
            let Some(rel) = rel else { continue };
            self.links.absorb(link_truth(&self.facts, Some(&rel)));
            self.related_block = Some(rel);
            self.rebuild_related();
            changed = true;
        }
        changed
    }

    /// Lay the kept related block out as sections in the session's sort
    /// order (`index_sort`). Called when the block lands and again when
    /// the order changes, so the page under the index does not keep the
    /// old order while the list shows the new one.
    pub(crate) fn rebuild_related(&mut self) {
        let Some(rel) = self.related_block.as_ref() else { return };
        self.related = build_related(&self.facts, Some(rel), &self.project, self.index_sort);
        self.virtual_items = self
            .related
            .iter()
            .flat_map(|s| s.entries.iter().map(|e| e.item.clone()))
            .collect();
    }

    /// Make sure the CURRENT project's member table is usable for resolving
    /// `want` (a userId), fetching or refetching when:
    /// - the project has no table yet (first `t` in this project), or
    /// - the table is older than `MEMBERS_TTL`, or
    /// - `want` is not in the table and the table is older than
    ///   `MEMBERS_MISS_COOLDOWN` (a newly joined member; the cooldown
    ///   stops an id that will never resolve from refetching every time).
    /// A failed fetch stores an empty table so the same rules pace retries.
    /// Called on demand (the `t` overlay), never per frame.
    pub(crate) fn ensure_members(&mut self, ctx: &Ctx, want: Option<&str>) {
        let stale = match self.members.get(&self.project) {
            None => true,
            Some(m) => {
                let age = m.fetched_at.elapsed();
                let missing = want.map_or(false, |id| !id.is_empty() && !m.names.contains_key(id));
                age > MEMBERS_TTL || (missing && age > MEMBERS_MISS_COOLDOWN)
            }
        };
        if !stale {
            return;
        }
        let names: HashMap<String, String> = match ctx.client.list_members_in(&self.project) {
            Ok(ms) => ms
                .into_iter()
                .map(|m| {
                    let name = if m.display_name.is_empty() { m.name } else { m.display_name };
                    (m.id, name)
                })
                .collect(),
            Err(e) => {
                // Say so: a silent empty table would look like "unknown
                // member" and hide a permission or network problem.
                self.toast_err(t!("メンバー一覧を取得できません: {} — {e}", "member list failed for {}: {e}", self.project));
                HashMap::new()
            }
        };
        self.members
            .insert(self.project.clone(), MembersCache { names, fetched_at: Instant::now() });
    }

    /// Display name for a userId in the current project, from the cached
    /// table; an unresolved id shows its first 8 characters.
    pub(crate) fn member_name(&self, id: &str) -> String {
        if id.is_empty() {
            return "(unknown)".into();
        }
        self.members
            .get(&self.project)
            .and_then(|m| m.names.get(id))
            .cloned()
            .unwrap_or_else(|| format!("({}…)", &id[..id.len().min(8)]))
    }

    /// True if the line was edited after the user last saw this page (or
    /// the page was never seen). Drives the telomere tint.
    pub(crate) fn line_unread(&self, l: &PageLine) -> bool {
        unread_since(l.updated, self.read_at)
    }

    /// Number of unread lines on this page. Only tests count them now: the
    /// header stopped showing the number (the telomere's colour says which
    /// lines are new).
    #[cfg(test)]
    pub(crate) fn unread_count(&self) -> usize {
        self.lines.iter().filter(|l| self.line_unread(l)).count()
    }
}
