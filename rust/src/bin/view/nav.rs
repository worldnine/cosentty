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
    /// `updated` epoch seconds (0 = unknown, no age shown).
    pub(crate) age: i64,
    /// Two-state telomere for related rows. RelatedPages has no per-user
    /// lastAccessed, so this is based on this TUI's persisted visit record.
    pub(crate) unread: bool,
}

/// Everything produced by loading one page.
pub(crate) struct Loaded {
    pub(crate) project: String,
    pub(crate) title: String,
    /// Site-theme-derived header colors for this project.
    pub(crate) header_colors: HeaderColors,
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
    /// Related-pages sections (see `build_related`).
    pub(crate) related: Vec<RelSection>,
    /// What this page's response said about the pages it links to.
    pub(crate) links: LinkTruth,
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

pub(crate) fn build_related(page: &cosense::api::Page, project: &str) -> Vec<RelSection> {
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
        unread: unread(project, &p.title, p.updated),
    };
    if let Some(rel) = &page.related {
        if !rel.links1hop.is_empty() {
            for p in &rel.links1hop {
                seen.insert(key_of(p));
            }
            secs.push(RelSection {
                heading: format!("Links ({})", rel.links1hop.len()),
                entries: rel.links1hop.iter().map(&entry_of).collect(),
            });
        }
        // 2-hop groups, one per link of this page (page order). `links_lc`
        // of an entry lists which links it shares.
        for hub in &page.links {
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
            secs.push(RelSection {
                heading: format!("{hub} ({})", group.len()),
                entries: group.into_iter().map(&entry_of).collect(),
            });
        }
        // 2-hop entries whose hubs did not match any current link (rename
        // races and the like) still deserve a place.
        let rest: Vec<&cosense::api::RelatedPage> =
            rel.links2hop.iter().filter(|p| !seen.contains(&key_of(p))).collect();
        if !rest.is_empty() {
            secs.push(RelSection {
                heading: format!("2 hop links ({})", rest.len()),
                entries: rest.into_iter().map(&entry_of).collect(),
            });
        }
    }
    if !page.project_links.is_empty() {
        let entries: Vec<RelEntry> = page
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
                    unread: unread(project, title, 0),
                })
            })
            .collect();
        if !entries.is_empty() {
            secs.push(RelSection {
                heading: format!("External links ({})", entries.len()),
                entries,
            });
        }
    }
    secs
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
        app.status = if back { t!("戻る先の履歴はありません", "no history") } else { t!("進む先の履歴はありません", "no forward history") };
        return;
    };
    let here = app.here();
    let label = place.label();
    let arrow = if back { "←" } else { "→" };
    let mut arrived = false;
    match place {
        Place::Page { project, title } => {
            // Every index keeps the page it was opened over loaded. If that
            // page is the destination, closing the index is both exact and
            // instant: cursor, scroll and images do not have to be rebuilt.
            if app.index.is_some() && app.project == project && app.title == title {
                app.index = None;
                app.status = format!("{arrow} {title}");
                arrived = true;
            } else {
                match load_page(ctx, &project, &title) {
                    Ok(loaded) => {
                        app.index = None;
                        app.set_page(loaded, ctx);
                        app.status = format!("{arrow} {title}");
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
                        app.status = t!("履歴の移動に失敗しました: {e}", "history failed: {e}");
                    }
                }
            }
        }
        Place::Index { project, state } => {
            app.index = Some(*state);
            app.index_project = project;
            app.overlay = None;
            app.status = format!("{arrow} {label}");
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
/// The list is one request (`/api/pages/<project>?limit=500&sort=updated`)
/// and the excerpt costs nothing on top of it: the same response carries
/// each page's first lines, which is what the reader is choosing between.
pub(crate) fn open_index(app: &mut App, ctx: &Ctx, project: &str, filter: String) {
    use cosense::index::{Entry, Index};
    let (count, pages) = match ctx.client.list_pages_in(project, INDEX_PAGE_LIMIT, 0, "updated") {
        Ok(v) => v,
        Err(e) => {
            app.status = t!("ページ一覧を取得できません: {e}", "page list failed: {e}");
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
    // The API floats pinned pages to the top even under sort=updated, so a
    // pinned two-year-old page would head a "recently updated" list.
    entries.sort_by(|a, b| b.updated.cmp(&a.updated));
    let mut ix = Index::new(entries, count.max(0) as usize);
    if !filter.is_empty() {
        ix.set_filter(filter);
    }
    app.index = Some(ix);
    app.index_project = project.to_string();
    app.overlay = None;
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
    let links = link_truth(&page);
    let rendered = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &links);
    // Last seen = later of the browser's and this viewer's previous visit;
    // then stamp this visit so the next open treats today's lines as read.
    let local_prev = record_visit(project, title, now_secs());
    let read_at = match (page.last_accessed, local_prev) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let related = build_related(&page, project);
    let editable = ctx.can_edit_in(project);
    let site_theme = ctx.project_theme(project);
    let (header_fg, header_bg) =
        cosense::theme::project_header_colors(site_theme.as_deref(), ctx.terminal_bg);
    Ok(Loaded {
        project: project.to_string(),
        title: title.to_string(),
        header_colors: HeaderColors { fg: header_fg, bg: header_bg },
        page_id: live_page_id(&page),
        lines,
        blocks: rendered.blocks,
        srcs: rendered.srcs,
        hits: rendered.hits,
        read_at,
        editable,
        related,
        links,
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
pub(crate) fn link_truth(page: &cosense::api::Page) -> LinkTruth {
    // No related-pages block, no reading: every link keeps its ordinary
    // colour rather than all of them turning red at once.
    let Some(r) = page.related.as_ref() else { return LinkTruth::default() };
    let neighbours = || r.links1hop.iter().chain(r.links2hop.iter());
    let mut existing: Vec<&str> = neighbours().map(|p| p.title.as_str()).collect();
    // Titles this page links to that a neighbour ALSO links to: shared
    // words, live whether or not anyone wrote the page.
    let ours: HashSet<String> =
        page.links.iter().map(|l| cosense::render::title_lc(l)).collect();
    existing.extend(
        neighbours()
            .flat_map(|p| p.links_lc.iter())
            .filter(|c| ours.contains(c.as_str()))
            .map(|c| c.as_str()),
    );
    let mut truth = LinkTruth::seed(page.links.iter().map(|s| s.as_str()), existing);
    // And this page itself, which is not in its own 1-hop list. Reading a
    // page is the most direct answer there is about it — including the
    // uncreated one opened through a link, which is exactly the title the
    // page we came from is drawing.
    truth.learn(&page.title, page.persistent);
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
