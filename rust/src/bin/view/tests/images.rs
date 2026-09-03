use crate::*;
use super::support::*;

    #[test]
    fn a_send_to_a_dead_worker_stops_the_pulse_instead_of_hanging_it() {
        let mut app = mermaid_page();
        app.rebuild(80);
        // No worker was ever spawned and the receiver is dropped: every
        // send fails, exactly as it does once the worker has stopped.
        drop(app.web_jobs_rx.take());
        app.start_web_renders(capability::Trigger::Auto);
        assert!(app.web_pending.is_empty(), "nothing waits on a reply that cannot come");
        app.rebuild(80);
        assert!(app.web_shimmer.is_empty(), "so nothing pulses");

        // Same for a resize.
        let key = app
            .web_request(cosense::webrender::WebKind::Mermaid, "flowchart LR\n  A-->B", 3)
            .unwrap()
            .cache_key();
        let info = decode_web_png(&Picker::halfblocks(), &tiny_png(), IMAGE_MAX_COLS).unwrap();
        app.images.insert(key.clone(), info);
        app.laid_width = 0;
        app.rebuild(30);
        app.rescale_diagrams();
        assert!(app.web_rescaling.is_empty());
    }

    #[test]
    fn a_diagram_is_never_encoded_wider_than_the_pane() {
        // The cap itself: the pane wins while it is narrower than the
        // image ceiling, and never goes to zero.
        assert_eq!(diagram_max_cols(120), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(IMAGE_MAX_COLS), IMAGE_MAX_COLS);
        assert_eq!(diagram_max_cols(35), 35);
        assert_eq!(diagram_max_cols(14), 14);
        assert_eq!(diagram_max_cols(0), 1);

        // …and the encoder honours it for an image far wider than any pane.
        let picker = Picker::halfblocks();
        for cap in [IMAGE_MAX_COLS, 35, 14, 5, 1] {
            let info = decode_web_png(&picker, &wide_png(), cap).unwrap();
            assert!(
                info.cells_w <= cap,
                "cap {cap} produced {} cells wide",
                info.cells_w
            );
            assert_eq!(info.built_for, cap);
            assert!(info.cells_h >= 1);
        }
    }

    /// A picture is measured in whole rows and capped in height: the first
    /// so a baseline lands level with its bottom edge, the second so one
    /// tall screenshot cannot take the whole screen.
    #[test]
    fn a_picture_is_whole_rows_and_never_taller_than_the_cap() {
        let picker = Picker::halfblocks();
        let font = picker.font_size();
        let build = |w: u32, h: u32, cols: u16| {
            let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(w, h));
            build_image(&picker, img, cols).unwrap()
        };

        // A picture whose pixels do not fill its last row keeps the rows it
        // FILLS — the leftover would read as a gap under the picture.
        let one_and_a_half = font.height as u32 + font.height as u32 / 2;
        let info = build(font.width as u32 * 4, one_and_a_half, 64);
        assert_eq!(info.cells_h, 1, "a row and a half is one row");

        // Tall pictures are scaled down, keeping their shape.
        let tall = build(font.width as u32 * 10, font.height as u32 * 100, 64);
        assert_eq!(tall.cells_h, MAX_IMAGE_ROWS);
        assert!(tall.cells_w < 10, "narrowed to match: {}", tall.cells_w);

        // Ordinary pictures are untouched by the cap.
        let normal = build(font.width as u32 * 8, font.height as u32 * 4, 64);
        assert_eq!((normal.cells_w, normal.cells_h), (8, 4));
    }

    /// Which protocol draws the pictures decides whether they clip with
    /// the panes around them, so it is named in the status line and can be
    /// forced by anyone whose terminal answers badly.
    #[test]
    fn the_picture_protocol_is_named_and_can_be_forced() {
        use ratatui_image::picker::ProtocolType;
        let name = |p: ProtocolType| {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(p);
            image_protocol_name(&picker)
        };
        assert_eq!(name(ProtocolType::Kitty), "kitty");
        assert_eq!(name(ProtocolType::Iterm2), "iterm2");
        assert_eq!(name(ProtocolType::Sixel), "sixel");
        assert_eq!(name(ProtocolType::Halfblocks), "halfblocks");

        // `COSENSE_IMAGE` takes a protocol NAME; anything else means auto.
        let forced = |want: &str| -> Option<ProtocolType> {
            match want {
                "kitty" => Some(ProtocolType::Kitty),
                "iterm2" => Some(ProtocolType::Iterm2),
                "sixel" => Some(ProtocolType::Sixel),
                "halfblocks" => Some(ProtocolType::Halfblocks),
                _ => None,
            }
        };
        assert_eq!(forced("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(forced("halfblocks"), Some(ProtocolType::Halfblocks));
        assert_eq!(forced("auto"), None);
        assert_eq!(forced(""), None, "unset changes nothing");
    }

    #[test]
    fn scrolled_image_placeholder_reclaims_the_top_body_row() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = page(&["image"]);
        app.blocks = vec![Block::Image { url: "https://example.com/a.png".into(), indent: 0, item: false }];
        app.srcs = vec![0];
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(42, 8)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        {
            // Not yet here: the notation itself, one row, dimmed and pulsing.
            let buf = terminal.backend().buffer();
            let rows: Vec<String> = (0..8)
                .map(|y| (0..42).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect())
                .collect();
            assert!(rows.iter().any(|r| r.contains("[https://example.com/a.png]")), "rows: {rows:?}");
            assert_eq!(app.rows.iter().find(|r| matches!(r, Row::ImageLoading { .. })).map(|r| r.height()), Some(1));
        }

        // The real sliced image uses a different widget path from its text
        // placeholder and must reclaim the same row.
        let pixels = image::RgbaImage::from_pixel(16, 16, image::Rgba([255, 0, 0, 255]));
        let info = build_image(&ctx.picker, image::DynamicImage::ImageRgba8(pixels), IMAGE_MAX_COLS).unwrap();
        app.images.insert("https://example.com/a.png".into(), info);
        app.rebuild(42);
        app.scroll = 1;
        app.follow = false;
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        assert!(
            (3..20).any(|x| {
                let cell = buf.cell((x, 1)).unwrap();
                cell.fg == Color::Rgb(255, 0, 0) || cell.bg == Color::Rgb(255, 0, 0)
            }),
            "sliced image paints the reclaimed top row",
        );
    }
