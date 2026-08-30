//! Scrapbox notation -> renderable blocks. Ported from render.ts.
//! Scrapbox is NOT markdown: structure is per-line, indent = nesting depth.
//!
//! Output is a Vec<Block>: either a styled text Line, a blank line, an image
//! reference (resolved to a real image later), or a table (pre-formatted rows).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone)]
pub enum Block {
    /// A rendered text line with styled spans.
    Text(Line<'static>),
    /// A preserved blank line.
    Blank,
    /// An image to be fetched and rendered inline (gyazo or scrapbox files).
    ///
    /// `indent` is the display column the picture starts at, and `item` is
    /// whether the picture IS the list item (a line that holds nothing
    /// else) or hangs under one. Cosense hangs pictures off bullets, and a
    /// picture that is its own item wears the bullet; a picture under a
    /// line of text is that line's continuation and wears none.
    ///
    Image { url: String, indent: usize, item: bool },
    /// A line that mixes text and pictures (`本文 [画像]`, `[画像]本文`,
    /// `[A] と [B]`). The viewer lays the parts out left to right with each
    /// picture sitting ON the text line — its bottom edge level with the
    /// words — which is how a browser draws an inline image. Kept as a
    /// SEQUENCE rather than special cases so all three read the same way.
    ///
    /// `item`: the line is an indented list item, so it wears a bullet at
    /// its top-left like every other item at that level.
    Inline { indent: usize, item: bool, parts: Vec<InlinePart> },
    /// A structured table, laid out against the pane width at draw time.
    Table(crate::table::Table),
    /// A code block Cosense draws as a picture in the browser (today: only
    /// Mermaid). The viewer shows the rendered artifact when it has one and
    /// falls back to `rows` — the ordinary highlighted code block — until
    /// then, or forever if no browser is available. See `crate::webrender`.
    WebRender {
        kind: crate::webrender::WebKind,
        /// The block's own source (the mermaid text), for the render key.
        code: String,
        /// The plain code-block presentation: `(source line, styled line)`
        /// for the `code:` header and every continuation line.
        rows: Vec<(usize, Line<'static>)>,
        /// Source line of the block's LAST content line. Cosense hangs the
        /// preview element off THAT line's id, not the header's (verified
        /// live — see HANDOFF.md).
        last_src: usize,
    },
}

/// One part of a mixed text-and-picture line (see [`Block::Inline`]).
#[derive(Clone, Debug)]
pub enum InlinePart {
    Text(Line<'static>),
    Image(String),
}

/// Where a `code:` block sits in the source/// Where a `code:` block sits in the source: its header line and the
/// indent depth (in raw whitespace characters) of its header.
///
/// The renderer decides what a code block *is* while it walks the page;
/// the editor needs the same answer for one line at a time ("is the caret
/// in code, and how deep does a new line there have to be indented?").
/// Both must agree, or Enter puts a line where the renderer will not read
/// it as code — which is exactly how a block ends up impossible to type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CodeSpan {
    /// Source index of the `code:` header line.
    pub header: usize,
    /// Raw whitespace width of the header's own indent.
    pub header_indent: usize,
}

impl CodeSpan {
    /// The indent a line must carry to belong to this block — one step
    /// deeper than the header, which is what `strip_leading_ws` removes
    /// again when the block is drawn.
    pub fn body_indent(&self) -> String {
        " ".repeat(self.header_indent + 1)
    }

    /// Leading whitespace CHARACTERS the renderer strips from a body line
    /// before drawing it.
    pub fn strip_chars(&self) -> usize {
        self.header_indent + 1
    }

    /// Display columns the renderer puts back in their place: the header's
    /// own nesting plus the block's two-cell code gutter. An editor that
    /// shows the raw line instead lands the caret one cell to the left of
    /// every other line in the block.
    pub fn gutter_cols(&self) -> usize {
        bullet_indent_width(self.header_indent) + 2
    }
}

/// Is `i` inside a `code:` block (header line included)?
///
/// Mirrors the scan in [`render_lines_with`]: continuation lines are the
/// ones indented deeper than the header, blank lines continue a block only
/// when more code follows, and the header itself counts as being "in" its
/// own block.
pub fn code_span_at(lines: &[&str], i: usize) -> Option<CodeSpan> {
    let mut k = 0;
    while k < lines.len() {
        let (_, header_indent, body) = indent_info(lines[k]);
        if !body.starts_with("code:") {
            k += 1;
            continue;
        }
        // Forward scan, exactly as the renderer's block collector: deeper
        // lines and blanks continue the block, then trailing blanks are
        // handed back to the page.
        let mut end = k + 1; // exclusive
        let mut j = k + 1;
        while j < lines.len() {
            let (_, raw_len, _) = indent_info(lines[j]);
            if raw_len > header_indent {
                j += 1;
                end = j; // a real code row: the block reaches at least here
            } else if lines[j].trim().is_empty() {
                j += 1; // may turn out to be trailing — `end` stays put
            } else {
                break;
            }
        }
        if i >= k && i < end {
            return Some(CodeSpan { header: k, header_indent });
        }
        if i < k {
            return None; // headers only come later now
        }
        k = j.max(k + 1);
    }
    None
}

/// Which source lines belong to a `code:` block, in one pass over the
/// page. The viewer paints those rows with a wash, so it needs the whole
/// map per frame rather than one lookup at a time.
pub fn code_line_flags(lines: &[&str]) -> Vec<bool> {
    let mut flags = vec![false; lines.len()];
    let mut k = 0;
    while k < lines.len() {
        let (_, header_indent, body) = indent_info(lines[k]);
        if !body.starts_with("code:") {
            k += 1;
            continue;
        }
        let mut end = k + 1;
        let mut j = k + 1;
        while j < lines.len() {
            let (_, raw_len, _) = indent_info(lines[j]);
            if raw_len > header_indent {
                j += 1;
                end = j;
            } else if lines[j].trim().is_empty() {
                j += 1;
            } else {
                break;
            }
        }
        for f in flags.iter_mut().take(end).skip(k) {
            *f = true;
        }
        k = j.max(k + 1);
    }
    flags
}

/// Is `i` inside a `table:` block (its header line included)?
///
/// Same shape as [`code_span_at`], and for the same reason: the editor has
/// to know what kind of line it is standing on. In a table the indent is
/// structure and a TAB is a cell separator, neither of which means what it
/// means in an outline.
pub fn table_span_at(lines: &[&str], i: usize) -> Option<CodeSpan> {
    let mut k = 0;
    while k < lines.len() {
        let (_, header_indent, body) = indent_info(lines[k]);
        if !body.starts_with("table:") {
            k += 1;
            continue;
        }
        // The renderer's own scan: rows are the deeper-indented lines, and
        // a blank line ends the table.
        let mut j = k + 1;
        while j < lines.len() {
            let (_, raw_len, _) = indent_info(lines[j]);
            if raw_len <= header_indent {
                break;
            }
            j += 1;
        }
        if i >= k && i < j {
            return Some(CodeSpan { header: k, header_indent });
        }
        if i < k {
            return None;
        }
        k = j.max(k + 1);
    }
    None
}

/// Does a `code:` block's language name mark a Mermaid diagram?/// Does a `code:` block's language name mark a Mermaid diagram?
///
/// Cosense accepts `code:mmd`, `code:mermaid` and `code:<filename>.mmd`
/// (documented on scrapbox.io/help-jp/Mermaid). Matching is
/// case-insensitive, and a bare extension-less name never counts.
pub fn mermaid_lang(lang: &str) -> bool {
    let name = lang.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if name == "mmd" || name == "mermaid" {
        return true;
    }
    matches!(name.rsplit_once('.'), Some((_, ext)) if ext == "mmd" || ext == "mermaid")
}

/// Links and images discovered while rendering, for navigation and prefetch.
#[derive(Debug, Default, Clone)]
pub struct Extracted {
    pub links: Vec<String>,
    pub images: Vec<String>,
}

pub struct RenderOutput {
    pub blocks: Vec<Block>,
    /// Source line index each block was rendered from (parallel to `blocks`).
    /// A table maps to its `table:` line; code continuation lines map to
    /// their own source line. This is the exact block↔source attribution the
    /// viewer uses to place the cursor, anchor comments, and insert cards.
    pub srcs: Vec<usize>,
    pub extracted: Extracted,
}

use crate::theme::Palette;

fn style_link(pal: &Palette) -> Style {
    Style::default().fg(pal.link).add_modifier(Modifier::UNDERLINED)
}
fn style_url(pal: &Palette) -> Style {
    Style::default().fg(pal.url).add_modifier(Modifier::UNDERLINED)
}

/// Service subdomains of gyazo.com that are NOT Teams org names.
const GYAZO_SERVICE_SUBS: [&str; 6] = ["i", "t", "thumb", "www", "api", "upload"];

/// Extract *canonical gyazo permalinks* from a raw string.
///
/// This layer only identifies images; it does NOT decide how to download them.
/// It emits an unambiguous permalink that the image-fetch layer (Phase 3) parses
/// back into (org, id) and resolves via the Gyazo API (Bearer token) with an
/// og:image fallback — the same proven approach as the gyazo skill.
///
///   - `<org>.gyazo.com/<id>` (Teams)    -> https://<org>.gyazo.com/<id>
///   - `gyazo.com/<id>`       (personal) -> https://gyazo.com/<id>
///   - `i.gyazo.com/<id>.png` (direct)   -> https://gyazo.com/<id>
///
/// Note: the API's own `permalink_url` is wrong for Teams (returns gyazo.com and
/// 404s), so a Teams permalink must keep its org subdomain.
/// The canonical gyazo PAGE for any gyazo URL form (`i.gyazo.com/<id>.png`,
/// `gyazo.com/<id>`, `<org>.gyazo.com/<id>`), or `None` for a non-gyazo URL.
/// The viewer opens this in the browser when jumping from an inline image.
pub fn gyazo_permalink(url: &str) -> Option<String> {
    let mut v = Vec::new();
    find_gyazo(url, &mut v);
    v.into_iter().next()
}

fn find_gyazo(s: &str, out: &mut Vec<String>) {
    let lower = s.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find("gyazo.com/") {
        let host_kw = search_from + rel; // index of "gyazo.com/"
        let id_start = host_kw + "gyazo.com/".len();
        let hex: String = s[id_start..]
            .chars()
            .take(32)
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if hex.len() == 32 {
            // Subdomain immediately preceding "gyazo.com" (after ://, '[', or space).
            let prefix = &s[..host_kw];
            let host_start = prefix
                .rfind(|c: char| c == '/' || c == '[' || c.is_whitespace())
                .map(|i| i + 1)
                .unwrap_or(0);
            let sub = s[host_start..host_kw].trim_end_matches('.');
            let url = if sub.is_empty() || GYAZO_SERVICE_SUBS.contains(&sub) {
                format!("https://gyazo.com/{hex}")
            } else {
                format!("https://{sub}.gyazo.com/{hex}")
            };
            if !out.contains(&url) {
                out.push(url);
            }
        }
        search_from = id_start;
        if search_from >= s.len() {
            break;
        }
    }
}

/// Indent depth. One leading whitespace char per level, but a *run* of
/// half-width spaces counts as one level (tabs and full-width spaces each +1).
/// Returns (level, raw_len, rest).
/// Strip up to `n` leading whitespace chars (tab/space/full-width space),
/// preserving any deeper indentation. Used to dedent a code block by its base
/// indent while keeping the code's own relative indentation intact.
fn strip_leading_ws(raw: &str, n: usize) -> String {
    let mut removed = 0usize;
    let mut idx = 0usize;
    for (i, c) in raw.char_indices() {
        if removed >= n {
            idx = i;
            return raw[idx..].to_string();
        }
        if c == ' ' || c == '\t' || c == '\u{3000}' {
            removed += 1;
            idx = i + c.len_utf8();
        } else {
            idx = i;
            return raw[idx..].to_string();
        }
    }
    raw[idx..].to_string()
}

/// Indent of a line: EVERY leading whitespace char (space, tab, 　) is
/// one level — Scrapbox counts characters, it does not collapse runs.
/// Bullets land wherever the whitespace puts them, logical or not.
fn indent_info(raw: &str) -> (usize, usize, &str) {
    let mut raw_len = 0usize;
    let mut level = 0usize;
    let mut end_byte = 0usize;
    for (idx, c) in raw.char_indices() {
        if c == ' ' || c == '\t' || c == '\u{3000}' {
            level += 1;
            raw_len += 1;
            end_byte = idx + c.len_utf8();
        } else {
            end_byte = idx;
            break;
        }
    }
    if level == 0 {
        end_byte = 0;
    }
    (level, raw_len, &raw[end_byte..])
}

/// Decorate inline content into styled spans. Also collects links + gyazo.
fn decorate_inline(s: &str, links: &mut Vec<String>, images: &mut Vec<String>, pal: &Palette) -> Vec<Span<'static>> {
    find_gyazo(s, images);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = s;

    // Walk the string, handling `code`, [..] brackets, bare urls, #tags.
    while !rest.is_empty() {
        // inline code
        if let Some(start) = rest.find('`') {
            if let Some(end_rel) = rest[start + 1..].find('`') {
                let end = start + 1 + end_rel;
                push_plain(&mut spans, &rest[..start], links, pal);
                let code = &rest[start + 1..end];
                spans.push(Span::styled(
                    format!(" {code} "),
                    Style::default().bg(Color::DarkGray).fg(Color::White),
                ));
                rest = &rest[end + 1..];
                continue;
            }
        }
        // bracket form
        if let Some(start) = rest.find('[') {
            // `[[text]]` (bold) has to be matched as a PAIR: taking the
            // first `]` would cut it at `[text`, and the notation would
            // read as a link to a page whose name starts with a bracket.
            // Balanced, so notation can nest: `[[[page]]]` is a bold link
            // and `[* [page]]` is a decorated one. Closing at the first
            // `]` cut both one bracket short, and the link inside was lost.
            if let Some(close) = matching_bracket(rest, start) {
                let doubled =
                    rest[start + 1..].starts_with('[') && rest[..close].ends_with(']');
                push_plain(&mut spans, &rest[..start], links, pal);
                if doubled {
                    decorate_bold(&rest[start + 2..close - 1], &mut spans, links, pal);
                } else {
                    decorate_bracket(&rest[start + 1..close], &mut spans, links, images, pal);
                }
                rest = &rest[close + 1..];
                continue;
            }
        }
        // no more special tokens
        push_plain(&mut spans, rest, links, pal);
        break;
    }
    spans
}

/// Handle bare text: detect URLs and #hashtags, produce spans.
fn push_plain(spans: &mut Vec<Span<'static>>, text: &str, links: &mut Vec<String>, pal: &Palette) {
    if text.is_empty() {
        return;
    }
    let mut rest = text;
    while !rest.is_empty() {
        // URL
        if let Some(pos) = find_url(rest) {
            let (url, after) = take_url(&rest[pos..]);
            if pos > 0 {
                push_tags(spans, &rest[..pos], links, pal);
            }
            spans.push(Span::styled(url.to_string(), style_url(pal)));
            rest = after;
            continue;
        }
        push_tags(spans, rest, links, pal);
        break;
    }
}

/// Handle #hashtags within a plain segment.
fn push_tags(spans: &mut Vec<Span<'static>>, text: &str, links: &mut Vec<String>, pal: &Palette) {
    let mut rest = text;
    loop {
        if let Some(pos) = rest.find('#') {
            // must be at start or preceded by whitespace
            let ok = pos == 0
                || rest[..pos].chars().next_back().map(|c| c.is_whitespace()).unwrap_or(false);
            let tag: String = rest[pos + 1..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if ok && !tag.is_empty() {
                if pos > 0 {
                    spans.push(Span::raw(rest[..pos].to_string()));
                }
                links.push(tag.clone());
                spans.push(Span::styled(
                    format!("#{tag}"),
                    Style::default().fg(pal.hashtag).add_modifier(Modifier::UNDERLINED),
                ));
                let consumed = pos + 1 + tag.len();
                rest = &rest[consumed..];
                continue;
            }
        }
        if !rest.is_empty() {
            spans.push(Span::raw(rest.to_string()));
        }
        break;
    }
}

fn find_url(s: &str) -> Option<usize> {
    let h = s.find("http://");
    let hs = s.find("https://");
    match (h, hs) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

fn take_url(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || *c == ']')
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// `[[text]]` — Cosense's other way of writing `[* text]`. Empty double
/// brackets are text, like empty single ones.
fn decorate_bold(
    inner: &str,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<String>,
    pal: &Palette,
) {
    if inner.trim().is_empty() {
        spans.push(Span::raw(format!("[[{inner}]]")));
        return;
    }
    let mut images = Vec::new();
    // The inside is ordinary notation: `[[[link]]]` is a bold link (still
    // followable), and a bold URL is still a URL. `[[x]]` is Cosense's
    // one-star heading, so it wears the same style `[* x]` does.
    let heading = star_style(1, pal);
    for sp in decorate_inline(inner, links, &mut images, pal) {
        spans.push(Span::styled(sp.content, heading.patch(sp.style)));
    }
}

/// The look of Cosense's star notation: `[* x]`, `[** x]`, `[[x]]`.
///
/// The star count is a LEVEL, and each level borrows the theme's markdown
/// heading style so the page looks like the rest of the terminal. But in
/// Cosense the notation is **emphasis** first — `[* x]` is how you bold a
/// word — so bold is the floor every level stands on. Without it, one
/// star landed on the smallest heading, whose structural style is italic,
/// and the most common emphasis in the wiki came out un-bolded.
fn star_style(stars: usize, pal: &Palette) -> Style {
    // Italic is NOT borrowed. In Cosense italic is its own flag (`[/ x]`),
    // so italics appearing without one is a false signal — and the theme's
    // smallest markdown heading is italic, which is exactly the level one
    // star lands on. Colour (and the top level's underline) carry the
    // level; bold carries the emphasis.
    pal.heading_style_for(stars)
        .remove_modifier(Modifier::ITALIC)
        .add_modifier(Modifier::BOLD)
}

/// Byte index of the `]` that closes the `[` at `open`, counting nesting./// Byte index of the `]` that closes the `[` at `open`, counting nesting.
///
/// Taking the FIRST `]` cuts `[* [改善案]]` at `[改善案`, and the link
/// inside a decoration is lost — it renders as text with a stray bracket
/// and cannot be followed.
fn matching_bracket(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s[open..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Decorate a `[...]` bracket's inner content./// Decorate a `[...]` bracket's inner content.
fn decorate_bracket(
    inner: &str,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
) {
    // `[]` and `[   ]` are not notation — an empty link is nothing to
    // link to, so Cosense shows the brackets as the text they are. The
    // viewer used to swallow them, and a line written about `[]` lost the
    // very thing it was about.
    if inner.trim().is_empty() {
        spans.push(Span::raw(format!("[{inner}]")));
        return;
    }
    // decoration: [* text] [** text] [*/ text] [- strike] [_ underline]
    if let Some(rest) = strip_deco_prefix(inner) {
        let (flags, body) = rest;
        // Stars are HEADING LEVELS, not just bold, and they mean the same
        // thing mid-line as they do on a line of their own: the number of
        // stars picks the level and the level wears the theme's markdown
        // heading style. Rendering them as plain bold threw the level away
        // — `[* x]` and `[*** x]` looked identical.
        let stars = flags.chars().filter(|c| *c == '*').count();
        let mut style =
            if stars > 0 { star_style(stars, pal) } else { Style::default() };
        if flags.contains('/') {
            style = style.add_modifier(Modifier::ITALIC);
        }
        // The inside is ordinary notation, so a decorated LINK is still a
        // link: `[[[改善案]]]` and `[* [改善案]]` both have to stay
        // followable, not just look bold.
        if body.contains('[') && body.contains(']') {
            for sp in decorate_inline(body, links, images, pal) {
                spans.push(Span::styled(sp.content, style.patch(sp.style)));
            }
            return;
        }
        if flags.contains('-') {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if flags.contains('_') {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(body.to_string(), style));
        return;
    }
    // icon: [name.icon]
    if inner.ends_with(".icon") || inner.contains(".icon") {
        let name = inner.split(".icon").next().unwrap_or(inner);
        spans.push(Span::styled(format!("@{name}"), Style::default().fg(pal.code_fence)));
        return;
    }
    // external link with optional title
    if let Some(pos) = find_url(inner) {
        let (url, _) = take_url(&inner[pos..]);
        let title = inner.replace(url, "");
        let title = title.trim();
        if url.contains("gyazo.com") {
            find_gyazo(url, images);
            let label = if title.is_empty() { "[gyazo]" } else { title };
            spans.push(Span::styled(
                format!("🖼 {label}"),
                Style::default().fg(Color::Magenta).add_modifier(Modifier::UNDERLINED),
            ));
        } else if is_scrapbox_file_url(url) {
            // Uploaded file: a paper-clip so it reads as "download", not
            // "open in browser". Enter/f in the viewer saves and opens it.
            let label = if title.is_empty() { file_name_of_url(url) } else { title };
            spans.push(Span::styled(format!("📎 {label}"), style_url(pal)));
        } else if looks_like_image_url(url) {
            // The picture is drawn on its own row; the text row only needs
            // to say that it is there. Spelling out the whole URL made a
            // one-line note wrap over three rows of link.
            let label = if title.is_empty() { file_name_of_url(url) } else { title };
            spans.push(Span::styled(format!("\u{1f5bc} {label}"), style_url(pal)));
        } else {
            let label = if title.is_empty() { url } else { title };
            spans.push(Span::styled(label.to_string(), style_url(pal)));
        }
        return;
    }
    // internal page link
    links.push(inner.to_string());
    spans.push(Span::styled(inner.to_string(), style_link(pal)));
}

/// If inner starts with a run of */-_ followed by space, return (flags, body).
fn strip_deco_prefix(inner: &str) -> Option<(&str, &str)> {
    let flag_end = inner
        .char_indices()
        .find(|(_, c)| !matches!(c, '*' | '/' | '_' | '-'))
        .map(|(i, _)| i)?;
    if flag_end == 0 {
        return None;
    }
    let rest = &inner[flag_end..];
    if let Some(stripped) = rest.strip_prefix(' ') {
        Some((&inner[..flag_end], stripped))
    } else {
        None
    }
}

/// Two display columns per nesting step. Level 1 is flush left, level 2
/// starts at column 2, level 3 at column 4, and so on. Source data still
/// stores one leading whitespace character per logical level.
/// Where the TEXT of a line at this nesting level starts: the indent plus
/// the `• ` marker an indented line wears.
pub fn text_column(level: usize) -> usize {
    if level == 0 {
        0
    } else {
        bullet_indent_width(level) + 2
    }
}

pub fn bullet_indent_width(level: usize) -> usize {
    level.saturating_sub(1) * 2
}

fn indent_str(level: usize) -> String {
    " ".repeat(bullet_indent_width(level))
}

/// File extensions rendered as images (what the `image` crate can decode).
const IMAGE_EXTS: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".tiff",
];

/// Whether a URL points at a decodable image: by file extension (query and
/// fragment ignored) or by being a Scrapbox upload.
pub fn looks_like_image_url(url: &str) -> bool {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    if IMAGE_EXTS.iter().any(|e| path.ends_with(e)) {
        return true;
    }
    // Scrapbox uploads: extension-less forms are images; anything with a
    // non-image extension (.pdf, .docx, …) is a downloadable file.
    if let Some(rest) = path.split("/files/").nth(1) {
        return !rest.contains('.');
    }
    false
}

/// A Scrapbox uploaded file that is NOT an image (`…/files/<id>.pdf`).
pub fn is_scrapbox_file_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.contains("/files/")
        && !looks_like_image_url(url)
}

/// The file name a Scrapbox upload URL ends in (`abc.pdf`).
pub fn file_name_of_url(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path)
}

/// Detect a standalone image line and return the image URL.
///
/// Handles gyazo (any host form), Scrapbox uploads, and **any** http(s) URL
/// with an image extension. Scrapbox's linked-image form `[href imageUrl]`
/// is supported by scanning the tokens for the image side.
/// The image a single `[...]` embeds, if it is one. Handles the gyazo
/// permalink forms, a bracketed image URL, and the linked-image form
/// (`[href imageUrl]` / `[imageUrl href]`).
fn image_in_bracket(inner: &str) -> Option<String> {
    let inner = inner.trim();
    let mut g = Vec::new();
    find_gyazo(inner, &mut g);
    if let Some(u) = g.into_iter().next() {
        if inner.contains("gyazo.com") && inner.split_whitespace().count() <= 2 {
            return Some(u);
        }
    }
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.len() <= 2 {
        if let Some(u) = tokens.iter().find(|t| looks_like_image_url(t)) {
            return Some((*u).to_string());
        }
    }
    None
}

/// Every image a line embeds, in the order they are written.
///
/// **Only the bracketed form draws a picture.** A bare URL is a link, at
/// the start of a line as anywhere else — the same rule cosense web
/// follows. The viewer used to turn any line STARTING with an image URL
/// into a picture, which silently ate the rest of the line.
fn line_images(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        // Inline code quotes its contents: `[url]` inside backticks is a
        // line ABOUT the notation, not an image. Skip those spans, the
        // same way `decorate_inline` does when it draws them.
        let tick = rest.find('`');
        let open = rest.find('[');
        match (tick, open) {
            (Some(t), Some(o)) if t < o => {
                let after = &rest[t + 1..];
                match after.find('`') {
                    Some(close) => {
                        rest = &after[close + 1..];
                        continue;
                    }
                    None => break, // an unclosed backtick quotes the rest
                }
            }
            (_, Some(o)) => {
                let Some(end) = matching_bracket(rest, o) else { break };
                if let Some(u) = image_in_bracket(&rest[o + 1..end]) {
                    out.push(u);
                }
                rest = &rest[end + 1..];
            }
            _ => break,
        }
    }
    out
}

/// Split a line into its inline parts: runs of text and the pictures
/// between them, in the order written. Text runs keep every other piece of
/// notation (links, decoration, code) intact.
fn inline_parts(
    body: &str,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
) -> Vec<InlinePart> {
    let mut parts: Vec<InlinePart> = Vec::new();
    let mut text_from = 0usize;
    let mut i = 0usize;
    // Ranges of plain text, collected first so the borrow checker is not
    // asked to share `images` between a closure and the loop.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut order: Vec<Result<String, usize>> = Vec::new(); // Ok(url) | Err(run index)
    while i < body.len() {
        let Some(rel) = body[i..].find('[') else { break };
        let open = i + rel;
        // Quoted notation is text about a picture, not a picture.
        if body[..open].matches('`').count() % 2 == 1 {
            i = open + 1;
            continue;
        }
        let Some(close) = matching_bracket(body, open) else { break };
        if let Some(url) = image_in_bracket(&body[open + 1..close]) {
            if open > text_from {
                runs.push((text_from, open));
                order.push(Err(runs.len() - 1));
            }
            order.push(Ok(url));
            text_from = close + 1;
        }
        i = close + 1;
    }
    if body.len() > text_from {
        runs.push((text_from, body.len()));
        order.push(Err(runs.len() - 1));
    }
    for item in order {
        match item {
            Ok(url) => {
                images.push(url.clone());
                parts.push(InlinePart::Image(url));
            }
            Err(idx) => {
                let (from, to) = runs[idx];
                let spans = decorate_inline(&body[from..to], links, images, pal);
                if !spans.is_empty() {
                    parts.push(InlinePart::Text(Line::from(spans)));
                }
            }
        }
    }
    parts
}

/// A line that opens with a picture and continues in text/// The image this line is ENTIRELY made of/// The image this line is ENTIRELY made of — one bracket and nothing
/// else, which is the case that becomes a picture on its own row.
fn standalone_image(body: &str) -> Option<String> {
    let inner = body.trim();
    if inner.contains('`') {
        return None; // quoted notation is text about notation
    }
    let inner = inner.strip_prefix('[')?.strip_suffix(']')?;
    if inner.contains('[') || inner.contains(']') {
        return None;
    }
    image_in_bracket(inner)
}


/// Render with defaults (dark palette, no syntax highlighting).
pub fn render_lines(lines: &[String]) -> RenderOutput {
    let pal = Palette::for_light(false);
    render_lines_with(lines, None, &pal)
}

/// Render, optionally syntax-highlighting code blocks with `hl`, using `pal`
/// for construct colors (heading levels, links, etc.).
pub fn render_lines_with(
    lines: &[String],
    hl: Option<&crate::highlight::Highlighter>,
    pal: &Palette,
) -> RenderOutput {
    let mut out: Vec<Block> = Vec::new();
    let mut srcs: Vec<usize> = Vec::new();
    let mut ex = Extracted::default();
    let mut i = 0;

    // Every block records the source line it came from (see RenderOutput.srcs).
    macro_rules! emit {
        ($block:expr) => {{
            out.push($block);
            srcs.push(i);
        }};
    }

    while i < lines.len() {
        let raw = &lines[i];
        let (level, raw_len, body) = indent_info(raw);
        let indent = indent_str(level);

        // blank line (preserve). A WHITESPACE-ONLY line is not blank: it
        // is an empty bullet at its indent depth (cosense shows the dot).
        // Code blocks are consumed whole below, so no in-code guard is
        // needed here anymore.
        if raw.is_empty() {
            emit!(Block::Blank);
            i += 1;
            continue;
        }
        if body.is_empty() {
            let mut spans: Vec<Span<'static>> = vec![Span::raw(indent.clone())];
            spans.push(Span::styled("•".to_string(), Style::default().fg(pal.bullet)));
            emit!(Block::Text(Line::from(spans)));
            i += 1;
            continue;
        }

        // table block
        if let Some(name) = body.strip_prefix("table:") {
            let name = name.trim().to_string();
            // (source line, raw cells) — one Scrapbox line per table row.
            let mut rows: Vec<(usize, Vec<String>)> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let (_, rl, rrest) = indent_info(&lines[j]);
                // Deeper indent = a row, even when the row is empty: that
                // is the row you are typing into, and cosense shows it as
                // an empty cell. Only a line that is not indented into the
                // table ends it.
                if rl <= raw_len {
                    break;
                }
                rows.push((j, rrest.split('\t').map(|c| c.trim_end().to_string()).collect()));
                j += 1;
            }
            // decorate cells to styled spans (links stay navigable via extraction)
            let styled_rows: Vec<(usize, Vec<Vec<Span<'static>>>)> = rows
                .iter()
                .map(|(src, r)| {
                    (
                        *src,
                        r.iter()
                            .map(|c| decorate_inline(c, &mut ex.links, &mut ex.images, pal))
                            .collect(),
                    )
                })
                .collect();
            // Blank-only leading indent is dropped: Scrapbox tables render
            // flush; nesting is rare and the width-adaptive layout owns
            // horizontal budget. Keep the indent as a note for the future.
            let _ = &indent;
            emit!(Block::Table(crate::table::Table { name, name_src: i, rows: styled_rows }));
            i = j;
            continue;
        }

        // code block: `code:name.ext` header + indented continuation lines.
        if let Some(rest) = body.strip_prefix("code:") {
            let lang = rest.trim().to_string();
            let code_indent = raw_len;
            // Cosense serves a code block as a file, so the header line
            // is something to follow: `Enter` saves it. The arrow marks
            // the row as actionable, like a link row.
            let header = Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(format!("code:{lang}"), Style::default().fg(pal.code_fence)),
                Span::styled("  ↓", Style::default().fg(Color::DarkGray)),
            ]);
            // A Mermaid block is collected whole and handed to the web
            // renderer; everything else emits the header row right away.
            let webbable = mermaid_lang(&lang);
            if !webbable {
                emit!(Block::Text(header.clone()));
            }
            // collect continuation lines (deeper indent, or blank)
            let mut j = i + 1;
            let mut raws: Vec<usize> = Vec::new(); // source indices
            let mut bodies: Vec<String> = Vec::new();
            while j < lines.len() {
                let (_, rl, _) = indent_info(&lines[j]);
                if rl > code_indent || lines[j].trim().is_empty() {
                    // strip the block's base indent (code_indent+1 ws chars),
                    // preserving deeper (relative) indentation.
                    let stripped = strip_leading_ws(&lines[j], code_indent + 1);
                    bodies.push(stripped);
                    raws.push(j);
                    j += 1;
                } else {
                    break;
                }
            }
            // Blank lines FOLLOWING the block get absorbed by the `||empty`
            // rule; hand every trailing blank back so each renders as its
            // own Blank row (blank lines between code lines stay inside the
            // block). Popping just one used to fold "code + N blanks" into
            // "code + 1 blank": the rest became empty code rows, and the
            // highlighter's `lines()` then dropped them outright.
            while bodies.last().map(|b| b.trim().is_empty()).unwrap_or(false) {
                bodies.pop();
                raws.pop();
                j -= 1;
            }
            // The code rows, styled exactly as they always were.
            let mut code_rows: Vec<(usize, Line<'static>)> = Vec::new();
            match hl {
                Some(h) => {
                    let content = bodies.join("\n");
                    let highlighted = h.highlight(&content, &lang);
                    for (k, spans) in highlighted.into_iter().enumerate() {
                        let src = raws.get(k).copied().unwrap_or(i);
                        let mut line_spans: Vec<Span<'static>> = vec![Span::raw(format!("{indent}  "))];
                        for (text, style) in spans {
                            line_spans.push(Span::styled(text, style));
                        }
                        code_rows.push((src, Line::from(line_spans)));
                    }
                }
                None => {
                    for (k, b) in bodies.iter().enumerate() {
                        let src = raws.get(k).copied().unwrap_or(i);
                        code_rows.push((
                            src,
                            Line::from(vec![
                                Span::raw(format!("{indent}  ")),
                                Span::styled(b.clone(), Style::default().fg(Color::DarkGray)),
                            ]),
                        ));
                    }
                }
            }
            if webbable {
                // Cosense attaches the preview to the block's LAST content
                // line. With no content there is nothing to draw, so such a
                // block stays an ordinary (empty) code block.
                match raws.last().copied() {
                    Some(last_src) => {
                        let mut rows = vec![(i, header)];
                        rows.extend(code_rows);
                        emit!(Block::WebRender {
                            kind: crate::webrender::WebKind::Mermaid,
                            code: bodies.join("\n"),
                            rows,
                            last_src,
                        });
                    }
                    None => emit!(Block::Text(header)),
                }
            } else {
                for (src, line) in code_rows {
                    out.push(Block::Text(line));
                    srcs.push(src);
                }
            }
            i = j;
            continue;
        }

        // image line. A line that is ONE bracket and nothing else becomes
        // the picture. A line that also carries text (or a second image)
        // keeps its text row and hangs the pictures under it: in a
        // terminal the text cannot flow around an image, and dropping
        // either of them is worse than stacking them.
        if let Some(url) = standalone_image(body) {
            ex.images.push(url.clone());
            emit!(Block::Image { url, indent: text_column(level), item: level > 0 });
            i += 1;
            continue;
        }
        // A line that mixes text and pictures: one sequence, laid out like
        // a browser lays out inline images. Asked FIRST whether there is a
        // picture at all, because splitting the line also collects its
        // links — doing that speculatively would count them twice.
        if !line_images(body).is_empty() {
            let parts = inline_parts(body, &mut ex.links, &mut ex.images, pal);
            emit!(Block::Inline { indent: text_column(level), item: level > 0, parts });
            i += 1;
            continue;
        }

        // command line ($ / %)
        if let Some(rest) = body.strip_prefix("$ ").or_else(|| body.strip_prefix("% ")) {
            let sigil = &body[..1];
            emit!(Block::Text(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(format!("{sigil} "), Style::default().fg(Color::Green)),
                Span::styled(rest.to_string(), Style::default().fg(Color::White)),
            ])));
            i += 1;
            continue;
        }

        // title (first line): the top heading's style, as the theme has it.
        if i == 0 {
            emit!(Block::Text(Line::from(Span::styled(
                body.to_string(),
                pal.heading_style_for(4),
            ))));
            i += 1;
            continue;
        }

        // quote
        if let Some(q) = body.strip_prefix('>') {
            let mut spans = vec![
                Span::raw(indent.clone()),
                Span::styled("┃ ".to_string(), Style::default().fg(pal.quote_bar)),
            ];
            for s in decorate_inline(q.trim(), &mut ex.links, &mut ex.images, pal) {
                let styled = s.style.add_modifier(Modifier::ITALIC);
                spans.push(Span::styled(s.content.into_owned(), styled));
            }
            emit!(Block::Text(Line::from(spans)));
            i += 1;
            continue;
        }

        // Section heading: `[* ...]` may ALSO be indented in Cosense.
        // Markdown would make these competing block types; Cosense composes
        // them. Keep the positional bullet in the bullet style and apply the
        // theme's heading style only to the heading text.
        if let Some(heading) = parse_heading(body, pal, &mut ex.links, &mut ex.images) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if level > 0 {
                spans.push(Span::raw(indent.clone()));
                spans.push(Span::styled("• ".to_string(), Style::default().fg(pal.bullet)));
            }
            spans.extend(heading);
            emit!(Block::Text(Line::from(spans)));
            i += 1;
            continue;
        }

        // bullet / plain
        let content = decorate_inline(body, &mut ex.links, &mut ex.images, pal);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if level > 0 {
            spans.push(Span::raw(indent.clone()));
            spans.push(Span::styled("• ".to_string(), Style::default().fg(pal.bullet)));
        }
        spans.extend(content);
        emit!(Block::Text(Line::from(spans)));
        i += 1;
    }

    RenderOutput { blocks: out, srcs, extracted: ex }
}

/// If the whole body is `[* ...]`/`[** ...]`/…, render it as a heading.
///
/// Scrapbox: more stars = bigger heading. `[**** ]` and up is level 1,
/// `[* ]` level 4, and each level wears the theme's own markdown heading
/// style (see `Palette::from_theme`) — the same look as a markdown file in
/// akapen under that theme.
fn parse_heading(
    body: &str,
    pal: &Palette,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
) -> Option<Vec<Span<'static>>> {
    let close = matching_bracket(body.trim_end(), body.find('[')?)?;
    if close + 1 != body.trim_end().len() {
        return None; // something follows the bracket: not a heading LINE
    }
    let inner = &body.trim_end()[body.find('[')? + 1..close];
    let stars = inner.char_indices().find(|(_, c)| *c != '*').map(|(i, _)| i)?;
    if stars == 0 {
        return None;
    }
    let text = inner[stars..].strip_prefix(' ')?;
    // A heading is still ordinary notation inside: `[* [page]]` has to
    // stay a followable link, not become the letters of one.
    let style = star_style(stars, pal);
    Some(
        decorate_inline(text, links, images, pal)
            .into_iter()
            .map(|sp| Span::styled(sp.content, style.patch(sp.style)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(b: &Block) -> String {
        match b {
            Block::Blank => "[BLANK]".into(),
            Block::Image { url, .. } => format!("[IMAGE {url}]"),
            Block::Inline { parts, .. } => parts
                .iter()
                .map(|p| match p {
                    InlinePart::Image(u) => format!("[IMAGE {u}]"),
                    InlinePart::Text(l) => {
                        l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            Block::Text(l) => l.spans.iter().map(|s| s.content.as_ref()).collect(),
            Block::Table(_) => "[TABLE]".into(),
            Block::WebRender { rows, .. } => rows
                .iter()
                .map(|(_, l)| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn gyazo_canonical_permalink() {
        let id = "319b11dda7ce73cb3c1505774cfa0931";
        // Teams: keep org subdomain
        let mut v = Vec::new();
        find_gyazo(&format!("[https://acme-inc.gyazo.com/{id}]"), &mut v);
        assert_eq!(v, vec![format!("https://acme-inc.gyazo.com/{id}")]);
        // personal + i.gyazo direct both normalize to gyazo.com/<id>
        let mut v2 = Vec::new();
        find_gyazo(&format!("https://gyazo.com/{id}"), &mut v2);
        find_gyazo(&format!("https://i.gyazo.com/{id}.png"), &mut v2);
        assert_eq!(v2, vec![format!("https://gyazo.com/{id}")]); // dedup, both canonical
    }

    #[test]
    fn non_gyazo_image_urls_are_detected() {
        // A picture needs the BRACKETED form — the rule cosense web
        // follows. A bare URL is a link, wherever it sits on the line.
        assert_eq!(standalone_image("https://example.com/a.png"), None);
        assert_eq!(
            standalone_image("[https://example.com/photo.JPG]"),
            Some("https://example.com/photo.JPG".into())
        );
        // linked image: [href imageUrl] -> the image side wins
        assert_eq!(
            standalone_image("[https://example.com/page https://cdn.example.com/x.webp]"),
            Some("https://cdn.example.com/x.webp".into())
        );
        // query strings don't defeat extension detection
        assert_eq!(
            standalone_image("[https://example.com/a.png?w=800]"),
            Some("https://example.com/a.png?w=800".into())
        );
        // non-image links are not images
        assert_eq!(standalone_image("[https://example.com/page.html]"), None);
        assert_eq!(standalone_image("ただの本文"), None);
        // …and a line that carries more than the picture is not "standalone",
        // though the picture is still found by `line_images`.
        assert_eq!(standalone_image("[https://example.com/a.png] こんな感じ"), None);
        assert_eq!(
            line_images("[https://example.com/a.png] こんな感じ"),
            vec!["https://example.com/a.png".to_string()],
        );
    }

    #[test]
    fn standalone_gyazo_becomes_image_block() {
        let id = "319b11dda7ce73cb3c1505774cfa0931";
        let lines = vec![
            "タイトル".to_string(),
            format!("[https://acme-inc.gyazo.com/{id}]"),
        ];
        let out = render_lines(&lines);
        match &out.blocks[1] {
            Block::Image { url, .. } => assert_eq!(url, &format!("https://acme-inc.gyazo.com/{id}")),
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn headings_take_the_palettes_level_style_by_star_count() {
        let pal = Palette::for_light(false);
        let lines: Vec<String> = ["title", "[* one]", "[** two]", "[*** three]", "[**** four]", "[***** five]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_lines(&lines);
        let got: Vec<String> = out.blocks.iter().map(plain).collect();
        assert_eq!(&got[1..], ["one", "two", "three", "four", "five"], "no lead-in marks");
        let style_of = |b: &Block| match b {
            Block::Text(l) => l.spans[0].style,
            _ => unreachable!(),
        };
        assert_eq!(style_of(&out.blocks[0]), pal.heading_style_for(4), "title = top level");
        // The star forms carry the level AND bold (see `star_style`).
        assert_eq!(style_of(&out.blocks[1]), star_style(1, &pal));
        assert_eq!(style_of(&out.blocks[2]), star_style(2, &pal));
        assert_eq!(style_of(&out.blocks[3]), star_style(3, &pal));
        assert_eq!(style_of(&out.blocks[4]), star_style(4, &pal));
        assert_eq!(style_of(&out.blocks[5]), star_style(4, &pal), "four+ stars share the top level");
        for b in &out.blocks[1..] {
            assert!(style_of(b).add_modifier.contains(Modifier::BOLD), "emphasis is the floor");
        }
        assert_ne!(style_of(&out.blocks[1]), style_of(&out.blocks[4]));
    }

    #[test]
    fn indented_headings_keep_both_bullet_and_theme_heading_styles() {
        let pal = Palette::for_light(false);
        let out = render_lines_with(
            &["title".into(), " [* one]".into(), "   [*** three]".into()],
            None,
            &pal,
        );
        assert_eq!(plain(&out.blocks[1]), "• one");
        assert_eq!(plain(&out.blocks[2]), "    • three");

        for (block, stars) in [(&out.blocks[1], 1), (&out.blocks[2], 3)] {
            let Block::Text(line) = block else { panic!("expected text") };
            assert_eq!(line.spans[1].content, "• ");
            assert_eq!(line.spans[1].style.fg, Some(pal.bullet));
            assert_eq!(line.spans[2].style, star_style(stars, &pal));
        }
    }

    #[test]
    fn uploaded_files_are_links_not_images() {
        let pdf = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
        assert!(!looks_like_image_url(pdf));
        assert!(is_scrapbox_file_url(pdf));
        assert_eq!(file_name_of_url(pdf), "6a8e7e5d714feb3f195319dd.pdf");
        // extension-less and image-extension uploads stay images
        assert!(looks_like_image_url("https://scrapbox.io/files/6a8e7e5d714feb3f195319dd"));
        assert!(looks_like_image_url("https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.png"));
        assert!(!is_scrapbox_file_url("https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.png"));
        // a titled file link renders as a paper-clip label, never an image block
        let out = render_lines(&["t".into(), format!("[260826ニセコ.pdf {pdf}]")]);
        assert_eq!(plain(&out.blocks[1]), "📎 260826ニセコ.pdf");
        assert!(out.extracted.images.is_empty());
        // an untitled one shows the file name
        let out = render_lines(&["t".into(), format!("[{pdf}]")]);
        assert_eq!(plain(&out.blocks[1]), "📎 6a8e7e5d714feb3f195319dd.pdf");
    }

    #[test]
    fn mermaid_is_recognised_by_language_and_by_filename() {
        // The three forms scrapbox.io/help-jp/Mermaid documents.
        for yes in ["mmd", "mermaid", "MMD", " Mermaid ", "flow.mmd", "図.mermaid", "a.b.mmd"] {
            assert!(mermaid_lang(yes), "{yes} should be a mermaid block");
        }
        for no in ["", "js", "python", "mmdx", "mermaidjs", "readme.md", "mmd.txt", "diagram"] {
            assert!(!mermaid_lang(no), "{no} should stay a plain code block");
        }
    }

    #[test]
    fn a_mermaid_block_becomes_one_web_render_keyed_on_its_last_line() {
        let lines: Vec<String> = [
            "title",
            "code:mmd",
            " flowchart LR",
            "   A-->B",
            "",
            "after",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = render_lines(&lines);
        let web: Vec<_> = out
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::WebRender { code, rows, last_src, .. } => Some((code, rows, *last_src)),
                _ => None,
            })
            .collect();
        assert_eq!(web.len(), 1);
        let (code, rows, last_src) = web[0];
        // Cosense hangs the preview off the block's LAST content line (3),
        // not the `code:` header (1) — verified live on help-jp/Mermaid.
        assert_eq!(last_src, 3);
        assert_eq!(code, "flowchart LR\n  A-->B");
        // The fallback presentation is the whole code block, header first.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 1);
        assert!(plain(&Block::Text(rows[0].1.clone())).contains("code:mmd"));
        assert_eq!(rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(), vec![1, 2, 3]);
        // The trailing blank still belongs to the page, not the block.
        assert!(matches!(out.blocks.last(), Some(Block::Text(_))));
    }

    #[test]
    fn several_mermaid_blocks_stay_separate_and_other_languages_are_untouched() {
        let lines: Vec<String> = [
            "title",
            "code:one.mmd",
            " graph TD",
            "code:js",
            " let a = 1",
            "code:mermaid",
            " pie",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = render_lines(&lines);
        let last_srcs: Vec<usize> = out
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::WebRender { last_src, .. } => Some(*last_src),
                _ => None,
            })
            .collect();
        assert_eq!(last_srcs, vec![2, 6], "one web block per mermaid block");
        // The JS block is still ordinary text rows.
        assert!(out
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text(_)) && plain(b).contains("let a = 1")));
    }

    #[test]
    fn an_empty_mermaid_block_stays_a_plain_code_header() {
        // No content line means no line id to hang a preview off.
        let lines: Vec<String> = ["title", "code:mmd"].iter().map(|s| s.to_string()).collect();
        let out = render_lines(&lines);
        assert!(!out.blocks.iter().any(|b| matches!(b, Block::WebRender { .. })));
        assert!(out.blocks.iter().any(|b| plain(b).contains("code:mmd")));
    }

    #[test]
    fn blank_lines_after_a_code_block_are_all_kept() {
        let lines: Vec<String> = ["t", "code:x.py", " print(1)", "", "", "", "after"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for hl in [None, Some(crate::highlight::Highlighter::new(None, false))] {
            let out = render_lines_with(&lines, hl.as_ref(), &Palette::for_light(false));
            let got: Vec<String> = out.blocks.iter().map(plain).collect();
            assert_eq!(
                got,
                vec!["t", "code:x.py  ↓", "  print(1)", "[BLANK]", "[BLANK]", "[BLANK]", "after"],
                "highlighted={}",
                hl.is_some()
            );
            // every source line owns exactly one block, in order
            assert_eq!(out.srcs, vec![0, 1, 2, 3, 4, 5, 6]);
        }
        // a blank INSIDE the block (more code follows) stays in the block
        let lines: Vec<String> = ["t", "code:x.py", " a = 1", "", " b = 2", "end"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_lines(&lines);
        let got: Vec<String> = out.blocks.iter().map(plain).collect();
        assert_eq!(got, vec!["t", "code:x.py  ↓", "  a = 1", "  ", "  b = 2", "end"]);
        assert_eq!(out.srcs, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn blank_preserved_and_flush_left_bullet() {
        let lines = vec![
            "お問い合わせメール対応".to_string(),
            " お問い合わせメールのフォーマット".to_string(),
            "".to_string(),
            "次のセクション".to_string(),
        ];
        let out = render_lines(&lines);
        let got: Vec<String> = out.blocks.iter().map(plain).collect();
        assert_eq!(got[0], "お問い合わせメール対応");
        assert_eq!(got[1], "• お問い合わせメールのフォーマット");
        assert_eq!(got[2], "[BLANK]");
        assert_eq!(got[3], "次のセクション");
    }

    #[test]
    fn fullwidth_space_nesting() {
        let lines = vec![
            "ページタイトル".to_string(),
            "\t\t\t\t本文".to_string(),
            "\t\t\t\t\u{3000}見出し".to_string(),
            "\t\t\t\t\u{3000}    基本は見出しH2で作成".to_string(),
        ];
        let out = render_lines(&lines);
        let got: Vec<String> = out.blocks.iter().map(plain).collect();
        // One source whitespace char remains one logical level, but each
        // nesting step is two terminal columns. 4 tabs → col 6; +　 →
        // col 8; +　+4 spaces → col 16.
        assert_eq!(got[1], "      • 本文");
        assert_eq!(got[2], "        • 見出し");
        assert_eq!(got[3], "                • 基本は見出しH2で作成");
    }

    /// The editor asks `code_span_at` where a block is; the renderer
    /// decides what a block is while it walks the page. If they disagree,
    /// Enter drops a line where the renderer will not read it as code.
    #[test]
    fn code_span_agrees_with_what_the_renderer_collected() {
        let src = [
            "title",
            " before",
            "code:x.py",
            " a = 1",
            "",
            " b = 2",
            "",
            "after",
            " nested",
            "  code:y.txt",
            "   deep",
            "plain",
        ];
        let refs: Vec<&str> = src.to_vec();
        let inside: Vec<usize> = (0..src.len())
            .filter(|&i| code_span_at(&refs, i).is_some())
            .collect();
        assert_eq!(inside, vec![2, 3, 4, 5, 9, 10], "blank INSIDE stays, trailing blank leaves");

        let top = code_span_at(&refs, 3).unwrap();
        assert_eq!(top.header, 2);
        assert_eq!(top.body_indent(), " ", "one step deeper than a flush header");

        let nested = code_span_at(&refs, 10).unwrap();
        assert_eq!(nested.header, 9);
        assert_eq!(nested.body_indent(), "   ", "deeper header, deeper body");
    }

    /// `[]` is not notation — an empty link has nothing to link to — so
    /// Cosense shows the brackets as the text they are. The viewer used to
    /// swallow them, which made a line ABOUT `[]` lose the thing it was
    /// about (they only survived inside a code block).
    #[test]
    fn empty_brackets_are_text_and_double_brackets_are_bold() {
        let pal = Palette::for_light(false);
        let plain = |src: &str| -> String {
            let out = render_lines_with(&["t".to_string(), src.to_string()], None, &pal);
            out.blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(l) => {
                        Some(l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    }
                    _ => None,
                })
                .nth(1)
                .unwrap_or_default()
        };
        assert_eq!(plain("[]"), "[]");
        assert_eq!(plain("a[]b"), "a[]b");
        assert_eq!(plain("空の [] を書く"), "空の [] を書く");
        assert_eq!(plain("[ ]"), "[ ]", "whitespace is not a page name either");
        assert_eq!(plain("[foo"), "[foo", "an unclosed bracket is text");

        // A real link still loses its brackets, as on the web.
        assert_eq!(plain("[リンク]"), "リンク");

        // `[[text]]` is bold: matched as a PAIR, or the first `]` would cut
        // it at `[text` and it would read as a link.
        assert_eq!(plain("[[太字]]"), "太字");
        assert_eq!(plain("[[]]"), "[[]]", "empty double brackets are text too");
        // `[[x]]` is Cosense's other spelling of `[* x]`, so it wears the
        // same style — whatever the theme makes that level look like.
        let styles = |src: &str| -> Vec<Style> {
            render_lines_with(&["t".to_string(), src.to_string()], None, &pal)
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(l) => Some(l.spans.iter().map(|s| s.style).collect::<Vec<_>>()),
                    _ => None,
                })
                .nth(1)
                .unwrap_or_default()
        };
        assert_eq!(styles("[[太字]]"), styles("[* 太字]"));
        assert_ne!(styles("[[太字]]"), styles("太字"), "and it is not plain text");
    }

    /// The memo's three image complaints, as one test: a bare URL is not a
    /// picture, the text after a picture survives, and a second picture on
    /// the same line is not dropped.
    #[test]
    fn a_line_keeps_its_text_and_every_picture_on_it() {
        let pal = Palette::for_light(false);
        let shape = |src: &str| -> Vec<String> {
            let out = render_lines_with(&["t".to_string(), src.to_string()], None, &pal);
            out.blocks
                .iter()
                .skip(1) // the title row
                .map(|b| match b {
                    Block::Image { url, indent, item } => {
                        format!("image:{indent}{}:{url}", if *item { "*" } else { "" })
                    }
                    Block::Inline { indent, item, parts } => {
                        let shape: Vec<String> = parts
                            .iter()
                            .map(|p| match p {
                                InlinePart::Image(u) => format!("img({u})"),
                                InlinePart::Text(l) => format!(
                                    "txt({})",
                                    l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
                                ),
                            })
                            .collect();
                        format!("inline:{indent}{}:{}", if *item { "*" } else { "" }, shape.join("|"))
                    }
                    Block::Text(l) => {
                        format!("text:{}", l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    }
                    _ => "other".into(),
                })
                .collect()
        };

        // A line that is nothing but the bracket becomes the picture.
        assert_eq!(
            shape("[https://example.com/a.png]"),
            vec!["image:0:https://example.com/a.png".to_string()],
        );

        // A bare URL stays a link — even at the start of the line, which is
        // exactly where the viewer used to turn it into a picture.
        let bare = shape("https://example.com/a.png こんな感じ");
        assert_eq!(bare.len(), 1, "{bare:?}");
        assert!(bare[0].starts_with("text:"), "{bare:?}");
        assert!(bare[0].contains("こんな感じ"));

        // A line that mixes text and pictures is ONE block: a sequence of
        // parts in the order written, which the viewer lays out like a
        // sentence with inline images.
        let after = shape("[https://example.com/a.png]こんな感じに後ろのテキストも表示される");
        assert_eq!(
            after,
            vec!["inline:0:img(https://example.com/a.png)|txt(こんな感じに後ろのテキストも表示される)"],
        );

        // Text BEFORE a picture is the same block, in the other order —
        // and the order is what the reading follows.
        let before = shape("先に本文 [https://example.com/a.png]");
        assert_eq!(before, vec!["inline:0:txt(先に本文 )|img(https://example.com/a.png)"]);

        // Cosense hangs pictures off bullets: an indented image line
        // belongs to the item above it, so it starts where that item's
        // text starts instead of flush left.
        assert_eq!(
            shape(" [https://example.com/a.png]"),
            vec!["image:2*:https://example.com/a.png".to_string()],
            "one level in = the column after `• `, and the picture IS the item",
        );
        // Indented, opening with the picture: the same block, carrying the
        // column its level starts at — and marked as a list ITEM, so the
        // bullet is not lost just because text was mixed in.
        let indented = shape("  [https://example.com/a.png] と本文");
        assert_eq!(indented.len(), 1, "{indented:?}");
        assert!(indented[0].starts_with("inline:4*:img("), "{indented:?}");
        assert!(indented[0].contains("txt( と本文)"), "{indented:?}");
        // Flush against the margin there is no item and no bullet.
        assert!(shape("[https://example.com/a.png] と本文")[0].starts_with("inline:0:"));

        // Quoted notation is a line ABOUT the picture, not a picture: a
        // documentation page must be able to show what it is describing.
        let quoted = shape("`[https://example.com/a.png]` → 画像になる");
        assert_eq!(quoted.len(), 1, "{quoted:?}");
        assert!(quoted[0].starts_with("text:"), "{quoted:?}");
        assert_eq!(shape("`[https://example.com/a.png]`").len(), 1);

        // Two pictures on one line: both, with the words between them kept
        // between them.
        let two = shape("[https://example.com/a.png] と [https://example.com/b.png]");
        assert_eq!(
            two,
            vec!["inline:0:img(https://example.com/a.png)|txt( と )|img(https://example.com/b.png)"],
        );
    }

    /// Star count is a heading LEVEL, mid-line as much as on a line of its
    /// own, and a decorated link is still a link. Rendering the inline
    /// forms as flat bold threw both away: `[* x]` and `[*** x]` looked
    /// identical, and `[[[page]]]` was bold text you could not follow.
    #[test]
    fn inline_decorations_carry_the_heading_level_and_keep_links() {
        let pal = Palette::for_light(false);
        let render = |src: &str| -> RenderOutput {
            render_lines_with(&["t".to_string(), src.to_string()], None, &pal)
        };
        let styles = |src: &str| -> Vec<Style> {
            render(src)
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(l) => Some(l.spans.iter().map(|s| s.style).collect::<Vec<_>>()),
                    _ => None,
                })
                .nth(1)
                .unwrap_or_default()
        };

        // Each level is its own look, and the inline form matches the
        // whole-line heading of the same level.
        let one = styles("→ [* 見出し]");
        let two = styles("→ [** 見出し]");
        let three = styles("→ [*** 見出し]");
        assert_ne!(one, two);
        assert_ne!(two, three);
        assert_eq!(
            *one.last().unwrap(),
            star_style(1, &pal),
            "one star = the level-4 heading style, same as a heading line",
        );
        assert_eq!(*two.last().unwrap(), star_style(2, &pal));
        assert_eq!(*three.last().unwrap(), star_style(3, &pal));
        // Bold is the floor: `[* x]` is how Cosense bolds a word, whatever
        // the theme does with the level on top of it.
        for st in [one.last().unwrap(), two.last().unwrap(), three.last().unwrap()] {
            assert!(st.add_modifier.contains(Modifier::BOLD), "{st:?}");
        }

        // Italic comes from the `/` flag and NOWHERE else: borrowing the
        // theme's italic markdown heading made `[* x]` look like `[/ x]`.
        for st in [one.last().unwrap(), two.last().unwrap(), three.last().unwrap()] {
            assert!(!st.add_modifier.contains(Modifier::ITALIC), "{st:?}");
        }
        let slash = *styles("→ [*/ 斜体]").last().unwrap();
        assert!(slash.add_modifier.contains(Modifier::ITALIC));
        assert!(slash.add_modifier.contains(Modifier::BOLD));
        assert_eq!(slash.fg, star_style(1, &pal).fg, "and keeps its level");

        // A decorated link: link colour, still underlined, bold — and not
        // italic, which was what "bold link" actually looked like.
        let deco = *styles("[[[改善案]]]").last().unwrap();
        assert!(deco.add_modifier.contains(Modifier::BOLD));
        assert!(!deco.add_modifier.contains(Modifier::ITALIC));

        // A link inside a decoration is REGISTERED as a link (so Enter can
        // follow it) and keeps the link colour.
        let out = render("[[[改善案]]]");
        assert_eq!(out.extracted.links, vec!["改善案".to_string()], "followable");
        let deco = styles("[[[改善案]]]");
        assert_eq!(deco.last().unwrap().fg, Some(pal.link), "still reads as a link");
        assert!(deco.last().unwrap().add_modifier.contains(Modifier::UNDERLINED));

        // Same for the single-bracket decoration form.
        let out = render("[* [改善案]]");
        assert_eq!(out.extracted.links, vec!["改善案".to_string()]);
    }
}
