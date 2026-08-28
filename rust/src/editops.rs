//! Whole-page editing: diff the page's lines against an edited text and
//! emit minimal lineId-based [`EditOp`]s.
//!
//! This is what makes `^e` (edit the page in $EDITOR) safe and cheap:
//! unchanged lines keep their ids (permalinks, telomere, comments stay
//! attached), 1:1 changed lines become `replace` (id preserved!), and only
//! genuinely added/removed lines become insert/delete. Anchors are always
//! KEPT lines, so the ops survive the server's fast-forward check whenever
//! the page itself has not moved.

use crate::api::EditOp;

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
            ops.push(EditOp::Insert { anchor, text: text.join("\n") });
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
        assert_eq!(
            ops,
            vec![EditOp::Insert { anchor: "id2".into(), text: "x\ny".into() }]
        );
    }

    #[test]
    fn appended_lines_anchor_at_end() {
        let o = old(&["title", "a"]);
        let ops = diff_to_ops(&o, &new(&["title", "a", "x"]));
        assert_eq!(ops, vec![EditOp::Insert { anchor: "_end".into(), text: "x".into() }]);
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
            ops,
            vec![
                EditOp::Replace { id: "id1".into(), text: "X".into() },
                EditOp::Delete { id: "id2".into() },
                EditOp::Insert { anchor: "_end".into(), text: "tail1\ntail2".into() },
            ]
        );
    }

    #[test]
    fn emptied_page_is_refused() {
        let o = old(&["title", "a"]);
        assert!(diff_to_ops(&o, &[]).is_empty());
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
