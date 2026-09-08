//! The viewer's own settings file: `$XDG_CONFIG_HOME/cosentty/config.toml`
//! (default `~/.config/cosentty/config.toml`).
//!
//! Until now everything was flags and environment variables. Two things
//! wanted a file at once — where pasted images are uploaded, and (later)
//! key bindings — so the file is shaped from the start to hold both, one
//! table each. `~/.cosense/settings.json` is the official CLI's and is not
//! written to.
//!
//! ```toml
//! [view]                    # この端末の設定。起動オプション > 環境変数 > ここ > 既定
//! lang = "ja"               # ja | en
//! theme = "Solarized (dark)"  # --theme と同じ名前
//! appearance = "auto"       # light | dark | auto
//! preview = "auto"          # on | off | auto
//! ime = "jp"                # jp | off
//! download_dir = "~/Downloads"
//! diagrams = "manual"       # off | manual | auto(COSENSE_WEB_RENDER と同じ)
//!
//! [upload]
//! images = "gcs"            # gcs | gyazo   (unset: project setting → gcs)
//! gyazo_team = "my-org"     # Gyazo Teams org, when images = "gyazo"
//!
//! [upload.project.acme]
//! images = "gyazo"
//! gyazo_team = "acme-inc"
//!
//! [project.acme]            # プロジェクトごとの設定(設定画面 `,` が書く側)
//! theme = "paper-dark"      # API が読めないとき(非公開・sid なし)の代わり
//! display_name = "ACME 社内wiki"
//! images = "gyazo"          # [upload.project.acme] と同じ意味。こちらが勝つ
//! gyazo_team = "acme-inc"
//! ```
//!
//! A missing file is not an error: it is the same as an empty one. A file
//! that does not parse IS reported, because a typo silently turning into
//! the defaults would send a picture to the wrong place.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `[view]`: this terminal's own preferences — what the flags and the
    /// environment decide today, written down. A flag or a variable still
    /// wins over the file (`ViewKey` names the table's keys).
    #[serde(default)]
    pub view: ViewSection,
    #[serde(default)]
    pub upload: UploadSection,
    /// `[project.<slug>]`: everything the viewer wants to know about one
    /// project and cannot always ask the server for. The settings screen
    /// (`,`) writes here; `[upload.project.<slug>]` stays readable for
    /// files written before this table existed.
    #[serde(default)]
    pub project: HashMap<String, ProjectSection>,
}

/// One `[project.<slug>]` table.
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    /// The Cosense theme name (`paper-dark`, `blue`, …). Used only when
    /// `/api/projects/<slug>` could not be read: the web setting is the
    /// truth and the file is its stand-in.
    pub theme: Option<String>,
    /// The project's proper name for the header, same precedence as `theme`.
    pub display_name: Option<String>,
    /// Upload destination, the same two fields as `[upload]` — but here the
    /// FILE beats the project setting (see `upload::Destination::resolve`).
    pub images: Option<String>,
    pub gyazo_team: Option<String>,
}

/// One `[view]` table. Every value is kept as the string written, and
/// interpreted where the flag with the same name is (`main`): a value the
/// file spells wrong falls back to the default there and is reported once,
/// instead of failing the whole file here.
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ViewSection {
    pub lang: Option<String>,
    pub theme: Option<String>,
    pub appearance: Option<String>,
    pub preview: Option<String>,
    pub ime: Option<String>,
    pub download_dir: Option<String>,
    pub diagrams: Option<String>,
}

/// Which key of `[view]` a write names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKey {
    Lang,
    Theme,
    Appearance,
    Preview,
    Ime,
    DownloadDir,
    Diagrams,
}

impl ViewKey {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewKey::Lang => "lang",
            ViewKey::Theme => "theme",
            ViewKey::Appearance => "appearance",
            ViewKey::Preview => "preview",
            ViewKey::Ime => "ime",
            ViewKey::DownloadDir => "download_dir",
            ViewKey::Diagrams => "diagrams",
        }
    }
}

/// Where a project-level value on screen came from. The settings screen
/// shows it next to each value, so "gcs" alone never hides that the
/// project's own setting was simply unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A command-line flag (`--lang`, `--theme`, …).
    Flag,
    /// An environment variable (`COSENSE_LANG`, `COSENSE_DOWNLOAD_DIR`, …).
    Env,
    /// `/api/projects/<slug>` answered.
    Api,
    /// `config.toml` names it.
    File,
    /// Detected at startup (the terminal's background, say).
    Auto,
    /// None of the above: the built-in fallback.
    Default,
}

impl Origin {
    /// The short tag the settings screen prints. Not translated: these
    /// are the names the documentation uses.
    pub fn tag(self) -> &'static str {
        match self {
            Origin::Flag => "flag",
            Origin::Env => "env",
            Origin::Api => "api",
            Origin::File => "file",
            Origin::Auto => "auto",
            Origin::Default => "-",
        }
    }

    /// A value from here beats whatever the file says: the screen tells
    /// the reader a file edit will not show until the next start without it.
    pub fn beats_file(self) -> bool {
        matches!(self, Origin::Flag | Origin::Env)
    }
}

/// The first answer wins: flag, then environment, then the file, then the
/// default — the precedence every `[view]` key follows.
pub fn pick<T>(
    flag: Option<T>,
    env: Option<T>,
    file: Option<T>,
    default: (T, Origin),
) -> (T, Origin) {
    if let Some(v) = flag {
        return (v, Origin::Flag);
    }
    if let Some(v) = env {
        return (v, Origin::Env);
    }
    if let Some(v) = file {
        return (v, Origin::File);
    }
    default
}

/// Which field of a `[project.<slug>]` table a write names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKey {
    Theme,
    DisplayName,
    Images,
    GyazoTeam,
}

impl ProjectKey {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKey::Theme => "theme",
            ProjectKey::DisplayName => "display_name",
            ProjectKey::Images => "images",
            ProjectKey::GyazoTeam => "gyazo_team",
        }
    }
}

/// `[upload]`: the destination for every project, plus per-project
/// overrides under `[upload.project.<name>]`.
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UploadSection {
    #[serde(flatten)]
    pub all: UploadChoice,
    #[serde(default)]
    pub project: HashMap<String, UploadChoice>,
}

/// One destination choice as written in the file. Both fields optional so
/// a project table can set just the team name.
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UploadChoice {
    /// `gcs` (Cosense's own file storage) or `gyazo`.
    pub images: Option<String>,
    /// Gyazo Teams organisation (`<org>.gyazo.com`). Unset = gyazo.com.
    pub gyazo_team: Option<String>,
}

impl Config {
    /// Where the file lives, or `None` when neither `$XDG_CONFIG_HOME` nor
    /// `$HOME` is set.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("cosentty").join("config.toml"))
    }

    /// Read the file. `Ok(Config::default())` when there is none;
    /// `Err` only for a file that is there and does not parse.
    pub fn load() -> Result<Config, String> {
        let Some(path) = Self::path() else {
            return Ok(Config::default());
        };
        Self::load_from(&path)
    }

    /// `load`, from a named file.
    pub fn load_from(path: &std::path::Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

    /// The upload choice the file makes for `project`, field by field:
    /// `[project.<slug>]` → `[upload.project.<slug>]` → `[upload]`.
    pub fn upload_choice(&self, project: &str) -> UploadChoice {
        let all = &self.upload.all;
        let own = self.upload.project.get(project);
        let sect = self.project.get(project);
        UploadChoice {
            images: sect
                .and_then(|c| c.images.clone())
                .or_else(|| own.and_then(|c| c.images.clone()))
                .or_else(|| all.images.clone()),
            gyazo_team: sect
                .and_then(|c| c.gyazo_team.clone())
                .or_else(|| own.and_then(|c| c.gyazo_team.clone()))
                .or_else(|| all.gyazo_team.clone()),
        }
    }

    /// `[project.<slug>].theme`, when the file has one.
    pub fn project_theme(&self, project: &str) -> Option<String> {
        self.project
            .get(project)
            .and_then(|p| p.theme.clone())
            .filter(|t| !t.is_empty())
    }

    /// `[project.<slug>].display_name`, when the file has one.
    pub fn project_display_name(&self, project: &str) -> Option<String> {
        self.project
            .get(project)
            .and_then(|p| p.display_name.clone())
            .filter(|t| !t.trim().is_empty())
    }

    /// `with_key` on `[project.<slug>]`.
    pub fn with_project_key(
        text: &str,
        project: &str,
        key: ProjectKey,
        value: Option<&str>,
    ) -> Result<String, String> {
        Self::with_key(text, &["project", project], key.as_str(), value)
    }

    /// `with_key` on `[view]`.
    pub fn with_view_key(text: &str, key: ViewKey, value: Option<&str>) -> Result<String, String> {
        Self::with_key(text, &["view"], key.as_str(), value)
    }

    /// Set (or, with `None`, remove) one key of the table at `path`
    /// (`["view"]`, `["project", "<slug>"]`) in the file's TEXT, keeping
    /// every comment, every other table and the author's order: a settings
    /// screen that rewrote the file from its parsed form would throw away
    /// whatever it did not understand. Intermediate tables that hold no
    /// keys of their own stay implicit (no bare `[project]` header); a table
    /// left empty is removed with its last key. Errors only for text that
    /// is not TOML — the caller must not overwrite a broken file.
    pub fn with_key(
        text: &str,
        path: &[&str],
        key: &str,
        value: Option<&str>,
    ) -> Result<String, String> {
        use toml_edit::{DocumentMut, Item, Table};
        let mut doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| e.message().to_string())?;
        let root = doc.as_table_mut();
        match value {
            Some(v) => {
                let mut table = root;
                for (i, seg) in path.iter().enumerate() {
                    let last = i + 1 == path.len();
                    table = table
                        .entry(seg)
                        .or_insert_with(|| {
                            let mut t = Table::new();
                            t.set_implicit(!last);
                            Item::Table(t)
                        })
                        .as_table_mut()
                        .ok_or_else(|| format!("`{}` is not a table", path[..=i].join(".")))?;
                }
                table[key] = toml_edit::value(v);
            }
            None => {
                // Walk down; if any table is missing there is nothing to remove.
                fn remove_in(table: &mut Table, path: &[&str], key: &str) {
                    match path.split_first() {
                        None => {
                            table.remove(key);
                        }
                        Some((seg, rest)) => {
                            let mut drop = false;
                            if let Some(t) = table.get_mut(seg).and_then(Item::as_table_mut) {
                                remove_in(t, rest, key);
                                drop = t.is_empty();
                            }
                            if drop {
                                table.remove(seg);
                            }
                        }
                    }
                }
                remove_in(root, path, key);
            }
        }
        Ok(doc.to_string())
    }

    /// `with_project_key` applied to the file at `path` (normally
    /// `Config::path`; tests pass a scratch file), written through a
    /// temporary file and a rename so a crash mid-write cannot leave an
    /// empty file. Returns the parsed result so the caller can swap its
    /// in-memory copy. A file that does not parse is NOT touched: the error
    /// names it.
    pub fn save_project_key(
        path: &std::path::Path,
        project: &str,
        key: ProjectKey,
        value: Option<&str>,
    ) -> Result<Config, String> {
        Self::save_key(path, &["project", project], key.as_str(), value)
    }

    /// `save_key` on `[view]`.
    pub fn save_view_key(
        path: &std::path::Path,
        key: ViewKey,
        value: Option<&str>,
    ) -> Result<Config, String> {
        Self::save_key(path, &["view"], key.as_str(), value)
    }

    /// `with_key` applied to the file at `path`: see `save_project_key`.
    pub fn save_key(
        path: &std::path::Path,
        table: &[&str],
        key: &str,
        value: Option<&str>,
    ) -> Result<Config, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        // Refuse to write over something we cannot read back: the typed
        // parse is the one the viewer trusts, so it is the gate.
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let next = Self::with_key(&text, table, key, value)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = Self::parse(&next).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = path
            .parent()
            .ok_or_else(|| format!("{}: no parent directory", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tmp =
            tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(tmp.path(), next.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        tmp.persist(path)
            .map_err(|e| format!("{}: {}", path.display(), e.error))?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_the_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(
            Config::default().upload_choice("x"),
            UploadChoice::default()
        );
    }

    #[test]
    fn project_table_overrides_field_by_field() {
        let c = Config::parse(
            r#"
[upload]
images = "gcs"
gyazo_team = "everyone"

[upload.project.acme]
images = "gyazo"
gyazo_team = "acme-inc"

[upload.project.other]
gyazo_team = "other-org"
"#,
        )
        .unwrap();
        assert_eq!(c.upload_choice("acme").images.as_deref(), Some("gyazo"));
        assert_eq!(
            c.upload_choice("acme").gyazo_team.as_deref(),
            Some("acme-inc")
        );
        assert_eq!(
            c.upload_choice("other").images.as_deref(),
            Some("gcs"),
            "falls back to [upload]"
        );
        assert_eq!(
            c.upload_choice("other").gyazo_team.as_deref(),
            Some("other-org")
        );
        assert_eq!(
            c.upload_choice("nobody").gyazo_team.as_deref(),
            Some("everyone")
        );
    }

    #[test]
    fn project_table_beats_the_upload_tables() {
        let c = Config::parse(
            r#"
[upload]
images = "gcs"

[upload.project.acme]
images = "gyazo"
gyazo_team = "old-org"

[project.acme]
theme = "paper-dark"
display_name = "ACME"
gyazo_team = "new-org"
"#,
        )
        .unwrap();
        let acme = c.upload_choice("acme");
        assert_eq!(
            acme.images.as_deref(),
            Some("gyazo"),
            "old table still read"
        );
        assert_eq!(
            acme.gyazo_team.as_deref(),
            Some("new-org"),
            "new table wins"
        );
        assert_eq!(c.project_theme("acme").as_deref(), Some("paper-dark"));
        assert_eq!(c.project_display_name("acme").as_deref(), Some("ACME"));
        assert_eq!(c.project_theme("other"), None);
        assert!(
            Config::parse("[project.acme]\ncolour = \"x\"\n").is_err(),
            "unknown key in a project table"
        );
    }

    #[test]
    fn writing_a_key_keeps_the_rest_of_the_file() {
        let text = "# my notes\n[upload]\nimages = \"gcs\" # default\n";
        let next = Config::with_project_key(text, "acme", ProjectKey::Theme, Some("blue")).unwrap();
        assert!(next.starts_with("# my notes\n[upload]\nimages = \"gcs\" # default\n"));
        assert!(
            next.contains("[project.acme]\ntheme = \"blue\"\n"),
            "{next}"
        );
        assert!(
            !next.contains("\n[project]\n"),
            "no bare [project] header: {next}"
        );
        let c = Config::parse(&next).unwrap();
        assert_eq!(c.project_theme("acme").as_deref(), Some("blue"));

        // A second key joins the same table; a changed value replaces.
        let next =
            Config::with_project_key(&next, "acme", ProjectKey::DisplayName, Some("ACME")).unwrap();
        let next = Config::with_project_key(&next, "acme", ProjectKey::Theme, Some("red")).unwrap();
        let c = Config::parse(&next).unwrap();
        assert_eq!(c.project_theme("acme").as_deref(), Some("red"));
        assert_eq!(c.project_display_name("acme").as_deref(), Some("ACME"));
        assert_eq!(next.matches("[project.acme]").count(), 1, "{next}");
    }

    #[test]
    fn removing_the_last_key_removes_the_table() {
        let text = "[project.acme]\ntheme = \"blue\"\n\n[project.other]\ntheme = \"red\"\n";
        let next = Config::with_project_key(text, "acme", ProjectKey::Theme, None).unwrap();
        assert!(!next.contains("acme"), "{next}");
        assert!(next.contains("[project.other]"), "{next}");
        let next = Config::with_project_key(&next, "other", ProjectKey::Theme, None).unwrap();
        assert_eq!(next.trim(), "", "nothing left: {next:?}");
        // Removing from a file that never had the key is a no-op, not an error.
        assert_eq!(
            Config::with_project_key("[upload]\nimages = \"gcs\"\n", "x", ProjectKey::Theme, None)
                .unwrap(),
            "[upload]\nimages = \"gcs\"\n"
        );
    }

    #[test]
    fn the_view_table_reads_and_writes() {
        let c = Config::parse("[view]\nlang = \"ja\"\nappearance = \"dark\"\n").unwrap();
        assert_eq!(c.view.lang.as_deref(), Some("ja"));
        assert_eq!(c.view.appearance.as_deref(), Some("dark"));
        assert_eq!(c.view.theme, None);
        assert!(
            Config::parse("[view]\nlanguage = \"ja\"\n").is_err(),
            "unknown key"
        );

        let text = "# notes\n[project.acme]\ntheme = \"blue\"\n";
        let next = Config::with_view_key(text, ViewKey::Lang, Some("en")).unwrap();
        assert!(next.starts_with("# notes\n"), "{next}");
        assert!(next.contains("[view]\nlang = \"en\"\n"), "{next}");
        assert!(
            next.contains("[project.acme]\ntheme = \"blue\"\n"),
            "{next}"
        );
        let next = Config::with_view_key(&next, ViewKey::Lang, None).unwrap();
        assert!(!next.contains("[view]"), "empty table goes: {next}");
        assert!(next.contains("[project.acme]"), "{next}");
    }

    #[test]
    fn pick_follows_flag_env_file_default() {
        assert_eq!(
            pick(Some(1), Some(2), Some(3), (4, Origin::Default)),
            (1, Origin::Flag)
        );
        assert_eq!(
            pick(None, Some(2), Some(3), (4, Origin::Default)),
            (2, Origin::Env)
        );
        assert_eq!(
            pick(None, None, Some(3), (4, Origin::Default)),
            (3, Origin::File)
        );
        assert_eq!(
            pick::<i32>(None, None, None, (4, Origin::Auto)),
            (4, Origin::Auto)
        );
        assert!(Origin::Flag.beats_file() && Origin::Env.beats_file());
        assert!(!Origin::File.beats_file() && !Origin::Default.beats_file());
    }

    #[test]
    fn a_broken_file_is_not_written() {
        assert!(
            Config::with_project_key("[upload\n", "acme", ProjectKey::Theme, Some("blue")).is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[upload\n").unwrap();
        assert!(Config::save_project_key(&path, "acme", ProjectKey::Theme, Some("blue")).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[upload\n",
            "left alone"
        );
    }

    #[test]
    fn saving_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cosentty").join("config.toml");
        let c = Config::save_project_key(&path, "acme", ProjectKey::Theme, Some("blue")).unwrap();
        assert_eq!(c.project_theme("acme").as_deref(), Some("blue"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[project.acme]\ntheme = \"blue\"\n"
        );
        let c = Config::save_project_key(&path, "acme", ProjectKey::Theme, None).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn a_typo_is_an_error_not_a_default() {
        assert!(
            Config::parse("[upload]\nimage = \"gcs\"\n").is_err(),
            "unknown key"
        );
        assert!(Config::parse("[upload\n").is_err(), "not TOML");
    }
}
