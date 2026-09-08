//! Inline notation, link/image recognition and span decoration.

use super::*;

/// A block label that can be followed (`code:name`, `table:name`): the
/// notation colour, underlined like every other followable row.
pub(super) fn style_block_label(pal: &Palette) -> Style {
    Style::default()
        .fg(pal.code_fence)
        .add_modifier(Modifier::UNDERLINED)
}

pub(super) fn style_link(pal: &Palette) -> Style {
    Style::default()
        .fg(pal.link)
        .add_modifier(Modifier::UNDERLINED)
}
/// A link to a page nobody has written yet. Still underlined: it is still
/// followable — Enter opens it and the first line you type creates it.
pub(super) fn style_link_missing(pal: &Palette) -> Style {
    Style::default()
        .fg(pal.link_missing)
        .add_modifier(Modifier::UNDERLINED)
}

pub(super) fn style_url(pal: &Palette) -> Style {
    Style::default()
        .fg(pal.url)
        .add_modifier(Modifier::UNDERLINED)
}

/// LaTeX shown as LaTeX (too tall for the pane, or beyond the renderer).
/// It borrows the code colour — unrendered, it is notation, the same kind
/// of thing as a `code:` label — without the underline, which in this
/// viewer means "you can press Enter here".
///
/// A DRAWN formula gets no colour of its own: it is prose's equal, and the
/// theme's foreground is the ink the sentence around it is written in.
pub(super) fn style_formula_source(pal: &Palette) -> Style {
    Style::default().fg(pal.code_fence)
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

pub(super) fn find_gyazo(s: &str, out: &mut Vec<String>) {
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
pub fn strip_leading_ws(raw: &str, n: usize) -> String {
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
pub fn indent_info(raw: &str) -> (usize, usize, &str) {
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
pub(super) fn decorate_inline(
    s: &str,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
) -> Vec<Span<'static>> {
    find_gyazo(s, images);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = s;

    // Walk the string, handling `code`, [..] brackets, bare urls, #tags.
    // Whichever token STARTS first is taken first: a `[link]` before a
    // `code` span is a link, and a `[bracket]` inside a code span is code.
    // (Taking the code span first, wherever it was, flushed everything in
    // front of it as plain text — and a link earlier on the same line
    // lost its brackets' meaning whenever the line also had some code.)
    while !rest.is_empty() {
        let tick = rest
            .find('`')
            .and_then(|start| rest[start + 1..].find('`').map(|e| (start, start + 1 + e)));
        let bracket = rest
            .find('[')
            .and_then(|start| matching_bracket(rest, start).map(|c| (start, c)));
        match (tick, bracket) {
            // inline code
            (Some((start, end)), b) if b.map_or(true, |(bs, _)| start < bs) => {
                push_plain(&mut spans, &rest[..start], links, pal, known, hits);
                let code = &rest[start + 1..end];
                spans.push(Span::styled(
                    format!(" {code} "),
                    Style::default().bg(Color::DarkGray).fg(Color::White),
                ));
                rest = &rest[end + 1..];
            }
            // bracket form
            (_, Some((start, close))) => {
                // `[[text]]` (bold) has to be matched as a PAIR: taking the
                // first `]` would cut it at `[text`, and the notation would
                // read as a link to a page whose name starts with a bracket.
                // Balanced, so notation can nest: `[[[page]]]` is a bold link
                // and `[* [page]]` is a decorated one. Closing at the first
                // `]` cut both one bracket short, and the link inside was lost.
                let doubled = rest[start + 1..].starts_with('[') && rest[..close].ends_with(']');
                push_plain(&mut spans, &rest[..start], links, pal, known, hits);
                if doubled {
                    decorate_bold(
                        &rest[start + 2..close - 1],
                        &mut spans,
                        links,
                        pal,
                        known,
                        hits,
                    );
                } else {
                    decorate_bracket(
                        &rest[start + 1..close],
                        &mut spans,
                        links,
                        images,
                        pal,
                        known,
                        hits,
                    );
                }
                rest = &rest[close + 1..];
            }
            // no more special tokens
            _ => {
                push_plain(&mut spans, rest, links, pal, known, hits);
                break;
            }
        }
    }
    spans
}

/// Merge the hits of a nested decoration, whose span indices start at 0,
/// into a line that already has `base` spans in front of them.
pub(super) fn merge_hits(hits: &mut Vec<Hit>, inner: Vec<Hit>, base: usize) {
    hits.extend(inner.into_iter().map(|h| Hit {
        span: h.span + base,
        ..h
    }));
}

/// Handle bare text: detect URLs and #hashtags, produce spans.
pub(super) fn push_plain(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    links: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
) {
    if text.is_empty() {
        return;
    }
    let mut rest = text;
    while !rest.is_empty() {
        // URL
        if let Some(pos) = find_url(rest) {
            let (url, after) = take_url(&rest[pos..]);
            if pos > 0 {
                push_tags(spans, &rest[..pos], links, pal, known, hits);
            }
            hits.push(Hit {
                span: spans.len(),
                target: HitTarget::Url {
                    label: url.to_string(),
                    url: url.to_string(),
                },
            });
            spans.push(Span::styled(url.to_string(), style_url(pal)));
            rest = after;
            continue;
        }
        push_tags(spans, rest, links, pal, known, hits);
        break;
    }
}

/// Handle #hashtags within a plain segment.
pub(super) fn push_tags(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    links: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
) {
    let mut rest = text;
    loop {
        if let Some(pos) = rest.find('#') {
            // must be at start or preceded by whitespace
            let ok = pos == 0
                || rest[..pos]
                    .chars()
                    .next_back()
                    .map(|c| c.is_whitespace())
                    .unwrap_or(false);
            let tag: String = rest[pos + 1..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if ok && !tag.is_empty() {
                if pos > 0 {
                    spans.push(Span::raw(rest[..pos].to_string()));
                }
                links.push(tag.clone());
                // A tag IS a link — `#foo` and `[foo]` are the same page,
                // and Cosense draws them the same way (both are
                // `.page-link` in its stylesheet; there is no rule for a
                // hashtag anywhere in it, uncreated ones included). So no
                // colour of its own: the `#` already says which notation
                // was written.
                let style = if known.missing(&tag) {
                    style_link_missing(pal)
                } else {
                    style_link(pal)
                };
                hits.push(Hit {
                    span: spans.len(),
                    target: HitTarget::Page(tag.clone()),
                });
                spans.push(Span::styled(format!("#{tag}"), style));
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

pub(super) fn find_url(s: &str) -> Option<usize> {
    let h = s.find("http://");
    let hs = s.find("https://");
    match (h, hs) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

pub(super) fn take_url(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || *c == ']')
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// `[[text]]` — Cosense's other way of writing `[* text]`. Empty double
/// brackets are text, like empty single ones.
pub(super) fn decorate_bold(
    inner: &str,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
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
    let mut inner_hits = Vec::new();
    let decorated = decorate_inline(inner, links, &mut images, pal, known, &mut inner_hits);
    merge_hits(hits, inner_hits, spans.len());
    for sp in decorated {
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
pub(super) fn star_style(stars: usize, pal: &Palette) -> Style {
    // Italic is NOT borrowed. In Cosense italic is its own flag (`[/ x]`),
    // so italics appearing without one is a false signal — and the theme's
    // smallest markdown heading is italic, which is exactly the level one
    // star lands on. Colour (and the top level's underline) carry the
    // level; bold carries the emphasis.
    pal.heading_style_for(stars)
        .remove_modifier(Modifier::ITALIC)
        .add_modifier(Modifier::BOLD)
}

/// Cosense の米印は 10 個まで効き、1 個ごとに 1.2 倍ずつ文字が大きくなる。
/// ここに大きさの概念は無い。
const MAX_STARS: usize = 10;

/// 米印 5 個以上に添える `*N` の印。
///
/// テーマの見出し書式は 4 段までで、5 個以上は最上位に丸められて見分けが
/// つかなかった(web では `[***** ]` と `[********** ]` は 2.5 倍と 6 倍で
/// 別物)。端末に文字の大きさは無いので、記法そのものの延長として個数を
/// 添える。閲覧時だけの印で、編集中の行は生テキストなので米印が直接見える。
/// 4 個以下はテーマの書式で足りているので何も添えない。
pub(super) fn star_level_marker(stars: usize, pal: &Palette) -> Option<Span<'static>> {
    if stars <= 4 {
        return None;
    }
    Some(Span::styled(
        format!(" *{}", stars.min(MAX_STARS)),
        Style::default()
            .fg(pal.heading_for(stars))
            .add_modifier(Modifier::DIM),
    ))
}

/// Byte index of the `]` that closes the `[` at `open`, counting nesting./// Byte index of the `]` that closes the `[` at `open`, counting nesting.
///
/// Taking the FIRST `]` cuts `[* [改善案]]` at `[改善案`, and the link
/// inside a decoration is lost — it renders as text with a stray bracket
/// and cannot be followed.
pub fn matching_bracket(s: &str, open: usize) -> Option<usize> {
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
pub(super) fn decorate_bracket(
    inner: &str,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
) {
    // `[]` and `[   ]` are not notation — an empty link is nothing to
    // link to, so Cosense shows the brackets as the text they are. The
    // viewer used to swallow them, and a line written about `[]` lost the
    // very thing it was about.
    if inner.trim().is_empty() {
        spans.push(Span::raw(format!("[{inner}]")));
        return;
    }
    // formula: [$ E = mc^2 ]. Cosense's own notation, and the one bracket
    // whose inside is NOT Cosense text — it is LaTeX, so no link, no
    // decoration, no icon is read out of it.
    if let Some(latex) = inner.strip_prefix('$') {
        spans.push(match crate::math::render_inline(latex) {
            // A DRAWN formula is the thing itself, so it is set in the
            // body's own ink — the same ink Cosense's own KaTeX uses, and
            // the same the rest of the sentence is written in.
            Some(text) => Span::raw(text),
            // Nothing could be drawn (`\begin{align}` and friends). What is
            // left is the NOTATION, which wears the notation colour — the
            // one a `code:` label and inline code already use.
            None => Span::styled(latex.trim().to_string(), style_formula_source(pal)),
        });
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
        let mut style = if stars > 0 {
            star_style(stars, pal)
        } else {
            Style::default()
        };
        if flags.contains('/') {
            style = style.add_modifier(Modifier::ITALIC);
        }
        // The inside is ordinary notation, so a decorated LINK is still a
        // link: `[[[改善案]]]` and `[* [改善案]]` both have to stay
        // followable, not just look bold.
        if body.contains('[') && body.contains(']') {
            let mut inner_hits = Vec::new();
            let inner = decorate_inline(body, links, images, pal, known, &mut inner_hits);
            merge_hits(hits, inner_hits, spans.len());
            for sp in inner {
                spans.push(Span::styled(sp.content, style.patch(sp.style)));
            }
            spans.extend(star_level_marker(stars, pal));
            return;
        }
        if flags.contains('-') {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if flags.contains('_') {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(body.to_string(), style));
        spans.extend(star_level_marker(stars, pal));
        return;
    }
    // icon: [name.icon]
    if inner.ends_with(".icon") || inner.contains(".icon") {
        let name = inner.split(".icon").next().unwrap_or(inner);
        spans.push(Span::styled(
            format!("@{name}"),
            Style::default().fg(pal.code_fence),
        ));
        return;
    }
    // external link with optional title
    if let Some(pos) = find_url(inner) {
        let (url, _) = take_url(&inner[pos..]);
        let title = inner.replace(url, "");
        let title = title.trim();
        // Whatever the label turns out to be, the thing this leads to is
        // the URL; recorded once here rather than in each arm below.
        let mut hit = |label: &str, spans: &Vec<Span<'static>>| {
            hits.push(Hit {
                span: spans.len(),
                target: HitTarget::Url {
                    label: label.to_string(),
                    url: url.to_string(),
                },
            });
        };
        if url.contains("gyazo.com") {
            find_gyazo(url, images);
            let label = if title.is_empty() { "[gyazo]" } else { title };
            hit(label, spans);
            spans.push(Span::styled(
                label.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::UNDERLINED),
            ));
        } else if is_scrapbox_file_url(url) {
            // Uploaded file: a paper-clip so it reads as "download", not
            // "open in browser". Enter/f in the viewer saves and opens it.
            let label = if title.is_empty() {
                file_name_of_url(url)
            } else {
                title
            };
            hit(label, spans);
            spans.push(Span::styled(label.to_string(), style_url(pal)));
        } else if looks_like_image_url(url) {
            // The picture is drawn on its own row; the text row only needs
            // to say that it is there. Spelling out the whole URL made a
            // one-line note wrap over three rows of link.
            let label = if title.is_empty() {
                file_name_of_url(url)
            } else {
                title
            };
            hit(label, spans);
            spans.push(Span::styled(label.to_string(), style_url(pal)));
        } else {
            let label = if title.is_empty() { url } else { title };
            hit(label, spans);
            spans.push(Span::styled(label.to_string(), style_url(pal)));
        }
        return;
    }
    // internal page link
    links.push(inner.to_string());
    let style = if known.missing(inner) {
        style_link_missing(pal)
    } else {
        style_link(pal)
    };
    hits.push(Hit {
        span: spans.len(),
        target: HitTarget::Page(inner.to_string()),
    });
    spans.push(Span::styled(inner.to_string(), style));
}

/// If inner starts with a run of */-_ followed by space, return (flags, body).
pub(super) fn strip_deco_prefix(inner: &str) -> Option<(&str, &str)> {
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

pub(super) fn indent_str(level: usize) -> String {
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
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
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
pub(super) fn image_in_bracket(inner: &str) -> Option<String> {
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
pub(super) fn line_images(body: &str) -> Vec<String> {
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
                let Some(end) = matching_bracket(rest, o) else {
                    break;
                };
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

/// Does this line hold a formula that needs rows of its own? Same scan as
/// [`line_images`], and for the same reason: quoted notation is text about
/// a formula, not a formula.
pub(super) fn has_tall_formula(body: &str) -> bool {
    let mut rest = body;
    loop {
        let tick = rest.find('`');
        let open = rest.find('[');
        match (tick, open) {
            (Some(t), Some(o)) if t < o => {
                let after = &rest[t + 1..];
                match after.find('`') {
                    Some(close) => rest = &after[close + 1..],
                    None => return false,
                }
            }
            (_, Some(o)) => {
                let Some(end) = matching_bracket(rest, o) else {
                    return false;
                };
                if tall_formula_rows(&rest[o + 1..end]).is_some() {
                    return true;
                }
                rest = &rest[end + 1..];
            }
            _ => return false,
        }
    }
}

/// Split a line into its inline parts: runs of text and the pictures
/// between them, in the order written. Text runs keep every other piece of
/// notation (links, decoration, code) intact.
pub(super) fn inline_parts(
    body: &str,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
    pal: &Palette,
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
) -> Vec<InlinePart> {
    let mut parts: Vec<InlinePart> = Vec::new();
    let mut text_from = 0usize;
    let mut i = 0usize;
    // Ranges of plain text, collected first so the borrow checker is not
    // asked to share `images` between a closure and the loop.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    // Ok(a part of its own) | Err(run index)
    let mut order: Vec<Result<InlinePart, usize>> = Vec::new();
    while i < body.len() {
        let Some(rel) = body[i..].find('[') else {
            break;
        };
        let open = i + rel;
        // Quoted notation is text about a picture, not a picture.
        if body[..open].matches('`').count() % 2 == 1 {
            i = open + 1;
            continue;
        }
        let Some(close) = matching_bracket(body, open) else {
            break;
        };
        let inner = &body[open + 1..close];
        let own_part = image_in_bracket(inner)
            .map(InlinePart::Image)
            .or_else(|| tall_formula(inner, pal));
        if let Some(part) = own_part {
            if open > text_from {
                runs.push((text_from, open));
                order.push(Err(runs.len() - 1));
            }
            order.push(Ok(part));
            text_from = close + 1;
        }
        i = close + 1;
    }
    if body.len() > text_from {
        runs.push((text_from, body.len()));
        order.push(Err(runs.len() - 1));
    }
    // Hits count spans across the text parts in order (see `Hit::span`),
    // so a later part's hits are shifted past the spans before it.
    let mut span_base = 0usize;
    for item in order {
        match item {
            Ok(InlinePart::Image(url)) => {
                images.push(url.clone());
                parts.push(InlinePart::Image(url));
            }
            Ok(part) => parts.push(part),
            Err(idx) => {
                let (from, to) = runs[idx];
                let mut run_hits = Vec::new();
                let spans =
                    decorate_inline(&body[from..to], links, images, pal, known, &mut run_hits);
                if !spans.is_empty() {
                    merge_hits(hits, run_hits, span_base);
                    span_base += spans.len();
                    parts.push(InlinePart::Text(Line::from(spans)));
                }
            }
        }
    }
    parts
}

/// A `[$ ... ]` that cannot be set inside a text run because it is drawn
/// over more than one row. One-row formulas return `None` here and are
/// handled by `decorate_bracket` as part of the text.
pub(super) fn tall_formula(inner: &str, pal: &Palette) -> Option<InlinePart> {
    let (latex, r) = tall_formula_rows(inner)?;
    Some(InlinePart::Formula {
        source: Line::from(Span::styled(latex, style_formula_source(pal))),
        rows: r.rows,
        baseline: r.baseline,
    })
}

/// The drawing behind [`tall_formula`], without the styling — so the line
/// scan can ask "is there one?" before any palette is involved.
pub(super) fn tall_formula_rows(inner: &str) -> Option<(String, crate::math::Rendered)> {
    let latex = inner.strip_prefix('$')?;
    let r = crate::math::render_rows(latex)?;
    (r.rows.len() > 1).then(|| (latex.trim().to_string(), r))
}

/// Every `[$ ... ]` formula on a line, left to right — the same reading the
/// renderer draws with (`decorate_bracket`), exposed for the editor's live
/// preview. Backtick-quoted spans are text ABOUT notation, not notation.
pub fn inline_formulas(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        let tick = rest.find('`');
        let open = rest.find('[');
        match (tick, open) {
            (Some(t), Some(o)) if t < o => {
                let after = &rest[t + 1..];
                match after.find('`') {
                    Some(close) => rest = &after[close + 1..],
                    None => return out,
                }
            }
            (_, Some(o)) => {
                let Some(end) = matching_bracket(rest, o) else {
                    return out;
                };
                if let Some(latex) = rest[o + 1..end].strip_prefix('$') {
                    out.push(latex.trim().to_string());
                }
                rest = &rest[end + 1..];
            }
            _ => return out,
        }
    }
}

/// A line that opens with a picture and continues in text/// The image this line is ENTIRELY made of/// The image this line is ENTIRELY made of — one bracket and nothing
/// else, which is the case that becomes a picture on its own row.
pub(super) fn standalone_image(body: &str) -> Option<String> {
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
