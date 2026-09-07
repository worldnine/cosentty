use super::*;

fn plain(b: &Block) -> String {
    match b {
        Block::Blank => "[BLANK]".into(),
        Block::Image { url, .. } => format!("[IMAGE {url}]"),
        Block::Inline { parts, .. } => parts
            .iter()
            .map(|p| match p {
                InlinePart::Image(u) => format!("[IMAGE {u}]"),
                InlinePart::Formula { rows, .. } => rows.join("\n"),
                InlinePart::Text(l) => l
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
            })
            .collect::<Vec<_>>()
            .join(""),
        Block::Text(l) => l.spans.iter().map(|s| s.content.as_ref()).collect(),
        Block::Table(_) => "[TABLE]".into(),
        Block::Artifact { rows, .. } => rows
            .iter()
            .map(|(_, l)| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[test]
fn gyazo_canonical_permalink() {
    let id = "319b11dda7ce73cb3c1505774cfa0931";
    // Teams: keep org subdomain
    let mut v = Vec::new();
    find_gyazo(&format!("[https://acme-inc.gyazo.com/{id}]"), &mut v);
    assert_eq!(v, vec![format!("https://acme-inc.gyazo.com/{id}")]);
    // personal + i.gyazo direct both normalize to gyazo.com/<id>
    let mut v2 = Vec::new();
    find_gyazo(&format!("https://gyazo.com/{id}"), &mut v2);
    find_gyazo(&format!("https://i.gyazo.com/{id}.png"), &mut v2);
    assert_eq!(v2, vec![format!("https://gyazo.com/{id}")]); // dedup, both canonical
}

#[test]
fn non_gyazo_image_urls_are_detected() {
    // A picture needs the BRACKETED form — the rule cosense web
    // follows. A bare URL is a link, wherever it sits on the line.
    assert_eq!(standalone_image("https://example.com/a.png"), None);
    assert_eq!(
        standalone_image("[https://example.com/photo.JPG]"),
        Some("https://example.com/photo.JPG".into())
    );
    // linked image: [href imageUrl] -> the image side wins
    assert_eq!(
        standalone_image("[https://example.com/page https://cdn.example.com/x.webp]"),
        Some("https://cdn.example.com/x.webp".into())
    );
    // query strings don't defeat extension detection
    assert_eq!(
        standalone_image("[https://example.com/a.png?w=800]"),
        Some("https://example.com/a.png?w=800".into())
    );
    // non-image links are not images
    assert_eq!(standalone_image("[https://example.com/page.html]"), None);
    assert_eq!(standalone_image("ただの本文"), None);
    // …and a line that carries more than the picture is not "standalone",
    // though the picture is still found by `line_images`.
    assert_eq!(
        standalone_image("[https://example.com/a.png] こんな感じ"),
        None
    );
    assert_eq!(
        line_images("[https://example.com/a.png] こんな感じ"),
        vec!["https://example.com/a.png".to_string()],
    );
}

#[test]
fn standalone_gyazo_becomes_image_block() {
    let id = "319b11dda7ce73cb3c1505774cfa0931";
    let lines = vec![
        "タイトル".to_string(),
        format!("[https://acme-inc.gyazo.com/{id}]"),
    ];
    let out = render_lines(&lines);
    match &out.blocks[1] {
        Block::Image { url, .. } => {
            assert_eq!(url, &format!("https://acme-inc.gyazo.com/{id}"))
        }
        other => panic!("expected image block, got {other:?}"),
    }
}

#[test]
fn headings_take_the_palettes_level_style_by_star_count() {
    let pal = Palette::for_light(false);
    let lines: Vec<String> = [
        "title",
        "[* one]",
        "[** two]",
        "[*** three]",
        "[**** four]",
        "[***** five]",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    assert_eq!(
        &got[1..],
        ["one", "two", "three", "four", "five"],
        "no lead-in marks"
    );
    let style_of = |b: &Block| match b {
        Block::Text(l) => l.spans[0].style,
        _ => unreachable!(),
    };
    assert_eq!(
        style_of(&out.blocks[0]),
        pal.heading_style_for(4),
        "title = top level"
    );
    // The star forms carry the level AND bold (see `star_style`).
    assert_eq!(style_of(&out.blocks[1]), star_style(1, &pal));
    assert_eq!(style_of(&out.blocks[2]), star_style(2, &pal));
    assert_eq!(style_of(&out.blocks[3]), star_style(3, &pal));
    assert_eq!(style_of(&out.blocks[4]), star_style(4, &pal));
    assert_eq!(
        style_of(&out.blocks[5]),
        star_style(4, &pal),
        "four+ stars share the top level"
    );
    for b in &out.blocks[1..] {
        assert!(
            style_of(b).add_modifier.contains(Modifier::BOLD),
            "emphasis is the floor"
        );
    }
    assert_ne!(style_of(&out.blocks[1]), style_of(&out.blocks[4]));
}

#[test]
fn indented_headings_keep_both_bullet_and_theme_heading_styles() {
    let pal = Palette::for_light(false);
    let out = render_lines_with(
        &["title".into(), " [* one]".into(), "   [*** three]".into()],
        None,
        &pal,
        &LinkTruth::default(),
    );
    assert_eq!(plain(&out.blocks[1]), "• one");
    assert_eq!(plain(&out.blocks[2]), "    • three");

    for (block, stars) in [(&out.blocks[1], 1), (&out.blocks[2], 3)] {
        let Block::Text(line) = block else {
            panic!("expected text")
        };
        assert_eq!(line.spans[1].content, "• ");
        assert_eq!(line.spans[1].style.fg, Some(pal.bullet));
        assert_eq!(line.spans[2].style, star_style(stars, &pal));
    }
}

#[test]
fn uploaded_files_are_links_not_images() {
    let pdf = "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.pdf";
    assert!(!looks_like_image_url(pdf));
    assert!(is_scrapbox_file_url(pdf));
    assert_eq!(file_name_of_url(pdf), "6a8e7e5d714feb3f195319dd.pdf");
    // extension-less and image-extension uploads stay images
    assert!(looks_like_image_url(
        "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd"
    ));
    assert!(looks_like_image_url(
        "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.png"
    ));
    assert!(!is_scrapbox_file_url(
        "https://scrapbox.io/files/6a8e7e5d714feb3f195319dd.png"
    ));
    // a titled file link renders as a paper-clip label, never an image block
    let out = render_lines(&["t".into(), format!("[260826ニセコ.pdf {pdf}]")]);
    assert_eq!(plain(&out.blocks[1]), "260826ニセコ.pdf");
    assert!(out.extracted.images.is_empty());
    // an untitled one shows the file name
    let out = render_lines(&["t".into(), format!("[{pdf}]")]);
    assert_eq!(plain(&out.blocks[1]), "6a8e7e5d714feb3f195319dd.pdf");
}

#[test]
fn mermaid_is_recognised_by_language_and_by_filename() {
    // The three forms scrapbox.io/help-jp/Mermaid documents.
    for yes in [
        "mmd",
        "mermaid",
        "MMD",
        " Mermaid ",
        "flow.mmd",
        "図.mermaid",
        "a.b.mmd",
    ] {
        assert!(mermaid_lang(yes), "{yes} should be a mermaid block");
    }
    for no in [
        "",
        "js",
        "python",
        "mmdx",
        "mermaidjs",
        "readme.md",
        "mmd.txt",
        "diagram",
    ] {
        assert!(!mermaid_lang(no), "{no} should stay a plain code block");
    }
}

#[test]
fn a_mermaid_block_becomes_one_web_render_keyed_on_its_last_line() {
    let lines: Vec<String> = [
        "title",
        "code:mmd",
        " flowchart LR",
        "   A-->B",
        "",
        "after",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let web: Vec<_> = out
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Artifact {
                code,
                rows,
                last_src,
                ..
            } => Some((code, rows, *last_src)),
            _ => None,
        })
        .collect();
    assert_eq!(web.len(), 1);
    let (code, rows, last_src) = web[0];
    // Cosense hangs the preview off the block's LAST content line (3),
    // not the `code:` header (1) — verified live on help-jp/Mermaid.
    assert_eq!(last_src, 3);
    assert_eq!(code, "flowchart LR\n  A-->B");
    // The fallback presentation is the whole code block, header first.
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, 1);
    assert!(plain(&Block::Text(rows[0].1.clone())).contains("code:mmd"));
    assert_eq!(
        rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // The trailing blank still belongs to the page, not the block.
    assert!(matches!(out.blocks.last(), Some(Block::Text(_))));
}

#[test]
fn several_mermaid_blocks_stay_separate_and_other_languages_are_untouched() {
    let lines: Vec<String> = [
        "title",
        "code:one.mmd",
        " graph TD",
        "code:js",
        " let a = 1",
        "code:mermaid",
        " pie",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let last_srcs: Vec<usize> = out
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Artifact { last_src, .. } => Some(*last_src),
            _ => None,
        })
        .collect();
    assert_eq!(last_srcs, vec![2, 6], "one web block per mermaid block");
    // The JS block is still ordinary text rows.
    assert!(out
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Text(_)) && plain(b).contains("let a = 1")));
}

#[test]
fn mermaid_nests_twice_without_bullets_then_becomes_plain_list_rows() {
    // 実ページの表記そのまま。`code::test.mmd` もファイル名が
    // `:test.mmd` なだけで、拡張子によりMermaidと判定される。
    let lines: Vec<String> = [
        "title",
        "code::test.mmd",
        "   flowchart LR",
        "     TUI-- CDP -->Chrome",
        "     Chrome-- PNG -->TUI",
        "",
        "",
        " code:test.mmd",
        "   flowchart LR",
        "     TUI-- CDP -->Chrome",
        "     Chrome-- PNG -->TUI",
        "",
        "",
        "　　code::test.mmd",
        "   flowchart LR",
        "     TUI-- CDP -->Chrome",
        "     Chrome-- PNG -->TUI",
        "",
        "",
        "",
        "  code::test.mmd",
        "   flowchart LR",
        "     TUI-- CDP -->Chrome",
        "     Chrome-- PNG -->TUI",
        "",
        "   code::test.mmd",
        "   flowchart LR",
        "     TUI-- CDP -->Chrome",
        "     Chrome-- PNG -->TUI",
        "     Chrome-- PNG -->TUI",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let indents: Vec<usize> = out
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Artifact { indent, .. } => Some(*indent),
            _ => None,
        })
        .collect();
    assert_eq!(
        indents,
        vec![0, 2, 4, 4],
        "blank-separated levels 0, 1 and 2 draw independently"
    );

    let plain_rows: Vec<String> = out.blocks.iter().map(plain).collect();
    assert!(plain_rows.iter().any(|s| s.contains("• code::test.mmd")));
    assert!(plain_rows.iter().any(|s| s.contains("• flowchart LR")));
    assert!(plain_rows
        .iter()
        .any(|s| s.contains("• TUI-- CDP -->Chrome")));
    assert!(plain_rows
        .iter()
        .any(|s| s.contains("• Chrome-- PNG -->TUI")));

    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert!((25..=29).all(|i| code_span_at(&refs, i).is_none()));
    let flags = code_line_flags(&refs);
    assert!((25..=29).all(|i| !flags[i]));
}

#[test]
fn an_inline_formula_is_set_in_the_line() {
    let lines: Vec<String> = [
        "title",
        r"[$ E = mc^2 ]",
        r"1行に複数も書ける: [$ 3 \times 2 ]は[$ 2 \times 3 ]と同じ",
        r"数式内に]を含む場合: [$ [x-3]+a^2 ]",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let got: Vec<String> = render_lines(&lines).blocks.iter().map(plain).collect();
    assert_eq!(
        got,
        vec![
            "title",
            "E = mc²",
            "1行に複数も書ける: 3 × 2は2 × 3と同じ",
            "数式内に]を含む場合: [x - 3] + a²",
        ]
    );
}

#[test]
fn a_drawn_formula_takes_the_body_ink_and_bare_latex_the_code_ink() {
    let pal = Palette::for_light(false);
    let out = render_lines_with(
        &[
            "title".to_string(),
            r"[$ E = mc^2 ]と[$ \begin{align} x &= 1 \end{align} ]".to_string(),
        ],
        None,
        &pal,
        &LinkTruth::default(),
    );
    let Block::Text(line) = &out.blocks[1] else {
        panic!("{:?}", out.blocks[1])
    };
    let ink = |needle: &str| {
        line.spans
            .iter()
            .find(|s| s.content.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing: {line:?}"))
            .style
            .fg
    };
    assert_eq!(ink("mc²"), None, "drawn: the sentence's own ink");
    assert_eq!(
        ink(r"\begin{align}"),
        Some(pal.code_fence),
        "undrawn: notation, so the notation colour"
    );
}

#[test]
fn an_inline_formula_is_not_a_page_link() {
    // 括弧の中は Cosense の文ではなく LaTeX なので、リンクも
    // 装飾もアイコンも読み取らない。
    let lines = vec![
        "title".to_string(),
        r"[$ E = mc^2 ]".to_string(),
        "[普通のリンク]".to_string(),
    ];
    let out = render_lines(&lines);
    assert_eq!(
        out.extracted.links,
        vec!["普通のリンク"],
        "{:?}",
        out.extracted.links
    );
    assert!(
        out.hits
            .iter()
            .flatten()
            .all(|h| { !matches!(&h.target, HitTarget::Page(p) if p.contains('$')) }),
        "no followable target on a formula"
    );
}

#[test]
fn a_tall_inline_formula_becomes_a_part_of_its_own() {
    // 分数は本文の行の上下に伸びるので、画像と同じく行の
    // 部品として切り出す。周りの本文は前後のテキストの部品のまま。
    let lines = vec!["title".to_string(), r"解は[$ \frac{-b}{2a} ]だ".to_string()];
    let out = render_lines(&lines);
    let Block::Inline { parts, .. } = &out.blocks[1] else {
        panic!("expected an inline block, got {:?}", out.blocks[1])
    };
    let shape: Vec<String> = parts
        .iter()
        .map(|p| match p {
            InlinePart::Text(l) => l
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            InlinePart::Formula {
                source,
                rows,
                baseline,
            } => {
                let latex: String = source.spans.iter().map(|s| s.content.as_ref()).collect();
                format!("math({latex}) rows={} baseline={baseline}", rows.len())
            }
            InlinePart::Image(u) => format!("img({u})"),
        })
        .collect();
    assert_eq!(
        shape,
        vec!["解は", r"math(\frac{-b}{2a}) rows=3 baseline=1", "だ"],
        "one sentence, three parts"
    );
    // 組まれた行はすべて同じ幅に揃っている(列がずれない)。
    let InlinePart::Formula { rows, .. } = &parts[1] else {
        unreachable!()
    };
    let w = unicode_width::UnicodeWidthStr::width(rows[0].as_str());
    for r in rows {
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(r.as_str()),
            w,
            "{rows:?}"
        );
    }
}

#[test]
fn a_formula_only_survives_at_the_left_margin() {
    // 図は2段まで入れ子にできるが、数式は0段だけ。
    let lines: Vec<String> = [
        "title",
        "code:tex",
        " \\frac{a}{b}",
        "",
        " code:tex",
        "  \\frac{c}{d}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let indents: Vec<usize> = out
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Artifact { indent, .. } => Some(*indent),
            _ => None,
        })
        .collect();
    assert_eq!(indents, vec![0], "only the flush block is a formula");

    let plain_rows: Vec<String> = out.blocks.iter().map(plain).collect();
    assert!(
        plain_rows.iter().any(|s| s.contains("• code:tex")),
        "{plain_rows:?}"
    );
    assert!(
        plain_rows.iter().any(|s| s.contains(r"• \frac{c}{d}")),
        "{plain_rows:?}"
    );

    // コードブロックではないので、編集側も普通の行として扱う。
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert!((4..=5).all(|i| code_span_at(&refs, i).is_none()));
    let flags = code_line_flags(&refs);
    assert!((4..=5).all(|i| !flags[i]));
}

#[test]
fn an_empty_mermaid_block_stays_a_plain_code_header() {
    // No content line means no line id to hang a preview off.
    let lines: Vec<String> = ["title", "code:mmd"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = render_lines(&lines);
    assert!(!out
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Artifact { .. })));
    assert!(out.blocks.iter().any(|b| plain(b).contains("code:mmd")));
}

#[test]
fn a_nested_plain_code_header_wears_the_lists_bullet_but_its_body_does_not() {
    // cosense web: `• go` on the header row, bare code rows under it.
    // Level 0 has no bullet (there is no list), level 1 and 2 do.
    let lines: Vec<String> = [
        "t",
        "code:top",
        " top()",
        "区切り",
        " 箇条",
        " code:go",
        "  func()",
        "区切り2",
        "  箇条2",
        "  code:rs",
        "   main()",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    assert!(
        got.iter().any(|s| s == "code:top"),
        "level 0 is bare: {got:?}"
    );
    assert!(
        got.iter().any(|s| s == "• code:go"),
        "level 1 wears it: {got:?}"
    );
    assert!(
        got.iter().any(|s| s == "  • code:rs"),
        "level 2 wears it: {got:?}"
    );
    assert!(
        got.iter()
            .filter(|s| s.contains("func()") || s.contains("main()"))
            .all(|s| !s.contains('•')),
        "body rows stay bare: {got:?}"
    );
    // The wash starts at the body: the header row keeps the page's
    // own background (see `code_line_flags`).
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let flags = code_line_flags(&refs);
    // headers 1, 5, 9 unflagged; bodies flagged.
    assert_eq!(
        flags,
        vec![false, false, true, false, false, false, true, false, false, false, true],
        "{flags:?}"
    );
}

#[test]
fn a_blank_code_line_keeps_its_block_instead_of_becoming_a_bullet() {
    // 実ページで起きたこと:ブロックの末尾でEnterして積んだ空行(深さを
    // 継承した空白行)が、末尾の「後続空行を手渡す」popでブロックから
    // 押し出され、空の箇条書き(ドット)として図の下に残っていた。
    // インデントはメンバーシップなので、空行は空のコードとして残る。
    let lines: Vec<String> = [
        "[* mindmap]",
        "code:mindmap.mmd",
        " mindmap",
        "  root((mindmap))",
        "    Origins",
        "    Research",
        "    ",
        "    ",
        "    ",
        "",
        "[* timeline]",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    assert!(
        !got.iter().any(|s| s.contains('•')),
        "no dots below the drawing: {got:?}"
    );
    // 空行はブロックの内側。最終行は最後の空行になる。
    let Block::Artifact { last_src, .. } = &out.blocks[1] else {
        panic!("{:?}", out.blocks[1])
    };
    assert_eq!(*last_src, 8, "the blanks are the block's tail");
    // フラッシュの空行だけは相変わらず手渡される(ページの空行)。
    assert_eq!(got[2], "[BLANK]");
}

#[test]
fn blank_lines_after_a_code_block_are_all_kept() {
    let lines: Vec<String> = ["t", "code:x.py", " print(1)", "", "", "", "after"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for hl in [None, Some(crate::highlight::Highlighter::new(None, false))] {
        let out = render_lines_with(
            &lines,
            hl.as_ref(),
            &Palette::for_light(false),
            &LinkTruth::default(),
        );
        let got: Vec<String> = out.blocks.iter().map(plain).collect();
        assert_eq!(
            got,
            vec![
                "t",
                "code:x.py",
                "  print(1)",
                "[BLANK]",
                "[BLANK]",
                "[BLANK]",
                "after"
            ],
            "highlighted={}",
            hl.is_some()
        );
        // every source line owns exactly one block, in order
        assert_eq!(out.srcs, vec![0, 1, 2, 3, 4, 5, 6]);
    }
    // a TRULY blank line ends the block (web parity): the blank is a
    // page blank, and the indented line after it is a bullet again.
    let lines: Vec<String> = ["t", "code:x.py", " a = 1", "", " b = 2", "end"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    assert_eq!(
        got,
        vec!["t", "code:x.py", "  a = 1", "[BLANK]", "• b = 2", "end"]
    );
    assert_eq!(out.srcs, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn blank_preserved_and_flush_left_bullet() {
    let lines = vec![
        "お問い合わせメール対応".to_string(),
        " お問い合わせメールのフォーマット".to_string(),
        "".to_string(),
        "次のセクション".to_string(),
    ];
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    assert_eq!(got[0], "お問い合わせメール対応");
    assert_eq!(got[1], "• お問い合わせメールのフォーマット");
    assert_eq!(got[2], "[BLANK]");
    assert_eq!(got[3], "次のセクション");
}

#[test]
fn fullwidth_space_nesting() {
    let lines = vec![
        "ページタイトル".to_string(),
        "\t\t\t\t本文".to_string(),
        "\t\t\t\t\u{3000}見出し".to_string(),
        "\t\t\t\t\u{3000}    基本は見出しH2で作成".to_string(),
    ];
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    // One source whitespace char remains one logical level, but each
    // nesting step is two terminal columns. 4 tabs → col 6; +　 →
    // col 8; +　+4 spaces → col 16.
    assert_eq!(got[1], "      • 本文");
    assert_eq!(got[2], "        • 見出し");
    assert_eq!(got[3], "                • 基本は見出しH2で作成");
}

/// The editor asks `code_span_at` where a block is; the renderer
/// decides what a block is while it walks the page. If they disagree,
/// Enter drops a line where the renderer will not read it as code.
#[test]
fn code_span_agrees_with_what_the_renderer_collected() {
    let src = [
        "title",
        " before",
        "code:x.py",
        " a = 1",
        "",
        " b = 2",
        "",
        "after",
        " nested",
        "  code:y.txt",
        "   deep",
        "plain",
    ];
    let refs: Vec<&str> = src.to_vec();
    let inside: Vec<usize> = (0..src.len())
        .filter(|&i| code_span_at(&refs, i).is_some())
        .collect();
    assert_eq!(
        inside,
        vec![2, 3, 9, 10],
        "a truly blank line ends the block"
    );

    let top = code_span_at(&refs, 3).unwrap();
    assert_eq!(top.header, 2);
    assert_eq!(
        top.body_indent(),
        " ",
        "one step deeper than a flush header"
    );

    let nested = code_span_at(&refs, 10).unwrap();
    assert_eq!(nested.header, 9);
    assert_eq!(nested.body_indent(), "   ", "deeper header, deeper body");
}

/// What can be followed on a line is reported as SPAN INDICES, so the
/// viewer never has to work it out from how the line looks. The click
/// path used to compare colours, which is why a new colour (the
/// uncreated-link one) silently made red links unclickable.
#[test]
fn every_followable_thing_says_which_span_it_is() {
    let pal = Palette::for_light(false);
    let render = |src: &str| {
        let out = render_lines_with(
            &["t".to_string(), src.to_string()],
            None,
            &pal,
            &LinkTruth::default(),
        );
        let line = match &out.blocks[1] {
            Block::Text(l) => l.clone(),
            b => panic!("not a text block: {b:?}"),
        };
        let hits = out.hits[1].clone();
        // Every hit must name a span that exists, and the span it
        // names is the one the reader sees.
        let seen: Vec<(String, HitTarget)> = hits
            .iter()
            .map(|h| (line.spans[h.span].content.to_string(), h.target.clone()))
            .collect();
        seen
    };

    assert_eq!(
        render("see [Target] and [Docs https://example.com] #tag"),
        vec![
            ("Target".into(), HitTarget::Page("Target".into())),
            (
                "Docs".into(),
                HitTarget::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into()
                }
            ),
            ("#tag".into(), HitTarget::Page("tag".into())),
        ]
    );
    // Two links with the SAME label: the indices tell them apart with
    // no counting of occurrences and no comparing of colours.
    assert_eq!(
        render("[Docs] then [Docs https://example.com]"),
        vec![
            ("Docs".into(), HitTarget::Page("Docs".into())),
            (
                "Docs".into(),
                HitTarget::Url {
                    label: "Docs".into(),
                    url: "https://example.com".into()
                }
            ),
        ]
    );
    // A link inside a decoration keeps its place once the decoration's
    // spans are counted.
    assert_eq!(
        render("a [* [ページ]] b [[[太字リンク]]]"),
        vec![
            ("ページ".into(), HitTarget::Page("ページ".into())),
            ("太字リンク".into(), HitTarget::Page("太字リンク".into())),
        ]
    );
    // An indented line puts its indent and bullet in front; the hit
    // has to point past them.
    assert_eq!(
        render(" ここに [リンク]"),
        vec![("リンク".into(), HitTarget::Page("リンク".into()))]
    );
    // A quote puts two spans in front, a code header one.
    assert_eq!(
        render("> 引用の中の [リンク]"),
        vec![("リンク".into(), HitTarget::Page("リンク".into()))]
    );
    assert_eq!(
        render(" code:hello.py"),
        vec![("code:hello.py".into(), HitTarget::BlockLabel)]
    );
    // Notation that leads nowhere reports nothing.
    assert_eq!(render("[* ただの太字] and [] text"), vec![]);
}

/// A link to a page nobody has written yet is coloured differently,
/// as it is on scrapbox.io — and, crucially, ONLY when the page said
/// so. Anything the page never claimed to link to (a link typed while
/// editing, most of all) keeps the ordinary link colour: the viewer
/// would rather be silent than call an existing page missing.
#[test]
fn only_links_the_page_vouched_for_can_be_marked_missing() {
    let pal = Palette::for_light(false);
    // The page links to three titles; the server listed one of them
    // as an existing neighbour.
    let known = LinkTruth::seed(["あるページ", "ないページ", "tag"], ["あるページ"]);
    let styles = |src: &str, known: &LinkTruth| -> Vec<(String, Style)> {
        render_lines_with(&["t".to_string(), src.to_string()], None, &pal, known)
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(l) => Some(
                    l.spans
                        .iter()
                        .map(|s| (s.content.to_string(), s.style))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .nth(1)
            .unwrap_or_default()
    };
    let style_of = |src: &str, text: &str, m: &LinkTruth| {
        styles(src, m)
            .into_iter()
            .find(|(c, _)| c == text)
            .unwrap_or_else(|| panic!("{text:?} not rendered from {src:?}"))
            .1
    };
    assert_eq!(
        style_of("[あるページ]", "あるページ", &known).fg,
        Some(pal.link)
    );
    assert_eq!(
        style_of("[ないページ]", "ないページ", &known).fg,
        Some(pal.link_missing)
    );
    // Still underlined: Enter opens it, and typing a line creates it.
    assert!(style_of("[ないページ]", "ないページ", &known)
        .add_modifier
        .contains(Modifier::UNDERLINED));
    // A hashtag is a link too.
    assert_eq!(style_of("#tag", "#tag", &known).fg, Some(pal.link_missing));
    // A decorated link keeps the marking.
    assert_eq!(
        style_of("[* [ないページ]]", "ないページ", &known).fg,
        Some(pal.link_missing)
    );
    // No evidence either way → the ordinary colour.
    let typed = style_of("[新しく打った]", "新しく打った", &known);
    assert_eq!(typed.fg, Some(pal.link));
    // And with nothing known at all, nothing is marked.
    assert_eq!(
        style_of("[ないページ]", "ないページ", &LinkTruth::default()).fg,
        Some(pal.link)
    );
}

/// Titles are compared as Cosense compares them: case-folded, with
/// whitespace read as `_`.
#[test]
fn link_truth_matches_titles_the_way_cosense_does() {
    assert_eq!(title_lc("AI supported coding"), "ai_supported_coding");
    assert_eq!(title_lc("選択した文字 URL"), "選択した文字_url");
    let m = LinkTruth::seed(["Scrapbox Golf"], ["scrapbox_golf"]);
    assert!(
        !m.missing("Scrapbox Golf"),
        "the same page under another spelling"
    );
    let m = LinkTruth::seed(["Scrapbox Golf"], ["別のページ"]);
    assert!(
        m.missing("scrapbox_golf"),
        "and the link is found under either spelling"
    );
}

/// `[]` is not notation — an empty link has nothing to link to — so
/// Cosense shows the brackets as the text they are. The viewer used to
/// swallow them, which made a line ABOUT `[]` lose the thing it was
/// about (they only survived inside a code block).
#[test]
fn empty_brackets_are_text_and_double_brackets_are_bold() {
    let pal = Palette::for_light(false);
    let plain = |src: &str| -> String {
        let out = render_lines_with(
            &["t".to_string(), src.to_string()],
            None,
            &pal,
            &LinkTruth::default(),
        );
        out.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(l) => Some(
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .nth(1)
            .unwrap_or_default()
    };
    assert_eq!(plain("[]"), "[]");
    assert_eq!(plain("a[]b"), "a[]b");
    assert_eq!(plain("空の [] を書く"), "空の [] を書く");
    assert_eq!(plain("[ ]"), "[ ]", "whitespace is not a page name either");
    assert_eq!(plain("[foo"), "[foo", "an unclosed bracket is text");

    // A real link still loses its brackets, as on the web.
    assert_eq!(plain("[リンク]"), "リンク");

    // `[[text]]` is bold: matched as a PAIR, or the first `]` would cut
    // it at `[text` and it would read as a link.
    assert_eq!(plain("[[太字]]"), "太字");
    assert_eq!(plain("[[]]"), "[[]]", "empty double brackets are text too");
    // `[[x]]` is Cosense's other spelling of `[* x]`, so it wears the
    // same style — whatever the theme makes that level look like.
    let styles = |src: &str| -> Vec<Style> {
        render_lines_with(
            &["t".to_string(), src.to_string()],
            None,
            &pal,
            &LinkTruth::default(),
        )
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(l) => Some(l.spans.iter().map(|s| s.style).collect::<Vec<_>>()),
            _ => None,
        })
        .nth(1)
        .unwrap_or_default()
    };
    assert_eq!(styles("[[太字]]"), styles("[* 太字]"));
    assert_ne!(
        styles("[[太字]]"),
        styles("太字"),
        "and it is not plain text"
    );
}

/// The memo's three image complaints, as one test: a bare URL is not a
/// picture, the text after a picture survives, and a second picture on
/// the same line is not dropped.
#[test]
fn a_line_keeps_its_text_and_every_picture_on_it() {
    let pal = Palette::for_light(false);
    let shape = |src: &str| -> Vec<String> {
        let out = render_lines_with(
            &["t".to_string(), src.to_string()],
            None,
            &pal,
            &LinkTruth::default(),
        );
        out.blocks
            .iter()
            .skip(1) // the title row
            .map(|b| match b {
                Block::Image { url, indent, item } => {
                    format!("image:{indent}{}:{url}", if *item { "*" } else { "" })
                }
                Block::Inline {
                    indent,
                    item,
                    parts,
                } => {
                    let shape: Vec<String> = parts
                        .iter()
                        .map(|p| match p {
                            InlinePart::Image(u) => format!("img({u})"),
                            InlinePart::Formula { source, .. } => format!(
                                "math({})",
                                source
                                    .spans
                                    .iter()
                                    .map(|s| s.content.as_ref())
                                    .collect::<String>()
                            ),
                            InlinePart::Text(l) => format!(
                                "txt({})",
                                l.spans
                                    .iter()
                                    .map(|s| s.content.as_ref())
                                    .collect::<String>()
                            ),
                        })
                        .collect();
                    format!(
                        "inline:{indent}{}:{}",
                        if *item { "*" } else { "" },
                        shape.join("|")
                    )
                }
                Block::Text(l) => {
                    format!(
                        "text:{}",
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    )
                }
                _ => "other".into(),
            })
            .collect()
    };

    // A line that is nothing but the bracket becomes the picture.
    assert_eq!(
        shape("[https://example.com/a.png]"),
        vec!["image:0:https://example.com/a.png".to_string()],
    );

    // A bare URL stays a link — even at the start of the line, which is
    // exactly where the viewer used to turn it into a picture.
    let bare = shape("https://example.com/a.png こんな感じ");
    assert_eq!(bare.len(), 1, "{bare:?}");
    assert!(bare[0].starts_with("text:"), "{bare:?}");
    assert!(bare[0].contains("こんな感じ"));

    // A line that mixes text and pictures is ONE block: a sequence of
    // parts in the order written, which the viewer lays out like a
    // sentence with inline images.
    let after = shape("[https://example.com/a.png]こんな感じに後ろのテキストも表示される");
    assert_eq!(
        after,
        vec!["inline:0:img(https://example.com/a.png)|txt(こんな感じに後ろのテキストも表示される)"],
    );

    // Text BEFORE a picture is the same block, in the other order —
    // and the order is what the reading follows.
    let before = shape("先に本文 [https://example.com/a.png]");
    assert_eq!(
        before,
        vec!["inline:0:txt(先に本文 )|img(https://example.com/a.png)"]
    );

    // Cosense hangs pictures off bullets: an indented image line
    // belongs to the item above it, so it starts where that item's
    // text starts instead of flush left.
    assert_eq!(
        shape(" [https://example.com/a.png]"),
        vec!["image:2*:https://example.com/a.png".to_string()],
        "one level in = the column after `• `, and the picture IS the item",
    );
    // Indented, opening with the picture: the same block, carrying the
    // column its level starts at — and marked as a list ITEM, so the
    // bullet is not lost just because text was mixed in.
    let indented = shape("  [https://example.com/a.png] と本文");
    assert_eq!(indented.len(), 1, "{indented:?}");
    assert!(indented[0].starts_with("inline:4*:img("), "{indented:?}");
    assert!(indented[0].contains("txt( と本文)"), "{indented:?}");
    // Flush against the margin there is no item and no bullet.
    assert!(shape("[https://example.com/a.png] と本文")[0].starts_with("inline:0:"));

    // Quoted notation is a line ABOUT the picture, not a picture: a
    // documentation page must be able to show what it is describing.
    let quoted = shape("`[https://example.com/a.png]` → 画像になる");
    assert_eq!(quoted.len(), 1, "{quoted:?}");
    assert!(quoted[0].starts_with("text:"), "{quoted:?}");
    assert_eq!(shape("`[https://example.com/a.png]`").len(), 1);

    // Two pictures on one line: both, with the words between them kept
    // between them.
    let two = shape("[https://example.com/a.png] と [https://example.com/b.png]");
    assert_eq!(
        two,
        vec!["inline:0:img(https://example.com/a.png)|txt( と )|img(https://example.com/b.png)"],
    );
}

/// Star count is a heading LEVEL, mid-line as much as on a line of its
/// own, and a decorated link is still a link. Rendering the inline
/// forms as flat bold threw both away: `[* x]` and `[*** x]` looked
/// identical, and `[[[page]]]` was bold text you could not follow.
#[test]
fn inline_decorations_carry_the_heading_level_and_keep_links() {
    let pal = Palette::for_light(false);
    let render = |src: &str| -> RenderOutput {
        render_lines_with(
            &["t".to_string(), src.to_string()],
            None,
            &pal,
            &LinkTruth::default(),
        )
    };
    let styles = |src: &str| -> Vec<Style> {
        render(src)
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(l) => Some(l.spans.iter().map(|s| s.style).collect::<Vec<_>>()),
                _ => None,
            })
            .nth(1)
            .unwrap_or_default()
    };

    // Each level is its own look, and the inline form matches the
    // whole-line heading of the same level.
    let one = styles("→ [* 見出し]");
    let two = styles("→ [** 見出し]");
    let three = styles("→ [*** 見出し]");
    assert_ne!(one, two);
    assert_ne!(two, three);
    assert_eq!(
        *one.last().unwrap(),
        star_style(1, &pal),
        "one star = the level-4 heading style, same as a heading line",
    );
    assert_eq!(*two.last().unwrap(), star_style(2, &pal));
    assert_eq!(*three.last().unwrap(), star_style(3, &pal));
    // Bold is the floor: `[* x]` is how Cosense bolds a word, whatever
    // the theme does with the level on top of it.
    for st in [
        one.last().unwrap(),
        two.last().unwrap(),
        three.last().unwrap(),
    ] {
        assert!(st.add_modifier.contains(Modifier::BOLD), "{st:?}");
    }

    // Italic comes from the `/` flag and NOWHERE else: borrowing the
    // theme's italic markdown heading made `[* x]` look like `[/ x]`.
    for st in [
        one.last().unwrap(),
        two.last().unwrap(),
        three.last().unwrap(),
    ] {
        assert!(!st.add_modifier.contains(Modifier::ITALIC), "{st:?}");
    }
    let slash = *styles("→ [*/ 斜体]").last().unwrap();
    assert!(slash.add_modifier.contains(Modifier::ITALIC));
    assert!(slash.add_modifier.contains(Modifier::BOLD));
    assert_eq!(slash.fg, star_style(1, &pal).fg, "and keeps its level");

    // A decorated link: link colour, still underlined, bold — and not
    // italic, which was what "bold link" actually looked like.
    let deco = *styles("[[[改善案]]]").last().unwrap();
    assert!(deco.add_modifier.contains(Modifier::BOLD));
    assert!(!deco.add_modifier.contains(Modifier::ITALIC));

    // A link inside a decoration is REGISTERED as a link (so Enter can
    // follow it) and keeps the link colour.
    let out = render("[[[改善案]]]");
    assert_eq!(
        out.extracted.links,
        vec!["改善案".to_string()],
        "followable"
    );
    let deco = styles("[[[改善案]]]");
    assert_eq!(
        deco.last().unwrap().fg,
        Some(pal.link),
        "still reads as a link"
    );
    assert!(deco
        .last()
        .unwrap()
        .add_modifier
        .contains(Modifier::UNDERLINED));

    // Same for the single-bracket decoration form.
    let out = render("[* [改善案]]");
    assert_eq!(out.extracted.links, vec!["改善案".to_string()]);
}

/// A line of text and pictures used to lose its links: the hits went
/// into a vector nobody read. They now count the spans of the TEXT
/// parts in order, which is the one coordinate the viewer can map a
/// clicked cell back to (it lays the parts out itself).
#[test]
fn a_mixed_text_and_picture_line_keeps_its_hits_across_its_text_parts() {
    let out = render_lines(&[
        "title".into(),
        "本文 [Target] [https://example.com/a.png] 後 [Docs https://example.com]".into(),
    ]);
    let Block::Inline { parts, .. } = &out.blocks[1] else {
        panic!("expected an inline block")
    };
    let spans: Vec<String> = parts
        .iter()
        .flat_map(|p| match p {
            InlinePart::Text(l) => l.spans.iter().map(|s| s.content.to_string()).collect(),
            InlinePart::Image(_) | InlinePart::Formula { .. } => Vec::new(),
        })
        .collect();
    let hits = &out.hits[1];
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(spans[hits[0].span], "Target");
    assert_eq!(hits[0].target, HitTarget::Page("Target".into()));
    assert_eq!(
        spans[hits[1].span], "Docs",
        "the second part's hit is shifted past the first part's spans"
    );
    assert_eq!(
        hits[1].target,
        HitTarget::Url {
            label: "Docs".into(),
            url: "https://example.com".into()
        }
    );
}

/// 同じ行の後ろに行内コードがあっても、手前のリンクはリンク。以前は
/// コード片を先に切り出し、その手前を全部プレーンで流していたので、
/// `[crowdin] … `#3117`` の crowdin が括弧のまま出ていた(2026-09-03 実測)。
/// 逆に、コード片の中の `[括弧]` はコードのまま。
#[test]
fn a_link_before_inline_code_on_the_same_line_is_still_a_link() {
    let out = render_lines(&[
        "title".into(),
        "a [crowdin] b `#3117` / c `[not a link]`".into(),
    ]);
    let Block::Text(l) = &out.blocks[1] else {
        panic!()
    };
    let texts: Vec<&str> = l.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(
        texts,
        vec!["a ", "crowdin", " b ", " #3117 ", " / c ", " [not a link] "]
    );
    assert_eq!(
        out.hits[1],
        vec![Hit {
            span: 1,
            target: HitTarget::Page("crowdin".into())
        }]
    );
    assert_eq!(out.extracted.links, vec!["crowdin"]);
}
/// 実ページ(my-sandbox/文章入力遅延テスト)で起きたこと:ブロックの
/// 中の完全な空行で web はブロックを切り、後続のインデント行は箇条書きに
/// 戻る。本家パーサ(progfay/scrapbox-parser の packRows)も同じ:子は
/// 「ヘッダより深いインデントの行」だけ。旧実装は空行を越えてブロックを
/// 続けていたため、web と見た目が食い違っていた。
#[test]
fn a_truly_blank_line_ends_the_code_block_like_web() {
    let lines: Vec<String> = [
        "t",
        "code:テスト.txt",
        " これは",
        "",
        " だからそれは楽しい話なのかもしれません。",
        " やったね",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = render_lines(&lines);
    let got: Vec<String> = out.blocks.iter().map(plain).collect();
    // ブロックは これは で終わり、空行はページの空行、後続は箇条書き。
    assert_eq!(
        got,
        vec![
            "t",
            "code:テスト.txt",
            "  これは",
            "[BLANK]",
            "• だからそれは楽しい話なのかもしれません。",
            "• やったね",
        ]
    );
    // フラグも同じ: 空行と後続行はコードとして洗われない。
    let strs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let flags = code_line_flags(&strs);
    assert_eq!(flags, [false, false, true, false, false, false]);
}

#[test]
fn inline_formula_scan_tests() {
    assert_eq!(
        inline_formulas(r"積分[$ \int_0^1 x^2 dx ]と和[$ \sum_{i=1}^{n} i ]"),
        vec![r"\int_0^1 x^2 dx", r"\sum_{i=1}^{n} i"]
    );
    // ネストした ] も、逆引用符で括られた「記法についての文」も、
    // レンダラと同じ読み方をする。
    assert_eq!(
        inline_formulas(r"[$ [x-3]+a^2 ] と `[$ これは読まない ]`"),
        vec![r"[x-3]+a^2"]
    );
    assert!(inline_formulas("数式のない行").is_empty());
    assert!(inline_formulas(r"[リンクだけ] の行").is_empty());
}
