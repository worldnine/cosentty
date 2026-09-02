use crate::*;
use super::support::*;

    #[test]
    fn related_telomere_has_only_read_and_unread_states() {
        let mut visits = HashMap::new();
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 80);
        assert!(related_is_unread(&visits, "proj", "Page", 100));
        visits.insert("proj/Page".into(), 120);
        assert!(!related_is_unread(&visits, "proj", "Page", 100));
        // No timestamp (external project link): a recorded visit is enough.
        assert!(!related_is_unread(&visits, "proj", "Page", 0));

        // A related row now wears the SAME mark as a body line and as an
        // index row: thickness = how recently that page changed, colour =
        // whether it has been seen. It used to be a flat hairline with
        // colour only, which made the one gutter column mean two different
        // things depending on where you were looking.
        let mark = |updated: i64, unread: bool| {
            gutter_cell(false, Some((related_age(updated), unread)), false)
        };
        let now = now_secs();
        assert_eq!(mark(now - 60, true).0, "█", "edited a minute ago: thick");
        assert_eq!(mark(now - 86_400 * 3, true).0, "▌", "three days ago: thinner");
        // Unread keeps its blue at every thickness; read is the gutter gray.
        assert_eq!(mark(now - 60, true).1.fg, Some(cosense::theme::telomere(60, true, false).1));
        assert_eq!(
            mark(now - 60, false).1.fg,
            Some(cosense::theme::border_color(false))
        );
        // An undated entry (a cross-project link: the related list carries
        // no timestamp for those) must not claim to be brand new.
        assert_eq!(mark(0, false).0, "▏");
        assert_eq!(related_age(0), UNDATED_AGE);
    }

    #[test]
    fn build_related_groups_2hop_under_hubs_and_dedupes() {
        use cosense::api::{Page, RelatedPage, RelatedPages};
        let rp = |title: &str, links: &[&str]| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: title.to_lowercase(),
            descriptions: vec![],
            links_lc: links.iter().map(|s| s.to_lowercase()).collect(),
            linked: 0,
            updated: 0,
        };
        let page = Page {
            id: String::new(),
            persistent: true,
            title: "me".into(),
            commit_id: String::new(),
            lines: vec![],
            links: vec!["Hub1".into(), "Hub2".into()],
            project_links: vec!["/other/Page".into(), "/bare".into()],
            related: Some(RelatedPages {
                links1hop: vec![rp("Direct", &[])],
                links2hop: vec![
                    rp("Shared12", &["hub1", "hub2"]),
                    rp("OnlyHub2", &["hub2"]),
                    rp("Direct", &["hub1"]), // already 1-hop → dropped
                    rp("Orphan", &["gone"]), // hub no longer on the page
                ],
                has_back_links_or_icons: true,
            }),
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        let secs = build_related(&PageFacts::of(&page), page.related.as_ref(), "proj");
        let heads: Vec<&str> = secs.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(
            heads,
            vec![
                "Links (1)",
                "Hub1 (1)",
                "Hub2 (1)",
                "2 hop links (1)",
                "External links (1)"
            ]
        );
        assert_eq!(secs[1].entries[0].title, "Shared12", "first hub claims the shared page");
        assert_eq!(secs[2].entries[0].title, "OnlyHub2");
        assert_eq!(secs[3].entries[0].title, "Orphan");
        assert_eq!(secs[4].entries[0].title, "/other/Page", "bare /project is not a page");
    }

    /// The uncreated links of a page are read out of the same response
    /// that drew it: `links` minus the 1-hop neighbours (which the API
    /// builds from EXISTING pages only). Shaped after a real reply — the
    /// project's own 「改善案」, whose only dead links were its `#issue`
    /// tags.
    #[test]
    fn a_pages_links_are_read_against_its_existing_neighbours() {
        use cosense::api::{Page, RelatedPage, RelatedPages};
        let rp = |title: &str| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: cosense::render::title_lc(title),
            descriptions: vec![],
            links_lc: vec![],
            linked: 0,
            updated: 0,
        };
        let page = Page {
            id: "P".into(),
            persistent: true,
            title: "改善案".into(),
            commit_id: String::new(),
            lines: vec![],
            links: vec![
                "テスト".into(),
                "選択した文字 URL".into(),
                "732".into(),
                "改善案".into(), // a page linking to itself
            ],
            project_links: vec![],
            related: Some(RelatedPages {
                links1hop: vec![rp("テスト"), rp("選択した文字_URL")],
                links2hop: vec![],
                has_back_links_or_icons: true,
            }),
            updated: 0,
            created: 0,
            lines_count: 0,
            last_accessed: None,
        };
        // The body and the related block arrive separately now (v2 does not
        // ship the block), so the reading is always "these facts, against
        // this block".
        let truth = |p: &Page| link_truth(&PageFacts::of(p), p.related.as_ref());
        let m = truth(&page);
        assert!(m.missing("732"), "a tag with no page behind it");
        assert!(!m.missing("テスト"), "listed as a neighbour");
        assert!(
            !m.missing("選択した文字 URL"),
            "the neighbour is spelled with `_`, the link with a space"
        );
        assert!(!m.missing("改善案"), "a page exists while you are reading it");
        assert_eq!(m.exists("何も知らないページ"), None, "no reading, no claim");

        // The rule is not "does the page exist". A title nobody wrote is
        // still live once a SECOND page uses it — Cosense's index marks
        // every link target live except where the only page writing it is
        // the one on screen. Shaped after villagepump/井戸端, where `実況`
        // is unwritten yet drawn as an ordinary link because other pages
        // link to it too.
        let shared = Page {
            links: vec!["実況".into(), "だれも書いていない".into()],
            related: Some(RelatedPages {
                links1hop: vec![RelatedPage { links_lc: vec!["実況".into()], ..rp("日記") }],
                links2hop: vec![],
                has_back_links_or_icons: true,
            }),
            ..page
        };
        let m = truth(&shared);
        assert!(
            !m.missing("実況"),
            "another page writes the same word: a shared word is not empty"
        );
        assert!(m.missing("だれも書いていない"), "this page is the only one saying it");
        let page = shared;

        // Opening a link to an uncreated page answers the question about
        // THAT title too — in the negative. Recording it as existing (it
        // is, after all, the page being read) would tell the page we came
        // from that its link is fine.
        let template = Page { persistent: false, ..page };
        assert_eq!(truth(&template).exists("改善案"), Some(false));

        // A response without relatedPages must not turn every link red.
        let bare = Page { related: None, ..template };
        assert!(!truth(&bare).missing("732"));
        assert!(truth(&bare).is_empty());
    }

    /// The body (v2) arrives WITHOUT the related block, so the sections
    /// below the page and the colour of its links both come out of a second
    /// request that lands a beat later. That merge is the only thing that
    /// ever puts them on screen — and because `Page::related` is an
    /// `Option`, losing it would be silent: the related list would simply
    /// never appear again.
    #[test]
    fn the_related_block_landing_after_the_body_fills_in_sections_and_link_colours() {
        use cosense::api::{RelatedPage, RelatedPages};
        let rp = |title: &str, links: &[&str]| RelatedPage {
            id: String::new(),
            title: title.into(),
            title_lc: title.to_lowercase(),
            descriptions: vec![],
            links_lc: links.iter().map(|s| s.to_lowercase()).collect(),
            linked: 0,
            updated: 0,
        };
        let block = || RelatedPages {
            links1hop: vec![rp("Direct", &[])],
            links2hop: vec![rp("Shared", &["hub"])],
            has_back_links_or_icons: true,
        };

        let mut app = App::new("proj".into());
        app.title = "me".into();
        app.facts = PageFacts {
            title: "me".into(),
            persistent: true,
            links: vec!["Hub".into(), "だれも書いていない".into()],
            project_links: vec![],
        };
        app.related_pending = true;
        assert!(app.related.is_empty(), "nothing is known while the fetch is out");

        // A block for a page the reader has already left says nothing about
        // this one, and must not lower the gate either.
        app.related_tx.send(("proj".into(), "other".into(), Some(block()))).unwrap();
        assert!(!app.drain_related());
        assert!(app.related_pending, "that answer was about another page");
        assert!(app.related.is_empty());

        app.related_tx.send(("proj".into(), "me".into(), Some(block()))).unwrap();
        assert!(app.drain_related(), "the page has to be laid out and coloured again");
        assert!(!app.related_pending);
        let heads: Vec<&str> = app.related.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(heads, vec!["Links (1)", "Hub (1)"]);
        assert_eq!(app.virtual_items.len(), 2, "related rows are addressable");
        assert!(app.links.missing("だれも書いていない"), "nobody but this page says it");
        assert!(!app.links.missing("Hub"), "a neighbour shares the word");
    }

    /// A failed related fetch has to come back all the same: it is what
    /// lowers the gate, and only then may the link prober go and ask about
    /// the links one at a time.
    #[test]
    fn a_failed_related_fetch_lowers_the_gate_and_hands_the_links_to_the_prober() {
        let mut app = App::new("proj".into());
        app.title = "me".into();
        app.page_id = "P".into();
        app.lines = vec![PageLine {
            id: "l0".into(),
            text: "[だれも書いていない]".into(),
            user_id: String::new(),
            created: 0,
            updated: 0,
        }];
        let (tx, probes) = mpsc::channel();
        app.link_probe_tx = Some(tx);

        // Nothing is asked while the page's own answer is still coming —
        // that burst is exactly what the related block exists to prevent.
        app.related_pending = true;
        app.probe_unknown_links();
        assert!(probes.try_recv().is_err());

        app.related_tx.send(("proj".into(), "me".into(), None)).unwrap();
        assert!(!app.drain_related(), "a failure changes nothing on screen");
        assert!(!app.related_pending);
        app.probe_unknown_links();
        assert_eq!(probes.try_recv().unwrap().title, "だれも書いていない");
    }

    #[test]
    fn unread_is_edited_after_last_seen_or_never_seen() {
        assert!(unread_since(100, None), "first visit: everything is unread");
        assert!(unread_since(101, Some(100)));
        assert!(!unread_since(100, Some(100)));
        assert!(!unread_since(50, Some(100)));
        let mut app = page(&["a", "b", "c"]);
        app.lines[1].updated = 500;
        app.read_at = Some(200);
        assert_eq!(app.unread_count(), 1);
        app.read_at = None;
        assert_eq!(app.unread_count(), 3);
    }
