use crate::*;
use super::support::*;

    fn done(app: &App, line_id: &str, offset: usize, result: Result<&str, &str>) {
        app.upload_tx
            .send(UploadMsg {
                project: "proj".into(),
                title: "t".into(),
                line_id: line_id.into(),
                offset,
                name: "shot.png".into(),
                dest: cosense::upload::Destination::Gcs,
                result: result.map(str::to_string).map_err(str::to_string),
            })
            .unwrap();
    }

    #[test]
    fn splice_puts_the_picture_in_with_breathing_room() {
        assert_eq!(splice_image("", 0, "u"), ("[u]".into(), 3));
        assert_eq!(splice_image("ab", 1, "u"), ("a [u] b".into(), 5));
        assert_eq!(splice_image("ab ", 3, "u"), ("ab [u]".into(), 3), "no double space");
        assert_eq!(splice_image("a b", 2, "u"), ("a [u] b".into(), 4), "a space already there is kept");
        assert_eq!(splice_image("日本", 1, "u"), ("[u] 日本".into(), 4), "offset inside a char backs up");
        assert_eq!(splice_image("ab", 99, "u"), ("ab [u]".into(), 4), "past the end is the end");
    }

    /// The URL lands where the paste happened, in the live buffer when the
    /// caret is still on that line — so the dirty line commits as one
    /// edit — and the caret, if after the spot, moves with the text.
    #[test]
    fn an_upload_landing_on_the_caret_line_goes_into_the_buffer() {
        let ctx = test_ctx();
        let mut app = page(&["title", "ab"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 1);
        // Typing went on meanwhile: caret now at the end of "aXb".
        app.session.as_mut().unwrap().input.insert_str("X");
        let id = app.lines[1].id.clone();
        done(&app, &id, 1, Ok("https://scrapbox.io/files/abc"));
        assert!(app.drain_uploads(&ctx));
        let s = app.session.as_ref().unwrap();
        assert_eq!(s.input.buf, "a [https://scrapbox.io/files/abc] Xb");
        assert_eq!(s.input.cur, s.input.buf.len() - 1, "caret still before the b");
        assert!(drain_jobs(&mut app).is_empty(), "nothing committed yet: the line is just dirty");
        assert!(app.status.contains("貼りました"), "{}", app.status);
        assert!(app.hint_body(&[]).contains("貼りました"), "EDIT shows the outcome behind its keys");
    }

    /// Caret elsewhere: the line's CURRENT text gets the URL, as a commit.
    #[test]
    fn an_upload_for_another_line_replaces_that_lines_current_text() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one", "two"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 2, 0);
        let id = app.lines[1].id.clone();
        done(&app, &id, 3, Ok("https://gyazo.com/x"));
        app.drain_uploads(&ctx);
        assert_eq!(app.lines[1].text, "one [https://gyazo.com/x]");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "two", "the caret line is untouched");
        let jobs = drain_jobs(&mut app);
        assert!(matches!(jobs.last().and_then(|(_, ops)| ops.first()), Some(EditOp::Replace { .. })));
    }

    /// The line was deleted in the meantime: the picture goes last, and
    /// the status says why it is not where it was pasted.
    #[test]
    fn an_upload_whose_line_is_gone_goes_to_the_end_and_says_so() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        done(&app, "no-such-line", 0, Ok("https://gyazo.com/x"));
        app.drain_uploads(&ctx);
        assert_eq!(app.lines.len(), 3);
        assert_eq!(app.lines[2].text, "[https://gyazo.com/x]");
        assert!(app.status.contains("末尾"), "{}", app.status);
    }

    #[test]
    fn a_failed_upload_only_reports_and_a_foreign_pages_result_is_dropped() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        let id = app.lines[1].id.clone();
        done(&app, &id, 0, Err("HTTP 402 for upload-request"));
        app.drain_uploads(&ctx);
        assert!(app.status.contains("容量"), "402 is said in words: {}", app.status);
        assert_eq!(app.lines[1].text, "one");

        app.title = "elsewhere".into();
        done(&app, &id, 0, Ok("https://gyazo.com/x"));
        assert!(!app.drain_uploads(&ctx), "a result for the page we left changes nothing");
        assert_eq!(app.lines[1].text, "one");
    }

    /// The paste hook: an existing image file's path starts an upload
    /// (which cannot go anywhere in tests, but says it is trying); text
    /// that merely looks like a path pastes as text.
    #[test]
    fn pasting_an_image_path_starts_an_upload_instead_of_typing_it() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 3);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"not really a png").unwrap();
        session_paste(&mut app, &ctx, &format!("{} ", png.display()));
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one", "the path is not typed");
        assert!(app.status.contains("アップロード中") && app.status.contains("gcs"), "{}", app.status);

        session_paste(&mut app, &ctx, "/nowhere/shot.png");
        assert_eq!(app.session.as_ref().unwrap().input.buf, "one/nowhere/shot.png", "a path to nothing is text");
    }

    /// READ has no line to put it in; a read-only page cannot upload.
    #[test]
    fn uploads_are_refused_with_a_reason_when_they_cannot_happen() {
        let ctx = test_ctx();
        let mut app = page(&["title", "one"]);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"x").unwrap();
        app.start_upload(&ctx, &png);
        assert!(app.status.contains("編集中"), "{}", app.status);
        enter_session(&mut app, &ctx, 1, 0);
        app.editable = false;
        app.start_upload(&ctx, &png);
        assert!(app.status.contains("読み取り専用"), "{}", app.status);
    }

    /// A Gyazo destination takes the token that matches it and no other:
    /// a Teams token would put the picture in the org while the page
    /// points at gyazo.com.
    #[test]
    fn a_gyazo_destination_wants_the_matching_token() {
        let mut ctx = test_ctx();
        ctx.gyazo_teams_token = Some("teams".into());
        ctx.config = cosense::config::Config::parse("[upload]\nimages = \"gyazo\"\n").unwrap();
        let mut app = page(&["title", "one"]);
        app.rebuild(40);
        enter_session(&mut app, &ctx, 1, 0);
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        std::fs::write(&png, b"x").unwrap();
        app.start_upload(&ctx, &png);
        assert!(app.status.contains("GYAZO_ACCESS_TOKEN") && !app.status.contains("TEAMS"), "{}", app.status);

        ctx.config = cosense::config::Config::parse("[upload]\nimages = \"gyazo\"\ngyazo_team = \"org\"\n").unwrap();
        app.start_upload(&ctx, &png);
        assert!(app.status.contains("org.gyazo.com") && app.status.contains("アップロード中"), "{}", app.status);
    }
