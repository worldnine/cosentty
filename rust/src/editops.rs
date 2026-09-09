//! Whole-page editing: diff the page's lines against an edited text and
//! emit minimal lineId-based [`EditOp`]s.
//!
//! This is what makes `^e` (edit the page in $EDITOR) safe and cheap:
//! unchanged lines keep their ids (permalinks, telomere, comments stay
//! attached), 1:1 changed lines become `replace` (id preserved!), and only
//! genuinely added/removed lines become insert/delete. Anchors are always
//! KEPT lines, so the ops survive the server's fast-forward check whenever
//! the page itself has not moved.

use crate::api::{EditOp, PageLine};

/// One step of the line-level edit script.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// old[i] == new[j], both advance.
    Match,
    /// old line removed.
    Del,
    /// new line added.
    Ins,
}

/// The LCS table is allowed this many cells. Beyond it the middle of the
/// page is paired positionally instead (see `edit_script`): the ops stay
/// correct, only the pairing is no longer the smallest possible one.
/// 4M cells of `u32` is 16 MB and a few milliseconds.
const LCS_CELL_CAP: usize = 4_000_000;

/// Edit script over line texts.
///
/// An external edit usually touches one region of the page, so the lines
/// shared at the top and bottom are matched first without any table.
/// Only the middle is diffed by LCS, whose time and memory are the product
/// of the two middle lengths. When even that is too large (`LCS_CELL_CAP`)
/// the middle is paired line by line, top to bottom: the extra lines on
/// one side become inserts or deletes. Either way every old line is
/// consumed and every new line is produced exactly once.
fn edit_script(old: &[&str], new: &[&str]) -> Vec<Step> {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (mid_old, mid_new) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let mut steps = Vec::with_capacity(old.len() + new.len() - head - tail);
    steps.extend(std::iter::repeat_n(Step::Match, head));
    if mid_old.len().saturating_mul(mid_new.len()) <= LCS_CELL_CAP {
        lcs_script(mid_old, mid_new, &mut steps);
    } else {
        positional_script(mid_old.len(), mid_new.len(), &mut steps);
    }
    steps.extend(std::iter::repeat_n(Step::Match, tail));
    steps
}

/// The classic O(n·m) LCS script, appended to `steps`.
fn lcs_script(old: &[&str], new: &[&str], steps: &mut Vec<Step>) {
    let n = old.len();
    let m = new.len();
    let w = m + 1;
    // dp[i*w + j] = LCS length of old[i..] vs new[j..]; one flat table
    // rather than a Vec per row.
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if old[i] == new[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            steps.push(Step::Match);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            steps.push(Step::Del);
            i += 1;
        } else {
            steps.push(Step::Ins);
            j += 1;
        }
    }
    steps.extend(std::iter::repeat_n(Step::Del, n - i));
    steps.extend(std::iter::repeat_n(Step::Ins, m - j));
}

/// Pair `n` old lines with `m` new lines in order, no table. Emitted as
/// one hunk (all deletes, then all inserts) so `diff_to_ops` pairs them
/// into replaces exactly as it would a changed region of the same shape.
fn positional_script(n: usize, m: usize, steps: &mut Vec<Step>) {
    steps.extend(std::iter::repeat_n(Step::Del, n));
    steps.extend(std::iter::repeat_n(Step::Ins, m));
}

/// Diff `old` (id, text) against `new` texts and emit ops.
///
/// Within one changed region, removed and added lines are paired 1:1 into
/// `replace` (top to bottom, ids preserved); leftovers become deletes or
/// one multi-line insert anchored at the next kept line (`_end` at the
/// bottom). An empty `new` returns no ops: deleting every line including
/// the title is page deletion, which is not this function's business.
pub fn diff_to_ops(old: &[(String, String)], new: &[String]) -> Vec<EditOp> {
    if new.is_empty() {
        return Vec::new();
    }
    let old_texts: Vec<&str> = old.iter().map(|(_, t)| t.as_str()).collect();
    let new_texts: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
    let steps = edit_script(&old_texts, &new_texts);

    let mut ops: Vec<EditOp> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize); // cursors into old / new
    let mut dels: Vec<usize> = Vec::new(); // old indices removed in this hunk
    let mut inss: Vec<usize> = Vec::new(); // new indices added in this hunk

    // Flush a hunk when the next KEPT old line (the anchor) is known.
    let flush = |ops: &mut Vec<EditOp>,
                 dels: &mut Vec<usize>,
                 inss: &mut Vec<usize>,
                 anchor: Option<usize>| {
        let pairs = dels.len().min(inss.len());
        for k in 0..pairs {
            ops.push(EditOp::Replace {
                id: old[dels[k]].0.clone(),
                text: new[inss[k]].clone(),
            });
        }
        for &d in dels.iter().skip(pairs) {
            ops.push(EditOp::Delete {
                id: old[d].0.clone(),
            });
        }
        if inss.len() > pairs {
            let text: Vec<&str> = inss[pairs..].iter().map(|&x| new[x].as_str()).collect();
            let anchor = anchor.map_or_else(|| "_end".to_string(), |a| old[a].0.clone());
            ops.push(EditOp::insert(anchor, &text.join("\n")));
        }
        dels.clear();
        inss.clear();
    };

    for step in steps {
        match step {
            Step::Match => {
                if !dels.is_empty() || !inss.is_empty() {
                    flush(&mut ops, &mut dels, &mut inss, Some(i));
                }
                i += 1;
                j += 1;
            }
            Step::Del => {
                dels.push(i);
                i += 1;
            }
            Step::Ins => {
                inss.push(j);
                j += 1;
            }
        }
    }
    if !dels.is_empty() || !inss.is_empty() {
        flush(&mut ops, &mut dels, &mut inss, None);
    }
    ops
}

/// Apply `ops` to the LOCAL page model — the same transition the server
/// will make. Insert ids are already in the op (client-generated), so the
/// local model and the server agree on every id without a reload.
pub fn apply_ops(lines: &mut Vec<PageLine>, ops: &[EditOp]) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for op in ops {
        match op {
            EditOp::Insert {
                anchor,
                lines: newl,
            } => {
                let at = if anchor == "_end" {
                    lines.len()
                } else {
                    lines
                        .iter()
                        .position(|l| l.id == *anchor)
                        .unwrap_or(lines.len())
                };
                for (k, (id, text)) in newl.iter().enumerate() {
                    lines.insert(
                        at + k,
                        PageLine {
                            id: id.clone(),
                            text: text.clone(),
                            user_id: String::new(),
                            created: now,
                            updated: now,
                        },
                    );
                }
            }
            EditOp::Replace { id, text } => {
                if let Some(l) = lines.iter_mut().find(|l| l.id == *id) {
                    l.text = text.clone();
                    l.updated = now;
                }
            }
            EditOp::Delete { id } => {
                lines.retain(|l| l.id != *id);
            }
        }
    }
}

/// Recognize the vertical-move lowering: a contiguous run is deleted and
/// the same texts are inserted elsewhere with fresh IDs. Its inverse must
/// move that same logical run, not rewrite the (possibly smaller) neighbor
/// that happened to cross it.
fn invert_move(lines: &[PageLine], ops: &[EditOp]) -> Option<Vec<EditOp>> {
    let (last, deletes) = ops.split_last()?;
    let EditOp::Insert {
        lines: inserted, ..
    } = last
    else {
        return None;
    };
    if deletes.len() != inserted.len() || deletes.is_empty() {
        return None;
    }
    let ids: Vec<&str> = deletes
        .iter()
        .map(|op| match op {
            EditOp::Delete { id } => Some(id.as_str()),
            EditOp::Insert { .. } | EditOp::Replace { .. } => None,
        })
        .collect::<Option<_>>()?;
    let start = lines.iter().position(|line| line.id == ids[0])?;
    let source = lines.get(start..start + ids.len())?;
    if source
        .iter()
        .map(|line| line.id.as_str())
        .ne(ids.iter().copied())
        || source
            .iter()
            .map(|line| line.text.as_str())
            .ne(inserted.iter().map(|(_, text)| text.as_str()))
        || inserted
            .iter()
            .any(|(id, _)| lines.iter().any(|line| line.id == *id))
    {
        return None;
    }

    let anchor = lines
        .get(start + source.len())
        .map(|line| line.id.clone())
        .unwrap_or_else(|| "_end".to_string());
    let mut inverse: Vec<EditOp> = inserted
        .iter()
        .map(|(id, _)| EditOp::Delete { id: id.clone() })
        .collect();
    inverse.push(EditOp::insert(
        anchor,
        &source
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    Some(inverse)
}

/// The inverse of `ops` against the state `lines` (BEFORE applying), such
/// that `apply(apply(lines, ops), invert_ops(lines, ops)) == lines` —
/// textually; re-inserted lines get fresh ids. Callers retaining older
/// history must rebind its references to those restoration ids.
///
/// Vertical moves are inverted as moves of the same logical source. Other
/// edits reverse the original operations by identity. In particular a replace
/// undoes to a replace on the SAME id, so permalinks survive undo. Redo is
/// simply the inverse of the inverse.
pub fn invert_ops(lines: &[PageLine], ops: &[EditOp]) -> Vec<EditOp> {
    if let Some(inverse) = invert_move(lines, ops) {
        return inverse;
    }
    // Invert identities, not an LCS of text: repeated blank lines must not
    // cause undo to delete a different line with the same contents.
    let mut current = lines.to_vec();
    let mut groups = Vec::new();
    for op in ops {
        let inverse = match op {
            EditOp::Insert { lines, .. } => lines
                .iter()
                .map(|(id, _)| EditOp::Delete { id: id.clone() })
                .collect(),
            EditOp::Replace { id, .. } => current
                .iter()
                .find(|l| l.id == *id)
                .map(|l| {
                    vec![EditOp::Replace {
                        id: id.clone(),
                        text: l.text.clone(),
                    }]
                })
                .unwrap_or_default(),
            EditOp::Delete { id } => current
                .iter()
                .position(|l| l.id == *id)
                .map(|i| {
                    vec![EditOp::insert(
                        current.get(i + 1).map(|l| l.id.as_str()).unwrap_or("_end"),
                        &current[i].text,
                    )]
                })
                .unwrap_or_default(),
        };
        groups.push(inverse);
        apply_ops(&mut current, std::slice::from_ref(op));
    }
    let mut inverse: Vec<EditOp> = groups.into_iter().rev().flatten().collect();
    // Earlier deletions can anchor to a line restored by a later deletion.
    let deleted: Vec<_> = ops
        .iter()
        .filter_map(|op| {
            if let EditOp::Delete { id } = op {
                Some(id)
            } else {
                None
            }
        })
        .rev()
        .collect();
    let restored: Vec<_> = inverse
        .iter()
        .filter_map(|op| {
            if let EditOp::Insert { lines, .. } = op {
                Some(lines[0].0.clone())
            } else {
                None
            }
        })
        .collect();
    for op in &mut inverse {
        let target = match op {
            EditOp::Insert { anchor, .. } => anchor,
            EditOp::Replace { id, .. } | EditOp::Delete { id } => id,
        };
        if let Some(new) = deleted
            .iter()
            .position(|id| *id == target)
            .and_then(|i| restored.get(i))
        {
            *target = new.clone();
        }
    }
    inverse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_blank_insert_deletes_only_the_inserted_identity() {
        let lines: Vec<PageLine> = serde_json::from_value(serde_json::json!([
            {"id":"title","text":"title"}, {"id":"old","text":""}
        ]))
        .unwrap();
        let ops = vec![EditOp::Insert {
            anchor: "old".into(),
            lines: vec![("new".into(), "".into())],
        }];
        assert_eq!(
            invert_ops(&lines, &ops),
            vec![EditOp::Delete { id: "new".into() }]
        );
    }

    fn old(lines: &[&str]) -> Vec<(String, String)> {
        lines
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("id{i}"), t.to_string()))
            .collect()
    }
    fn new(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// Comparable shape of an op: inserted line ids are random, so tests
    /// compare (kind, target/anchor, texts) instead of raw equality.
    fn shape(op: &EditOp) -> (String, String, String) {
        match op {
            EditOp::Replace { id, text } => ("replace".into(), id.clone(), text.clone()),
            EditOp::Delete { id } => ("delete".into(), id.clone(), String::new()),
            EditOp::Insert { anchor, lines } => (
                "insert".into(),
                anchor.clone(),
                lines
                    .iter()
                    .map(|(_, t)| t.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
    fn shapes(ops: &[EditOp]) -> Vec<(String, String, String)> {
        ops.iter().map(shape).collect()
    }
    fn s(k: &str, a: &str, t: &str) -> (String, String, String) {
        (k.into(), a.into(), t.into())
    }

    #[test]
    fn unchanged_page_needs_no_ops() {
        let o = old(&["title", "a", "b"]);
        assert!(diff_to_ops(&o, &new(&["title", "a", "b"])).is_empty());
    }

    #[test]
    fn changed_line_becomes_replace_preserving_its_id() {
        let o = old(&["title", "a", "b"]);
        let ops = diff_to_ops(&o, &new(&["title", "A!", "b"]));
        assert_eq!(
            ops,
            vec![EditOp::Replace {
                id: "id1".into(),
                text: "A!".into()
            }]
        );
    }

    #[test]
    fn title_rename_is_a_replace_of_line_zero() {
        let o = old(&["title", "a"]);
        let ops = diff_to_ops(&o, &new(&["new title", "a"]));
        assert_eq!(
            ops,
            vec![EditOp::Replace {
                id: "id0".into(),
                text: "new title".into()
            }]
        );
    }

    #[test]
    fn added_lines_become_one_insert_before_the_next_kept_line() {
        let o = old(&["title", "a", "b"]);
        let ops = diff_to_ops(&o, &new(&["title", "a", "x", "y", "b"]));
        assert_eq!(shapes(&ops), vec![s("insert", "id2", "x\ny")]);
        // inserted lines carry fresh 24-hex ids
        if let EditOp::Insert { lines, .. } = &ops[0] {
            assert!(lines.iter().all(|(id, _)| id.len() == 24));
        }
    }

    #[test]
    fn appended_lines_anchor_at_end() {
        let o = old(&["title", "a"]);
        let ops = diff_to_ops(&o, &new(&["title", "a", "x"]));
        assert_eq!(shapes(&ops), vec![s("insert", "_end", "x")]);
    }

    #[test]
    fn removed_lines_become_deletes() {
        let o = old(&["title", "a", "b", "c"]);
        let ops = diff_to_ops(&o, &new(&["title", "c"]));
        assert_eq!(
            ops,
            vec![
                EditOp::Delete { id: "id1".into() },
                EditOp::Delete { id: "id2".into() },
            ]
        );
    }

    #[test]
    fn mixed_hunk_pairs_replaces_then_extras() {
        // a,b → X (1 replace + 1 delete); then d gains lines after it
        let o = old(&["t", "a", "b", "keep", "d"]);
        let ops = diff_to_ops(&o, &new(&["t", "X", "keep", "d", "tail1", "tail2"]));
        assert_eq!(
            shapes(&ops),
            vec![
                s("replace", "id1", "X"),
                s("delete", "id2", ""),
                s("insert", "_end", "tail1\ntail2"),
            ]
        );
    }

    #[test]
    fn emptied_page_is_refused() {
        let o = old(&["title", "a"]);
        assert!(diff_to_ops(&o, &[]).is_empty());
    }

    fn pl(id: &str, text: &str) -> PageLine {
        PageLine {
            id: id.into(),
            text: text.into(),
            user_id: String::new(),
            created: 0,
            updated: 0,
        }
    }
    fn texts(lines: &[PageLine]) -> Vec<String> {
        lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn apply_ops_mirrors_the_server_transition() {
        let mut lines = vec![pl("a", "title"), pl("b", "one"), pl("c", "two")];
        apply_ops(
            &mut lines,
            &[
                EditOp::Replace {
                    id: "b".into(),
                    text: "ONE".into(),
                },
                EditOp::Insert {
                    anchor: "c".into(),
                    lines: vec![("x".into(), "mid".into())],
                },
                EditOp::Delete { id: "c".into() },
                EditOp::Insert {
                    anchor: "_end".into(),
                    lines: vec![("y".into(), "tail".into())],
                },
            ],
        );
        assert_eq!(texts(&lines), vec!["title", "ONE", "mid", "tail"]);
        assert_eq!(lines[2].id, "x");
    }

    #[test]
    fn invert_roundtrips_textually() {
        let orig = vec![
            pl("a", "title"),
            pl("b", "one"),
            pl("c", "two"),
            pl("d", "three"),
        ];
        let ops = vec![
            EditOp::Replace {
                id: "b".into(),
                text: "ONE".into(),
            },
            EditOp::Insert {
                anchor: "c".into(),
                lines: vec![("x".into(), "mid".into())],
            },
            EditOp::Delete { id: "d".into() },
        ];
        let inv = invert_ops(&orig, &ops);
        let mut lines = orig.clone();
        apply_ops(&mut lines, &ops);
        assert_eq!(texts(&lines), vec!["title", "ONE", "mid", "two"]);
        apply_ops(&mut lines, &inv);
        assert_eq!(texts(&lines), texts(&orig), "undo restores the text");
        // replace undo kept the line id (permalinks survive)
        assert_eq!(lines[1].id, "b");
    }

    #[test]
    fn multi_line_delete_undoes_in_order_regardless_of_delete_order() {
        let orig = vec![
            pl("t", "title"),
            pl("a", "1"),
            pl("b", "2"),
            pl("c", "3"),
            pl("z", "end"),
        ];
        for ops in [
            // top-down and bottom-up delete of the a..c range
            vec![
                EditOp::Delete { id: "a".into() },
                EditOp::Delete { id: "b".into() },
                EditOp::Delete { id: "c".into() },
            ],
            vec![
                EditOp::Delete { id: "c".into() },
                EditOp::Delete { id: "b".into() },
                EditOp::Delete { id: "a".into() },
            ],
        ] {
            let inv = invert_ops(&orig, &ops);
            let mut lines = orig.clone();
            apply_ops(&mut lines, &ops);
            assert_eq!(texts(&lines), vec!["title", "end"]);
            apply_ops(&mut lines, &inv);
            assert_eq!(texts(&lines), texts(&orig), "ops={ops:?}");
        }
    }

    #[test]
    fn deleting_the_tail_undoes_via_end_anchor() {
        let orig = vec![pl("t", "title"), pl("a", "1"), pl("b", "2")];
        let ops = vec![
            EditOp::Delete { id: "a".into() },
            EditOp::Delete { id: "b".into() },
        ];
        let inv = invert_ops(&orig, &ops);
        let mut lines = orig.clone();
        apply_ops(&mut lines, &ops);
        apply_ops(&mut lines, &inv);
        assert_eq!(texts(&lines), texts(&orig));
    }

    #[test]
    fn redo_is_the_inverse_of_the_inverse() {
        let orig = vec![pl("t", "title"), pl("a", "1")];
        let ops = vec![
            EditOp::Replace {
                id: "a".into(),
                text: "ONE".into(),
            },
            EditOp::Insert {
                anchor: "_end".into(),
                lines: vec![("n".into(), "new".into())],
            },
        ];
        let mut lines = orig.clone();
        apply_ops(&mut lines, &ops);
        let after = texts(&lines);
        let undo = invert_ops(&orig, &ops);
        let redo = invert_ops(&lines, &undo);
        apply_ops(&mut lines, &undo);
        assert_eq!(texts(&lines), texts(&orig));
        apply_ops(&mut lines, &redo);
        assert_eq!(texts(&lines), after);
    }

    #[test]
    fn move_undo_and_redo_recreate_only_the_moved_source() {
        let orig = vec![
            pl("t", "title"),
            pl("a", "neighbor"),
            pl("b", "root"),
            pl("c", " child"),
            pl("z", "after"),
        ];
        let moved = vec![("n1".into(), "root".into()), ("n2".into(), " child".into())];
        let ops = vec![
            EditOp::Delete { id: "b".into() },
            EditOp::Delete { id: "c".into() },
            EditOp::Insert {
                anchor: "a".into(),
                lines: moved,
            },
        ];

        let undo = invert_ops(&orig, &ops);
        assert!(matches!(&undo[0], EditOp::Delete { id } if id == "n1"));
        assert!(matches!(&undo[1], EditOp::Delete { id } if id == "n2"));
        assert!(matches!(&undo[2], EditOp::Insert { anchor, .. } if anchor == "z"));

        let mut after = orig.clone();
        apply_ops(&mut after, &ops);
        let redo = invert_ops(&after, &undo);
        apply_ops(&mut after, &undo);
        assert_eq!(texts(&after), texts(&orig));
        assert_eq!(after[1].id, "a", "the smaller neighbor kept its ID");
        assert_eq!(after[4].id, "z", "the following line kept its ID");

        apply_ops(&mut after, &redo);
        assert_eq!(
            texts(&after),
            vec!["title", "root", " child", "neighbor", "after"]
        );
        assert_eq!(after[3].id, "a", "redo still moves the user's target");
        assert_eq!(after[4].id, "z");
    }

    /// Every old line must be consumed and every new line produced once,
    /// whatever route the script took.
    fn assert_script_shape(old: &[&str], new: &[&str]) -> Vec<Step> {
        let steps = edit_script(old, new);
        let dels = steps.iter().filter(|s| **s != Step::Ins).count();
        let inss = steps.iter().filter(|s| **s != Step::Del).count();
        assert_eq!(dels, old.len(), "old lines consumed");
        assert_eq!(inss, new.len(), "new lines produced");
        // Replaying the script reproduces `new`.
        let (mut i, mut j) = (0, 0);
        for s in &steps {
            match s {
                Step::Match => {
                    assert_eq!(old[i], new[j], "a Match pairs equal lines");
                    i += 1;
                    j += 1;
                }
                Step::Del => i += 1,
                Step::Ins => j += 1,
            }
        }
        steps
    }

    #[test]
    fn shared_head_and_tail_are_matched_without_a_table() {
        // 3 shared on top, 2 below, one changed line in the middle: the
        // script is exactly what a full LCS would say.
        let old = ["t", "a", "b", "x", "y", "z"];
        let new = ["t", "a", "b", "X", "y", "z"];
        let steps = assert_script_shape(&old, &new);
        assert_eq!(
            steps,
            vec![
                Step::Match,
                Step::Match,
                Step::Match,
                Step::Del,
                Step::Ins,
                Step::Match,
                Step::Match
            ]
        );
        // An identical page: all matches, an empty middle.
        assert!(assert_script_shape(&old, &old)
            .iter()
            .all(|s| *s == Step::Match));
        // Everything shared on top, extra lines only below.
        assert_eq!(
            assert_script_shape(&["t", "a"], &["t", "a", "b"]),
            vec![Step::Match, Step::Match, Step::Ins]
        );
        assert_eq!(
            assert_script_shape(&["t", "a", "b"], &["t"]),
            vec![Step::Match, Step::Del, Step::Del]
        );
        // A repeated line at the edge: the head takes as many as match,
        // and the middle still comes out consistent.
        assert_script_shape(&["a", "a", "a"], &["a", "a"]);
        assert_script_shape(&["a", "b", "a"], &["a", "a"]);
    }

    #[test]
    fn an_oversized_middle_pairs_positionally_and_still_replaces() {
        // Two pages with no line in common and a product above the cap.
        // Each side is 2001+ lines so the middle is 2001*2001 > 4M cells.
        let n = 2_001;
        let olds: Vec<String> = (0..n).map(|i| format!("old {i}")).collect();
        let news: Vec<String> = (0..n + 2).map(|i| format!("new {i}")).collect();
        let old: Vec<&str> = olds.iter().map(String::as_str).collect();
        let new: Vec<&str> = news.iter().map(String::as_str).collect();
        let steps = assert_script_shape(&old, &new);
        assert_eq!(steps.iter().filter(|s| **s == Step::Del).count(), n);
        assert_eq!(steps.iter().filter(|s| **s == Step::Ins).count(), n + 2);
        // Through `diff_to_ops`, the lines pair into replaces (ids kept)
        // and the two extra lines become one insert at the end.
        let page: Vec<(String, String)> = olds
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("id{i}"), t.clone()))
            .collect();
        let ops = diff_to_ops(&page, &news);
        assert_eq!(ops.len(), n + 1);
        assert!(matches!(&ops[0], EditOp::Replace { id, text } if id == "id0" && text == "new 0"));
        assert!(
            matches!(&ops[n], EditOp::Insert { anchor, lines } if anchor == "_end" && lines.len() == 2)
        );
    }

    #[test]
    fn duplicate_lines_do_not_confuse_the_pairing() {
        // Scrapbox pages are full of empty lines; the LCS must keep them
        // stable and only move the actual change.
        let o = old(&["t", "", "a", "", "b", ""]);
        let ops = diff_to_ops(&o, &new(&["t", "", "a", "", "B", ""]));
        assert_eq!(
            ops,
            vec![EditOp::Replace {
                id: "id4".into(),
                text: "B".into()
            }]
        );
    }
}
