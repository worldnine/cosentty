//! The image on the clipboard, as a file — the other way a picture gets
//! into a page besides a dragged path (see `upload`).
//!
//! Terminals only ever hand an app TEXT from the clipboard, so this goes
//! around the terminal, to the OS:
//!
//! * macOS — a tiny Swift helper (`scripts/pbimage.swift`, embedded here
//!   and compiled once into `~/.cache/cosentty/pbimage-<hash>`, exactly
//!   as `ime` does). It writes the pixels out as PNG, or, when what was
//!   copied is an image FILE, answers with that file's own path;
//! * Wayland — `wl-paste --type image/png`; X11 — `xclip … -t image/png -o`.
//!
//! Every failure is a `Reason` the status line can show, because "nothing
//! happened" is the one answer a paste must never give.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const PBIMAGE_SWIFT: &str = include_str!("../scripts/pbimage.swift");

/// Why no image came out.
#[derive(Debug, PartialEq, Eq)]
pub enum Reason {
    /// Nothing on the clipboard is an image.
    NoImage,
    /// macOS: the helper is still being compiled (first use). Try again.
    HelperBuilding,
    /// macOS: no `swiftc`, so the helper cannot be built.
    NoSwiftc,
    /// Linux: neither `wl-paste` nor `xclip` answered.
    NoTool,
    /// Something else, in words.
    Other(String),
}

/// Fetch the clipboard image. The returned path is either a file the
/// user copied (left alone) or a PNG written under `dir` named `stem.png`.
pub fn image_to(dir: &Path, stem: &str) -> Result<PathBuf, Reason> {
    std::fs::create_dir_all(dir).map_err(|e| Reason::Other(e.to_string()))?;
    let out = dir.join(format!("{stem}.png"));
    if cfg!(target_os = "macos") {
        macos(&out)
    } else {
        linux(&out)
    }
}

/// Whether `path` is a file `image_to` wrote (and may be thrown away once
/// uploaded), as opposed to the user's own file.
pub fn is_scratch(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

fn macos(out: &Path) -> Result<PathBuf, Reason> {
    let bin = match bin_path() {
        Some(b) => b,
        None => {
            if which("swiftc").is_none() {
                return Err(Reason::NoSwiftc);
            }
            start_background_build();
            return Err(Reason::HelperBuilding);
        }
    };
    let res = Command::new(&bin)
        .arg(out)
        .output()
        .map_err(|e| Reason::Other(e.to_string()))?;
    match res.status.code() {
        Some(0) => {
            let path = String::from_utf8_lossy(&res.stdout).trim().to_string();
            if path.is_empty() {
                return Err(Reason::Other("pbimage: no path".into()));
            }
            Ok(PathBuf::from(path))
        }
        Some(1) => Err(Reason::NoImage),
        _ => Err(Reason::Other(
            String::from_utf8_lossy(&res.stderr).trim().to_string(),
        )),
    }
}

fn linux(out: &Path) -> Result<PathBuf, Reason> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let attempts: &[(&str, &[&str])] = if wayland {
        &[
            ("wl-paste", &["--no-newline", "--type", "image/png"]),
            (
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
            ),
        ]
    } else {
        &[
            (
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
            ),
            ("wl-paste", &["--no-newline", "--type", "image/png"]),
        ]
    };
    let mut any_tool = false;
    for (tool, args) in attempts {
        let Ok(res) = Command::new(tool).args(*args).output() else {
            continue;
        };
        any_tool = true;
        if res.status.success() && is_png(&res.stdout) {
            std::fs::write(out, &res.stdout).map_err(|e| Reason::Other(e.to_string()))?;
            return Ok(out.to_path_buf());
        }
    }
    Err(if any_tool {
        Reason::NoImage
    } else {
        Reason::NoTool
    })
}

/// PNG signature — what the Linux tools must hand back for the clipboard
/// to count as holding a picture.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

// ---- the helper's build, mirroring `ime` ---------------------------------

fn helper_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("cosentty")
}

fn helper_bin_name() -> String {
    format!(
        "pbimage-{:08x}",
        (crate::ime::fnv1a(PBIMAGE_SWIFT.as_bytes()) & 0xffff_ffff) as u32
    )
}

fn bin_path() -> Option<PathBuf> {
    let bin = helper_dir().join(helper_bin_name());
    bin.is_file().then_some(bin)
}

static BUILD_STARTED: AtomicBool = AtomicBool::new(false);

/// Compile the helper in the background, once per process, so the first
/// `^v` does not sit on swiftc. A no-op off macOS and when it exists.
pub fn start_background_build() {
    if !cfg!(target_os = "macos")
        || bin_path().is_some()
        || BUILD_STARTED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    std::thread::spawn(|| {
        let _ = ensure_binary();
    });
}

fn ensure_binary() -> Option<PathBuf> {
    if let Some(bin) = bin_path() {
        return Some(bin);
    }
    let dir = helper_dir();
    let bin = dir.join(helper_bin_name());
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("pbimage.swift");
    std::fs::write(&src, PBIMAGE_SWIFT).ok()?;
    let status = Command::new("swiftc")
        .args(["-O", src.to_str()?, "-o", bin.to_str()?])
        .status()
        .ok()?;
    (status.success() && bin.exists()).then_some(bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_signature_and_scratch_paths() {
        assert!(is_png(b"\x89PNG\r\n\x1a\n...."));
        assert!(!is_png(b"GIF89a"));
        let dir = Path::new("/tmp/cosentty/clip");
        assert!(is_scratch(&dir.join("clipboard-1.png"), dir));
        assert!(!is_scratch(Path::new("/Users/me/shot.png"), dir));
    }

    #[test]
    fn helper_bin_name_embeds_the_source_hash() {
        assert!(helper_bin_name().starts_with("pbimage-"));
        assert_eq!(helper_bin_name().len(), "pbimage-".len() + 8);
    }

    /// Needs a macOS clipboard with an image on it, so ignored by default:
    /// `cargo test --lib clipboard -- --ignored` after copying a picture.
    #[test]
    #[ignore]
    fn reads_the_clipboard_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = image_to(dir.path(), "clip").unwrap();
        assert!(path.is_file());
    }
}
