//! Scrapbox notation -> renderable blocks. Ported from render.ts.
//! Scrapbox is NOT markdown: structure is per-line, indent = nesting depth.
//!
//! Output is a Vec<Block>: either a styled text Line, a blank line, an image
//! reference (resolved to a real image later), or a table (pre-formatted rows).

mod inline;
use inline::*;
pub use inline::{
    bullet_indent_width, file_name_of_url, gyazo_permalink, indent_info, inline_formulas,
    is_scrapbox_file_url, looks_like_image_url, matching_bracket, strip_leading_ws, text_column,
};

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
    Image {
        url: String,
        indent: usize,
        item: bool,
    },
    /// A line that mixes text and pictures (`本文 [画像]`, `[画像]本文`,
    /// `[A] と [B]`). The viewer lays the parts out left to right with each
    /// picture sitting ON the text line — its bottom edge level with the
    /// words — which is how a browser draws an inline image. Kept as a
    /// SEQUENCE rather than special cases so all three read the same way.
    ///
    /// `item`: the line is an indented list item, so it wears a bullet at
    /// its top-left like every other item at that level.
    Inline {
        indent: usize,
        item: bool,
        parts: Vec<InlinePart>,
    },
    /// A structured table, laid out against the pane width at draw time.
    Table(crate::table::Table),
    /// A code block that is really a PICTURE of something: a Mermaid
    /// diagram, a LaTeX formula. The viewer draws it as text when it can,
    /// falls back to the browser's own rendering where one exists, and to
    /// `rows` — the ordinary highlighted code block — when neither works.
    Artifact {
        kind: ArtifactKind,
        /// The block's own source (the diagram or formula text).
        code: String,
        /// The plain code-block presentation: `(source line, styled line)`
        /// for the `code:` header and every continuation line.
        rows: Vec<(usize, Line<'static>)>,
        /// Source line of the block's LAST content line. Cosense hangs the
        /// preview element off THAT line's id, not the header's (verified
        /// live — see NOTE-webrender-handoff.md).
        last_src: usize,
        /// Display columns the artifact is pushed right. Mermaid previews
        /// may nest two levels, but unlike ordinary list rows they wear no
        /// bullet (matching Cosense web).
        indent: usize,
    },
}

/// What an [`Block::Artifact`] is a picture of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArtifactKind {
    /// `code:mmd` — a Mermaid diagram.
    Mermaid,
    /// `code:tex` — a LaTeX formula.
    Math,
}

impl ArtifactKind {
    /// The browser fallback for this kind, where there is one. Cosense draws
    /// formulas with KaTeX, but the element it hangs them off has not been
    /// pinned down the way `#mermaid-preview-<lineId>` was, so math stops at
    /// text and source rather than screenshotting a guess.
    pub fn web(self) -> Option<crate::webrender::WebKind> {
        match self {
            ArtifactKind::Mermaid => Some(crate::webrender::WebKind::Mermaid),
            ArtifactKind::Math => None,
        }
    }
}

/// One part of a mixed text-and-picture line (see [`Block::Inline`]).
#[derive(Clone, Debug)]
pub enum InlinePart {
    /// A formula too tall to sit inside a text run: `[$ \frac{a}{b} ]`.
    /// Laid out like a picture — it takes columns on the line and rows
    /// above and below the text — so `解は[$ \frac{-b}{2a} ]だ` reads as one
    /// sentence. One-row formulas never get here: they are set in the text
    /// run itself, where wrapping and selection already work.
    Formula {
        /// The LaTeX as written, styled as notation — what a pane too narrow
        /// for the drawing shows instead.
        source: Line<'static>,
        /// The drawn rows, all the same display width.
        rows: Vec<String>,
        /// Which row lines up with the text (see `math::Rendered`).
        baseline: usize,
    },
    Text(Line<'static>),
    Image(String),
}

/// Where a `code:` block sits in the source: its header line and the
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
    /// This lookup points at a Mermaid header itself. Its level is outline
    /// structure — Tab/Shift+Tab moves the whole block — while the body rows
    /// stay ordinary code rows.
    pub mermaid_header: bool,
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

    /// Does the caret line wear a bullet instead of a code gutter? Only a
    /// NESTED Mermaid header does: that is the line whose indent the reader
    /// sees as nesting, so EDIT shows it the way the outline shows it.
    pub fn outline_header(&self) -> bool {
        self.mermaid_header && self.header_indent > 0
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
/// Mirrors the scan in [`render_lines_with`] and the official parser
/// (progfay/scrapbox-parser `packRows`): a block's children are the lines
/// indented DEEPER than the header — a truly empty line has indent 0 and
/// therefore ENDS the block (only a whitespace-only line, which carries
/// the indent, keeps it). The header itself counts as "in" its own block.
pub fn code_span_at(lines: &[&str], i: usize) -> Option<CodeSpan> {
    let mut k = 0;
    while k < lines.len() {
        let (_, header_indent, body) = indent_info(lines[k]);
        if !body.starts_with("code:") || artifact_too_deep(header_indent, body) {
            k += 1;
            continue;
        }
        // Forward scan, exactly as the renderer's block collector: a
        // deeper line continues the block; anything else — a shallower
        // line, or a truly EMPTY one — ends it.
        let mut end = k + 1; // exclusive
        let mut j = k + 1;
        while j < lines.len() {
            let (_, raw_len, _) = indent_info(lines[j]);
            if raw_len > header_indent {
                j += 1;
                end = j; // a real code row: the block reaches at least here
            } else {
                break;
            }
        }
        if i >= k && i < end {
            let mermaid_header = i == k
                && body
                    .strip_prefix("code:")
                    .map(mermaid_lang)
                    .unwrap_or(false);
            return Some(CodeSpan {
                header: k,
                header_indent,
                mermaid_header,
            });
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
/// map per frame rather than one lookup at a time. The HEADER is not
/// flagged: cosense web leaves `• go` unpainted and starts the wash at
/// the body, so the bullet keeps the page's own background.
pub fn code_line_flags(lines: &[&str]) -> Vec<bool> {
    let mut flags = vec![false; lines.len()];
    let mut k = 0;
    while k < lines.len() {
        let (_, header_indent, body) = indent_info(lines[k]);
        if !body.starts_with("code:") || artifact_too_deep(header_indent, body) {
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
            } else {
                break;
            }
        }
        for f in flags.iter_mut().take(end).skip(k + 1) {
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
            return Some(CodeSpan {
                header: k,
                header_indent,
                mermaid_header: false,
            });
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
    lang_is(lang, &["mmd", "mermaid"])
}

/// Is this `code:` block a LaTeX formula? Cosense's help calls out `tex` and
/// `latex`; the same file-name rule as Mermaid applies (`code:eq.tex`).
pub fn math_lang(lang: &str) -> bool {
    lang_is(lang, &["tex", "latex"])
}

/// A `code:` label names one of `names`, either outright (`code:tex`) or as
/// the extension of a file name (`code:proof.tex`).
fn lang_is(lang: &str, names: &[&str]) -> bool {
    let name = lang.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if names.contains(&name.as_str()) {
        return true;
    }
    matches!(name.rsplit_once('.'), Some((_, ext)) if names.contains(&ext))
}

/// Is this `code:` header nested deeper than its kind survives? Past the
/// limit the header and every line under it are ordinary list items, each
/// keeping its own indentation — the block is not even a code block.
fn artifact_too_deep(header_indent: usize, body: &str) -> bool {
    let Some(lang) = body.strip_prefix("code:") else {
        return false;
    };
    match artifact_max_indent(lang) {
        Some(max) => header_indent > max,
        None => false,
    }
}

/// How deep a `code:` block of this kind may nest and still be drawn.
/// `None` for ordinary code, which nests as deep as anyone likes.
fn artifact_max_indent(lang: &str) -> Option<usize> {
    if mermaid_lang(lang) {
        return Some(MERMAID_MAX_INDENT);
    }
    if math_lang(lang) {
        return Some(MATH_MAX_INDENT);
    }
    None
}

/// How deep a Mermaid block may nest and still be drawn as a diagram, which is
/// where Cosense stops too. Deeper than this the whole block reads as list text.
pub const MERMAID_MAX_INDENT: usize = 2;

/// A formula stops at the left margin: the moment a bullet's indent appears,
/// the block is list text, not mathematics.
pub const MATH_MAX_INDENT: usize = 0;

/// A blank run separates two code blocks even when the next `code:` header is
/// more deeply indented. Without this boundary a level-0 Mermaid block absorbs
/// every later level as source, and the duplicate diagram declarations make the
/// whole combined block fail to render.
/// Links and images discovered while rendering, for navigation and prefetch.
#[derive(Debug, Default, Clone)]
pub struct Extracted {
    pub links: Vec<String>,
    pub images: Vec<String>,
}

/// One followable thing on a rendered line: which SPAN of the line it is,
/// and what following it does.
///
/// A span index, not a colour and not a label. The viewer used to work
/// out what had been clicked by comparing the span's foreground against
/// the palette, which meant the click path had to be told about every new
/// colour — and when "uncreated link" got its own colour, red links
/// silently became the one thing on a page a click could not follow. What
/// was drawn is known here, at the moment of drawing; saying so is both
/// cheaper and impossible to get out of step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into the rendered line's `spans`. For a [`Block::Inline`] the
    /// line is the block's `InlinePart::Text` parts read in order, as if
    /// their spans were one line (a picture part has no spans and adds
    /// nothing to the count).
    pub span: usize,
    pub target: HitTarget,
}

/// What a [`Hit`] leads to. Deliberately close to the notation rather
/// than to the viewer's actions: the viewer decides that an uploaded file
/// is downloaded and a page is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// The text inside the brackets of a page link, or a `#tag` without
    /// its `#`. `/project/title` included, verbatim.
    Page(String),
    Url {
        label: String,
        url: String,
    },
    /// A `code:name` / `table:name` header, which the viewer serves as a
    /// file.
    BlockLabel,
}

pub struct RenderOutput {
    pub blocks: Vec<Block>,
    /// Source line index each block was rendered from (parallel to `blocks`).
    /// A table maps to its `table:` line; code continuation lines map to
    /// their own source line. This is the exact block↔source attribution the
    /// viewer uses to place the cursor, anchor comments, and insert cards.
    pub srcs: Vec<usize>,
    pub extracted: Extracted,
    /// What can be followed on each block, by block index (parallel to
    /// `blocks`). Empty for a block that draws nothing followable.
    pub hits: Vec<Vec<Hit>>,
}

use crate::theme::Palette;

/// Cosense's `titleLc`: the form two page titles are compared in. Lower
/// case, and whitespace folded to `_` — `[AI supported coding]` and
/// `[ai_supported_coding]` are one page. Verified against the API's own
/// `titleLc` field.
pub fn title_lc(title: &str) -> String {
    title
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// What is known about the pages a page links to, keyed by `title_lc`.
///
/// Three states, and the third one is the point: **exists**, **does not
/// exist**, and **not known yet**. Only the second is painted. A link
/// nobody has asked about yet keeps the ordinary colour until an answer
/// arrives, because the alternative — guessing — is how an existing page
/// ends up wearing the colour that says it was never written.
///
/// Answers come from two places: the page response itself (its `links`
/// against `relatedPages.links1hop`, which is free), and, for anything
/// that response never mentioned — above all a link just typed — a
/// background lookup of that one title.
#[derive(Debug, Default, Clone)]
pub struct LinkTruth {
    known: std::collections::HashMap<String, bool>,
}

impl LinkTruth {
    /// Read a page response: `links` = every link the page has, `existing`
    /// = titles that response showed to exist (1-hop neighbours, and the
    /// page itself). Everything in `links` that is not among them was
    /// never written.
    pub fn seed<'a>(
        links: impl IntoIterator<Item = &'a str>,
        existing: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut known: std::collections::HashMap<String, bool> =
            existing.into_iter().map(|t| (title_lc(t), true)).collect();
        for link in links {
            known.entry(title_lc(link)).or_insert(false);
        }
        Self { known }
    }

    /// Record one answer. A later answer wins: pages are written and
    /// deleted while the viewer is open.
    pub fn learn(&mut self, title: &str, exists: bool) {
        self.known.insert(title_lc(title), exists);
    }

    /// Fold another set of answers in, letting `other` win — it is the
    /// fresher reading (a page install over what the session has learned).
    pub fn absorb(&mut self, other: LinkTruth) {
        self.known.extend(other.known);
    }

    /// Forget every "nobody says this word" before reading another page.
    ///
    /// The two answers do not travel equally. **Live travels**: a written
    /// page stays written, and a word two pages share is shared wherever
    /// you read it — the page that made it live is not the page asking,
    /// or the asking page would not have needed to ask. **Dead does not**:
    /// it means "nobody BUT the page that asked writes this word", and the
    /// next page you open may be the second one writing it. Carrying that
    /// answer over is exactly how a tag stays red on the page that just
    /// made it a shared one.
    pub fn forget_dead(&mut self) {
        self.known.retain(|_, live| *live);
    }

    /// `None` = nobody has looked yet.
    pub fn exists(&self, title: &str) -> Option<bool> {
        if self.known.is_empty() {
            return None; // do not fold a string when nothing is known
        }
        self.known.get(&title_lc(title)).copied()
    }

    /// The one state worth colouring.
    pub fn missing(&self, title: &str) -> bool {
        self.exists(title) == Some(false)
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// Render with defaults (dark palette, no syntax highlighting).
pub fn render_lines(lines: &[String]) -> RenderOutput {
    let pal = Palette::for_light(false);
    render_lines_with(lines, None, &pal, &LinkTruth::default())
}

/// Render, optionally syntax-highlighting code blocks with `hl`, using `pal`
/// for construct colors (heading levels, links, etc.).
pub fn render_lines_with(
    lines: &[String],
    hl: Option<&crate::highlight::Highlighter>,
    pal: &Palette,
    known: &LinkTruth,
) -> RenderOutput {
    let mut out: Vec<Block> = Vec::new();
    let mut srcs: Vec<usize> = Vec::new();
    let mut all_hits: Vec<Vec<Hit>> = Vec::new();
    let mut ex = Extracted::default();
    let mut i = 0;
    // Filled by the decorating pass for the line being built, then handed
    // to the block it belongs to. `emit!` shifts it by the spans the block
    // puts in FRONT of the decorated content (indent, bullet, quote bar),
    // which is the only place those prefixes are known.
    let mut hits: Vec<Hit> = Vec::new();

    // Every block records the source line it came from (see RenderOutput.srcs).
    macro_rules! emit {
        ($block:expr) => {{
            emit!($block, 0)
        }};
        ($block:expr, $shift:expr) => {{
            out.push($block);
            srcs.push(i);
            let shift: usize = $shift;
            all_hits.push(
                std::mem::take(&mut hits)
                    .into_iter()
                    .map(|h| Hit {
                        span: h.span + shift,
                        ..h
                    })
                    .collect(),
            );
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
            spans.push(Span::styled(
                "•".to_string(),
                Style::default().fg(pal.bullet),
            ));
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
                rows.push((
                    j,
                    rrest
                        .split('\t')
                        .map(|c| c.trim_end().to_string())
                        .collect(),
                ));
                j += 1;
            }
            // decorate cells to styled spans (links stay navigable via extraction)
            let styled_rows: Vec<(usize, Vec<Vec<Span<'static>>>)> = rows
                .iter()
                .map(|(src, r)| {
                    (
                        *src,
                        r.iter()
                            .map(|c| {
                                decorate_inline(
                                    c,
                                    &mut ex.links,
                                    &mut ex.images,
                                    pal,
                                    known,
                                    &mut hits,
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            // Blank-only leading indent is dropped: Scrapbox tables render
            // flush; nesting is rare and the width-adaptive layout owns
            // horizontal budget. Keep the indent as a note for the future.
            let _ = &indent;
            emit!(Block::Table(crate::table::Table {
                name,
                name_src: i,
                rows: styled_rows,
                name_style: style_block_label(pal),
            }));
            i = j;
            continue;
        }

        // code block: `code:name.ext` header + indented continuation lines.
        // Mermaid nested at level 3+ is deliberately NOT consumed here:
        // Cosense treats its header and body as independent list rows.
        if let Some(rest) = body
            .strip_prefix("code:")
            .filter(|_| !artifact_too_deep(raw_len, body))
        {
            let lang = rest.trim().to_string();
            let code_indent = raw_len;
            // Cosense serves a code block as a file, so the header line is
            // something to follow: `Enter` saves it. That is said with the
            // underline every followable row wears — one signal for "you
            // can press Enter here", not a glyph per kind.
            // A nested block wears the list's own bullet on its header
            // (cosense web: `• go`), while the body rows stay bare — the
            // bullet says where the block hangs, the wash says what it is.
            let mut header_spans = vec![Span::raw(indent.clone())];
            if level > 0 {
                header_spans.push(Span::styled(
                    "• ".to_string(),
                    Style::default().fg(pal.bullet),
                ));
            }
            header_spans.push(Span::styled(format!("code:{lang}"), style_block_label(pal)));
            let header = Line::from(header_spans);
            // A diagram or a formula is collected whole and drawn as one
            // artifact; everything else emits its header row right away.
            let artifact = if mermaid_lang(&lang) {
                Some(ArtifactKind::Mermaid)
            } else if math_lang(&lang) {
                Some(ArtifactKind::Math)
            } else {
                None
            };
            if artifact.is_none() {
                hits.push(Hit {
                    span: 0,
                    target: HitTarget::BlockLabel,
                });
                // Past the indent and, when nested, the bullet: the hit
                // has to land on the `code:` label itself.
                emit!(Block::Text(header.clone()), if level > 0 { 2 } else { 1 });
            }
            // collect continuation lines: deeper indent ONLY. A truly
            // empty line has indent 0 and ends the block, exactly as the
            // official parser packs rows (a whitespace-only line still
            // carries the indent, so it stays — the indent is the
            // membership, here as everywhere: deleting it is what leaves
            // the block, which is how web ends one).
            let mut j = i + 1;
            let mut raws: Vec<usize> = Vec::new(); // source indices
            let mut bodies: Vec<String> = Vec::new();
            while j < lines.len() {
                let (_, rl, _) = indent_info(&lines[j]);
                if rl > code_indent {
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
            // The code rows, styled exactly as they always were.
            let mut code_rows: Vec<(usize, Line<'static>)> = Vec::new();
            match hl {
                Some(h) => {
                    let content = bodies.join("\n");
                    let mut highlighted = h.highlight(&content, &lang);
                    // A block that ENDS on an empty body (a blank line the
                    // writer typed inside) joins into a string ending in
                    // `\n`, and the highlighter's `lines()` drops that tail.
                    // Pad it back: every body line keeps its row and its
                    // source attribution — the blank line the caret sits on
                    // has to be a row, or the edit session's "this block is
                    // being edited" check misses it and the diagram never
                    // makes way for its source.
                    while highlighted.len() < bodies.len() {
                        highlighted.push(Vec::new());
                    }
                    for (k, spans) in highlighted.into_iter().enumerate() {
                        let src = raws.get(k).copied().unwrap_or(i);
                        let mut line_spans: Vec<Span<'static>> =
                            vec![Span::raw(format!("{indent}  "))];
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
            if let Some(kind) = artifact {
                // Cosense attaches the preview to the block's LAST content
                // line. With no content there is nothing to draw, so such a
                // block stays an ordinary (empty) code block.
                match raws.last().copied() {
                    Some(last_src) => {
                        let mut rows = vec![(i, header)];
                        rows.extend(code_rows);
                        emit!(Block::Artifact {
                            kind,
                            code: bodies.join("\n"),
                            rows,
                            last_src,
                            indent: text_column(level),
                        });
                    }
                    None => {
                        hits.push(Hit {
                            span: 0,
                            target: HitTarget::BlockLabel,
                        });
                        emit!(Block::Text(header), 1)
                    }
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
            emit!(Block::Image {
                url,
                indent: text_column(level),
                item: level > 0
            });
            i += 1;
            continue;
        }
        // A line that mixes text with something taller than text: pictures,
        // and formulas that need more than one row. One sequence, laid out
        // like a browser lays out inline images. Asked FIRST whether there
        // is such a thing at all, because splitting the line also collects
        // its links — doing that speculatively would count them twice.
        if !line_images(body).is_empty() || has_tall_formula(body) {
            // The hits of a mixed text-and-picture line count the spans of
            // its text parts in order (see `Hit::span`); the viewer lays the
            // parts out itself and maps a clicked cell back to one of those
            // spans (`App::link_at_screen_position`). No shift: the indent
            // and bullet are drawn by the viewer, not carried as spans.
            let parts = inline_parts(body, &mut ex.links, &mut ex.images, pal, known, &mut hits);
            emit!(Block::Inline {
                indent: text_column(level),
                item: level > 0,
                parts
            });
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
            for s in decorate_inline(
                q.trim(),
                &mut ex.links,
                &mut ex.images,
                pal,
                known,
                &mut hits,
            ) {
                let styled = s.style.add_modifier(Modifier::ITALIC);
                spans.push(Span::styled(s.content.into_owned(), styled));
            }
            emit!(Block::Text(Line::from(spans)), 2);
            i += 1;
            continue;
        }

        // Section heading: `[* ...]` may ALSO be indented in Cosense.
        // Markdown would make these competing block types; Cosense composes
        // them. Keep the positional bullet in the bullet style and apply the
        // theme's heading style only to the heading text.
        if let Some(heading) =
            parse_heading(body, pal, known, &mut hits, &mut ex.links, &mut ex.images)
        {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if level > 0 {
                spans.push(Span::raw(indent.clone()));
                spans.push(Span::styled(
                    "• ".to_string(),
                    Style::default().fg(pal.bullet),
                ));
            }
            spans.extend(heading);
            let prefix = if level > 0 { 2 } else { 0 };
            emit!(Block::Text(Line::from(spans)), prefix);
            i += 1;
            continue;
        }

        // bullet / plain
        let content = decorate_inline(body, &mut ex.links, &mut ex.images, pal, known, &mut hits);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if level > 0 {
            spans.push(Span::raw(indent.clone()));
            spans.push(Span::styled(
                "• ".to_string(),
                Style::default().fg(pal.bullet),
            ));
        }
        spans.extend(content);
        let prefix = if level > 0 { 2 } else { 0 };
        emit!(Block::Text(Line::from(spans)), prefix);
        i += 1;
    }

    RenderOutput {
        blocks: out,
        srcs,
        extracted: ex,
        hits: all_hits,
    }
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
    known: &LinkTruth,
    hits: &mut Vec<Hit>,
    links: &mut Vec<String>,
    images: &mut Vec<String>,
) -> Option<Vec<Span<'static>>> {
    let close = matching_bracket(body.trim_end(), body.find('[')?)?;
    if close + 1 != body.trim_end().len() {
        return None; // something follows the bracket: not a heading LINE
    }
    let inner = &body.trim_end()[body.find('[')? + 1..close];
    let stars = inner
        .char_indices()
        .find(|(_, c)| *c != '*')
        .map(|(i, _)| i)?;
    if stars == 0 {
        return None;
    }
    let text = inner[stars..].strip_prefix(' ')?;
    // A heading is still ordinary notation inside: `[* [page]]` has to
    // stay a followable link, not become the letters of one.
    let style = star_style(stars, pal);
    Some(
        decorate_inline(text, links, images, pal, known, hits)
            .into_iter()
            .map(|sp| Span::styled(sp.content, style.patch(sp.style)))
            .collect(),
    )
}

#[cfg(test)]
mod tests;
