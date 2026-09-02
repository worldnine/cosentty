//! The viewer's own settings file: `$XDG_CONFIG_HOME/cosense-tui/config.toml`
//! (default `~/.config/cosense-tui/config.toml`).
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
        Some(base.join("cosense-tui").join("config.toml"))
    }

    /// Read the file. `Ok(Config::default())` when there is none;
    /// `Err` only for a file that is there and does not parse.
    pub fn load() -> Result<Config, String> {
        let Some(path) = Self::path() else { return Ok(Config::default()) };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

    /// The upload choice the file makes for `project`: the project's own
    /// table, each field falling back to `[upload]`'s.
    pub fn upload_choice(&self, project: &str) -> UploadChoice {
        let all = &self.upload.all;
        let own = self.upload.project.get(project);
        UploadChoice {
            images: own.and_then(|c| c.images.clone()).or_else(|| all.images.clone()),
            gyazo_team: own.and_then(|c| c.gyazo_team.clone()).or_else(|| all.gyazo_team.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_the_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::default().upload_choice("x"), UploadChoice::default());
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
        assert_eq!(c.upload_choice("acme").gyazo_team.as_deref(), Some("acme-inc"));
        assert_eq!(c.upload_choice("other").images.as_deref(), Some("gcs"), "falls back to [upload]");
        assert_eq!(c.upload_choice("other").gyazo_team.as_deref(), Some("other-org"));
        assert_eq!(c.upload_choice("nobody").gyazo_team.as_deref(), Some("everyone"));
    }

    #[test]
    fn a_typo_is_an_error_not_a_default() {
        assert!(Config::parse("[upload]\nimage = \"gcs\"\n").is_err(), "unknown key");
        assert!(Config::parse("[upload\n").is_err(), "not TOML");
    }
}
