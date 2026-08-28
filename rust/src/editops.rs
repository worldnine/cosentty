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

/// LCS edit script over line texts. O(n·m) — pages are at most a few
/// thousand lines, this is instant.
fn edit_script(old: &[&str], new: &[&str]) -> Vec<Step> {
    let n = old.len();
    let m = new.len();
    // dp[i][j] = LCS length of old[i..] vs new[j..]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut steps = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            steps.push(Step::Match);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            steps.push(Step::Del);
            i += 1;
        } else {
            steps.push(Step::Ins);
            j += 1;
        }
    }
    steps.extend(std::iter::repeat(Step::Del).take(n - i));
    steps.extend(std::iter::repeat(Step::Ins).take(m - j));
    steps
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
            ops.push(EditOp::Delete { id: old[d].0.clone() });
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
            EditOp::Insert { anchor, lines: newl } => {
                let at = if anchor == "_end" {
                    lines.len()
                } else {
                    lines.iter().position(|l| l.id == *anchor).unwrap_or(lines.len())
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

/// The inverse of `ops` against the state `lines` (BEFORE applying), such
/// that `apply(apply(lines, ops), invert_ops(lines, ops)) == lines` —
/// textually; re-inserted lines get fresh ids, which is harmless.
///
/// Implemented as `diff_to_ops(after, before)`: the undo program IS the
/// minimal edit from the post-state back to the pre-state, computed by the
/// same LCS engine that powers `^e` — one correctness story instead of a
/// hand-rolled per-op inversion (which gets sibling-insert ordering wrong).
/// Lines whose text survives keep their ids; in particular a replace
/// undoes to a replace on the SAME id, so permalinks survive undo. Redo is
/// simply the inverse of the inverse.
pub fn invert_ops(lines: &[PageLine], ops: &[EditOp]) -> Vec<EditOp> {
    let before_texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
    let mut after = lines.to_vec();
    apply_ops(&mut after, ops);
    let after_pairs: Vec<(String, String)> =
        after.iter().map(|l| (l.id.clone(), l.text.clone())).collect();
    diff_to_ops(&after_pairs, &before_texts)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n"),
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
        assert_eq!(ops, vec![EditOp::Replace { id: "id1".into(), text: "A!".into() }]);
    }

    #[test]
    fn title_rename_is_a_replace_of_line_zero() {
        let o = old(&["title", "a"]);
        let ops = diff_to_ops(&o, &new(&["new title", "a"]));
        assert_eq!(ops, vec![EditOp::Replace { id: "id0".into(), text: "new title".into() }]);
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
                EditOp::Replace { id: "b".into(), text: "ONE".into() },
                EditOp::Insert { anchor: "c".into(), lines: vec![("x".into(), "mid".into())] },
                EditOp::Delete { id: "c".into() },
                EditOp::Insert { anchor: "_end".into(), lines: vec![("y".into(), "tail".into())] },
            ],
        );
        assert_eq!(texts(&lines), vec!["title", "ONE", "mid", "tail"]);
        assert_eq!(lines[2].id, "x");
    }

    #[test]
    fn invert_roundtrips_textually() {
        let orig = vec![pl("a", "title"), pl("b", "one"), pl("c", "two"), pl("d", "three")];
        let ops = vec![
            EditOp::Replace { id: "b".into(), text: "ONE".into() },
            EditOp::Insert { anchor: "c".into(), lines: vec![("x".into(), "mid".into())] },
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
        let orig = vec![pl("t", "title"), pl("a", "1"), pl("b", "2"), pl("c", "3"), pl("z", "end")];
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
        let ops = vec![EditOp::Delete { id: "a".into() }, EditOp::Delete { id: "b".into() }];
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
            EditOp::Replace { id: "a".into(), text: "ONE".into() },
            EditOp::Insert { anchor: "_end".into(), lines: vec![("n".into(), "new".into())] },
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
    fn duplicate_lines_do_not_confuse_the_pairing() {
        // Scrapbox pages are full of empty lines; the LCS must keep them
        // stable and only move the actual change.
        let o = old(&["t", "", "a", "", "b", ""]);
        let ops = diff_to_ops(&o, &new(&["t", "", "a", "", "B", ""]));
        assert_eq!(ops, vec![EditOp::Replace { id: "id4".into(), text: "B".into() }]);
    }
}
