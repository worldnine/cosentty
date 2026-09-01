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

        let (ug, us) = gutter_cell(false, None, Some(true), false);
        let (rg, rs) = gutter_cell(false, None, Some(false), false);
        assert_eq!((ug, rg), ("▏", "▏"), "read state must not encode thickness");
        assert_eq!(us.fg, Some(Color::LightBlue));
        assert_eq!(rs.fg, Some(Color::DarkGray));
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
        let secs = build_related(&page, "proj");
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
        let m = link_truth(&page);
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
        let m = link_truth(&shared);
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
        assert_eq!(link_truth(&template).exists("改善案"), Some(false));

        // A response without relatedPages must not turn every link red.
        let bare = Page { related: None, ..template };
        assert!(!link_truth(&bare).missing("732"));
        assert!(link_truth(&bare).is_empty());
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
