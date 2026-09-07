use crate::*;
use crate::tests::support::*;

/// コメントは書かれた版に固定される。NOW のコメントは過去版に出ず、
/// 過去版で書いたコメントはその版でだけ出る(akapen の履歴モデル)。
/// 以前は行番号だけで結び付いていて、過去版の無関係な行の下に出ていた。
#[test]
fn a_comment_lives_on_the_revision_it_was_written_on() {
    use cosense::api::SnapshotStamp;
    let mut app = page(&["title", "one", "two"]);
    app.rebuild(40);
    app.cursor = 1;
    let now_c = app.make_comment("now".into()).unwrap();
    assert!(now_c.revision.is_none());
    app.comments.push(now_c);
    assert!(app.src_has_comment(1));

    // 過去版を開く(ネットワーク無し: 一覧と本文を手で置く)。
    app.time = Some(TimeMachine {
        points: vec![SnapshotStamp { id: "snap1".into(), created: 1_700_000_000 }],
        pos: 0,
        cache: HashMap::new(),
    });
    app.laid_width = 0;
    app.rebuild(40);
    assert!(!app.src_has_comment(1), "a NOW comment is not shown on a snapshot");
    assert!(!app.rows.iter().any(|r| matches!(r, Row::Card { .. }) && !matches!(r, Row::Card { line } if line.spans.is_empty())), "no card woven");

    // 過去版で書く: その版に固定され、そこでは見える。
    let old_c = app.make_comment("put it back".into()).unwrap();
    assert_eq!(old_c.snapshot_id(), Some("snap1"));
    assert_eq!(old_c.page_id, "pid");
    app.comments.push(old_c);
    assert!(app.src_has_comment(1));
    assert_eq!(app.comment_for_range(), Some(1), "c again edits the snapshot's comment, not NOW's");

    // NOW に戻ると、過去版のコメントは隠れ、NOW のものが戻る。
    app.time = None;
    assert!(app.src_has_comment(1));
    assert_eq!(app.comment_for_range(), Some(0));
}

/// 一覧の Enter は、そのコメントの版まで戻ってから行へ飛ぶ。NOW の
/// コメントを過去版の閲覧中に選べば NOW へ戻る(ここでは再取得が失敗
/// するので履歴に留まる——それは既存の Esc と同じ振る舞い)。
#[test]
fn the_list_travels_to_the_comments_revision_before_landing() {
    use cosense::api::{PageLine, Snapshot, SnapshotStamp};
    let ctx = offline_ctx();
    let mut app = page(&["title", "one", "two"]);
    app.rebuild(40);
    let stamp = SnapshotStamp { id: "snap1".into(), created: 1_700_000_000 };
    let snap = Snapshot {
        title: "t".into(),
        created: 1_700_000_000,
        lines: vec![
            PageLine { id: "l0".into(), text: "title".into(), user_id: String::new(), created: 0, updated: 0 },
            PageLine { id: "l1".into(), text: "old one".into(), user_id: String::new(), created: 0, updated: 0 },
        ],
    };
    // 過去版のコメントを1件持った状態で NOW にいる。一覧は Enter で
    // その版へ入る(一覧は手元、本文はキャッシュから)。
    app.snapshots = Some(vec![stamp.clone()]);
    app.comments.push(Comment {
        project: "proj".into(),
        title: "t".into(),
        page_id: "pid".into(),
        revision: Some(cosense::comment::Revision { snapshot_id: "snap1".into(), created: 1_700_000_000 }),
        start: 1,
        end: 1,
        line_texts: vec!["old one".into()],
        line_ids: vec!["l1".into()],
        text: "back".into(),
    });
    let mut cache = HashMap::new();
    cache.insert("snap1".to_string(), snap);
    // The machine is seeded by hand (its cache holds the body, so no
    // fetch is needed) and the snapshot installed as ← would.
    app.time = Some(TimeMachine { points: vec![stamp.clone()], pos: 0, cache });
    show_snapshot(&mut app, &ctx, 0);
    assert_eq!(app.lines[1].text, "old one");
    // Asking for the revision on screen is a no-op, not a refetch.
    show_revision(&mut app, &ctx, "snap1");
    assert_eq!(app.lines[1].text, "old one");
    assert!(app.time.is_some());
    app.overlay = Some(Overlay::Comments { cursor: 0 });
    handle_key(&mut app, &ctx, key(KeyCode::Enter));
    assert!(app.overlay.is_none());
    assert_eq!(app.cursor, 1);
    assert!(app.time.as_ref().is_some_and(|tm| tm.points[tm.pos].id == "snap1"), "stays on the comment's revision");
    assert!(app.src_has_comment(1), "and the card is there");
}
