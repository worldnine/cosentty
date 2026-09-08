//! The settings screen (`,`): what the viewer knows about the current
//! project — its theme, its proper name, where pasted images go — and where
//! each answer came from (`api` / `file` / the built-in default). Every
//! change is written to `config.toml` at once (`[project.<slug>]`,
//! `Config::save_project_key`) and takes effect on screen at once. See
//! `docs/PLAN-settings-ui.md`.

use super::*;
use cosense::config::{Origin, ProjectKey};

/// Which of the three rows a key press is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingField {
    Theme,
    DisplayName,
    Images,
}

impl SettingField {
    const ALL: [SettingField; 3] = [
        SettingField::Theme,
        SettingField::DisplayName,
        SettingField::Images,
    ];

    fn label(self) -> String {
        match self {
            SettingField::Theme => t!("テーマ", "theme"),
            SettingField::DisplayName => t!("表示名", "display name"),
            SettingField::Images => t!("画像の保存先", "images go to"),
        }
    }
}

/// One row of the screen: the value in force and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingRow {
    pub(crate) field: SettingField,
    pub(crate) value: String,
    pub(crate) origin: Origin,
}

/// What the screen is doing: showing the rows, or asking for one value.
#[derive(Debug)]
pub(crate) enum SettingsMode {
    List,
    /// A short menu (theme names; gcs / gyazo). `items[0]` is always
    /// "(unset)" — removing the file's value.
    Pick {
        field: SettingField,
        items: Vec<String>,
        cursor: usize,
    },
    /// A one-line text field (display name; the Gyazo Teams org). Empty
    /// text removes the file's value.
    Input {
        key: ProjectKey,
        input: Input,
    },
}

#[derive(Debug)]
pub(crate) struct SettingsView {
    pub(crate) project: String,
    pub(crate) cursor: usize,
    pub(crate) rows: Vec<SettingRow>,
    /// `/api/projects/<slug>` answered — the theme and name rows then show
    /// the web's values and a file value would not be used.
    pub(crate) api_readable: bool,
    /// The file exists and does not parse: nothing will be written over it.
    pub(crate) file_broken: bool,
    pub(crate) mode: SettingsMode,
}

/// Open the screen for `project` (the page's, or the index's).
pub(crate) fn open_settings(app: &mut App, ctx: &Ctx, project: &str) {
    let mut view = SettingsView {
        project: project.to_string(),
        cursor: 0,
        rows: Vec::new(),
        api_readable: false,
        file_broken: ctx.config_error.is_some(),
        mode: SettingsMode::List,
    };
    refresh_rows(&mut view, ctx);
    app.overlay = Some(Overlay::Settings(view));
}

/// Recompute the rows from what the viewer now knows.
fn refresh_rows(view: &mut SettingsView, ctx: &Ctx) {
    let project = view.project.as_str();
    view.api_readable = ctx.project_settings(project).is_some();
    let (theme, theme_from) = ctx.project_theme_with(project);
    let (name, name_from) = ctx.project_display_with(project);
    let (dest, decided) = cosense::upload::Destination::resolve_with(
        &ctx.config(),
        project,
        ctx.project_settings(project).as_ref(),
    );
    let dest_from = match decided {
        cosense::upload::Decided::File => Origin::File,
        cosense::upload::Decided::Project => Origin::Api,
        cosense::upload::Decided::Default => Origin::Default,
    };
    view.rows = vec![
        SettingRow {
            field: SettingField::Theme,
            value: theme.unwrap_or_else(|| t!("(なし)", "(none)")),
            origin: theme_from,
        },
        SettingRow {
            field: SettingField::DisplayName,
            value: name,
            origin: name_from,
        },
        SettingRow {
            field: SettingField::Images,
            value: dest.label(),
            origin: dest_from,
        },
    ];
}

/// The lines the panel shows above the rows: where the values come from,
/// and whether the file can be written. Exposed for the drawing code.
pub(crate) fn settings_notes(view: &SettingsView) -> Vec<String> {
    let mut notes = Vec::new();
    if view.api_readable {
        notes.push(t!(
            "プロジェクト設定は API から読めています。テーマと表示名は API が優先されます",
            "project settings are readable from the API; theme and name follow it"
        ));
    } else {
        notes.push(t!(
            "プロジェクト設定を API から読めません（非公開・sid なし）。file の値を使います",
            "project settings unreadable from the API (private, no sid); file values are used"
        ));
    }
    if view.file_broken {
        notes.push(t!(
            "config.toml が読めないので保存しません（直してから）",
            "config.toml does not parse; nothing will be saved until it is fixed"
        ));
    }
    notes
}

/// The rows as the panel prints them: label, value, origin tag.
pub(crate) fn settings_lines(view: &SettingsView) -> Vec<String> {
    view.rows
        .iter()
        .map(|r| {
            format!(
                "{:<14} {:<24} ← {}",
                r.field.label(),
                r.value,
                r.origin.tag()
            )
        })
        .collect()
}

/// Keys while the screen is open. Returns whether the key was taken.
pub(crate) fn handle_settings_key(
    app: &mut App,
    ctx: &Ctx,
    code: KeyCode,
    mods: KeyModifiers,
) -> bool {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let Some(Overlay::Settings(view)) = app.overlay.as_mut() else {
        return false;
    };
    match &mut view.mode {
        SettingsMode::List => {
            let last = SettingField::ALL.len() - 1;
            match code {
                KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
                KeyCode::Char('?') => app.overlay = Some(Overlay::Help),
                KeyCode::Down | KeyCode::Char('j') => view.cursor = (view.cursor + 1).min(last),
                KeyCode::Char('n') if ctrl => view.cursor = (view.cursor + 1).min(last),
                KeyCode::Up | KeyCode::Char('k') => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Char('p') if ctrl => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Enter => {
                    let field = SettingField::ALL[view.cursor.min(last)];
                    begin_change(app, ctx, field);
                }
                // `d`: back to the default (removes the file's value).
                KeyCode::Char('d') => {
                    let field = SettingField::ALL[view.cursor.min(last)];
                    let project = view.project.clone();
                    let keys: &[ProjectKey] = match field {
                        SettingField::Theme => &[ProjectKey::Theme],
                        SettingField::DisplayName => &[ProjectKey::DisplayName],
                        SettingField::Images => &[ProjectKey::Images, ProjectKey::GyazoTeam],
                    };
                    for k in keys {
                        if !save(app, ctx, &project, *k, None) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        SettingsMode::Pick {
            field,
            items,
            cursor,
        } => {
            let last = items.len().saturating_sub(1);
            match code {
                KeyCode::Esc | KeyCode::Char('q') => view.mode = SettingsMode::List,
                KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1).min(last),
                KeyCode::Char('n') if ctrl => *cursor = (*cursor + 1).min(last),
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Char('p') if ctrl => *cursor = cursor.saturating_sub(1),
                KeyCode::Enter => {
                    let field = *field;
                    let chosen = if *cursor == 0 {
                        None
                    } else {
                        items.get(*cursor).cloned()
                    };
                    let project = view.project.clone();
                    view.mode = SettingsMode::List;
                    match field {
                        SettingField::Theme => {
                            save(app, ctx, &project, ProjectKey::Theme, chosen.as_deref());
                        }
                        SettingField::Images => match chosen.as_deref() {
                            None => {
                                if save(app, ctx, &project, ProjectKey::Images, None) {
                                    save(app, ctx, &project, ProjectKey::GyazoTeam, None);
                                }
                            }
                            Some("gyazo") => {
                                if save(app, ctx, &project, ProjectKey::Images, Some("gyazo")) {
                                    // …and which Gyazo: the org, or gyazo.com.
                                    let current = ctx
                                        .config()
                                        .upload_choice(&project)
                                        .gyazo_team
                                        .unwrap_or_default();
                                    begin_input(app, ctx, ProjectKey::GyazoTeam, current);
                                }
                            }
                            Some(k) => {
                                if save(app, ctx, &project, ProjectKey::Images, Some(k)) {
                                    save(app, ctx, &project, ProjectKey::GyazoTeam, None);
                                }
                            }
                        },
                        SettingField::DisplayName => {}
                    }
                }
                _ => {}
            }
        }
        SettingsMode::Input { key, input } => match (code, ctrl) {
            (KeyCode::Esc, _) => {
                view.mode = SettingsMode::List;
                app.ime_guard = None;
            }
            (KeyCode::Enter, _) => {
                let key = *key;
                let text = input.buf.trim().to_string();
                let project = view.project.clone();
                view.mode = SettingsMode::List;
                app.ime_guard = None;
                let value = if text.is_empty() {
                    None
                } else {
                    Some(text.as_str())
                };
                save(app, ctx, &project, key, value);
            }
            (KeyCode::Backspace, _) => input.backspace(),
            (KeyCode::Delete, _) | (KeyCode::Char('d'), true) => input.delete(),
            (KeyCode::Left, _) | (KeyCode::Char('b'), true) => input.left(),
            (KeyCode::Right, _) | (KeyCode::Char('f'), true) => input.right(),
            (KeyCode::Home, _) | (KeyCode::Char('a'), true) => input.home(),
            (KeyCode::End, _) | (KeyCode::Char('e'), true) => input.end(),
            (KeyCode::Char('w'), true) => input.delete_word(),
            (KeyCode::Char('u'), true) => input.kill_to_start(),
            (KeyCode::Char(ch), false) => input.insert_char(ch),
            _ => {}
        },
    }
    true
}

/// Text pasted while a field is open goes into it (first line only).
pub(crate) fn settings_paste(app: &mut App, text: &str) -> bool {
    if let Some(Overlay::Settings(SettingsView {
        mode: SettingsMode::Input { input, .. },
        ..
    })) = app.overlay.as_mut()
    {
        if let Some(first) = text.lines().next() {
            input.insert_str(first);
        }
        return true;
    }
    false
}

/// Enter on a row: a picker for the theme and the destination, a text
/// field for the name.
fn begin_change(app: &mut App, ctx: &Ctx, field: SettingField) {
    let Some(Overlay::Settings(view)) = app.overlay.as_mut() else {
        return;
    };
    let project = view.project.clone();
    match field {
        SettingField::Theme => {
            let mut items = vec![t!("(設定しない)", "(unset)")];
            items.extend(cosense::theme::COSENSE_THEMES.iter().map(|t| t.to_string()));
            let current = ctx.config().project_theme(&project);
            let cursor = current
                .and_then(|c| items.iter().position(|i| *i == c))
                .unwrap_or(0);
            view.mode = SettingsMode::Pick {
                field,
                items,
                cursor,
            };
        }
        SettingField::Images => {
            let items = vec![t!("(設定しない)", "(unset)"), "gcs".into(), "gyazo".into()];
            let current = ctx.config().upload_choice(&project).images;
            let cursor = current
                .and_then(|c| items.iter().position(|i| *i == c))
                .unwrap_or(0);
            view.mode = SettingsMode::Pick {
                field,
                items,
                cursor,
            };
        }
        SettingField::DisplayName => {
            let current = ctx
                .config()
                .project_display_name(&project)
                .unwrap_or_default();
            begin_input(app, ctx, ProjectKey::DisplayName, current);
        }
    }
}

fn begin_input(app: &mut App, ctx: &Ctx, key: ProjectKey, current: String) {
    if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
        view.mode = SettingsMode::Input {
            key,
            input: Input::new(current),
        };
        // A name is typed in Japanese as often as not: the input source
        // switches for the field and returns to ASCII when it closes.
        app.ime_guard = Some(cosense::ime::ImeGuard::enter(ctx.ime_mode()));
    }
}

/// Write one key, swap the in-memory config, and bring the screen up to
/// date — the settings rows AND the page or index behind them. Returns
/// whether the write went through.
fn save(app: &mut App, ctx: &Ctx, project: &str, key: ProjectKey, value: Option<&str>) -> bool {
    if ctx.config_error.is_some() {
        app.toast_err(t!(
            "config.toml が読めないので保存できません",
            "config.toml does not parse; not saving"
        ));
        return false;
    }
    let Some(path) = ctx.config_path.as_deref() else {
        app.toast_err(t!(
            "設定ファイルの場所が決まりません（HOME が未設定）",
            "no place for the settings file ($HOME is unset)"
        ));
        return false;
    };
    match cosense::config::Config::save_project_key(path, project, key, value) {
        Ok(next) => {
            ctx.set_config(next);
            app.note(match value {
                Some(v) => t!("{} = {v} を保存しました", "saved {} = {v}", key.as_str()),
                None => t!("{} を既定に戻しました", "{} back to default", key.as_str()),
            });
            apply_config_change(app, ctx, project);
            true
        }
        Err(e) => {
            app.toast_err(t!("保存できません: {e}", "cannot save: {e}"));
            false
        }
    }
}

/// The file changed for `project`: refresh what is on screen from it.
fn apply_config_change(app: &mut App, ctx: &Ctx, project: &str) {
    if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
        if view.project == project {
            refresh_rows(view, ctx);
        }
    }
    if app.index_project == project {
        app.index_display = ctx.project_display(project);
    }
    if app.project == project {
        app.project_display = ctx.project_display(project);
        let theme = ctx.project_theme(project);
        let (fg, bg) = cosense::theme::project_header_colors(theme.as_deref(), ctx.terminal_bg());
        app.header_colors = HeaderColors { fg, bg };
        app.telomere_tint = theme
            .as_deref()
            .and_then(cosense::theme::cosense_telomere_tint);
        let palette = cosense::theme::tinted_page_palette(&ctx.palette(), theme.as_deref());
        if palette != app.palette {
            app.palette = palette;
            rerender(app, ctx);
        }
    }
}
