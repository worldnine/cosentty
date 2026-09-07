//! Putting a picture INTO a page: recognising a pasted image path, deciding
//! where it goes, and sending it there. The read side (drawing `[URL]`) has
//! long existed in `render` / `image_fetch`; this is the write side.
//!
//! Two destinations, chosen per project (see `Destination::resolve`):
//!
//! * `gcs` — Cosense's own file storage, the site default. Three requests
//!   (`upload-request` → `PUT signedUrl` → `verify`), the official CLI's
//!   `uploadFile` route; a file already there answers at the first step.
//!   The picture belongs to the project and shares its permissions, which is
//!   why it is also this viewer's fallback: not knowing where to send a
//!   picture must not send it OUTSIDE.
//! * `gyazo` — one `POST` to `upload.gyazo.com`; the token decides which
//!   Gyazo (personal or a Teams org) receives it. Teams permalinks have to
//!   be rebuilt as `https://<org>.gyazo.com/<id>`: the API answers with a
//!   `gyazo.com/<id>` that 404s (measured).

use std::error::Error;
use std::path::{Path, PathBuf};

use crate::api::ProjectSettings;
use crate::config::Config;
use crate::url::percent_decode;

/// One Gyazo upload may take this long end to end (a photo over a slow
/// line), independent of the shorter limit the page requests live under.
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// File extensions accepted as an image to upload — the same set the
/// renderer draws, so what goes up comes back as a picture.
pub const IMAGE_EXTS: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".tiff",
];

/// Cosense's per-file cap (the API answers 413 above it; better said here).
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;

/// The local image file a paste names, if that is what it is.
///
/// Terminals hand a dragged file to the app as its path — the one way to
/// get a picture in without an OS clipboard helper — and they dress it in
/// several ways: a trailing space or newline, `file://`, shell-escaped
/// spaces, quotes, `~`. All are undone here. The answer is `Some` only for
/// a single line naming an EXISTING file with an image extension; anything
/// else is ordinary text and pastes as such.
pub fn image_path_from_paste(pasted: &str) -> Option<PathBuf> {
    let line = pasted.trim();
    if line.is_empty() || line.contains('\n') {
        return None;
    }
    let mut s = line.to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        s = percent_decode(rest);
    }
    if (s.starts_with('\'') && s.ends_with('\'') || s.starts_with('"') && s.ends_with('"'))
        && s.len() >= 2
    {
        s = s[1..s.len() - 1].to_string();
    }
    let s = s.replace("\\ ", " ");
    let s = match s.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest).to_string_lossy().into_owned(),
            None => return None,
        },
        None => s,
    };
    if !has_image_ext(&s) {
        return None;
    }
    let path = PathBuf::from(s);
    path.is_file().then_some(path)
}

pub fn has_image_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS.iter().any(|e| lower.ends_with(e))
}

/// MIME type from the extension. Only the image set above ever reaches
/// here; the fallback is for form's sake.
pub fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("tiff") => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// Lower-case hex MD5, the form `upload-request` wants.
pub fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}

/// Where a pasted picture goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Cosense file storage (`/api/gcs/<projectId>/…`).
    Gcs,
    /// Gyazo; `team` is the Teams org (`<team>.gyazo.com`) or `None` for
    /// personal gyazo.com.
    Gyazo { team: Option<String> },
}

/// Who decided the destination — the status line says so when it was
/// nobody, because "gcs" alone does not tell the reader that the project's
/// own setting (Gyazo, say) was simply out of reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decided {
    /// `config.toml` named it.
    File,
    /// The project's Upload tab named it.
    Project,
    /// Neither could be read: the safe default.
    Default,
}

impl Destination {
    /// `resolve`, with who decided.
    ///
    /// A project response WITHOUT `uploadImageTo` counts as unread: the
    /// public view of a project (no sid, or a PAT) carries the theme and
    /// even `gyazoTeamsName` but not this field (measured on acme-edu),
    /// and a team name alone does not mean Gyazo — `別のプロジェクト` has one
    /// and uploads to gcs.
    pub fn resolve_with(
        config: &Config,
        project: &str,
        settings: Option<&ProjectSettings>,
    ) -> (Destination, Decided) {
        let choice = config.upload_choice(project);
        let (kind, decided) = match (
            choice.images.clone(),
            settings.and_then(|s| s.upload_image_to.clone()),
        ) {
            (Some(k), _) => (k, Decided::File),
            (None, Some(k)) => (k, Decided::Project),
            (None, None) => ("gcs".to_string(), Decided::Default),
        };
        let dest = match kind.as_str() {
            "gyazo" => {
                let team = choice
                    .gyazo_team
                    .clone()
                    .or_else(|| settings.and_then(|s| s.gyazo_teams_name.clone()))
                    .filter(|t| !t.is_empty());
                Destination::Gyazo { team }
            }
            _ => Destination::Gcs,
        };
        (dest, decided)
    }

    /// TOML > project setting > `gcs`. The project's own Upload tab is what
    /// the browser follows, so following it keeps the two from diverging;
    /// the file overrides it for whoever wants otherwise; and with neither
    /// (`/api/projects/<name>` refuses a PAT, so the setting is often out of
    /// reach) the picture stays inside the project.
    pub fn resolve(
        config: &Config,
        project: &str,
        settings: Option<&ProjectSettings>,
    ) -> Destination {
        Self::resolve_with(config, project, settings).0
    }

    /// What the status line calls it.
    pub fn label(&self) -> String {
        match self {
            Destination::Gcs => "gcs".into(),
            Destination::Gyazo { team: None } => "gyazo.com".into(),
            Destination::Gyazo { team: Some(t) } => format!("{t}.gyazo.com"),
        }
    }
}

/// Upload `bytes` to Gyazo with `token` and return the permalink to paste.
pub fn upload_gyazo(
    http: &reqwest::blocking::Client,
    token: &str,
    bytes: Vec<u8>,
    name: &str,
    content_type: &str,
    team: Option<&str>,
) -> Result<String, Box<dyn Error>> {
    #[derive(serde::Deserialize)]
    struct Resp {
        image_id: Option<String>,
        permalink_url: Option<String>,
    }
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(name.to_string())
        .mime_str(content_type)?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("access_token", token.to_string())
        .part("imagedata", part);
    // The shared client's request timeout is sized for a page, not a
    // picture: a large upload on a slow line gets its own, longer limit.
    let res = http
        .post("https://upload.gyazo.com/api/upload")
        .timeout(UPLOAD_TIMEOUT)
        .multipart(form)
        .send()?;
    if !res.status().is_success() {
        return Err(format!("Gyazo: HTTP {}", res.status()).into());
    }
    let r: Resp = res.json()?;
    gyazo_permalink(team, r.image_id.as_deref(), r.permalink_url.as_deref())
        .ok_or_else(|| "Gyazo: no image_id in response".into())
}

/// The permalink the page should carry. Teams: rebuilt from the id, since
/// the API's own `permalink_url` points at gyazo.com and does not resolve.
pub fn gyazo_permalink(
    team: Option<&str>,
    image_id: Option<&str>,
    permalink_url: Option<&str>,
) -> Option<String> {
    match (team, image_id) {
        (Some(org), Some(id)) => Some(format!("https://{org}.gyazo.com/{id}")),
        (None, _) if permalink_url.is_some() => permalink_url.map(str::to_string),
        (None, Some(id)) => Some(format!("https://gyazo.com/{id}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_is_an_image_path_only_when_the_file_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot 1.PNG");
        std::fs::write(&png, b"x").unwrap();
        let p = png.to_string_lossy().into_owned();
        assert_eq!(image_path_from_paste(&p), Some(png.clone()));
        assert_eq!(
            image_path_from_paste(&format!("{p} \n")),
            Some(png.clone()),
            "terminal's trailing space"
        );
        assert_eq!(
            image_path_from_paste(&p.replace(' ', "\\ ")),
            Some(png.clone()),
            "shell escape"
        );
        assert_eq!(
            image_path_from_paste(&format!("'{p}'")),
            Some(png.clone()),
            "quoted"
        );
        assert_eq!(
            image_path_from_paste(&format!("file://{}", p.replace(' ', "%20"))),
            Some(png.clone()),
            "file URL"
        );
        assert_eq!(
            image_path_from_paste(&format!("{p}\n{p}")),
            None,
            "two lines are text"
        );
        assert_eq!(
            image_path_from_paste(&p.replace(".PNG", ".txt")),
            None,
            "not an image"
        );
        assert_eq!(
            image_path_from_paste(&p.replace("shot", "gone")),
            None,
            "not a file"
        );
        assert_eq!(
            image_path_from_paste("https://example.com/a.png"),
            None,
            "a URL is not a path"
        );
    }

    #[test]
    fn md5_is_lower_hex() {
        assert_eq!(md5_hex(b"hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn destination_prefers_file_then_project_then_gcs() {
        let settings = ProjectSettings {
            theme: None,
            upload_image_to: Some("gyazo".into()),
            gyazo_teams_name: Some("acme-inc".into()),
            ..Default::default()
        };
        let none = Config::default();
        assert_eq!(
            Destination::resolve(&none, "p", None),
            Destination::Gcs,
            "knowing nothing stays inside"
        );
        assert_eq!(
            Destination::resolve(&none, "p", Some(&settings)),
            Destination::Gyazo {
                team: Some("acme-inc".into())
            },
            "the project's Upload tab is followed"
        );
        let file = Config::parse("[upload.project.p]\nimages = \"gcs\"\n").unwrap();
        assert_eq!(
            Destination::resolve(&file, "p", Some(&settings)),
            Destination::Gcs,
            "the file wins"
        );
        let file = Config::parse("[upload]\nimages = \"gyazo\"\n").unwrap();
        assert_eq!(
            Destination::resolve(&file, "q", None),
            Destination::Gyazo { team: None },
            "gyazo with no team is personal gyazo.com"
        );
        assert_eq!(
            Destination::Gyazo {
                team: Some("x".into())
            }
            .label(),
            "x.gyazo.com"
        );

        // Who decided is reported; a public view with the field missing
        // is "nobody", even with a team name on it.
        assert_eq!(
            Destination::resolve_with(&none, "p", Some(&settings)).1,
            Decided::Project
        );
        assert_eq!(Destination::resolve_with(&file, "q", None).1, Decided::File);
        let public_view = ProjectSettings {
            theme: Some("x".into()),
            upload_image_to: None,
            gyazo_teams_name: Some("org".into()),
            ..Default::default()
        };
        assert_eq!(
            Destination::resolve_with(&none, "p", Some(&public_view)),
            (Destination::Gcs, Decided::Default)
        );
    }

    #[test]
    fn teams_permalink_is_rebuilt_from_the_id() {
        assert_eq!(
            gyazo_permalink(Some("org"), Some("abc"), Some("https://gyazo.com/abc")).as_deref(),
            Some("https://org.gyazo.com/abc")
        );
        assert_eq!(
            gyazo_permalink(None, Some("abc"), Some("https://gyazo.com/abc")).as_deref(),
            Some("https://gyazo.com/abc")
        );
        assert_eq!(gyazo_permalink(Some("org"), None, None), None);
    }
}
