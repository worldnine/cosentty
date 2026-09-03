use crate::*;
use super::support::*;

    /// 一致した語には敷きと太字が付き、そうでない字には付かない。
    /// 「なぜこの行がここにあるか」を画面が答える印なので、消えたら
    /// 検索結果はただの一覧に戻ってしまう。
    #[test]
    fn the_matched_word_wears_a_wash_and_the_rest_of_the_title_does_not() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.project = "proj".into();
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let wash = cosense::theme::match_wash(ctx.terminal_bg);

        let mut ix = cosense::index::Index::new(
            vec![cosense::index::Entry {
                title: "改善案".into(),
                updated: now_secs(),
                ..Default::default()
            }],
            1,
            cosense::index::SortKey::Updated,
        );
        ix.set_filter("改善".into());
        app.index = Some(ix);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer();
        // 行は " █  0s 改善案" の形。タイトルの開始桁を探す。カーソル行
        // (帯が敷いてある)でも、一致の敷きはその上に乗って見分けがつく。
        let row = 1u16;
        let text: String = (0..buf.area.width)
            .map(|x| buf.cell((x, row)).unwrap().symbol().to_string())
            .collect();
        let at = text.find("改").expect("タイトルが描かれている") as u16;
        // ratatui は全角を1セル+空セルで置くので、桁は先頭だけ見る。
        let marked = buf.cell((at, row)).unwrap().style();
        assert_eq!(marked.bg, Some(wash), "一致には敷きが付く");
        assert!(marked.add_modifier.contains(Modifier::BOLD));
        // 「案」は一致していないので、敷きは付かない。
        let plain = buf.cell((at + 4, row)).unwrap().style();
        assert_ne!(plain.bg, Some(wash), "一致していない字に印は付かない");
    }

    /// 日本語は端末の IME が「変換窓」をハードウェアカーソルの位置に出す。
    /// だから文字を打ち込める場所は、必ずそこにカーソルを置かなければ
    /// ならない。EDIT セッションは元からそうしていたが、一覧の絞り込み行と
    /// コメント入力欄は置いておらず、変換中の文字が見当違いの場所に出ていた。
    #[test]
    fn the_hardware_cursor_sits_on_the_caret_wherever_japanese_can_be_typed() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.project = "proj".into();
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();

        // （何も打てない READ で隠れることは TestBackend では見えない——
        //  ratatui が show/hide を投げるだけで、位置は保持される。実機の
        //  pty では hidden=True になるのを確認済み。）

        // --- 一覧の絞り込み行 ---
        let mut ix = cosense::index::Index::new(
            vec![cosense::index::Entry {
                title: "改善案".into(),
                updated: now_secs(),
                ..Default::default()
            }],
            1,
            cosense::index::SortKey::Updated,
        );
        ix.begin_filter();
        ix.push_filter('改');
        ix.push_filter('善');
        app.index = Some(ix);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        // " proj — /" は9桁、全角2文字で4桁。キャレットはその次。
        assert_eq!(term.get_cursor_position().unwrap().x, 13);
        assert_eq!(term.get_cursor_position().unwrap().y, 0);
        app.index = None;

        // --- コメント入力欄 ---
        // 入力欄はカーソル行(title の次、行 1)の直下に割って入る:
        // 罫線1行、本文1行。本文の行にキャレットがある。
        app.cursor = 1;
        let mut input = Input::new(String::new());
        input.insert_char('あ');
        input.insert_char('い');
        app.composing = Some(input);
        app.laid_width = 0;
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let pos = term.get_cursor_position().unwrap();
        let text_x = app.text_rect.x;
        assert_eq!(pos.x, text_x + 4, "全角は2桁で数える");
        // ヘッダ1・上罫線1・title・one・入力欄の上罫線 → 本文は行 5。
        assert_eq!(pos.y, app.text_rect.y + 3);
    }

    /// コメント入力欄は akapen と同じく、コメントする範囲の最終行の直下に
    /// 割って入る(フッターの下に出るのではない)。シアンの罫線に挟まれ、
    /// ラベルは ` comment · 2-3 `。同じ範囲の既存コメントを編集するときは
    /// ` edit · ` になり、そのカードは入力欄に置き換わる(2段にならない)。
    #[test]
    fn the_composer_opens_under_the_commented_range_and_replaces_the_card_it_edits() {
        let mut app = page(&["title", "one", "two", "three"]);
        app.rebuild(40);
        let plain = |r: &Row| -> String {
            match r {
                Row::Composer { line, .. } | Row::Card { line } => {
                    line.spans.iter().map(|s| s.content.as_ref()).collect()
                }
                _ => String::new(),
            }
        };
        app.selection = Some(Selection { anchor: 1, cursor: 2 });
        app.composing = Some(Input::new("メモ".into()));
        app.rebuild(40);
        let comp: Vec<usize> = app.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Composer { .. })).map(|(i, _)| i).collect();
        assert_eq!(comp.len(), 3, "rule, one body row, rule: {comp:?}");
        // 直前の行は範囲の最終行(two = src 2)、直後は three。
        assert_eq!(app.rows[comp[0] - 1].src(), Some(2));
        assert_eq!(app.rows[comp[2] + 1].src(), Some(3));
        assert!(plain(&app.rows[comp[0]]).starts_with(" comment · 2-3 ─"), "{}", plain(&app.rows[comp[0]]));
        assert_eq!(plain(&app.rows[comp[1]]), "メモ");
        assert!(matches!(app.rows[comp[1]], Row::Composer { caret: Some(4), .. }), "caret after 2 wide chars");
        assert!(matches!(app.rows[comp[0]], Row::Composer { caret: None, .. }));

        // 保存するとカードになる(同じ形、色だけ変わる)。
        let c = app.make_comment("メモ".into()).unwrap();
        app.comments.push(c);
        app.composing = None;
        app.rebuild(40);
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Composer { .. })));
        let cards: Vec<&Row> = app.rows.iter().filter(|r| matches!(r, Row::Card { .. }) && !plain(r).is_empty()).collect();
        assert!(plain(cards[0]).starts_with(" comment · 2-3 ─"), "{}", plain(cards[0]));
        assert_eq!(plain(cards[1]), "メモ");

        // 同じ範囲でもう一度: edit ラベルの入力欄がカードの場所に立ち、カードは隠れる。
        app.composing = Some(Input::new("メモ".into()));
        app.rebuild(40);
        let comp: Vec<&Row> = app.rows.iter().filter(|r| matches!(r, Row::Composer { .. })).collect();
        assert!(plain(comp[0]).starts_with(" edit · 2-3 ─"), "{}", plain(comp[0]));
        assert!(!app.rows.iter().any(|r| matches!(r, Row::Card { .. }) && plain(r).starts_with(" comment")), "the edited card gives way to the bar");
    }

    /// 入力欄の本文は列幅で折り返し、キャレットの位置は折り返し後の
    /// (行, 桁)で報告する。行がいっぱいのときのキャレットは次の行の頭
    /// (次に打つ字がそこへ行くから)。末尾だけは最終行の右端になりうる。
    #[test]
    fn the_composer_body_wraps_and_reports_where_the_caret_landed() {
        let (rows, caret) = wrap_with_caret("abcdefgh", 3, 4);
        assert_eq!(rows, vec!["abcd", "efgh"]);
        assert_eq!(caret, (0, 3));
        assert_eq!(wrap_with_caret("abcdefgh", 4, 4).1, (1, 0), "at a full row's end the caret is where the next char will go");
        assert_eq!(wrap_with_caret("abcdefgh", 5, 4).1, (1, 1));
        assert_eq!(wrap_with_caret("abcdefgh", 8, 4).1, (1, 4));
        assert_eq!(wrap_with_caret("", 0, 4), (vec![String::new()], (0, 0)));
        assert_eq!(wrap_with_caret("あいう", 2, 4).1, (1, 0), "a wide char that does not fit starts the next row");
    }

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
                project_display: String::new(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                facts: PageFacts::default(),
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
                project_display: String::new(),
                lines: Vec::new(),
                blocks: Vec::new(),
                srcs: Vec::new(),
                hits: Vec::new(),
                related: Vec::new(),
                facts: PageFacts::default(),
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
                project_display: String::new(),
                page_id: "p2".into(),
                lines: vec![],
                blocks: vec![],
                srcs: vec![],
                hits: vec![],
                read_at: None,
                editable: true,
                related: Vec::new(),
                facts: PageFacts::default(),
                links: LinkTruth::default(),
            },
            &ctx,
        );
        assert!(app.web_pending.is_empty(), "no request outlives the page it was for");
        assert!(app.web_rescaling.is_empty());
    }

    /// READ のページ枠はヘッダ自身のアクセント色をまとう(サイトでページが
    /// プロジェクト色の中に置かれているのと同じ)。EDIT は枠を持たず、
    /// 本文全域の下敷きがモードを語る(描画側のテストは別)。
    #[test]
    fn the_read_frame_wears_the_pages_own_accent() {
        let header = HeaderColors { fg: Color::Black, bg: Color::Rgb(20, 120, 200) };
        let read = page_frame_style(header);
        assert_eq!(read.fg, Some(header.bg), "the frame is the page's colour");
        assert_eq!(read.bg, None);
    }

    /// EDIT では行カーソル `>` を出さず、帯もキャレットが実際に届く
    /// 本文領域だけに塗る: テロメアやその右の余白、スクロールバー列には
    /// キャレットは届かないので、そこまで帯を伸ばさない。READ は従来
    /// どおり行全体がひとつの帯。
    #[test]
    fn edit_drops_the_line_caret_and_bands_only_where_the_caret_can_go() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.cursor = 1;
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();

        let cursor_row_y = |app: &App| -> u16 {
            let (first, _) = app.src_rows(1).unwrap();
            // text.y = 2(ヘッダ+枠上辺)から scroll 0 で並ぶ。
            app.text_rect.y + first as u16 - app.scroll
        };

        // READ: `>` が左フレーム列に出て、帯はテロメア列にも及ぶ。
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = term.backend().buffer();
            let y = cursor_row_y(&app);
            assert_eq!(buf.cell((0, y)).unwrap().symbol(), ">");
            assert_eq!(buf.cell((1, y)).unwrap().bg, CURSOR_BG, "telomere joins the READ band");
        }

        // EDIT: `>` は消え、テロメア列・右端(スクロールバー列)は素のまま。
        // 本文領域は帯のまま。
        enter_session(&mut app, &ctx, 1, 0);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            let buf = term.backend().buffer();
            let y = cursor_row_y(&app);
            assert_ne!(buf.cell((0, y)).unwrap().symbol(), ">", "no line caret in EDIT");
            assert_ne!(buf.cell((1, y)).unwrap().bg, CURSOR_BG, "telomere stays bare");
            let right = app.text_rect.x + app.text_rect.width;
            assert_ne!(buf.cell((right, y)).unwrap().bg, CURSOR_BG, "right margin stays bare");
            assert_eq!(
                buf.cell((app.text_rect.x, y)).unwrap().bg,
                CURSOR_BG,
                "the caret's runway keeps the band"
            );
        }
    }

    /// EDIT の下敷き: 編集中は本文全域に、カーソル行の帯より暗い背景が
    /// 敷かれ(紙の色が変わる)、ヘッダに [✎ 編集中] が出る。キャレット
    /// 行の帯はその上に明るく浮く。読んでいる画面と書いている画面が常に
    /// 違って見えることが、EDIT に居ることを忘れて j/k を打ってしまう
    /// 事故への持続的な防波堤。
    #[test]
    fn edit_lays_a_darker_backdrop_and_says_so_in_the_header() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let backdrop = cosense::theme::edit_backdrop(ctx.terminal_bg);

        let bg_of = |buf: &ratatui::buffer::Buffer, needle: &str| -> Color {
            for y in 0..buf.area.height {
                let text: String = (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect();
                if text.contains(needle) {
                    let x = (0..buf.area.width)
                        .find(|&x| {
                            let rest: String = (x..buf.area.width)
                                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                                .collect();
                            rest.starts_with(needle)
                        })
                        .unwrap();
                    return buf.cell((x, y)).unwrap().bg;
                }
            }
            panic!("{needle:?} not on screen");
        };
        // ワイド文字の継続セルには古い内容が残ることがあるので、
        // 文字幅ぶんセルを読み飛ばして画面を文字列化する。
        let screen = |term: &Terminal<TestBackend>| -> String {
            let buf = term.backend().buffer();
            let mut out = String::new();
            for y in 0..buf.area.height {
                let mut skip = 0usize;
                for x in 0..buf.area.width {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    let sym = buf.cell((x, y)).unwrap().symbol().to_string();
                    skip = str_width(&sym).saturating_sub(1);
                    out.push_str(&sym);
                }
            }
            out
        };

        // READ: 下敷きは無い。
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_ne!(bg_of(&buf, "two"), backdrop, "READ lays no backdrop");
        assert!(!screen(&term).contains("編集中"));

        // EDIT: キャレット行(one)は帯、他(two)は下敷きの上に居る。
        enter_session(&mut app, &ctx, 1, 0);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(bg_of(&buf, "one"), CURSOR_BG, "the caret line floats on its band");
        assert_eq!(bg_of(&buf, "two"), backdrop, "every other line sits on the backdrop");
        assert!(!screen(&term).contains("編集中"), "the header adds no third badge: backdrop, band and footer say it");
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
        let mut app = page(&["title", "one", "two", "", "five"]);
        app.rebuild(40);
        // アンカーは 1 行目 "one" の 'e' の手前(byte 2)。そこから下へ
        // 3 行伸ばすと、キャレットは "five" に居て、間には "two" と
        // 空行が挟まる。
        enter_session(&mut app, &ctx, 1, 2);
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_session_key(&mut app, &ctx, shift(KeyCode::Down));
        assert_eq!(app.session.as_ref().unwrap().sel_from, Some((1, 2)));

        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = term.backend().buffer().clone();

        // 行の本文セルを (画面行, [(シンボル, REVERSED か, 背景)]) で拾う。
        let row_cells = |needle: &str| -> (u16, Vec<(String, bool, Color)>) {
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
                    let cells = (x0..x0 + needle.len() as u16)
                        .map(|x| {
                            let c = buf.cell((x, y)).unwrap();
                            (
                                c.symbol().to_string(),
                                c.modifier.contains(Modifier::REVERSED),
                                c.bg,
                            )
                        })
                        .collect();
                    return (y, cells);
                }
            }
            panic!("row containing {needle:?} not on screen");
        };

        // アンカー行: 'on' は選択外、'e' から行末が反転。
        let (_, one) = row_cells("one");
        assert!(!one[0].1 && !one[1].1, "before the anchor stays plain: {one:?}");
        assert!(one[2].1, "from the anchor on it reads as selected: {one:?}");

        // 間の行: 本文の全文字が反転し、カーソル行と同じグレーの帯が乗る。
        let (two_y, two) = row_cells("two");
        assert!(two.iter().all(|(_, r, _)| *r), "a middle line is wholly selected: {two:?}");
        assert!(
            two.iter().all(|(_, _, bg)| *bg == CURSOR_BG),
            "the band is the cursor grey: {two:?}"
        );

        // 空行にも帯: 反転する文字が無くても、選択に入っていることは見える。
        // "two" の直下の画面行を、本文領域内の列で突く。
        let blank_probe = buf.cell((app.text_rect.x + 2, two_y + 1)).unwrap();
        assert_eq!(blank_probe.bg, CURSOR_BG, "a blank line still wears the band");

        // READ の選択色はどこにも出ない: EDIT の帯はグレー一色。
        let sel = selection_bg(ctx.terminal_bg);
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                assert_ne!(
                    buf.cell((x, y)).unwrap().bg,
                    sel,
                    "EDIT char selection never wears the READ colour (cell {x},{y})"
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
            assert_eq!(
                buf.cell((40, 1)).unwrap().fg,
                app.header_colors.bg,
                "READ frame wears the page's accent"
            );
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
        // EDIT has no frame: the backdrop (a paper darker than the cursor
        // band) is the mode signal, and it runs to the pane's edge.
        assert_ne!(buf.cell((41, 3)).unwrap().symbol(), "│", "EDIT drops the frame");
        assert_eq!(
            buf.cell((41, 3)).unwrap().bg,
            cosense::theme::edit_backdrop(ctx.terminal_bg),
            "the backdrop reaches the edge"
        );
    }

    /// ヘッダは左に `正式名称 / タイトル`、右端に「状態で、ほかに印が無いもの」
    /// だけ(未同期・読み取り専用)。コメント数・編集中・ソース・選択情報・未読は
    /// 別の場所が言うので載せない。
    #[test]
    fn the_header_keeps_the_name_and_only_the_unsigned_states() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.project = "slug-1234".into();
        app.project_display = "研究ノート".into();
        app.read_at = None; // a first visit: the telomere says so, not the header
        app.comments.push(Comment {
            project: "slug-1234".into(),
            title: "title".into(),
            start: 1,
            end: 1,
            line_texts: vec!["one".into()],
            line_ids: vec!["id1".into()],
            text: "c".into(),
        });
        let mut term = Terminal::new(TestBackend::new(50, 8)).unwrap();
        let header = |term: &Terminal<TestBackend>| -> String {
            let buf = term.backend().buffer();
            (0..buf.area.width).map(|x| buf.cell((x, 0)).unwrap().symbol().to_string()).collect::<String>()
        };
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let h = header(&term).replace(' ', "");
        assert!(h.starts_with(&format!("研究ノート/{}", app.title)), "{h:?}");
        assert!(!h.contains("slug"), "the slug gives way to the proper name");
        assert!(!h.contains("コメント"), "the count lives behind `l`");
        assert!(h.ends_with(&app.title), "nothing at the right end: no unread badge either: {h:?}");

        app.web_unsynced = true;
        app.editable = false;
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let h = header(&term).replace(' ', "");
        assert!(h.ends_with("未同期·読み取り専用"), "{h:?}");
        assert_eq!(header_line("s", "t", "b ", 10), " s / t  b ", "right flush when it fits");
        // 足りないときはまずサイト名が丸ごと消え、`/ タイトル` が残る。
        assert_eq!(header_line("研究ノート", "設計", "4/4 ", 12), " / 設計 4/4 ", "the name goes first, whole");
        // それでも足りなければタイトルを … で削る。バッジは動かない。
        assert_eq!(header_line("研究ノート", "日本語のタイトル", "3/12 ", 12), " / 日… 3/12 ", "then the title is cut");
    }

    /// ヘッダ右端の日時は「いま見ているページが書かれた時」。NOW では最新行の
    /// 更新時刻、履歴では快照の時刻と位置に入れ替わるだけで、場所は同じ。
    /// 履歴に入るとヘッダと枠が紫になる(言葉ではなく縁の色で分かる)。
    #[test]
    fn the_header_date_slides_from_now_into_history_and_the_chrome_turns_purple() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.lines[1].updated = 1_700_000_000;
        app.lines[2].updated = 1_700_000_060; // the newest line is the page's `updated`
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let header = |term: &Terminal<TestBackend>| -> String {
            let buf = term.backend().buffer();
            (0..buf.area.width).map(|x| buf.cell((x, 0)).unwrap().symbol().to_string()).collect::<String>().replace(' ', "")
        };
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let now_stamp = cosense::theme::format_local(1_700_000_060).replace(' ', "");
        assert!(header(&term).ends_with(&now_stamp), "no list yet: the date alone: {:?}", header(&term));
        // 快照一覧が届くと NOW が最後の位置として数えられる: 3件なら 4/4。
        app.snapshots = Some((0..3).map(|i| cosense::api::SnapshotStamp { id: format!("s{i}"), created: i }).collect());
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        assert!(header(&term).ends_with(&format!("4/4·{now_stamp}")), "{:?}", header(&term));
        let live_bg = term.backend().buffer().cell((0, 0)).unwrap().style().bg;
        assert_eq!(live_bg, Some(app.header_colors.bg));

        in_history(&mut app);
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let h = header(&term);
        let old_stamp = cosense::theme::format_local(1).replace(' ', "");
        assert!(h.ends_with(&format!("1/2·{old_stamp}")), "one snapshot + NOW = 2; the oldest is 1: {h:?}");
        assert!(!h.contains('⏪'), "no emoji arrows");
        let (_, purple) = cosense::theme::history_header_colors(ctx.terminal_bg);
        let buf = term.backend().buffer();
        assert_eq!(buf.cell((0, 0)).unwrap().style().bg, Some(purple), "header goes purple");
        assert_ne!(Some(purple), live_bg);
        // 枠の縦線(本文左端の列)も同じ紫。
        let frame_fg = buf.cell((0, 3)).unwrap().style().fg;
        assert_eq!(frame_fg, Some(purple), "so does the page frame");
    }
