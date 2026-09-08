//! Syntax highlighting for code blocks via syntect (default-fancy: pure-Rust
//! regex, no C toolchain). Adapted from akapen's highlight.rs, pared down to
//! what a Scrapbox `code:name.ext` block needs: pick a grammar by the block's
//! language/extension, tokenize the whole block once (cross-line context like
//! strings/comments needs it), and return ratatui spans per line.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
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

fn theme_by_name(name: &str) -> Option<Theme> {
    EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name() == name)
        .map(|t| embedded_themes().get(t).clone())
}

pub struct Highlighter {
    theme: Theme,
}

impl Highlighter {
    /// Every embedded theme name `--theme` accepts, in two-face's order.
    pub fn theme_names() -> Vec<&'static str> {
        EmbeddedLazyThemeSet::theme_names()
            .iter()
            .map(|t| t.as_name())
            .collect()
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
            theme_by_name(name).expect("default theme is embedded")
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
