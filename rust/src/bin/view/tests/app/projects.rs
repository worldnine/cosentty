use crate::*;
use crate::tests::support::*;

// ---------------------------------------------------------------
// The projects list: the level above the site top, on the same screen.
// ---------------------------------------------------------------

fn two_projects() -> Vec<cosense::api::ProjectSummary> {
    vec![
        cosense::api::ProjectSummary {
            name: "acme".into(),
            display_name: "Acme, Inc.".into(),
            public_visible: false,
            plan: Some("business".into()),
            updated: now_secs() - 60,
            created: 1,
            users_count: 20,
        },
        cosense::api::ProjectSummary {
            name: "my-sandbox".into(),
            display_name: String::new(),
            public_visible: true,
            plan: None,
            updated: now_secs() - 3600,
            created: 2,
            users_count: 1,
        },
    ]
}

/// A project row is its proper name with the slug beside it, and the
/// filter matches either — the slug is what the URL and the command
/// line call it. Nothing is ever offered for creation, and the line
/// has no body search to swap to.
#[test]
fn the_projects_list_filters_on_slug_and_name_and_offers_nothing_to_create() {
    use_japanese();
    use cosense::index::{Entry, FilterMode, Index, Row, Scope, SortKey};
    let entries: Vec<Entry> = two_projects().into_iter().map(Entry::from_project).collect();
    assert_eq!(entries[0].title, "Acme, Inc.");
    assert_eq!(entries[0].slug, "acme");
    assert_eq!(entries[1].title, "my-sandbox", "no proper name: the slug stands in");
    assert_eq!(entries[0].descriptions[0], "scrapbox.io/acme");
    assert_eq!(entries[0].descriptions[1], "非公開 · 20 members · business");
    let mut ix = Index::new(entries, 2, SortKey::Updated);
    ix.scope = Scope::Projects;
    ix.can_create = true; // would offer on the page list; must not here

    ix.set_filter("acme".into());
    assert!(matches!(ix.rows()[..], [Row::Page(e)] if e.slug == "acme"), "matched by slug");
    ix.set_filter("inc.".into());
    assert!(matches!(ix.rows()[..], [Row::Page(e)] if e.slug == "acme"), "matched by name");
    ix.set_filter("nothing-here".into());
    assert!(ix.rows().is_empty(), "no create row for a name that is not a project");
    assert!(!ix.offers_create());

    ix.begin_filter();
    ix.toggle_filter_mode();
    assert_eq!(ix.filter_mode, FilterMode::Title, "projects have no bodies to search");
}

/// `^o` over a project's pages goes one level up, Enter comes back
/// down, and both are places on the stack. A list that cannot be
/// fetched leaves the reader where they were.
#[test]
fn control_o_steps_up_to_the_projects_and_enter_steps_back_down() {
    use cosense::index::Scope;
    let ctx = offline_ctx();
    use_japanese();
    let mut app = page(&["A", "one"]);
    app.project = "proj".into();
    app.title = "A".into();
    app.index_project = "proj".into();
    app.index = Some(cosense::index::Index::new(
        vec![cosense::index::Entry { title: "B".into(), ..Default::default() }],
        1,
        cosense::index::SortKey::Updated,
    ));
    let pages = app.here();

    // No server: the page list stays, and the reason is said.
    handle_index_key(&mut app, &ctx, ctrl('o'));
    assert_eq!(app.index.as_ref().unwrap().scope, Scope::Pages);
    assert!(app.history.is_empty());
    assert!(app.toast_text().contains("プロジェクト一覧を取得できません"), "{}", app.toast_text());

    // With the list in hand (the session cache stands in for the
    // server), `^o` lists the projects newest first.
    app.projects_cache = Some(ProjectsCache { projects: two_projects(), at: Instant::now() });
    handle_index_key(&mut app, &ctx, ctrl('o'));
    let ix = app.index.as_ref().unwrap();
    assert_eq!(ix.scope, Scope::Projects);
    assert_eq!(ix.entries.iter().map(|e| e.slug.as_str()).collect::<Vec<_>>(), ["acme", "my-sandbox"]);
    assert_eq!(app.history.last(), Some(&pages), "the page list is where [ goes");
    assert!(app.index_project.is_empty(), "no project is being listed");

    // Enter on a project asks for its page list; offline, that fails,
    // and the projects stay on screen with the stack untouched.
    handle_index_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(app.index.as_ref().unwrap().scope, Scope::Projects);
    assert_eq!(app.history.len(), 1);

    // `^o` again is a refetch, not a move: nothing more on the stack.
    handle_index_key(&mut app, &ctx, ctrl('o'));
    assert_eq!(app.history.len(), 1);
    assert_eq!(app.index.as_ref().unwrap().scope, Scope::Projects);

    // `s` has no menu to open here.
    handle_index_key(&mut app, &ctx, key(KeyCode::Char('s')));
    assert!(app.index_sort_menu.is_none());
    assert!(app.toast_text().contains("更新の新しい順"), "{}", app.toast_text());

    // `[` walks back down into the page list, exactly as it was.
    handle_index_key(&mut app, &ctx, key(KeyCode::Char('[')));
    assert_eq!(app.here(), pages);
    assert!(matches!(app.forward.last(), Some(Place::Index { state, .. }) if state.scope == Scope::Projects));
}

/// The header names the level, not a project, and wears no read-only
/// badge (nothing is written here). Each row shows the proper name
/// with the slug beside it.
#[test]
fn the_projects_list_draws_its_level_and_the_slugs() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = offline_ctx();
    use_japanese();
    let mut app = page(&["A", "one"]);
    app.projects_cache = Some(ProjectsCache { projects: two_projects(), at: Instant::now() });
    open_projects(&mut app, &ctx, true);
    let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let screen = format!("{:?}", t.backend().buffer());
    assert!(screen.contains("プロジェクト — 2 projects"), "{screen}");
    assert!(!screen.contains("読み取り専用"), "{screen}");
    assert!(screen.contains("Acme, Inc.  acme"), "{screen}");
    assert!(screen.contains("my-sandbox"), "{screen}");
    assert!(screen.contains("^o 取り直す"), "{screen}");
}
