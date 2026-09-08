//! Frame drawing; the submodules own layout, text, lists and overlays.

use super::*;

mod chrome;
mod content;
mod index;
mod inline;
mod overlay;
mod text;

pub(crate) use chrome::*;
pub(crate) use index::*;
pub(crate) use inline::*;
pub(crate) use overlay::*;
pub(crate) use text::*;

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
        if app.overlay.is_some() {
            draw_overlay(f, app, area);
        }
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
    let name = if app.project_display.is_empty() {
        app.project.as_str()
    } else {
        app.project_display.as_str()
    };
    let mut badges: Vec<String> = Vec::new();
    // The position counts NOW as the newest entry: `4/4` while reading the
    // live page with three snapshots, and ← walks the number down.
    let stamp = app.shown_updated();
    let date = if stamp > 0 {
        cosense::theme::format_local(stamp)
    } else {
        String::new()
    };
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
    let right = if badges.is_empty() {
        String::new()
    } else {
        format!("{} ", badges.join(" · "))
    };
    let head = header_line(name, &app.title, &right, area.width);
    // The site name — or the lone `/` when there is no room for the name —
    // is the click target that opens the project's page list (the header's
    // way of saying `^o`). Same arithmetic as `header_line`: the full
    // " name / title" when it fits, else the bare " / title".
    let home_w = if (1 + str_width(name) + 3 + str_width(&app.title)) as u16
        <= area.width.saturating_sub(str_width(&right) as u16 + 1)
    {
        1 + str_width(name) + 3
    } else {
        2
    };
    app.header_home_rect = Rect::new(area.x, area.y, home_w as u16, 1);
    f.render_widget(
        Paragraph::new(head).style(
            // No bold: the header says itself by its colours (the theme's
            // own on NOW, purple in history). Bold would just brighten the
            // fg on terminals that read bold as bright — and the index's
            // header wears the same colours WITHOUT bold, so keeping it
            // here would make the two screens disagree.
            Style::default().fg(chrome.fg).bg(chrome.bg),
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
        format!(
            "link {}/{}",
            app.cursor - app.lines.len() + 1,
            app.virtual_items.len()
        )
    } else {
        let cur = app
            .cursor_src()
            .map(|s| s + 1)
            .unwrap_or(1)
            .min(app.lines.len());
        format!("L{}/{}", cur, app.lines.len())
    };
    let cur_links = if app.session.is_some() {
        Vec::new()
    } else {
        app.cursor_line_links()
    };
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
            Span::styled(
                format!("{} · {}", pos, hint),
                Style::default().fg(CHROME_DIM),
            ),
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
    let sel_range = move_block_range(app).or_else(|| app.selection.map(|s| s.range()));
    // Telomeres and frame-column carets are painted after the rows.
    let mut gutter: Vec<(u16, &'static str, Style)> = Vec::new();
    let mut carets: Vec<(u16, Style)> = Vec::new();
    // Rows that carry a comment (yellow) or are being commented on right
    // now (the composer's cyan): a thick bar in the frame's left column,
    // akapen's `▌` marker. Painted last, so it wins over the `>` caret —
    // the caret is one row, the bar says which lines the comment covers.
    let mut comment_bars: Vec<(u16, Style)> = Vec::new();
    let composing_range = app.composing.as_ref().map(|_| {
        app.selection
            .map(|s| s.range())
            .unwrap_or((app.cursor, app.cursor))
    });
    // Rows inside a `code:` block. Painted LAST, as a background-only pass:
    // the band has to run to the frame, and the telomere, the thumb and the
    // padding columns are all drawn after the rows. Each entry carries its
    // wash start: the block's content column, so the indent and the bullet
    // keep the page's own background (cosense web paints its code box the
    // same way; the header row is not flagged at all).
    let mut wash_rows: Vec<(u16, u16)> = Vec::new();
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
        // Telomere: age + state of this row's source line (None for
        // synthesized rows). A row BELOW the body is a related page, and it
        // answers the same two questions about itself — how recently it
        // changed, and whether it has been seen — so it wears the same
        // mark. One encoding for the whole viewer: body lines, the index's
        // list, and the related sections.
        let age = row.src().and_then(|s| match app.lines.get(s) {
            Some(l) => Some(((now_secs() - l.updated).max(0), app.line_state(l))),
            None => app.related_telomere(s),
        });
        let in_code = row
            .src()
            .map(|s| code_flags.get(s) == Some(&true))
            .unwrap_or(false);
        let mut base = Style::default();
        // The wash goes down first: selection and the cursor band are
        // stronger signals and paint over it. It starts at the block's
        // content column — `code_span_at` knows the header's indent, so
        // wrapped rows and the edit session's raw rows agree with the
        // laid-out ones. Rows without a code span (should not happen for
        // flagged rows) wash whole, as before.
        if in_code && !in_sel && !in_edit_sel && !is_cursor {
            let mut start: Option<u16> = None;
            for k in 0..h {
                let y = text.y as i32 + screen_y + k;
                if y >= band_top && y <= band_bot {
                    let x = *start.get_or_insert_with(|| {
                        let col = row
                            .src()
                            .and_then(|s| app.code_span_at_line(s))
                            .map(|sp| cosense::render::text_column(sp.header_indent))
                            .unwrap_or(0);
                        // `text` starts three columns into `body`
                        // (frame, telomere, blank); the column counts from
                        // the text, not the frame.
                        text.x.saturating_add(col as u16)
                    });
                    wash_rows.push((y as u16, x));
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
                    // The tint is this page's project's (web's --telomere-*).
                    let (glyph, mut style) = gutter_cell(age, ctx.light, app.telomere_tint);
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
                        let mut caret_style = Style::default()
                            .fg(CHROME_CARET)
                            .add_modifier(Modifier::BOLD);
                        if let Some(bg) = base.bg {
                            caret_style = caret_style.bg(bg);
                        }
                        carets.push((y as u16, caret_style));
                    }
                    if in_composing || has_comment {
                        let color = if in_composing {
                            CHROME_ACCENT
                        } else {
                            Color::Yellow
                        };
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
            Row::Line {
                line,
                src,
                start,
                hang,
            } => {
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
                            let raw = app.lines.get(*src).map(|l| l.text.as_str()).unwrap_or("");
                            let code = caret_span(app, *src);
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
            Row::ImageError {
                msg, indent, item, ..
            } => {
                if let Some(r) = one_row(screen_y) {
                    let pad = bullet_pad(*indent, *item);
                    f.render_widget(
                        Paragraph::new(format!("{pad} {msg}")).style(base.fg(Color::Red)),
                        r,
                    );
                }
            }
            Row::ImageLoading {
                indent, item, url, ..
            } => {
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
            Row::Inline {
                images,
                texts,
                indent,
                item,
                ..
            } => {
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
                        let Some(r) = one_row(screen_y + baseline) else {
                            continue;
                        };
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
            Row::Image {
                url, indent, item, ..
            } => {
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

    // The code wash, run to the frame on the right. Only the background
    // is touched, so the telomere glyph, the scrollbar thumb and the text
    // keep their own colors and simply sit on the block's surface. Each
    // row starts at its block's content column: the indent and the bullet
    // stay on the page's own background.
    let left = body.x.saturating_add(1);
    let right = body.x.saturating_add(body.width).saturating_sub(1);
    for (sy, x0) in wash_rows {
        for x in x0.max(left)..right {
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
            let code = caret_span(app, s.line);
            let disp = session_display(&s.input.buf, code);
            let dcaret = display_caret(&s.input.buf, s.input.cur, code);
            let wrapped = SessionWrap::new(&disp, text_w, session_hang(&s.input.buf, code));
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
                if let (false, Some(c)) = (selecting, f.buffer_mut().cell_mut((x as u16, y as u16)))
                {
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
    // The order menu (S) floats over the page, exactly as the index's `s`
    // menu floats over the list: one order everywhere, applied here to the
    // related sections and remembered as the index's standing choice. It
    // is drawn AFTER the body: drawn before it, the rows painted over the
    // panel and left it torn.
    if app.index.is_none() {
        draw_sort_menu(f, area, app.index_sort, app.index_sort_menu);
    }
    // A toast rides above the footer, over everything else, and leaves by
    // itself (toast.rs).
    draw_toast(f, app, ctx, area);
}
