use crate::*;
use super::support::*;

    #[test]
    fn a_diagram_note_never_hides_or_erases_a_commit_or_auth_message() {
        let mut app = mermaid_page();
        // Live sync keeps the footer quiet, so this test sees only the
        // status-vs-note ordering it is about.
        app.sync_state = capability::SyncState::Live;
        // Something the reader must not miss is already on the status line.
        app.status = "コミットに失敗しました: line 4 — 500".into();
        app.note_web_failure("diagram: Chrome が見つかりません（ソースを表示します）".into());
        assert_eq!(
            app.hint_text(&[]),
            "コミットに失敗しました: line 4 — 500",
            "status outranks a diagram note",
        );
        // When the note times out it takes only itself with it.
        let (msg, _) = app.web_notice.clone().unwrap();
        app.web_notice = Some((msg, std::time::Instant::now()));
        assert!(app.expire_web_notice());
        assert_eq!(app.status, "コミットに失敗しました: line 4 — 500", "the real message survives");
        assert_eq!(app.hint_text(&[]), "コミットに失敗しました: line 4 — 500");

        // And with the status line free, the note would have shown.
        app.note_web_failure("diagram: ブラウザがタイムアウトしました（ソースを表示します）".into());
        app.status.clear();
        assert!(app.hint_text(&[]).contains("ブラウザがタイムアウト"));
        // The edit session and cursor links still outrank both.
        let links = vec![LinkItem::Page("Somewhere".into())];
        assert!(app.hint_text(&links).contains("Enter/f で開く"));
    }

    /// Backticks quote notation. A line that WRITES about `[a link]` must
    /// not BE one — a page documenting the syntax turned into a field of
    /// links to pages nobody meant to name, and Enter followed them.
    #[test]
    fn quoted_notation_is_not_a_link() {
        let line = "リンク付き画像 `[リンク先 画像URL]` → 画像を出し、Enter でリンク先へ";
        let mut app = page(&["t", line]);
        app.rebuild(80);
        app.goto_src(1);
        assert!(app.cursor_line_links().is_empty(), "{:?}", app.cursor_line_links());

        // A quoted URL is not a link either.
        let mut app = page(&["t", "`[https://example.com/a.png]` は画像になる"]);
        app.rebuild(80);
        app.goto_src(1);
        assert!(app.cursor_line_links().is_empty());

        // Outside the quotes, everything still works — including a real
        // link on the same line as a quoted one.
        let mut app = page(&["t", "`[quoted]` と [本物]"]);
        app.rebuild(80);
        app.goto_src(1);
        assert_eq!(app.cursor_line_links(), vec![LinkItem::Page("本物".into())]);

        // The masking keeps byte offsets, so mouse targets stay put.
        let masked = mask_inline_code("`[a]` [b]");
        assert_eq!(masked.len(), "`[a]` [b]".len());
        assert_eq!(&masked[6..], "[b]");
    }

    /// A linked image (`[href imageUrl]`) is ONE link: the picture is the
    /// label, the href is where Enter goes. Treating the two URLs as two
    /// links made the viewer ask which one you meant — and label each
    /// choice with the other one's address, so either pick looked wrong.
    #[test]
    fn a_linked_image_is_one_link_to_its_href() {
        let img = "https://example.com/photo.png";
        let href = "https://scrapbox.io/proj/Page";
        let mut app = page(&["t", &format!("[{href} {img}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1, "one link, not a question: {links:?}");
        match &links[0] {
            LinkItem::Url { label, url } => {
                assert_eq!(url, href, "Enter follows the href");
                assert!(label.contains("photo.png"), "labelled by the picture: {label}");
            }
            other => panic!("expected a url link, got {other:?}"),
        }

        // The order the two are written in does not change the answer.
        let mut app = page(&["t", &format!("[{img} {href}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1, "{links:?}");
        assert!(matches!(&links[0], LinkItem::Url { url, .. } if url == href));

        // A picture with no href is still just itself.
        let mut app = page(&["t", &format!("[{img}]")]);
        app.rebuild(80);
        app.goto_src(1);
        let links = app.cursor_line_links();
        assert_eq!(links.len(), 1);
        assert!(matches!(&links[0], LinkItem::Url { url, .. } if url == img));
    }

    /// A code block and a table are files: `Enter` saves them, written
    /// from the page in hand (Cosense's own endpoints answer 401 to the
    /// token this viewer holds — they want a session cookie).
    #[test]
    fn a_code_or_table_header_saves_the_block() {
        let mut app = page(&["t", "code:sample.py", " print(1)", "table:売上", " a\tb"]);
        app.project = "proj".into();
        app.title = "ページ".into();
        app.rebuild(80);

        app.goto_src(1);
        match app.cursor_line_links().first() {
            Some(LinkItem::Export { label, csv, .. }) => {
                assert_eq!(label, "sample.py");
                assert!(!csv);
            }
            other => panic!("expected the code file, got {other:?}"),
        }
        assert_eq!(
            block_export_body(&app.lines, 1, false),
            "print(1)\n",
            "the code as written, without the block's own indent",
        );

        app.goto_src(3);
        match app.cursor_line_links().first() {
            Some(LinkItem::Export { label, csv, .. }) => {
                assert_eq!(label, "売上.csv", "a table comes back as CSV");
                assert!(csv);
            }
            other => panic!("expected the table file, got {other:?}"),
        }
        assert_eq!(block_export_body(&app.lines, 3, true), "a,b\n");

        // Cells that need quoting get it, and nothing else does.
        assert_eq!(csv_row("a\tb"), "a,b");
        assert_eq!(csv_row("a,b\tc"), "\"a,b\",c");
        assert_eq!(csv_row("say \"hi\"\tx"), "\"say \"\"hi\"\"\",x");

        // Body lines are not links, and a nameless block offers nothing.
        app.goto_src(2);
        assert!(app.cursor_line_links().is_empty());
        assert!(block_export_link("code:", 0).is_none());
    }

    /// Shift+↑↓ selects in READ too. `v` then j/k is the akapen way and
    /// still works; this is the one people try first, and it has to reach
    /// the same selection — the one `y` and `c` act on.
    #[test]
    fn shift_arrows_select_lines_in_read_too() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.rebuild(40);
        app.cursor = 1;

        handle_key(&mut app, &ctx, shift(KeyCode::Down));
        handle_key(&mut app, &ctx, shift(KeyCode::Down));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 3)));
        assert_eq!(app.cursor, 3, "the cursor carries the far end");
        assert!(app.status.contains("3 行を選択"), "status: {}", app.status);

        // It is the same selection the rest of READ acts on.
        assert_eq!(copy_payload(&app, false).unwrap().0, "one\ntwo\nthree");

        // Shrinking works the same way, and Esc clears it.
        handle_key(&mut app, &ctx, shift(KeyCode::Up));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)));
        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.selection.is_none());

        // A plain arrow moves without selecting.
        handle_key(&mut app, &ctx, key(KeyCode::Down));
        assert!(app.selection.is_none());
    }

    /// J/K are Shift+↓/↑ for j/k hands: the same selection, grown and
    /// shrunk line by line. (In the move mode J/K belong to the drag and
    /// step over a whole sibling — that state is checked first.)
    #[test]
    fn shift_j_and_k_select_lines_like_the_shifted_arrows() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two", "three"]);
        app.rebuild(40);
        app.cursor = 1;

        handle_key(&mut app, &ctx, modified(KeyCode::Char('J'), KeyModifiers::SHIFT));
        handle_key(&mut app, &ctx, modified(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 3)));
        assert_eq!(app.cursor, 3, "the cursor carries the far end");

        handle_key(&mut app, &ctx, modified(KeyCode::Char('K'), KeyModifiers::SHIFT));
        assert_eq!(app.selection.map(|s| s.range()), Some((1, 2)));

        handle_key(&mut app, &ctx, key(KeyCode::Esc));
        assert!(app.selection.is_none());

        // Plain j still only moves.
        handle_key(&mut app, &ctx, key(KeyCode::Char('j')));
        assert!(app.selection.is_none());
    }

    /// OSC 52 is the only clipboard that reaches the machine the user is
    /// sitting at when the viewer runs over SSH, so its encoding has to be
    /// right.
    #[test]
    fn base64_for_osc52_matches_the_standard() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_encode("あ".as_bytes()), "44GC");
        // Round-trips through the decoder the browser side already uses.
        let round = cosense::chrome::b64_decode(&b64_encode("行 の コピー".as_bytes())).unwrap();
        assert_eq!(String::from_utf8(round).unwrap(), "行 の コピー");
    }

    #[test]
    fn file_links_are_found_with_their_labels() {
        let pdf = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
        let line = format!("[260826ニセコ.pdf {pdf}] and [Other Page]");
        assert_eq!(files_on_line(&line), vec![("260826ニセコ.pdf".to_string(), pdf.to_string())]);
        // bare URL: labelled by its file name
        assert_eq!(
            files_on_line(&format!("see {pdf} now")),
            vec![("6a8e7e5d714feb3f195319dd.pdf".to_string(), pdf.to_string())]
        );
        // images and plain URLs are not files
        assert!(files_on_line("[https://scrapbox.io/files/abc.png]").is_empty());
        assert!(files_on_line("[https://example.com/a.pdf]").is_empty());
        // …but every URL is offered to the browser, titled or bare
        assert_eq!(
            labelled_urls("[Docs https://example.com/a.pdf] and https://b.example/x?y=1 done"),
            vec![
                ("Docs".to_string(), "https://example.com/a.pdf".to_string()),
                ("https://b.example/x?y=1".to_string(), "https://b.example/x?y=1".to_string()),
            ]
        );
        // Scrapbox's url-first order `[https://… title]` labels too
        assert_eq!(
            labelled_urls("[https://example.com Example]"),
            vec![("Example".to_string(), "https://example.com".to_string())]
        );
        // a gyazo image line jumps to the image's PAGE, whatever URL form
        // the line used; Teams keeps its org host
        let id = "1d507226c261ce8cc513e8736ee5070c";
        for form in [
            format!("[https://gyazo.com/{id}]"),
            format!("[https://i.gyazo.com/{id}.png]"),
            format!("https://i.gyazo.com/{id}.jpg"),
        ] {
            let mut g = page(&["t", &form]);
            g.rebuild(80);
            g.goto_src(1);
            assert_eq!(
                g.cursor_line_links(),
                vec![LinkItem::Url { label: "gyazo".into(), url: format!("https://gyazo.com/{id}") }],
                "{form}"
            );
        }
        let mut g = page(&["t", &format!("[図 https://acme.gyazo.com/{id}]")]);
        g.rebuild(80);
        g.goto_src(1);
        assert_eq!(
            g.cursor_line_links(),
            vec![LinkItem::Url { label: "図".into(), url: format!("https://acme.gyazo.com/{id}") }]
        );
        let mut app2 = page(&["t", "see [Docs https://example.com/a.pdf]"]);
        app2.rebuild(80);
        app2.goto_src(1);
        assert_eq!(
            app2.cursor_line_links(),
            vec![LinkItem::Url { label: "Docs".into(), url: "https://example.com/a.pdf".into() }]
        );
        assert_eq!(app2.cursor_line_links()[0].label(), "↗ Docs");
        // the cursor line offers pages first, then files
        let mut app = page(&["t", &line]);
        app.rebuild(80);
        app.goto_src(1);
        let items = app.cursor_line_links();
        assert_eq!(items[0], LinkItem::Page("Other Page".into()));
        // Cross-project links: `[/project/title]` is that page, and a bare
        // `[/project]` is the project itself — on Cosense its home screen,
        // here its index. Tags stay in-project.
        assert_eq!(
            links_on_line("see [/shokai/階層整理型WiKiはスケールしない] and [/tus-alpine] #Scrapboxの哲学 [/ italic]"),
            vec![
                LinkItem::ProjectPage { project: "shokai".into(), title: "階層整理型WiKiはスケールしない".into() },
                LinkItem::ProjectIndex { project: "tus-alpine".into() },
                LinkItem::Page("Scrapboxの哲学".into()),
            ]
        );
        assert_eq!(
            LinkItem::ProjectPage { project: "shokai".into(), title: "t".into() }.label(),
            "/shokai/t"
        );
        assert!(matches!(&items[1], LinkItem::File { label, .. } if label == "260826ニセコ.pdf"));
        assert_eq!(items[1].label(), "↓ 260826ニセコ.pdf");
    }

    #[test]
    fn download_path_prefers_the_label_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("cosense-tui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = |p: &std::path::Path| p.file_name().unwrap().to_str().unwrap().to_string();
        let url = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ.pdf");
        // a label without an extension falls back to the URL's file name
        assert_eq!(name(&download_path_in(&dir, "資料", url)), "6a8e7e5d714feb3f195319dd.pdf");
        // slashes in a label cannot escape the directory
        assert_eq!(name(&download_path_in(&dir, "a/b.pdf", url)), "a_b.pdf");
        // an existing file is kept: the new one gets a numbered suffix
        std::fs::write(dir.join("260826ニセコ.pdf"), b"x").unwrap();
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ (2).pdf");
        std::fs::write(dir.join("260826ニセコ (2).pdf"), b"x").unwrap();
        assert_eq!(name(&download_path_in(&dir, "260826ニセコ.pdf", url)), "260826ニセコ (3).pdf");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Where downloads go is asked in a fixed order, and `~/Downloads` is
    /// only one answer in it — a box without that directory used to save
    /// into whatever directory the viewer started in (改善案3).
    #[test]
    fn the_download_directory_is_chosen_in_a_fixed_order() {
        let tmp = std::env::temp_dir().join(format!("cosense-dl-{}", std::process::id()));
        let home = tmp.join("home");
        let xdg = tmp.join("xdg");
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();
        let cwd = tmp.join("cwd");
        let h = home.to_str().unwrap();
        let pick = |explicit, env, xdg: Option<&str>| {
            pick_download_dir(explicit, env, xdg, Some(h), cwd.clone())
        };

        // The flag wins, and naming a place means "make it" — even under a
        // `~`, which arrives unexpanded when it was set by hand.
        let (dir, named) = pick(Some("~/elsewhere"), Some("/env"), xdg.to_str());
        assert_eq!((dir, named), (home.join("elsewhere"), true));
        // then the env var
        assert_eq!(pick(None, Some("/env"), xdg.to_str()).0, std::path::PathBuf::from("/env"));
        // then the desktop's own setting, but only where it really exists
        assert_eq!(pick(None, None, xdg.to_str()), (xdg.clone(), false));
        assert_eq!(pick(None, None, Some("/nope")).0, home.join("Downloads"), "missing XDG dir is skipped");
        // then ~/Downloads, and the working directory when even that is gone
        assert_eq!(pick(None, None, None), (home.join("Downloads"), false));
        std::fs::remove_dir_all(home.join("Downloads")).unwrap();
        assert_eq!(pick(None, None, None), (cwd.clone(), false));
        // blanks are not answers
        assert_eq!(pick(Some("  "), Some(""), None), (cwd, false));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn page_urls_from_the_command_line_are_parsed() {
        let enc = "CBT%E3%83%93%E3%82%B8%E3%83%8D%E3%82%B9%E9%83%A8%E6%A1%88%E4%BB%B6";
        assert_eq!(
            parse_page_url(&format!("https://scrapbox.io/acme/{enc}#6a8f895000000000000000e1")),
            Some(("acme".into(), Some("CBTビジネス部案件".into()), Some("6a8f895000000000000000e1".into())))
        );
        assert_eq!(
            parse_page_url("https://scrapbox.io/my-sandbox/"),
            Some(("my-sandbox".into(), None, None)),
            "a project URL opens its latest page"
        );
        assert_eq!(
            parse_page_url("scrapbox.io/p/T?x=1"),
            Some(("p".into(), Some("T".into()), None)),
            "scheme-less, query dropped"
        );
        assert_eq!(parse_page_url("https://cosen.se/p/a%2Fb"), Some(("p".into(), Some("a/b".into()), None)));
        // titles may contain a slash: everything after the project is title
        assert_eq!(parse_page_url("https://scrapbox.io/p/a/b"), Some(("p".into(), Some("a/b".into()), None)));
        assert_eq!(parse_page_url("acme"), None, "a bare project name is not a URL");
        assert_eq!(parse_page_url("https://example.com/x/y"), None);
        // decoding round-trips the encoder used for browser deep links
        assert_eq!(percent_decode(&urlencode_component("階層整理型WiKi はスケールしない")), "階層整理型WiKi はスケールしない");
        assert_eq!(percent_decode("100%"), "100%");
    }
