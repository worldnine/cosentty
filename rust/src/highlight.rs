//! Syntax highlighting for code blocks via syntect (default-fancy: pure-Rust
//! regex, no C toolchain). Adapted from akapen's highlight.rs, pared down to
//! what a Scrapbox `code:name.ext` block needs: pick a grammar by the block's
//! language/extension, tokenize the whole block once (cross-line context like
//! strings/comments needs it), and return ratatui spans per line.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedLazyThemeSet;

/// Default dark theme (matches ghostty's usual dark background).
pub const DEFAULT_THEME: &str = "Catppuccin Mocha";
pub const DEFAULT_THEME_LIGHT: &str = "Solarized (light)";

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(|| {
        // build.rs appends the current Sublime Markdown grammar to two-face's
        // set. Keep YAML parsing out of TUI startup; this is the same compact
        // dump-loading path two-face itself uses.
        syntect::dumps::from_uncompressed_data(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/cosense-syntaxes.packdump"
        )))
        .expect("build-time syntax dump is valid")
    })
}

fn embedded_themes() -> &'static EmbeddedLazyThemeSet {
    static T: OnceLock<EmbeddedLazyThemeSet> = OnceLock::new();
    T.get_or_init(two_face::theme::extra)
}

fn embedded_by_name(name: &str) -> Option<Theme> {
    EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name() == name)
        .map(|t| embedded_themes().get(t).clone())
}

/// The reader's own `.tmTheme` files, read once from every directory
/// `theme_dirs` names (cosentty's own, then bat's — where people already
/// keep these). A theme is known by its `name` entry, or its file name
/// without the extension when it has none; one that shares a name with an
/// embedded theme replaces it (bat's rule), and an earlier directory beats
/// a later one. Files that do not parse are kept as messages, said once at
/// startup, and skipped.
pub struct UserThemes {
    themes: Vec<(String, Theme)>,
    pub errors: Vec<String>,
}

impl UserThemes {
    /// Read every `*.tmTheme` in `dir`, in name order. A missing directory
    /// is simply no themes.
    pub fn load(dir: &Path) -> Self {
        let mut themes = Vec::new();
        let mut errors = Vec::new();
        let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.eq_ignore_ascii_case("tmTheme"))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        for path in files {
            match ThemeSet::get_theme(&path) {
                Ok(theme) => {
                    let name = theme
                        .name
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .or_else(|| {
                            path.file_stem()
                                .and_then(|s| s.to_str())
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| path.display().to_string());
                    themes.push((name, theme));
                }
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        Self { themes, errors }
    }

    /// `load` over several directories; the first directory to name a
    /// theme keeps it.
    pub fn load_dirs(dirs: &[PathBuf]) -> Self {
        let mut all = Self {
            themes: Vec::new(),
            errors: Vec::new(),
        };
        for dir in dirs {
            let one = Self::load(dir);
            for (name, theme) in one.themes {
                if all.get(&name).is_none() {
                    all.themes.push((name, theme));
                }
            }
            all.errors.extend(one.errors);
        }
        all
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.iter().map(|(n, _)| n.as_str())
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }
}

/// Where the reader's themes live: cosentty's own directory (next to
/// config.toml), then bat's, since that is where `.tmTheme` files are
/// already kept by anyone who uses bat or delta. Only directories that
/// exist are listed, so the startup notice can name real places.
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(own) =
        crate::config::Config::path().and_then(|p| p.parent().map(|d| d.join("themes")))
    {
        dirs.push(own);
    }
    dirs.extend(bat_theme_dirs(&|k| std::env::var_os(k).map(PathBuf::from)));
    dirs
}

/// bat's `themes/` candidates, in bat's own order of preference:
/// `$BAT_CONFIG_DIR`, then `$XDG_CONFIG_HOME/bat`, then `~/.config/bat`,
/// and on macOS also `~/Library/Application Support/bat` (bat's default
/// there). `env` is looked up by name so tests can feed their own.
pub fn bat_theme_dirs(env: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = env("BAT_CONFIG_DIR").filter(|p| !p.as_os_str().is_empty()) {
        dirs.push(d.join("themes"));
    }
    if let Some(x) = env("XDG_CONFIG_HOME").filter(|p| !p.as_os_str().is_empty()) {
        dirs.push(x.join("bat").join("themes"));
    }
    if let Some(home) = env("HOME").filter(|p| !p.as_os_str().is_empty()) {
        dirs.push(home.join(".config").join("bat").join("themes"));
        if cfg!(target_os = "macos") {
            dirs.push(
                home.join("Library")
                    .join("Application Support")
                    .join("bat")
                    .join("themes"),
            );
        }
    }
    dirs.dedup();
    dirs
}

fn user_themes() -> &'static UserThemes {
    static U: OnceLock<UserThemes> = OnceLock::new();
    U.get_or_init(|| UserThemes::load_dirs(&theme_dirs()))
}

fn theme_by_name(name: &str) -> Option<Theme> {
    user_themes()
        .get(name)
        .cloned()
        .or_else(|| embedded_by_name(name))
}

pub struct Highlighter {
    theme: Theme,
}

impl Highlighter {
    /// Every theme name `--theme` accepts: the reader's own first, then the
    /// embedded ones in two-face's order.
    pub fn theme_names() -> Vec<String> {
        let mut names: Vec<String> = user_themes().names().map(str::to_string).collect();
        for t in EmbeddedLazyThemeSet::theme_names() {
            let n = t.as_name();
            if !names.iter().any(|u| u == n) {
                names.push(n.to_string());
            }
        }
        names
    }

    /// Is `name` a theme `new` would actually use (rather than fall back)?
    pub fn theme_exists(name: &str) -> bool {
        theme_by_name(name).is_some()
    }

    /// The reader's theme files that could not be read, for the startup
    /// notice.
    pub fn user_theme_errors() -> Vec<String> {
        user_themes().errors.clone()
    }

    /// Build from an optional theme name; unknown names fall back to the
    /// light/dark default.
    pub fn new(theme_name: Option<&str>, light: bool) -> Self {
        let theme = theme_name.and_then(theme_by_name).unwrap_or_else(|| {
            let name = if light {
                DEFAULT_THEME_LIGHT
            } else {
                DEFAULT_THEME
            };
            embedded_by_name(name).expect("default theme is embedded")
        });
        Self { theme }
    }

    /// Pick a grammar for a Scrapbox code-block language token. Scrapbox
    /// writes `code:name.ext` (e.g. `code:hello.js`, `code:Dockerfile`), so
    /// try the token as an extension, then as a name/filename, then plain.
    fn syntax_for(&self, lang: &str) -> &'static SyntaxReference {
        let ss = syntaxes();
        let lang = lang.trim();
        // `foo.rs` -> extension "rs"; bare "rust"/"rs" -> token itself.
        let ext = lang.rsplit('.').next().unwrap_or(lang);
        ss.find_syntax_by_extension(ext)
            .or_else(|| ss.find_syntax_by_token(lang))
            .or_else(|| ss.find_syntax_by_name(lang))
            .or_else(|| ss.find_syntax_by_extension(lang))
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    }

    /// Resolve a single scope (e.g. `markup.heading.2.markdown`) against
    /// the theme exactly as syntect would: the best-matching rule wins, and
    /// rules apply in ascending specificity order. `None` when the theme
    /// has no rule touching `scope`. Ported from akapen: this is how the
    /// page's own constructs borrow the code theme's colors.
    pub fn scope_style(&self, scope: &str) -> Option<Style> {
        use syntect::highlighting::{FontStyle, Highlighter as SynHighlighter};
        use syntect::parsing::Scope;
        let scope = Scope::new(scope).ok()?;
        let highlighter = SynHighlighter::new(&self.theme);
        let m = highlighter.style_mod_for_stack(&[scope]);
        if m.foreground.is_none() && m.background.is_none() && m.font_style.is_none() {
            return None;
        }
        let mut style = Style::default();
        if let Some(fg) = m.foreground {
            style = style.fg(Color::Rgb(fg.r, fg.g, fg.b));
        }
        if let Some(bg) = m.background {
            style = style.bg(Color::Rgb(bg.r, bg.g, bg.b));
        }
        if let Some(fs) = m.font_style {
            if fs.contains(FontStyle::BOLD) {
                style = style.add_modifier(Modifier::BOLD);
            }
            if fs.contains(FontStyle::ITALIC) {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if fs.contains(FontStyle::UNDERLINE) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
        }
        Some(style)
    }

    /// Highlight `content` (a whole code block) with the grammar for `lang`.
    /// Returns one Vec<(text, Style)> per line, newline stripped.
    pub fn highlight(&self, content: &str, lang: &str) -> Vec<Vec<(String, Style)>> {
        let syntax = self.syntax_for(lang);
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(content) {
            let spans = match h.highlight_line(line, syntaxes()) {
                Ok(regions) => regions
                    .into_iter()
                    .map(|(style, text)| {
                        (
                            text.trim_end_matches('\n').to_string(),
                            Style::default().fg(Color::Rgb(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            )),
                        )
                    })
                    .collect(),
                Err(_) => vec![(
                    line.trim_end_matches('\n').to_string(),
                    Style::default().fg(Color::Gray),
                )],
            };
            out.push(spans);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_listed_theme_loads() {
        let names = super::Highlighter::theme_names();
        assert!(names.len() > 5, "{names:?}");
        for n in &names {
            assert!(super::theme_by_name(n).is_some(), "{n}");
        }
        assert!(!super::Highlighter::theme_exists("No Such Theme"));
    }

    const TINY: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>name</key><string>NAME</string>
  <key>settings</key><array>
    <dict><key>settings</key><dict>
      <key>foreground</key><string>#7aa2f7</string>
      <key>background</key><string>#1a1b26</string>
    </dict></dict>
    <dict><key>scope</key><string>markup.heading</string>
      <key>settings</key><dict><key>foreground</key><string>#ff9e64</string></dict></dict>
  </array>
</dict></plist>"##;

    /// A `.tmTheme` in the themes directory is known by its `name`, or by
    /// its file name when it has none; a broken file is reported, not fatal.
    #[test]
    fn user_themes_are_read_by_name_or_file_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tokyo.tmTheme"),
            TINY.replace("NAME", "Tokyo Night"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Nameless.tmTheme"),
            TINY.replace("<key>name</key><string>NAME</string>", ""),
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.tmTheme"), "<plist>nope").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let u = super::UserThemes::load(dir.path());
        let names: Vec<&str> = u.names().collect();
        assert_eq!(
            names,
            vec!["Nameless", "Tokyo Night"],
            "file order, named by content or stem"
        );
        assert_eq!(u.errors.len(), 1, "{:?}", u.errors);
        assert!(u.errors[0].contains("broken.tmTheme"), "{:?}", u.errors);
        let t = u.get("Tokyo Night").unwrap();
        assert_eq!(
            t.settings.foreground.map(|c| (c.r, c.g, c.b)),
            Some((0x7a, 0xa2, 0xf7))
        );
        assert!(u.get("nope").is_none());
        // A missing directory is no themes and no error.
        let none = super::UserThemes::load(&dir.path().join("absent"));
        assert_eq!(none.names().count(), 0);
        assert!(none.errors.is_empty());

        // Several directories: the first to name a theme keeps it.
        let bat = tempfile::tempdir().unwrap();
        std::fs::write(
            bat.path().join("tn.tmTheme"),
            TINY.replace("NAME", "Tokyo Night")
                .replace("#7aa2f7", "#000000"),
        )
        .unwrap();
        std::fs::write(
            bat.path().join("x.tmTheme"),
            TINY.replace("NAME", "Only In Bat"),
        )
        .unwrap();
        let both =
            super::UserThemes::load_dirs(&[dir.path().to_path_buf(), bat.path().to_path_buf()]);
        let names: Vec<&str> = both.names().collect();
        assert_eq!(names, vec!["Nameless", "Tokyo Night", "Only In Bat"]);
        assert_eq!(
            both.get("Tokyo Night")
                .unwrap()
                .settings
                .foreground
                .map(|c| c.r),
            Some(0x7a),
            "cosentty's own copy wins over bat's"
        );
        assert_eq!(both.errors.len(), 1);
    }

    /// bat's directory is found the way bat finds it, so a theme placed
    /// there for bat is found here too.
    #[test]
    fn bat_theme_dirs_follow_bats_precedence() {
        use std::path::PathBuf;
        let env = |k: &str| -> Option<PathBuf> {
            match k {
                "BAT_CONFIG_DIR" => Some("/b".into()),
                "XDG_CONFIG_HOME" => Some("/x".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            }
        };
        let dirs = super::bat_theme_dirs(&env);
        assert_eq!(dirs[0], PathBuf::from("/b/themes"));
        assert_eq!(dirs[1], PathBuf::from("/x/bat/themes"));
        assert_eq!(dirs[2], PathBuf::from("/h/.config/bat/themes"));
        if cfg!(target_os = "macos") {
            assert_eq!(
                dirs[3],
                PathBuf::from("/h/Library/Application Support/bat/themes")
            );
        }
        let none = |_: &str| -> Option<PathBuf> { None };
        assert!(super::bat_theme_dirs(&none).is_empty());
    }

    use super::*;

    #[test]
    fn highlights_rust_into_multiple_spans() {
        let h = Highlighter::new(None, false);
        let out = h.highlight("fn main() {}\n", "hello.rs");
        assert_eq!(out.len(), 1);
        // `fn` keyword tokenizes separately from the rest.
        assert!(out[0].len() > 1);
        let joined: String = out[0].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
    }

    #[test]
    fn scope_style_resolves_theme_colors() {
        // Dracula styles markdown headings with its cyan.
        let h = Highlighter::new(Some("Dracula"), false);
        let style = h
            .scope_style("markup.heading.2.markdown")
            .expect("dracula colors headings");
        assert_eq!(style.fg, Some(Color::Rgb(139, 233, 253)));
        // A scope the theme has no rule for resolves to None.
        assert_eq!(h.scope_style("markup.table.definitely.not.a.scope"), None);
    }

    #[test]
    fn markdown_heading_after_a_list_uses_the_heading_scope_without_a_blank() {
        let h = Highlighter::new(None, false);
        let source = "# テスト\n\
- それは大変な1日でしたね。\n\
 - それほどでもないけどね。\n\
 - ちょっと笑っちゃったかも\n\
## テスト\n\
ワイワイ\n";
        let out = h.highlight(source, "markdown.md");
        assert_eq!(out.len(), 6);
        assert_eq!(
            out[4]
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>(),
            "## テスト"
        );
        assert!(
            out[4].iter().any(|(text, _)| text == "##"),
            "ATX marker must tokenize separately after the list: {:?}",
            out[4]
        );
        let plain = out[5][0].1;
        assert!(
            out[4].iter().any(|(_, style)| *style != plain),
            "H2 must not keep the following paragraph's base style"
        );
    }

    #[test]
    fn unknown_lang_falls_back_to_plain_but_preserves_text() {
        let h = Highlighter::new(None, false);
        let out = h.highlight("just some text\n", "unknownext");
        let joined: String = out[0].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "just some text");
    }

    #[test]
    fn extension_and_token_both_resolve() {
        let h = Highlighter::new(None, false);
        // by extension
        assert!(h.highlight("x = 1\n", "foo.py")[0].len() >= 1);
        // by bare token
        assert!(h.highlight("x = 1\n", "python")[0].len() >= 1);
    }
}
