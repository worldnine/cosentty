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

/// Plan one outline action against source lines.
pub fn plan(lines: &[String], scope: Scope, direction: Direction) -> Result<Plan, PlanError> {
    let range = match scope {
        Scope::Lines(range) => checked_range(lines, range)?,
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
            let mut sibling = root - 1;
            while sibling > 0 && depth(&lines[sibling]) > root_depth {
                sibling -= 1;
            }
            if sibling == 0
                || depth(&lines[sibling]) != root_depth
                || parent(lines, sibling) != root_parent
            {
                return Err(PlanError::NotSibling);
            }
            Ok(Plan::Move {
                range,
                destination: Destination::Before(sibling),
                destination_start: sibling,
            })
        }
        Direction::Down => {
            let sibling = range.end + 1;
            if sibling >= lines.len() {
                return Err(PlanError::Boundary);
            }
            if depth(&lines[sibling]) != root_depth || parent(lines, sibling) != root_parent {
                return Err(PlanError::NotSibling);
            }
            let sibling_range = subtree(lines, sibling)?;
            Ok(Plan::Move {
                range,
                destination: if sibling_range.end + 1 < lines.len() {
                    Destination::Before(sibling_range.end + 1)
                } else {
                    Destination::End
                },
                destination_start: range.start + sibling_range.len(),
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
