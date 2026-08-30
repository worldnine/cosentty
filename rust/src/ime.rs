//! macOS input-source switching — the Japanese-first operating model.
//!
//! Ported from akapen's `src/ime.rs` (same embedded Swift helper, Carbon
//! TIS API, no accessibility permission). The policy that makes a
//! Japanese environment first-class:
//!
//!   - command mode always runs in ASCII: the session forces 英数 at
//!     startup and after every composer close, so j/k/o/E are never
//!     swallowed by the IME;
//!   - the composer opens in Japanese (default `--ime jp`): body text is
//!     Japanese, so no manual toggle in either direction;
//!   - on exit the input source the user had before launching is restored.
//!
//! The helper compiles once into `~/.cache/cosense-tui/ime-<hash>`; a
//! helper already built by akapen from the SAME source (same hash, in
//! `~/.cache/akapen/`) is reused as is. On non-macOS or without swiftc
//! every call degrades to a no-op.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

const IME_SWIFT: &str = include_str!("../scripts/ime.swift");

/// FNV-1a 64-bit (see akapen: picks a cache file name, nothing more).
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn bin_name_for(src: &str) -> String {
    format!("ime-{:08x}", (fnv1a(src.as_bytes()) & 0xffff_ffff) as u32)
}

fn helper_bin_name() -> String {
    bin_name_for(IME_SWIFT)
}

fn cache_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache")
}

fn ime_dir() -> PathBuf {
    cache_root().join("cosense-tui")
}

/// The compiled helper: ours, or akapen's build of the identical source.
fn bin_path() -> Option<PathBuf> {
    let name = helper_bin_name();
    for dir in [ime_dir(), cache_root().join("akapen")] {
        let bin = dir.join(&name);
        if bin.is_file() {
            return Some(bin);
        }
    }
    None
}

static BUILD_STARTED: AtomicBool = AtomicBool::new(false);

/// Background-compile the helper once per process (no-op when it already
/// exists), so the first composer open never blocks on swiftc.
pub fn start_background_build() {
    if bin_path().is_some() || BUILD_STARTED.swap(true, Ordering::SeqCst) {
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
    let dir = ime_dir();
    let bin = dir.join(helper_bin_name());
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("ime.swift");
    std::fs::write(&src, IME_SWIFT).ok()?;
    let status = Command::new("swiftc")
        .args(["-O", src.to_str()?, "-o", bin.to_str()?])
        .status()
        .ok()?;
    (status.success() && bin.exists()).then_some(bin)
}

/// Run the helper; `None` while it is still building (callers no-op).
fn ime(args: &[&str]) -> Option<String> {
    let bin = bin_path()?;
    let out = Command::new(&bin).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn set_ascii() {
    let _ = ime(&["abc"]);
}

fn set_japanese() {
    let _ = ime(&["jp"]);
}

/// Suspend marker for an external `ime guard` daemon (akapen's contract:
/// the guard pins ASCII while the terminal is frontmost, and stands down
/// while this file exists so composers can type Japanese).
fn guard_suspend_path_from_env(home: &str, env: Option<&str>) -> PathBuf {
    match env {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(home).join(".cache").join("ime-guard.suspend"),
    }
}

fn guard_suspend_path() -> PathBuf {
    guard_suspend_path_from_env(
        &std::env::var("HOME").unwrap_or_else(|_| ".".to_string()),
        std::env::var("IME_GUARD_SUSPEND_FILE").ok().as_deref(),
    )
}

fn suspend_external_ime_guard() {
    let p = guard_suspend_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap_or_else(|| std::path::Path::new(".")));
    let _ = std::fs::write(&p, "");
}

fn resume_external_ime_guard() {
    let _ = std::fs::remove_file(guard_suspend_path());
}

/// `--ime <mode>`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImeMode {
    /// No input-source control at all.
    Off,
    /// ASCII after the composer closes; typing state untouched while open.
    Ascii,
    /// Also switch to Japanese when the composer opens (the default here:
    /// this viewer is Japanese-first, unlike akapen's neutral default).
    Jp,
}

impl ImeMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "off" => ImeMode::Off,
            "ascii" => ImeMode::Ascii,
            _ => ImeMode::Jp,
        }
    }
}

/// Held while a composer is open: optionally force Japanese on enter, and
/// always return to ASCII on drop (unless `Off`) so navigation keys work
/// the instant the composer closes. Suspends an external `ime guard`
/// daemon for the duration.
pub struct ImeGuard {
    mode: ImeMode,
}

impl ImeGuard {
    pub fn enter(mode: ImeMode) -> Self {
        if mode != ImeMode::Off {
            suspend_external_ime_guard();
        }
        if mode == ImeMode::Jp {
            set_japanese();
        }
        ImeGuard { mode }
    }
}

impl Drop for ImeGuard {
    fn drop(&mut self) {
        if self.mode != ImeMode::Off {
            resume_external_ime_guard();
            set_ascii();
        }
    }
}

/// Session-level control: command mode runs in ASCII; the input source the
/// user had before launching is restored on drop.
pub struct SessionIme {
    mode: ImeMode,
    saved: Option<String>,
}

impl SessionIme {
    pub fn new(mode: ImeMode) -> Self {
        Self { mode, saved: None }
    }

    /// Save the current source and switch to ASCII. Returns true when the
    /// session is protected (or `Off`); retry until true while the helper
    /// compiles in the background.
    pub fn force_ascii(&mut self) -> bool {
        if self.mode == ImeMode::Off {
            return true;
        }
        if self.saved.is_none() {
            self.saved = ime(&["get"]);
        }
        ime(&["abc"]).is_some()
    }
}

impl Drop for SessionIme {
    fn drop(&mut self) {
        if self.mode == ImeMode::Off {
            return;
        }
        if let Some(id) = &self.saved {
            let _ = ime(&["set", id]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_source_supports_the_commands() {
        assert!(IME_SWIFT.contains("case \"abc\""));
        assert!(IME_SWIFT.contains("case \"jp\""));
        assert!(IME_SWIFT.contains("TISSelectInputSource"));
    }

    #[test]
    fn helper_bin_name_embeds_the_source_hash() {
        assert_eq!(helper_bin_name(), bin_name_for(IME_SWIFT));
        assert!(helper_bin_name().starts_with("ime-"));
        assert_eq!(helper_bin_name().len(), "ime-".len() + 8);
    }

    #[test]
    fn guard_suspend_path_matches_akapen_contract() {
        assert_eq!(
            guard_suspend_path_from_env("/Users/me", None),
            PathBuf::from("/Users/me/.cache/ime-guard.suspend")
        );
        assert_eq!(
            guard_suspend_path_from_env("/Users/me", Some("/tmp/x")),
            PathBuf::from("/tmp/x")
        );
    }

    #[test]
    fn default_mode_is_japanese_first() {
        assert_eq!(ImeMode::parse("jp"), ImeMode::Jp);
        assert_eq!(ImeMode::parse("ascii"), ImeMode::Ascii);
        assert_eq!(ImeMode::parse("off"), ImeMode::Off);
        // unknown → jp (the Japanese-first default)
        assert_eq!(ImeMode::parse(""), ImeMode::Jp);
    }
}

