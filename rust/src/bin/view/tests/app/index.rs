use crate::*;
use crate::tests::support::*;

/// The index is a list of links, so the mouse treats it as one: the
/// wheel moves through it and a click opens the row it landed on.
#[test]
fn a_click_in_the_index_opens_that_page() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["t", "one"]);
    app.rebuild(100);
    let mk = |title: &str| cosense::index::Entry {
        title: title.into(),
        updated: now_secs(),
        descriptions: vec!["body".into()],
        unread: false,
        ..Default::default()
    };
    let mut entries = vec![mk("最初"), mk("二番目"), mk("三番目")];
    // …and enough behind them that the list has somewhere to scroll.
    entries.extend((0..30).map(|i| mk(&format!("その他{i}"))));
    let n = entries.len();
    app.index = Some(cosense::index::Index::new(entries, n, cosense::index::SortKey::Updated));
    // A frame has to have been drawn: the click is answered with the
    // geometry that was on screen.
    let mut t = Terminal::new(TestBackend::new(100, 10)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();

    let at = |kind: MouseEventKind, col: u16, row: u16| MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    // The wheel scrolls the list under the pointer and leaves the
    // selection where it is — content scrolls, cursors do not.
    for _ in 0..3 {
        handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollDown, 5, 3));
    }
    assert_eq!(app.index.as_ref().unwrap().cursor, 0, "the selection stayed");
    assert!(app.index.as_ref().unwrap().scroll > 0, "and the view moved");
    assert!(app.index_scrolled_at.is_some(), "so the scrollbar shows itself");
    for _ in 0..3 {
        handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollUp, 5, 3));
    }
    assert_eq!(app.index.as_ref().unwrap().scroll, 0);

    // Over the excerpt dock the wheel scrolls the excerpt instead.
    let preview_x = app.index_preview_rect.x + 2;
    let preview_y = app.index_preview_rect.y;
    handle_mouse(&mut app, &ctx, at(MouseEventKind::ScrollDown, preview_x, preview_y));
    assert_eq!(app.index.as_ref().unwrap().preview_scroll, 1);
    assert_eq!(app.index.as_ref().unwrap().cursor, 0, "and leaves the list alone");

    // A click on the third row opens the third page — one click. The
    // opening itself is a page fetch, which a test has no server for;
    // what is checked here is that the click resolved to THAT row and
    // went to open it (the status names what was asked for).
    handle_mouse(
        &mut app,
        &ctx,
        at(MouseEventKind::Down(MouseButton::Left), 5, 3),
    );
    // The opening itself is a page fetch, which a test has no server
    // for — so the list is still there (a failed open keeps the
    // reader's place). What is checked is which row it resolved to.
    assert!(
        app.toast_text().contains("三番目"),
        "the third row was opened, not another: {}",
        app.toast_text()
    );
}

/// The index is a place, so it is on the back stack: leaving it for a
/// page and pressing `[` comes back to the list, not to whatever page
/// happened to be open before it.
#[test]
fn the_index_is_somewhere_you_can_come_back_to() {
    let ctx = test_ctx();
    let mut app = page(&["t", "one"]);
    app.project = "proj".into();
    app.title = "A".into();
    app.index_project = "proj".into();
    app.index = Some(cosense::index::Index::new(
        vec![cosense::index::Entry {
            title: "B".into(),
            updated: now_secs(),
            descriptions: vec![],
            unread: false,
            ..Default::default()
        }],
        1,
        cosense::index::SortKey::Updated,
    ));
    assert!(matches!(
        app.here(),
        Place::Index { project, state }
            if project == "proj" && state.selected().map(|e| e.title.as_str()) == Some("B")
    ));

    // Opening a row that cannot be fetched (no server in a test) puts
    // the reader back in the list rather than dropping them onto the
    // page they had left.
    open_from_index(&mut app, &ctx, Some(("B".into(), false)));
    assert!(app.index.is_some(), "the list survives a failed open");
    assert!(app.history.is_empty(), "and nothing went on the stack");

    // `[/other-project]` opens someone else's index, and that is a
    // place too — reached from the PAGE the reader is on.
    app.history.clear();
    app.index = None;
    activate_link(&mut app, &ctx, LinkItem::ProjectIndex { project: "help-jp".into() });
    // (No network in tests: the index only opens when the list arrives.
    // Either way the reader must not lose their place.)
    if app.index.is_some() {
        assert_eq!(app.index_project, "help-jp");
        assert_eq!(
            app.history.last(),
            Some(&Place::Page { project: "proj".into(), title: "A".into() })
        );
    } else {
        assert!(app.history.is_empty(), "a failed open leaves the stack alone");
    }
}

/// Back is back even while printable keys normally belong to the
/// filter. The index itself goes onto the forward stack with its exact
/// cursor/filter/scroll state, so returning does not restart the list.
#[test]
fn brackets_and_escape_leave_and_restore_the_index() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.project = "proj".into();
    app.title = "A".into();
    app.index_project = "proj".into();
    let entries: Vec<_> = (0..12)
        .map(|i| cosense::index::Entry {
            title: format!("B{i}"),
            updated: now_secs() - i,
            descriptions: vec![format!("body {i}")],
            unread: i % 2 == 0,
            ..Default::default()
        })
        .collect();
    let mut index = cosense::index::Index::new(entries, 12, cosense::index::SortKey::Updated);
    index.cursor = 4;
    index.scroll = 2;
    index.preview_scroll = 1;
    index.focus = cosense::index::Pane::Preview;
    let saved = index.clone();
    app.index = Some(index);
    app.history.push(Place::Page { project: "proj".into(), title: "A".into() });

    handle_index_key(&mut app, &ctx, key(KeyCode::Char('[')));
    assert!(app.index.is_none(), "[ leaves the index rather than filtering");
    assert!(matches!(
        app.forward.last(),
        Some(Place::Index { project, state }) if project == "proj" && **state == saved
    ));

    go_history(&mut app, &ctx, false);
    assert_eq!(app.index.as_ref(), Some(&saved), "] restores the exact list state");

    handle_index_key(&mut app, &ctx, key(KeyCode::Esc));
    assert!(app.index.is_none(), "Esc uses the same back route");
}

/// The filter is a line you OPEN. That is what gives the letters back
/// to the index: `q` could not quit while every printable key was
/// filter text.
#[test]
fn the_index_filter_is_a_line_that_slash_opens_and_q_quits_beside_it() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.project = "proj".into();
    let entries: Vec<_> = ["quick", "quiet", "other"]
        .iter()
        .map(|t| cosense::index::Entry {
            title: (*t).into(),
            updated: now_secs(),
            descriptions: vec![],
            unread: false,
            ..Default::default()
        })
        .collect();
    let mut ix = cosense::index::Index::new(entries, 3, cosense::index::SortKey::Updated);
    ix.can_create = true; // a project this session may write in
    app.index = Some(ix);

    // Closed line: letters are commands, not text. `q` asks first.
    assert!(matches!(
        handle_index_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Continue
    ));
    assert!(app.toast_text().contains("もう一度 q"));
    assert!(matches!(
        handle_index_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Quit
    ));
    assert!(matches!(handle_index_key(&mut app, &ctx, ctrl('c')), Action::Quit));
    handle_index_key(&mut app, &ctx, key(KeyCode::Char('j')));
    assert_eq!(app.index.as_ref().unwrap().cursor, 1, "j moves even under a filter");
    assert!(app.index.as_ref().unwrap().filter.is_empty(), "nothing was typed");

    // `/` opens it, and now the same letters are text.
    handle_index_key(&mut app, &ctx, key(KeyCode::Char('/')));
    // …and the line holds the IME guard while it is open, so a Japanese
    // title can be typed without toggling the input source by hand —
    // the whole reason the filter became a line you open.
    assert!(app.ime_guard.is_some(), "the open line switches the input source");
    for c in "qui".chars() {
        handle_index_key(&mut app, &ctx, key(KeyCode::Char(c)));
    }
    let ix = app.index.as_ref().unwrap();
    assert!(ix.filter_editing);
    assert_eq!(ix.filter, "qui");
    assert_eq!(ix.len(), 3, "two matches plus the create offer");

    // ↑/↓ still pick while typing; `^c` still leaves.
    handle_index_key(&mut app, &ctx, key(KeyCode::Down));
    assert_eq!(app.index.as_ref().unwrap().cursor, 1);
    assert!(matches!(handle_index_key(&mut app, &ctx, ctrl('c')), Action::Quit));

    // Enter keeps the filter and hands the keys back to the list.
    handle_index_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(app.ime_guard.is_none(), "closing the line returns to ASCII");
    let ix = app.index.as_ref().unwrap();
    assert!(!ix.filter_editing);
    assert_eq!(ix.filter, "qui");
    app.quit_armed = None;
    assert!(matches!(
        handle_index_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Continue
    ));
    assert!(matches!(
        handle_index_key(&mut app, &ctx, key(KeyCode::Char('q'))),
        Action::Quit
    ));

    // Esc on an open line drops the filter rather than leaving the index.
    handle_index_key(&mut app, &ctx, key(KeyCode::Char('/')));
    assert!(app.ime_guard.is_some());
    handle_index_key(&mut app, &ctx, key(KeyCode::Esc));
    let ix = app.index.as_ref().expect("Esc closed the line, not the index");
    assert!(!ix.filter_editing);
    assert!(ix.filter.is_empty());
    assert!(app.ime_guard.is_none(), "Esc gives the input source back too");
}

/// The same screen answers in English when the environment asks for
/// it. Only the words change: keys, flags and notation are names, not
/// words, and stay put in both.
#[test]
fn the_whole_screen_can_be_read_in_english() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["title", "body"]);
    app.editable = true;
    cosense::lang::set_for_thread(cosense::lang::Lang::En);

    app.status = String::new();
    app.sync_state = capability::SyncState::Live; // no sync tag in front
    assert_eq!(
        app.hint_text(&[]),
        "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit"
    );
    handle_key(&mut app, &ctx, key(KeyCode::Char('y')));
    assert!(app.note_text().contains("copied"), "note: {}", app.note_text());

    app.overlay = Some(Overlay::Help);
    // Tall enough for every help line: the panel clips from the
    // bottom, and this test reads the whole list. (26 rows in four
    // sections: READ / EDIT / index / overlays.)
    let mut t = Terminal::new(TestBackend::new(100, 35)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let screen: String = {
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(screen.contains("move        j/k"), "help is in English: {screen}");
    assert!(screen.contains("Esc close"));
    assert!(!screen.contains("移動"), "and nothing Japanese is left behind");
    // Keys and env vars are names, not words: they read the same either way.
    assert!(screen.contains("── EDIT ──"), "the edit section: {screen}");
    assert!(screen.contains("── index ──"), "the index section: {screen}");
    assert!(screen.contains("── overlays ──"), "the overlay section: {screen}");
    assert!(screen.contains("quit        q twice"));
    assert!(screen.contains("^u/^d · PgUp/PgDn"));

    cosense::lang::set_for_thread(cosense::lang::Lang::Ja);
}

/// The help is four sections, one per place the keys work in — and
/// the stale `x deletes` row is gone (`x` now explains itself).
#[test]
fn the_help_lists_four_sections_and_no_stale_deletion_key() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["title", "body"]);
    app.editable = true;
    app.overlay = Some(Overlay::Help);
    let mut t = Terminal::new(TestBackend::new(100, 35)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let screen: String = {
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    for section in ["── READ ──", "── EDIT ──"] {
        assert!(screen.contains(section), "missing {section}: {screen}");
    }
    // Wide glyphs occupy two cells (grapheme + filler space), so CJK
    // assertions read the screen with the filler taken out.
    let packed: String = screen.chars().filter(|c| *c != ' ').collect();
    for section in ["──一覧──", "──オーバーレイ──"] {
        assert!(packed.contains(section), "missing {section}: {screen}");
    }
    assert!(!packed.contains("x行/選択を削除"), "stale deletion row: {screen}");
    assert!(!screen.contains("COSENSE_WEB_IDLE_SECS"), "the diagram notes moved to KEYMAP: {screen}");
}

/// The panel does not scroll, so every help row fits its 90 cells
/// (2 for the cursor marker) in both languages — a clipped row reads
/// as a different key.
#[test]
fn every_help_row_fits_the_panel_in_both_languages() {
    use cosense::lang::{set_for_thread, Lang};
    for lang in [Lang::Ja, Lang::En] {
        set_for_thread(lang);
        let app = page(&["title", "body"]);
        let rows = help_keys(&app);
        assert_eq!(rows.len(), 26, "four sections, nothing clipped vertically");
        for r in &rows {
            assert!(
                str_width(r) <= 88,
                "clipped: {r} ({} cells)",
                str_width(r)
            );
        }
    }
    set_for_thread(Lang::Ja);
}

/// `?` opens the help from the index (filter line closed) and from
/// any overlay — the sections it lists would otherwise be unreachable
/// where they are needed. While the filter line is open `?` stays a
/// filter character.
#[test]
fn question_opens_help_from_the_index_and_from_an_overlay() {
    let ctx = test_ctx();
    let mut app = page(&["title", "body"]);
    app.rebuild(100);
    let mk = |title: &str| cosense::index::Entry {
        title: title.into(),
        updated: now_secs(),
        descriptions: vec!["body".into()],
        unread: false,
        ..Default::default()
    };
    let entries = vec![mk("a"), mk("b")];
    let n = entries.len();
    app.index = Some(cosense::index::Index::new(entries, n, cosense::index::SortKey::Updated));
    assert!(!app.index.as_ref().unwrap().filter_editing, "filter line closed");
    handle_key(&mut app, &ctx, key(KeyCode::Char('?')));
    assert!(matches!(app.overlay, Some(Overlay::Help)), "? in the index should open help");

    app.overlay = Some(Overlay::LineInfo);
    handle_overlay_key(&mut app, &ctx, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(matches!(app.overlay, Some(Overlay::Help)), "? on an overlay should open help");
}

/// Eyeball the help and footer: prints the drawn screen so the mixed
/// language can be seen rather than argued about.
/// `cargo test -- --ignored --nocapture help_screen_dump`
#[test]
#[ignore]
fn help_screen_dump() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["ページ", "本文の行"]);
    app.editable = true;
    app.overlay = Some(Overlay::Help);
    let mut t = Terminal::new(TestBackend::new(100, 34)).unwrap();
    t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width)
            .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect();
        println!("|{}|", row.trim_end());
    }
}

/// Eyeball the index: prints the drawn screen so the look can be
/// judged, not just asserted. `cargo test -- --ignored --nocapture
/// index_screen_dump`
#[test]
#[ignore]
fn index_screen_dump() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["t", "one"]);
    app.rebuild(100);
    let mk = |title: &str, mins: i64, unread: bool, desc: &[&str]| cosense::index::Entry {
        title: title.into(),
        updated: now_secs() - mins * 60,
        descriptions: desc.iter().map(|s| s.to_string()).collect(),
        unread,
        ..Default::default()
    };
    app.index = Some(cosense::index::Index::new(
        vec![
            mk("改善案", 1, true, &["from [テスト]", "進め方", " 方針: 編集は EDIT セッションを本筋にする", " READ は読むためのモードに寄せる"]),
            mk("画像表示テスト", 60 * 26, false, &["画像の出方を並べたページ", "[https://gyazo.com/abc]"]),
            mk("ブラケット記法テスト", 60 * 24 * 9, false, &["各行は「`ソース` → 実際の描画」の形で並べてある", "`[]` → []"]),
            mk("websocket 同期の設計メモ", 60 * 24 * 40, true, &["socket.io の生フレームで話す", "code:frame.txt", " 0{\"sid\":…}"]),
            mk("テスト", 60 * 24 * 400, false, &["ここはテスト用のページ"]),
        ],
        137,
        cosense::index::SortKey::Updated,
    ));
    // …and a real project's worth of pages, where the scrollbar has
    // something to say.
    for i in 0..200 {
        let e = mk(&format!("ページ{i}"), 60 * (i as i64 + 2), i % 7 == 0, &["本文"]);
        app.index.as_mut().unwrap().entries.push(e);
    }
    for (w, h) in [(100u16, 14u16), (78, 12)] {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = t.backend().buffer().clone();
        println!("\n┌{}┐  ({w}x{h})", "─".repeat(w as usize));
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                .collect();
            println!("│{row}│");
        }
        println!("└{}┘", "─".repeat(w as usize));
    }
}

/// The index draws a full-width list above a shallow excerpt dock, and
/// gives the whole height to the list when `--preview auto` is below
/// its established 80-column threshold.
#[test]
fn the_index_draws_a_list_above_a_shallow_excerpt() {
    use ratatui::{backend::TestBackend, Terminal};
    let ctx = test_ctx();
    let mut app = page(&["t", "one"]);
    app.rebuild(80);
    app.index = Some(cosense::index::Index::new(
        vec![
            cosense::index::Entry {
                title: "改善案".into(),
                updated: now_secs() - 60,
                descriptions: vec!["from [テスト]".into(), "進め方".into()],
                unread: true,
                ..Default::default()
            },
            cosense::index::Entry {
                title: "画像表示テスト".into(),
                updated: now_secs() - 86_400,
                descriptions: vec!["画像の出方を並べたページ".into()],
                unread: false,
                ..Default::default()
            },
        ],
        2,
        cosense::index::SortKey::Updated,
    ));

    let draw = |app: &mut App, cols: u16| -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(cols, 10)).unwrap();
        t.draw(|f| ui(f, app, &ctx)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).map_or(" ".into(), |c| c.symbol().to_string()))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    };

    // A wide (CJK) glyph occupies two cells, the second of which is
    // blank; the text is read back without those.
    let flat = |s: &str| s.replace(' ', "");
    let has = |rows: &[String], needle: &str| rows.iter().any(|r| flat(r).contains(needle));

    // Wide: the list spends the full row; the excerpt starts below it.
    let rows = draw(&mut app, 100);
    assert!(rows[0].contains("proj"), "header names the project: {:?}", rows[0]);
    assert!(flat(&rows[1]).contains("改善案"), "first row: {:?}", rows[1]);
    let layout = cosense::index::layout(ctx.preview, 100, 8);
    let excerpt_y = 1 + layout.list as usize + layout.gap as usize;
    assert!(
        flat(&rows[excerpt_y]).contains("改善案"),
        "the page under the cursor heads the excerpt below: {:?}",
        rows[excerpt_y]
    );
    assert!(has(&rows[excerpt_y + 1..], "進め方"), "…including its first lines: {rows:?}");
    assert!(rows.last().unwrap().contains("Esc"), "footer: {:?}", rows.last());

    // Narrow: no excerpt, and the list has the height to itself. If a
    // resize removed the focused excerpt, focus returns to what remains.
    app.index.as_mut().unwrap().focus = cosense::index::Pane::Preview;
    let rows = draw(&mut app, 70);
    assert!(flat(&rows[1]).contains("改善案"));
    assert!(!has(&rows, "進め方"), "under 80 columns the excerpt is gone: {rows:?}");
    assert_eq!(app.index.as_ref().unwrap().focus, cosense::index::Pane::List);

    // Moving the cursor updates the excerpt.
    app.index.as_mut().unwrap().move_cursor(1);
    let rows = draw(&mut app, 100);
    assert!(has(&rows, "画像の出方"), "the preview follows the cursor: {rows:?}");
}

/// 並び順の切り替えは取り直しではなく、さっき取った一覧の再利用。6つの
/// 順を巡ると 500 件の要求が6本飛び、サイトの 429 に当たっていた。
/// `^o` / `^u` は今まで通り取り直す(その結果も覚える)。
#[test]
fn a_fetched_list_is_reused_by_an_order_switch_while_young() {
    use cosense::api::PageSummary;
    use cosense::index::SortKey;
    let mut app = page(&["title"]);
    let p = PageSummary {
        id: "id".into(),
        title: "t".into(),
        image: None,
        descriptions: vec![],
        updated: 1,
        created: 0,
        accessed: 0,
        views: 0,
        linked: 0,
    };
    assert!(app.cached_list("proj", SortKey::Updated).is_none(), "nothing fetched yet");
    app.remember_list("proj", SortKey::Updated, 1, &[p]);
    let (count, pages) = app.cached_list("proj", SortKey::Updated).expect("young entry");
    assert_eq!((count, pages.len()), (1, 1));
    // 順ごと・プロジェクトごとに別の一覧。
    assert!(app.cached_list("proj", SortKey::Title).is_none());
    assert!(app.cached_list("other", SortKey::Updated).is_none());
    // 古くなった一覧は使わない(取り直す)。
    let c = app.index_cache.get_mut(&("proj".to_string(), SortKey::Updated)).unwrap();
    c.at = Instant::now() - INDEX_CACHE_SECS - Duration::from_secs(1);
    assert!(app.cached_list("proj", SortKey::Updated).is_none(), "stale entry is a miss");
}

#[test]
fn the_related_gap_names_the_standing_sort() {
    // The one-line gap before the Links sections is where the index's
    // standing `s` choice says itself — it reaches every page.
    let mut app = page(&["t", "body"]);
    // One section is all the gap line needs; the entries are irrelevant.
    app.related = vec![crate::nav::RelSection {
        heading: "Links (0)".into(),
        entries: vec![],
    }];
    let rows = app.related_rows(60);
    let first = match &rows[0] {
        crate::app::Row::Aside { line } => line
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>(),
        _ => panic!("the gap is an aside"),
    };
    assert!(
        first.contains("links ·") && first.contains(app.index_sort.name()),
        "the gap names the sort: {first:?}"
    );
}

#[test]
fn s_shift_opens_the_order_menu_and_enter_applies_everywhere() {
    let ctx = test_ctx();
    let mut app = page(&["t"]);
    let want = cosense::index::SortKey::Title;
    let at = cosense::index::SortKey::ALL.iter().position(|&k| k == want).unwrap();
    app.index_sort_menu = Some(at);
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(app.index_sort_menu.is_none(), "the menu closed");
    assert_eq!(app.index_sort, want, "one standing order everywhere");
}
