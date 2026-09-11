use super::support::*;
use crate::*;

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
        use cosense::theme::TelomereState as S;
        let state = if unread { S::Unread } else { S::Read };
        gutter_cell(Some((related_age(updated), state)), false, None)
    };
    let now = now_secs();
    assert_eq!(mark(now - 60, true).0, "█", "edited a minute ago: thick");
    assert_eq!(
        mark(now - 86_400 * 3, true).0,
        "▌",
        "three days ago: thinner"
    );
    // Unread keeps its blue at every thickness; read is the gutter gray.
    assert_eq!(
        mark(now - 60, true).1.fg,
        Some(cosense::theme::telomere(60, true, false).1)
    );
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
fn updated_after_load_demotes_to_unread_on_the_next_visit() {
    use cosense::theme::TelomereState as S;
    let now = now_secs();
    let mut app = App::new("proj".into());
    let line = |updated: i64| PageLine {
        id: "L1".into(),
        text: "a".into(),
        user_id: String::new(),
        created: now,
        updated,
    };
    // First visit (never seen before): every line is unread, except the
    // one that changed AT this visit — web's `.updated-after-load`.
    app.read_at = None;
    app.open_stamp = now;
    assert_eq!(app.line_state(&line(now - 100)), S::Unread);
    assert_eq!(app.line_state(&line(now)), S::UpdatedAfterLoad);
    // The next visit catches `read_at` up to the previous one, so the
    // line we watched change is now plain unread — and read after that.
    app.read_at = Some(now);
    app.open_stamp = now + 10;
    assert_eq!(app.line_state(&line(now)), S::Unread);
    app.read_at = Some(now + 10);
    assert_eq!(app.line_state(&line(now)), S::Read);
    // History shows no news at all: the past is all read.
    app.read_at = None;
    app.open_stamp = now;
    in_history(&mut app);
    assert_eq!(app.line_state(&line(now)), S::Read);
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
        accessed: 0,
        created: 0,
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
    let secs = build_related(
        &PageFacts::of(&page),
        page.related.as_ref(),
        "proj",
        cosense::index::SortKey::Updated,
        &HashMap::new(),
    );
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
    assert_eq!(
        secs[1].entries[0].title, "Shared12",
        "first hub claims the shared page"
    );
    assert_eq!(secs[2].entries[0].title, "OnlyHub2");
    assert_eq!(secs[3].entries[0].title, "Orphan");
    assert_eq!(
        secs[4].entries[0].title, "/other/Page",
        "bare /project is not a page"
    );
}

/// The list's sort order reaches the related sections too: each
/// section is reordered on the same key (the block carries `accessed`
/// and `created` next to `updated` and `linked`), while the sections
/// themselves keep their place — Links, then one group per hub in
/// page order. `views` is not in the block, so that order leaves the
/// server's (updated, newest first) alone. The row shows the value it
/// is sorted on.
#[test]
fn related_sections_follow_the_index_sort_order_within_each_section() {
    use cosense::api::{Page, RelatedPage, RelatedPages};
    use cosense::index::SortKey;
    let rp = |title: &str, updated: i64, accessed: i64, created: i64, linked: i64| RelatedPage {
        id: String::new(),
        title: title.into(),
        title_lc: title.to_lowercase(),
        descriptions: vec![],
        links_lc: vec!["hub".into()],
        linked,
        updated,
        accessed,
        created,
    };
    let page = Page {
        id: String::new(),
        persistent: true,
        title: "me".into(),
        commit_id: String::new(),
        lines: vec![],
        links: vec!["Hub".into()],
        project_links: vec!["/z/Page".into(), "/a/Page".into()],
        related: Some(RelatedPages {
            // Server order: updated, newest first.
            links1hop: vec![
                rp("bee", 30, 1, 3, 5),
                rp("Ant", 20, 3, 1, 9),
                rp("cat", 10, 2, 2, 7),
            ],
            links2hop: vec![rp("Two", 2, 1, 2, 1), rp("One", 1, 2, 1, 2)],
            ..Default::default()
        }),
        updated: 0,
        created: 0,
        lines_count: 0,
        last_accessed: None,
    };
    let facts = PageFacts::of(&page);
    let titles = |sort: SortKey| -> Vec<Vec<String>> {
        build_related(&facts, page.related.as_ref(), "proj", sort, &HashMap::new())
            .iter()
            .map(|s| s.entries.iter().map(|e| e.title.clone()).collect())
            .collect()
    };
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        titles(SortKey::Updated),
        vec![
            v(&["bee", "Ant", "cat"]),
            v(&["Two", "One"]),
            v(&["/z/Page", "/a/Page"])
        ]
    );
    assert_eq!(
        titles(SortKey::Views),
        titles(SortKey::Updated),
        "views is not reported: server order"
    );
    assert_eq!(titles(SortKey::Accessed)[0], v(&["Ant", "cat", "bee"]));
    assert_eq!(titles(SortKey::Created)[0], v(&["bee", "cat", "Ant"]));
    assert_eq!(titles(SortKey::Linked)[0], v(&["Ant", "cat", "bee"]));
    let by_title = titles(SortKey::Title);
    assert_eq!(
        by_title[0],
        v(&["Ant", "bee", "cat"]),
        "A→Z, case-insensitive"
    );
    assert_eq!(
        by_title[1],
        v(&["One", "Two"]),
        "sections stay put, each sorted on its own"
    );
    assert_eq!(
        by_title[2],
        v(&["/a/Page", "/z/Page"]),
        "external links have only a title to sort on"
    );

    // The dim value after the title is the one being sorted on.
    let secs = build_related(
        &facts,
        page.related.as_ref(),
        "proj",
        SortKey::Linked,
        &HashMap::new(),
    );
    assert_eq!(secs[0].entries[0].sort_meta(SortKey::Linked), "linked 9");
    assert!(
        secs[0].entries[0]
            .sort_meta(SortKey::Accessed)
            .ends_with('y'),
        "an epoch-3 stamp is years old"
    );
    assert_eq!(
        secs[2].entries[0].sort_meta(SortKey::Linked),
        "",
        "no count for a cross-project link"
    );
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
        accessed: 0,
        created: 0,
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
    assert!(
        !m.missing("改善案"),
        "a page exists while you are reading it"
    );
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
            links1hop: vec![RelatedPage {
                links_lc: vec!["実況".into()],
                ..rp("日記")
            }],
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
    assert!(
        m.missing("だれも書いていない"),
        "this page is the only one saying it"
    );
    let page = shared;

    // Opening a link to an uncreated page answers the question about
    // THAT title too — in the negative. Recording it as existing (it
    // is, after all, the page being read) would tell the page we came
    // from that its link is fine.
    let template = Page {
        persistent: false,
        ..page
    };
    assert_eq!(truth(&template).exists("改善案"), Some(false));

    // A response without relatedPages must not turn every link red.
    let bare = Page {
        related: None,
        ..template
    };
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
        accessed: 0,
        created: 0,
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
    assert!(
        app.related.is_empty(),
        "nothing is known while the fetch is out"
    );

    // A block for a page the reader has already left says nothing about
    // this one, and must not lower the gate either.
    app.related_tx
        .send((
            "proj".into(),
            "other".into(),
            RelatedAnswer::Block(Box::new(block())),
        ))
        .unwrap();
    assert!(!app.drain_related());
    assert!(app.related_pending, "that answer was about another page");
    assert!(app.related.is_empty());

    app.related_tx
        .send((
            "proj".into(),
            "me".into(),
            RelatedAnswer::Block(Box::new(block())),
        ))
        .unwrap();
    assert!(
        app.drain_related(),
        "the page has to be laid out and coloured again"
    );
    assert!(!app.related_pending);
    let heads: Vec<&str> = app.related.iter().map(|s| s.heading.as_str()).collect();
    assert_eq!(heads, vec!["Links (1)", "Hub (1)"]);
    assert_eq!(app.virtual_items.len(), 2, "related rows are addressable");
    assert!(
        app.links.missing("だれも書いていない"),
        "nobody but this page says it"
    );
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

    app.related_tx
        .send(("proj".into(), "me".into(), RelatedAnswer::Failed))
        .unwrap();
    assert!(!app.drain_related(), "a failure changes nothing on screen");
    assert!(!app.related_pending);
    app.probe_unknown_links();
    assert_eq!(probes.try_recv().unwrap().title, "だれも書いていない");
}

/// A REFUSED related fetch is the opposite case: the server is already
/// holding this viewer off, and the fallback costs one or two requests
/// per link. The prober stays shut.
#[test]
fn a_refused_related_fetch_does_not_hand_the_links_to_the_prober() {
    let mut app = App::new("proj".into());
    app.title = "me".into();
    app.page_id = "P".into();
    app.lines = vec![PageLine {
        id: "l0".into(),
        text: "[だれも書いていない] [これも]".into(),
        user_id: String::new(),
        created: 0,
        updated: 0,
    }];
    let (tx, probes) = mpsc::channel();
    app.link_probe_tx = Some(tx);
    app.related_pending = true;

    app.related_tx
        .send(("proj".into(), "me".into(), RelatedAnswer::Refused))
        .unwrap();
    assert!(!app.drain_related(), "a refusal changes nothing on screen");
    assert!(!app.related_pending);
    assert!(app.related_refused);
    app.probe_unknown_links();
    assert!(
        probes.try_recv().is_err(),
        "not one lookup goes out while the server is refusing"
    );

    // The next fetch that ANSWERS is what opens the prober again.
    app.related_pending = true;
    app.related_refused = false;
    app.related_tx
        .send(("proj".into(), "me".into(), RelatedAnswer::Failed))
        .unwrap();
    app.drain_related();
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

/// Following a link asks for the page in the background: the page on
/// screen stays as it is, the status says what was asked for, and only
/// the answer moves the reader — with history recorded then, not before.
#[test]
fn navigation_keeps_the_current_page_until_the_fetch_lands() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.title = "A".into();
    navigate_to(&mut app, &ctx, "proj", "B");
    assert_eq!(app.title, "A", "still on A while B is fetched");
    assert!(app.history.is_empty(), "nothing on the stack yet");
    assert!(
        app.status.contains("/proj/B"),
        "the status names the fetch: {}",
        app.status
    );
    assert_eq!(
        app.pending_load.as_ref().map(|p| &p.intent),
        Some(&LoadIntent::Navigate {
            from: Place::Page {
                project: "proj".into(),
                title: "A".into()
            },
            create: false
        })
    );

    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("B", &["B", "body"]))
    ));
    assert_eq!(app.title, "B");
    assert_eq!(app.lines[1].text, "body");
    assert_eq!(
        app.history,
        vec![Place::Page {
            project: "proj".into(),
            title: "A".into()
        }]
    );
    assert!(app.pending_load.is_none());
    assert!(
        app.status.is_empty(),
        "the loading status is gone: {}",
        app.status
    );
}

/// A fetch that fails leaves the reader where they were, with a word on
/// why; the stack is untouched.
#[test]
fn a_failed_navigation_leaves_the_reader_in_place() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.title = "A".into();
    navigate_to(&mut app, &ctx, "proj", "B");
    fail_pending_load(&mut app, &ctx, "HTTP 500");
    assert_eq!(app.title, "A");
    assert!(app.history.is_empty());
    assert!(app.toast_text().contains("/proj/B"), "{}", app.toast_text());
    assert!(
        app.toast_text().contains("HTTP 500"),
        "{}",
        app.toast_text()
    );
    assert!(app.status.is_empty());
}

/// Two links in a row: only the last one asked for counts. The first
/// answer arrives with an older generation and is dropped, whatever its
/// order on the wire.
#[test]
fn a_newer_request_supersedes_an_older_one() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.title = "A".into();
    navigate_to(&mut app, &ctx, "proj", "B");
    let old_gen = app.pending_load.as_ref().unwrap().gen;
    navigate_to(&mut app, &ctx, "proj", "C");
    assert_eq!(
        app.pending_load.as_ref().map(|p| p.title.as_str()),
        Some("C")
    );

    // The stale answer: B lands after C was asked for.
    app.page_load_tx
        .send(PageLoadMsg {
            gen: old_gen,
            result: Ok(LoadedPage {
                page: server_page("B", &["B"]),
                editable: false,
            }),
        })
        .unwrap();
    assert!(
        !drain_page_loads(&mut app, &ctx),
        "an abandoned fetch installs nothing"
    );
    assert_eq!(app.title, "A");
    assert!(app.pending_load.is_some(), "C is still awaited");

    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("C", &["C"]))
    ));
    assert_eq!(app.title, "C");
    assert_eq!(app.history.len(), 1, "one move, not two");
}

/// `[` pops its destination when it asks for it. Until the page lands the
/// stacks are as if the key had not been pressed on the destination's
/// side; a failure — or a newer request — puts the destination back so
/// the same key retries.
#[test]
fn a_history_hop_returns_its_destination_when_it_does_not_land() {
    let ctx = test_ctx();
    let mut app = page(&["B", "one"]);
    app.title = "B".into();
    let a = Place::Page {
        project: "proj".into(),
        title: "A".into(),
    };
    app.history.push(a.clone());

    go_history(&mut app, &ctx, true);
    assert!(
        app.history.is_empty(),
        "the destination was taken off the stack"
    );
    assert_eq!(app.title, "B", "still on B");
    fail_pending_load(&mut app, &ctx, "offline");
    assert_eq!(
        app.history,
        vec![a.clone()],
        "a failed back keeps the destination"
    );
    assert!(app.forward.is_empty());

    // A back superseded by a link: the destination goes back too.
    go_history(&mut app, &ctx, true);
    navigate_to(&mut app, &ctx, "proj", "C");
    assert_eq!(
        app.history,
        vec![a.clone()],
        "the abandoned back restored A"
    );
    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("C", &["C"]))
    ));
    assert_eq!(
        app.history,
        vec![
            a.clone(),
            Place::Page {
                project: "proj".into(),
                title: "B".into()
            }
        ]
    );

    // And a back that lands moves the place left onto the forward stack.
    go_history(&mut app, &ctx, true);
    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("B", &["B"]))
    ));
    assert_eq!(app.title, "B");
    assert_eq!(app.history, vec![a]);
    assert_eq!(
        app.forward,
        vec![Place::Page {
            project: "proj".into(),
            title: "C".into()
        }]
    );
}

/// Opening a row from the list keeps the list on screen until the page
/// lands; the list itself is the place that goes onto history.
#[test]
fn opening_from_the_index_closes_it_only_when_the_page_lands() {
    let ctx = test_ctx();
    let mut app = page(&["A", "one"]);
    app.title = "A".into();
    app.index_project = "proj".into();
    app.index = Some(cosense::index::Index::new(
        vec![cosense::index::Entry {
            title: "B".into(),
            updated: now_secs(),
            ..Default::default()
        }],
        1,
        cosense::index::SortKey::Updated,
    ));
    open_from_index(&mut app, &ctx, Some(("B".into(), false)));
    assert!(app.index.is_some(), "the list stays up while B is fetched");
    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("B", &["B"]))
    ));
    assert!(app.index.is_none(), "the list closes when the page lands");
    assert_eq!(app.title, "B");
    assert!(matches!(app.history.last(), Some(Place::Index { project, .. }) if project == "proj"));
}

/// 非同期ページ移動のレビュー(2026-09-07)で見つかった 3 件の再現。
fn warmed_ctx() -> Ctx {
    let ctx = test_ctx();
    ctx.project_settings
        .lock()
        .unwrap()
        .insert("proj".into(), None);
    ctx.editability.lock().unwrap().insert("proj".into(), true);
    ctx
}

#[test]
fn pending_navigation_preserves_text_typed_while_waiting() {
    let ctx = warmed_ctx();
    let mut app = page(&["A", "body"]);
    app.title = "A".into();
    navigate_to(&mut app, &ctx, "proj", "B");
    enter_session(&mut app, &ctx, 1, 4);
    app.session.as_mut().unwrap().input.insert_str(" UNSAVED");
    answer_pending_load(&mut app, &ctx, Ok(server_page("B", &["B", "new body"])));
    let retained = app
        .session
        .as_ref()
        .is_some_and(|s| s.input.buf.contains("UNSAVED"));
    let queued = drain_jobs(&mut app).iter().any(|job| {
        job.1
            .iter()
            .any(|op| matches!(op, EditOp::Replace { text, .. } if text.contains("UNSAVED")))
    });
    assert!(
        retained || queued,
        "arrival discarded the dirty session without queueing a commit"
    );
}

#[test]
fn entering_projects_cancels_the_old_page_request() {
    let ctx = warmed_ctx();
    let mut app = page(&["A", "body"]);
    navigate_to(&mut app, &ctx, "proj", "B");
    let gen = app.pending_load.as_ref().unwrap().gen;
    app.projects_cache = Some(ProjectsCache {
        projects: vec![],
        at: Instant::now(),
    });
    open_projects(&mut app, &ctx, true);
    assert!(app
        .index
        .as_ref()
        .is_some_and(|i| i.scope == cosense::index::Scope::Projects));
    app.page_load_tx
        .send(PageLoadMsg {
            gen,
            result: Ok(LoadedPage {
                page: server_page("B", &["B"]),
                editable: true,
            }),
        })
        .unwrap();
    assert!(
        !drain_page_loads(&mut app, &ctx),
        "an obsolete page request replaced the projects picker"
    );
}

#[test]
fn two_back_presses_do_not_reorder_the_remaining_history() {
    let ctx = warmed_ctx();
    let mut app = page(&["D", "body"]);
    app.title = "D".into();
    let place = |title: &str| Place::Page {
        project: "proj".into(),
        title: title.into(),
    };
    app.history = vec![place("A"), place("B"), place("C")];
    go_history(&mut app, &ctx, true);
    go_history(&mut app, &ctx, true);
    assert_eq!(app.pending_load.as_ref().unwrap().title, "B");
    answer_pending_load(&mut app, &ctx, Ok(server_page("B", &["B"])));
    assert_eq!(
        app.history,
        vec![place("A")],
        "C is newer than B and must not become B's back destination"
    );
    // 飛ばした C と出発点 D は進む側に、押す順(C, D)で並ぶ。
    assert_eq!(app.forward, vec![place("D"), place("C")]);
    assert_eq!(app.title, "B");
}

#[test]
fn back_at_the_history_boundary_keeps_the_last_destination() {
    // A→B と訪問し、B で `[` を素早く 2 回。2 回目は戻る先がないので、A の読み込みを
    // 取り消さずそのまま待つ。
    let ctx = warmed_ctx();
    let mut app = page(&["B", "body"]);
    app.title = "B".into();
    let a = Place::Page {
        project: "proj".into(),
        title: "A".into(),
    };
    app.history = vec![a.clone()];
    go_history(&mut app, &ctx, true);
    go_history(&mut app, &ctx, true);
    assert!(
        app.pending_load.as_ref().is_some_and(|p| p.title == "A"),
        "the extra press must not cancel A: history={:?} forward={:?}",
        app.history,
        app.forward
    );
    assert!(app.history.is_empty() && app.forward.is_empty());
    assert!(answer_pending_load(
        &mut app,
        &ctx,
        Ok(server_page("A", &["A"]))
    ));
    assert_eq!(app.title, "A");
    assert_eq!(
        app.forward,
        vec![Place::Page {
            project: "proj".into(),
            title: "B".into()
        }]
    );
}

/// 訪問記録の保存先は起点で決めて持ち回る。`None` ならファイルに触れないので、
/// テストが開発者の実 `visits.json` を読んだり書いたりすることがない。
#[test]
fn visits_stay_in_memory_without_a_path_and_persist_with_one() {
    assert_eq!(record_visit(None, "proj", "A", 10), None);
    assert_eq!(
        record_visit(None, "proj", "A", 20),
        None,
        "nothing was kept"
    );
    assert!(load_visits(None).is_empty());

    let dir = std::env::temp_dir().join(format!("cosentty-test-visits-{}", std::process::id()));
    let path = dir.join("visits.json");
    assert_eq!(record_visit(Some(&path), "proj", "A", 10), None);
    assert_eq!(record_visit(Some(&path), "proj", "A", 20), Some(10));
    assert_eq!(load_visits(Some(&path)).get("proj/A"), Some(&20));
    let _ = std::fs::remove_dir_all(dir);
}
