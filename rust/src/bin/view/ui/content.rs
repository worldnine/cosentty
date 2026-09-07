//! Page rows, source projection, wrapping and cached layout.

use super::*;

impl App {
    /// Columns available to the text for a `width`-wide body in `mode`:
    /// frame/caret + telomere + one blank column on the left, and a blank,
    /// scrollbar, and frame column on the right. Source mode also spends
    /// the line-number label.
    pub(crate) fn text_width(mode: Mode, width: u16) -> usize {
        let w = width as usize;
        match mode {
            Mode::View => w.saturating_sub(6).max(1),
            Mode::Source => w.saturating_sub(6 + SOURCE_NUM_W).max(1),
        }
    }

    /// View mode: rendered blocks wrapped to the pane, each tagged with the
    /// source line it came from. The edit session's caret line renders as
    /// RAW SOURCE (cosense web: the line with the caret reveals its
    /// notation; everything else stays rendered).
    pub(crate) fn content_view(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::View, width);
        let edit: Option<(usize, &str)> = self
            .session
            .as_ref()
            .map(|s| (s.line, s.input.buf.as_str()));
        // Inside a `code:` block the caret line is code, not an outline
        // row: its leading whitespace is content and must not be drawn as
        // a bullet. Judged on the WORKING text, so typing `code:` turns
        // the bullets off as you type it, not one commit later.
        let edit_code = edit.and_then(|(line, _)| caret_span(self, line));
        // The character selection, in DISPLAY byte offsets — the caret line
        // is drawn through `session_display`, so the span has to be mapped
        // the same way the caret is.
        // The character selection, in DISPLAY byte offsets — the caret
        // line is drawn through `session_display`, so the span has to be
        // mapped the same way the caret is. A range across lines reaches
        // the caret line to its near edge; that is what `caret_sel_bytes`
        // answers even when the range does not fit on the line.
        let sel_disp: Option<(usize, usize)> = self.session.as_ref().and_then(|s| {
            let (a, b) = s.caret_sel_bytes()?;
            Some((
                display_caret(&s.input.buf, a, edit_code),
                display_caret(&s.input.buf, b, edit_code),
            ))
        });
        let raw_rows = |content: &mut Vec<Row>, buf: &str, src: usize| {
            // The indent renders as its bullet (dim) — same shape as the
            // view — while the underlying data stays whitespace. A nested
            // Mermaid header is the one place inside a code block where that
            // still holds: its indent IS the diagram's nesting level.
            let outline = edit_code.is_none_or(|span| span.outline_header());
            let disp = session_display(buf, edit_code);
            let prefix = if outline {
                display_prefix_bytes(buf)
            } else {
                0
            };
            let wrapped = SessionWrap::new(&disp, text_w, session_hang(buf, edit_code));
            let mut at = 0usize; // byte offset of this segment within `disp`
            for (k, seg) in wrapped.segs.iter().cloned().enumerate() {
                let seg_start = at;
                at += seg.len();
                let mut spans: Vec<Span<'static>> = Vec::new();
                if wrapped.indent_of(k) > 0 {
                    // The continuation hangs under the text, not under the
                    // bullet — the same shape READ has always had.
                    spans.push(Span::raw(" ".repeat(wrapped.indent_of(k))));
                }
                let mut push = |text: &str, selected: bool, dim: bool| {
                    if text.is_empty() {
                        return;
                    }
                    let mut st = Style::default();
                    if dim {
                        st = st.fg(Color::DarkGray);
                    }
                    if selected {
                        // REVERSED, not a background colour: the caret line
                        // is already painted with the cursor band, and a
                        // band-coloured selection is invisible exactly where
                        // it always is (the caret's own line).
                        // Swapping fg/bg contrasts against any background,
                        // on any terminal, without inventing a colour.
                        st = st.add_modifier(Modifier::REVERSED);
                    }
                    spans.push(Span::styled(text.to_string(), st));
                };
                // Cut this segment into [before | selected | after], with
                // the bullet prefix (first row only) staying dim.
                let (lo, hi) = match sel_disp {
                    Some((a, b)) => (
                        a.saturating_sub(seg_start).min(seg.len()),
                        b.saturating_sub(seg_start).min(seg.len()),
                    ),
                    None => (seg.len(), seg.len()),
                };
                let (lo, hi) = (floor_boundary(&seg, lo), floor_boundary(&seg, hi));
                let dim_to = if k == 0 { prefix.min(seg.len()) } else { 0 };
                for (from, to, selected) in [(0, lo, false), (lo, hi, true), (hi, seg.len(), false)]
                {
                    if from >= to {
                        continue;
                    }
                    // The dim prefix may end inside this piece.
                    let cut = dim_to.clamp(from, to);
                    push(&seg[from..cut], selected, true);
                    push(&seg[cut..to], selected, false);
                }
                content.push(Row::Line {
                    line: Line::from(spans),
                    src,
                    start: 0,
                    hang: 0,
                });
            }
        };
        let mut content: Vec<Row> = Vec::new();
        for (b, &src) in self.blocks.iter().zip(self.srcs.iter()) {
            if let Some((eline, ebuf)) = edit {
                if src == eline && !matches!(b, Block::Table(_) | Block::Artifact { .. }) {
                    raw_rows(&mut content, ebuf, src);
                    // The line's inline formulas, drawn under it (cosense
                    // web previews them while the line is being edited).
                    // Same chrome as the block preview: a dim bar on every
                    // row, a label saying what it is — the caret line shows
                    // RAW notation, so even a one-row formula loses its
                    // rendering while edited, and all of them preview.
                    // Only OUTSIDE code: inside a code block `[$ ]` is
                    // literal text, and the notation bargain says so.
                    if edit_code.is_none() {
                        let latexes = cosense::render::inline_formulas(ebuf);
                        if let Some(preview) =
                            cosense::math::preview_line(&latexes).filter(|p| p.width <= text_w)
                        {
                            let dim = Style::default().fg(Color::DarkGray);
                            content.push(Row::Aside {
                                line: Line::from(vec![Span::styled(
                                    format!("▏ {}", t!("プレビュー", "preview")),
                                    dim,
                                )]),
                            });
                            for row in &preview.rows {
                                content.push(Row::Aside {
                                    line: Line::from(vec![
                                        Span::styled("▏ ".to_string(), dim),
                                        Span::raw(row.clone()),
                                    ]),
                                });
                            }
                        }
                    }
                    continue;
                }
            }
            match b {
                Block::Text(line) => {
                    // bullets / code hang their continuation rows under the
                    // text; quotes repeat their bar on every row
                    for w in wrap_line_parts(line, text_w, &hanging_prefix(line)) {
                        content.push(Row::Line {
                            line: w.line,
                            src,
                            start: w.start,
                            hang: w.hang,
                        });
                    }
                }
                Block::Blank => content.push(Row::Blank { src }),
                Block::Table(t) => {
                    // Table rows carry their OWN source lines (one Scrapbox
                    // line per row), so the cursor addresses rows directly.
                    // The session's caret row swaps to raw source once.
                    let mut emitted_raw = false;
                    for (line, row_src) in t.layout(text_w) {
                        if let Some((eline, ebuf)) = edit {
                            if row_src == eline {
                                if !emitted_raw {
                                    raw_rows(&mut content, ebuf, row_src);
                                    emitted_raw = true;
                                }
                                continue;
                            }
                        }
                        content.push(Row::Line {
                            line,
                            src: row_src,
                            start: 0,
                            hang: 0,
                        });
                    }
                }
                Block::Artifact {
                    kind,
                    code,
                    rows,
                    last_src,
                    indent,
                } => {
                    let key = kind
                        .web()
                        .and_then(|k| self.web_request(k, code, *last_src))
                        .map(|r| r.cache_key());
                    // While the edit session is inside this block the reader
                    // is working on the raw source, so the picture steps
                    // aside — the existing source-editing contract wins.
                    let editing_here = edit
                        .map(|(eline, _)| rows.iter().any(|(rsrc, _)| *rsrc == eline))
                        .unwrap_or(false);
                    // lib 検証 spike のテキスト段。編集中は素のソース契約が勝つ。
                    // level 1–2は図全体だけを右へ送り、箇条書き記号は描かない。
                    // 描画幅もその分だけ絞る。
                    let text_tier = match kind {
                        ArtifactKind::Mermaid => self.mermaid_text,
                        ArtifactKind::Math => self.math_text,
                    };
                    // 幅不足で描けなかったときだけ理由を持ち越し、コード行の
                    // 上に注記を出す(画像に落ちた場合は注記しない)。
                    let mut too_narrow: Option<usize> = None;
                    if !editing_here && text_tier {
                        let draw_w = text_w.saturating_sub(*indent);
                        let drawn = match kind {
                            ArtifactKind::Mermaid => {
                                match mmd_text::render_text_outcome(code, draw_w) {
                                    mmd_text::TextOutcome::Drawn(lines) => Some(lines),
                                    mmd_text::TextOutcome::TooNarrow { needed } => {
                                        too_narrow = Some(needed);
                                        None
                                    }
                                    mmd_text::TextOutcome::Declined => None,
                                }
                            }
                            ArtifactKind::Math => cosense::math::render_text(code, draw_w),
                        };
                        if let Some(lines) = drawn {
                            for text in lines {
                                let line = drawn_line(&text, *indent, *kind);
                                for w in wrap_line_parts(&line, text_w, &hanging_prefix(&line)) {
                                    content.push(Row::Line {
                                        line: w.line,
                                        src: *last_src,
                                        start: w.start,
                                        hang: w.hang,
                                    });
                                }
                            }
                            continue;
                        }
                    }
                    let art = if editing_here {
                        None
                    } else {
                        key.as_ref()
                            .and_then(|k| self.images.get(k).map(|i| (k, i)))
                    };
                    if let Some((k, info)) = art {
                        content.push(Row::Image {
                            url: k.clone(),
                            height: info.cells_h.max(1),
                            src: *last_src,
                            indent: *indent,
                            item: false,
                        });
                        continue;
                    }
                    // No artifact (yet, or ever): the plain code block, with
                    // the session's caret row swapped to raw source.
                    if let Some(needed) = too_narrow {
                        let have = text_w.saturating_sub(*indent);
                        content.push(Row::Aside {
                            line: Line::from(vec![
                                Span::raw(" ".repeat(*indent)),
                                Span::styled(
                                    format!(
                                        "▏ {}",
                                        t!(
                                            "図はペイン幅に入らないためソースを表示(必要 {} 桁 / 幅 {} 桁)",
                                            "diagram wider than the pane, showing source (needs {} cols / have {})",
                                            needed,
                                            have
                                        )
                                    ),
                                    Style::default().fg(Color::DarkGray),
                                ),
                            ]),
                        });
                    }
                    for (rsrc, line) in rows {
                        if let Some((eline, ebuf)) = edit {
                            if *rsrc == eline {
                                raw_rows(&mut content, ebuf, *rsrc);
                                continue;
                            }
                        }
                        let line = line.clone();
                        for w in wrap_line_parts(&line, text_w, &hanging_prefix(&line)) {
                            content.push(Row::Line {
                                line: w.line,
                                src: *rsrc,
                                start: w.start,
                                hang: w.hang,
                            });
                        }
                    }
                    // The live preview (cosense web does the same): while the
                    // session is inside the block the reader is typing the
                    // SOURCE, and the web draws the formula/diagram it makes
                    // under it, updating as they type. Same here — the text
                    // tier over the CURRENT source, caret buffer included
                    // (source_texts swaps it in). A dim bar on every row says
                    // "attached commentary, not page content"; what cannot be
                    // set (unknown syntax, too wide) shows nothing, since the
                    // source is already on screen.
                    if let Some(code) = edit.and_then(|_| {
                        let first = rows.first().map(|(s, _)| *s)?;
                        let base = indent_of(self.lines.get(first)?.text.as_str())
                            .chars()
                            .count();
                        let texts = self.source_texts();
                        let mut bodies = Vec::new();
                        for i in first + 1..=*last_src {
                            bodies.push(cosense::render::strip_leading_ws(texts.get(i)?, base + 1));
                        }
                        Some(bodies.join("\n"))
                    }) {
                        let w = text_w.saturating_sub(*indent + 2);
                        let drawn = match kind {
                            ArtifactKind::Mermaid => mmd_text::render_text(&code, w),
                            ArtifactKind::Math => cosense::math::render_text(&code, w),
                        };
                        if let Some(lines) = drawn {
                            let dim = Style::default().fg(Color::DarkGray);
                            let pad = " ".repeat(*indent);
                            let bar = "▏ ";
                            // The rows are `Aside`: chrome attached to the
                            // block, not page content — no cursor band (the
                            // caret's runway ends with the source), no
                            // cursor addressing, no selection. The reader
                            // reads the preview; the cursor never stands on
                            // it.
                            content.push(Row::Aside {
                                line: Line::from(vec![
                                    Span::raw(pad.clone()),
                                    Span::styled(
                                        format!("{bar}{}", t!("プレビュー", "preview")),
                                        dim,
                                    ),
                                ]),
                            });
                            for text in lines {
                                let mut spans = vec![
                                    Span::raw(pad.clone()),
                                    Span::styled(bar.to_string(), dim),
                                ];
                                spans.extend(drawn_line(&text, 0, *kind).spans);
                                content.push(Row::Aside {
                                    line: Line::from(spans),
                                });
                            }
                        }
                    }
                }
                Block::Inline {
                    indent,
                    item,
                    parts,
                } => {
                    let indent = (*indent).min(text_w.saturating_sub(4));
                    content.push(self.inline_row(parts, indent, *item, text_w, src));
                }
                Block::Image { url, indent, item } => {
                    let indent = (*indent).min(text_w.saturating_sub(4));
                    if let Some(info) = self.images.get(url) {
                        content.push(Row::Image {
                            url: url.clone(),
                            height: info.cells_h.max(1),
                            src,
                            indent,
                            item: *item,
                        });
                    } else if let Some(msg) = self.image_errors.get(url) {
                        content.push(Row::ImageError {
                            msg: msg.clone(),
                            src,
                            indent,
                            item: *item,
                        });
                    } else {
                        // still downloading — reserve space so the page is
                        // readable now and the image slots in when it lands
                        content.push(Row::ImageLoading {
                            src,
                            indent,
                            item: *item,
                            url: url.clone(),
                        });
                    }
                }
            }
        }
        content
    }

    /// Lay a mixed text-and-picture line out for this pane. A picture the
    /// viewer has not decoded yet still takes part: it reserves a
    /// placeholder-sized box so the line does not jump when it lands.
    pub(crate) fn inline_row(
        &self,
        parts: &[cosense::render::InlinePart],
        indent: usize,
        item: bool,
        text_w: usize,
        src: usize,
    ) -> Row {
        use cosense::render::InlinePart;
        let body = (text_w.saturating_sub(indent)).max(1) as u16;
        // The box a picture gets: its real size once decoded, the
        // placeholder's while it is still coming. Clamped to the line here so
        // the size the layout used is the size the draw sees reserved — the
        // notation of a waiting picture may not paint into its neighbour's
        // columns.
        let reserved = |url: &str| -> (u16, u16) {
            match self.images.get(url) {
                Some(info) => (info.cells_w.min(body), info.cells_h.max(1)),
                None => (IMAGE_PLACEHOLDER_W.min(body), IMAGE_PLACEHOLDER_H),
            }
        };
        let items: Vec<Inline> = parts
            .iter()
            .map(|p| match p {
                InlinePart::Text(line) => Inline::Text(line.clone()),
                InlinePart::Image(url) => {
                    let (w, h) = reserved(url);
                    Inline::Image {
                        url: url.clone(),
                        w,
                        h,
                    }
                }
                InlinePart::Formula {
                    source,
                    rows,
                    baseline,
                } => {
                    // A formula cannot be wrapped — the two-dimensional
                    // setting is the meaning — so in a pane too narrow to
                    // hold it the LaTeX goes back, which wraps like prose.
                    let f = inline_formula(rows, *baseline);
                    match &f {
                        Inline::Formula { w, .. } if *w <= body => f,
                        _ => Inline::Text(source.clone()),
                    }
                }
            })
            .collect();
        let (images, texts, height) = layout_inline(&items, indent, text_w);
        Row::Inline {
            src,
            height,
            indent,
            item,
            images: images
                .into_iter()
                .map(|p| {
                    let (w, h) = reserved(&p.what);
                    (p.row, p.col, w, h, p.what)
                })
                .collect(),
            texts: texts.into_iter().map(|p| (p.row, p.col, p.what)).collect(),
        }
    }

    /// Build the related-pages sections that live BELOW and OUTSIDE the page    /// Build the related-pages sections that live BELOW and OUTSIDE the page
    /// frame. Rows remain cursor-addressable virtual lines; only the visual
    /// containment changes.
    pub(crate) fn related_rows(&self, text_w: usize) -> Vec<Row> {
        if self.related.is_empty() {
            return Vec::new();
        }
        let dim = Style::default().fg(CHROME_DIM);
        // Neither underlined nor coloured for being links. That rule is
        // for finding the pressable thing IN A LINE OF PROSE; these rows
        // are nothing but links, one per line, under a heading that says
        // so. With link-ness needing no mark, the colour is free to carry
        // the one thing that differs between rows — whether the page has
        // been seen since it changed — in the same blue the telomere uses
        // for it. Same reading as the index's list.
        let unread_style = Style::default().fg(CHROME_CARET);
        let read_style = Style::default();
        let mut vsrc = self.lines.len();
        // The one-line gap before the sections is where the standing sort
        // says itself: the index's `s` choice reaches here too, and `S`
        // re-orders from this side.
        let arrow = if self.index_sort == cosense::index::SortKey::Title {
            "↑"
        } else {
            "↓"
        };
        let mut rows = vec![Row::Aside {
            line: Line::from(Span::styled(
                format!(
                    " links · {} {} · {}",
                    self.index_sort.name(),
                    arrow,
                    t!("S で並べ替え", "S re-orders")
                ),
                dim,
            )),
        }];
        for sec in &self.related {
            let head = format!("── {} ", sec.heading);
            let used = str_width(&head);
            let fill = "─".repeat(text_w.saturating_sub(used));
            rows.push(Row::Aside {
                line: Line::from(Span::styled(format!("{head}{fill}"), dim)),
            });
            for e in &sec.entries {
                let style = if e.unread { unread_style } else { read_style };
                rows.push(Row::Line {
                    line: related_row(e, text_w, style, self.index_sort),
                    src: vsrc,
                    start: 0,
                    hang: 0,
                });
                vsrc += 1;
            }
            rows.push(Row::Aside {
                line: Line::from(""),
            });
        }
        rows
    }

    /// Source mode: the raw Scrapbox notation, one numbered row per source
    /// line (wrapped). Same `src` tagging as view mode, so the cursor,
    /// comments and telomere all line up across the toggle.
    pub(crate) fn content_source(&self, width: u16) -> Vec<Row> {
        let text_w = Self::text_width(Mode::Source, width);
        let num_style = Style::default().fg(Color::DarkGray);
        let mut content: Vec<Row> = Vec::new();
        for (src, l) in self.lines.iter().enumerate() {
            let raw = Line::from(l.text.clone());
            for (k, wrapped) in wrap_line(&raw, text_w).into_iter().enumerate() {
                // number only on a line's first display row
                let label = if k == 0 {
                    format!("{:>4} ", src + 1)
                } else {
                    " ".repeat(SOURCE_NUM_W)
                };
                let mut spans: Vec<Span<'static>> = vec![Span::styled(label, num_style)];
                spans.extend(wrapped.spans);
                content.push(Row::Line {
                    line: Line::from(spans),
                    src,
                    start: 0,
                    hang: 0,
                });
            }
        }
        content
    }

    /// Rebuild the visual rows for `width`: content rows (with source
    /// attribution) plus inline comment cards inserted after each comment's
    /// anchor block. Called on resize and whenever comments change.
    pub(crate) fn rebuild(&mut self, width: u16) {
        // Diagrams must fit the pane: the draw clips to the text column, so
        // an image encoded wider than the pane loses its right-hand side.
        // Recorded here (the layout is the only place the width is known)
        // and acted on by `rescale_diagrams`.
        self.web_cols = diagram_max_cols(Self::text_width(self.mode, width) as u16);
        // 1. Page rows and related rows are separate visual regions. The
        // page is boxed; related sections are appended after its FrameEnd.
        // The edit session (and source mode) hides the related sections:
        // the caret line is raw source and the region below the frame is
        // pure page content.
        let (content, related): (Vec<Row>, Vec<Row>) = match self.mode {
            Mode::View => {
                let text_w = Self::text_width(Mode::View, width);
                let related = if self.session.is_none() {
                    self.related_rows(text_w)
                } else {
                    Vec::new()
                };
                (self.content_view(width), related)
            }
            Mode::Source => (self.content_source(width), Vec::new()),
        };

        // 2. compute each comment's anchor: the last content index whose src
        //    lies in the comment's range (so the card sits under its block).
        let mut cards_after: HashMap<usize, Vec<usize>> = HashMap::new(); // content idx -> comment idxs
        for (ci, c) in self.comments.iter().enumerate() {
            // Only weave cards for comments that belong to THIS page, on
            // the revision being shown.
            if !self.comment_is_shown(c) {
                continue;
            }
            let mut anchor: Option<usize> = None;
            for (idx, row) in content.iter().enumerate() {
                if let Some(s) = row.src() {
                    if c.start <= s && s <= c.end {
                        anchor = Some(idx);
                    }
                }
            }
            let key = anchor.unwrap_or(content.len().saturating_sub(1));
            cards_after.entry(key).or_default().push(ci);
        }

        // 3. weave cards in (cards span the text column, not the gutters)
        let mut rows: Vec<Row> = Vec::new();
        // One wider than the text: the block starts a column to the left
        // of it (see `bar_row` in the drawer).
        let width_cols = 1 + match self.mode {
            Mode::View => Self::text_width(Mode::View, width),
            // source rows carry their number label inline, so the card
            // spans label + text
            Mode::Source => Self::text_width(Mode::Source, width) + SOURCE_NUM_W,
        };
        // The composer opens where its comment will sit: under the last
        // content row of the range being commented. An existing comment
        // on exactly that range is being edited, so its card gives way to
        // the bar (one bar, not two stacked).
        let mut composer: Option<(usize, Vec<Row>)> = self.composing.as_ref().map(|input| {
            let range = self
                .selection
                .map(|s| s.range())
                .unwrap_or((self.cursor, self.cursor));
            let anchor = content
                .iter()
                .enumerate()
                .filter(|(_, row)| row.src().is_some_and(|s| range.0 <= s && s <= range.1))
                .map(|(idx, _)| idx)
                .last()
                .unwrap_or(content.len().saturating_sub(1));
            let revision = self
                .shown_revision()
                .map(|r| cosense::theme::format_local(r.created));
            // The body may take the pane minus the badge row, the band
            // row, and one row of the page for context; at least one row.
            // Before the first frame the height is not known (0): no cap.
            let max_body = match self.view_h {
                0 => usize::MAX,
                h => (h as usize).saturating_sub(3).max(1),
            };
            (
                anchor,
                composer_rows(
                    input,
                    range,
                    self.comment_for_range().is_some(),
                    revision,
                    width_cols,
                    max_body,
                ),
            )
        });
        let hidden_card = if self.composing.is_some() {
            self.comment_for_range()
        } else {
            None
        };
        for (idx, row) in content.into_iter().enumerate() {
            rows.push(row);
            if let Some(cidxs) = cards_after.get(&idx) {
                for &ci in cidxs {
                    if Some(ci) == hidden_card {
                        continue;
                    }
                    for line in card_lines(&self.comments[ci], width_cols) {
                        rows.push(Row::Card { line });
                    }
                }
            }
            if composer.as_ref().is_some_and(|(anchor, _)| *anchor == idx) {
                let (_, bar) = composer.take().unwrap();
                rows.extend(bar);
            }
        }
        // Painted as └───┘ by `ui`; related rows begin after it.
        rows.push(Row::FrameEnd);
        rows.extend(related);
        self.rows = rows;
        self.web_shimmer = self.web_shimmer_rows();
        self.laid_width = width;
        self.clamp_cursor();
    }

    /// If `src` lies inside a diagram block that is currently showing its
    /// picture, the source line that owns that picture's row.
    pub(crate) fn diagram_row_owner(&self, src: usize) -> Option<usize> {
        self.blocks.iter().find_map(|b| {
            let Block::Artifact { rows, last_src, .. } = b else {
                return None;
            };
            if !rows.iter().any(|(rsrc, _)| *rsrc == src) {
                return None;
            }
            // Only when the block really is a picture right now: an undrawn
            // one renders its source lines normally, and they own rows.
            self.src_rows(*last_src).map(|_| *last_src)
        })
    }

    /// The display rows occupied by source line `src`, as an inclusive
    /// `(first, last)` row-index range — `None` if it renders to nothing.
    /// Card rows woven in between count toward the range (like akapen's
    /// `source_starts[i]..source_starts[i+1]` span including comment cards),
    /// so following the cursor keeps the line's card on screen too.
    pub(crate) fn src_rows(&self, src: usize) -> Option<(usize, usize)> {
        let first = self.rows.iter().position(|r| r.src() == Some(src))?;
        let last = self.rows.iter().rposition(|r| r.src() == Some(src))?;
        Some((first, last))
    }

    /// Visual row at `screen_row` (0-based inside the text area), accounting
    /// for scrolling and multi-row images.
    pub(crate) fn row_at_screen_row(&self, screen_row: i32) -> Option<&Row> {
        // `screen_row == 0` is the normal content anchor one row below the
        // top rule. Once scrolled, `-1` is valid: content reclaims the screen
        // row where that rule used to be.
        self.row_and_offset_at_screen_row(screen_row)
            .map(|(row, _)| row)
    }

    /// The source line rendered at `screen_row`; `None` on a card row or
    /// past the end.
    pub(crate) fn src_at_screen_row(&self, screen_row: i32) -> Option<usize> {
        self.row_at_screen_row(screen_row).and_then(Row::src)
    }

    /// The rendered (unwrapped) line for a source line and what can be
    /// followed on it. `Hit::span` counts spans of THIS line, which is the
    /// coordinate system the renderer reported in.
    pub(crate) fn text_block_at(
        &self,
        src: usize,
    ) -> Option<(&Line<'static>, &[cosense::render::Hit])> {
        let i = self
            .srcs
            .iter()
            .position(|s| *s == src)
            .filter(|i| matches!(self.blocks.get(*i), Some(Block::Text(_))))?;
        let Some(Block::Text(line)) = self.blocks.get(i) else {
            return None;
        };
        Some((line, self.hits.get(i).map(|v| v.as_slice()).unwrap_or(&[])))
    }

    /// The parts of a mixed text-and-picture line and what can be followed
    /// on it. `Hit::span` counts the spans of the TEXT parts in order.
    pub(crate) fn inline_block_at(
        &self,
        src: usize,
    ) -> Option<(&[cosense::render::InlinePart], &[cosense::render::Hit])> {
        let i = self
            .srcs
            .iter()
            .position(|s| *s == src)
            .filter(|i| matches!(self.blocks.get(*i), Some(Block::Inline { .. })))?;
        let Some(Block::Inline { parts, .. }) = self.blocks.get(i) else {
            return None;
        };
        Some((parts, self.hits.get(i).map(|v| v.as_slice()).unwrap_or(&[])))
    }

    /// `row_at_screen_row`, plus how many rows into that row the screen
    /// row is — a picture row is several rows tall.
    pub(crate) fn row_and_offset_at_screen_row(&self, screen_row: i32) -> Option<(&Row, u16)> {
        let want = self.scroll as i32 + screen_row;
        if want < 0 {
            return None;
        }
        let want = want as u32;
        let mut y = 0u32;
        for row in &self.rows {
            let h = row.height() as u32;
            if want < y + h {
                return Some((row, (want - y) as u16));
            }
            y += h;
        }
        None
    }

    /// Y offset (in height units) of row index `i` from the top.
    pub(crate) fn row_top(&self, i: usize) -> u16 {
        self.rows[..i].iter().map(Row::height).sum()
    }

    /// Total content height in height units.
    pub(crate) fn total_height(&self) -> u16 {
        self.rows.iter().map(Row::height).sum()
    }
}
