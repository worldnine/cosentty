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

/// Where a project-level value on screen came from. The settings screen
/// shows it next to each value, so "gcs" alone never hides that the
/// project's own setting was simply unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `/api/projects/<slug>` answered.
    Api,
    /// `config.toml` names it.
    File,
    /// Neither: the built-in fallback.
    Default,
}

impl Origin {
    /// The short tag the settings screen prints. Not translated: `api` /
    /// `file` are the names the documentation uses.
    pub fn tag(self) -> &'static str {
        match self {
            Origin::Api => "api",
            Origin::File => "file",
            Origin::Default => "-",
        }
    }
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
        match std::fs::read_to_string(&path) {
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

    /// Set (or, with `None`, remove) one key of `[project.<slug>]` in the
    /// file's TEXT, keeping every comment, every other table and the
    /// author's order: a settings screen that rewrote the file from its
    /// parsed form would throw away whatever it did not understand. A table
    /// left empty is removed with its last key. Errors only for text that is
    /// not TOML — the caller must not overwrite a broken file.
    pub fn with_project_key(
        text: &str,
        project: &str,
        key: ProjectKey,
        value: Option<&str>,
    ) -> Result<String, String> {
        use toml_edit::{DocumentMut, Item, Table};
        let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.message().to_string())?;
        let root = doc.as_table_mut();
        match value {
            Some(v) => {
                let projects = root
                    .entry("project")
                    .or_insert_with(|| {
                        let mut t = Table::new();
                        // `[project]` itself holds no keys: keep it out of the
                        // text so the file starts at `[project.<slug>]`.
                        t.set_implicit(true);
                        Item::Table(t)
                    })
                    .as_table_mut()
                    .ok_or_else(|| "`project` is not a table".to_string())?;
                let sect = projects
                    .entry(project)
                    .or_insert_with(|| Item::Table(Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("`project.{project}` is not a table"))?;
                sect[key.as_str()] = toml_edit::value(v);
            }
            None => {
                let Some(projects) = root.get_mut("project").and_then(Item::as_table_mut) else {
                    return Ok(doc.to_string());
                };
                let mut drop_sect = false;
                if let Some(sect) = projects.get_mut(project).and_then(Item::as_table_mut) {
                    sect.remove(key.as_str());
                    drop_sect = sect.is_empty();
                }
                if drop_sect {
                    projects.remove(project);
                }
                if projects.is_empty() {
                    root.remove("project");
                }
            }
        }
        Ok(doc.to_string())
    }

    /// `with_project_key` applied to the file on disk (`Config::path`),
    /// written through a temporary file and a rename so a crash mid-write
    /// cannot leave an empty file. Returns the parsed result so the caller
    /// can swap its in-memory copy. A file that does not parse is NOT
    /// touched: the error names it.
    pub fn save_project_key(
        project: &str,
        key: ProjectKey,
        value: Option<&str>,
    ) -> Result<Config, String> {
        let path = Self::path().ok_or_else(|| {
            "no config path (neither $XDG_CONFIG_HOME nor $HOME is set)".to_string()
        })?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        // Refuse to write over something we cannot read back: the typed
        // parse is the one the viewer trusts, so it is the gate.
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let next = Self::with_project_key(&text, project, key, value)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = Self::parse(&next).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = path
            .parent()
            .ok_or_else(|| format!("{}: no parent directory", path.display()))?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(tmp.path(), next.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        tmp.persist(&path).map_err(|e| format!("{}: {}", path.display(), e.error))?;
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
        assert_eq!(acme.images.as_deref(), Some("gyazo"), "old table still read");
        assert_eq!(acme.gyazo_team.as_deref(), Some("new-org"), "new table wins");
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
        assert!(next.contains("[project.acme]\ntheme = \"blue\"\n"), "{next}");
        assert!(!next.contains("\n[project]\n"), "no bare [project] header: {next}");
        let c = Config::parse(&next).unwrap();
        assert_eq!(c.project_theme("acme").as_deref(), Some("blue"));

        // A second key joins the same table; a changed value replaces.
        let next = Config::with_project_key(&next, "acme", ProjectKey::DisplayName, Some("ACME")).unwrap();
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
            Config::with_project_key("[upload]\nimages = \"gcs\"\n", "x", ProjectKey::Theme, None).unwrap(),
            "[upload]\nimages = \"gcs\"\n"
        );
    }

    #[test]
    fn a_broken_file_is_not_written() {
        assert!(Config::with_project_key("[upload\n", "acme", ProjectKey::Theme, Some("blue")).is_err());
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
