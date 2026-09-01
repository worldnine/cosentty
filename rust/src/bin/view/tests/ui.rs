use crate::*;
use super::support::*;

    #[test]
    fn navigating_away_from_a_live_room_goes_back_to_the_fast_poll() {
        let ctx = test_ctx();
        let mut app = page(&["a"]);
        app.title = "t".into();
        app.ws_attempted = true;
        app.status = "認証: sid · 編集可 · 同期: poll · ? ヘルプ".into();
        handle_ws_event(
            &mut app,
            &ctx,
            WsEvent::State {
                project: "proj".into(),
                title: "t".into(),
                state: SyncState::Live,
            },
        );
        assert_eq!(app.sync_state, SyncState::Live);
        assert!(app.status.contains("同期: ws"));

        // The reader moves to another page. The old room is gone; nothing
        // has joined the new one yet, so the insurance poll must be fast.
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "other".into(),
                page_id: "P2".into(),
                header_colors: HeaderColors::fallback(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                read_at: None,
                editable: true,
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert_eq!(app.sync_state, SyncState::Polling);
        let rx = app.poll_ctrl_rx_for_test();
        let mut last = None;
        while let Ok(d) = rx.try_recv() {
            last = Some(d);
        }
        assert_eq!(last, Some(Duration::from_secs(3)), "the poller is retuned at once");
    }

    #[test]
    fn moving_to_another_project_forgets_the_old_one_s_verdict() {
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.caps.visibility = capability::Visibility::Private;
        app.caps.browser_denied = true;
        app.caps.sid = true;
        app.set_page(
            Loaded {
                project: "other".into(),
                title: "t".into(),
                page_id: "P2".into(),
                header_colors: HeaderColors::fallback(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                read_at: None,
                editable: true,
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert_eq!(app.caps.visibility, capability::Visibility::Unknown);
        assert!(!app.caps.browser_denied);
        assert!(app.caps.sid, "the session's cookie did not go anywhere");
    }

    #[test]
    fn set_page_clears_the_state_a_dropped_job_would_have_answered() {
        // The worker drops stale-generation jobs without replying, which is
        // only safe because installing a page clears what those replies
        // would have cleared. This pins that invariant.
        let ctx = test_ctx();
        let mut app = mermaid_page();
        app.rebuild(80);
        app.start_web_renders(capability::Trigger::Auto);
        app.web_rescaling.insert("web:mermaid:whatever".into());
        assert!(!app.web_pending.is_empty());
        app.set_page(
            Loaded {
                project: "proj".into(),
                title: "next".into(),
                header_colors: HeaderColors::fallback(),
                page_id: "p2".into(),
                lines: vec![],
                blocks: vec![],
                srcs: vec![],
                hits: vec![],
                read_at: None,
                editable: true,
                related: Vec::new(),
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert!(app.web_pending.is_empty(), "no request outlives the page it was for");
        assert!(app.web_rescaling.is_empty());
    }

    /// READ の枠は ANSI ロール色、EDIT では同じ枠がヘッダのアクセント色に
    /// 変わる。「枠が消える」のではなく「色が変わる」のがモードサイン。
    #[test]
    fn edit_session_recolors_the_page_frame_with_the_header_accent() {
        let header = HeaderColors { fg: Color::Black, bg: Color::Rgb(20, 120, 200) };
        let read = page_frame_style(false, header);
        assert_eq!(read.fg, Some(Color::DarkGray));
        assert_eq!(read.bg, None);
        assert!(!matches!(read.fg, Some(Color::Rgb(..))), "READ chrome must follow ANSI palette");
        let edit = page_frame_style(true, header);
        assert_eq!(edit.fg, Some(header.bg), "EDIT frame wears the page's own colour");
        assert_eq!(edit.bg, None);
    }

    /// EDIT を見分ける残り二つのサイン: フッタのモードタグはヘッダ配色の
    /// バッジになり、キャレットのセルはソフト描画(REVERSED)でも塗られる
    /// — ハードウェアカーソルの形状変更に応えない端末のための併走。
    #[test]
    fn edit_shows_a_header_colored_badge_and_a_software_caret() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "hello world"]);
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();

        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let footer_y = 9;
        let read_tag = term.backend().buffer().cell((1, footer_y)).unwrap().clone();
        assert_ne!(read_tag.bg, app.header_colors.bg, "READ tag stays quiet");

        enter_session(&mut app, &ctx, 1, 0);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer().clone();
        let badge = buf.cell((1, footer_y)).unwrap();
        assert_eq!(badge.bg, app.header_colors.bg, "EDIT tag wears the header colours");
        assert_eq!(badge.fg, app.header_colors.fg);

        // キャレット行のどこかのセルが REVERSED で塗られていること。
        let reversed = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| {
                buf.cell((x, y))
                    .map(|c| c.modifier.contains(Modifier::REVERSED))
                    .unwrap_or(false)
            })
        });
        assert!(reversed, "the caret cell is painted, not only the hardware cursor");
    }

    /// EDIT の行またぎ文字選択は、キャレット行だけでなく全ての行で選択
    /// された文字そのものが反転する: アンカーの行は境界の文字から行末
    /// まで、間の行は本文全部。帯だけでは「どこから選んだか」「途中の行が
    /// 入っているか」が読めなかった。
    #[test]
    fn a_cross_line_selection_reverses_its_characters_on_every_line() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.rebuild(40);
        // アンカーは 1 行目 "one" の 'e' の手前(byte 2)。そこから下へ
        // 2 行伸ばすと、キャレットは 3 行目 "three" に居る。
        enter_session(&mut app, &ctx, 1, 2);
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        assert_eq!(app.session.as_ref().unwrap().sel_from, Some((1, 2)));

        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer().clone();

        // 行の本文セルを (シンボル, REVERSED か) で拾う。
        let row_cells = |needle: &str| -> Vec<(String, bool)> {
            for y in 0..buf.area.height {
                let text: String = (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect();
                if text.contains(needle) {
                    let x0 = (0..buf.area.width)
                        .find(|&x| {
                            let rest: String = (x..buf.area.width)
                                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                                .collect();
                            rest.starts_with(needle)
                        })
                        .unwrap();
                    return (x0..x0 + needle.len() as u16)
                        .map(|x| {
                            let c = buf.cell((x, y)).unwrap();
                            (c.symbol().to_string(), c.modifier.contains(Modifier::REVERSED))
                        })
                        .collect();
                }
            }
            panic!("row containing {needle:?} not on screen");
        };

        // アンカー行: 'on' は選択外、'e' から行末が反転。
        let one = row_cells("one");
        assert!(!one[0].1 && !one[1].1, "before the anchor stays plain: {one:?}");
        assert!(one[2].1, "from the anchor on it reads as selected: {one:?}");

        // 間の行: 本文の全文字が反転。
        let two = row_cells("two");
        assert!(two.iter().all(|(_, r)| *r), "a middle line is wholly selected: {two:?}");

        // EDIT の文字選択は行の帯(READ の選択色)を敷かない: 文字の
        // 反転だけが選択の形を語る。
        let sel = selection_bg(ctx.terminal_bg);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                assert_ne!(
                    buf.cell((x, y)).unwrap().bg,
                    sel,
                    "no line band in EDIT char selection (cell {x},{y})"
                );
            }
        }
    }

    /// The block being carried is marked on screen the way a selection is:
    /// the same highlight, on every line of it. (The mode's name and its
    /// keys are in the footer, so this colour never carries the news
    /// alone.)
    #[test]
    fn the_grabbed_block_is_marked_like_a_selection() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", " parent", "  first", "   child", "  second"]);
        app.cursor = 2;
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let highlighted = |app: &mut App, term: &mut Terminal<TestBackend>| -> usize {
            term.draw(|f| ui(f, app, &ctx)).unwrap();
            let x = app.text_rect.x;
            let buf = term.backend().buffer().clone();
            (0..buf.area.height)
                .filter(|&y| buf.cell((x, y)).map(|cell| cell.bg == selection_bg(ctx.terminal_bg)).unwrap_or(false))
                .count()
        };

        let cursor_band = highlighted(&mut app, &mut term);
        handle_key(&mut app, &ctx, key(KeyCode::Char('m')));
        assert_eq!(
            highlighted(&mut app, &mut term),
            cursor_band + 1,
            "the grabbed child line is marked next to the cursor line"
        );

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert_eq!(
            highlighted(&mut app, &mut term),
            cursor_band,
            "and the mark goes when the block is let go"
        );
    }

    /// Mixing text into an indented line must not cost it its bullet: it
    /// is still an item at that level, whatever it holds.
    #[test]
    fn an_indented_line_keeps_its_bullet_when_it_holds_a_picture() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let url = "https://example.com/a.png";
        let mut app = page(&["t", &format!(" [{url}]ワイワイ")]);
        app.images
            .insert(url.to_string(), decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap());
        app.rebuild(40);
        // The row knows it is an item, at its level's column.
        let row = app
            .content_view(40)
            .into_iter()
            .find_map(|r| match r {
                Row::Inline { item, indent, .. } => Some((item, indent)),
                _ => None,
            })
            .expect("an inline row");
        assert_eq!(row, (true, 2));

        // …and the bullet really is drawn.
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let has_bullet = (0..12).any(|y| {
            (0..40).any(|x| buf.cell((x, y)).map(|c| c.symbol() == BULLET).unwrap_or(false))
        });
        assert!(has_bullet, "the item lost its bullet");
    }

    /// The browser lays an inline image ON the text line: its bottom edge
    /// level with the words, so a sentence reads straight through it. The
    /// same layout answers all three shapes — text then picture, picture
    /// then text, and two pictures with words between them.
    #[test]
    fn a_line_of_text_and_pictures_is_laid_out_like_one_sentence() {
        let txt = |s: &str| Inline::Text(Line::from(s.to_string()));
        let img = |name: &str, w: u16, h: u16| Inline::Image { url: name.into(), w, h };

        // Picture first, then text: the words ride the picture\'s last row.
        let (imgs, texts, h) = layout_inline(&[img("a", 10, 6), txt("こんな感じ")], 0, 60);
        assert_eq!(h, 6, "the box is as tall as the picture");
        assert_eq!(imgs[0].row, 0);
        assert_eq!(texts[0].row, 5, "on the picture\'s last row — its baseline");
        assert_eq!(texts[0].col, 10, "right after it");

        // Text first, then picture: the picture grows UPWARD from the text
        // line, which is what an inline image does in a browser.
        let (imgs, texts, h) = layout_inline(&[txt("先に本文 "), img("a", 10, 4)], 0, 60);
        assert_eq!(h, 4);
        assert_eq!(texts[0].row, 3, "the text sits on the baseline");
        assert_eq!(imgs[0].row, 0, "and the picture reaches up from it");
        assert!(imgs[0].col >= 6, "after the words: {}", imgs[0].col);

        // Two pictures with text between them, side by side on one line.
        let (imgs, texts, h) =
            layout_inline(&[img("a", 10, 4), txt(" と "), img("b", 8, 6)], 0, 60);
        assert_eq!(imgs.len(), 2);
        assert_eq!(h, 6, "the tallest picture sets the height");
        assert_eq!(imgs[0].row, 2, "the shorter one is bottom-aligned with it");
        assert_eq!(imgs[1].row, 0);
        assert!(imgs[1].col > imgs[0].col, "in the order written");
        assert_eq!(texts[0].row, 5, "the words are on the shared baseline");

        // Out of room: the next picture starts a new box below.
        let (imgs, _, h) = layout_inline(&[img("a", 20, 3), img("b", 20, 3)], 0, 30);
        assert_eq!(imgs[0].row, 0);
        assert_eq!(imgs[1].row, 3, "wrapped under the first");
        assert_eq!(h, 6);

        // An indented line puts everything at its own column.
        let (imgs, texts, _) = layout_inline(&[img("a", 6, 2), txt("と本文")], 4, 40);
        assert_eq!(imgs[0].col, 4);
        assert_eq!(texts[0].col, 10);
    }

    /// An indented picture is a LIST ITEM: cosense web draws the bullet
    /// beside it, and without one the picture floats free of the item it
    /// belongs to.
    #[test]
    fn an_indented_picture_gets_its_bullet() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let url = "https://example.com/a.png";
        let mut app = page(&["t", &format!(" [{url}]"), "  after"]);
        // Pretend the picture has landed.
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap();
        app.images.insert(url.to_string(), info);
        app.rebuild(40);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let row_text = |y: u16| -> String {
            (0..40).map(|x| buf.cell((x, y)).unwrap().symbol()).collect()
        };
        // The picture is painted by the protocol; its bullet is ours.
        let bulleted: Vec<String> =
            (0..12).map(row_text).filter(|r| r.contains(BULLET)).collect();
        assert!(!bulleted.is_empty(), "a bullet is drawn for the picture");
        let col = bulleted[0].find(BULLET).unwrap();
        assert!(col >= 3, "at the item's own indent: {:?}", bulleted[0]);

        // A picture with text on the same line is not a picture ROW at all:
        // it is laid out inline with the words, so there is no bullet to
        // place — the sentence itself shows what it belongs to.
        let mut app = page(&["t", &format!(" [{url}]と本文が続く")]);
        app.images
            .insert(url.to_string(), decode_web_png(&Picker::halfblocks(), &tiny_png(), 8).unwrap());
        app.rebuild(40);
        let rows = app.content_view(40);
        assert!(
            rows.iter().any(|r| matches!(r, Row::Inline { images, texts, .. }
                if images.len() == 1 && !texts.is_empty())),
            "one inline row carrying both",
        );
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Image { .. })),
            "and no bare picture row to hang a bullet off",
        );

        // The placeholder shown before it lands wears the same lead-in.
        assert_eq!(bullet_pad(2, true), format!("{BULLET} "));
        assert_eq!(bullet_pad(4, true), format!("  {BULLET} "));
        assert_eq!(bullet_pad(0, true), "", "a flush picture has no bullet");
        assert_eq!(bullet_pad(4, false), "    ", "a hanging picture has none either");
    }

    #[test]
    fn page_chrome_separates_caret_telomere_pad_and_top_scrollbar() {
        use ratatui::{backend::TestBackend, Terminal};

        let texts: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut app = page(&refs);
        app.header_colors = HeaderColors {
            fg: Color::White,
            bg: Color::Rgb(80, 102, 184),
        };
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 12)).unwrap();
        // The first draw establishes wrapped rows; the next is the stable
        // frame the event loop presents.
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            // body starts at y=1; its first content row is y=2.
            assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::White);
            assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(80, 102, 184));
            assert_eq!(buf.cell((0, 2)).unwrap().symbol(), ">");
            assert_eq!(buf.cell((0, 2)).unwrap().fg, Color::LightBlue);
            assert_ne!(buf.cell((1, 2)).unwrap().symbol(), " ", "telomere remains visible");
            assert_eq!(buf.cell((2, 2)).unwrap().symbol(), " ", "one blank after telomere");
            assert_eq!(buf.cell((3, 2)).unwrap().symbol(), "l", "text starts after the blank");
            for x in 1..=40 {
                assert_eq!(
                    buf.cell((x, 2)).unwrap().bg,
                    Color::DarkGray,
                    "cursor band must fill every inside column at x={x}",
                );
            }
            // The thumb starts below the top frame rule instead of overwriting it.
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "─");
            assert_eq!(buf.cell((40, 1)).unwrap().fg, Color::DarkGray);
            assert_eq!(buf.cell((40, 2)).unwrap().symbol(), "▐");
            assert_eq!(buf.cell((40, 2)).unwrap().fg, Color::Gray);
        }

        // Once the top rule scrolls away, screen row 2 (y=1) is reclaimed
        // by every page layer instead of remaining a permanent blank strip.
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = terminal.backend().buffer();
            assert_eq!(buf.cell((0, 1)).unwrap().symbol(), ">");
            assert_ne!(buf.cell((1, 1)).unwrap().symbol(), " ", "telomere at top row");
            assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "l", "text at top row");
            assert_eq!(buf.cell((40, 1)).unwrap().symbol(), "▐", "scrollbar at top row");
        }
        app.cursor = 5;
        handle_mouse_content(
            &mut app,
            &ctx,
            mouse(MouseEventKind::Down(MouseButton::Left), 8, 1),
        );
        assert_eq!(app.cursor, 0, "reclaimed top row remains mouse-addressable");

        enter_session(&mut app, &ctx, 0, 0);
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        // EDIT keeps the frame and recolors it with the header accent:
        // the frame itself is the mode signal now. (The top rule is still
        // scrolled off here, as in the READ passage above — the sides are
        // what remains on screen.)
        assert_eq!(buf.cell((41, 3)).unwrap().symbol(), "│", "EDIT keeps the side frame");
        assert_eq!(buf.cell((41, 3)).unwrap().fg, app.header_colors.bg);
    }
