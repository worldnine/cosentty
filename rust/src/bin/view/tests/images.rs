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

    /// The download gate lets `IMAGE_PARALLEL` through and holds the rest
    /// until a slot comes back.
    #[test]
    fn the_image_gate_counts_slots_and_gives_them_back() {
        let gate = Slots::new(2);
        let a = gate.acquire();
        let b = gate.acquire();
        assert_eq!(gate.in_use(), 2);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _c = gate.acquire();
                tx.send(()).unwrap();
            });
            assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(), "third waits");
            drop(a);
            assert!(rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok(), "…until a slot frees");
        });
        drop(b);
        assert_eq!(gate.in_use(), 0);
    }

    /// The band a waiting picture wears has to MOVE. A `[URL]` is twenty to
    /// sixty cells wide, and the vertical band's unit — six rows a second —
    /// gives a ten-second sweep along a row: longer than the download, so the
    /// reader watched a still grey line and called the animation dead. The
    /// sweep is therefore timed, not counted.
    #[test]
    fn a_waiting_picture_sweeps_a_band_along_its_url() {
        use ratatui::{backend::TestBackend, style::Modifier, Terminal};

        let url = "https://example.com/a.png";
        let mut app = page(&["image"]);
        app.blocks =
            vec![Block::Image { url: url.into(), indent: 0, item: false }];
        app.srcs = vec![0];
        let ctx = test_ctx();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();

        // Paint at `seconds` into the sweep and take the notation's own cells.
        let mut draw_at = |app: &mut App, seconds: f32| -> Vec<(String, ratatui::style::Style)> {
            app.image_anim = Instant::now() - Duration::from_secs_f32(seconds);
            terminal.draw(|f| ui(f, app, &ctx)).unwrap();
            let buf = terminal.backend().buffer();
            let row = |y: u16| -> String {
                (0..60)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            };
            let y = (0..8).find(|y| row(*y).contains('[')).expect("the notation on screen");
            let x0 = (0..60).find(|x| buf.cell((*x, y)).unwrap().symbol() == "[").unwrap();
            let x1 = (0..60).rev().find(|x| buf.cell((*x, y)).unwrap().symbol() == "]").unwrap();
            (x0..=x1)
                .map(|x| {
                    let c = buf.cell((x, y)).unwrap();
                    (c.symbol().to_string(), c.style())
                })
                .collect()
        };

        let early = draw_at(&mut app, 0.3);
        let late = draw_at(&mut app, 0.9);
        assert_eq!(early.len(), late.len(), "the same notation; only its brightness moved");

        // Every cell carries an RGB foreground. A palette colour could only
        // be DIM-ed, and `DarkGray` dimmed is the same grey it already was —
        // that is what made the band invisible rather than slow.
        for (sym, st) in &early {
            assert!(matches!(st.fg, Some(Color::Rgb(..))), "{sym} kept a palette colour: {st:?}");
            assert!(!st.add_modifier.contains(Modifier::DIM), "the band is colour, not DIM");
        }

        let peak = |frame: &[(String, ratatui::style::Style)]| -> usize {
            frame
                .iter()
                .enumerate()
                .max_by_key(|(_, (_, s))| match s.fg {
                    Some(Color::Rgb(r, g, b)) => u32::from(r) + u32::from(g) + u32::from(b),
                    _ => 0,
                })
                .map(|(i, _)| i)
                .unwrap()
        };
        let (a, b) = (peak(&early), peak(&late));
        assert!(b >= a + 6, "the band covered ground between two frames, not a cell or two: {a} → {b}");
        assert!(a > 0, "and it entered from the head of the row, not the middle");
        assert!(b + 3 < early.len(), "…and had not fallen off the end yet: {b}/{}", early.len());
    }

    /// The other half of the same complaint: a picture written INSIDE a line
    /// of text reserved its box and painted nothing in it, so the reader
    /// stared at a sixteen-row hole with no hint of what would fill it.
    #[test]
    fn a_waiting_picture_names_itself_in_the_box_it_reserved() {
        use ratatui::{backend::TestBackend, Terminal};

        let url = "https://example.com/a.png";
        let mut app = page(&["t", &format!("本文 [{url}] が続く")]);
        let ctx = test_ctx();
        app.rebuild(80);
        assert!(
            app.content_view(80).iter().any(|r| matches!(r, Row::Inline { .. })),
            "the line really is a mixed one"
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.image_anim = Instant::now() - Duration::from_secs_f32(0.6);
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        terminal.draw(|f| ui(f, &mut app, &ctx)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (0..80)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .filter(|s| s != " ")
                .collect::<String>()
        };
        let hits: Vec<u16> = (0..24).filter(|y| row(*y).contains("[https://")).collect();
        assert_eq!(hits.len(), 1, "the waiting picture speaks once, on its own row: {hits:?}");
        let y = hits[0];
        assert!(row(y).contains("本文"), "on the line's own baseline, where the words ride: {}", row(y));
        let x0 = (0..80).find(|x| buf.cell((*x, y)).unwrap().symbol() == "[").unwrap();
        let fg = buf.cell((x0 + 1, y)).unwrap().style().fg;
        assert!(matches!(fg, Some(Color::Rgb(..))), "and it wears the band: {fg:?}");
    }

