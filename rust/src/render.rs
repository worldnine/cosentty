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
    Image { url: String },
    /// A structured table, laid out against the pane width at draw time.
    Table(crate::table::Table),
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

fn indent_info(raw: &str) -> (usize, usize, &str) {
    let mut raw_len = 0usize;
    let mut level = 0usize;
    let mut chars = raw.char_indices().peekable();
    let mut end_byte = 0usize;
    while let Some(&(idx, c)) = chars.peek() {
        if c == ' ' {
            level += 1;
            raw_len += 1;
            chars.next();
            // consume the rest of this run of spaces (same level)
            while let Some(&(_, ' ')) = chars.peek() {
                raw_len += 1;
                chars.next();
            }
        } else if c == '\t' || c == '\u{3000}' {
            level += 1;
            raw_len += 1;
            chars.next();
        } else {
            end_byte = idx;
            break;
        }
        // track byte offset of remaining text
        end_byte = chars.peek().map(|&(i, _)| i).unwrap_or(raw.len());
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
            if let Some(end_rel) = rest[start + 1..].find(']') {
                let end = start + 1 + end_rel;
                push_plain(&mut spans, &rest[..start], links, pal);
                let inner = &rest[start + 1..end];
                decorate_bracket(inner, &mut spans, links, images, pal);
                rest = &rest[end + 1..];
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
                spans.push(Span::styled(format!("#{tag}"), Style::default().fg(pal.hashtag)));
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

/// Decorate a [...] bracket's inner content.
fn decorate_bracket(
    inner: &str,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
) {
    // decoration: [* text] [** text] [*/ text] [- strike] [_ underline]
    if let Some(rest) = strip_deco_prefix(inner) {
        let (flags, body) = rest;
        let mut style = Style::default();
        if flags.contains('*') {
            style = style.add_modifier(Modifier::BOLD);
        }
        if flags.contains('/') {
            style = style.add_modifier(Modifier::ITALIC);
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
            spans.push(Span::styled(format!("🖼 {label}"), Style::default().fg(Color::Magenta)));
        } else if is_scrapbox_file_url(url) {
            // Uploaded file: a paper-clip so it reads as "download", not
            // "open in browser". Enter/f in the viewer saves and opens it.
            let label = if title.is_empty() { file_name_of_url(url) } else { title };
            spans.push(Span::styled(format!("📎 {label}"), style_url(pal)));
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

fn indent_str(level: usize) -> String {
    "  ".repeat(level.saturating_sub(1))
}

/// File extensions rendered as images (what the `image` crate can decode).
const IMAGE_EXTS: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".tiff",
];

/// Whether a URL points at a decodable image: by file extension (query and
/// fragment ignored) or by being a Scrapbox upload.
fn looks_like_image_url(url: &str) -> bool {
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
fn standalone_image(body: &str) -> Option<String> {
    let inner = body.trim();
    let inner = inner.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(inner);
    let inner = inner.trim();

    // gyazo (permalink form, resolved by the image fetcher)
    let mut g = Vec::new();
    find_gyazo(inner, &mut g);
    if let Some(u) = g.into_iter().next() {
        if inner.contains("gyazo.com") && inner.split_whitespace().count() <= 2 {
            return Some(u);
        }
    }

    // Any image URL among the tokens: covers a bare URL, `[imageUrl]`, and
    // the linked-image form `[href imageUrl]` / `[imageUrl href]`.
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.len() <= 2 {
        if let Some(u) = tokens.iter().find(|t| looks_like_image_url(t)) {
            return Some((*u).to_string());
        }
    }

    // legacy explicit branch kept for scrapbox.io/files with extension
    if (inner.starts_with("https://scrapbox.io/files/")
        || inner.starts_with("https://") && inner.contains("/files/"))
        && (inner.ends_with(".png")
            || inner.ends_with(".jpg")
            || inner.ends_with(".jpeg")
            || inner.ends_with(".gif")
            || inner.ends_with(".webp"))
    {
        return Some(inner.to_string());
    }
    None
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

        // blank line (preserve). Code blocks are consumed whole below, so no
        // in-code guard is needed here anymore.
        if raw.trim().is_empty() {
            emit!(Block::Blank);
            i += 1;
            continue;
        }

        // table block
        if let Some(name) = body.strip_prefix("table:") {
            let name = name.trim().to_string();
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let (_, rl, rrest) = indent_info(&lines[j]);
                if rl <= raw_len || lines[j].trim().is_empty() {
                    break;
                }
                rows.push(rrest.split('\t').map(|c| c.trim_end().to_string()).collect());
                j += 1;
            }
            // decorate cells to styled spans (links stay navigable via extraction)
            let styled_rows: Vec<Vec<Vec<Span<'static>>>> = rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|c| decorate_inline(c, &mut ex.links, &mut ex.images, pal))
                        .collect()
                })
                .collect();
            // Blank-only leading indent is dropped: Scrapbox tables render
            // flush; nesting is rare and the width-adaptive layout owns
            // horizontal budget. Keep the indent as a note for the future.
            let _ = &indent;
            emit!(Block::Table(crate::table::Table { name, rows: styled_rows }));
            i = j;
            continue;
        }

        // code block: `code:name.ext` header + indented continuation lines.
        if let Some(rest) = body.strip_prefix("code:") {
            let lang = rest.trim().to_string();
            let code_indent = raw_len;
            // header line
            emit!(Block::Text(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(format!("code:{lang}"), Style::default().fg(pal.code_fence)),
            ])));
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
                        out.push(Block::Text(Line::from(line_spans)));
                        srcs.push(src);
                    }
                }
                None => {
                    for (k, b) in bodies.iter().enumerate() {
                        let src = raws.get(k).copied().unwrap_or(i);
                        out.push(Block::Text(Line::from(vec![
                            Span::raw(format!("{indent}  ")),
                            Span::styled(b.clone(), Style::default().fg(Color::DarkGray)),
                        ])));
                        srcs.push(src);
                    }
                }
            }
            i = j;
            continue;
        }

        // standalone image line
        if let Some(url) = standalone_image(body) {
            ex.images.push(url.clone());
            emit!(Block::Image { url });
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

        // section heading: whole line wrapped in [* ...] at level 0
        if level == 0 {
            if let Some(spans) = parse_heading(body, pal) {
                emit!(Block::Text(Line::from(spans)));
                i += 1;
                continue;
            }
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
fn parse_heading(body: &str, pal: &Palette) -> Option<Vec<Span<'static>>> {
    let inner = body.strip_prefix('[')?.strip_suffix(']')?;
    let stars = inner.char_indices().find(|(_, c)| *c != '*').map(|(i, _)| i)?;
    if stars == 0 {
        return None;
    }
    let text = inner[stars..].strip_prefix(' ')?;
    Some(vec![Span::styled(text.to_string(), pal.heading_style_for(stars))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(b: &Block) -> String {
        match b {
            Block::Blank => "[BLANK]".into(),
            Block::Image { url } => format!("[IMAGE {url}]"),
            Block::Text(l) => l.spans.iter().map(|s| s.content.as_ref()).collect(),
            Block::Table(_) => "[TABLE]".into(),
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
        // bare URL, bracketed URL, and the linked-image form all resolve
        assert_eq!(
            standalone_image("https://example.com/a.png"),
            Some("https://example.com/a.png".into())
        );
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
            standalone_image("https://example.com/a.png?w=800"),
            Some("https://example.com/a.png?w=800".into())
        );
        // non-image links are not images
        assert_eq!(standalone_image("[https://example.com/page.html]"), None);
        assert_eq!(standalone_image("ただの本文"), None);
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
            Block::Image { url } => assert_eq!(url, &format!("https://acme-inc.gyazo.com/{id}")),
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
        assert_eq!(style_of(&out.blocks[1]), pal.heading_style_for(1));
        assert_eq!(style_of(&out.blocks[2]), pal.heading_style_for(2));
        assert_eq!(style_of(&out.blocks[3]), pal.heading_style_for(3));
        assert_eq!(style_of(&out.blocks[4]), pal.heading_style_for(4));
        assert_eq!(style_of(&out.blocks[5]), pal.heading_style_for(4), "four+ stars share the top level");
        assert_ne!(style_of(&out.blocks[1]), style_of(&out.blocks[4]));
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
                vec!["t", "code:x.py", "  print(1)", "[BLANK]", "[BLANK]", "[BLANK]", "after"],
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
        assert_eq!(got, vec!["t", "code:x.py", "  a = 1", "  ", "  b = 2", "end"]);
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
        // relative nesting: 本文 < 見出し < 基本は, each +2 columns
        assert_eq!(got[1], "      • 本文");
        assert_eq!(got[2], "        • 見出し");
        assert_eq!(got[3], "          • 基本は見出しH2で作成");
    }
}
