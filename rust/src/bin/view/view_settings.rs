//! This terminal's own settings — language, colour scheme, light/dark, the
//! index's excerpt dock, IME handling, where downloads land, whether
//! diagrams are drawn — with where each answer came from. Resolved once at
//! start (`resolve`) from the flags, the environment, `config.toml`'s
//! `[view]` and the defaults, in that order; kept behind `Ctx::view` so the
//! settings screen can change them while the viewer runs. See
//! `docs/PLAN-settings-global.md`.

use super::*;
use cosense::config::{pick, Origin, ViewSection};
use cosense::lang::Lang;

/// `--light` / `--dark`, or neither: follow the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Appearance {
    Light,
    Dark,
    #[default]
    Auto,
}

impl Appearance {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "light" => Some(Appearance::Light),
            "dark" => Some(Appearance::Dark),
            "auto" => Some(Appearance::Auto),
            _ => None,
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
            Appearance::Auto => "auto",
        }
    }
}

/// What the flags said. `None` everywhere is a bare start.
#[derive(Debug, Default, Clone)]
pub(crate) struct ViewFlags {
    pub lang: Option<String>,
    pub theme: Option<String>,
    pub appearance: Option<Appearance>,
    pub preview: Option<cosense::index::PreviewMode>,
    pub ime: Option<cosense::ime::ImeMode>,
    pub download_dir: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ViewSettings {
    pub lang: (Lang, Origin),
    /// A `--theme` name (two-face's), or none: the light/dark default.
    pub theme: (Option<String>, Origin),
    pub appearance: (Appearance, Origin),
    pub preview: (cosense::index::PreviewMode, Origin),
    pub ime: (cosense::ime::ImeMode, Origin),
    /// As written (a `~` is expanded, `$VAR`s are not); `download_dir`
    /// below is the usable path.
    pub download_dir: (std::path::PathBuf, Origin),
    pub diagrams: (capability::RenderPolicy, Origin),
    /// `theme` names something neither embedded nor in the reader's themes
    /// directory: the default is in use, and the screen says so.
    pub theme_missing: bool,
    /// The terminal's background as OSC 11 reported it at start, when it
    /// did. `Appearance::Auto` reads this; a forced mode ignores it.
    pub detected_bg: Option<(u8, u8, u8)>,
    /// What the command line said — kept so a later re-resolution (after
    /// the settings screen wrote the file) still lets the flags win.
    pub flags: ViewFlags,
    /// Where downloads go when nothing names a place.
    pub default_download: std::path::PathBuf,
    // ---- derived: `recompute` keeps these in step with the above --------
    pub light: bool,
    pub terminal_bg: (u8, u8, u8),
    pub hl: Arc<Highlighter>,
    pub palette: cosense::theme::Palette,
}

impl ViewSettings {
    /// Resolve every setting. `env` is looked up by name so tests can feed
    /// their own. `notes` collects what the file spelled wrong — each such
    /// key falls back as if unset, and the caller says so once.
    pub(crate) fn resolve(
        flags: &ViewFlags,
        env: &dyn Fn(&str) -> Option<String>,
        file: &ViewSection,
        detected_bg: Option<(u8, u8, u8)>,
        default_download: std::path::PathBuf,
        notes: &mut Vec<String>,
    ) -> Self {
        let home = env("HOME");
        let mut bad = |key: &str, v: &str| {
            notes.push(t!(
                "config.toml の [view] {key} = \"{v}\" は解釈できません。無視します",
                "config.toml [view] {key} = \"{v}\" is not understood; ignored"
            ));
        };
        // A file value that does not parse is reported and dropped.
        let mut file_parsed = |key: &str, v: &Option<String>, ok: bool| -> bool {
            if let (Some(v), false) = (v, ok) {
                bad(key, v);
            }
            ok
        };

        let lang_file = file.lang.as_deref().and_then(Lang::from_locale);
        file_parsed(
            "lang",
            &file.lang,
            lang_file.is_some() || file.lang.is_none(),
        );
        let lang_env = ["COSENSE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| env(k).as_deref().and_then(Lang::from_locale));
        let lang = pick(
            flags.lang.as_deref().and_then(Lang::from_locale),
            lang_env,
            lang_file,
            (Lang::En, Origin::Default),
        );

        let theme = pick(
            flags.theme.clone().map(Some),
            None,
            file.theme.clone().filter(|t| !t.is_empty()).map(Some),
            (None, Origin::Default),
        );
        // A name nothing answers to falls back silently in the highlighter;
        // it must not fall back silently on the reader.
        let theme_missing = theme
            .0
            .as_deref()
            .is_some_and(|n| !Highlighter::theme_exists(n));
        let mut late_notes = Vec::new();
        if let (Some(n), true) = (theme.0.as_deref(), theme_missing) {
            let dir = cosense::highlight::user_themes_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "~/.config/cosentty/themes".into());
            late_notes.push(t!(
                "テーマ「{n}」は見つかりません（同梱にも {dir} にも無い）。既定の配色を使います",
                "theme \"{n}\" not found (neither embedded nor in {dir}); using the default"
            ));
        }

        let app_file = file.appearance.as_deref().and_then(Appearance::parse);
        file_parsed(
            "appearance",
            &file.appearance,
            app_file.is_some() || file.appearance.is_none(),
        );
        let appearance = pick(
            flags.appearance,
            None,
            app_file,
            (Appearance::Auto, Origin::Default),
        );

        let prev_file = file
            .preview
            .as_deref()
            .and_then(cosense::index::PreviewMode::parse);
        file_parsed(
            "preview",
            &file.preview,
            prev_file.is_some() || file.preview.is_none(),
        );
        let preview = pick(
            flags.preview,
            None,
            prev_file,
            (cosense::index::PreviewMode::Auto, Origin::Default),
        );

        let ime_file = file.ime.as_deref().and_then(parse_ime);
        file_parsed("ime", &file.ime, ime_file.is_some() || file.ime.is_none());
        let ime = pick(
            flags.ime,
            None,
            ime_file,
            (cosense::ime::ImeMode::Jp, Origin::Default),
        );

        let dir_of = |s: &str| expand_home(s.trim(), home.as_deref());
        let download_dir = pick(
            flags
                .download_dir
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(dir_of),
            env("COSENSE_DOWNLOAD_DIR")
                .filter(|s| !s.trim().is_empty())
                .as_deref()
                .map(dir_of),
            file.download_dir
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(dir_of),
            (default_download.clone(), Origin::Auto),
        );

        let diag_file = file
            .diagrams
            .as_deref()
            .and_then(capability::RenderPolicy::parse);
        file_parsed(
            "diagrams",
            &file.diagrams,
            diag_file.is_some() || file.diagrams.is_none(),
        );
        let diagrams = pick(
            None,
            env("COSENSE_WEB_RENDER")
                .as_deref()
                .and_then(capability::RenderPolicy::parse),
            diag_file,
            (capability::RenderPolicy::Off, Origin::Default),
        );

        let mut v = ViewSettings {
            lang,
            theme,
            appearance,
            preview,
            ime,
            download_dir,
            diagrams,
            theme_missing,
            detected_bg,
            flags: flags.clone(),
            default_download,
            light: false,
            terminal_bg: (24, 24, 24),
            hl: Arc::new(Highlighter::new(None, false)),
            palette: cosense::theme::Palette::for_light(false),
        };
        v.recompute();
        notes.append(&mut late_notes);
        v
    }

    /// Resolve again against a freshly saved file, keeping the flags, the
    /// environment and the measured background as they were at start.
    pub(crate) fn reresolve(&self, file: &ViewSection, notes: &mut Vec<String>) -> Self {
        Self::resolve(
            &self.flags,
            &|k| std::env::var(k).ok(),
            file,
            self.detected_bg,
            self.default_download.clone(),
            notes,
        )
    }

    /// The derived values from the chosen ones. Called after every change.
    pub(crate) fn recompute(&mut self) {
        // A forced mode composes translucent colours over a representative
        // base rather than the measured one, so the result is predictable.
        let (light, bg) = match self.appearance.0 {
            Appearance::Light => (true, (250, 250, 250)),
            Appearance::Dark => (false, (24, 24, 24)),
            Appearance::Auto => {
                let light = self
                    .detected_bg
                    .map(cosense::theme::background_is_light)
                    .unwrap_or(false);
                (
                    light,
                    self.detected_bg
                        .unwrap_or(if light { (250, 250, 250) } else { (24, 24, 24) }),
                )
            }
        };
        self.light = light;
        self.terminal_bg = bg;
        let hl = Highlighter::new(self.theme.0.as_deref(), light);
        self.palette = cosense::theme::Palette::from_theme(&hl, light);
        self.hl = Arc::new(hl);
    }

    /// The fixture every test starts from: dark, no theme, IME off, the
    /// temp dir for downloads, nothing from a flag or a file. The palette
    /// is the plain dark one (`Palette::for_light`), as the page fixtures
    /// render with — not `recompute`'s theme-derived one — so a test can
    /// compare the two.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        ViewSettings {
            lang: (Lang::Ja, Origin::Default),
            theme: (None, Origin::Default),
            appearance: (Appearance::Dark, Origin::Default),
            preview: (cosense::index::PreviewMode::Auto, Origin::Default),
            ime: (cosense::ime::ImeMode::Off, Origin::Default),
            download_dir: (std::env::temp_dir(), Origin::Default),
            diagrams: (capability::RenderPolicy::Off, Origin::Default),
            theme_missing: false,
            detected_bg: None,
            flags: ViewFlags::default(),
            default_download: std::env::temp_dir(),
            light: false,
            terminal_bg: (24, 24, 24),
            hl: Arc::new(Highlighter::new(None, false)),
            palette: cosense::theme::Palette::for_light(false),
        }
    }
}

/// `--ime jp|off|ascii`, strictly: the flag's own parser takes anything
/// and answers `Jp`, which is right for a typo on the command line and
/// wrong for a file, where the typo should be reported.
pub(crate) fn parse_ime(s: &str) -> Option<cosense::ime::ImeMode> {
    match s {
        "jp" => Some(cosense::ime::ImeMode::Jp),
        "off" => Some(cosense::ime::ImeMode::Off),
        "ascii" => Some(cosense::ime::ImeMode::Ascii),
        _ => None,
    }
}
