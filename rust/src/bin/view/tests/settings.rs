//! The settings screen (`,`): rows, origins, and saving to config.toml.

use super::support::*;
use crate::*;
use cosense::config::Origin;

/// A Ctx whose settings file is a scratch one, and whose server is
/// unreachable — the private-project-without-a-sid situation the screen
/// exists for.
fn settings_ctx() -> (Ctx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = offline_ctx();
    ctx.config_path = Some(dir.path().join("config.toml"));
    // "Asked, and the server had nothing" — so no fetch is attempted.
    ctx.project_settings
        .lock()
        .unwrap()
        .insert("proj".into(), None);
    (ctx, dir)
}

fn view(app: &App) -> &SettingsView {
    match app.overlay.as_ref() {
        Some(Overlay::Settings(v)) => v,
        Some(_) => panic!("settings screen expected, another overlay is open"),
        None => panic!("settings screen expected, none is open"),
    }
}

fn file(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default()
}

fn type_str(app: &mut App, ctx: &Ctx, s: &str) {
    for ch in s.chars() {
        handle_key(app, ctx, key(KeyCode::Char(ch)));
    }
}

#[test]
fn comma_opens_the_screen_with_the_defaults_named_as_such() {
    let (ctx, _dir) = settings_ctx();
    let mut app = page(&["title", "one"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    let v = view(&app);
    assert_eq!(v.project, "proj");
    assert!(!v.api_readable);
    assert!(!v.file_broken);
    assert_eq!(v.rows.len(), 3);
    assert!(
        v.rows.iter().all(|r| r.origin == Origin::Default),
        "{:?}",
        v.rows
    );
    assert_eq!(
        v.rows[1].value, "proj",
        "the slug until something better is known"
    );
    assert_eq!(v.rows[2].value, "gcs");
    let notes = settings_notes(v);
    assert!(notes[0].contains("読めません"), "{notes:?}");
    assert!(
        settings_lines(v)[0].contains("← -"),
        "{:?}",
        settings_lines(v)
    );

    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(app.overlay.is_none());
}

#[test]
fn picking_a_theme_writes_the_file_and_recolours_the_page() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title", "[link]"]);
    let plain_link = app.palette.link;
    let plain_header = app.header_colors;
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let (items, cursor) = match &view(&app).mode {
        SettingsMode::Pick { items, cursor, .. } => (items.clone(), *cursor),
        other => panic!("{other:?}"),
    };
    assert_eq!(cursor, 0, "nothing set: the cursor is on (unset)");
    assert_eq!(items.len(), 1 + cosense::theme::COSENSE_THEMES.len());
    let want = items.iter().position(|i| i == "green").unwrap();
    for _ in 0..want {
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    }
    handle_key(&mut app, &ctx, key(KeyCode::Enter));

    assert_eq!(file(&dir), "[project.proj]\ntheme = \"green\"\n");
    assert_eq!(ctx.config().project_theme("proj").as_deref(), Some("green"));
    let v = view(&app);
    assert!(matches!(v.mode, SettingsMode::List));
    assert_eq!(v.rows[0].value, "green");
    assert_eq!(v.rows[0].origin, Origin::File);
    assert_ne!(
        app.palette.link, plain_link,
        "links wear the theme's colour now"
    );
    assert_ne!(app.header_colors, plain_header, "and so does the header");
    assert!(app.telomere_tint.is_some());
    assert!(
        app.note_text().contains("theme = green"),
        "{}",
        app.note_text()
    );

    // `d` takes it back out — of the file and of the page.
    handle_key(&mut app, &ctx, key(KeyCode::Char('d')));
    assert_eq!(file(&dir).trim(), "");
    assert_eq!(view(&app).rows[0].origin, Origin::Default);
    assert_eq!(app.palette.link, plain_link);
    assert_eq!(app.header_colors, plain_header);
}

#[test]
fn the_display_name_is_typed_and_shows_in_the_header_at_once() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(matches!(view(&app).mode, SettingsMode::Input { .. }));
    type_str(&mut app, &ctx, "研究ノート");
    // Pasting lands in the field too.
    handle_paste(&mut app, &ctx, "（第2版）\n");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));

    assert_eq!(app.project_display, "研究ノート（第2版）");
    assert_eq!(view(&app).rows[1].origin, Origin::File);
    assert!(
        file(&dir).contains("display_name = \"研究ノート（第2版）\""),
        "{}",
        file(&dir)
    );

    // Reopen, clear the text: back to the slug.
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    for _ in 0..20 {
        handle_key(&mut app, &ctx, key(KeyCode::Backspace));
    }
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(app.project_display, "proj");
    assert!(!file(&dir).contains("display_name"), "{}", file(&dir));
}

#[test]
fn gyazo_asks_for_the_org_and_gcs_forgets_it() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    // (unset) gcs gyazo
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(
        matches!(
            view(&app).mode,
            SettingsMode::Input {
                key: cosense::config::ProjectKey::GyazoTeam,
                ..
            }
        ),
        "{:?}",
        view(&app).mode
    );
    type_str(&mut app, &ctx, "acme");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let c = ctx.config().upload_choice("proj");
    assert_eq!(c.images.as_deref(), Some("gyazo"));
    assert_eq!(c.gyazo_team.as_deref(), Some("acme"));
    assert_eq!(view(&app).rows[2].value, "acme.gyazo.com");
    assert_eq!(view(&app).rows[2].origin, Origin::File);

    // Back to gcs: the org goes with it.
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let c = ctx.config().upload_choice("proj");
    assert_eq!(c.images.as_deref(), Some("gcs"));
    assert_eq!(c.gyazo_team, None);
    assert!(!file(&dir).contains("gyazo_team"), "{}", file(&dir));
}

#[test]
fn a_broken_file_is_shown_and_never_written() {
    let (mut ctx, dir) = settings_ctx();
    std::fs::write(dir.path().join("config.toml"), "[upload\n").unwrap();
    ctx.config_error = Some("config.toml: broken".into());
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert!(view(&app).file_broken);
    assert!(settings_notes(view(&app))
        .iter()
        .any(|n| n.contains("保存しません")));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[upload\n", "left alone");
    assert!(
        app.toast_text().contains("保存できません"),
        "{}",
        app.toast_text()
    );
}

#[test]
fn the_index_opens_the_listed_projects_settings() {
    let (ctx, _dir) = settings_ctx();
    let mut app = page(&["title"]);
    app.index = Some(cosense::index::Index::new(
        Vec::new(),
        0,
        cosense::index::SortKey::default(),
    ));
    app.index_project = "proj".into();
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert_eq!(view(&app).project, "proj");
    // Keys go to the screen, not the list.
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    assert_eq!(view(&app).cursor, 1);
    handle_key(&mut app, &ctx, key(KeyCode::Char('q')));
    assert!(app.overlay.is_none());
    assert!(app.index.is_some(), "q closed the screen, not the index");
}

/// The projects list is above any one project: `,` there has nothing to
/// open, and must not open a screen for the empty slug.
#[test]
fn the_projects_list_has_no_settings_to_open() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    app.index = Some(cosense::index::Index::new(
        Vec::new(),
        0,
        cosense::index::SortKey::default(),
    ));
    app.index_project = String::new();
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert!(app.overlay.is_none());
    assert!(app.toast_text().contains("プロジェクトを開いてから"), "{}", app.toast_text());
    assert_eq!(file(&dir), "");
}
