//! Which language the reader is addressed in.
//!
//! Both texts live at the call site (see [`t!`]) rather than in a catalog
//! keyed by names like `error.reload_failed`. A key is a third thing to
//! invent, keep in sync and look up; with two literals side by side the
//! message and its translation are read, reviewed and changed together.
//! The cost is that adding a message means writing it twice, which is the
//! honest price of speaking to both readers.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

/// 0 = not chosen yet. Set once at startup, read on every message.
static CURRENT: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// A language for THIS thread only, above the process-wide one.
    ///
    /// Tests are threads that run at the same time and assert on wording,
    /// so a test that wants English must not change what a test running
    /// beside it sees. Nothing in the running program sets this.
    static OVERRIDE: Cell<u8> = const { Cell::new(0) };
}

fn code(lang: Lang) -> u8 {
    match lang {
        Lang::Ja => 1,
        Lang::En => 2,
    }
}

fn decode(v: u8) -> Option<Lang> {
    match v {
        1 => Some(Lang::Ja),
        2 => Some(Lang::En),
        _ => None,
    }
}

impl Lang {
    /// Which language a locale string asks for. Anything that is not
    /// Japanese is answered in English: it is the language this program is
    /// most likely to be understood in when the environment says nothing
    /// useful.
    pub fn from_locale(s: &str) -> Option<Lang> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        // `ja`, `ja_JP.UTF-8`, `japanese`
        if lower == "ja" || lower.starts_with("ja_") || lower.starts_with("ja-") || lower.starts_with("japanese") {
            return Some(Lang::Ja);
        }
        // `C` and `POSIX` are "no preference stated", not "English wanted",
        // but the answer is the same and saying so keeps the chain short.
        Some(Lang::En)
    }

    /// The reader's language, in the order a reader would expect to be
    /// asked: the flag they just typed, then this program's own variable,
    /// then the locale the shell already carries.
    pub fn detect(explicit: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> Lang {
        if let Some(l) = explicit.and_then(Lang::from_locale) {
            return l;
        }
        for var in ["COSENSE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(l) = env(var).as_deref().and_then(Lang::from_locale) {
                return l;
            }
        }
        Lang::En
    }
}

/// Fix the language for the rest of the process.
pub fn set(lang: Lang) {
    CURRENT.store(code(lang), Ordering::Relaxed);
}

/// Fix the language for the current thread only (tests).
pub fn set_for_thread(lang: Lang) {
    OVERRIDE.with(|o| o.set(code(lang)));
}

pub fn current() -> Lang {
    if let Some(l) = OVERRIDE.with(|o| decode(o.get())) {
        return l;
    }
    // Nothing chosen at all: `main` always sets one before drawing, so this
    // is what an un-initialised test or a stray background thread sees.
    decode(CURRENT.load(Ordering::Relaxed)).unwrap_or(Lang::En)
}

pub fn is_ja() -> bool {
    current() == Lang::Ja
}

/// One message, in both languages: `t!("行がありません", "no such line")`.
///
/// Takes `format!` arguments and returns `String`, so a message with a
/// value in it is written once per language and nothing else changes:
/// `t!("{n} 行削除", "deleted {n} line(s)")`.
#[macro_export]
macro_rules! t {
    ($ja:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::lang::is_ja() {
            format!($ja $(, $arg)*)
        } else {
            format!($en $(, $arg)*)
        }
    };
}

/// [`t!`] for the places that need a `&'static str` (a match arm feeding a
/// wider expression, a `Display` impl) and therefore cannot allocate.
#[macro_export]
macro_rules! ts {
    ($ja:literal, $en:literal) => {
        if $crate::lang::is_ja() { $ja } else { $en }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locale_string_is_read_as_japanese_only_when_it_says_so() {
        assert_eq!(Lang::from_locale("ja_JP.UTF-8"), Some(Lang::Ja));
        assert_eq!(Lang::from_locale("ja"), Some(Lang::Ja));
        assert_eq!(Lang::from_locale("Japanese_Japan.932"), Some(Lang::Ja));
        assert_eq!(Lang::from_locale("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::from_locale("fr_FR"), Some(Lang::En), "not Japanese → English");
        assert_eq!(Lang::from_locale("C"), Some(Lang::En));
        assert_eq!(Lang::from_locale("  "), None, "an empty variable is not an answer");
        // `java`-like prefixes must not be mistaken for `ja`
        assert_eq!(Lang::from_locale("java"), Some(Lang::En));
    }

    /// The same call site answers in either language, and a thread that
    /// picks one does not disturb the thread beside it.
    #[test]
    fn a_message_is_written_in_whichever_language_the_thread_asked_for() {
        set_for_thread(Lang::Ja);
        let n = 3;
        assert_eq!(crate::t!("{n} 行削除", "deleted {n} line(s)"), "3 行削除");
        assert_eq!(crate::ts!("一覧", "index"), "一覧");

        let other = std::thread::spawn(|| {
            set_for_thread(Lang::En);
            let n = 3;
            crate::t!("{n} 行削除", "deleted {n} line(s)")
        })
        .join()
        .unwrap();
        assert_eq!(other, "deleted 3 line(s)");
        assert_eq!(crate::t!("{n} 行削除", "deleted {n} line(s)"), "3 行削除", "unchanged here");
    }

    #[test]
    fn the_flag_outranks_the_environment_and_an_empty_one_does_not_count() {
        let env = |k: &str| match k {
            "LANG" => Some("ja_JP.UTF-8".to_string()),
            _ => None,
        };
        assert_eq!(Lang::detect(Some("en"), &env), Lang::En, "the flag wins");
        assert_eq!(Lang::detect(Some(""), &env), Lang::Ja, "an empty flag is not a choice");
        assert_eq!(Lang::detect(None, &env), Lang::Ja);
        assert_eq!(Lang::detect(None, &|_| None), Lang::En, "nothing said → English");

        // The specific variables outrank the general one, as in POSIX.
        let env = |k: &str| match k {
            "LC_ALL" => Some("en_US.UTF-8".to_string()),
            "LANG" => Some("ja_JP.UTF-8".to_string()),
            _ => None,
        };
        assert_eq!(Lang::detect(None, &env), Lang::En);
        // COSENSE_LANG is this program's own say, above the shell's locale.
        let env = |k: &str| match k {
            "COSENSE_LANG" => Some("ja".to_string()),
            "LC_ALL" => Some("en_US.UTF-8".to_string()),
            _ => None,
        };
        assert_eq!(Lang::detect(None, &env), Lang::Ja);
    }
}
