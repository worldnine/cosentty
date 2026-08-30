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
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Heading colors, BIGGEST first: index 0 = `[**** ]` and up, 3 = `[* ]`
    /// (see `heading_for`). A warm family, deliberately away from the link
    /// blue and the hashtag green so a heading is never read as a link.
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
    pub hashtag: Color,
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
            Style::default().fg(p.heading[i]).add_modifier(heading_modifier(i + 1))
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
                hashtag: Color::Rgb(0x2e, 0x7d, 0x32),
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
                hashtag: Color::Rgb(0x9d, 0xe0, 0x9d),
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
    /// | `[Page]`, URL, `#tag`       | `markup.underline.link.markdown`   |
    /// | a link to an uncreated page | `markup.deleted.markdown`          |
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
            // "Deleted" is the nearest thing a code theme has to "this is
            // not there": it is a FOREGROUND red in every theme that sets
            // it. `invalid` reads better on paper but is a BACKGROUND rule
            // (measured across the embedded themes: Solarized, Dracula,
            // Nord, Monokai and OneHalfDark all paint the background and
            // leave the text near-white), so borrowing its fg would give a
            // link the colour of ordinary prose.
            link_missing: fg("markup.deleted.markdown").unwrap_or(base.link_missing),
            url: link.unwrap_or(base.url),
            hashtag: link.unwrap_or(base.hashtag),
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

/// Telomere age buckets, newest first. The scale is coarse and
/// logarithmic-ish because edit recency matters most at short ranges —
/// "today vs last week" is far more interesting than "one year vs two".
const TELOMERE_BUCKETS: [i64; 5] = [
    3_600,      // < 1h
    86_400,     // < 1d
    604_800,    // < 1w
    2_592_000,  // < 30d
    31_536_000, // < 1y
];

/// Left-edge glyphs, thickest (newest) to thinnest (oldest). All are one
/// terminal column wide, so thickness costs no layout space — and all are
/// LEFT-aligned block elements, so the bar's left edge lines up down the
/// whole gutter. (A box-drawing `│` sits in the cell's centre and made old
/// lines look shifted right.) The two oldest buckets share the hairline
/// `▏` and differ by shade only.
const TELOMERE_GLYPHS: [&str; 6] = ["█", "▊", "▌", "▎", "▏", "▏"];

/// How far an unread line's blue may fade with age (0 = none, 1 = all the
/// way to gray). Capped well short of gray: "unread" must stay legible as
/// a tint on a first visit to a page written years ago.
const UNREAD_FADE_MAX: f32 = 0.45;

/// Bucket index for an age: 0 = brand new … 5 = ancient.
fn telomere_bucket(age_secs: i64) -> usize {
    TELOMERE_BUCKETS
        .iter()
        .position(|&limit| age_secs < limit)
        .unwrap_or(TELOMERE_BUCKETS.len())
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
    let b = telomere_bucket(age_secs);
    let glyph = TELOMERE_GLYPHS[b];
    let oldest = b == TELOMERE_GLYPHS.len() - 1;
    if !unread {
        return (glyph, if oldest { faded_border_color(light) } else { border_color(light) });
    }
    // 0.0 = brand new … UNREAD_FADE_MAX = ancient but still unmistakably blue
    let t = b as f32 / (TELOMERE_GLYPHS.len() - 1) as f32 * UNREAD_FADE_MAX;
    let f = if light { (0x1f, 0x6f, 0xb2) } else { (0x7f, 0xc8, 0xff) };
    let o = match border_color(light) {
        Color::Rgb(r, g, b) => (r as i32, g as i32, b as i32),
        _ => (0x80, 0x80, 0x80),
    };
    let lerp = |a: i32, b: i32| (a as f32 + (b - a) as f32 * t).round() as u8;
    (glyph, Color::Rgb(lerp(f.0, o.0), lerp(f.1, o.1), lerp(f.2, o.2)))
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
    let Color::Rgb(r, g, b) = border_color(light) else { return border_color(light) };
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
pub fn scroll_thumb(content_len: usize, viewport: usize, position: usize) -> Option<(usize, usize)> {
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
    let start = if thumb_max == 0 { 0 } else { (pos * thumb_max / max_pos).min(thumb_max) };
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
pub fn project_header_colors(
    theme: Option<&str>,
    terminal_bg: (u8, u8, u8),
) -> (Color, Color) {
    let Some((r, g, b, alpha)) = theme.and_then(cosense_navbar_rgba) else {
        return (Color::Black, Color::Cyan);
    };
    let blend = |site: u8, terminal: u8| -> u8 {
        let a = alpha as u16;
        ((site as u16 * a + terminal as u16 * (255 - a) + 127) / 255) as u8
    };
    let bg = (blend(r, terminal_bg.0), blend(g, terminal_bg.1), blend(b, terminal_bg.2));
    let fg = if relative_luminance(bg) > 0.179 { Color::Black } else { Color::White };
    (fg, Color::Rgb(bg.0, bg.1, bg.2))
}

/// `(red, green, blue, alpha)` for Cosense's standard `--navbar-bg`.
fn cosense_navbar_rgba(theme: &str) -> Option<(u8, u8, u8, u8)> {
    Some(match theme {
        "default" => (196, 197, 202, 179),
        "default-dark" => (55, 59, 68, 128),
        "default-minimal" => (249, 249, 251, 255),
        "paper-light" => (157, 155, 141, 77),
        "paper-dark" | "paper-dark-dark" => (35, 61, 77, 204),
        "blue" => (94, 122, 224, 204),
        "green" => (91, 165, 111, 204),
        "purple" => (148, 113, 192, 204),
        "orange" => (219, 169, 39, 204),
        "red" => (207, 85, 77, 204),
        "hacker1" => (64, 69, 81, 77),
        "hacker2" => (40, 62, 57, 77),
        "spring" => (2, 167, 137, 128),
        "summer" => (242, 196, 13, 128),
        "autumn" => (72, 61, 56, 128),
        "winter" => (115, 123, 138, 128),
        "tropical" => (35, 161, 138, 128),
        "kyoto" => (71, 37, 65, 204),
        "paris" => (102, 192, 157, 204),
        "newyork" => (18, 34, 59, 77),
        "mred" => (235, 51, 56, 204),
        "lgreen" => (74, 187, 12, 204),
        _ => return None,
    })
}

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f32 {
    let linear = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
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
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let mut out: Box<dyn Write> = if std::io::stdout().is_terminal() {
        Box::new(std::io::stdout())
    } else {
        Box::new(std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?)
    };
    out.write_all(OSC11_QUERY).ok()?;
    out.flush().ok()?;

    let fd = std::io::stdin().as_raw_fd();
    let mut resp = Vec::new();
    let mut buf = [0u8; 64];
    let deadline = std::time::Instant::now() + Duration::from_millis(150);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe {
            libc::poll(&mut pfd, 1, deadline.saturating_duration_since(now).as_millis() as i32)
        };
        if n <= 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n as usize]);
        if response_complete(&resp) {
            break;
        }
    }
    parse_osc11(&resp)
}

#[cfg(not(unix))]
pub fn detect_background() -> Option<(u8, u8, u8)> {
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
    let Some(pos) = find(resp, OSC11_HEADER) else { return false };
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
    let end = rest.iter().position(|&b| b == 0x1b || b == 0x07).unwrap_or(rest.len());
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

/// Apply `shimmer_level` to one span's style: an RGB foreground is mixed
/// toward the terminal background, which is the only way to modulate
/// brightness without inventing a color the theme never chose.
///
/// A named or indexed color has no components to mix, so it falls back to
/// the DIM attribute — coarser, but the same signal. (In the viewer, code
/// rows are syntax-highlighted and therefore RGB.)
pub fn shimmer_style(base: Style, terminal_bg: (u8, u8, u8), level: f32) -> Style {
    if level >= 0.995 {
        return base;
    }
    match base.fg {
        Some(Color::Rgb(r, g, b)) => {
            let mix = |fg: u8, bg: u8| -> u8 {
                (bg as f32 + (fg as f32 - bg as f32) * level).round().clamp(0.0, 255.0) as u8
            };
            base.fg(Color::Rgb(
                mix(r, terminal_bg.0),
                mix(g, terminal_bg.1),
                mix(b, terminal_bg.2),
            ))
        }
        _ if level < 0.75 => base.add_modifier(Modifier::DIM),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::Highlighter;

    #[test]
    fn parse_osc11_forms() {
        assert_eq!(parse_osc11(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\"), Some((0x1e, 0x1e, 0x1e)));
        assert_eq!(parse_osc11(b"\x1b]11;#ffffff\x1b\\"), Some((255, 255, 255)));
        assert_eq!(parse_osc11(b"\x1b]11;rgb:f/0/8\x1b\\"), Some((0xff, 0x00, 0x88)));
    }

    #[test]
    fn lightness_threshold() {
        assert!(is_light_rgb((253, 246, 227)));
        assert!(!is_light_rgb((30, 30, 30)));
    }

    #[test]
    fn telomere_thins_and_fades_with_age() {
        let ages = [60i64, 7_200, 172_800, 1_209_600, 15_552_000, 94_608_000];
        let marks: Vec<&str> = ages.iter().map(|&a| telomere(a, true, false).0).collect();
        // thinning, newest -> oldest; every glyph is a left-aligned block
        // (no centred `│`), the two oldest share the hairline
        assert_eq!(marks, vec!["█", "▊", "▌", "▎", "▏", "▏"]);
        assert!(marks.iter().all(|m| "█▊▌▍▎▏".contains(m)));
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
    fn read_lines_are_gray_but_keep_their_thickness() {
        for light in [false, true] {
            // a minute-old edit you have already seen: thick, but no tint
            let (glyph, color) = telomere(60, false, light);
            assert_eq!(glyph, "█");
            assert_eq!(color, border_color(light));
            // 30d-1y: plain gray hairline; >1y: the same glyph, a step fainter
            assert_eq!(telomere(15_552_000, false, light), ("▏", border_color(light)));
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
        assert_eq!(scroll_offset_at(100, 20, 99), Some(80), "clamped onto the track");
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
        assert_eq!(dark, Color::Rgb(36, 36, 36), "dark terminal: a little lighter");
        assert_eq!(light, Color::Rgb(240, 240, 240), "light terminal: a little darker");
        assert_ne!(dark, light);
        // Never so far that the text on top has to be re-chosen.
        for bg in [(0, 0, 0), (255, 255, 255), (40, 44, 52)] {
            let Color::Rgb(r, g, b) = code_wash(bg) else { panic!("rgb") };
            let delta = (r as i16 - bg.0 as i16).abs();
            assert!(delta <= 12, "{bg:?} moved by {delta}");
            assert_eq!((r as i16 - bg.0 as i16), (g as i16 - bg.1 as i16));
            assert_eq!((g as i16 - bg.1 as i16), (b as i16 - bg.2 as i16));
        }
    }

    #[test]
    fn project_header_uses_site_theme_and_terminal_background() {
        let light = project_header_colors(Some("blue"), (250, 250, 250));
        let dark = project_header_colors(Some("blue"), (24, 24, 24));
        assert_eq!(light, (Color::Black, Color::Rgb(125, 148, 229)));
        assert_eq!(dark, (Color::White, Color::Rgb(80, 102, 184)));
        assert_ne!(light.1, dark.1, "translucent navbar adapts to terminal background");

        // Opaque site themes are independent of the terminal base.
        assert_eq!(
            project_header_colors(Some("default-minimal"), (0, 0, 0)),
            (Color::Black, Color::Rgb(249, 249, 251)),
        );
        // Missing/private-without-SID and future theme ids stay terminal-themed.
        assert_eq!(project_header_colors(None, (24, 24, 24)), (Color::Black, Color::Cyan));
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
        assert_eq!(p.heading_style_for(3), p.heading_style[1], "three stars = level 2");
        assert_eq!(p.title, p.heading[0], "the page title is the top heading");
        assert_eq!(p.link, p.hashtag, "hashtags are links");
        // headings are not the same color as links in this theme
        assert_ne!(p.heading[1], p.link);
        // An uncreated link borrows the theme's "deleted" colour, which is
        // a FOREGROUND (unlike `invalid`, which most themes spell as a
        // background) — and it must not collide with a live link.
        assert_eq!(p.link_missing, Color::Rgb(255, 121, 198), "dracula's deleted pink");
        assert_ne!(p.link_missing, p.link);
        // a theme without markdown rules falls back to the hand palette
        let plain = Highlighter::new(Some("definitely-not-a-theme"), false); // -> default theme
        let q = Palette::from_theme(&plain, false);
        let base = Palette::for_light(false);
        assert_eq!(q.link_missing, base.link_missing, "no rule → the hand-picked red");
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
            assert_eq!(p.heading_style_for(4).add_modifier, Modifier::BOLD | Modifier::UNDERLINED);
            assert_eq!(p.heading_style_for(1).add_modifier, Modifier::ITALIC);
            assert_eq!(p.heading_style_for(1).fg, Some(p.heading[3]));
            for c in p.heading {
                assert_ne!(c, p.link, "a heading must not wear the link color");
                assert_ne!(c, p.url);
                assert_ne!(c, p.hashtag);
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
        assert_eq!(shimmer_level(5, 6, 0.0), 0.55, "far from the band = the floor");
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
        assert_eq!(shimmer_style(base, bg, 1.0).fg, Some(Color::Rgb(200, 100, 50)));
        // Half way to the background is half the distance on every channel.
        assert_eq!(shimmer_style(base, bg, 0.5).fg, Some(Color::Rgb(100, 50, 25)));
        // …and the mix is toward the ACTUAL background, not toward black.
        assert_eq!(
            shimmer_style(base, (100, 100, 100), 0.0).fg,
            Some(Color::Rgb(100, 100, 100))
        );
    }

    #[test]
    fn a_named_color_falls_back_to_the_dim_attribute() {
        let base = Style::default().fg(Color::DarkGray);
        assert_eq!(shimmer_style(base, (0, 0, 0), 1.0), base, "untouched at full");
        assert!(!shimmer_style(base, (0, 0, 0), 0.9).add_modifier.contains(Modifier::DIM));
        assert!(shimmer_style(base, (0, 0, 0), 0.55).add_modifier.contains(Modifier::DIM));
        // A span with no foreground at all is still safe to pass through.
        assert!(shimmer_style(Style::default(), (0, 0, 0), 0.55)
            .add_modifier
            .contains(Modifier::DIM));
    }
}
