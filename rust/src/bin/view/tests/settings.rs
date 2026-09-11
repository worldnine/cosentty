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

fn row<'a>(app: &'a App, field: SettingField) -> &'a SettingRow {
    view(app)
        .rows
        .iter()
        .find(|r| r.field == field)
        .unwrap_or_else(|| panic!("no row for {field:?}"))
}

/// Move the cursor onto `field` (from wherever it is).
fn goto(app: &mut App, ctx: &Ctx, field: SettingField) {
    for _ in 0..20 {
        handle_key(app, ctx, key(KeyCode::Char('k')));
    }
    for _ in 0..20 {
        if view(app).rows[view(app).cursor].field == field {
            return;
        }
        handle_key(app, ctx, key(KeyCode::Char('j')));
    }
    panic!("no row {field:?}");
}

fn file(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("config.toml")).unwrap_or_default()
}

fn type_str(app: &mut App, ctx: &Ctx, s: &str) {
    for ch in s.chars() {
        handle_key(app, ctx, key(KeyCode::Char(ch)));
    }
}

fn picker_items(app: &App) -> (Vec<String>, usize) {
    match &view(app).mode {
        SettingsMode::Pick { items, cursor, .. } => (items.clone(), *cursor),
        other => panic!("picker expected: {other:?}"),
    }
}

/// Press `j` until the picker's cursor sits on `item`.
fn pick(app: &mut App, ctx: &Ctx, item: &str) {
    let (items, cursor) = picker_items(app);
    let want = items
        .iter()
        .position(|i| i == item)
        .unwrap_or_else(|| panic!("{item} not offered: {items:?}"));
    // Headings are stepped over, so a press may move more than one row:
    // press until the cursor is there.
    let down = want > cursor;
    for _ in 0..items.len() {
        if picker_items(app).1 == want {
            return;
        }
        handle_key(app, ctx, key(KeyCode::Char(if down { 'j' } else { 'k' })));
    }
    assert_eq!(picker_items(app).1, want, "could not reach {item}");
}

#[test]
fn comma_opens_both_sections_with_the_defaults_named_as_such() {
    let (ctx, _dir) = settings_ctx();
    let mut app = page(&["title", "one"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    let v = view(&app);
    assert_eq!(v.project.as_deref(), Some("proj"));
    assert!(!v.api_readable);
    assert!(!v.file_broken);
    assert_eq!(v.rows.len(), 10, "seven view rows, three project rows");
    assert!(
        v.rows
            .iter()
            .all(|r| matches!(r.origin, Origin::Default | Origin::Auto)),
        "{:?}",
        v.rows
    );
    assert_eq!(row(&app, SettingField::ProjectDisplayName).value, "proj");
    assert_eq!(row(&app, SettingField::ProjectImages).value, "gcs");
    assert_eq!(row(&app, SettingField::Lang).value, "ja");
    let (items, cursor) = settings_items(v, 80);
    assert!(items[0].contains("読めません"), "{items:?}");
    assert!(items.iter().any(|i| i.contains("── この端末")), "{items:?}");
    assert!(
        items.iter().any(|i| i.contains("── プロジェクト: proj")),
        "{items:?}"
    );
    assert!(
        items[cursor].contains("言語"),
        "cursor on the first row: {:?}",
        items[cursor]
    );
    for i in &items {
        assert!(str_width(i) <= 80, "fits: {i}");
    }

    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(app.overlay.is_none());
}

#[test]
fn picking_a_project_theme_writes_the_file_and_recolours_the_page() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title", "[link]"]);
    let plain_link = app.palette.link;
    let plain_header = app.header_colors;
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::ProjectTheme);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let (items, cursor) = picker_items(&app);
    assert_eq!(cursor, 0, "nothing set: the cursor is on (unset)");
    assert_eq!(items.len(), 1 + cosense::theme::COSENSE_THEMES.len());
    pick(&mut app, &ctx, "green");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));

    assert_eq!(file(&dir), "[project.proj]\ntheme = \"green\"\n");
    assert_eq!(ctx.config().project_theme("proj").as_deref(), Some("green"));
    assert!(matches!(view(&app).mode, SettingsMode::List));
    assert_eq!(row(&app, SettingField::ProjectTheme).value, "green");
    assert_eq!(row(&app, SettingField::ProjectTheme).origin, Origin::File);
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
    assert_eq!(
        row(&app, SettingField::ProjectTheme).origin,
        Origin::Default
    );
    assert_eq!(app.palette.link, plain_link);
    assert_eq!(app.header_colors, plain_header);
}

#[test]
fn the_display_name_is_typed_and_shows_in_the_header_at_once() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::ProjectDisplayName);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(matches!(view(&app).mode, SettingsMode::Input { .. }));
    type_str(&mut app, &ctx, "研究ノート");
    // Pasting lands in the field too.
    handle_paste(&mut app, &ctx, "（第2版）\n");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));

    assert_eq!(app.project_display, "研究ノート（第2版）");
    assert_eq!(
        row(&app, SettingField::ProjectDisplayName).origin,
        Origin::File
    );
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
    goto(&mut app, &ctx, SettingField::ProjectImages);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "gyazo");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(
        matches!(
            view(&app).mode,
            SettingsMode::Input {
                target: Target::Project(cosense::config::ProjectKey::GyazoTeam),
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
    assert_eq!(
        row(&app, SettingField::ProjectImages).value,
        "acme.gyazo.com"
    );
    assert_eq!(row(&app, SettingField::ProjectImages).origin, Origin::File);

    // Back to gcs: the org goes with it.
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "gcs");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let c = ctx.config().upload_choice("proj");
    assert_eq!(c.images.as_deref(), Some("gcs"));
    assert_eq!(c.gyazo_team, None);
    assert!(!file(&dir).contains("gyazo_team"), "{}", file(&dir));
}

#[test]
fn a_broken_file_is_shown_and_never_written() {
    let (ctx, dir) = settings_ctx();
    std::fs::write(dir.path().join("config.toml"), "[upload\n").unwrap();
    ctx.set_config_error(Some("config.toml: broken".into()));
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert!(view(&app).file_broken);
    assert!(settings_notes(view(&app))
        .iter()
        .any(|n| n.contains("保存しません")));
    goto(&mut app, &ctx, SettingField::ProjectTheme);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "blue");
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
    assert_eq!(view(&app).project.as_deref(), Some("proj"));
    // Keys go to the screen, not the list.
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    assert_eq!(view(&app).cursor, 1);
    handle_key(&mut app, &ctx, key(KeyCode::Char('q')));
    assert!(app.overlay.is_none());
    assert!(app.index.is_some(), "q closed the screen, not the index");
}

/// The projects list is above any one project: `,` there shows only this
/// terminal's section, and never a screen for the empty slug.
#[test]
fn the_projects_list_shows_only_the_terminal_section() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    app.index = Some(cosense::index::Index::new(
        Vec::new(),
        0,
        cosense::index::SortKey::default(),
    ));
    app.index_project = String::new();
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert_eq!(view(&app).project, None);
    assert_eq!(view(&app).rows.len(), 7);
    let (items, _) = settings_items(view(&app), 80);
    assert!(
        !items.iter().any(|i| i.contains("── プロジェクト")),
        "{items:?}"
    );
    // Saving a view key here is fine and writes nothing project-shaped.
    goto(&mut app, &ctx, SettingField::Preview);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "off");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[view]\npreview = \"off\"\n");
    assert_eq!(
        ctx.preview(),
        cosense::index::PreviewMode::Off,
        "in force at once"
    );
}

/// The colour theme is tried live: the cursor's choice is what the page
/// wears; Esc restores what the file said, Enter writes the choice.
#[test]
fn the_colour_theme_is_previewed_and_esc_reverts() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title", "[link]"]);
    let before = app.palette;
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::Theme);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let (items, _) = picker_items(&app);
    assert!(items.iter().any(|i| i == "Dracula"), "{items:?}");
    pick(&mut app, &ctx, "Dracula");
    assert_eq!(ctx.view().theme.0.as_deref(), Some("Dracula"), "previewed");
    assert_ne!(app.palette, before, "the page wears it already");
    assert_eq!(file(&dir), "", "nothing written yet");
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert_eq!(ctx.view().theme.0, None, "back to before");
    assert_eq!(file(&dir), "");

    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "Dracula");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[view]\ntheme = \"Dracula\"\n");
    assert_eq!(ctx.view().theme, (Some("Dracula".into()), Origin::File));
    assert_eq!(row(&app, SettingField::Theme).origin, Origin::File);
    assert_ne!(app.palette, before);

    handle_key(&mut app, &ctx, key(KeyCode::Char('d')));
    assert_eq!(file(&dir).trim(), "");
    assert_eq!(ctx.view().theme.1, Origin::Default);
}

/// A flag keeps winning after the file is written: the row says so and
/// shows the flag's value, not the file's.
#[test]
fn a_flag_still_wins_over_a_freshly_saved_file_value() {
    let (ctx, dir) = settings_ctx();
    ctx.with_view(|v| v.flags.lang = Some("en".into()));
    let cfg = ctx.config();
    let next = ctx.view().reresolve(&cfg.view, &mut Vec::new());
    ctx.with_view(|v| *v = next);
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    let r = row(&app, SettingField::Lang);
    assert_eq!((r.value.as_str(), r.origin), ("en", Origin::Flag));
    assert!(
        r.note.as_deref().unwrap_or("").contains("起動オプション"),
        "{:?}",
        r.note
    );

    goto(&mut app, &ctx, SettingField::Lang);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "ja");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[view]\nlang = \"ja\"\n", "saved for next time");
    let r = row(&app, SettingField::Lang);
    assert_eq!(
        (r.value.as_str(), r.origin),
        ("en", Origin::Flag),
        "still the flag"
    );
}

/// The downloads row is a path field; `~` is expanded for use and kept as
/// written in the file.
#[test]
fn the_download_dir_is_typed_and_expanded() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::DownloadDir);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(app.ime_guard.is_none(), "a path is typed in ASCII");
    type_str(&mut app, &ctx, "~/cosense-dl");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[view]\ndownload_dir = \"~/cosense-dl\"\n");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        ctx.download_dir(),
        std::path::PathBuf::from(format!("{home}/cosense-dl"))
    );
    assert_eq!(row(&app, SettingField::DownloadDir).origin, Origin::File);
}

/// `e` hands the file to `$EDITOR` — the key handler only asks for it.
#[test]
fn e_asks_for_the_editor() {
    let (ctx, _dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert!(matches!(
        handle_key(&mut app, &ctx, key(KeyCode::Char('e'))),
        Action::EditConfig
    ));
    assert!(
        matches!(app.overlay, Some(Overlay::Settings(_))),
        "the screen stays"
    );
}

/// After the editor, the file is read again: a fixed file unblocks saving,
/// and its values are in force.
#[test]
fn reload_after_the_editor_applies_the_file() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title"]);
    ctx.set_config_error(Some("broken".into()));
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    assert!(view(&app).file_broken);
    // The user fixed it in the editor (simulated), and set a value.
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[view]\npreview = \"on\"\n[project.proj]\ndisplay_name = \"P\"\n",
    )
    .unwrap();
    // `Config::load` reads `Config::path`; point it at the scratch file.
    reload_config_from(&mut app, &ctx, &path);
    assert!(!view(&app).file_broken);
    assert_eq!(ctx.preview(), cosense::index::PreviewMode::On);
    assert_eq!(app.project_display, "P");
    assert_eq!(row(&app, SettingField::Preview).origin, Origin::File);
}

/// `[view]` is resolved flag > env > file > default, and a file value that
/// does not parse is reported and dropped rather than failing the file.
#[test]
fn view_settings_follow_flag_env_file_default() {
    use cosense::config::Config;
    let file = Config::parse(
        "[view]\nlang = \"ja\"\ntheme = \"Nord\"\nappearance = \"light\"\npreview = \"off\"\nime = \"off\"\ndownload_dir = \"~/dl\"\ndiagram_text = \"bogus\"\n",
    )
    .unwrap()
    .view;
    let env = |k: &str| match k {
        "HOME" => Some("/home/me".to_string()),
        "COSENSE_LANG" => Some("en".to_string()),
        _ => None,
    };
    let mut notes = Vec::new();
    let flags = ViewFlags {
        theme: Some("Dracula".into()),
        ..ViewFlags::default()
    };
    let v = ViewSettings::resolve(&flags, &env, &file, None, "/tmp".into(), &mut notes);
    assert_eq!(
        v.lang,
        (cosense::lang::Lang::En, Origin::Env),
        "env beats file"
    );
    assert_eq!(
        v.theme,
        (Some("Dracula".into()), Origin::Flag),
        "flag beats file"
    );
    assert_eq!(v.appearance, (Appearance::Light, Origin::File));
    assert!(v.light);
    assert_eq!(v.preview, (cosense::index::PreviewMode::Off, Origin::File));
    assert_eq!(v.ime, (cosense::ime::ImeMode::Off, Origin::File));
    assert_eq!(
        v.download_dir,
        (std::path::PathBuf::from("/home/me/dl"), Origin::File),
        "~ is expanded"
    );
    assert_eq!(
        v.diagram_text,
        (mmd_text::DiagramText::Box, Origin::Default),
        "a bad file value falls back"
    );
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("diagram_text") && notes[0].contains("bogus"),
        "{notes:?}"
    );

    // Nothing anywhere: the defaults, and the detected background decides.
    let none = |_: &str| None;
    let v = ViewSettings::resolve(
        &ViewFlags::default(),
        &none,
        &Default::default(),
        Some((250, 250, 250)),
        "/tmp".into(),
        &mut Vec::new(),
    );
    assert_eq!(v.lang.1, Origin::Default);
    assert_eq!(v.appearance, (Appearance::Auto, Origin::Default));
    assert!(v.light, "auto follows the terminal");
    assert_eq!(v.terminal_bg, (250, 250, 250));
    assert_eq!(v.download_dir, ("/tmp".into(), Origin::Auto));
}

/// A theme name nothing answers to is not swallowed: the resolution says
/// so, and the row says the default is in use.
#[test]
fn an_unknown_theme_name_is_reported_not_swallowed() {
    let (ctx, _dir) = settings_ctx();
    let mut notes = Vec::new();
    let flags = ViewFlags {
        theme: Some("Tokyo Night Storm".into()),
        ..ViewFlags::default()
    };
    let v = ViewSettings::resolve(
        &flags,
        &|_| None,
        &Default::default(),
        None,
        "/tmp".into(),
        &mut notes,
    );
    assert!(v.theme_missing);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("Tokyo Night Storm") && notes[0].contains("themes"),
        "{notes:?}"
    );
    ctx.with_view(|cur| *cur = v);

    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    let r = row(&app, SettingField::Theme);
    assert_eq!(r.value, "Tokyo Night Storm");
    assert_eq!(r.origin, Origin::Flag);
    assert!(
        r.note.as_deref().unwrap_or("").contains("見つからず"),
        "{:?}",
        r.note
    );

    // A known one clears it.
    let v = ViewSettings::resolve(
        &ViewFlags {
            theme: Some("Nord".into()),
            ..ViewFlags::default()
        },
        &|_| None,
        &Default::default(),
        None,
        "/tmp".into(),
        &mut Vec::new(),
    );
    assert!(!v.theme_missing);
}

/// The theme picker groups names under a heading per source; headings are
/// drawn but the cursor steps over them and none can be chosen.
#[test]
fn the_theme_picker_groups_by_source_and_skips_headings() {
    let (ctx, _dir) = settings_ctx();
    let mut app = page(&["title"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::Theme);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    let (items, headings) = match &view(&app).mode {
        SettingsMode::Pick {
            items, headings, ..
        } => (items.clone(), headings.clone()),
        other => panic!("{other:?}"),
    };
    assert!(!headings.is_empty(), "{items:?}");
    let built_in = headings.last().copied().unwrap();
    assert!(items[built_in].contains("同梱"), "{:?}", items[built_in]);
    assert!(items.len() > built_in + 5, "the embedded names follow");
    // From (unset), one step down lands on the first NAME, not a heading.
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    let cursor = picker_items(&app).1;
    assert!(
        !headings.contains(&cursor),
        "cursor {cursor} is a heading: {items:?}"
    );
    assert!(cursor > 0);
    // Stepping back stops on (unset).
    handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
    assert_eq!(picker_items(&app).1, 0);
    handle_key(&mut app, &ctx, key(KeyCode::Char('k')));
    assert_eq!(picker_items(&app).1, 0, "stays at the top");
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
}

/// The project's theme is tried live too: the header and links take the
/// cursor's choice; Esc puts back what was in force; Enter writes it.
#[test]
fn the_project_theme_is_previewed_and_esc_reverts() {
    let (ctx, dir) = settings_ctx();
    let mut app = page(&["title", "[link]"]);
    let plain = (app.palette, app.header_colors);
    handle_key(&mut app, &ctx, key(KeyCode::Char(',')));
    goto(&mut app, &ctx, SettingField::ProjectTheme);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "green");
    assert_ne!((app.palette, app.header_colors), plain, "worn while trying");
    assert_eq!(ctx.project_theme("proj").as_deref(), Some("green"));
    assert_eq!(file(&dir), "", "nothing written yet");
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert_eq!((app.palette, app.header_colors), plain, "Esc puts it back");
    assert_eq!(ctx.project_theme("proj"), None);
    assert_eq!(
        row(&app, SettingField::ProjectTheme).origin,
        Origin::Default
    );

    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    pick(&mut app, &ctx, "green");
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert_eq!(file(&dir), "[project.proj]\ntheme = \"green\"\n");
    assert_ne!((app.palette, app.header_colors), plain);
    assert_eq!(row(&app, SettingField::ProjectTheme).origin, Origin::File);
}
