//! Pure source-line outline planning.
//!
//! Outline depth is deliberately smaller than Unicode whitespace: only an
//! ASCII space, a tab, and U+3000 count, one character per level. Plans name
//! source-line ranges and never know about wrapped display rows or line IDs.

/// An inclusive range of source lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    fn len(self) -> usize {
        self.end - self.start + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Lines(LineRange),
    Block(usize),
    /// A block whose extent was frozen when the reader grabbed it (the
    /// sticky move mode). Its first line is the root. Indentation may have
    /// changed since the grab, so the extent is no longer re-derivable from
    /// the text: the reader picked up THESE lines and no others.
    Grabbed(LineRange),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Where moved source text is inserted in the original line ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Before(usize),
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Replace {
        range: LineRange,
        texts: Vec<String>,
    },
    Move {
        range: LineRange,
        destination: Destination,
        /// Start of the moved range after applying the plan.
        destination_start: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    InvalidTarget,
    TitleProtected,
    CannotOutdent,
    Boundary,
    NotSibling,
}

/// Count exactly the supported leading indentation characters.
pub fn depth(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t' | '\u{3000}'))
        .count()
}

/// The extent of the block rooted at `root`: what the sticky move mode
/// freezes when the reader grabs it, before any of its own edits.
pub fn grab(lines: &[String], root: usize) -> Result<LineRange, PlanError> {
    let range = subtree(lines, root)?;
    if range.start == 0 {
        return Err(PlanError::TitleProtected);
    }
    Ok(range)
}

/// Plan one outline action against source lines.
pub fn plan(lines: &[String], scope: Scope, direction: Direction) -> Result<Plan, PlanError> {
    let range = match scope {
        Scope::Lines(range) | Scope::Grabbed(range) => checked_range(lines, range)?,
        Scope::Block(root) => subtree(lines, root)?,
    };
    if range.start == 0 {
        return Err(PlanError::TitleProtected);
    }

    match direction {
        Direction::Left | Direction::Right => horizontal(lines, range, direction),
        Direction::Up | Direction::Down => match scope {
            Scope::Lines(_) => move_lines(lines, range, direction),
            Scope::Block(root) => move_block(lines, root, range, direction),
            // The grabbed range says what moves; its first line still says
            // at what depth and under which parent it is looking for a
            // sibling.
            Scope::Grabbed(range) => move_block(lines, range.start, range, direction),
        },
    }
}

fn checked_range(lines: &[String], range: LineRange) -> Result<LineRange, PlanError> {
    if range.start > range.end || range.end >= lines.len() {
        Err(PlanError::InvalidTarget)
    } else {
        Ok(range)
    }
}

fn subtree(lines: &[String], root: usize) -> Result<LineRange, PlanError> {
    if root >= lines.len() {
        return Err(PlanError::InvalidTarget);
    }
    let root_depth = depth(&lines[root]);
    let mut end = root;
    while end + 1 < lines.len() && depth(&lines[end + 1]) > root_depth {
        end += 1;
    }
    Ok(LineRange::new(root, end))
}

fn horizontal(lines: &[String], range: LineRange, direction: Direction) -> Result<Plan, PlanError> {
    let target = &lines[range.start..=range.end];
    let texts = match direction {
        Direction::Right => target.iter().map(|line| format!(" {line}")).collect(),
        Direction::Left => {
            if target.iter().any(|line| depth(line) == 0) {
                return Err(PlanError::CannotOutdent);
            }
            target
                .iter()
                .map(|line| {
                    line.chars()
                        .next()
                        .map(|ch| line[ch.len_utf8()..].to_string())
                        .unwrap()
                })
                .collect()
        }
        Direction::Up | Direction::Down => unreachable!(),
    };
    Ok(Plan::Replace { range, texts })
}

fn move_lines(lines: &[String], range: LineRange, direction: Direction) -> Result<Plan, PlanError> {
    match direction {
        // Line 0 remains the title, so line 1 cannot move above it.
        Direction::Up if range.start <= 1 => Err(PlanError::Boundary),
        Direction::Up => Ok(Plan::Move {
            range,
            destination: Destination::Before(range.start - 1),
            destination_start: range.start - 1,
        }),
        Direction::Down if range.end + 1 >= lines.len() => Err(PlanError::Boundary),
        Direction::Down => Ok(Plan::Move {
            range,
            destination: if range.end + 2 < lines.len() {
                Destination::Before(range.end + 2)
            } else {
                Destination::End
            },
            // Only the one adjacent source line crosses the target range.
            destination_start: range.start + 1,
        }),
        Direction::Left | Direction::Right => unreachable!(),
    }
}

fn parent(lines: &[String], line: usize) -> Option<usize> {
    let child_depth = depth(&lines[line]);
    (0..line)
        .rev()
        .find(|&candidate| depth(&lines[candidate]) < child_depth)
}

/// A whole-sibling step: over the previous or next sibling subtree at once.
/// This is the fast way to reorder sections; `Scope::Lines` is the one that
/// steps a single source line, and between them nothing is unreachable.
fn move_block(
    lines: &[String],
    root: usize,
    range: LineRange,
    direction: Direction,
) -> Result<Plan, PlanError> {
    let root_depth = depth(&lines[root]);
    let root_parent = parent(lines, root);
    match direction {
        Direction::Up => {
            if root <= 1 {
                return Err(PlanError::Boundary);
            }
            let prev = root - 1;
            // The ordinary case: step over the previous sibling's whole
            // subtree, found by walking back past the lines under it.
            let mut sibling = prev;
            while sibling > 0 && depth(&lines[sibling]) > root_depth {
                sibling -= 1;
            }
            if sibling > 0
                && depth(&lines[sibling]) == root_depth
                && parent(lines, sibling) == root_parent
            {
                return Ok(Plan::Move {
                    range,
                    destination: Destination::Before(sibling),
                    destination_start: sibling,
                });
            }
            Err(PlanError::NotSibling)
        }
        Direction::Down => {
            let next = range.end + 1;
            if next >= lines.len() {
                return Err(PlanError::Boundary);
            }
            if depth(&lines[next]) != root_depth || parent(lines, next) != root_parent {
                return Err(PlanError::NotSibling);
            }
            let over = subtree(lines, next)?;
            Ok(Plan::Move {
                range,
                destination: if over.end + 1 < lines.len() {
                    Destination::Before(over.end + 1)
                } else {
                    Destination::End
                },
                destination_start: range.start + over.len(),
            })
        }
        Direction::Left | Direction::Right => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &[&str]) -> Vec<String> {
        source.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn depth_counts_only_the_three_supported_characters_one_by_one() {
        assert_eq!(depth(" \t\u{3000}body"), 3);
        assert_eq!(depth("\u{00a0} body"), 0);
        assert_eq!(depth(""), 0);
    }

    #[test]
    fn indent_adds_ascii_and_outdent_removes_exactly_one_supported_character() {
        let source = lines(&["title", "\tbody", "\u{3000}child", "  leaf"]);
        assert_eq!(
            plan(
                &source,
                Scope::Lines(LineRange::new(1, 3)),
                Direction::Right
            ),
            Ok(Plan::Replace {
                range: LineRange::new(1, 3),
                texts: lines(&[" \tbody", " \u{3000}child", "   leaf"]),
            })
        );
        assert_eq!(
            plan(&source, Scope::Lines(LineRange::new(1, 3)), Direction::Left),
            Ok(Plan::Replace {
                range: LineRange::new(1, 3),
                texts: lines(&["body", "child", " leaf"]),
            })
        );
    }

    #[test]
    fn title_and_partial_outdent_are_rejected_all_or_nothing() {
        let source = lines(&[" title", " child", "", "\tleaf"]);
        assert_eq!(
            plan(
                &source,
                Scope::Lines(LineRange::new(0, 1)),
                Direction::Right
            ),
            Err(PlanError::TitleProtected)
        );
        assert_eq!(
            plan(&source, Scope::Lines(LineRange::new(1, 3)), Direction::Left),
            Err(PlanError::CannotOutdent)
        );
    }

    #[test]
    fn block_is_the_maximal_following_run_of_greater_depth() {
        let source = lines(&[
            "title",
            " root",
            "\t\u{3000}skipped",
            "   leaf",
            " peer",
            "tail",
        ]);
        assert_eq!(
            plan(&source, Scope::Block(1), Direction::Right),
            Ok(Plan::Replace {
                range: LineRange::new(1, 3),
                texts: lines(&["  root", " \t\u{3000}skipped", "    leaf"]),
            })
        );
    }

    #[test]
    fn line_range_moves_across_exactly_one_adjacent_source_line() {
        let source = lines(&["title", "a", "b", "c", "d"]);
        assert_eq!(
            plan(&source, Scope::Lines(LineRange::new(2, 3)), Direction::Up),
            Ok(Plan::Move {
                range: LineRange::new(2, 3),
                destination: Destination::Before(1),
                destination_start: 1,
            })
        );
        assert_eq!(
            plan(&source, Scope::Lines(LineRange::new(1, 2)), Direction::Down),
            Ok(Plan::Move {
                range: LineRange::new(1, 2),
                destination: Destination::Before(4),
                destination_start: 2,
            })
        );
        assert_eq!(
            plan(&source, Scope::Lines(LineRange::new(1, 1)), Direction::Up),
            Err(PlanError::Boundary)
        );
    }

    #[test]
    fn block_moves_only_against_a_sibling_subtree_with_the_same_parent() {
        let source = lines(&[
            "title",
            " parent",
            "  first",
            "   child",
            "  second",
            "    skipped",
            " next-parent",
            "  outsider",
        ]);
        assert_eq!(
            plan(&source, Scope::Block(2), Direction::Down),
            Ok(Plan::Move {
                range: LineRange::new(2, 3),
                destination: Destination::Before(6),
                destination_start: 4,
            })
        );
        assert_eq!(
            plan(&source, Scope::Block(4), Direction::Down),
            Err(PlanError::NotSibling),
            "the next subtree belongs to another parent"
        );
        assert_eq!(
            plan(&source, Scope::Block(2), Direction::Up),
            Err(PlanError::NotSibling),
            "an ancestor is not a sibling"
        );
    }

    #[test]
    fn grab_freezes_the_subtree_and_refuses_the_title() {
        let source = lines(&["title", " root", "  child", " peer"]);
        assert_eq!(grab(&source, 1), Ok(LineRange::new(1, 2)));
        assert_eq!(grab(&source, 0), Err(PlanError::TitleProtected));
        assert_eq!(grab(&source, 9), Err(PlanError::InvalidTarget));
    }

    /// Apply a plan the way the mode does: pull the range out and splice it
    /// back at `destination_start`. Used by the sweeps below.
    fn apply(source: &[String], plan: &Plan) -> Vec<String> {
        let mut out = source.to_vec();
        match plan {
            Plan::Replace { range, texts } => {
                for (line, text) in (range.start..=range.end).zip(texts) {
                    out[line] = text.clone();
                }
            }
            Plan::Move {
                range,
                destination_start,
                ..
            } => {
                let block: Vec<String> = out.drain(range.start..=range.end).collect();
                let at = (*destination_start).min(out.len());
                out.splice(at..at, block);
            }
        }
        out
    }

    /// While a grab still matches its indentation it is not a special case:
    /// the whole-sibling step must move it exactly as a recomputed block
    /// moves. Once an outdent makes the grab smaller than its subtree the
    /// two part company, which is what the next test covers.
    #[test]
    fn a_fresh_grab_moves_exactly_like_a_recomputed_block() {
        let mut checked = 0usize;
        for pattern in 0..3usize.pow(5) {
            let mut source = vec!["title".to_string()];
            let mut rest = pattern;
            for k in 0..5 {
                source.push(format!("{}line{k}", " ".repeat(rest % 3)));
                rest /= 3;
            }
            for root in 1..source.len() {
                let Ok(range) = subtree(&source, root) else {
                    continue;
                };
                for direction in [Direction::Up, Direction::Down] {
                    let block = plan(&source, Scope::Block(root), direction);
                    let grabbed = plan(&source, Scope::Grabbed(range), direction);
                    assert_eq!(
                        block, grabbed,
                        "root {root} of {source:?} moving {direction:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 1000, "the sweep actually ran: {checked} pairs");
    }

    /// A grabbed step must never lose a line, never touch the title, and
    /// never break the block apart. Swept over every arrangement of five
    /// body lines across three depths and every grabbed range.
    #[test]
    fn a_grabbed_step_keeps_the_page_and_the_block_whole() {
        let mut checked = 0usize;
        for pattern in 0..3usize.pow(5) {
            let mut source = vec!["title".to_string()];
            let mut rest = pattern;
            for k in 0..5 {
                source.push(format!("{}line{k}", " ".repeat(rest % 3)));
                rest /= 3;
            }
            for start in 1..source.len() {
                for end in start..source.len() {
                    let grabbed = LineRange::new(start, end);
                    for direction in [Direction::Up, Direction::Down] {
                        let Ok(plan) = plan(&source, Scope::Grabbed(grabbed), direction) else {
                            continue;
                        };
                        let moved = apply(&source, &plan);
                        assert_eq!(moved.len(), source.len(), "no line is lost: {plan:?}");
                        assert_eq!(moved[0], source[0], "the title stays put");
                        let mut sorted = moved.clone();
                        sorted.sort();
                        let mut want = source.clone();
                        want.sort();
                        assert_eq!(sorted, want, "the same lines, rearranged: {plan:?}");
                        let block = &source[start..=end];
                        let at = moved
                            .windows(block.len())
                            .position(|w| w == block)
                            .expect("the block is still contiguous");
                        assert!(at >= 1, "and never above the title");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1000, "the sweep actually ran: {checked} steps");
    }

    /// The route out of a list: `Left` to leave the parent, then step. The
    /// line step never refuses, so nothing has to be reached the long way
    /// round; the sibling step is the fast one and may have nowhere to go.
    #[test]
    fn a_line_step_goes_where_the_sibling_step_refuses() {
        let source = lines(&["title", "root", "a", " b", " c", "next"]);
        let grabbed = LineRange::new(2, 2);

        // No sibling below: "a" is at depth 0 and " b" is deeper.
        assert_eq!(
            plan(&source, Scope::Grabbed(grabbed), Direction::Down),
            Err(PlanError::NotSibling)
        );
        // The line step goes anyway, one line at a time, depth untouched.
        let first = plan(&source, Scope::Lines(grabbed), Direction::Down);
        let moved = apply(&source, first.as_ref().unwrap());
        assert_eq!(moved, lines(&["title", "root", " b", "a", " c", "next"]));
        let second = plan(&moved, Scope::Lines(LineRange::new(3, 3)), Direction::Down);
        assert_eq!(
            apply(&moved, second.as_ref().unwrap()),
            lines(&["title", "root", " b", " c", "a", "next"])
        );
        // And it comes straight back.
        assert_eq!(
            plan(&moved, Scope::Lines(LineRange::new(3, 3)), Direction::Up)
                .as_ref()
                .map(|plan| apply(&moved, plan)),
            Ok(source.clone())
        );
    }

    /// A line step is its own inverse in both directions, over every
    /// arrangement of five body lines across three depths, and it never
    /// takes a block above the title.
    #[test]
    fn a_line_step_up_and_down_always_come_home() {
        let mut checked = 0usize;
        for pattern in 0..3usize.pow(5) {
            let mut source = vec!["title".to_string()];
            let mut rest = pattern;
            for k in 0..5 {
                source.push(format!("{}line{k}", " ".repeat(rest % 3)));
                rest /= 3;
            }
            for start in 1..source.len() {
                for end in start..source.len() {
                    let range = LineRange::new(start, end);
                    for direction in [Direction::Up, Direction::Down] {
                        let Ok(step) = plan(&source, Scope::Lines(range), direction) else {
                            continue;
                        };
                        let moved = apply(&source, &step);
                        assert_eq!(moved[0], source[0], "the title stays put");
                        let Plan::Move {
                            destination_start, ..
                        } = step
                        else {
                            panic!("a vertical step moves");
                        };
                        let landed =
                            LineRange::new(destination_start, destination_start + (end - start));
                        let back = match direction {
                            Direction::Up => Direction::Down,
                            _ => Direction::Up,
                        };
                        let home = plan(&moved, Scope::Lines(landed), back);
                        assert!(
                            matches!(&home, Ok(plan) if apply(&moved, plan) == source),
                            "{direction:?} then {back:?} must come home\n  from {source:?}\n  moved {moved:?}\n  range {start}..={end}\n  back {home:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1000, "the sweep actually ran: {checked} pairs");
    }

    #[test]
    fn a_grabbed_block_keeps_its_extent_after_an_outdent() {
        let source = lines(&["title", " parent", " first", "  child", "  second"]);
        assert_eq!(
            subtree(&source, 2),
            Ok(LineRange::new(2, 4)),
            "indentation alone would now hand over three lines"
        );
        // The sibling step has nowhere to go — the lines below are deeper
        // now — and that is fine: `j` steps one line and never refuses.
        assert_eq!(
            plan(
                &source,
                Scope::Grabbed(LineRange::new(2, 3)),
                Direction::Down
            ),
            Err(PlanError::NotSibling)
        );
        assert_eq!(
            plan(
                &source,
                Scope::Grabbed(LineRange::new(2, 3)),
                Direction::Right
            ),
            Ok(Plan::Replace {
                range: LineRange::new(2, 3),
                texts: lines(&["  first", "   child"]),
            })
        );
    }

    #[test]
    fn empty_lines_end_indented_subtrees_naturally() {
        let source = lines(&["title", " root", "  child", "", " peer"]);
        assert_eq!(
            plan(&source, Scope::Block(1), Direction::Right),
            Ok(Plan::Replace {
                range: LineRange::new(1, 2),
                texts: lines(&["  root", "   child"]),
            })
        );
    }
}
