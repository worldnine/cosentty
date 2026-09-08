use crate::tests::support::*;
use crate::*;

#[test]
fn set_page_carries_the_page_palette() {
    use cosense::theme::Palette;
    let ctx = test_ctx();
    let mut app = page(&["t"]);
    // A themed project's tinted palette arrives with the page; from then
    // on every re-render of THIS page keeps it.
    let mut tinted = ctx.palette();
    tinted.link = Color::Rgb(1, 2, 3);
    app.set_page(
        Loaded {
            project: "proj".into(),
            title: "t".into(),
            page_id: "P2".into(),
            header_colors: HeaderColors::fallback(),
            project_display: String::new(),
            lines: Vec::new(),
            blocks: Vec::new(),
            srcs: Vec::new(),
            hits: Vec::new(),
            related: Vec::new(),
            facts: PageFacts::default(),
            read_at: None,
            open_stamp: 0,
            palette: tinted,
            telomere_tint: None,
            editable: true,
            links: LinkTruth::default(),
        },
        &ctx,
    );
    assert_eq!(app.palette.link, Color::Rgb(1, 2, 3));
    let _ = Palette::for_light(false); // the base is only a default
}

#[test]
fn the_rerender_keeps_the_page_palette() {
    let ctx = test_ctx();
    let mut app = page(&["t", "[gone]"]);
    // The page arrived with its theme's colours; a link that is "nowhere"
    // paints in the missing colour — and re-rendering the mutated model
    // must not fall back to the terminal scheme mid-page.
    app.palette.link = Color::Rgb(1, 2, 3);
    app.palette.link_missing = Color::Rgb(4, 5, 6);
    let mut links = LinkTruth::seed(["gone"], []);
    links.learn("gone", false);
    app.links = links;
    rerender(&mut app, &ctx);
    let painted = app.blocks.iter().any(|b| match b {
        cosense::render::Block::Text(line) => line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::Rgb(4, 5, 6))),
        _ => false,
    });
    assert!(painted, "the missing-link colour survives the re-render");
}

#[test]
fn the_index_excerpt_wears_the_listed_projects_theme() {
    let ctx = test_ctx();
    // The listed project is green; its settings are already known (an
    // index arrival reads them for its display name). The preview must
    // not paint with the bare terminal scheme.
    ctx.project_settings.lock().unwrap().insert(
        "proj".to_string(),
        Some(cosense::api::ProjectSettings {
            display_name: String::new(),
            theme: Some("green".into()),
            upload_image_to: None,
            gyazo_teams_name: None,
        }),
    );
    let mut app = page(&["t"]);
    app.index_project = "proj".into();
    app.index = Some(cosense::index::Index {
        entries: vec![cosense::index::Entry {
            title: "p".into(),
            slug: String::new(),
            updated: now_secs(),
            created: 0,
            accessed: 0,
            linked: 0,
            views: 0,
            descriptions: vec!["see [linked]".into()],
            unread: false,
            matched: Vec::new(),
        }],
        scope: cosense::index::Scope::Pages,
        ..Default::default()
    });
    let lines = index_preview_lines(&app, &ctx, 60);
    let (green, _) = cosense::theme::cosense_link_colors("green").unwrap();
    assert_ne!(
        green,
        ctx.palette().link,
        "a themed link is a different colour"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style.fg == Some(green))),
        "the excerpt's links wear the listed project's theme"
    );
}

#[test]
fn the_index_header_wears_the_listed_projects_theme() {
    let ctx = test_ctx();
    ctx.project_settings.lock().unwrap().insert(
        "proj".to_string(),
        Some(cosense::api::ProjectSettings {
            display_name: String::new(),
            theme: Some("green".into()),
            upload_image_to: None,
            gyazo_teams_name: None,
        }),
    );
    let mut app = page(&["t"]);
    app.index_project = "proj".into();
    app.index = Some(cosense::index::Index {
        scope: cosense::index::Scope::Pages,
        ..Default::default()
    });
    // The list answers to ITS project: green's navbar, not the page the
    // reader came from.
    let want = cosense::theme::project_header_colors(Some("green"), ctx.terminal_bg());
    let got = app.chrome_colors(&ctx);
    assert_eq!(got.bg, want.1);
    // The page's own colours are untouched under the list.
    assert_ne!(app.header_colors.bg, got.bg, "no permanent overwrite");
}

#[test]
fn the_projects_list_header_is_a_menu_not_a_project() {
    // ^o^o: the projects list is a picker. It wears the usual menu title's
    // look — no project's theme, not even the current project's.
    let ctx = test_ctx();
    let mut app = page(&["t"]);
    app.index = Some(cosense::index::Index {
        scope: cosense::index::Scope::Projects,
        ..Default::default()
    });
    let c = app.chrome_colors(&ctx);
    assert_eq!(c.fg, Color::Black);
    assert_eq!(c.bg, CHROME_ACCENT);
}

/// With the server unreadable, `[project.<slug>]` in config.toml stands in
/// for the theme and the display name — and says so.
#[test]
fn the_file_stands_in_for_unreadable_project_settings() {
    use cosense::config::Origin;
    let ctx = offline_ctx();
    // The cache says "asked, nothing there" so no fetch is attempted.
    ctx.project_settings
        .lock()
        .unwrap()
        .insert("acme".into(), None);
    assert_eq!(ctx.project_theme_with("acme"), (None, Origin::Default));
    assert_eq!(
        ctx.project_display_with("acme"),
        ("acme".to_string(), Origin::Default)
    );

    ctx.set_config(
        cosense::config::Config::parse(
            "[project.acme]\ntheme = \"paper-dark\"\ndisplay_name = \"ACME\"\n",
        )
        .unwrap(),
    );
    assert_eq!(
        ctx.project_theme_with("acme"),
        (Some("paper-dark".into()), Origin::File)
    );
    assert_eq!(
        ctx.project_display_with("acme"),
        ("ACME".to_string(), Origin::File)
    );

    // The API, once it answers, wins over the file.
    ctx.project_settings.lock().unwrap().insert(
        "acme".into(),
        Some(cosense::api::ProjectSettings {
            display_name: "Acme Corp".into(),
            theme: Some("blue".into()),
            upload_image_to: None,
            gyazo_teams_name: None,
        }),
    );
    assert_eq!(
        ctx.project_theme_with("acme"),
        (Some("blue".into()), Origin::Api)
    );
    assert_eq!(ctx.project_display("acme"), "Acme Corp");
}
