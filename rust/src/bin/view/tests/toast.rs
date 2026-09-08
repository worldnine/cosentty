use super::support::*;
use crate::*;

/// 起きたこと(コピーした・失敗した)はトースト、いま何であるか(選択中・
/// 履歴の位置)は status。トーストが status を押しのけないのが分割の要点。
#[test]
fn a_toast_never_displaces_the_standing_status() {
    let mut app = page(&["title", "one"]);
    app.status = "選択中 — j/k で広げる".into();
    app.toast("✓ copied");
    assert_eq!(app.status, "選択中 — j/k で広げる");
    assert_eq!(app.toast_text(), "✓ copied");
    assert!(
        app.hint_body(&[]).contains("選択中"),
        "the footer still shows the state"
    );
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
    app.toast.as_mut().unwrap().shown =
        Some(Instant::now() - TOAST_SECS - Duration::from_millis(1));
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
    assert_eq!(
        clip_to_width("日本語の長い文", 6),
        "日本…",
        "cut on a character boundary"
    );
    assert!(
        toast_rect(Rect::new(0, 0, 40, 2), "x").is_none(),
        "no spare row"
    );
}

/// 描くと、その行にだけバナーの地色が乗り、フッターのヒントはそのまま。
/// 索引画面でも同じ位置に出る。
#[test]
fn the_toast_is_painted_on_its_own_row_over_page_and_index_alike() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let bg = cosense::theme::toast_bg(ctx.terminal_bg());
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
    assert!(
        row_text(&term, 11).contains("VIEW"),
        "the footer keeps its badge and hint"
    );
    let buf = term.backend().buffer();
    let at = banner.find('✓').unwrap() as u16;
    assert_eq!(
        buf.cell((at, 10)).unwrap().style().bg,
        Some(bg),
        "the banner's own background"
    );
    assert_ne!(
        buf.cell((0, 10)).unwrap().style().bg,
        Some(bg),
        "the rest of the row is not the banner"
    );
    assert!(
        app.toast.as_ref().unwrap().shown.is_some(),
        "the draw started the clock"
    );

    // 索引でも。
    app.index = Some(cosense::index::Index::new(
        vec![cosense::index::Entry {
            title: "改善案".into(),
            updated: now_secs(),
            ..Default::default()
        }],
        1,
        cosense::index::SortKey::Updated,
    ));
    app.toast("「x」は本文にありません");
    let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    assert!(
        row_text(&term, 10).contains("本文にありません"),
        "{:?}",
        row_text(&term, 10)
    );

    // 期限が切れたら描かれない。
    app.toast.as_mut().unwrap().shown = Some(Instant::now() - TOAST_SECS - Duration::from_secs(1));
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    assert!(!row_text(&term, 10).contains("本文にありません"));
}

/// 薄い段(note)はヒント欄を数秒だけ丸ごと置き換える。キーも status も
/// 脇に退き、期限が来たら戻る。status 自体は消えない。索引のフッターでも同じ。
#[test]
fn a_note_overlays_the_hint_slot_and_gives_it_back() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["title", "one"]);
    app.note("ページの先頭です");
    assert_eq!(
        app.hint_body(&[]),
        "ページの先頭です",
        "alone, it takes the hint slot"
    );
    app.status = "選択中 — j/k で広げる".into();
    assert_eq!(
        app.hint_body(&[]),
        "ページの先頭です",
        "the status steps aside, not beside"
    );
    assert!(app.toast_text().is_empty(), "never a banner");

    assert!(!app.expire_note(), "still fresh");
    app.note = Some(("late".into(), Instant::now() - Duration::from_millis(1)));
    assert!(app.expire_note());
    assert_eq!(
        app.hint_body(&[]),
        "選択中 — j/k で広げる",
        "the slot is back to the state"
    );

    // 索引画面: 一覧にはステータス行が無いので、ヒント欄を借りる。
    app.index = Some(cosense::index::Index::new(
        vec![],
        0,
        cosense::index::SortKey::Updated,
    ));
    app.note("「x」は本文にありません");
    let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let buf = term.backend().buffer();
    let footer: String = (0..buf.area.width)
        .map(|x| buf.cell((x, 7)).unwrap().symbol().to_string())
        .collect::<String>()
        .replace(' ', "");
    assert!(footer.contains("本文にありません"), "{footer:?}");
}

/// バナーは帯の最終行に乗る。カーソル行は読者が見ている場所なので、
/// トーストが出ている間はその下に入らないよう、視界を1行ずらす。
#[test]
fn the_cursor_line_is_never_under_the_toast() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let mut app = page(&refs);
    let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    // ホイールでカーソル行を帯の最終行(トーストの行)まで送る。
    app.cursor = 20;
    app.follow = false;
    let toast_row = 10u16;
    let text_y = app.text_rect.y;
    // カーソル行 20 が画面の toast_row に来る scroll。
    app.scroll = (20 + text_y - toast_row) as u16;
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let row_of = |term: &Terminal<TestBackend>, needle: &str| -> Option<u16> {
        let buf = term.backend().buffer();
        (0..buf.area.height).find(|&y| {
            let s: String = (0..buf.area.width)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect();
            s.contains(needle)
        })
    };
    assert_eq!(
        row_of(&term, "line 20"),
        Some(toast_row),
        "without a toast the cursor may sit there"
    );

    app.toast("✓ copied");
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    assert_eq!(
        row_of(&term, "line 20"),
        Some(toast_row - 1),
        "one row up, clear of the banner"
    );
    assert!(row_of(&term, "✓ copied") == Some(toast_row));

    // 消えたあとは元の自由に戻る(押し戻しはしない)。
    app.dismiss_toast();
    term.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    assert_eq!(
        row_of(&term, "line 20"),
        Some(toast_row - 1),
        "nothing moves back on its own"
    );
}

/// 失敗は見逃されてはいけないので、赤は黄の2倍残る。
#[test]
fn an_error_toast_lives_longer_than_an_info_toast() {
    let mut app = page(&["title"]);
    app.toast_err("コミットに失敗しました");
    app.toast.as_mut().unwrap().shown = Some(Instant::now() - TOAST_SECS - Duration::from_secs(1));
    assert!(
        !app.expire_toast(),
        "past the info lifetime, an error is still up"
    );
    app.toast.as_mut().unwrap().shown =
        Some(Instant::now() - TOAST_ERR_SECS - Duration::from_secs(1));
    assert!(app.expire_toast());
}

/// `q` は一度目が問いで二度目が答え。他のキーや Esc、時間切れで取り下げる。
/// `^c` は問わずに出る。失うものがあれば問いに添える。
#[test]
fn q_asks_once_and_quits_on_the_second_press() {
    let ctx = test_ctx();
    let mut app = page(&["title", "one"]);
    assert!(matches!(
        handle_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Continue
    ));
    assert!(
        app.toast_text().contains("もう一度 q で終了"),
        "{}",
        app.toast_text()
    );
    assert!(matches!(
        handle_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Quit
    ));

    // 間に別のキーが入れば取り下げ。
    let mut app = page(&["title", "one"]);
    handle_key(&mut app, &ctx, key(KeyCode::Char('q')));
    handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
    assert!(
        matches!(
            handle_key(&mut app, &ctx, key(KeyCode::Char('q'))),
            Action::Continue
        ),
        "q j q must not leave"
    );
    // Esc も取り下げ(トーストも消える)。
    handle_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(app.quit_armed.is_none() && app.toast_text().is_empty());
    // 時間切れ。
    handle_key(&mut app, &ctx, key(KeyCode::Char('q')));
    app.quit_armed = Some(Instant::now() - QUIT_ARM_SECS - Duration::from_secs(1));
    assert!(
        matches!(
            handle_key(&mut app, &ctx, key(KeyCode::Char('q'))),
            Action::Continue
        ),
        "asks again after the window"
    );

    // 失うものは問いに添える。^c は問わない。
    let mut app = page(&["title", "one"]);
    app.inflight = 2;
    handle_key(&mut app, &ctx, key(KeyCode::Char('q')));
    assert!(
        app.toast_text().contains("未送信の編集 2 件"),
        "{}",
        app.toast_text()
    );
    assert!(matches!(
        handle_key(&mut app, &ctx, ctrl('c')),
        Action::Quit
    ));
}

/// q の問いには失われるものを添える: 未送信のコメントもその1つ。
#[test]
fn the_quit_question_counts_unsent_comments() {
    let mut app = page(&["title", "one"]);
    app.cursor = 1;
    let c = app.make_comment("fix".into()).unwrap();
    app.comments.push(c);
    assert!(!app.confirm_quit());
    assert!(
        app.toast_text().contains("未送信のコメント 1 件"),
        "{}",
        app.toast_text()
    );
}
