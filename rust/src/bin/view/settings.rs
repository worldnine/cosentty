//! The settings screen (`,`), in two sections: this terminal's own
//! settings (`config.toml [view]`: language, colour scheme, light/dark, the
//! index's excerpt dock, IME, downloads, diagrams) and, when a project is on
//! screen, that project's (`[project.<slug>]`: theme, display name, where
//! pasted images go). Every row shows the value in force and where it came
//! from (`flag` / `env` / `api` / `file` / `auto` / `-`). A change is written
//! to config.toml at once and takes effect on screen at once; `e` opens the
//! file in `$EDITOR` instead. See `docs/PLAN-settings-ui.md` and
//! `docs/PLAN-settings-global.md`.

use super::*;
use cosense::config::{Origin, ProjectKey, ViewKey};

/// One row of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingField {
    // ---- [view] ----
    Lang,
    Theme,
    Appearance,
    Preview,
    Ime,
    DownloadDir,
    Diagrams,
    DiagramText,
    // ---- [project.<slug>] ----
    ProjectTheme,
    ProjectDisplayName,
    ProjectImages,
}

impl SettingField {
    const VIEW: [SettingField; 8] = [
        SettingField::Lang,
        SettingField::Theme,
        SettingField::Appearance,
        SettingField::Preview,
        SettingField::Ime,
        SettingField::DownloadDir,
        SettingField::Diagrams,
        SettingField::DiagramText,
    ];
    const PROJECT: [SettingField; 3] = [
        SettingField::ProjectTheme,
        SettingField::ProjectDisplayName,
        SettingField::ProjectImages,
    ];

    fn label(self) -> String {
        match self {
            SettingField::Lang => t!("言語", "language"),
            SettingField::Theme => t!("配色テーマ", "colour theme"),
            SettingField::Appearance => t!("明暗", "appearance"),
            SettingField::Preview => t!("一覧の抜粋", "index excerpt"),
            SettingField::Ime => t!("IME", "IME"),
            SettingField::DownloadDir => t!("保存先", "downloads"),
            SettingField::Diagrams => t!("図の表示", "diagrams"),
            SettingField::DiagramText => t!("図の罫線", "diagram glyphs"),
            SettingField::ProjectTheme => t!("テーマ", "theme"),
            SettingField::ProjectDisplayName => t!("表示名", "display name"),
            SettingField::ProjectImages => t!("画像の保存先", "images go to"),
        }
    }

    /// The `[view]` key this row writes, for the view rows.
    fn view_key(self) -> Option<ViewKey> {
        Some(match self {
            SettingField::Lang => ViewKey::Lang,
            SettingField::Theme => ViewKey::Theme,
            SettingField::Appearance => ViewKey::Appearance,
            SettingField::Preview => ViewKey::Preview,
            SettingField::Ime => ViewKey::Ime,
            SettingField::DownloadDir => ViewKey::DownloadDir,
            SettingField::Diagrams => ViewKey::Diagrams,
            SettingField::DiagramText => ViewKey::DiagramText,
            _ => return None,
        })
    }
}

/// One row as shown: the value in force, where it came from, and a short
/// remark when the reader should know more (a flag overrides the file, a
/// change waits for the next start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingRow {
    pub(crate) field: SettingField,
    pub(crate) value: String,
    pub(crate) origin: Origin,
    pub(crate) note: Option<String>,
}

/// What a text field or a picker writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    View(ViewKey),
    Project(ProjectKey),
}

/// What the screen is doing: showing the rows, or asking for one value.
#[derive(Debug)]
pub(crate) enum SettingsMode {
    List,
    /// A short menu. `items[0]` is always "(unset)" — removing the file's
    /// value. For the colour theme the cursor's choice is applied as a
    /// preview; `revert` is the file value to go back to on Esc.
    Pick {
        field: SettingField,
        items: Vec<String>,
        cursor: usize,
        /// Indices in `items` that are group headings (the theme picker
        /// says where each group of names comes from): drawn, never landed
        /// on, never chosen.
        headings: Vec<usize>,
        revert: Option<Option<String>>,
    },
    /// A one-line text field. Empty text removes the file's value.
    Input {
        target: Target,
        input: Input,
    },
}

#[derive(Debug)]
pub(crate) struct SettingsView {
    /// The project section's subject; `None` over the projects list.
    pub(crate) project: Option<String>,
    /// Index into `rows`.
    pub(crate) cursor: usize,
    pub(crate) rows: Vec<SettingRow>,
    /// `/api/projects/<slug>` answered — the project's theme and name
    /// rows then show the web's values and a file value would not be used.
    pub(crate) api_readable: bool,
    /// The file exists and does not parse: nothing will be written over it.
    pub(crate) file_broken: bool,
    pub(crate) mode: SettingsMode,
}

/// What a key press on the screen amounts to, for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsOutcome {
    /// No settings screen is open.
    NotOurs,
    Handled,
    /// `e`: open config.toml in `$EDITOR` (needs the terminal).
    EditConfig,
}

/// Open the screen for `project` (the page's, or the index's; `None` over
/// the projects list, where only this terminal's section is shown).
pub(crate) fn open_settings(app: &mut App, ctx: &Ctx, project: Option<&str>) {
    let mut view = SettingsView {
        project: project.map(str::to_string),
        cursor: 0,
        rows: Vec::new(),
        api_readable: false,
        file_broken: ctx.config_error().is_some(),
        mode: SettingsMode::List,
    };
    refresh_rows(&mut view, ctx);
    app.overlay = Some(Overlay::Settings(view));
}

fn origin_note(origin: Origin) -> Option<String> {
    match origin {
        Origin::Flag => Some(t!("起動オプションが優先", "the flag wins")),
        Origin::Env => Some(t!("環境変数が優先", "the variable wins")),
        _ => None,
    }
}

/// What `image` needs that this session does not have. Said on the row,
/// so "I set image and nothing changed" has its answer where it was set.
fn diagrams_note(ctx: &Ctx, policy: capability::RenderPolicy) -> Option<String> {
    if policy != capability::RenderPolicy::Image {
        return None;
    }
    if cosense::chrome::find_chrome().is_none() {
        return Some(t!(
            "ブラウザが見つかりません (COSENSE_CHROME)",
            "no browser found (COSENSE_CHROME)"
        ));
    }
    if ctx.client.sid().is_none() {
        return Some(t!(
            "非公開の図には connect.sid が必要 (COSENSE_SID)",
            "private diagrams need a connect.sid (COSENSE_SID)"
        ));
    }
    None
}

/// Recompute the rows from what the viewer now knows.
fn refresh_rows(view: &mut SettingsView, ctx: &Ctx) {
    let v = ctx.view();
    let mut rows = vec![
        SettingRow {
            field: SettingField::Lang,
            value: match v.lang.0 {
                cosense::lang::Lang::Ja => "ja".into(),
                cosense::lang::Lang::En => "en".into(),
            },
            origin: v.lang.1,
            note: origin_note(v.lang.1),
        },
        SettingRow {
            field: SettingField::Theme,
            value: v
                .theme
                .0
                .clone()
                .unwrap_or_else(|| t!("(端末の配色)", "(terminal default)")),
            origin: v.theme.1,
            note: if v.theme_missing {
                Some(t!("見つからず既定を使用", "not found; default in use"))
            } else if let Some((dir, true)) = v
                .theme
                .0
                .as_deref()
                .and_then(Highlighter::user_theme_source)
            {
                // The same name exists embedded: say which one is in force.
                Some(t!(
                    "同梱と同名。{} のものを使用",
                    "same name as an embedded one; using {}",
                    short_home(&dir)
                ))
            } else {
                origin_note(v.theme.1)
            },
        },
        SettingRow {
            field: SettingField::Appearance,
            value: match v.appearance.0 {
                Appearance::Auto => format!("auto ({})", if v.light { "light" } else { "dark" }),
                a => a.as_str().to_string(),
            },
            origin: v.appearance.1,
            note: origin_note(v.appearance.1),
        },
        SettingRow {
            field: SettingField::Preview,
            value: match v.preview.0 {
                cosense::index::PreviewMode::On => "on",
                cosense::index::PreviewMode::Off => "off",
                cosense::index::PreviewMode::Auto => "auto",
            }
            .into(),
            origin: v.preview.1,
            note: origin_note(v.preview.1),
        },
        SettingRow {
            field: SettingField::Ime,
            value: match v.ime.0 {
                cosense::ime::ImeMode::Jp => "jp",
                cosense::ime::ImeMode::Off => "off",
                cosense::ime::ImeMode::Ascii => "ascii",
            }
            .into(),
            origin: v.ime.1,
            note: origin_note(v.ime.1),
        },
        SettingRow {
            field: SettingField::DownloadDir,
            value: v.download_dir.0.display().to_string(),
            origin: v.download_dir.1,
            note: origin_note(v.download_dir.1),
        },
        SettingRow {
            field: SettingField::Diagrams,
            value: v.diagrams.0.as_str().into(),
            origin: v.diagrams.1,
            note: origin_note(v.diagrams.1).or_else(|| diagrams_note(ctx, v.diagrams.0)),
        },
        SettingRow {
            field: SettingField::DiagramText,
            value: v.diagram_text.0.as_str().into(),
            origin: v.diagram_text.1,
            note: origin_note(v.diagram_text.1),
        },
    ];
    if let Some(project) = view.project.as_deref() {
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
        // A file value under an API one is not in force: say so on the row.
        let shadowed = |from: Origin, file_has: bool| -> Option<String> {
            (from == Origin::Api && file_has)
                .then(|| t!("API が読める間は API が優先", "the API wins while readable"))
        };
        let cfg = ctx.config();
        rows.extend([
            SettingRow {
                field: SettingField::ProjectTheme,
                value: theme.unwrap_or_else(|| t!("(なし)", "(none)")),
                origin: theme_from,
                note: shadowed(theme_from, cfg.project_theme(project).is_some()),
            },
            SettingRow {
                field: SettingField::ProjectDisplayName,
                value: name,
                origin: name_from,
                note: shadowed(name_from, cfg.project_display_name(project).is_some()),
            },
            SettingRow {
                field: SettingField::ProjectImages,
                value: dest.label(),
                origin: dest_from,
                note: None,
            },
        ]);
    }
    view.rows = rows;
    view.cursor = view.cursor.min(view.rows.len().saturating_sub(1));
}

/// The lines above the rows: only what the reader must know now.
pub(crate) fn settings_notes(view: &SettingsView) -> Vec<String> {
    let mut notes = Vec::new();
    if view.project.is_some() && !view.api_readable {
        notes.push(t!(
            "プロジェクト設定を API から読めません（非公開・sid なし）。file の値を使います",
            "project settings unreadable from the API (private, no sid); file values are used"
        ));
    }
    if view.file_broken {
        notes.push(t!(
            "config.toml が読めないので保存しません（e で開いて直す）",
            "config.toml does not parse; nothing is saved until it is fixed (e opens it)"
        ));
    }
    notes
}

/// The panel's items and which of them carries the cursor: the notes, then
/// each section under a heading, the rows fitted to `width` cells (label,
/// value, origin, remark — the remark goes first when space is short, then
/// the value is shortened).
pub(crate) fn settings_items(view: &SettingsView, width: usize) -> (Vec<String>, usize) {
    let mut items = settings_notes(view);
    let mut cursor_item = usize::MAX;
    let heading = |s: String| format!("── {s} ──");
    let mut push_section = |title: String, fields: &[SettingField], items: &mut Vec<String>| {
        items.push(heading(title));
        for f in fields {
            let Some((i, row)) = view.rows.iter().enumerate().find(|(_, r)| r.field == *f) else {
                continue;
            };
            if matches!(view.mode, SettingsMode::List) && i == view.cursor {
                cursor_item = items.len();
            }
            items.push(format_row(row, width));
        }
    };
    push_section(
        t!(
            "この端末 (config.toml [view])",
            "this terminal (config.toml [view])"
        ),
        &SettingField::VIEW,
        &mut items,
    );
    if let Some(p) = view.project.as_deref() {
        push_section(
            t!("プロジェクト: {p}", "project: {p}"),
            &SettingField::PROJECT,
            &mut items,
        );
    }
    (items, cursor_item)
}

/// `label  value  ← origin  remark`, fitted to `width` cells.
fn format_row(row: &SettingRow, width: usize) -> String {
    const LABEL: usize = 14;
    let label = pad_to(&row.label_text(), LABEL);
    let origin = format!("← {}", row.origin.tag());
    // What is left for the value and the remark: label, two gaps, origin.
    let rest = width.saturating_sub(LABEL + 2 + str_width(&origin) + 1);
    let (value_w, note) = match &row.note {
        Some(n) if rest >= 30 => {
            // The remark gets what the value does not need, up to half.
            let value_w = rest.saturating_sub(str_width(n) + 2).max(rest / 2).min(24);
            (
                value_w,
                Some(truncate_width(n, rest.saturating_sub(value_w + 2))),
            )
        }
        _ => (rest.clamp(8, 24), None),
    };
    let value = pad_to(&truncate_width(&row.value, value_w), value_w);
    match note {
        Some(n) => format!("{label} {value}  {origin}  {n}"),
        None => format!("{label} {value}  {origin}"),
    }
}

impl SettingRow {
    fn label_text(&self) -> String {
        self.field.label()
    }
}

fn pad_to(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    let used = str_width(s);
    if used < w {
        out.push_str(&" ".repeat(w - used));
    }
    out
}

/// Keys while the screen is open.
pub(crate) fn handle_settings_key(
    app: &mut App,
    ctx: &Ctx,
    code: KeyCode,
    mods: KeyModifiers,
) -> SettingsOutcome {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let Some(Overlay::Settings(view)) = app.overlay.as_mut() else {
        return SettingsOutcome::NotOurs;
    };
    match &mut view.mode {
        SettingsMode::List => {
            let last = view.rows.len().saturating_sub(1);
            match code {
                KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
                KeyCode::Char('?') => app.overlay = Some(Overlay::Help),
                KeyCode::Down | KeyCode::Char('j') => view.cursor = (view.cursor + 1).min(last),
                KeyCode::Char('n') if ctrl => view.cursor = (view.cursor + 1).min(last),
                KeyCode::Up | KeyCode::Char('k') => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Char('p') if ctrl => view.cursor = view.cursor.saturating_sub(1),
                KeyCode::Char('e') => return SettingsOutcome::EditConfig,
                KeyCode::Enter => {
                    if let Some(field) = view.rows.get(view.cursor).map(|r| r.field) {
                        begin_change(app, ctx, field);
                    }
                }
                // `d`: back to the default (removes the file's value).
                KeyCode::Char('d') => {
                    let Some(field) = view.rows.get(view.cursor).map(|r| r.field) else {
                        return SettingsOutcome::Handled;
                    };
                    let project = view.project.clone();
                    let targets: Vec<Target> = match field {
                        SettingField::ProjectTheme => vec![Target::Project(ProjectKey::Theme)],
                        SettingField::ProjectDisplayName => {
                            vec![Target::Project(ProjectKey::DisplayName)]
                        }
                        SettingField::ProjectImages => vec![
                            Target::Project(ProjectKey::Images),
                            Target::Project(ProjectKey::GyazoTeam),
                        ],
                        f => f.view_key().map(Target::View).into_iter().collect(),
                    };
                    for t in targets {
                        if !save(app, ctx, project.as_deref(), t, None) {
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
            headings,
            revert,
        } => {
            let field = *field;
            // Step over headings; stay put at either end.
            let step = |from: usize, down: bool| -> usize {
                let mut i = from;
                loop {
                    let next = if down { i + 1 } else { i.wrapping_sub(1) };
                    if next >= items.len() {
                        return from;
                    }
                    i = next;
                    if !headings.contains(&i) {
                        return i;
                    }
                }
            };
            let moved = match code {
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = step(*cursor, true);
                    true
                }
                KeyCode::Char('n') if ctrl => {
                    *cursor = step(*cursor, true);
                    true
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *cursor = step(*cursor, false);
                    true
                }
                KeyCode::Char('p') if ctrl => {
                    *cursor = step(*cursor, false);
                    true
                }
                _ => false,
            };
            if moved {
                // The colour theme is previewed live: what the cursor is
                // on is what the page wears, until Enter or Esc decides.
                let chosen = (*cursor > 0).then(|| items[*cursor].clone());
                match field {
                    SettingField::Theme => preview_theme(app, ctx, chosen),
                    // The project's theme too: header, links and telomere
                    // take the cursor's choice until Enter or Esc decides.
                    SettingField::ProjectTheme => {
                        if let Some(p) = view.project.clone() {
                            ctx.set_project_theme_preview(Some((p.clone(), chosen)));
                            apply_project_change(app, ctx, &p);
                        }
                    }
                    _ => {}
                }
                return SettingsOutcome::Handled;
            }
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    let revert = revert.take();
                    let project = view.project.clone();
                    view.mode = SettingsMode::List;
                    if let Some(back) = revert {
                        preview_theme(app, ctx, back);
                    }
                    if field == SettingField::ProjectTheme {
                        ctx.set_project_theme_preview(None);
                        if let Some(p) = project {
                            apply_project_change(app, ctx, &p);
                            if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
                                refresh_rows(view, ctx);
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    let chosen = (*cursor > 0).then(|| items[*cursor].clone());
                    let project = view.project.clone();
                    view.mode = SettingsMode::List;
                    // The save below decides from the file, not the preview.
                    ctx.set_project_theme_preview(None);
                    match field {
                        SettingField::ProjectTheme => {
                            save(
                                app,
                                ctx,
                                project.as_deref(),
                                Target::Project(ProjectKey::Theme),
                                chosen.as_deref(),
                            );
                        }
                        SettingField::ProjectImages => match chosen.as_deref() {
                            None => {
                                if save(
                                    app,
                                    ctx,
                                    project.as_deref(),
                                    Target::Project(ProjectKey::Images),
                                    None,
                                ) {
                                    save(
                                        app,
                                        ctx,
                                        project.as_deref(),
                                        Target::Project(ProjectKey::GyazoTeam),
                                        None,
                                    );
                                }
                            }
                            Some("gyazo") => {
                                if save(
                                    app,
                                    ctx,
                                    project.as_deref(),
                                    Target::Project(ProjectKey::Images),
                                    Some("gyazo"),
                                ) {
                                    // …and which Gyazo: the org, or gyazo.com.
                                    let current = project
                                        .as_deref()
                                        .map(|p| ctx.config().upload_choice(p))
                                        .and_then(|c| c.gyazo_team)
                                        .unwrap_or_default();
                                    begin_input(
                                        app,
                                        ctx,
                                        Target::Project(ProjectKey::GyazoTeam),
                                        current,
                                    );
                                }
                            }
                            Some(k) => {
                                if save(
                                    app,
                                    ctx,
                                    project.as_deref(),
                                    Target::Project(ProjectKey::Images),
                                    Some(k),
                                ) {
                                    save(
                                        app,
                                        ctx,
                                        project.as_deref(),
                                        Target::Project(ProjectKey::GyazoTeam),
                                        None,
                                    );
                                }
                            }
                        },
                        f => {
                            if let Some(key) = f.view_key() {
                                save(app, ctx, None, Target::View(key), chosen.as_deref());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        SettingsMode::Input { target, input } => match (code, ctrl) {
            (KeyCode::Esc, _) => {
                view.mode = SettingsMode::List;
                app.ime_guard = None;
            }
            (KeyCode::Enter, _) => {
                let target = *target;
                let text = input.buf.trim().to_string();
                let project = view.project.clone();
                view.mode = SettingsMode::List;
                app.ime_guard = None;
                let value = (!text.is_empty()).then_some(text.as_str());
                save(app, ctx, project.as_deref(), target, value);
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
    SettingsOutcome::Handled
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

fn unset_label() -> String {
    t!("(設定しない)", "(unset)")
}

/// `/Users/me/.config/bat/themes` → `~/.config/bat/themes`.
fn short_home(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// Enter on a row: a picker for the enumerated values, a text field for
/// the free ones.
fn begin_change(app: &mut App, ctx: &Ctx, field: SettingField) {
    let Some(Overlay::Settings(view)) = app.overlay.as_mut() else {
        return;
    };
    let project = view.project.clone();
    let cfg = ctx.config();
    let pick = |items: Vec<String>, current: Option<String>| SettingsMode::Pick {
        field,
        cursor: current
            .and_then(|c| items.iter().position(|i| *i == c))
            .unwrap_or(0),
        items,
        headings: Vec::new(),
        revert: None,
    };
    let with_unset = |vals: &[&str]| -> Vec<String> {
        std::iter::once(unset_label())
            .chain(vals.iter().map(|s| s.to_string()))
            .collect()
    };
    match field {
        SettingField::Lang => view.mode = pick(with_unset(&["ja", "en"]), cfg.view.lang.clone()),
        SettingField::Theme => {
            // One heading per source, so the reader can tell their own
            // files from the embedded set — and nothing on the rows.
            let mut items = vec![unset_label()];
            let mut heads = Vec::new();
            for (dir, names) in Highlighter::theme_groups() {
                heads.push(items.len());
                items.push(match dir {
                    Some(d) => format!("── {} ──", short_home(&d)),
                    None => t!("── 同梱 ──", "── built in ──"),
                });
                items.extend(names);
            }
            let mut mode = pick(items, cfg.view.theme.clone());
            if let SettingsMode::Pick {
                revert, headings, ..
            } = &mut mode
            {
                *revert = Some(cfg.view.theme.clone());
                *headings = heads;
            }
            view.mode = mode;
        }
        SettingField::Appearance => {
            view.mode = pick(
                with_unset(&["light", "dark", "auto"]),
                cfg.view.appearance.clone(),
            )
        }
        SettingField::Preview => {
            view.mode = pick(with_unset(&["on", "off", "auto"]), cfg.view.preview.clone())
        }
        SettingField::Ime => view.mode = pick(with_unset(&["jp", "off"]), cfg.view.ime.clone()),
        SettingField::Diagrams => {
            view.mode = pick(
                with_unset(&["text", "image"]),
                cfg.view.diagrams.clone(),
            )
        }
        SettingField::DiagramText => {
            view.mode = pick(
                with_unset(&["box", "ascii"]),
                cfg.view.diagram_text.clone(),
            )
        }
        SettingField::DownloadDir => {
            let current = cfg.view.download_dir.clone().unwrap_or_default();
            begin_input(app, ctx, Target::View(ViewKey::DownloadDir), current);
        }
        SettingField::ProjectTheme => {
            let mut items = vec![unset_label()];
            items.extend(cosense::theme::COSENSE_THEMES.iter().map(|t| t.to_string()));
            let current = project.as_deref().and_then(|p| cfg.project_theme(p));
            view.mode = pick(items, current);
        }
        SettingField::ProjectImages => {
            let current = project.as_deref().and_then(|p| cfg.upload_choice(p).images);
            view.mode = pick(with_unset(&["gcs", "gyazo"]), current);
        }
        SettingField::ProjectDisplayName => {
            let current = project
                .as_deref()
                .and_then(|p| cfg.project_display_name(p))
                .unwrap_or_default();
            begin_input(app, ctx, Target::Project(ProjectKey::DisplayName), current);
        }
    }
}

fn begin_input(app: &mut App, ctx: &Ctx, target: Target, current: String) {
    if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
        view.mode = SettingsMode::Input {
            target,
            input: Input::new(current),
        };
        // A name is typed in Japanese as often as not: the input source
        // switches for the field and returns to ASCII when it closes. A
        // path is not, so that field stays in ASCII.
        let japanese = matches!(target, Target::Project(ProjectKey::DisplayName));
        app.ime_guard = japanese.then(|| cosense::ime::ImeGuard::enter(ctx.ime_mode()));
    }
}

/// Wear `theme` (a `--theme` name, or none) without writing anything.
fn preview_theme(app: &mut App, ctx: &Ctx, theme: Option<String>) {
    ctx.with_view(|v| {
        v.theme = (theme, Origin::File);
        v.recompute();
    });
    apply_view_change(app, ctx);
}

/// Write one key, swap the in-memory config, and bring the screen up to
/// date — the settings rows AND the page or index behind them. Returns
/// whether the write went through.
fn save(
    app: &mut App,
    ctx: &Ctx,
    project: Option<&str>,
    target: Target,
    value: Option<&str>,
) -> bool {
    if ctx.config_error().is_some() {
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
    let (result, key_name) = match target {
        Target::View(k) => (
            cosense::config::Config::save_view_key(path, k, value),
            k.as_str(),
        ),
        Target::Project(k) => match project {
            Some(p) => (
                cosense::config::Config::save_project_key(path, p, k, value),
                k.as_str(),
            ),
            None => return false,
        },
    };
    match result {
        Ok(next) => {
            ctx.set_config(next);
            app.note(match value {
                Some(v) => t!("{key_name} = {v} を保存しました", "saved {key_name} = {v}"),
                None => t!(
                    "{key_name} を既定に戻しました",
                    "{key_name} back to default"
                ),
            });
            match target {
                Target::View(_) => {
                    // The file changed: resolve again so a flag or a
                    // variable still wins where it should.
                    let cfg = ctx.config();
                    let mut notes = Vec::new();
                    let next = ctx.view().reresolve(&cfg.view, &mut notes);
                    ctx.with_view(|v| *v = next);
                    for n in notes {
                        app.toast_err(n);
                    }
                    apply_view_change(app, ctx);
                }
                Target::Project(_) => {
                    if let Some(p) = project {
                        apply_project_change(app, ctx, p);
                    }
                }
            }
            if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
                refresh_rows(view, ctx);
            }
            true
        }
        Err(e) => {
            app.toast_err(t!("保存できません: {e}", "cannot save: {e}"));
            false
        }
    }
}

/// This terminal's settings changed: everything derived from them follows.
fn apply_view_change(app: &mut App, ctx: &Ctx) {
    let v = ctx.view();
    cosense::lang::set(v.lang.0);
    app.light = v.light;
    app.web_dark = !v.light;
    // The render worker waits from launch whatever the policy, so a switch
    // to `image` takes effect here: the page is laid out again (pictures
    // may now stand in for text drawings) and the misses are requested.
    let was = (app.render_policy, app.diagram_text);
    app.render_policy = v.diagrams.0;
    app.diagram_text = v.diagram_text.0;
    repaint_page(app, ctx);
    if was != (app.render_policy, app.diagram_text) {
        rerender(app, ctx);
        app.start_web_renders(capability::Trigger::Auto);
    }
}

/// The file changed for `project`: refresh what is on screen from it.
fn apply_project_change(app: &mut App, ctx: &Ctx, project: &str) {
    if app.index_project == project {
        app.index_display = ctx.project_display(project);
    }
    if app.project == project {
        app.project_display = ctx.project_display(project);
        repaint_page(app, ctx);
    }
}

/// Header colours, palette, telomere tint for the page on screen, from the
/// current terminal settings and the page's project theme; the body is
/// re-rendered only when its palette actually changed.
fn repaint_page(app: &mut App, ctx: &Ctx) {
    let theme = ctx.project_theme(&app.project);
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

/// `e` on the screen: config.toml in `$EDITOR`, then read it back. The
/// file may have been broken and fixed, or fixed and broken; the screen
/// says which.
pub(crate) fn config_editor_roundtrip(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    ctx: &Ctx,
) {
    let Some(path) = ctx.config_path.clone() else {
        app.toast_err(t!(
            "設定ファイルの場所が決まりません（HOME が未設定）",
            "no place for the settings file ($HOME is unset)"
        ));
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    suspend_tui(ctx);
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} '{}'", path.display()))
        .status();
    resume_tui(terminal, ctx);
    app.laid_width = 0;
    if !matches!(status, Ok(s) if s.success()) {
        app.toast_err(t!(
            "エディタが中断しました（{editor}）",
            "editor aborted ({editor})"
        ));
    }
    reload_config(app, ctx);
}

/// Read config.toml again (after `e`) and apply it as a save would.
pub(crate) fn reload_config(app: &mut App, ctx: &Ctx) {
    let Some(path) = ctx.config_path.clone() else {
        return;
    };
    reload_config_from(app, ctx, &path);
}

pub(crate) fn reload_config_from(app: &mut App, ctx: &Ctx, path: &std::path::Path) {
    match cosense::config::Config::load_from(path) {
        Ok(c) => {
            ctx.set_config(c);
            ctx.set_config_error(None);
            let cfg = ctx.config();
            let mut notes = Vec::new();
            let next = ctx.view().reresolve(&cfg.view, &mut notes);
            ctx.with_view(|v| *v = next);
            for n in notes {
                app.toast_err(n);
            }
            apply_view_change(app, ctx);
            let project = app.project.clone();
            apply_project_change(app, ctx, &project);
            let index_project = app.index_project.clone();
            if !index_project.is_empty() {
                apply_project_change(app, ctx, &index_project);
            }
            app.note(t!("config.toml を読み直しました", "config.toml reloaded"));
        }
        Err(e) => {
            ctx.set_config_error(Some(e.clone()));
            app.toast_err(t!(
                "設定ファイルを読めませんでした: {e}",
                "could not read the config file: {e}"
            ));
        }
    }
    if let Some(Overlay::Settings(view)) = app.overlay.as_mut() {
        view.file_broken = ctx.config_error().is_some();
        refresh_rows(view, ctx);
    }
}
