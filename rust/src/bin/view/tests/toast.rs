use crate::*;
use super::support::*;

    /// 起きたこと(コピーした・失敗した)はトースト、いま何であるか(選択中・
    /// 履歴の位置)は status。トーストが status を押しのけないのが分割の要点。
    #[test]
    fn a_toast_never_displaces_the_standing_status() {
        let mut app = page(&["title", "one"]);
        app.status = "選択中 — j/k で広げる".into();
        app.toast("✓ copied");
        assert_eq!(app.status, "選択中 — j/k で広げる");
        assert_eq!(app.toast_text(), "✓ copied");
        assert!(app.hint_body(&[]).contains("選択中"), "the footer still shows the state");
        assert!(!app.hint_body(&[]).contains("copied"), "and not the toast");

        // 新しいトーストは古いものを置き換える。
        app.toast_err("失敗");
        assert_eq!(app.toast_text(), "失敗");
        assert!(app.toast.as_ref().unwrap().error);
        // 空文は「消す」。
        app.toast("");
        assert_eq!(app.toast_text(), "");
    }

    /// 寿命は描かれた時点から数える。起動中に上がったトースト(設定ファイルが
    /// 読めない)が、画面が出る前に秒数を使い切ってはいけない。
    #[test]
    fn a_toast_expires_from_its_first_frame_not_from_when_it_was_raised() {
        let mut app = page(&["title"]);
        app.toast("hello");
        assert!(!app.expire_toast(), "never drawn: not expired");
        app.toast.as_mut().unwrap().shown = Some(Instant::now() - TOAST_SECS - Duration::from_millis(1));
        assert!(app.expire_toast());
        assert_eq!(app.toast_text(), "");
        assert!(!app.expire_toast(), "nothing left to expire");

        app.toast("bye");
        app.dismiss_toast();
        assert_eq!(app.toast_text(), "", "Esc takes it away at once");
    }

    /// バナーはフッターの1行上、中央寄せ、幅は文+2。画面が狭ければ切り詰め、
    /// 行が無ければ描かない。
    #[test]
    fn the_banner_sits_above_the_footer_and_fits_the_screen() {
        let area = Rect::new(0, 0, 40, 12);
        let r = toast_rect(area, "✓ copied").unwrap();
        assert_eq!(r.y, 10, "one row above the footer");
        assert_eq!(r.width, 10, "text width + 2");
        assert_eq!(r.x, 15, "centred");
        // 全角は2桁。
        assert_eq!(toast_rect(area, "最新").unwrap().width, 6);
        // 長文は画面幅-4に収める。
        let long = "x".repeat(100);
        assert_eq!(toast_rect(area, &long).unwrap().width, 36);
        assert_eq!(clip_to_width(&long, 34), format!("{}…", "x".repeat(33)));
        assert_eq!(clip_to_width("短い", 10), "短い", "nothing to cut");
        assert_eq!(clip_to_width("日本語の長い文", 6), "日本…", "cut on a character boundary");
        assert!(toast_rect(Rect::new(0, 0, 40, 2), "x").is_none(), "no spare row");
    }

    /// 描くと、その行にだけバナーの地色が乗り、フッターのヒントはそのまま。
    /// 索引画面でも同じ位置に出る。
    #[test]
    fn the_toast_is_painted_on_its_own_row_over_page_and_index_alike() {
        use ratatui::{backend::TestBackend, Terminal};
        let ctx = test_ctx();
        let bg = cosense::theme::toast_bg(ctx.terminal_bg);
        // 全角は1セル+続きの空セル。続きのセルは飛ばして文字列にする。
        let row_text = |term: &Terminal<TestBackend>, y: u16| -> String {
            use unicode_width::UnicodeWidthStr;
            let buf = term.backend().buffer();
            let mut out = String::new();
            let mut skip = 0;
            for x in 0..buf.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf.cell((x, y)).unwrap().symbol();
                skip = sym.width().saturating_sub(1);
                out.push_str(sym);
            }
            out
        };

        let mut app = page(&["title", "one"]);
        app.toast("✓ copied");
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let banner = row_text(&term, 10);
        assert!(banner.contains("✓ copied"), "row 10: {banner:?}");
        assert!(row_text(&term, 11).contains("VIEW"), "the footer keeps its badge and hint");
        let buf = term.backend().buffer();
        let at = banner.find('✓').unwrap() as u16;
        assert_eq!(buf.cell((at, 10)).unwrap().style().bg, Some(bg), "the banner's own background");
        assert_ne!(buf.cell((0, 10)).unwrap().style().bg, Some(bg), "the rest of the row is not the banner");
        assert!(app.toast.as_ref().unwrap().shown.is_some(), "the draw started the clock");

        // 索引でも。
        app.index = Some(cosense::index::Index::new(
            vec![cosense::index::Entry { title: "改善案".into(), updated: now_secs(), ..Default::default() }],
            1,
            cosense::index::SortKey::Updated,
        ));
        app.toast("「x」は本文にありません");
        let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        assert!(row_text(&term, 10).contains("本文にありません"), "{:?}", row_text(&term, 10));

        // 期限が切れたら描かれない。
        app.toast.as_mut().unwrap().shown = Some(Instant::now() - TOAST_SECS - Duration::from_secs(1));
        term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        assert!(!row_text(&term, 10).contains("本文にありません"));
    }
