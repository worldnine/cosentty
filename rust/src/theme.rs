//! Terminal background detection (OSC 11) + a color palette for rendering.
//!
//! Detection half is ported from akapen's theme.rs: at startup we ask the
//! terminal for its background color (`ESC ] 11 ; ?`) and pick light/dark by
//! the answer's perceived lightness. Terminals that don't answer fall back to
//! dark. The `Palette` maps Scrapbox constructs (heading levels, links, etc.)
//! to colors that adapt to light/dark.

use ratatui::style::{Color, Modifier, Style};

#[cfg(unix)]
use std::io::{IsTerminal, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::time::Duration;

/// Colors for rendering Scrapbox constructs, chosen per light/dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Heading colors, BIGGEST first: index 0 = `[**** ]` and up, 3 = `[* ]`
    /// (see `heading_for`). A warm family, deliberately away from the link
    /// blue so a heading is never read as a link.
    pub heading: [Color; 4],
    /// Full heading styles (color + font style), same indexing. From a
    /// theme these are the `markup.heading.N.markdown` rules verbatim; the
    /// hand palette pairs `heading` with `heading_modifier`.
    pub heading_style: [Style; 4],
    pub link: Color,
    /// A link to a page that does not exist yet. Cosense paints these red;
    /// here the colour is borrowed from the theme's own "this is gone"
    /// rule rather than pinned to a red of our choosing (see
    /// `from_theme`), so it stays inside whatever palette the reader runs.
    pub link_missing: Color,
    pub url: Color,
    pub quote_bar: Color,
    pub code_fence: Color,
    pub bullet: Color,
    pub title: Color,
}

/// The structural font style for a heading level (1 = biggest), applied
/// when the theme's heading rule sets no font style of its own — a heading
/// must still read as a heading. akapen's `heading_modifier`.
pub fn heading_modifier(level: usize) -> Modifier {
    match level {
        0 | 1 => Modifier::BOLD | Modifier::UNDERLINED,
        2 => Modifier::BOLD,
        3 => Modifier::BOLD | Modifier::ITALIC,
        _ => Modifier::ITALIC,
    }
}

impl Palette {
    pub fn for_light(light: bool) -> Self {
        let mut p = Self::colors_for_light(light);
        p.heading_style = std::array::from_fn(|i| {
            Style::default()
                .fg(p.heading[i])
                .add_modifier(heading_modifier(i + 1))
        });
        p
    }

    /// The hand-picked colors per background (heading_style left default;
    /// `for_light` fills it in).
    fn colors_for_light(light: bool) -> Self {
        if light {
            Palette {
                heading: [
                    Color::Rgb(0xb3, 0x4a, 0x00), // burnt orange (biggest)
                    Color::Rgb(0x9a, 0x55, 0x10),
                    Color::Rgb(0x80, 0x5c, 0x28),
                    Color::Rgb(0x5e, 0x50, 0x3a), // warm gray-brown (smallest)
                ],
                heading_style: [Style::default(); 4],
                link: Color::Rgb(0x1a, 0x5f, 0x9e),
                link_missing: Color::Rgb(0xc0, 0x39, 0x2b),
                url: Color::Rgb(0x0b, 0x61, 0x74),
                quote_bar: Color::Rgb(0x88, 0x88, 0x88),
                code_fence: Color::Rgb(0x8a, 0x6d, 0x00),
                bullet: Color::Rgb(0x99, 0x99, 0x99),
                title: Color::Rgb(0x20, 0x20, 0x30),
            }
        } else {
            Palette {
                heading: [
                    Color::Rgb(0xff, 0xb0, 0x5c), // vivid amber (biggest)
                    Color::Rgb(0xf2, 0xc0, 0x7b),
                    Color::Rgb(0xe4, 0xcc, 0x9c),
                    Color::Rgb(0xd0, 0xc8, 0xb4), // warm off-white (smallest)
                ],
                heading_style: [Style::default(); 4],
                link: Color::Rgb(0x8a, 0xc6, 0xff),
                link_missing: Color::Rgb(0xf3, 0x8b, 0xa8),
                url: Color::Rgb(0x7f, 0xd0, 0xe0),
                quote_bar: Color::Rgb(0x88, 0x88, 0x88),
                code_fence: Color::Rgb(0xf2, 0xd0, 0x8b),
                bullet: Color::Rgb(0x88, 0x88, 0x88),
                title: Color::Rgb(0xff, 0xff, 0xff),
            }
        }
    }

    /// Resolve the palette from a syntect theme (the `--theme` the code
    /// blocks use), akapen-style, so the page's own constructs and its code
    /// blocks come from one color scheme. Cosense notation maps onto the
    /// theme's MARKDOWN scopes:
    ///
    /// | Cosense                     | markdown scope                     |
    /// |-----------------------------|------------------------------------|
    /// | `[**** ]` … `[* ]` headings | `markup.heading.1..4.markdown`     |
    /// | page title (line 1)         | `markup.heading.1.markdown`        |
    /// | `[Page]`, `#tag`, URL       | `markup.underline.link.markdown`   |
    /// | a link to an uncreated page | `markup.deleted` (diff's "removed")|
    /// | `>` quote bar               | `markup.quote.markdown`            |
    /// | `code:` label, `[x.icon]`   | `markup.raw.inline.markdown`       |
    /// | `•` bullet                  | `comment`                          |
    ///
    /// Headings take the theme's rule VERBATIM — color and font style
    /// (bold / italic / underline), exactly as the theme's author styled
    /// markdown headings; only when the rule sets no font style is akapen's
    /// structural `heading_modifier` added so the heading still reads as
    /// one. Other constructs borrow the color only. A construct the theme
    /// has no rule for keeps the hand-picked `for_light` style, so nothing
    /// goes unstyled.
    pub fn from_theme(h: &crate::highlight::Highlighter, light: bool) -> Self {
        let base = Self::for_light(light);
        let fg = |scope: &str| h.scope_style(scope).and_then(|s| s.fg);
        let heading_style: [Style; 4] = std::array::from_fn(|i| {
            match h.scope_style(&format!("markup.heading.{}.markdown", i + 1)) {
                Some(mut st) => {
                    if st.add_modifier.is_empty() {
                        st = st.add_modifier(heading_modifier(i + 1));
                    }
                    st
                }
                None => base.heading_style[i],
            }
        });
        let heading = std::array::from_fn(|i| heading_style[i].fg.unwrap_or(base.heading[i]));
        let link = fg("markup.underline.link.markdown");
        Palette {
            heading,
            heading_style,
            link: link.unwrap_or(base.link),
            // `markup.deleted` is the diff family's "this line was
            // removed" (its siblings are `markup.inserted` and
            // `markup.changed`), which is the nearest thing a code theme
            // has to "this is not there" — and, being a diff colour, it is
            // a FOREGROUND red in every theme that sets it. `invalid`
            // reads better on paper but is a BACKGROUND rule (measured
            // across the embedded themes: Solarized, Dracula, Nord,
            // Monokai and OneHalfDark all paint the background and leave
            // the text near-white), so borrowing its fg would give a link
            // the colour of ordinary prose. Asked bare, the way themes
            // spell it: unlike the heading rules, no theme qualifies this
            // one with a language.
            link_missing: fg("markup.deleted").unwrap_or(base.link_missing),
            url: link.unwrap_or(base.url),
            quote_bar: fg("markup.quote.markdown").unwrap_or(base.quote_bar),
            code_fence: fg("markup.raw.inline.markdown").unwrap_or(base.code_fence),
            bullet: fg("comment").unwrap_or(base.bullet),
            title: heading[0],
        }
    }

    /// Heading color for a star count (`[* ]`=1): more stars = bigger =
    /// more vivid. Four or more stars share the top color.
    pub fn heading_for(&self, stars: usize) -> Color {
        self.heading[self.heading_index(stars)]
    }

    /// Full heading style for a star count (see `heading_for`).
    pub fn heading_style_for(&self, stars: usize) -> Style {
        self.heading_style[self.heading_index(stars)]
    }

    /// `[* ]` = level 4 (index 3), `[**** ]` and up = level 1 (index 0).
    fn heading_index(&self, stars: usize) -> usize {
        let level = stars.clamp(1, self.heading.len());
        self.heading.len() - level
    }
}

/// Telomere age buckets, newest first — the thresholds between the 9
/// thickness states. Cosense web thins its bar 1px per step over 14 log
/// thresholds (`shokai/テロメア`: 0h…約1年); a terminal cell only has 8
/// left-aligned block elements, so the web scale is compressed onto them —
/// keeping the same-day steps and the long tail (SPEC-telomere-web-parity).
const TELOMERE_BUCKETS: [i64; 8] = [
    3_600,      // < 1h
    21_600,     // < 6h
    86_400,     // < 24h
    259_200,    // < 3d
    604_800,    // < 1w
    2_592_000,  // < 30d
    15_552_000, // < 180d
    31_536_000, // < 1y
];

/// Left-edge glyphs, thickest (newest) to thinnest (oldest). All are one
/// terminal column wide, so thickness costs no layout space — and all are
/// LEFT-aligned block elements, so the bar's left edge lines up down the
/// whole gutter. (A box-drawing `│` sits in the cell's centre and made old
/// lines look shifted right.) These are all 8 of the left-aligned block
/// elements; the 9th bucket (`≥1y`) reuses the hairline `▏` and differs by
/// shade only, so thinness never runs out of steps.
const TELOMERE_GLYPHS: [&str; 8] = ["█", "▉", "▊", "▋", "▌", "▍", "▎", "▏"];

/// How far an unread line's blue may fade with age (0 = none, 1 = all the
/// way to gray). Capped well short of gray: "unread" must stay legible as
/// a tint on a first visit to a page written years ago.
const UNREAD_FADE_MAX: f32 = 0.45;

/// Bucket index for an age: 0 = brand new … 8 = ancient (`≥1y`).
fn telomere_bucket(age_secs: i64) -> usize {
    TELOMERE_BUCKETS
        .iter()
        .position(|&limit| age_secs < limit)
        .unwrap_or(TELOMERE_BUCKETS.len())
}

/// The telomere's colour axis. Thickness stays the age; this picks the
/// colour. `Read`/`Unread` are the original two; the other two are Cosense
/// web states the TUI now shares (SPEC-telomere-web-parity, measured off
/// app.css 2026-09-06):
///
/// - `UpdatedAfterLoad` — the line changed while the page is open. Web
///   paints it a stronger blue than unread (`#6b8cff` vs `#89a3ff`) that
///   does NOT age-fade: by definition the edit is fresh. NOW only — lists
///   and history never wear it. Light themes get a darker indigo of the
///   same saturation step.
/// - `WillDelete` — a history row the NEXT version deletes. Web paints it
///   `#fd7373` red, no strikethrough. Fixed reds, slightly darkened on
///   light so the bar still reads on white.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelomereState {
    Read,
    Unread,
    UpdatedAfterLoad,
    WillDelete,
}

/// A project theme's own telomere colours for the two states that carry
/// information (既読は端末の罫線グレーに任せるので載せない): web's
/// `--telomere-unread` and `--telomere-updated`. Two generations exist:
/// the new themes are blue (`#89a3ff`/`#6b8cff`), the old ones — still
/// selectable — define green (`#7fca8f`/`#47ba5f`), and a theme that
/// defines neither falls back to the css defaults (blue).
/// Generated by `scripts/cosense-theme-vars.py` (app.css, 2026-09-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TelomereTint {
    pub unread: Color,
    pub updated: Color,
}

/// Every theme name Cosense's settings page offers, in the order of its
/// picker (`app.css`, 2026-09-06). The settings screen (`,`) lists these;
/// the colour tables below answer for each of them. Regenerate with
/// `scripts/cosense-theme-vars.py` when the web side changes — do not add
/// names by hand.
pub const COSENSE_THEMES: &[&str] = &[
    "default",
    "default-dark",
    "default-minimal",
    "paper-light",
    "paper-dark",
    "paper-dark-dark",
    "blue",
    "purple",
    "green",
    "orange",
    "red",
    "spring",
    "kyoto",
    "newyork",
    "paris",
    "summer",
    "tropical",
    "autumn",
    "winter",
    "hacker1",
    "hacker2",
    "lgreen",
    "mred",
];

pub fn cosense_telomere_tint(theme: &str) -> Option<TelomereTint> {
    let (ur, ug, ub, pr, pg, pb) = match theme {
        // 旧系(Hacker より下): 緑の未読/ロード後更新
        "hacker2" | "lgreen" | "mred" | "summer" => (127, 202, 143, 71, 186, 95),
        // paper 系は沈んだ青
        "paper-dark" => (134, 168, 187, 92, 150, 182),
        "paper-dark-dark" => (134, 168, 187, 107, 184, 237),
        "paper-light" => (149, 181, 210, 110, 154, 194),
        // 以下は自前で定義しない(新系を含む): css のフォールバックの青
        "autumn" | "blue" | "default" | "default-dark" | "default-minimal" | "green"
        | "hacker1" | "kyoto" | "newyork" | "orange" | "paris" | "purple" | "red" | "spring"
        | "tropical" | "winter" => (137, 163, 255, 107, 140, 255),
        _ => return None,
    };
    Some(TelomereTint {
        unread: Color::Rgb(ur, ug, ub),
        updated: Color::Rgb(pr, pg, pb),
    })
}

/// The telomere mark for a line in a given state: THICKNESS still encodes
/// the edit's age, COLOR says which state the line is in.
pub fn telomere_in(state: TelomereState, age_secs: i64, light: bool) -> (&'static str, Color) {
    telomere_with(state, age_secs, light, None)
}

/// The same, with the project theme's own telomere colours (`tint`).
pub fn telomere_with(
    state: TelomereState,
    age_secs: i64,
    light: bool,
    tint: Option<TelomereTint>,
) -> (&'static str, Color) {
    let b = telomere_bucket(age_secs);
    // 9 buckets, 8 glyphs: the oldest bucket wears the hairline with a
    // faded tint, so thinness never runs out of steps.
    let glyph = TELOMERE_GLYPHS[b.min(TELOMERE_GLYPHS.len() - 1)];
    let color = match state {
        TelomereState::Read => {
            let oldest = b == TELOMERE_BUCKETS.len();
            if oldest {
                faded_border_color(light)
            } else {
                border_color(light)
            }
        }
        TelomereState::Unread => {
            // 0.0 = brand new … UNREAD_FADE_MAX = ancient but still
            // unmistakably unread (the theme's own hue when it has one)
            let t = b as f32 / TELOMERE_BUCKETS.len() as f32 * UNREAD_FADE_MAX;
            let f = match tint {
                Some(t) => match t.unread {
                    Color::Rgb(r, g, b) => (r as i32, g as i32, b as i32),
                    _ => (0x7f, 0xc8, 0xff),
                },
                None => {
                    if light {
                        (0x1f, 0x6f, 0xb2)
                    } else {
                        (0x7f, 0xc8, 0xff)
                    }
                }
            };
            let o = match border_color(light) {
                Color::Rgb(r, g, b) => (r as i32, g as i32, b as i32),
                _ => (0x80, 0x80, 0x80),
            };
            let lerp = |a: i32, b: i32| (a as f32 + (b - a) as f32 * t).round() as u8;
            Color::Rgb(lerp(f.0, o.0), lerp(f.1, o.1), lerp(f.2, o.2))
        }
        TelomereState::UpdatedAfterLoad => match tint {
            Some(t) => t.updated,
            None => {
                if light {
                    Color::Rgb(0x3f, 0x51, 0xd9)
                } else {
                    Color::Rgb(0x6b, 0x8c, 0xff)
                }
            }
        },
        TelomereState::WillDelete => {
            if light {
                Color::Rgb(0xd9, 0x4f, 0x4f)
            } else {
                Color::Rgb(0xfd, 0x73, 0x73)
            }
        }
    };
    (glyph, color)
}

/// The telomere mark for a line (Scrapbox's left-edge bar): THICKNESS
/// encodes the edit's age, COLOR encodes whether YOU have seen it.
///
/// - `unread` (edited after you last opened the page, or a first visit):
///   blue, whatever the age — a years-old line is still news to you. It
///   only mellows slightly with age (`UNREAD_FADE_MAX`), never to gray.
/// - read: the neutral gutter gray; the oldest bucket recedes a further
///   step so it stays distinguishable from `<1y` despite the shared glyph.
///
/// Thickness and tint are driven by the SAME bucket so they never disagree.
pub fn telomere(age_secs: i64, unread: bool, light: bool) -> (&'static str, Color) {
    telomere_in(
        if unread {
            TelomereState::Unread
        } else {
            TelomereState::Read
        },
        age_secs,
        light,
    )
}

/// Neutral rule gray for the gutter (read telomeres) and scrollbar track —
/// akapen's frame color: a calm mid gray, since ANSI bright-black sinks
/// into most dark backgrounds.
pub fn border_color(light: bool) -> Color {
    if light {
        Color::Rgb(180, 180, 190)
    } else {
        Color::Rgb(127, 132, 156)
    }
}

/// The border gray receded ~40% toward the background: the oldest read
/// telomere bucket — "present but quiet".
pub fn faded_border_color(light: bool) -> Color {
    let Color::Rgb(r, g, b) = border_color(light) else {
        return border_color(light);
    };
    let toward = if light { 0xff } else { 0x18 };
    let mix = |c: u8| (c as f32 + (toward as f32 - c as f32) * 0.4).round() as u8;
    Color::Rgb(mix(r), mix(g), mix(b))
}

/// Scrollbar thumb color: a step brighter than the border on dark themes
/// (so the thumb reads against the `│` track), a step darker on light.
pub fn scrollbar_thumb(light: bool) -> Color {
    if light {
        Color::Rgb(105, 105, 115)
    } else {
        Color::Rgb(170, 174, 200)
    }
}

/// The scrollbar thumb over a `viewport`-row track, as `(start row, length)`
/// — or `None` when the content fits (no scrollbar, the border stays clean).
/// `position` is the scroll offset, clamped to the last scrollable row. The
/// thumb length is proportional to the visible fraction (`viewport² /
/// content`), and the start maps the offset range onto the track so the
/// thumb sits at the bottom at max offset. Ported from akapen's theme.rs.
pub fn scroll_thumb(
    content_len: usize,
    viewport: usize,
    position: usize,
) -> Option<(usize, usize)> {
    scroll_thumb_in_track(content_len, viewport, viewport, position)
}

/// The scrollbar thumb when its painted track is shorter than the viewport.
/// This lets a framed view reserve its top-rule row without changing the
/// viewport's actual scroll range.
pub fn scroll_thumb_in_track(
    content_len: usize,
    viewport: usize,
    track_len: usize,
    position: usize,
) -> Option<(usize, usize)> {
    let (max_pos, thumb_len, thumb_max) = scroll_geometry(content_len, viewport, track_len)?;
    let pos = position.min(max_pos);
    let start = if thumb_max == 0 {
        0
    } else {
        (pos * thumb_max / max_pos).min(thumb_max)
    };
    Some((start, thumb_len))
}

/// The scrollbar's geometry when the content overflows the viewport:
/// `(max_pos, thumb_len, thumb_max)` — the last scrollable offset, the
/// thumb length in track rows, and the last track row the thumb can start
/// on. `None` when the content fits.
fn scroll_geometry(
    content_len: usize,
    viewport: usize,
    track_len: usize,
) -> Option<(usize, usize, usize)> {
    if content_len <= viewport || viewport == 0 || track_len == 0 {
        return None;
    }
    let max_pos = content_len - viewport;
    let thumb_len = (viewport * track_len / content_len).clamp(1, track_len);
    let thumb_max = track_len - thumb_len;
    Some((max_pos, thumb_len, thumb_max))
}

/// The scroll offset a track click lands on: the thumb's start moves to
/// the clicked row (clamped so the thumb stays on the track). `None` when
/// the content fits. Ported from akapen.
pub fn scroll_offset_at(content_len: usize, viewport: usize, track_row: usize) -> Option<usize> {
    scroll_offset_at_in_track(content_len, viewport, viewport, track_row)
}

/// Variant of [`scroll_offset_at`] for a painted track whose height differs
/// from the viewport height.
pub fn scroll_offset_at_in_track(
    content_len: usize,
    viewport: usize,
    track_len: usize,
    track_row: usize,
) -> Option<usize> {
    let (max_pos, _, thumb_max) = scroll_geometry(content_len, viewport, track_len)?;
    if thumb_max == 0 {
        return Some(0);
    }
    Some(track_row.min(thumb_max) * max_pos / thumb_max)
}

/// The scroll offset while dragging the thumb: the thumb follows the
/// pointer 1:1 in track rows from the drag start (the pointer may leave
/// the track; the row is clamped). `start_track_row` / `start_offset` are
/// the drag anchor. `None` when the content fits. Ported from akapen.
pub fn scroll_offset_drag(
    content_len: usize,
    viewport: usize,
    start_track_row: usize,
    start_offset: usize,
    track_row: usize,
) -> Option<usize> {
    scroll_offset_drag_in_track(
        content_len,
        viewport,
        viewport,
        start_track_row,
        start_offset,
        track_row,
    )
}

/// Variant of [`scroll_offset_drag`] for a painted track whose height differs
/// from the viewport height.
pub fn scroll_offset_drag_in_track(
    content_len: usize,
    viewport: usize,
    track_len: usize,
    start_track_row: usize,
    start_offset: usize,
    track_row: usize,
) -> Option<usize> {
    let (max_pos, _, thumb_max) = scroll_geometry(content_len, viewport, track_len)?;
    if thumb_max == 0 {
        return Some(0);
    }
    let start_thumb = start_offset.min(max_pos) * thumb_max / max_pos;
    let thumb = (start_thumb as isize + track_row as isize - start_track_row as isize)
        .clamp(0, thumb_max as isize) as usize;
    Some(thumb * max_pos / thumb_max)
}

/// Header foreground/background derived from a Cosense project's selected
/// site theme. Known themes use Cosense's `--navbar-bg`; unknown or
/// unavailable themes retain an ANSI fallback owned by the terminal theme.
///
/// Source for the standard navbar values (checked 2026-08-28):
/// https://scrapbox.io/terfno/Scrapbox_%E3%81%AE_theme_%E3%81%94%E3%81%A8%E3%81%AE_CSS
pub fn project_header_colors(theme: Option<&str>, terminal_bg: (u8, u8, u8)) -> (Color, Color) {
    let Some((r, g, b, alpha)) = theme.and_then(cosense_navbar_rgba) else {
        return (Color::Black, Color::Cyan);
    };
    let blend = |site: u8, terminal: u8| -> u8 {
        let a = alpha as u16;
        ((site as u16 * a + terminal as u16 * (255 - a) + 127) / 255) as u8
    };
    let bg = (
        blend(r, terminal_bg.0),
        blend(g, terminal_bg.1),
        blend(b, terminal_bg.2),
    );
    let fg = if relative_luminance(bg) > 0.179 {
        Color::Black
    } else {
        Color::White
    };
    (fg, Color::Rgb(bg.0, bg.1, bg.2))
}

/// The chrome (header and page frame) while a past snapshot is shown:
/// Cosense's `purple` theme colour, blended onto the terminal background
/// the same way a project's own header colour is. One colour for one
/// meaning — "this is not NOW" — whatever the project's theme.
pub fn history_header_colors(terminal_bg: (u8, u8, u8)) -> (Color, Color) {
    project_header_colors(Some("purple"), terminal_bg)
}

/// `(red, green, blue, alpha)` for Cosense's standard `--navbar-bg`.
/// Values are GENERATED by `scripts/cosense-theme-vars.py` from the
/// shipped `app.css` (measured 2026-09-06) — regenerate and diff when the
/// web side changes, do not hand-tune.
fn cosense_navbar_rgba(theme: &str) -> Option<(u8, u8, u8, u8)> {
    Some(match theme {
        "autumn" => (72, 61, 56, 128),
        "blue" => (94, 122, 224, 204),
        "default-dark" => (55, 59, 68, 128),
        "default-minimal" => (249, 249, 251, 255),
        "green" => (91, 165, 111, 204),
        "hacker1" => (64, 69, 81, 77),
        "hacker2" => (40, 62, 57, 77),
        "kyoto" => (71, 37, 65, 204),
        "lgreen" => (74, 187, 12, 204),
        "mred" => (235, 51, 56, 204),
        "newyork" => (18, 34, 59, 77),
        "orange" => (219, 169, 39, 204),
        "paper-light" => (157, 155, 141, 77),
        "paris" => (102, 192, 157, 204),
        "purple" => (148, 113, 192, 204),
        "red" => (207, 85, 77, 204),
        "spring" => (2, 167, 137, 128),
        "summer" => (242, 196, 13, 128),
        "tropical" => (35, 161, 138, 128),
        "winter" => (115, 123, 138, 128),
        // `default` の `--navbar-bg` は `hsl(225 6% 78% / 70%)`(app.css)
        "default" => (196, 197, 202, 179),
        _ => return None,
    })
}

/// The project theme's own LINK colours: `--page-link-color` and
/// `--empty-page-link-color` (a link to an uncreated page). Where the
/// theme block defines neither, web falls back to the base values in the
/// css itself (`var(--page-link-color, #3d72f5)`); those are what the
/// generated table carries, so every known theme answers.
/// Generated by `scripts/cosense-theme-vars.py` (app.css, 2026-09-06).
pub fn cosense_link_colors(theme: &str) -> Option<(Color, Color)> {
    let (r, g, b, mr, mg, mb) = match theme {
        "blue" => (110, 138, 243, 251, 116, 118),
        "default-dark" => (128, 201, 254, 251, 116, 118),
        "default-minimal" => (110, 138, 243, 255, 82, 82),
        "green" | "orange" | "purple" | "red" => (110, 138, 243, 251, 116, 118),
        "paper-dark" => (47, 152, 182, 251, 116, 118),
        "paper-dark-dark" => (104, 179, 229, 251, 116, 118),
        "paper-light" => (94, 139, 179, 251, 116, 118),
        // 以下は自前で定義しないテーマ: web の基底 #3d72f5 / #fd7373
        "autumn" | "default" | "hacker1" | "hacker2" | "kyoto" | "lgreen" | "mred" | "newyork"
        | "paris" | "spring" | "summer" | "tropical" | "winter" => (61, 114, 245, 253, 115, 115),
        _ => return None,
    };
    Some((Color::Rgb(r, g, b), Color::Rgb(mr, mg, mb)))
}

/// The body palette tinted with the project theme's own link colours: web
/// paints links with `--page-link-color` no matter what the reader's own
/// colour scheme is, so a project whose theme is green should not read its
/// links in the terminal scheme's blue. An unknown theme (settings
/// unreadable, no sid) keeps the terminal scheme untouched — the telomere,
/// caret and selection stay theme-independent as before.
pub fn tinted_page_palette(base: &Palette, theme: Option<&str>) -> Palette {
    let mut p = *base;
    if let Some((link, missing)) = theme.and_then(cosense_link_colors) {
        p.link = link;
        p.link_missing = missing;
    }
    p
}

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f32 {
    let linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// Format an epoch-seconds timestamp as local `YYYY-MM-DD HH:MM`.
/// Uses libc's localtime_r so the user's timezone is respected without
/// pulling in a date crate.
#[cfg(unix)]
pub fn format_local(epoch: i64) -> String {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid time_t and `tm` is a valid out-param.
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(not(unix))]
pub fn format_local(_epoch: i64) -> String {
    String::new()
}

const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";
const OSC11_HEADER: &[u8] = b"\x1b]11;";

/// Ask the terminal for its actual background RGB. `None` means unknown
/// (no tty or no OSC 11 answer). Call after entering raw mode / alt screen.
#[cfg(unix)]
pub fn detect_background() -> Option<(u8, u8, u8)> {
    let resp = query_tty(OSC11_QUERY, Duration::from_millis(150), response_complete)?;
    parse_osc11(&resp)
}

/// Write `query` to the terminal and read its answer from stdin until
/// `complete` says so or `deadline` passes — on THIS thread, with
/// `poll(2)`, so a terminal that never answers costs the deadline and
/// nothing else. (A reader left blocking on stdin in a helper thread would
/// swallow the first keys the user types; that is what this avoids.)
/// `None` when stdin is not a terminal or nothing arrived. Call after
/// entering raw mode.
#[cfg(unix)]
pub fn query_tty(
    query: &[u8],
    deadline: Duration,
    complete: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let mut out: Box<dyn Write> = if std::io::stdout().is_terminal() {
        Box::new(std::io::stdout())
    } else {
        Box::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()?,
        )
    };
    out.write_all(query).ok()?;
    out.flush().ok()?;

    let fd = std::io::stdin().as_raw_fd();
    let mut resp = Vec::new();
    let mut buf = [0u8; 64];
    let deadline = std::time::Instant::now() + deadline;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe {
            libc::poll(
                &mut pfd,
                1,
                deadline.saturating_duration_since(now).as_millis() as i32,
            )
        };
        if n <= 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n as usize]);
        if complete(&resp) {
            break;
        }
    }
    (!resp.is_empty()).then_some(resp)
}

#[cfg(not(unix))]
pub fn detect_background() -> Option<(u8, u8, u8)> {
    None
}

#[cfg(not(unix))]
pub fn query_tty(
    _query: &[u8],
    _deadline: Duration,
    _complete: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    None
}

/// Ask the terminal for its background and classify it as light/dark.
pub fn detect_light() -> Option<bool> {
    detect_background().map(is_light_rgb)
}

/// Classify a known terminal background. Exposed so callers that need both
/// the RGB and light/dark state only issue one OSC 11 query.
pub fn background_is_light(rgb: (u8, u8, u8)) -> bool {
    is_light_rgb(rgb)
}

fn response_complete(resp: &[u8]) -> bool {
    let Some(pos) = find(resp, OSC11_HEADER) else {
        return false;
    };
    let rest = &resp[pos + OSC11_HEADER.len()..];
    rest.windows(2).any(|w| w == b"\x1b\\") || rest.contains(&0x07)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse an OSC 11 answer into RGB. Accepts `rgb:RRRR/GGGG/BBBB` (1-4 digit
/// channels), `rgba:…`, and `#rrggbb`.
fn parse_osc11(resp: &[u8]) -> Option<(u8, u8, u8)> {
    let pos = find(resp, OSC11_HEADER)? + OSC11_HEADER.len();
    let rest = &resp[pos..];
    let end = rest
        .iter()
        .position(|&b| b == 0x1b || b == 0x07)
        .unwrap_or(rest.len());
    let s = std::str::from_utf8(&rest[..end]).ok()?;
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        return Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ));
    }
    let s = s.strip_prefix("rgba:").or_else(|| s.strip_prefix("rgb:"))?;
    Some((
        parse_hex_channel(s.split('/').next()?)?,
        parse_hex_channel(s.split('/').nth(1)?)?,
        parse_hex_channel(s.split('/').nth(2)?)?,
    ))
}

fn parse_hex_channel(s: &str) -> Option<u8> {
    match s.len() {
        1 => u8::from_str_radix(s, 16).ok().map(|v| v * 0x11),
        2 => u8::from_str_radix(s, 16).ok(),
        3 | 4 => u8::from_str_radix(&s[..2], 16).ok(),
        _ => None,
    }
}

fn is_light_rgb((r, g, b): (u8, u8, u8)) -> bool {
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) > 128.0
}

/// A block the web renderer is still working on: a band of brightness runs
/// down its rows so the reader can see that something is happening, without
/// the text moving or being replaced by a spinner they cannot read around.
///
/// `pos` is the row's index inside the block, `len` the block's row count,
/// and `elapsed` seconds since the animation started. The result is a
/// brightness in `0.55..=1.0` — dimmed enough to read as provisional, never
/// so dim the code becomes unreadable.
pub fn shimmer_level(pos: u16, len: u16, elapsed: f32) -> f32 {
    /// Rows per second the band travels. Slow enough to read as a sweep
    /// rather than a flicker at the viewer's ~8–16 frames per second.
    const SPEED: f32 = 6.0;
    /// Rows the band fades out over on each side.
    const HALF_WIDTH: f32 = 2.0;
    /// Rows of quiet after the band leaves the block, so a short block
    /// pulses instead of strobing.
    const TAIL: f32 = 3.0;

    let span = len.max(1) as f32 + TAIL;
    let head = (elapsed.max(0.0) * SPEED).rem_euclid(span);
    let intensity = (1.0 - (head - pos as f32).abs() / HALF_WIDTH).max(0.0);
    0.55 + 0.45 * intensity
}

/// `shimmer_level` turned sideways: the band runs ALONG one row, cell by
/// cell, for a line that is one row tall (a `[URL]` whose picture is still
/// coming). Same floor, same ceiling, so the two read as one signal.
///
/// The speed is NOT shared with the vertical case, and cannot be. Down a
/// block the band travels a fixed number of ROWS per second, which suits
/// three rows and thirty alike — each takes the sweep the time it needs.
/// Along a row there is no such match: the notation is twenty to sixty cells
/// wide, so six cells a second gives a ten-second sweep — longer than the
/// download. The reader sees a still, dim line and calls the animation dead
/// (measured: two or three cells of travel over a whole load, which is not
/// motion). Here the SWEEP IS TIMED and the speed falls out of the width.
pub fn shimmer_level_across(pos: u16, len: u16, elapsed: f32) -> f32 {
    /// Seconds the band takes to cross the row, however wide it is.
    const SWEEP: f32 = 1.2;
    /// Seconds of quiet after it leaves, so a short row pulses rather than
    /// strobes.
    const TAIL: f32 = 0.5;
    /// Cells the band fades out over on each side. Wider than the vertical
    /// band's, because this one travels so much faster.
    const HALF_WIDTH: f32 = 3.0;

    // The band travels its own width beyond both ends, so it is fully clear
    // of the row at each end of the sweep: no half-band parked on the first
    // cell while the line ought to look settled.
    let travel = len.max(1) as f32 + 2.0 * HALF_WIDTH;
    let t = elapsed.max(0.0).rem_euclid(SWEEP + TAIL);
    if t > SWEEP {
        return 0.55;
    }
    let head = (t / SWEEP) * travel - HALF_WIDTH;
    let intensity = (1.0 - (head - pos as f32).abs() / HALF_WIDTH).max(0.0);
    0.55 + 0.45 * intensity
}

/// The terminal's palette as RGB — the sixteen named colours, the 256-colour
/// cube, and the greyscale ramp.
///
/// A reader's own theme may render `DarkGray` as anything, so this is a
/// guess, and it replaces the theme's colour for the cells the band has not
/// reached. That is the trade: the alternative was the DIM attribute, which
/// is faithful to the palette and invisible on a row that is already dark —
/// and some terminals do not implement faint at all. The guess is only ever
/// asked one question, how much brighter than the terminal background, and
/// the band's peak keeps the theme's own colour (full level returns the
/// style untouched).
pub fn palette_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((205, 0, 0)),
        Color::Green => Some((0, 205, 0)),
        Color::Yellow => Some((205, 205, 0)),
        Color::Blue => Some((0, 0, 238)),
        Color::Magenta => Some((205, 0, 205)),
        Color::Cyan => Some((0, 205, 205)),
        Color::Gray => Some((229, 229, 229)),
        Color::DarkGray => Some((127, 127, 127)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((92, 92, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Indexed(n) => {
            if n < 16 {
                // The base sixteen at the palette's own values: index 8 is
                // the dim grey, which is not `Color::DarkGray`'s story.
                return Some(match n {
                    0 => (0, 0, 0),
                    1 => (128, 0, 0),
                    2 => (0, 128, 0),
                    3 => (128, 128, 0),
                    4 => (0, 0, 128),
                    5 => (128, 0, 128),
                    6 => (0, 128, 128),
                    7 => (192, 192, 192),
                    8 => (128, 128, 128),
                    9 => (255, 0, 0),
                    10 => (0, 255, 0),
                    11 => (255, 255, 0),
                    12 => (0, 0, 255),
                    13 => (255, 0, 255),
                    14 => (0, 255, 255),
                    _ => (255, 255, 255),
                });
            }
            if n < 232 {
                let v = n - 16;
                let ch = |i: u8| -> u8 {
                    if i == 0 {
                        0
                    } else {
                        55 + i * 40
                    }
                };
                Some((ch(v / 36), ch((v / 6) % 6), ch(v % 6)))
            } else {
                // 232..=255: the greyscale ramp.
                let g = 8 + (n - 232) * 10;
                Some((g, g, g))
            }
        }
        // `Reset`: the terminal's own default foreground, which is whatever
        // the reader's theme says. Nothing to mix.
        Color::Reset => None,
    }
}

/// A barely-there wash marking a `code:` block as one surface.
///
/// Derived from the TERMINAL background, not from the syntax theme: this
/// band sits directly against rows that are on the terminal background, so
/// a color borrowed from the theme would read as a foreign rectangle
/// dropped on the page (the site header composites onto the terminal
/// background for the same reason). Nudging the real background a few
/// percent also keeps every syntax color the theme chose legible on top.
///
/// Light backgrounds get darker, dark ones get lighter — always by less
/// than a step the eye reads as "a color", so the block reads as a region
/// rather than a box.
pub fn code_wash(terminal_bg: (u8, u8, u8)) -> Color {
    let (r, g, b) = terminal_bg;
    let step = |c: u8| -> u8 {
        if relative_luminance(terminal_bg) > 0.179 {
            c.saturating_sub(10)
        } else {
            c.saturating_add(12)
        }
    };
    Color::Rgb(step(r), step(g), step(b))
}

/// 選択バンドの背景色。カーソル行の帯(ANSI の DarkGray)と同系だと
/// 「どこまでが選択で、いまどこにいるか」が読めなくなるので、端末背景から
/// 寒色(青)寄りに寄せた別の色を作る。code_wash と同じ流儀 — 端末背景を
/// 起点に数%だけ動かす — だが、こちらは「選択」と分かる程度に振れ幅を
/// 大きくし、色相も変える。明るい背景は青みへ沈め、暗い背景は青みへ持ち上げる。
pub fn selection_band(terminal_bg: (u8, u8, u8)) -> Color {
    let (r, g, b) = terminal_bg;
    if relative_luminance(terminal_bg) > 0.179 {
        Color::Rgb(
            r.saturating_sub(40),
            g.saturating_sub(20),
            b.saturating_sub(2),
        )
    } else {
        Color::Rgb(
            r.saturating_add(12),
            g.saturating_add(28),
            b.saturating_add(56),
        )
    }
}

/// 検索語・絞り込み語に一致した部分の下敷き。
///
/// 一覧の行には既に意味を持った印が並んでいる——青い文字=未読、
/// 反転=キャレットと選択、カーソル行の帯。だから一致は**前景色を奪わない**
/// 敷きで語る(青い未読タイトルは青いまま一致を着られる)。selection_band と
/// 同じ流儀で端末背景から作り、色相だけ**暖色側**へ振って寒色の選択と
/// すれ違わないようにする。
pub fn match_wash(terminal_bg: (u8, u8, u8)) -> Color {
    let (r, g, b) = terminal_bg;
    if relative_luminance(terminal_bg) > 0.179 {
        Color::Rgb(
            r.saturating_sub(2),
            g.saturating_sub(22),
            b.saturating_sub(48),
        )
    } else {
        Color::Rgb(
            r.saturating_add(64),
            g.saturating_add(34),
            b.saturating_add(6),
        )
    }
}

/// EDIT モードの下敷き(本文領域全体の背景)。「編集中は紙の色が違う」を
/// 敷きで語る。キャレット行の帯(ANSI の DarkGray)より暗い側に置くのが
/// 約束: 帯が下敷きの上で明るく浮き、キャレット行がスポットライトになる。
/// 暗い端末では背景をわずかに持ち上げたパネルに(それでも帯よりずっと
/// 暗い)、明るい端末では一段沈めたグレーのパネルにする。code_wash より
/// 一歩大きく動かして「領域」ではなく「状態」として読めるようにする。
pub fn edit_backdrop(terminal_bg: (u8, u8, u8)) -> Color {
    let (r, g, b) = terminal_bg;
    if relative_luminance(terminal_bg) > 0.179 {
        Color::Rgb(
            r.saturating_sub(24),
            g.saturating_sub(24),
            b.saturating_sub(22),
        )
    } else {
        Color::Rgb(
            r.saturating_add(16),
            g.saturating_add(16),
            b.saturating_add(20),
        )
    }
}

/// トーストの地色。文字(黄/赤)が浮くよう、端末の背景とは十分に離す。
/// akapen は黒地固定だが、真っ黒な端末では矩形が消えるので、その場合と
/// 明るい端末では持ち上げた黒(濃いグレー)にする。他の帯(選択・EDIT の
/// 下敷き)と一致しない色であることも大事: フェードはこの色のセルだけに
/// かかる。
pub fn toast_bg(terminal_bg: (u8, u8, u8)) -> Color {
    let (r, g, b) = terminal_bg;
    let near_black = r.max(g).max(b) < 24;
    if near_black || relative_luminance(terminal_bg) > 0.179 {
        Color::Rgb(44, 44, 48)
    } else {
        Color::Rgb(0, 0, 0)
    }
}

/// Apply `shimmer_level` to one span's style: the foreground is mixed toward
/// the terminal background, which is the only way to modulate brightness
/// without inventing a color the theme never chose.
///
/// A named or indexed colour has no components of its own, so it goes
/// through `palette_rgb` first. The alternative was the DIM attribute, and
/// DIM turned out not to be a signal at all: a row already drawn in
/// `DarkGray` (a waiting `[URL]`) dimmed by `DIM` is the same colour it was,
/// so the band ran along it and nothing moved.
pub fn shimmer_style(base: Style, terminal_bg: (u8, u8, u8), level: f32) -> Style {
    if level >= 0.995 {
        return base;
    }
    let mix = |fg: (u8, u8, u8)| -> Color {
        let f = |a: u8, b: u8| -> u8 {
            (b as f32 + (a as f32 - b as f32) * level)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color::Rgb(
            f(fg.0, terminal_bg.0),
            f(fg.1, terminal_bg.1),
            f(fg.2, terminal_bg.2),
        )
    };
    match base.fg.and_then(palette_rgb) {
        Some(fg) => base.fg(mix(fg)),
        // No foreground to work on at all: the coarse fallback is still
        // better than silence.
        None if level < 0.75 => base.add_modifier(Modifier::DIM),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_listed_theme_has_colours() {
        for t in super::COSENSE_THEMES {
            assert!(
                super::cosense_link_colors(t).is_some(),
                "link colours for {t}"
            );
            assert!(
                super::cosense_telomere_tint(t).is_some(),
                "telomere for {t}"
            );
        }
        assert!(super::cosense_link_colors("no-such-theme").is_none());
    }

    use super::*;
    use crate::highlight::Highlighter;

    /// トーストの地色は端末の背景から十分離れ、真っ黒な端末では黒に
    /// 溶けない。
    #[test]
    fn toast_bg_stands_off_the_terminal_background() {
        assert_eq!(
            toast_bg((0, 0, 0)),
            Color::Rgb(44, 44, 48),
            "pure black: lifted"
        );
        assert_eq!(
            toast_bg((30, 30, 46)),
            Color::Rgb(0, 0, 0),
            "a dark theme: black"
        );
        assert_eq!(
            toast_bg((250, 250, 250)),
            Color::Rgb(44, 44, 48),
            "light: dark grey, not ink"
        );
    }

    /// 選択バンドは「端末背景」「コードの wash」「カーソル行の DarkGray」の
    /// どれとも見分けがつくこと。明・暗どちらの背景でも。
    #[test]
    fn selection_band_is_its_own_color() {
        for bg in [(250u8, 250u8, 250u8), (24u8, 24u8, 24u8)] {
            let sel = selection_band(bg);
            assert_ne!(sel, Color::Rgb(bg.0, bg.1, bg.2));
            assert_ne!(sel, code_wash(bg));
            assert_ne!(sel, Color::DarkGray);
            // 寒色寄り: 青成分が最も強く残る(明では最も削られない)方向。
            if let Color::Rgb(r, _, b) = sel {
                assert!(b >= r, "selection should lean cool: {sel:?}");
            }
        }
    }

    /// 一致の敷きは、選択・コード・端末背景・カーソル行のどれとも
    /// 見分けがつき、選択とは色相で逆を向いていること。
    #[test]
    fn match_wash_is_warm_and_its_own_color() {
        for bg in [(250u8, 250u8, 250u8), (24u8, 24u8, 24u8)] {
            let m = match_wash(bg);
            assert_ne!(m, Color::Rgb(bg.0, bg.1, bg.2));
            assert_ne!(m, code_wash(bg));
            assert_ne!(m, selection_band(bg));
            assert_ne!(m, edit_backdrop(bg));
            assert_ne!(m, Color::DarkGray);
            // 暖色寄り: 選択(寒色 b >= r)とちょうど逆を向く。
            if let Color::Rgb(r, _, b) = m {
                assert!(r >= b, "match should lean warm: {m:?}");
            }
        }
    }

    #[test]
    fn parse_osc11_forms() {
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"),
            Some((0x1e, 0x1e, 0x1e))
        );
        assert_eq!(parse_osc11(b"\x1b]11;#ffffff\x1b\\"), Some((255, 255, 255)));
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:f/0/8\x1b\\"),
            Some((0xff, 0x00, 0x88))
        );
    }

    #[test]
    fn lightness_threshold() {
        assert!(is_light_rgb((253, 246, 227)));
        assert!(!is_light_rgb((30, 30, 30)));
    }

    #[test]
    fn telomere_thins_and_fades_with_age() {
        // one age per bucket: <1h, <6h, <24h, <3d, <1w, <30d, <180d, <1y, ≥1y
        let ages = [
            60i64,
            7_200,
            50_000,
            200_000,
            400_000,
            900_000,
            10_000_000,
            30_000_000,
            200_000_000,
        ];
        let marks: Vec<&str> = ages.iter().map(|&a| telomere(a, true, false).0).collect();
        // thinning, newest -> oldest; every glyph is a left-aligned block
        // (no centred `│`), the two oldest share the hairline
        assert_eq!(marks, vec!["█", "▉", "▊", "▋", "▌", "▍", "▎", "▏", "▏"]);
        assert!(marks.iter().all(|m| "█▉▊▋▌▍▎▏".contains(m)));
        // unread stays BLUE at every age: a first visit to a years-old page
        // must still show the tint. It mellows a little, never to gray.
        let newest = telomere(60, true, false).1;
        assert_eq!(newest, Color::Rgb(0x7f, 0xc8, 0xff));
        for light in [false, true] {
            for &a in &ages {
                let c = telomere(a, true, light).1;
                assert_ne!(c, border_color(light), "unread {a}s must not be gray");
                assert_ne!(c, faded_border_color(light));
                let Color::Rgb(r, g, b) = c else { panic!() };
                assert!(b > r && b > g, "unread {a}s keeps a blue cast: {c:?}");
            }
            let ancient_unread = telomere(94_608_000, true, light).1;
            assert_ne!(ancient_unread, newest, "still fades a step");
        }
    }

    #[test]
    fn the_four_telomere_states_keep_their_own_colour() {
        // one age per bucket, as in telomere_thins_and_fades_with_age
        let ages = [
            60i64,
            7_200,
            50_000,
            200_000,
            400_000,
            900_000,
            10_000_000,
            30_000_000,
            200_000_000,
        ];
        for light in [false, true] {
            // UpdatedAfterLoad does NOT age-fade: one constant colour per
            // theme, distinct from both the unread blue and the gutter gray.
            let fresh = telomere_in(TelomereState::UpdatedAfterLoad, 60, light).1;
            for &a in &ages {
                assert_eq!(
                    telomere_in(TelomereState::UpdatedAfterLoad, a, light).1,
                    fresh
                );
                assert_ne!(
                    telomere_in(TelomereState::UpdatedAfterLoad, a, light).1,
                    telomere(a, true, light).1,
                    "after-load is not the unread blue at {a}s"
                );
                assert_ne!(
                    telomere_in(TelomereState::UpdatedAfterLoad, a, light).1,
                    telomere(a, false, light).1,
                    "after-load is not the gutter gray at {a}s"
                );
            }
            let Color::Rgb(r, g, b) = fresh else { panic!() };
            assert!(b > r && b > g, "after-load keeps a blue cast: {fresh:?}");

            // WillDelete is one fixed red, never confused with the others.
            let gone = telomere_in(TelomereState::WillDelete, 60, light).1;
            for &a in &ages {
                assert_eq!(telomere_in(TelomereState::WillDelete, a, light).1, gone);
            }
            let Color::Rgb(r, g, b) = gone else { panic!() };
            assert!(r > g && r > b, "will-delete is red: {gone:?}");

            // thickness still follows the age in every state
            assert_eq!(telomere_in(TelomereState::WillDelete, 60, light).0, "█");
            assert_eq!(
                telomere_in(TelomereState::WillDelete, 200_000_000, light).0,
                "▏"
            );
            assert_eq!(
                telomere_in(TelomereState::UpdatedAfterLoad, 7_200, light).0,
                "▉"
            );
        }
    }

    #[test]
    fn read_lines_are_gray_but_keep_their_thickness() {
        for light in [false, true] {
            // a minute-old edit you have already seen: thick, but no tint
            let (glyph, color) = telomere(60, false, light);
            assert_eq!(glyph, "█");
            assert_eq!(color, border_color(light));
            // 30d-1y: plain gray hairline; >1y: the same glyph, a step fainter
            assert_eq!(
                telomere(15_552_000, false, light),
                ("▏", border_color(light))
            );
            let oldest = telomere(94_608_000, false, light);
            assert_eq!(oldest.0, "▏");
            assert_eq!(oldest.1, faded_border_color(light));
            assert_ne!(oldest.1, border_color(light));
        }
    }

    #[test]
    fn scroll_thumb_geometry() {
        // content fits: no bar
        assert_eq!(scroll_thumb(10, 20, 0), None);
        assert_eq!(scroll_thumb(10, 0, 0), None);
        // 100 rows in a 20-row viewport: thumb is 4 rows; top at offset 0,
        // bottom (row 16) at the max offset, clamped past it
        assert_eq!(scroll_thumb(100, 20, 0), Some((0, 4)));
        assert_eq!(scroll_thumb(100, 20, 80), Some((16, 4)));
        assert_eq!(scroll_thumb(100, 20, 999), Some((16, 4)));
        // A framed 20-row viewport may reserve its top-rule cell and paint
        // on a 19-row track without changing the 0..80 scroll range.
        assert_eq!(scroll_thumb_in_track(100, 20, 19, 0), Some((0, 3)));
        assert_eq!(scroll_thumb_in_track(100, 20, 19, 80), Some((16, 3)));
        // a middle offset lands in the middle of the track
        let (start, _) = scroll_thumb(100, 20, 40).unwrap();
        assert!(start > 0 && start < 16);
    }

    #[test]
    fn scrollbar_click_and_drag_map_track_rows_to_offsets() {
        // 100 rows, 20-row viewport: thumb 4 rows, track rows 0..=16
        assert_eq!(scroll_offset_at(100, 20, 0), Some(0));
        assert_eq!(scroll_offset_at(100, 20, 16), Some(80));
        assert_eq!(
            scroll_offset_at(100, 20, 99),
            Some(80),
            "clamped onto the track"
        );
        assert_eq!(scroll_offset_at(10, 20, 3), None);
        // drag: grabbed at row 4 with offset 20 (thumb at 4); pointer to
        // row 8 moves the thumb 4 rows -> offset 40
        assert_eq!(scroll_offset_drag(100, 20, 4, 20, 8), Some(40));
        // dragging above the track pins to the top
        assert_eq!(scroll_offset_drag(100, 20, 4, 20, 0), Some(0));
        assert_eq!(scroll_offset_drag(100, 20, 4, 20, 999), Some(80));
        // The explicit shorter track has the same endpoint offsets.
        assert_eq!(scroll_offset_at_in_track(100, 20, 19, 0), Some(0));
        assert_eq!(scroll_offset_at_in_track(100, 20, 19, 16), Some(80));
        assert_eq!(scroll_offset_drag_in_track(100, 20, 19, 4, 20, 8), Some(40));
    }

    /// The wash has to come from the terminal's own background, or the
    /// block reads as a foreign rectangle dropped onto the page.
    #[test]
    fn the_code_wash_nudges_the_terminal_background_both_ways() {
        let dark = code_wash((24, 24, 24));
        let light = code_wash((250, 250, 250));
        assert_eq!(
            dark,
            Color::Rgb(36, 36, 36),
            "dark terminal: a little lighter"
        );
        assert_eq!(
            light,
            Color::Rgb(240, 240, 240),
            "light terminal: a little darker"
        );
        assert_ne!(dark, light);
        // Never so far that the text on top has to be re-chosen.
        for bg in [(0, 0, 0), (255, 255, 255), (40, 44, 52)] {
            let Color::Rgb(r, g, b) = code_wash(bg) else {
                panic!("rgb")
            };
            let delta = (r as i16 - bg.0 as i16).abs();
            assert!(delta <= 12, "{bg:?} moved by {delta}");
            assert_eq!((r as i16 - bg.0 as i16), (g as i16 - bg.1 as i16));
            assert_eq!((g as i16 - bg.1 as i16), (b as i16 - bg.2 as i16));
        }
    }

    /// Links answer to the project theme, generated from app.css
    /// (2026-09-06): web's `--page-link-color` / `--empty-page-link-color`,
    /// with the css base (#3d72f5 / #fd7373) where the theme defines none.
    #[test]
    fn project_themes_paint_links_their_own_way() {
        let (link, missing) = cosense_link_colors("default").unwrap();
        assert_eq!(link, Color::Rgb(61, 114, 245), "the css base blue");
        assert_eq!(missing, Color::Rgb(253, 115, 115), "the css base red");
        // a dark theme carries its own lighter blue
        let (link, _) = cosense_link_colors("default-dark").unwrap();
        assert_eq!(link, Color::Rgb(0x80, 0xc9, 0xfe));
        // a theme whose block defines both
        let (link, missing) = cosense_link_colors("blue").unwrap();
        assert_eq!(link, Color::Rgb(110, 138, 243));
        assert_eq!(missing, Color::Rgb(251, 116, 118));
        // an unknown theme answers nothing: the terminal scheme stands
        assert_eq!(cosense_link_colors("future-theme"), None);

        // tinting only touches the two link colours
        let base = Palette::for_light(false);
        let tinted = tinted_page_palette(&base, Some("default-dark"));
        assert_eq!(tinted.link, Color::Rgb(0x80, 0xc9, 0xfe));
        assert_eq!(
            tinted.link_missing,
            cosense_link_colors("default-dark").unwrap().1
        );
        assert_eq!(tinted.url, base.url, "everything else keeps the scheme");
        let untouched = tinted_page_palette(&base, Some("future-theme"));
        assert_eq!(untouched.link, base.link);
        assert_eq!(untouched.link_missing, base.link_missing);
    }

    #[test]
    fn project_header_uses_site_theme_and_terminal_background() {
        let light = project_header_colors(Some("blue"), (250, 250, 250));
        let dark = project_header_colors(Some("blue"), (24, 24, 24));
        assert_eq!(light, (Color::Black, Color::Rgb(125, 148, 229)));
        assert_eq!(dark, (Color::White, Color::Rgb(80, 102, 184)));
        assert_ne!(
            light.1, dark.1,
            "translucent navbar adapts to terminal background"
        );

        // Opaque site themes are independent of the terminal base.
        assert_eq!(
            project_header_colors(Some("default-minimal"), (0, 0, 0)),
            (Color::Black, Color::Rgb(249, 249, 251)),
        );
        // Missing/private-without-SID and future theme ids stay terminal-themed.
        assert_eq!(
            project_header_colors(None, (24, 24, 24)),
            (Color::Black, Color::Cyan)
        );
        assert_eq!(
            project_header_colors(Some("future-theme"), (250, 250, 250)),
            (Color::Black, Color::Cyan),
        );
    }

    #[test]
    fn palette_from_theme_borrows_markdown_scope_colors() {
        // Dracula: headings cyan, links (underline.link) also colored.
        let h = crate::highlight::Highlighter::new(Some("Dracula"), false);
        let p = Palette::from_theme(&h, false);
        assert_eq!(p.heading[1], Color::Rgb(139, 233, 253), "H2 = dracula cyan");
        assert_eq!(p.heading_style[1].fg, Some(p.heading[1]));
        // Dracula's heading rule is bold: taken verbatim, no extra modifier
        assert_eq!(p.heading_style[1].add_modifier, Modifier::BOLD);
        assert_eq!(
            p.heading_style_for(3),
            p.heading_style[1],
            "three stars = level 2"
        );
        assert_eq!(p.title, p.heading[0], "the page title is the top heading");

        // headings are not the same color as links in this theme
        assert_ne!(p.heading[1], p.link);
        // An uncreated link borrows the theme's "deleted" colour, which is
        // a FOREGROUND (unlike `invalid`, which most themes spell as a
        // background) — and it must not collide with a live link.
        assert_eq!(
            p.link_missing,
            Color::Rgb(255, 121, 198),
            "dracula's deleted pink"
        );
        assert_ne!(p.link_missing, p.link);
        // a theme without markdown rules falls back to the hand palette
        let plain = Highlighter::new(Some("definitely-not-a-theme"), false); // -> default theme
        let q = Palette::from_theme(&plain, false);
        let base = Palette::for_light(false);
        assert_eq!(
            q.link_missing, base.link_missing,
            "no rule → the hand-picked red"
        );
        // every slot is either the theme's color or the base color — never unset
        for (a, b) in q.heading.iter().zip(base.heading.iter()) {
            assert!(matches!(a, Color::Rgb(..)), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn heading_color_grows_with_stars_and_never_matches_a_link() {
        for light in [false, true] {
            let p = Palette::for_light(light);
            assert_eq!(p.heading_for(1), p.heading[3], "one star = smallest");
            assert_eq!(p.heading_for(4), p.heading[0], "four stars = biggest");
            assert_eq!(p.heading_for(9), p.heading[0], "clamps at four");
            assert_eq!(p.heading_for(0), p.heading[3], "zero is treated as one");
            // the hand palette pairs each level with akapen's structural style
            assert_eq!(
                p.heading_style_for(4).add_modifier,
                Modifier::BOLD | Modifier::UNDERLINED
            );
            assert_eq!(p.heading_style_for(1).add_modifier, Modifier::ITALIC);
            assert_eq!(p.heading_style_for(1).fg, Some(p.heading[3]));
            for c in p.heading {
                assert_ne!(c, p.link, "a heading must not wear the link color");
                assert_ne!(c, p.url);
            }
        }
    }
}

#[cfg(test)]
mod shimmer_tests {
    use super::*;

    #[test]
    fn the_band_sweeps_the_block_and_repeats() {
        // At t=0 the band is on the first row and nowhere near the last.
        assert!(shimmer_level(0, 6, 0.0) > 0.99);
        assert_eq!(
            shimmer_level(5, 6, 0.0),
            0.55,
            "far from the band = the floor"
        );
        // A third of a second later (6 rows/s) it has moved two rows down.
        assert!(shimmer_level(2, 6, 2.0 / 6.0) > 0.99);
        assert!(shimmer_level(0, 6, 2.0 / 6.0) < shimmer_level(0, 6, 0.0));
        // The cycle (block + tail) brings it back to the top.
        let period = (6.0 + 3.0) / 6.0;
        assert!((shimmer_level(0, 6, period) - shimmer_level(0, 6, 0.0)).abs() < 1e-5);
    }

    #[test]
    fn brightness_stays_inside_a_readable_band() {
        for len in [1u16, 2, 7, 40] {
            for step in 0..200 {
                for pos in 0..len {
                    let v = shimmer_level(pos, len, step as f32 / 40.0);
                    assert!((0.55..=1.0).contains(&v), "len={len} pos={pos} -> {v}");
                }
            }
        }
        // A degenerate (empty) block must not divide by zero or spin.
        assert!((0.55..=1.0).contains(&shimmer_level(0, 0, 1.0)));
    }

    #[test]
    fn rgb_is_mixed_toward_the_terminal_background() {
        let bg = (0, 0, 0);
        let base = Style::default().fg(Color::Rgb(200, 100, 50));
        // Full brightness leaves the theme's color exactly alone.
        assert_eq!(
            shimmer_style(base, bg, 1.0).fg,
            Some(Color::Rgb(200, 100, 50))
        );
        // Half way to the background is half the distance on every channel.
        assert_eq!(
            shimmer_style(base, bg, 0.5).fg,
            Some(Color::Rgb(100, 50, 25))
        );
        // …and the mix is toward the ACTUAL background, not toward black.
        assert_eq!(
            shimmer_style(base, (100, 100, 100), 0.0).fg,
            Some(Color::Rgb(100, 100, 100))
        );
    }

    #[test]
    fn a_named_color_is_mixed_through_the_palette() {
        // The band that reads a loading picture's `[URL]` has a DarkGray
        // foreground — a palette entry, not an RGB triple. Mixing it through
        // the palette's grey is the whole reason that band became visible.
        let base = Style::default().fg(Color::DarkGray);
        assert_eq!(
            shimmer_style(base, (0, 0, 0), 1.0),
            base,
            "untouched at full"
        );
        assert_eq!(
            shimmer_style(base, (0, 0, 0), 0.55).fg,
            Some(Color::Rgb(70, 70, 70)),
            "the floor is a fifth of the way to a black terminal, not the same grey"
        );
        // The signal is a colour change, not the DIM attribute: grey dimmed
        // by DIM on a dark terminal is the same grey the band just left.
        assert!(!shimmer_style(base, (0, 0, 0), 0.55)
            .add_modifier
            .contains(Modifier::DIM));
        // A span with no foreground at all still gets the coarse fallback.
        assert!(shimmer_style(Style::default(), (0, 0, 0), 0.55)
            .add_modifier
            .contains(Modifier::DIM));
        assert_eq!(
            shimmer_style(Style::default(), (0, 0, 0), 1.0),
            Style::default()
        );
    }

    #[test]
    fn the_sweep_along_a_row_is_timed_not_counted() {
        // Half a second in, the band is halfway along ANY row — which is the
        // point: at six cells a second a 48-cell `[URL]` never finished a
        // sweep inside the download.
        assert!(
            shimmer_level_across(24, 48, 0.6) > 0.99,
            "mid-sweep on a long row"
        );
        assert!(
            shimmer_level_across(6, 12, 0.6) > 0.99,
            "…and the same moment on a short one"
        );
        assert!(
            shimmer_level_across(47, 48, 0.6) < 0.6,
            "the far end of a long row waits"
        );
        // Fully clear of the row at both ends of the sweep.
        assert_eq!(shimmer_level_across(0, 48, 0.0), 0.55);
        assert_eq!(shimmer_level_across(47, 48, 1.2), 0.55);
        // Quiet after it leaves, then back to the start.
        assert_eq!(shimmer_level_across(0, 48, 1.5), 0.55);
        assert!(
            (shimmer_level_across(6, 48, 1.9) - shimmer_level_across(6, 48, 0.2)).abs() < 1e-4,
            "the sweep repeats: one and two thirds of a second is the same phase as a fifth"
        );
        // Same floor and ceiling as the vertical band, so the two are one
        // signal, and a degenerate row cannot divide by zero.
        for len in [0u16, 1, 2, 60] {
            for step in 0..120 {
                for pos in 0..len.max(1) {
                    let v = shimmer_level_across(pos, len, step as f32 / 40.0);
                    assert!((0.55..=1.0).contains(&v), "len={len} pos={pos} -> {v}");
                }
            }
        }
    }

    #[test]
    fn the_palette_answer_is_monotone_in_lightness() {
        // Only the ordering matters to `shimmer_style`: a darker name must
        // not resolve brighter than a lighter one, or the band inverts.
        let lum = |c: Color| {
            palette_rgb(c).map(|(r, g, b)| 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32)
        };
        for (dark, light) in [
            (Color::Black, Color::DarkGray),
            (Color::DarkGray, Color::Gray),
            (Color::Gray, Color::White),
        ] {
            assert!(
                lum(dark).unwrap() < lum(light).unwrap(),
                "{dark:?} vs {light:?}"
            );
        }
        assert_eq!(
            palette_rgb(Color::Reset),
            None,
            "the terminal's own default: nothing to mix"
        );
        assert_eq!(
            palette_rgb(Color::Indexed(232)),
            Some((8, 8, 8)),
            "the grey ramp"
        );
        assert_eq!(
            palette_rgb(Color::Indexed(196)),
            Some((255, 0, 0)),
            "the 6x6x6 cube"
        );
        assert_eq!(palette_rgb(Color::Rgb(1, 2, 3)), Some((1, 2, 3)));
    }

    /// The telomere follows the project theme: new themes are blue, the
    /// old ones (Hacker and below) are green — web's two generations.
    #[test]
    fn the_telomere_tint_follows_the_project_theme() {
        // a NEW theme (even named "green") is the css fallback blue
        let new = cosense_telomere_tint("green").unwrap();
        assert_eq!(new.unread, Color::Rgb(137, 163, 255));
        assert_eq!(new.updated, Color::Rgb(107, 140, 255));
        // an OLD theme defines its own green
        let old = cosense_telomere_tint("mred").unwrap();
        assert_eq!(old.unread, Color::Rgb(127, 202, 143));
        assert_eq!(old.updated, Color::Rgb(71, 186, 95));
        assert_ne!(old, new);
        // an unknown theme answers nothing
        assert_eq!(cosense_telomere_tint("future-theme"), None);

        // the tint reaches the mark: an unread line wears the theme's
        // hue (fresh), and updated-after-load the theme's second colour
        let (g1, c1) = telomere_with(TelomereState::Unread, 60, false, Some(old));
        assert_eq!(g1, "█");
        assert_eq!(c1, old.unread);
        let (_, c2) = telomere_with(TelomereState::UpdatedAfterLoad, 60, false, Some(old));
        assert_eq!(c2, old.updated);
        // without a tint the original blues stand (theme unknown)
        let (_, c3) = telomere_in(TelomereState::Unread, 60, false);
        assert_eq!(c3, Color::Rgb(0x7f, 0xc8, 0xff));
    }
}
