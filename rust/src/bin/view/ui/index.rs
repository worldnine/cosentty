//! Project list, sort menu, search marks and excerpt rendering.

use super::*;

/// Draw the project index: a full-width list with a shallow excerpt from
/// the selected page docked below.
///
/// The excerpt is the first lines the list API already returned. That is
/// deliberate: it costs no request, so moving through a thousand pages
/// never waits for the network. Its small height makes that limited content
/// read as an intentional peek rather than an incomplete page.
pub(crate) fn draw_index(f: &mut Frame, app: &mut App, ctx: &Ctx, area: Rect) {
    use cosense::index::{Pane, Row};
    // Before `ix` borrows `app.index`: the header answers to the LISTED
    // project (chrome_colors derives it from `index_project` in list
    // mode), not to the page left behind — entering a project from the
    // projects list turns the header at once.
    let chrome = app.chrome_colors(ctx);
    let Some(ix) = app.index.as_mut() else { return };
    use unicode_width::UnicodeWidthStr;
    let body_h = area.height.saturating_sub(2); // header + footer
    let layout = cosense::index::layout(ctx.preview(), area.width, body_h);
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
            (
                e.title.clone(),
                ix.terms_for(e).into_iter().map(str::to_string).collect(),
            )
        })
        .collect();
    let focus = ix.focus;
    // Hits report their page's own age: they are not sorted on anything the
    // column could be showing instead.
    let sort = if ix.is_search() {
        cosense::index::SortKey::Updated
    } else {
        ix.sort
    };
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
    let ro = if ix.can_create || projects {
        ""
    } else {
        ts!("  [読み取り専用]", "  [read-only]")
    };
    // Where the typed text ends, in display columns — the caret's column.
    // Only meaningful while the line is open.
    let mut caret_col: Option<u16> = None;
    // The project by its proper name, as on the page header.
    let pname = if projects {
        // The level, where a project's list names the project.
        ts!("プロジェクト", "Projects")
    } else {
        [
            app.index_display.as_str(),
            app.index_project.as_str(),
            app.project.as_str(),
        ]
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
            UnicodeWidthStr::width(lead.as_str()).min(area.width.saturating_sub(1) as usize) as u16,
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
        Paragraph::new(head).style(Style::default().fg(chrome.fg).bg(chrome.bg)),
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
    let dim_when_away = |st: Style| {
        if focus == Pane::List {
            st
        } else {
            st.fg(CHROME_DIM)
        }
    };
    let app_light = app.light;
    // Why this row is here: the words the search matched on it, or what was
    // typed to narrow the list. Marked with a background wash, so the
    // colours the row already uses (blue = unread) survive underneath.
    let wash = cosense::theme::match_wash(ctx.terminal_bg());
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
                // seen — in the LISTED project's own telomere colours (the
                // settings are already read for the display name, so this
                // is a cached lookup).
                let listed = if ix.scope == cosense::index::Scope::Pages {
                    &app.index_project
                } else {
                    &app.project
                };
                let tint = ctx
                    .project_theme(listed)
                    .as_deref()
                    .and_then(cosense::theme::cosense_telomere_tint);
                let state = if e.unread {
                    cosense::theme::TelomereState::Unread
                } else {
                    cosense::theme::TelomereState::Read
                };
                let (glyph, tel) =
                    cosense::theme::telomere_with(state, now_secs() - e.updated, app_light, tint);
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
                    spans.extend(marked_spans(
                        &slug,
                        &terms_of(e),
                        style.fg(CHROME_DIM),
                        wash,
                    ));
                }
                Line::from(spans)
            }
            Row::Create(name) => Line::from(Span::styled(
                t!("  ＋ 「{name}」を作成", "  ＋ create \"{name}\""),
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
        .and_then(|_| cosense::theme::scroll_thumb_in_track(rows.len(), rows_h, rows_h, scroll));

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
        app.index_preview_rect = Rect::new(area.x, prev_area.y, area.width, height);
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
        ts!(
            "絞り込み — Enter 確定 · Esc 解除",
            "filter — Enter apply · Esc clear"
        )
        .to_string()
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
        Paragraph::new(format!(" {} {pos} · {hint}", ts!("一覧", "index")))
            .style(Style::default().fg(CHROME_DIM)),
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        ),
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
    draw_sort_menu(
        f,
        area,
        app.index.as_ref().map(|ix| ix.sort).unwrap_or_default(),
        app.index_sort_menu,
    );
}

/// The order menu, shared by the index (`s`) and the page (`S`): the same
/// items, the same mark on the standing order.
pub(crate) fn draw_sort_menu(
    f: &mut Frame,
    area: Rect,
    current: cosense::index::SortKey,
    cursor: Option<usize>,
) {
    if let Some(cursor) = cursor {
        use cosense::index::SortKey;
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
            let st = if hit {
                base.bg(wash).add_modifier(Modifier::BOLD)
            } else {
                base
            };
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
    let Some(ix) = app.index.as_ref() else {
        return Vec::new();
    };
    let Some(entry) = ix.selected() else {
        return vec![Line::from(Span::styled(
            t!(
                "（まだ無いページ — Enter で書きはじめる）",
                "(an uncreated page — Enter starts writing it)"
            ),
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
        if entry.unread {
            ts!(" · 未読", " · unread")
        } else {
            ""
        }
    );
    let show_meta = width > str_width(&meta) + 8;
    let title_width = if show_meta {
        width - str_width(&meta)
    } else {
        width
    };
    let title = truncate_width(&entry.title, title_width);
    let terms: Vec<String> = ix
        .terms_for(entry)
        .into_iter()
        .map(str::to_string)
        .collect();
    let wash = cosense::theme::match_wash(ctx.terminal_bg());
    let mut heading = marked_spans(
        &title,
        &terms,
        Style::default()
            .fg(ctx.palette().title)
            .add_modifier(Modifier::BOLD),
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
    // The excerpt belongs to the LISTED project — acme's list should
    // wear acme's colours before any page of it is open. The projects
    // list (no project of its own) previews the page below, which is the
    // current project's. Both lookups are cached: arriving at an index
    // already reads the project settings for its display name.
    let listed = if ix.scope == cosense::index::Scope::Pages {
        &app.index_project
    } else {
        &app.project
    };
    let palette =
        cosense::theme::tinted_page_palette(&ctx.palette(), ctx.project_theme(listed).as_deref());
    let out = render_lines_with(
        &texts,
        Some(ctx.hl().as_ref()),
        &palette,
        &LinkTruth::default(),
    );
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
