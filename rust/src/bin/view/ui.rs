use super::*;

/// READ の選択バンド背景。カーソル行の帯(`CURSOR_BG` の DarkGray)と
/// 見分けがつくよう、端末背景から作った寒色を使う(theme::selection_band)。
pub(crate) fn selection_bg(terminal_bg: (u8, u8, u8)) -> Color {
    cosense::theme::selection_band(terminal_bg)
}

/// The list marker, in the one place both the drawing and its tests read.
pub(crate) const BULLET: &str = "\u{2022}";

/// What a TAB looks like on the caret line: one column, so the cells it
/// separates stay apart and the caret has somewhere to be.
pub(crate) const TAB_MARK: &str = "|";

/// Source mode's line-number gutter: `"  12 "` (4 digits + space).
pub(crate) const SOURCE_NUM_W: usize = 5;

/// Cursor-row highlight inside the page body.
pub(crate) const CURSOR_BG: Color = Color::DarkGray;


// Non-content chrome uses ANSI palette entries, never fixed RGB. Terminal
// themes own the actual values behind these role colors.
pub(crate) const CHROME_ACCENT: Color = Color::Cyan;

pub(crate) const CHROME_ACTIVE: Color = Color::Green;

pub(crate) const CHROME_CARET: Color = Color::LightBlue;

pub(crate) const CHROME_DIM: Color = Color::DarkGray;

pub(crate) const CHROME_SCROLL: Color = Color::Gray;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeaderColors {
    pub(crate) fg: Color,
    pub(crate) bg: Color,
}

impl HeaderColors {
    pub(crate) fn fallback() -> Self {
        Self { fg: Color::Black, bg: CHROME_ACCENT }
    }
}

impl App {
    /// The colours the header, the page frame and the footer badge wear:
    /// the project's own while reading NOW, Cosense's purple while a past
    /// snapshot is shown — the one sign of history that is on every edge
    /// of the page rather than in a word.
    pub(crate) fn chrome_colors(&self, ctx: &Ctx) -> HeaderColors {
        if self.time.is_some() {
            let (fg, bg) = cosense::theme::history_header_colors(ctx.terminal_bg);
            HeaderColors { fg, bg }
        } else {
            self.header_colors
        }
    }

    /// When the page as shown was last written: the newest line's stamp
    /// for NOW (Cosense's `updated` is exactly that), the snapshot's for
    /// history. 0 when nothing carries a stamp (a page typed offline, a
    /// test fixture).
    pub(crate) fn shown_updated(&self) -> i64 {
        match self.time.as_ref() {
            Some(tm) => tm.points[tm.pos].created,
            None => self.lines.iter().map(|l| l.updated).max().unwrap_or(0),
        }
    }
}

/// Truncate `s` to at most `w` columns, appending `…` when cut.
pub(crate) fn truncate_width(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if str_width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// One related-pages row: `title · value  description` (the value being
/// the one the section is sorted on, see `RelEntry::sort_meta`), the title in
/// `title_style` (see `related_rows`: blue while unread, plain once seen)
/// and the rest dim, truncated to the pane width — related rows do not
/// wrap, being a scannable list rather than body text.
pub(crate) fn related_row(
    e: &RelEntry,
    text_w: usize,
    title_style: Style,
    sort: cosense::index::SortKey,
) -> Line<'static> {
    let dim = Style::default().fg(CHROME_DIM);
    let title = truncate_width(&e.title, text_w);
    let mut spans = vec![Span::styled(title.clone(), title_style)];
    let mut used = str_width(&title);
    let value = e.sort_meta(sort);
    if !value.is_empty() {
        let meta = format!(" · {value}");
        if used + str_width(&meta) <= text_w {
            used += str_width(&meta);
            spans.push(Span::styled(meta, dim));
        }
    }
    if !e.desc.is_empty() && used + 2 < text_w {
        let desc = truncate_width(&e.desc, text_w - used - 2);
        spans.push(Span::styled(format!("  {desc}"), dim));
    }
    Line::from(spans)
}

/// The block a comment (and the composer) is drawn as. Flat, like the
/// rest of this viewer's chrome: no rules, no borders — a band of this
/// background under the lines, with the title as a badge in the top-left
/// corner (black text on the badge colour, the menu panels' title style).
pub(crate) const CARD_BG: Color = Color::Black;

/// A saved comment as a flat block under its lines: the title badge
/// (` comment · 12-14 `, black on yellow) on the first row, the wrapped
/// text, and one blank row of the band below, so the block has the same
/// shape the composer had — Enter turns one into the other with only the
/// badge colour changing. akapen's placement (under the range) with this
/// viewer's flat look instead of akapen's rules.
pub(crate) fn card_lines(c: &Comment, width: usize) -> Vec<Line<'static>> {
    let title = Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD);
    // A comment on a past revision says which one on its title, so a
    // reader stepping through history knows the card is about THIS version.
    let label = match c.revision_label() {
        Some(at) => format!(" comment · {} · {at} ", c.range_label()),
        None => format!(" comment · {} ", c.range_label()),
    };
    bar_lines(&label, title, c.text.lines().flat_map(|l| wrap_plain(l, width)).collect(), width)
}

/// The block shape both the card and the composer use: the badge on the
/// first row, `body` rows, one blank row — every row filled to `width`
/// with the block's background.
fn bar_lines(label: &str, title: Style, body: Vec<String>, width: usize) -> Vec<Line<'static>> {
    let band = Style::default().bg(CARD_BG);
    let pad = |used: usize| " ".repeat(width.saturating_sub(used));
    let mut out = vec![Line::from(vec![
        Span::styled(label.to_string(), title),
        Span::styled(pad(str_width(label)), band),
    ])];
    out.extend(body.into_iter().map(|row| {
        let used = str_width(&row);
        Line::from(vec![Span::styled(row, band), Span::styled(pad(used), band)])
    }));
    out.push(Line::from(Span::styled(pad(0), band)));
    out
}

/// The comment composer as rows under the commented range: the card's
/// block with a cyan badge, ` comment · 12-14 ` (or ` edit · 12-14 ` when replacing
/// an existing comment), the input wrapped to the column, and the
/// insertion point's cell recorded so the hardware cursor — and with it
/// the IME's composition window — sits in the bar.
pub(crate) fn composer_rows(
    input: &Input,
    range: (usize, usize),
    editing: bool,
    revision: Option<String>,
    width: usize,
    max_body: usize,
) -> Vec<Row> {
    let width = width.max(1);
    let title = Style::default().fg(Color::Black).bg(CHROME_ACCENT).add_modifier(Modifier::BOLD);
    let label = if range.0 == range.1 {
        format!("{} {}", if editing { " edit ·" } else { " comment ·" }, range.0 + 1)
    } else {
        format!("{} {}-{}", if editing { " edit ·" } else { " comment ·" }, range.0 + 1, range.1 + 1)
    };
    let label = match revision {
        Some(at) => format!("{label} · {at} "),
        None => format!("{label} "),
    };
    let (before, _) = input.parts();
    let (mut body, (mut crow, ccol)) = wrap_with_caret(&input.buf, before.chars().count(), width);
    // A draft taller than the pane would push its own caret off screen
    // (the whole bar is kept visible, and there is no "whole" that fits).
    // Show a window of `max_body` rows around the caret instead, and say
    // how many rows are folded away above and below on the band rows.
    let (mut above, mut below) = (0usize, 0usize);
    if body.len() > max_body {
        let first = crow.saturating_sub(max_body / 2).min(body.len() - max_body);
        above = first;
        below = body.len() - (first + max_body);
        body = body[first..first + max_body].to_vec();
        crow -= first;
    }
    let mut rows: Vec<Row> = bar_lines(&label, title, body, width)
        .into_iter()
        .map(|line| Row::Composer { line, caret: None })
        .collect();
    if let Some(Row::Composer { caret, .. }) = rows.get_mut(1 + crow) {
        *caret = Some(ccol.min(width - 1) as u16);
    }
    let dim = Style::default().fg(CHROME_DIM).bg(CARD_BG);
    let fold = |n: usize, arrow: &str| -> Line<'static> {
        let text = t!("{arrow} あと {n} 行", "{arrow} {n} more line(s)");
        let pad = " ".repeat(width.saturating_sub(str_width(&text)));
        Line::from(vec![Span::styled(text, dim), Span::styled(pad, dim)])
    };
    if above > 0 {
        // The badge row keeps the badge; the fold note takes its tail.
        if let Some(Row::Composer { line, .. }) = rows.first_mut() {
            let badge = line.spans[0].clone();
            let note = t!("↑ あと {above} 行 ", "↑ {above} more line(s) ");
            let used = str_width(badge.content.as_ref()) + str_width(&note);
            let band = Style::default().bg(CARD_BG);
            *line = Line::from(vec![
                badge,
                Span::styled(" ".repeat(width.saturating_sub(used)), band),
                Span::styled(note, dim),
            ]);
        }
    }
    if below > 0 {
        if let Some(Row::Composer { line, .. }) = rows.last_mut() {
            *line = fold(below, "↓");
        }
    }
    rows
}

/// `wrap_plain`, also reporting the cell the caret (after `caret_chars`
/// characters) lands in: `(row, display column)`. A caret between a full
/// row and the next character sits where that character goes — the start
/// of the next row. Only at the very end of the text can it sit on a full
/// row's right edge; the drawer clamps that one cell.
pub(crate) fn wrap_with_caret(s: &str, caret_chars: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    use unicode_width::UnicodeWidthChar;
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    let mut caret: Option<(usize, usize)> = None;
    for (i, ch) in s.chars().enumerate() {
        // A line break in the draft (^j) ends the row where it stands; a
        // caret sitting on it belongs to the row it ends.
        if ch == '\n' {
            if i == caret_chars {
                caret = Some((out.len(), w));
            }
            out.push(std::mem::take(&mut cur));
            w = 0;
            continue;
        }
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        if i == caret_chars {
            caret = Some((out.len(), w));
        }
        cur.push(ch);
        w += cw;
    }
    let caret = caret.unwrap_or((out.len(), w));
    out.push(cur);
    (out, caret)
}

pub(crate) fn wrap_plain(s: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(ch);
        w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Draw the project index: a full-width list with a shallow excerpt from
/// the selected page docked below.
///
/// The excerpt is the first lines the list API already returned. That is
/// deliberate: it costs no request, so moving through a thousand pages
/// never waits for the network. Its small height makes that limited content
/// read as an intentional peek rather than an incomplete page.
pub(crate) fn draw_index(f: &mut Frame, app: &mut App, ctx: &Ctx, area: Rect) {
    use cosense::index::{Pane, Row};
    let Some(ix) = app.index.as_mut() else { return };
    use unicode_width::UnicodeWidthStr;
    let body_h = area.height.saturating_sub(2); // header + footer
    let layout = cosense::index::layout(ctx.preview, area.width, body_h);
    if layout.preview.is_none() {
        // A resize may remove the excerpt while it owns focus. Hand the
        // keys back to the visible list rather than leaving them attached
        // to a region that no longer exists.
        ix.focus = Pane::List;
    }
    let rows_h = layout.list as usize;
    let scroll = ix.follow(rows_h);
    let rows = ix.rows();
    // Collected before the rows are borrowed for drawing: `terms_for`
    // borrows the index, and the draw loop already holds it.
    let ix_terms: std::collections::HashMap<String, Vec<String>> = ix
        .entries
        .iter()
        .map(|e| {
            (e.title.clone(), ix.terms_for(e).into_iter().map(str::to_string).collect())
        })
        .collect();
    let focus = ix.focus;
    // Hits report their page's own age: they are not sorted on anything the
    // column could be showing instead.
    let sort = if ix.is_search() { cosense::index::SortKey::Updated } else { ix.sort };
    let searching = ix.is_search();
    let projects = ix.scope == cosense::index::Scope::Projects;

    // ---- header ------------------------------------------------------
    let shown = rows.len();
    // The caret belongs to the OPEN line only: a filter that is merely in
    // force is a state, and a state that wears a caret reads as "still
    // typing" (see `Index::filter_editing`).
    //
    // The open line says which question it is asking, because the two look
    // identical otherwise: `/` narrows the titles listed, `?` searches
    // every page's body.
    let sigil = match ix.filter_mode {
        cosense::index::FilterMode::Title => "/",
        cosense::index::FilterMode::FullText => "?",
    };
    // The same badge the page wears. It rides EVERY form of this header,
    // because the moment it explains the most is while a new name is being
    // typed and nothing is offered to create.
    // Not over the projects: nothing is written there, so nothing is
    // read-only either.
    let ro = if ix.can_create || projects { "" } else { ts!("  [読み取り専用]", "  [read-only]") };
    // Where the typed text ends, in display columns — the caret's column.
    // Only meaningful while the line is open.
    let mut caret_col: Option<u16> = None;
    // The project by its proper name, as on the page header.
    let pname = if projects {
        // The level, where a project's list names the project.
        ts!("プロジェクト", "Projects")
    } else {
        [app.index_display.as_str(), app.index_project.as_str(), app.project.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("")
    };
    let unit = if projects { "projects" } else { "pages" };
    let head = if ix.filter_editing {
        // A full-text query has no count yet — the list on screen is still
        // the unsearched one, and calling its length a match count would be
        // a plain lie.
        let tail = match ix.filter_mode {
            cosense::index::FilterMode::Title => format!("({} match)", shown),
            cosense::index::FilterMode::FullText => {
                ts!("(Enter で本文を検索)", "(Enter searches bodies)").to_string()
            }
        };
        // Built in two pieces so the caret's column is MEASURED off the
        // text that precedes it rather than recomputed from the parts —
        // the two could not then disagree. Japanese titles are two columns
        // per character, which is exactly where a re-derivation goes wrong.
        let lead = format!(" {pname} — {sigil}{}", ix.filter);
        caret_col = Some(
            UnicodeWidthStr::width(lead.as_str())
                .min(area.width.saturating_sub(1) as usize) as u16,
        );
        format!("{lead}_ {tail}{ro}")
    } else if let Some(q) = ix.search.as_deref() {
        // The endpoint caps the hit list — and caps its `count` with it —
        // so a full page of hits means "at least this many". Saying "100
        // hits" for a word that is on a thousand pages would be a plain
        // untruth, and the `+` is the whole correction it needs.
        let more = if ix.search_capped { "+" } else { "" };
        format!(" {pname} — ?{q} ({}{more} hits){ro}", ix.entries.len())
    } else if ix.filter.is_empty() {
        let more = if ix.total > ix.entries.len() {
            format!(" of {}", ix.total)
        } else {
            String::new()
        };
        format!(" {pname} — {} {unit}{}{ro}", ix.entries.len(), more)
    } else {
        format!(" {pname} — /{} ({} match){ro}", ix.filter, shown)
    };
    // The order rides at the right end of the header, where it does not
    // push the project and the count around as it changes length. `↓`
    // because five of the six read newest/most first; `title` is the one
    // ascending order and says so. Hits are not in any of those orders.
    let order = if ix.is_search() {
        ts!("関連度順 ", "relevance ").to_string()
    } else if ix.sort == cosense::index::SortKey::Title {
        format!("{} ↑ ", ix.sort.name())
    } else {
        format!("{} ↓ ", ix.sort.name())
    };
    let pad = (area.width as usize)
        .saturating_sub(UnicodeWidthStr::width(head.as_str()))
        .saturating_sub(UnicodeWidthStr::width(order.as_str()));
    let head = format!("{head}{}{order}", " ".repeat(pad));
    f.render_widget(
        Paragraph::new(head).style(
            Style::default().fg(app.header_colors.fg).bg(app.header_colors.bg),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // ---- list --------------------------------------------------------
    //
    // Full width: the short description supplied by the pages API is an
    // excerpt below, not a half-empty reading pane beside the list. A caret
    // column sits at the left edge; the transient scrollbar uses the far
    // right edge, where it cannot look like a divider between content.
    let caret_x = area.x;
    let text_x = area.x + 2;
    let text_w = area.width.saturating_sub(3);
    let list_area = Rect::new(text_x, area.y + 1, text_w, layout.list);
    // The whole row is the click target, not just its text: hitting the
    // telomere or age means that row.
    app.index_list_rect = Rect::new(area.x, area.y + 1, area.width, layout.list);
    let dim_when_away = |st: Style| if focus == Pane::List { st } else { st.fg(CHROME_DIM) };
    let app_light = app.light;
    // Why this row is here: the words the search matched on it, or what was
    // typed to narrow the list. Marked with a background wash, so the
    // colours the row already uses (blue = unread) survive underneath.
    let wash = cosense::theme::match_wash(ctx.terminal_bg);
    let terms_of = |e: &cosense::index::Entry| -> Vec<String> {
        ix_terms.get(&e.title).cloned().unwrap_or_default()
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut caret_rows: Vec<u16> = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(scroll).take(rows_h) {
        let selected = i == ix.cursor;
        let mut style = Style::default();
        if selected {
            style = style.bg(CURSOR_BG);
            caret_rows.push(area.y + 1 + (i - scroll) as u16);
        }
        let line = match row {
            Row::Page(e) => {
                // The same mark the page's own gutter wears: THICKNESS is
                // how recently it changed, COLOUR is whether it has been
                // seen. A project's list then reads the way its lines do.
                let (glyph, tel) =
                    cosense::theme::telomere(now_secs() - e.updated, e.unread, app_light);
                let title_style = if e.unread {
                    dim_when_away(style.fg(CHROME_CARET))
                } else {
                    style
                };
                // Truncate FIRST, then mark: the ranges have to index the
                // text that is actually drawn, not the title it came from.
                // A project row wears its slug after the proper name: it
                // is what the URL and the command line call it, and what
                // the filter may have matched.
                let slug = if e.slug.is_empty() || e.slug == e.title {
                    String::new()
                } else {
                    format!("  {}", e.slug)
                };
                let title_room = (text_w as usize)
                    .saturating_sub(6)
                    .saturating_sub(UnicodeWidthStr::width(slug.as_str()));
                let shown_title = truncate_width(&e.title, title_room);
                let mut spans = vec![
                    Span::styled(glyph.to_string(), style.fg(tel)),
                    // Whatever the list is sorted ON — an age for the three
                    // time orders, the count itself for `linked`/`views`.
                    // Sorting by a number the reader cannot see is no help.
                    Span::styled(format!("{:>4} ", sort.column(e)), style.fg(CHROME_DIM)),
                ];
                spans.extend(marked_spans(&shown_title, &terms_of(e), title_style, wash));
                if !slug.is_empty() {
                    spans.extend(marked_spans(&slug, &terms_of(e), style.fg(CHROME_DIM), wash));
                }
                Line::from(spans)
            }
            Row::Create(name) => Line::from(Span::styled(
                format!("  ＋ 「{name}」を作成"),
                dim_when_away(style.fg(CHROME_ACTIVE)),
            )),
        };
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), list_area);

    // The scrollbar, for as long as the scroll is still in the reader's
    // hand: thumb only, in the column just inside the list's right edge.
    let bar = app
        .index_scrolled_at
        .filter(|t| t.elapsed() < INDEX_BAR_LINGER)
        .and_then(|_| {
            cosense::theme::scroll_thumb_in_track(rows.len(), rows_h, rows_h, scroll)
        });

    // Everything the rest of the frame needs from the borrowed list, so
    // the preview can read the app again.
    let row_count = rows.len();
    let cursor = ix.cursor;
    drop(rows);

    // The cursor rides the left edge, as it does on the page.
    let buf = f.buffer_mut();
    if let Some((start, len)) = bar {
        let bar_x = area.x + area.width.saturating_sub(1);
        for k in start..start + len {
            if let Some(c) = buf.cell_mut((bar_x, area.y + 1 + k as u16)) {
                c.set_symbol("▐");
                c.set_style(Style::default().fg(CHROME_SCROLL));
            }
        }
    }
    for y in caret_rows {
        if let Some(c) = buf.cell_mut((caret_x, y)) {
            c.set_symbol(">");
            c.set_style(Style::default().fg(if focus == Pane::List {
                CHROME_CARET
            } else {
                CHROME_DIM
            }));
        }
    }

    // ---- excerpt -----------------------------------------------------
    app.index_preview_rect = Rect::default();
    if let Some(height) = layout.preview {
        // One blank row after the list, then a shallow, full-width peek.
        // Nothing frames it: its fixed height is what says "excerpt".
        let prev_area = Rect::new(
            text_x,
            area.y + 1 + layout.list + layout.gap,
            text_w,
            height,
        );
        app.index_preview_rect = Rect::new(
            area.x,
            prev_area.y,
            area.width,
            height,
        );
        let lines = index_preview_lines(app, ctx, text_w as usize);
        let ix = app.index.as_ref().expect("open");
        let skip = ix.preview_scroll as usize;
        let shown: Vec<Line<'static>> =
            lines.into_iter().skip(skip).take(height as usize).collect();
        f.render_widget(Paragraph::new(shown), prev_area);
    }

    // ---- footer ------------------------------------------------------
    let ix = app.index.as_ref().expect("open");
    // The index has no status line; a note takes the hint slot for its
    // seconds, as on the page.
    let hint: String = if let Some((n, _)) = app.note.as_ref() {
        n.clone()
    } else if ix.filter_editing && projects {
        ts!("絞り込み — Enter 確定 · Esc 解除", "filter — Enter apply · Esc clear").to_string()
    } else if projects {
        ts!(
            "j/k · / 絞り込み · Enter 開く · ^o 取り直す · Esc/[ 戻る · q 終了",
            "j/k · / filter · Enter open · ^o refetch · Esc/[ back · q quit"
        )
        .to_string()
    } else if ix.filter_editing {
        match ix.filter_mode {
            cosense::index::FilterMode::Title => ts!(
                "タイトル絞り込み — Enter 確定 · Tab 本文検索へ · Esc 解除",
                "filter titles — Enter apply · Tab full-text · Esc clear"
            ),
            cosense::index::FilterMode::FullText => ts!(
                "本文検索 — Enter 検索 · Tab タイトル絞り込みへ · Esc 解除",
                "search bodies — Enter search · Tab titles · Esc clear"
            ),
        }
        .to_string()
    } else if searching && ix.focus == Pane::List {
        ts!(
            "j/k · Enter 開く · / 検索し直す · ^u 一覧へ · Esc/[ 戻る · q 終了",
            "j/k · Enter open · / search again · ^u list · Esc/[ back · q quit"
        )
        .to_string()
    } else {
        match (layout.preview.is_some(), ix.focus) {
            (true, Pane::List) => ts!(
                "j/k · / 絞り込み・検索 · s 並び順 · Enter 開く · Tab 抜粋 · Esc/[ 戻る · q 終了",
                "j/k · / filter or search · s order · Enter open · Tab excerpt · Esc/[ back · q quit"
            ),
            (true, Pane::Preview) => ts!(
                "j/k 抜粋をスクロール · Tab 一覧 · Enter 開く · Esc/[ 戻る · q 終了",
                "j/k scroll excerpt · Tab list · Enter open · Esc/[ back · q quit"
            ),
            (false, _) => ts!(
                "j/k · / 絞り込み・検索 · s 並び順 · Enter 開く · Esc/[ 戻る · q 終了",
                "j/k · / filter or search · s order · Enter open · Esc/[ back · q quit"
            ),
        }
        .to_string()
    };
    // Where in the list the reader is — the footer's job here as on the
    // page (`L12/205`), which is why the list needs no scrollbar.
    let pos = if row_count == 0 {
        "0/0".to_string()
    } else {
        format!("{}/{}", cursor + 1, row_count)
    };
    f.render_widget(
        Paragraph::new(format!(" {} {pos} · {hint}", ts!("一覧", "index"))).style(Style::default().fg(CHROME_DIM)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );

    // The filter line's caret: put the HARDWARE cursor on it. Terminal
    // IMEs anchor their inline composition to the hardware cursor, so this
    // is what makes 日本語入力 appear where the word is being typed instead
    // of wherever the cursor was last left (the same technique the edit
    // session uses — see `set_cursor_position` in `draw_page`). The `_` is
    // kept as a soft caret for terminals that draw no cursor at all.
    if let Some(col) = caret_col {
        f.set_cursor_position(ratatui::layout::Position::new(area.x + col, area.y));
    }

    // ---- the sort menu, over the list --------------------------------
    //
    // Drawn here rather than through `Overlay`: the index returns early in
    // `ui`, so an overlay laid over the page would never appear above it.
    if let Some(cursor) = app.index_sort_menu {
        use cosense::index::SortKey;
        let current = app.index.as_ref().map(|ix| ix.sort).unwrap_or_default();
        // The names are the API's own words and stay in English; the mark
        // is what says which order the list is actually in.
        let items: Vec<String> = SortKey::ALL
            .iter()
            .map(|k| {
                let here = if *k == current { "·" } else { " " };
                format!("{here} {}", k.name())
            })
            .collect();
        draw_menu_panel(
            f,
            area,
            ts!("並び順", "order"),
            &items,
            cursor,
            ts!(
                " ↑/↓ 移動 · Enter 並べ替え · Esc 閉じる ",
                " ↑/↓ move · Enter re-order · Esc close "
            ),
        );
    }
}

/// Split `text` into spans, marking the parts that matched.
///
/// The mark is a BACKGROUND wash and bold, never a foreground colour: the
/// index's rows already spend fg on "unread" and the excerpt's spans spend
/// it on notation, and a match has to be readable on top of both rather
/// than instead of them.
pub(crate) fn marked_spans(
    text: &str,
    terms: &[String],
    base: Style,
    wash: Color,
) -> Vec<Span<'static>> {
    let refs: Vec<&str> = terms.iter().map(String::as_str).collect();
    cosense::index::split_on_terms(text, &refs)
        .into_iter()
        .map(|(piece, hit)| {
            let st = if hit { base.bg(wash).add_modifier(Modifier::BOLD) } else { base };
            Span::styled(piece.to_string(), st)
        })
        .collect()
}

/// The same, applied to an ALREADY RENDERED line: each span is split on its
/// own displayed text.
///
/// It has to run after rendering, not before: a hit line arrives as raw
/// notation (`[* 画像の挿入をテストする]`) and matching that would mark
/// bracket positions instead of words. The cost is that a term straddling
/// two spans — half inside a link, half outside — goes unmarked; flattening
/// and re-splitting the whole line to catch that is not worth what it is
/// worth.
pub(crate) fn mark_line(line: Line<'static>, terms: &[String], wash: Color) -> Line<'static> {
    if terms.is_empty() {
        return line;
    }
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .flat_map(|sp| {
            let st = sp.style;
            marked_spans(sp.content.as_ref(), terms, st, wash)
        })
        .collect();
    Line::from(spans)
}

/// The excerpt dock for the page under the cursor: one compact heading and
/// the first lines supplied by the pages API, rendered as the page would.
pub(crate) fn index_preview_lines(app: &App, ctx: &Ctx, width: usize) -> Vec<Line<'static>> {
    let Some(ix) = app.index.as_ref() else { return Vec::new() };
    let Some(entry) = ix.selected() else {
        return vec![Line::from(Span::styled(
            t!("（まだ無いページ — Enter で書きはじめる）", "(an uncreated page — Enter starts writing it)"),
            Style::default().fg(CHROME_DIM),
        ))];
    };
    // Compact on purpose: title and metadata share one row, leaving almost
    // all of this shallow region to the excerpt itself. A rule or box would
    // make it look like a second full view and amplify the empty space.
    let dim = Style::default().fg(CHROME_DIM);
    let meta = format!(
        " · {} ago{}",
        relative_age(entry.updated),
        if entry.unread { " · 未読" } else { "" }
    );
    let show_meta = width > str_width(&meta) + 8;
    let title_width = if show_meta { width - str_width(&meta) } else { width };
    let title = truncate_width(&entry.title, title_width);
    let terms: Vec<String> = ix.terms_for(entry).into_iter().map(str::to_string).collect();
    let wash = cosense::theme::match_wash(ctx.terminal_bg);
    let mut heading = marked_spans(
        &title,
        &terms,
        Style::default().fg(ctx.palette.title).add_modifier(Modifier::BOLD),
        wash,
    );
    if show_meta {
        heading.push(Span::styled(meta, dim));
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(heading)];
    // The renderer reads line 0 as the page's title (it wears the title
    // style and carries no notation), so the title has to be there — and
    // its block dropped, since the heading above already says it.
    let mut texts: Vec<String> = vec![entry.title.clone()];
    texts.extend(entry.descriptions.iter().cloned());
    let out = render_lines_with(&texts, Some(&ctx.hl), &ctx.palette, &LinkTruth::default());
    for block in out.blocks.iter().skip(1) {
        match block {
            Block::Text(line) => {
                for w in wrap_line_parts(line, width.saturating_sub(1), &hanging_prefix(line)) {
                    lines.push(mark_line(w.line, &terms, wash));
                }
            }
            Block::Blank => lines.push(Line::from("")),
            // A picture, a table or a diagram in the first lines: the
            // preview says it is there rather than drawing it, which would
            // cost a download per cursor move.
            _ => lines.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(CHROME_DIM),
            ))),
        }
    }
    lines
}

/// Page chrome: READ のページ枠は、ページヘッダ自身のアクセント色を
/// まとう — サイト(cosense web)でページがプロジェクト色の中に置かれて
/// いるのと同じ言い方。EDIT は枠を持たず、本文全域の下敷き
/// (theme::edit_backdrop)がモードを語る。
pub(crate) fn page_frame_style(header: HeaderColors) -> Style {
    Style::default().fg(header.bg)
}

pub(crate) fn ui(f: &mut Frame, app: &mut App, ctx: &Ctx) {
    let area = f.area();
    f.render_widget(Clear, area);

    // The index owns the whole screen while it is open: it is a place to
    // be, not something laid over the page (the page is still loaded and
    // Esc goes back to it).
    if app.index.is_some() {
        draw_index(f, app, ctx, area);
        draw_toast(f, app, ctx, area);
        return;
    }

    // header: `name / title` on the left — the project's proper name, or
    // its slug until the settings are read — and, at the right end, only
    // what is STATE and has no other sign: when the page was written, a
    // page that has drifted from the server, read-only. EDIT/SRC have the
    // footer badge, the backdrop and the caret; the comment count has `l`;
    // a selection has its footer hint; unread lines have the telomere's
    // colour. None of those earn a second badge here.
    //
    // The date is the same slot for NOW and for history: stepping back
    // with ← changes the date in place (and adds the position), while the
    // chrome turns purple — that is the whole transition.
    let chrome = app.chrome_colors(ctx);
    let name = if app.project_display.is_empty() { app.project.as_str() } else { app.project_display.as_str() };
    let mut badges: Vec<String> = Vec::new();
    // The position counts NOW as the newest entry: `4/4` while reading the
    // live page with three snapshots, and ← walks the number down.
    let stamp = app.shown_updated();
    let date = if stamp > 0 { cosense::theme::format_local(stamp) } else { String::new() };
    match (app.history_position(), date.is_empty()) {
        (Some((pos, total)), false) => badges.push(format!("{pos}/{total} · {date}")),
        (Some((pos, total)), true) => badges.push(format!("{pos}/{total}")),
        (None, false) => badges.push(date),
        (None, true) => {}
    }
    if app.web_unsynced {
        badges.push(ts!("未同期", "unsynced").to_string());
    }
    if !app.editable {
        badges.push(ts!("読み取り専用", "read-only").to_string());
    }
    let right = if badges.is_empty() { String::new() } else { format!("{} ", badges.join(" · ")) };
    let head = header_line(name, &app.title, &right, area.width);
    f.render_widget(
        Paragraph::new(head).style(
            Style::default()
                .fg(chrome.fg)
                .bg(chrome.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // help / status: a leading position readout (akapen's `VIEW L23/118`),
    // then the contextual hint — numbered links when the cursor line has
    // them, else the static key list, else the standing status (what IS:
    // a selection, the history position). One-shot notices are toasts and
    // float above this line instead (toast.rs).
    let mode_tag = if app.session.is_some() {
        "EDIT"
    } else if app.mode == Mode::Source {
        "SRC"
    } else {
        "VIEW"
    };
    let pos = if app.lines.is_empty() {
        "L-/-".to_string()
    } else if app.cursor >= app.lines.len() && !app.virtual_items.is_empty() {
        // Cursor is on a related row below the body.
        format!("link {}/{}", app.cursor - app.lines.len() + 1, app.virtual_items.len())
    } else {
        let cur = app.cursor_src().map(|s| s + 1).unwrap_or(1).min(app.lines.len());
        format!("L{}/{}", cur, app.lines.len())
    };
    let cur_links = if app.session.is_some() { Vec::new() } else { app.cursor_line_links() };
    let hint = app.hint_text(&cur_links);
    // EDIT のタグはヘッダと同じ配色の反転バッジで示す: 枠色・キャレットと
    // 並ぶ三つ目のモードサイン。READ は従来どおり控えめに。
    let mode_style = if app.session.is_some() {
        Style::default()
            .fg(chrome.fg)
            .bg(chrome.bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(CHROME_DIM)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", mode_tag), mode_style),
            Span::styled(format!("{} · {}", pos, hint), Style::default().fg(CHROME_DIM)),
        ])),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );

    // The page is boxed flush with the terminal edge. The cursor `>` rides
    // ON the left frame column, while the telomere keeps its own inside
    // column and therefore remains visible on the cursor line. One blank
    // column separates that telomere from the text. The right side keeps a
    // blank, the scrollbar thumb, and the frame column.
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(2),
    );
    // Layout: frame/caret, telomere, blank, text, blank, thumb, frame.
    let caret_x = body.x;
    let gutter_x = body.x + 1;
    let bar_x = body.x + body.width.saturating_sub(2);
    let text = Rect::new(
        body.x + 3,
        body.y + 1,
        body.width.saturating_sub(5),
        body.height.saturating_sub(2),
    );
    // While the top rule is visible, the scrollbar starts immediately
    // below it. After scrolling the rule away, the track reclaims that row
    // just like text, images, and telomeres do.
    let (bar_y, bar_h) = if app.scroll == 0 {
        (body.y.saturating_add(1), body.height.saturating_sub(1))
    } else {
        (body.y, body.height)
    };
    let bar = Rect::new(bar_x, bar_y, 1, bar_h);

    // The page frame hugs PAGE CONTENT ONLY. Its explicit FrameEnd closes
    // the box; related sections continue beneath it without vertical sides.
    // Everything scrolls in one coordinate system, so j/k crosses naturally.
    let top_rule = text.y as i32 - 1 - app.scroll as i32;
    let bot_rule = text.y as i32 + app.frame_end_top() as i32 - app.scroll as i32;
    let band_top = body.y as i32;
    let band_bot = (area.y + area.height - 2) as i32; // one above status
    let band_h = (band_bot - band_top + 1).max(1) as u16; // visible rows
    // EDIT の下敷き: 本文領域の全幅に、キャレット行の帯より暗い背景を
    // 敷く。読む画面と書く画面で「紙の色」が変わるのが持続的なモード
    // サイン。行の帯(CURSOR_BG)はこの上に明るく浮く。
    if app.session.is_some() {
        let backdrop = cosense::theme::edit_backdrop(ctx.terminal_bg);
        let buf = f.buffer_mut();
        for y in band_top..=band_bot {
            for x in body.x..body.x + body.width {
                if let Some(c) = buf.cell_mut((x, y as u16)) {
                    c.set_bg(backdrop);
                }
            }
        }
    } else {
        let buf = f.buffer_mut();
        // READ のページ枠はヘッダのアクセント色(サイトでページが
        // プロジェクト色の中に置かれているのと同じ)。EDIT は枠なし —
        // 下敷きがモードを語る。
        let frame_style = page_frame_style(chrome);
        let right_x = body.x + body.width.saturating_sub(1);
        let set = |buf: &mut ratatui::buffer::Buffer, x: u16, y: i32, s: &str| {
            if y < band_top || y > band_bot {
                return;
            }
            if let Some(c) = buf.cell_mut((x, y as u16)) {
                c.set_symbol(s);
                c.set_style(frame_style);
            }
        };
        // top rule ┌─…─┐
        if top_rule >= band_top && top_rule <= band_bot {
            set(buf, body.x, top_rule, "┌");
            for x in (body.x + 1)..right_x {
                set(buf, x, top_rule, "─");
            }
            set(buf, right_x, top_rule, "┐");
        }
        // bottom rule └─…─┘
        if bot_rule >= band_top && bot_rule <= band_bot {
            set(buf, body.x, bot_rule, "└");
            for x in (body.x + 1)..right_x {
                set(buf, x, bot_rule, "─");
            }
            set(buf, right_x, bot_rule, "┘");
        }
        // vertical sides: strictly between the content's rule rows, so the
        // sides scroll off with the page (ratatui clips rows off-screen;
        // we only limit writes to the band so we never paint over the
        // header/status strips).
        let v_top = (top_rule + 1).max(band_top);
        let v_bot = (bot_rule - 1).min(band_bot);
        for y in v_top..=v_bot {
            set(buf, body.x, y, "│");
            set(buf, right_x, y, "│");
        }
    }
    app.text_rect = text;
    app.bar_rect = bar;

    if app.laid_width != body.width {
        // Width changed: re-wrap, keeping the cursor line on its screen row
        // (the resize equivalent of akapen's cursor_fraction handoff — the
        // cursor is a source line, so only the viewport needs adjusting).
        app.relayout_preserving_screen_row(body.width, band_h);
    }
    if app.view_h != band_h {
        // Height changed: the old offset may now overshoot the end, and the
        // cursor may have fallen off the bottom.
        app.view_h = band_h;
        app.scroll = app.scroll.min(app.max_scroll(band_h));
        app.follow = true;
    }
    if app.follow {
        app.follow_cursor(band_h);
        app.follow = false;
    }
    // A toast floats on the band's last row. The cursor line is what the
    // reader is looking at, so it never sits under the banner: while one
    // is up, the viewport is nudged so the cursor's rows end above it.
    if app.toast.is_some() {
        app.keep_cursor_above(text.y, band_bot as u16, band_h);
    }
    // The composer bar is what is being typed into: it stays on screen
    // whatever the range it hangs under is doing.
    if app.composing.is_some() {
        app.keep_composer_visible(band_h);
    }

    // EDIT の行またぎ文字選択の両端。キャレット行は自分の行の反転を
    // session 描画(caret_sel_bytes)が行うので、ここではそれ以外の行 —
    // 遠端の行と間の行 — の文字反転に使う。
    let sess_sel: Option<((usize, usize), (usize, usize))> = app
        .session
        .as_ref()
        .and_then(|s| s.sel_ends())
        .filter(|((la, _), (lb, _))| la != lb);

    let view_top = app.scroll as i32;
    // Row coordinates exclude the top rule, while drawing uses
    // `text.y + row - scroll`. Therefore one row above `view_top` remains
    // visible at the band's top after scrolling; omitting it left screen row
    // 2 permanently blank.
    let visible_rows_top = view_top - 1;
    let visible_rows_bottom = view_top + band_h as i32 - 1;
    // The grabbed block wears the SAME highlight a selection does — it is
    // the thing being carried around, which is what a selection means
    // here. The footer says the mode and its keys in words, so this colour
    // is never the only thing telling the reader what is going on.
    // EDIT の文字選択はここに合流させない(READ の選択色を継がない)。
    // 正確な範囲は文字反転(sess_sel と reverse_cols)が語り、行の帯は
    // 下の in_edit_sel が「選択が通っている行」— 反転する文字を持たない
    // 空行も含めて — をカーソル行と同じグレーで示す。
    let sel_range =
        move_block_range(app).or_else(|| app.selection.map(|s| s.range()));
    // Telomeres and frame-column carets are painted after the rows.
    let mut gutter: Vec<(u16, &'static str, Style)> = Vec::new();
    let mut carets: Vec<(u16, Style)> = Vec::new();
    // Rows that carry a comment (yellow) or are being commented on right
    // now (the composer's cyan): a thick bar in the frame's left column,
    // akapen's `▌` marker. Painted last, so it wins over the `>` caret —
    // the caret is one row, the bar says which lines the comment covers.
    let mut comment_bars: Vec<(u16, Style)> = Vec::new();
    let composing_range = app
        .composing
        .as_ref()
        .map(|_| app.selection.map(|s| s.range()).unwrap_or((app.cursor, app.cursor)));
    // Rows inside a `code:` block. Painted LAST, as a background-only pass:
    // the band has to run to the frame, and the telomere, the thumb and the
    // padding columns are all drawn after the rows.
    let mut wash_rows: Vec<u16> = Vec::new();
    // Bullets for image rows. An indented picture is a LIST ITEM whose
    // content is the picture (cosense web draws the bullet there too), and
    // the image protocol paints its own area, so the marker is written
    // beside it in the same pass as the telomeres.
    let mut image_bullets: Vec<(u16, u16)> = Vec::new();

    // Which rows are code: a `code:` block reads as one surface, so its
    // rows carry a wash. Computed once per frame from the text the screen
    // is showing (the caret line included, uncommitted and all).
    let code_flags = cosense::render::code_line_flags(&app.source_texts());
    let wash = cosense::theme::code_wash(ctx.terminal_bg);
    let sel_bg = selection_bg(ctx.terminal_bg);

    let mut y = 0i32;
    for row in app.rows.iter() {
        let h = row.height() as i32;
        let top = y;
        let bottom = y + h;
        y = bottom;
        if bottom <= visible_rows_top || top >= visible_rows_bottom {
            continue;
        }
        let screen_y = top - view_top;

        // Every display row of the cursor's source line is the cursor row
        // (wrapped continuations too), so the band covers the whole line.
        let is_cursor = row.src() == Some(app.cursor);
        let in_sel = row
            .src()
            .and_then(|s| sel_range.map(|(a, b)| a <= s && s <= b))
            .unwrap_or(false);
        // EDIT の文字選択が通っている行。正確な範囲は文字反転が示すので、
        // 帯は「この行が含まれている」— 反転する文字を持たない空行も —
        // をカーソル行と全く同じグレーで静かに言うだけでよい。
        let in_edit_sel = row
            .src()
            .and_then(|s| sess_sel.map(|((a, _), (b, _))| a <= s && s <= b))
            .unwrap_or(false);
        let has_comment = row.src().map(|s| app.src_has_comment(s)).unwrap_or(false);
        let in_composing = row
            .src()
            .and_then(|s| composing_range.map(|(a, b)| a <= s && s <= b))
            .unwrap_or(false);
        // Telomere: age + read state of this row's source line (None for
        // synthesized rows). A row BELOW the body is a related page, and it
        // answers the same two questions about itself — how recently it
        // changed, and whether it has been seen — so it wears the same
        // mark. One encoding for the whole viewer: body lines, the index's
        // list, and the related sections.
        let age = row.src().and_then(|s| match app.lines.get(s) {
            Some(l) => Some(((now_secs() - l.updated).max(0), app.line_unread(l))),
            None => app.related_telomere(s),
        });
        let in_code = row.src().map(|s| code_flags.get(s) == Some(&true)).unwrap_or(false);
        let mut base = Style::default();
        // The wash goes down first: selection and the cursor band are
        // stronger signals and paint over it.
        if in_code && !in_sel && !in_edit_sel && !is_cursor {
            base = base.bg(wash);
            for k in 0..h {
                let y = text.y as i32 + screen_y + k;
                if y >= band_top && y <= band_bot {
                    wash_rows.push(y as u16);
                }
            }
        }
        if in_sel {
            base = base.bg(sel_bg);
        }
        if in_edit_sel {
            base = base.bg(CURSOR_BG);
        }
        if is_cursor {
            base = base.bg(CURSOR_BG);
            // READ: paint the WHOLE row between the frame columns before
            // drawing its content: telomere, the blank after it, text-side
            // padding, and the scrollbar column must read as one cursor
            // band. EDIT: the band is the caret's runway, so it covers only
            // the columns the caret can actually reach — the text area,
            // which `base` already paints — and the telomere and the
            // scrollbar column stay bare.
            if app.session.is_none() {
                let left = body.x.saturating_add(1);
                let right = body.x.saturating_add(body.width).saturating_sub(1);
                let buf = f.buffer_mut();
                for k in 0..h {
                    let sy = screen_y + k;
                    let y = text.y as i32 + sy;
                    if y >= band_top && y <= band_bot {
                        for x in left..right {
                            if let Some(c) = buf.cell_mut((x, y as u16)) {
                                c.set_bg(CURSOR_BG);
                            }
                        }
                    }
                }
            }
        }

        // The gutter marker for each on-screen row of this Row (an image
        // occupies several). Uses the same screen row as the text: `text.y`
        // anchors content, rows flow with `-scroll` (top is the row's
        // content offset, view_top = scroll).
        if row.src().is_some() {
            for k in 0..h {
                let sy = screen_y + k;
                let y = text.y as i32 + sy;
                if y >= band_top && y <= band_bot {
                    let (glyph, mut style) = gutter_cell(age, ctx.light);
                    // EDIT の帯は本文領域だけ(上のコメント参照)なので、
                    // テロメアには帯の色を継がせない。
                    if let Some(bg) = base.bg {
                        if app.session.is_none() {
                            style = style.bg(bg);
                        }
                    }
                    gutter.push((y as u16, glyph, style));
                    // 行カーソル `>`(左フレーム列)は READ のもの。EDIT
                    // ではキャレット(点滅バー+反転セル)が居場所を語る
                    // ので、二重に指さない。
                    if is_cursor && app.session.is_none() {
                        let mut caret_style =
                            Style::default().fg(CHROME_CARET).add_modifier(Modifier::BOLD);
                        if let Some(bg) = base.bg {
                            caret_style = caret_style.bg(bg);
                        }
                        carets.push((y as u16, caret_style));
                    }
                    if in_composing || has_comment {
                        let color = if in_composing { CHROME_ACCENT } else { Color::Yellow };
                        let mut bar = Style::default().fg(color).add_modifier(Modifier::BOLD);
                        if let Some(bg) = base.bg {
                            bar = bar.bg(bg);
                        }
                        comment_bars.push((y as u16, bar));
                    }
                }
            }
        }

        let one_row = |screen_y: i32| -> Option<Rect> {
            let y = text.y as i32 + screen_y;
            if y >= band_top && y <= band_bot {
                Some(Rect::new(text.x, y as u16, text.width, 1))
            } else {
                None
            }
        };
        // A comment block (card or composer) starts one column left of the
        // text, over the telomere column: its badge and its first letter
        // sit where the page's own gutter is, so the block reads as an
        // aside hung on the lines, not as more text.
        let bar_row = |screen_y: i32| -> Option<Rect> {
            one_row(screen_y).map(|r| {
                if r.x > 0 {
                    Rect::new(r.x - 1, r.y, r.width + 1, 1)
                } else {
                    r
                }
            })
        };
        // …and its band runs the whole width of the frame, frame columns
        // included: the block cuts straight across the page. What belongs
        // on top — the `▌` comment marks, the caret, the scrollbar thumb —
        // is painted after the rows and lands over it.
        let band_across = |f: &mut Frame, y: u16| {
            let buf = f.buffer_mut();
            for x in body.x..body.x + body.width {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_symbol(" ");
                    c.set_style(Style::default().bg(CARD_BG));
                }
            }
        };

        match row {
            Row::Line { line, src, start, hang } => {
                if let Some(r) = one_row(screen_y) {
                    // A diagram being rendered dims its code and lets a band
                    // of brightness run down it: the reader sees the work
                    // happening without the text moving under them.
                    let painted = match app.web_shimmer.get(src) {
                        Some((pos, len)) => shimmer(line, *pos, *len, app, ctx),
                        None => line.clone(),
                    };
                    // EDIT の行またぎ文字選択: キャレット行以外も選択された
                    // 文字そのものを反転する。遠端の行は境界の文字から
                    // (または境界の文字まで)、間の行は本文の全文字。境界の
                    // 表示列は click_caret と同じ物差し(session_display)で
                    // 測るので、クリックが置いた端と食い違わない。
                    let painted = match (sess_sel, app.session.as_ref()) {
                        (Some(((la, ba), (lb, bb))), Some(sess))
                            if *src != sess.line && *src >= la && *src <= lb =>
                        {
                            let raw =
                                app.lines.get(*src).map(|l| l.text.as_str()).unwrap_or("");
                            let code = app.raw_span_at_line(*src);
                            let text_col = session_hang(raw, code);
                            let (lo, hi) = if *src == la {
                                (sel_boundary_col(raw, ba, code), usize::MAX)
                            } else if *src == lb {
                                (text_col, sel_boundary_col(raw, bb, code))
                            } else {
                                (text_col, usize::MAX)
                            };
                            let w = painted.width();
                            let x0 = hang + lo.saturating_sub(*start);
                            let x1 = if hi == usize::MAX {
                                w
                            } else if hi <= *start {
                                0
                            } else {
                                (hang + hi.saturating_sub(*start)).min(w)
                            };
                            reverse_cols(painted, x0.min(w), x1)
                        }
                        _ => painted,
                    };
                    f.render_widget(Paragraph::new(painted).style(base), r);
                }
            }
            Row::Card { line } => {
                if let Some(r) = bar_row(screen_y) {
                    band_across(f, r.y);
                    f.render_widget(Paragraph::new(line.clone()).style(base), r);
                }
            }
            Row::Aside { line } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(Paragraph::new(line.clone()).style(base), r);
                }
            }
            Row::Composer { line, caret } => {
                if let Some(r) = bar_row(screen_y) {
                    band_across(f, r.y);
                    f.render_widget(Paragraph::new(line.clone()).style(base), r);
                    if let Some(col) = caret {
                        // The hardware cursor marks the insertion point:
                        // this is a Japanese input surface, and the IME's
                        // composition window follows the hardware cursor.
                        let x = r.x + (*col).min(r.width.saturating_sub(1));
                        f.set_cursor_position(ratatui::layout::Position::new(x, r.y));
                    }
                }
            }
            // Already painted as the frame's └───┘ rule above.
            Row::FrameEnd => {}
            Row::Blank { .. } => {
                if let Some(r) = one_row(screen_y) {
                    f.render_widget(Paragraph::new("").style(base), r);
                }
            }
            Row::ImageError { msg, indent, item, .. } => {
                if let Some(r) = one_row(screen_y) {
                    let pad = bullet_pad(*indent, *item);
                    f.render_widget(
                        Paragraph::new(format!("{pad} {msg}")).style(base.fg(Color::Red)),
                        r,
                    );
                }
            }
            Row::ImageLoading { indent, item, url, .. } => {
                if let Some(r) = one_row(screen_y) {
                    // The line as the author wrote it, provisional and
                    // working: the band running along it says a picture is
                    // on its way, the way a diagram's code says so.
                    let pad = bullet_pad(*indent, *item);
                    let text = truncate_width(&format!("{pad}[{url}]"), r.width as usize);
                    let line = Line::from(Span::styled(text, base.fg(CHROME_DIM)));
                    f.render_widget(Paragraph::new(shimmer_across(&line, app, ctx)), r);
                }
            }
            Row::Inline { images, texts, indent, item, .. } => {
                // The bullet belongs at the item's TOP-left: the line
                // starts there, however tall the pictures on it are.
                if *item && *indent >= 2 {
                    let y = text.y as i32 + screen_y;
                    if y >= band_top && y <= band_bot {
                        image_bullets.push((y as u16, text.x + *indent as u16 - 2));
                    }
                }
                // Text first: a picture paints its own cells over the top,
                // and nothing here may write into them.
                for (row_off, col, piece) in texts {
                    if let Some(r) = one_row(screen_y + *row_off as i32) {
                        let w = r.width.saturating_sub(*col);
                        if w > 0 {
                            f.render_widget(
                                Paragraph::new(piece.line.clone()).style(base),
                                Rect::new(r.x + *col, r.y, w, 1),
                            );
                        }
                    }
                }
                for (row_off, col, w, h, url) in images {
                    let Some(info) = app.images.get(url) else {
                        // Not arrived. The box stands reserved so the words
                        // beside it do not jump; a hole that says nothing is
                        // the other half of the dead animation, so the
                        // notation goes on the line's own baseline — the row
                        // the picture hangs off — wearing the band a waiting
                        // `[URL]` has always worn.
                        let baseline = *row_off as i32 + *h as i32 - 1;
                        let Some(r) = one_row(screen_y + baseline) else { continue };
                        let room = (*w).min(r.width.saturating_sub(*col));
                        if room == 0 {
                            continue;
                        }
                        let text = truncate_width(&format!("[{url}]"), room as usize);
                        let line = Line::from(Span::styled(text, base.fg(CHROME_DIM)));
                        f.render_widget(
                            Paragraph::new(shimmer_across(&line, app, ctx)),
                            Rect::new(r.x + *col, r.y, room, 1),
                        );
                        continue;
                    };
                    let top = screen_y + *row_off as i32;
                    let pos = SignedPosition {
                        x: 0,
                        y: (top + text.y as i32 - band_top) as i16,
                    };
                    let w = info.cells_w.min(text.width.saturating_sub(*col));
                    if w == 0 {
                        continue;
                    }
                    f.render_widget(
                        SlicedImage::new(&info.sliced, pos),
                        Rect::new(text.x + *col, band_top as u16, w, band_h),
                    );
                }
            }
            Row::Image { url, indent, item, .. } => {
                if *item && *indent >= 2 {
                    let y = text.y as i32 + screen_y;
                    if y >= band_top && y <= band_bot {
                        image_bullets.push((y as u16, text.x + *indent as u16 - 2));
                    }
                }
                // Render into the full text area with a signed position: when
                // the image top is scrolled above the viewport, screen_y is
                // negative and the sliced protocol clips the hidden rows; when
                // it extends past the bottom, the area clips it. Either way it
                // stays visible for its in-view portion (no all-or-nothing).
                // The cursor band cannot paint over image pixels, so on an
                // image line only the gutter marker shows the cursor (akapen
                // behaves the same). Position the slice relative to text.y
                // (the content anchor), matching how text rows move.
                if let Some(info) = app.images.get(url) {
                    // The image area starts at the whole band's top, unlike
                    // the unscrolled text anchor (`text.y`) one row below it.
                    // Adding that one-cell delta lets a partially scrolled
                    // image paint the reclaimed top row too.
                    // The picture starts exactly at the text column of its
                    // level, so it lines up with the lines around it. (The
                    // protocol's own one-column inset used to add itself on
                    // top of the indent and pushed indented pictures right.)
                    let off = (*indent as u16).min(text.width.saturating_sub(2));
                    let pos = SignedPosition {
                        x: 0,
                        y: (screen_y + text.y as i32 - band_top) as i16,
                    };
                    // Never hand the protocol more columns than it has, and
                    // never more than the pane: a diagram encoded for a
                    // wider pane (a rescale still in flight) is clipped here
                    // rather than painting over the frame.
                    let img_area = Rect::new(
                        text.x + off,
                        band_top as u16,
                        info.cells_w.min(text.width.saturating_sub(off)),
                        band_h,
                    );
                    f.render_widget(SlicedImage::new(&info.sliced, pos), img_area);
                }
            }
        }
    }

    // Marker columns: the telomere remains in the inside gutter, while `>`
    // replaces the frame glyph at the same screen row. Rows without a
    // source line leave both blank. (`sy` is already an absolute row.)
    let buf = f.buffer_mut();
    for (sy, glyph, style) in gutter {
        if let Some(c) = buf.cell_mut((gutter_x, sy)) {
            c.set_symbol(glyph);
            c.set_style(style);
        }
    }
    for (sy, x) in image_bullets {
        if let Some(c) = buf.cell_mut((x, sy)) {
            c.set_symbol(BULLET);
            c.set_style(Style::default().fg(ctx.palette.bullet));
        }
    }
    for (sy, style) in carets {
        if let Some(c) = buf.cell_mut((caret_x, sy)) {
            c.set_symbol(">");
            c.set_style(style);
        }
    }
    for (sy, style) in comment_bars {
        if let Some(c) = buf.cell_mut((caret_x, sy)) {
            c.set_symbol(COMMENT_BAR);
            c.set_style(style);
        }
    }
    // Scrollbar: ONLY the thumb is drawn, in its own column just inside the
    // frame's right border — there is no always-on track, so non-thumb rows
    // show just the clean `│` border (akapen: `…▐│` only where the thumb
    // is, never a double line). The thumb tracks the VIEWPORT offset (wheel
    // scroll moves the viewport only); when the content fits, nothing is
    // drawn and the border stays clean.
    // The track starts below the top rule while that rule is visible, then
    // expands into the reclaimed row. Its scroll range still matches the
    // full keyboard/wheel viewport.
    let thumb = Style::default().fg(CHROME_SCROLL);
    if let Some((start, len)) = cosense::theme::scroll_thumb_in_track(
        (app.total_height() + 1) as usize,
        band_h as usize,
        bar.height as usize,
        app.scroll as usize,
    ) {
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((bar.x, bar.y + i as u16)) {
                c.set_symbol("▐");
                c.set_style(thumb);
            }
        }
    }

    // The code wash, run to the frame on both sides. Only the background
    // is touched, so the telomere glyph, the scrollbar thumb and the text
    // keep their own colors and simply sit on the block's surface.
    let left = body.x.saturating_add(1);
    let right = body.x.saturating_add(body.width).saturating_sub(1);
    for sy in wash_rows {
        for x in left..right {
            if let Some(c) = buf.cell_mut((x, sy)) {
                c.set_bg(wash);
            }
        }
    }

    // The edit session's caret: put the HARDWARE cursor on it. Terminal
    // IMEs anchor their inline composition window to the hardware cursor,
    // so this is what makes 日本語入力 land visually at the caret (akapen's
    // composer technique). `wrap_plain_columns` drives both the display
    // and this math, so they can never disagree.
    if let Some(s) = &app.session {
        if let Some((first, _)) = app.src_rows(s.line) {
            let text_w = App::text_width(app.mode, app.laid_width.max(1));
            let code = app.raw_span_at_line(s.line);
            let disp = session_display(&s.input.buf, code);
            let dcaret = display_caret(&s.input.buf, s.input.cur, code);
            let wrapped =
                SessionWrap::new(&disp, text_w, session_hang(&s.input.buf, code));
            let (crow, ccol) = wrapped.row_col(dcaret);
            let y = text.y as i32 + (app.row_top(first) as i32 + crow as i32) - app.scroll as i32;
            let x = text.x as i32 + (ccol as i32).min(text.width.saturating_sub(1) as i32);
            if y >= band_top && y <= band_bot {
                f.set_cursor_position(ratatui::layout::Position::new(x as u16, y as u16));
                // ソフト描画のキャレット: 同じセルの REVERSED をトグルする。
                // ハードウェアカーソルの形状変更(点滅バー)に応えない端末
                // でもキャレットが見えるように、常に併走させる。
                //
                // ただし選択があるあいだは描かない。キャレットは常に選択の
                // 端にいるので、その外側の文字のセルを反転させると選択が
                // 1文字広く見える(`Garry` を選ぶと直後の全角 `・` の箱まで
                // 反転して `Garry・` に見えた)。選択の端がキャレットの位置
                // そのものだし、点滅バーもそこにある。
                let selecting = s.sel_ends().is_some();
                if let (false, Some(c)) = (selecting, f.buffer_mut().cell_mut((x as u16, y as u16))) {
                    let st = c.style();
                    let st = if st.add_modifier.contains(Modifier::REVERSED) {
                        st.remove_modifier(Modifier::REVERSED)
                    } else {
                        st.add_modifier(Modifier::REVERSED)
                    };
                    c.set_style(st);
                }
            }
        }
    }

    // Modal overlay (comments list / link picker / help) on top.
    if app.overlay.is_some() {
        draw_overlay(f, app, area);
    }
    // A toast rides above the footer, over everything else, and leaves by
    // itself (toast.rs).
    draw_toast(f, app, ctx, area);
}

/// One thing on a line: a run of text, or a picture with its cell size.
#[derive(Clone, Debug)]
pub(crate) enum Inline {
    Text(Line<'static>),
    Image { url: String, w: u16, h: u16 },
}

/// Where the layout put something.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed<T> {
    pub(crate) row: u16,
    pub(crate) col: u16,
    pub(crate) what: T,
}

/// One row's worth of one text part, and where in the UNWRAPPED part it
/// begins — what turns a clicked cell back into a span of the renderer's
/// line (see `App::link_at_screen_position`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextPiece {
    /// Index into the `items` given to `layout_inline`.
    pub(crate) part: usize,
    /// Display column of the part's text this piece starts at.
    pub(crate) start: usize,
    pub(crate) line: Line<'static>,
}

/// A line laid out the way a browser lays out inline images: items run
/// left to right, and each picture sits ON the text line — its BOTTOM
/// edge level with the text, growing upwards — so the words before and
/// after it read as one sentence. When a row fills, the next "line box"
/// starts below, indented like the rest of the item.
///
/// This is what lets `本文 [画像]`, `[画像]本文` and `[A] と [B]` all be
/// the same thing: a sequence, not three special cases.
#[allow(unused_assignments)] // the final `flush!` resets state nobody reads
pub(crate) fn layout_inline(
    items: &[Inline],
    indent: usize,
    width: usize,
) -> (Vec<Placed<String>>, Vec<Placed<TextPiece>>, u16) {
    let body = width.saturating_sub(indent).max(1);
    let mut images: Vec<Placed<String>> = Vec::new();
    let mut texts: Vec<Placed<TextPiece>> = Vec::new();
    // The box being filled: its top row, how tall it is so far, how far
    // along it we are, and what has been put in it (positions are relative
    // to the box, resolved against its height when it is flushed).
    let mut top = 0u16;
    let mut box_h = 1u16;
    let mut x = 0usize;
    let mut pending_img: Vec<(u16, u16, u16, String)> = Vec::new(); // (col, w, h, url)
    let mut pending_txt: Vec<(u16, TextPiece)> = Vec::new(); // (col, text)

    // Close the current box: pictures sit on the text line, so each one is
    // placed so its LAST row is the box's last row.
    macro_rules! flush {
        () => {
            for (col, _, h, url) in pending_img.drain(..) {
                let row = top + box_h - h;
                images.push(Placed { row, col: col + indent as u16, what: url });
            }
            for (col, piece) in pending_txt.drain(..) {
                texts.push(Placed {
                    row: top + box_h - 1,
                    col: col + indent as u16,
                    what: piece,
                });
            }
            top += box_h;
            box_h = 1;
            x = 0;
        };
    }

    for (part, item) in items.iter().enumerate() {
        match item {
            Inline::Image { url, w, h } => {
                let w = (*w).min(body as u16);
                if x > 0 && x + w as usize > body {
                    flush!();
                }
                pending_img.push((x as u16, w, (*h).max(1), url.clone()));
                box_h = box_h.max((*h).max(1));
                x += w as usize;
            }
            Inline::Text(line) => {
                let mut rest = line.clone();
                // How far into the part's text the next piece begins. The
                // wrap drops nothing, so the widths of the pieces add up.
                let mut start = 0usize;
                loop {
                    let room = body.saturating_sub(x);
                    // Too little room to say anything: start a new box.
                    if room < 2 && x > 0 {
                        flush!();
                        continue;
                    }
                    let mut pieces = wrap_line(&rest, room.max(1)).into_iter();
                    let Some(first) = pieces.next() else { break };
                    let used = str_width(
                        &first.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
                    );
                    pending_txt.push((x as u16, TextPiece { part, start, line: first }));
                    start += used;
                    x += used;
                    let tail: Vec<Span<'static>> =
                        pieces.flat_map(|l| l.spans.into_iter()).collect();
                    if tail.is_empty() {
                        break;
                    }
                    flush!();
                    rest = Line::from(tail);
                }
            }
        }
    }
    flush!();
    (images, texts, top.max(1))
}

/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder/// The lead-in for an indented image placeholder: the bullet where the
/// picture's own bullet goes, then the space the picture would start at.
pub(crate) fn bullet_pad(indent: usize, item: bool) -> String {
    if item && indent >= 2 {
        format!("{}{BULLET} ", " ".repeat(indent - 2))
    } else {
        " ".repeat(indent)
    }
}

/// One row of a block the web renderer is still working on/// One row of a block the web renderer is still working on, with the
/// brightness band applied to every span.
/// EDIT の行またぎ選択の境界(行内バイト位置)を、折り返し前の表示列に
/// 変換する。rendered な行の列とは厳密には一致しないことがある(記法が
/// 隠れる行など)が、click_caret がアンカーを置くときと同じ近似なので、
/// マウスが選んだ端と描画は食い違わない。
pub(crate) fn sel_boundary_col(raw: &str, byte: usize, code: Option<CodeSpan>) -> usize {
    let disp = session_display(raw, code);
    let d = display_caret(raw, byte, code);
    str_width(&disp[..floor_boundary(&disp, d)])
}

/// 1つの表示行の [from, to) 列を REVERSED にする。スパンは表示列で
/// 切り分け、境界をまたぐスパンだけ分割する。
pub(crate) fn reverse_cols(line: Line<'static>, from: usize, to: usize) -> Line<'static> {
    if from >= to {
        return line;
    }
    let style = line.style;
    let alignment = line.alignment;
    let mut out: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut at = 0usize;
    for sp in line.spans {
        let w = str_width(sp.content.as_ref());
        let (s, e) = (at, at + w);
        at = e;
        if e <= from || s >= to || w == 0 {
            out.push(sp);
            continue;
        }
        let text = sp.content.into_owned();
        let cut_a = byte_at_col(&text, from.saturating_sub(s));
        let cut_b = byte_at_col(&text, to.saturating_sub(s).min(w));
        if cut_a > 0 {
            out.push(Span::styled(text[..cut_a].to_string(), sp.style));
        }
        if cut_b > cut_a {
            out.push(Span::styled(
                text[cut_a..cut_b].to_string(),
                sp.style.add_modifier(Modifier::REVERSED),
            ));
        }
        if cut_b < text.len() {
            out.push(Span::styled(text[cut_b..].to_string(), sp.style));
        }
    }
    let mut line = Line::from(out);
    line.style = style;
    line.alignment = alignment;
    line
}

pub(crate) fn shimmer(line: &Line<'static>, pos: u16, len: u16, app: &App, ctx: &Ctx) -> Line<'static> {
    let level = cosense::theme::shimmer_level(pos, len, app.web_anim.elapsed().as_secs_f32());
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| {
            Span::styled(
                s.content.clone(),
                cosense::theme::shimmer_style(s.style, ctx.terminal_bg, level),
            )
        })
        .collect();
    Line::from(spans)
}

/// `shimmer` turned sideways: the band runs ALONG a single row, character
/// by character, for the rows that are one line tall (a picture's `[URL]`
/// while it downloads, and the box a mixed line reserves for one).
///
/// Two things differ from the vertical band beyond the direction, and both
/// are the reason this row used to look dead: it animates against the
/// pictures' own clock, and `shimmer_level_across` times the sweep instead
/// of counting cells per second, so a sixty-cell URL crosses in the same
/// moment a twenty-cell one does. Same brightness floor and ceiling, so the
/// two still read as one signal.
pub(crate) fn shimmer_across(line: &Line<'static>, app: &App, ctx: &Ctx) -> Line<'static> {
    let elapsed = app.image_anim.elapsed().as_secs_f32();
    let len = str_width(&line.to_string()) as u16;
    let mut col: u16 = 0;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for s in &line.spans {
        for ch in s.content.chars() {
            let level = cosense::theme::shimmer_level_across(col, len, elapsed);
            spans.push(Span::styled(
                ch.to_string(),
                cosense::theme::shimmer_style(s.style, ctx.terminal_bg, level),
            ));
            col += str_width(&ch.to_string()) as u16;
        }
    }
    Line::from(spans)
}

/// Draw the open overlay as a centered panel.
pub(crate) fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    let (title, items, cursor): (String, Vec<String>, usize) = match app.overlay.as_ref() {
        Some(Overlay::Links { items, cursor }) => (
            t!("この行のリンク", "links on this line"),
            items.iter().map(LinkItem::label).collect(),
            *cursor,
        ),
        Some(Overlay::Comments { cursor }) => (
            format!("comments ({})", app.comments.len()),
            app.comments
                .iter()
                .map(|c| {
                    let first = c.text.lines().next().unwrap_or("");
                    match c.revision_label() {
                        Some(at) => format!("{} :{} @{at}  {}", c.title, c.range_label(), first),
                        None => format!("{} :{}  {}", c.title, c.range_label(), first),
                    }
                })
                .collect(),
            *cursor,
        ),
        Some(Overlay::LineInfo) => {
            let src = app.cursor_src();
            let items = match src.and_then(|s| app.lines.get(s).map(|l| (s, l))) {
                Some((s, l)) => {
                    let name_of = |id: &str| -> String { app.member_name(id) };
                    // `line.user_id` is the LAST UPDATER (verified against the
                    // commit log). The original author would need the commit
                    // history (~1s per page), which is deliberately not worth
                    // it here — so only the updater is named.
                    // The line's text is already on screen under the cursor,
                    // and its id is only needed by `e` (browser deep-link),
                    // so neither is repeated here.
                    vec![
                        t!("行        {}", "line      {}", s + 1),
                        t!(
                            "更新      {}  （{}前）  {}",
                            "updated   {}  ({} ago)   {}",
                            cosense::theme::format_local(l.updated),
                            relative_age(l.updated),
                            name_of(&l.user_id)
                        ),
                        t!(
                            "作成      {}  （{}前）",
                            "created   {}  ({} ago)",
                            cosense::theme::format_local(l.created),
                            relative_age(l.created)
                        ),
                    ]
                }
                None => vec![t!("カーソルの下に行がありません", "no line under the cursor")],
            };
            (t!("行の詳細", "line detail"), items, usize::MAX)
        }
        Some(Overlay::Help) => {
            // The label column is 12 display cells wide. Japanese labels
            // are two cells per character, so each language pads its own
            // labels here rather than through a `{:<12}` that counts bytes.
            let mut keys: Vec<String> = vec![
                t!("移動        j/k · g/G · ^u/^d · PgUp/PgDn",
                   "move        j/k · g/G · ^u/^d · PgUp/PgDn"),
                t!("リンク      Enter/f で開く: ページ · 📎 ファイル → 保存先 · ↗ URL → ブラウザ",
                   "link        Enter/f open: page · 📎 file → download dir · ↗ URL → browser"),
                t!("            Tab/S-Tab 次/前のリンク行へ",
                   "            Tab/S-Tab next/previous link line"),
                t!("マウス      クリックでリンク/行移動 · ドラッグで選択 · ホイールでスクロール",
                   "mouse       click link/open · click row/move · drag/select · wheel/scroll"),
                t!("移動履歴    [ 戻る · ] 進む", "history     [ back · ] forward"),
                t!("表示切替    z 表示⇄ソース（行番号つき raw）", "source      z view⇄source (raw with line numbers)"),
                t!("ページ一覧  ^o 一覧＋抜粋 · / 絞り込み（Tab で本文検索）· s 並び順 · ^o もう一度でプロジェクト一覧 · q 終了",
                   "index       ^o list + excerpt · / filter (Tab: full-text) · s order · ^o again: projects · q quit"),
            ];
            if app.editable {
                keys.extend([
                    t!("編集        e 行末 · i 行頭 · o/O 行を追加 · ダブルクリック — モードレスな編集",
                       "edit        e line end · i line start · o/O new line · double-click — modeless"),
                    t!("            編集中: そのまま入力 · ↑↓ 行移動 · Enter 改行 · ⌫@行頭 前の行と結合 · Esc 終了",
                       "            in session: type freely · ↑↓ lines · Enter new line · ⌫@BOL join · Esc done"),
                    t!("            x 行/選択を削除 · ^e ページ全体を $EDITOR で編集 · コミットは自動",
                       "            x delete line/selection · ^e whole page in $EDITOR · commits are automatic"),
                    t!("            画像: 編集中に ^v でクリップボードの画像 / 画像ファイルのパスを貼る → アップロードして [URL]",
                       "            images: while editing, ^v pastes the clipboard image / paste an image file's path → uploaded as [URL]"),
                    t!("構造編集    m 移動モード（ブロックをつかむ）: j/k/↑↓ 1行 · J/K 兄弟ごと",
                       "outline     m move mode (grab a block): j/k/↑↓ one line · J/K whole sibling"),
                    t!("            h/l/←→ 字下げ · Esc/Enter/m 確定（掴んだまま他のキーを押すと確定）",
                       "            h/l/←→ indent · Esc/Enter/m commit (any other key commits too)"),
                    t!("            Ctrl+←/→/↑/↓ 行・選択範囲 · Alt+←/→/↑/↓ ブロック",
                       "            Ctrl+←/→/↑/↓ line/range · Alt+←/→/↑/↓ block"),
                    t!("            ^g h/j/k/l 行 左/下/上/右 · ^g H/J/K/L ブロック（選択中は不可）",
                       "            ^g h/j/k/l line left/down/up/right · ^g H/J/K/L block (no selection)"),
                    t!("取り消し    u 取り消し · ^r やり直し（どのコミットも戻せます）",
                       "undo        u undo · ^r redo (every commit is reversible)"),
                ]);
            } else {
                keys.push(t!(
                    "編集        できません — このアカウントはプロジェクトのメンバーではありません",
                    "edit        unavailable — this account is not a project member"
                ));
            }
            keys.extend([
                t!("ブラウザ    w カーソル行でページを開く", "browser     w open page at cursor line"),
                t!("ページ履歴  ← 古い履歴 · → 新しい · Esc 最新へ戻る（履歴中は読み取り専用）",
                   "time        ← older snapshot · → newer · Esc back to NOW (read-only while back)"),
                t!("コメント    c 書く（同じ範囲で再度 c = 編集）· s 送る · l 一覧（Enter 移動 · d 削除 · y コピー · s 送る）",
                   "comment     c write (c again on the range: edit) · s send · l list (Enter jump · d delete · y copy · s send)"),
                t!("行の詳細    t この行をいつ誰が更新したか", "detail      t who/when edited this line"),
                t!("図          code:mmd / code:mermaid / code:<名前>.mmd を図として描く。",
                   "diagram     code:mmd / code:mermaid / code:<name>.mmd draw as pictures,"),
                t!("            ヘッドレス Chrome で実際の Cosense ページを撮って切り出す",
                   "            screenshotted from the real Cosense page by headless Chrome"),
                t!("            （場所は COSENSE_CHROME）。ブラウザが無い、COSENSE_SID 無しで",
                   "            (COSENSE_CHROME to point at it). No browser, a private page"),
                t!("            非公開ページ、Mermaid のエラーのときはコードブロックのまま。",
                   "            without COSENSE_SID, or a Mermaid error → the code block stays."),
                t!("            R でこのページの未生成分を描く。ページを開いただけでは",
                   "            R renders this page's missing web artifacts. Opening a page"),
                t!("            キャッシュ済みしか出ない（ブラウザは高価なので、頼んだときに",
                   "            only shows cached ones: a browser is expensive, so it starts"),
                t!("            起動する）。COSENSE_WEB_RENDER=auto|off で振る舞いを変えられ、",
                   "            when you ask. COSENSE_WEB_RENDER=auto|off changes that;"),
                t!("            COSENSE_WEB_IDLE_SECS でブラウザを残す長さを決められる。",
                   "            COSENSE_WEB_IDLE_SECS is how long the browser stays warm."),
                t!("出力        y カーソル行/選択をコピー · Y ページ全体", "output      y copy line/selection · Y whole page"),
                t!("画面        ? ヘルプ", "screen      ? help"),
                t!("終了        q 二度押し（未送信のコメントがあれば件数を添えて問う）",
                   "quit        q twice (asks, naming unsent comments)"),
            ]);
            (t!("キー割り当て", "keys"), keys, usize::MAX)
        }
        None => return,
    };

    let footer = if matches!(app.overlay, Some(Overlay::Comments { .. })) {
        ts!(
            " ↑/↓ 移動 · Enter 開く · d 削除 · y 全件コピー · s 送る · Esc 閉じる ",
            " ↑/↓ move · Enter open · d delete · y copy all · s send · Esc close "
        )
    } else {
        ts!(
            " ↑/↓ 移動 · Enter 開く · Esc 閉じる ",
            " ↑/↓ move · Enter open · Esc close "
        )
    };
    draw_menu_panel(f, area, &title, &items, cursor, footer);
}

/// A centered menu panel: title bar, the items with a `▸` on the cursor,
/// and one dim line of keys. `cursor == usize::MAX` marks nothing, which is
/// how the read-only panels (help, line detail) use it.
pub(crate) fn draw_menu_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    items: &[String],
    cursor: usize,
    footer: &str,
) {
    let w = area.width.saturating_sub(8).min(90).max(20);
    let want_h = items.len() as u16 + 2;
    let h = want_h.min(area.height.saturating_sub(4)).max(3);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let panel = Rect::new(x, y, w, h);
    f.render_widget(Clear, panel);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::Black)
            .bg(CHROME_ACCENT)
            .add_modifier(Modifier::BOLD),
    )));
    let visible = (h.saturating_sub(2)) as usize;
    let start = if cursor != usize::MAX && cursor >= visible {
        cursor + 1 - visible
    } else {
        0
    };
    for (i, it) in items.iter().enumerate().skip(start).take(visible) {
        let sel = i == cursor;
        let style = if sel {
            Style::default()
                .fg(Color::Black)
                .bg(CHROME_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if sel { "▸ " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{it}"), style)));
    }
    lines.push(Line::from(Span::styled(
        footer.to_string(),
        Style::default().fg(CHROME_DIM),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Black)),
        panel,
    );
}

/// The telomere gutter cell for a row with a source line (`age` = seconds
/// since the edit, plus whether the line is unread). The cursor does not
/// compete for this cell: its `>` is painted separately on the frame column.
///
/// The SAME cell serves the related-page rows below the body (their age is
/// their page's, see `App::related_telomere`) and, through
/// `theme::telomere`, the index's list. Thickness is always "how recently
/// did this change" and colour is always "have I seen it" — a mark that
/// meant different things on different screens would be worse than no mark.
///
/// A comment marker is deliberately NOT drawn yet: akapen colors comments
/// yellow (`▌`) and reserves green for changed lines, so guessing a color
/// before the telomere/comment balance is settled would bake in a wrong
/// convention. `has_comment` is kept so the caller can still tint the row
/// without touching the marker column.
/// The thick bar in the frame's left column on commented lines (yellow)
/// and on the lines being commented on (cyan, while the composer is open).
/// akapen's marker, same glyph.
pub(crate) const COMMENT_BAR: &str = "▌";

pub(crate) fn gutter_cell(age: Option<(i64, bool)>, light: bool) -> (&'static str, Style) {
    match age {
        Some((a, unread)) => {
            let (glyph, color) = cosense::theme::telomere(a, unread, light);
            (glyph, Style::default().fg(color))
        }
        None => (" ", Style::default()),
    }
}

impl App {
    pub(crate) fn hint_text(&self, cursor_links: &[LinkItem]) -> String {
        match self.sync_notice() {
            Some(tag) => format!("{tag} · {}", self.hint_body(cursor_links)),
            None => self.hint_body(cursor_links),
        }
    }

    pub(crate) fn hint_body(&self, cursor_links: &[LinkItem]) -> String {
        // The quiet level overlays the slot: for its few seconds the note
        // is the whole hint — the keys and the standing status step aside
        // rather than crowd it, and come back when it expires. The one
        // exception is the ^g prefix: its next key is consumed whatever it
        // is, so the reader must see what it accepts.
        if let (Some((msg, _)), false) = (self.note.as_ref(), self.outline_prefix) {
            return msg.clone();
        }
        // The move mode says its own name and its own keys, in words: the
        // grabbed block is highlighted, but a highlight is a colour and a
        // colour alone must not be what tells the reader where they are.
        if self.move_mode.is_some() {
            let keys = t!(
                "MOVE — j/k 1行 · J/K 兄弟 · h/l 字下げ · Esc 確定",
                "MOVE — j/k line · J/K sibling · h/l indent · Esc commit"
            );
            return match self.status.is_empty() {
                true => keys,
                false => format!("{keys} · {}", self.status),
            };
        }
        if self.outline_prefix {
            return t!(
                "OUTLINE h/j/k/l 行 左/下/上/右 · H/J/K/L ブロック · Esc 取消",
                "OUTLINE h/j/k/l line left/down/up/right · H/J/K/L block · Esc cancel"
            );
        }
        if let Some(s) = self.session.as_ref() {
            let dirty = s.input.buf != s.orig;
            let keys = t!("{}↑↓ 移動 · Enter 改行 · ⌫@行頭 前の行と結合 · Tab 字下げ · Esc 終了", "{}↑↓ move · Enter new line · ⌫@BOL join · Tab indent · Esc done",
                if dirty { "● " } else { "" }
            );
            // An upload's progress arrives while the keys are being typed;
            // it rides along behind the hint as MOVE's does.
            return match self.status.is_empty() {
                true => keys,
                false => format!("{keys} · {}", self.status),
            };
        }
        if !cursor_links.is_empty() {
            let listed: Vec<String> = cursor_links
                .iter()
                .take(9)
                .enumerate()
                .map(|(i, l)| {
                    // A link with nothing behind it does not open: Enter
                    // starts the page. Better said before it is pressed.
                    let mark = match l {
                        LinkItem::Page(t) if self.links.missing(t) => ts!("(未作成)", "(uncreated)"),
                        _ => "",
                    };
                    format!("{}:{}{mark}", i + 1, l.label())
                })
                .collect();
            return t!("Enter/f で開く → {}", "Enter/f open → {}", listed.join("  "));
        }
        if !self.status.is_empty() {
            return self.status.clone();
        }
        if self.editable {
            t!("j/k 移動  Enter リンク  e 編集  o 行追加  u 取り消し  w ブラウザ  ? ヘルプ  q 終了", "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit")
        } else {
            t!("j/k 移動  Enter リンク  w ブラウザ  ? ヘルプ  q 終了  · 読み取り専用", "j/k move  Enter link  w browser  ? help  q quit  · read-only")
        }
    }

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
        let edit: Option<(usize, &str)> =
            self.session.as_ref().map(|s| (s.line, s.input.buf.as_str()));
        // Inside a `code:` block the caret line is code, not an outline
        // row: its leading whitespace is content and must not be drawn as
        // a bullet. Judged on the WORKING text, so typing `code:` turns
        // the bullets off as you type it, not one commit later.
        let edit_code = edit.and_then(|(line, _)| self.raw_span_at_line(line));
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
            // view — while the underlying data stays whitespace.
            let disp = session_display(buf, edit_code);
            let prefix = if edit_code.is_some() { 0 } else { display_prefix_bytes(buf) };
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
                for (from, to, selected) in
                    [(0, lo, false), (lo, hi, true), (hi, seg.len(), false)]
                {
                    if from >= to {
                        continue;
                    }
                    // The dim prefix may end inside this piece.
                    let cut = dim_to.clamp(from, to);
                    push(&seg[from..cut], selected, true);
                    push(&seg[cut..to], selected, false);
                }
                content.push(Row::Line { line: Line::from(spans), src, start: 0, hang: 0 });
            }
        };
        let mut content: Vec<Row> = Vec::new();
        for (b, &src) in self.blocks.iter().zip(self.srcs.iter()) {
            if let Some((eline, ebuf)) = edit {
                if src == eline
                    && !matches!(b, Block::Table(_) | Block::WebRender { .. })
                {
                    raw_rows(&mut content, ebuf, src);
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
                        content.push(Row::Line { line, src: row_src, start: 0, hang: 0 });
                    }
                }
                Block::WebRender { kind, code, rows, last_src } => {
                    let key = self
                        .web_request(*kind, code, *last_src)
                        .map(|r| r.cache_key());
                    // While the edit session is inside this block the reader
                    // is working on the raw source, so the picture steps
                    // aside — the existing source-editing contract wins.
                    let editing_here = edit
                        .map(|(eline, _)| rows.iter().any(|(rsrc, _)| *rsrc == eline))
                        .unwrap_or(false);
                    let art = if editing_here {
                        None
                    } else {
                        key.as_ref().and_then(|k| self.images.get(k).map(|i| (k, i)))
                    };
                    if let Some((k, info)) = art {
                        content.push(Row::Image {
                            url: k.clone(),
                            height: info.cells_h.max(1),
                            src: *last_src,
                            indent: 0,
                            item: false,
                        });
                        continue;
                    }
                    // No artifact (yet, or ever): the plain code block, with
                    // the session's caret row swapped to raw source.
                    for (rsrc, line) in rows {
                        if let Some((eline, ebuf)) = edit {
                            if *rsrc == eline {
                                raw_rows(&mut content, ebuf, *rsrc);
                                continue;
                            }
                        }
                        for w in wrap_line_parts(line, text_w, &hanging_prefix(line)) {
                            content.push(Row::Line {
                                line: w.line,
                                src: *rsrc,
                                start: w.start,
                                hang: w.hang,
                            });
                        }
                    }
                }
                Block::Inline { indent, item, parts } => {
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
                        content.push(Row::ImageLoading { src, indent, item: *item, url: url.clone() });
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
                    Inline::Image { url: url.clone(), w, h }
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
        let mut rows = vec![Row::Aside { line: Line::from("") }];
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
            rows.push(Row::Aside { line: Line::from("") });
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
                content.push(Row::Line { line: Line::from(spans), src, start: 0, hang: 0 });
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
            let range = self.selection.map(|s| s.range()).unwrap_or((self.cursor, self.cursor));
            let anchor = content
                .iter()
                .enumerate()
                .filter(|(_, row)| row.src().is_some_and(|s| range.0 <= s && s <= range.1))
                .map(|(idx, _)| idx)
                .last()
                .unwrap_or(content.len().saturating_sub(1));
            let revision = self.shown_revision().map(|r| cosense::theme::format_local(r.created));
            // The body may take the pane minus the badge row, the band
            // row, and one row of the page for context; at least one row.
            // Before the first frame the height is not known (0): no cap.
            let max_body = match self.view_h {
                0 => usize::MAX,
                h => (h as usize).saturating_sub(3).max(1),
            };
            (anchor, composer_rows(input, range, self.comment_for_range().is_some(), revision, width_cols, max_body))
        });
        let hidden_card = if self.composing.is_some() { self.comment_for_range() } else { None };
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
            let Block::WebRender { rows, last_src, .. } = b else { return None };
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
        self.row_and_offset_at_screen_row(screen_row).map(|(row, _)| row)
    }

    /// The source line rendered at `screen_row`; `None` on a card row or
    /// past the end.
    pub(crate) fn src_at_screen_row(&self, screen_row: i32) -> Option<usize> {
        self.row_at_screen_row(screen_row).and_then(Row::src)
    }

    /// The rendered (unwrapped) line for a source line and what can be
    /// followed on it. `Hit::span` counts spans of THIS line, which is the
    /// coordinate system the renderer reported in.
    pub(crate) fn text_block_at(&self, src: usize) -> Option<(&Line<'static>, &[cosense::render::Hit])> {
        let i = self
            .srcs
            .iter()
            .position(|s| *s == src)
            .filter(|i| matches!(self.blocks.get(*i), Some(Block::Text(_))))?;
        let Some(Block::Text(line)) = self.blocks.get(i) else { return None };
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
        let Some(Block::Inline { parts, .. }) = self.blocks.get(i) else { return None };
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

/// One header row: ` name / title` at the left edge, `right` flush with
/// the right edge. When it all does not fit, things give way in this order:
/// the RIGHT side never (the states — history position, date, unsynced,
/// read-only — are what the header is for); the site NAME goes first and
/// goes whole, leaving ` / title` (the name is one word the reader already
/// knows, and cutting it to `研…` says nothing); the TITLE is cut to `…`
/// only after that.
pub(crate) fn header_line(name: &str, title: &str, right: &str, width: u16) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = width as usize;
    let rw = UnicodeWidthStr::width(right);
    let room = width.saturating_sub(rw + 1); // one blank before the badges
    let full = format!(" {name} / {title}");
    let left = if UnicodeWidthStr::width(full.as_str()) <= room {
        full
    } else {
        let bare = format!(" / {title}");
        if UnicodeWidthStr::width(bare.as_str()) <= room { bare } else { truncate_width(&bare, room) }
    };
    let pad = width.saturating_sub(UnicodeWidthStr::width(left.as_str()) + rw);
    format!("{left}{}{right}", " ".repeat(pad))
}
