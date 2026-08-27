//! Line selection and review comments for Cosense pages.
//!
//! Adapted from akapen's comment/export model, but tuned for Cosense: a
//! comment anchors to a run of a page's lines and carries both the exact
//! line text and the Cosense line IDs. That makes the export directly
//! actionable by an agent driving the cosense CLI / MCP, whose
//! `edit_lines` / `insert_lines` / `delete_lines` match on *exact line
//! text*, and whose browser deep-link uses the *line ID*
//! (`https://scrapbox.io/{project}/{title}#{lineId}`).

/// A range selection over a page's lines (0-based), anchored where `v` was
/// pressed and extended with j/k.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub anchor: usize,
    pub cursor: usize,
}

impl Selection {
    pub fn new(anchor: usize) -> Self {
        Self { anchor, cursor: anchor }
    }
    /// Inclusive 0-based range, normalized so start <= end.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }
    pub fn contains(&self, line: usize) -> bool {
        let (a, b) = self.range();
        a <= line && line <= b
    }
}

/// A comment anchored to a run of a page's lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    pub project: String,
    pub title: String,
    /// 0-based first/last line index into the page's line list.
    pub start: usize,
    pub end: usize,
    /// The exact text of each anchored line (verbatim, for exact-match edit).
    pub line_texts: Vec<String>,
    /// The Cosense line IDs of each anchored line (for deep links).
    pub line_ids: Vec<String>,
    /// The reviewer's instruction / comment body.
    pub text: String,
}

impl Comment {
    /// 1-based inline card title, e.g. `12-14` or `31`.
    pub fn range_label(&self) -> String {
        if self.start == self.end {
            format!("{}", self.start + 1)
        } else {
            format!("{}-{}", self.start + 1, self.end + 1)
        }
    }

    /// Deep link to the first anchored line (Cosense jumps to `#lineId`).
    pub fn edit_url(&self) -> String {
        let title = urlencode(&self.title);
        match self.line_ids.first() {
            Some(id) if !id.is_empty() => {
                format!("https://scrapbox.io/{}/{}#{}", self.project, title, id)
            }
            _ => format!("https://scrapbox.io/{}/{}", self.project, title),
        }
    }

    pub fn covers(&self, line: usize) -> bool {
        self.start <= line && line <= self.end
    }
}

/// Format one comment as an agent-actionable instruction block:
///
/// ```text
/// Page: acme / Team Tips
/// URL: https://scrapbox.io/acme/TAO%20Tips#6a79...
/// Lines 12-13 (exact text — edit_lines matches verbatim):
///   > 元の行テキスト1
///   > 元の行テキスト2
/// Instruction:
/// この行の誤字を直して
/// ```
///
/// The `> ` snippet is the *exact* line text so an agent can pass it as the
/// `edit_lines` target without re-fetching. The URL deep-links the line.
pub fn format_comment(c: &Comment) -> String {
    let mut out = String::new();
    out.push_str(&format!("Page: {} / {}\n", c.project, c.title));
    out.push_str(&format!("URL: {}\n", c.edit_url()));
    out.push_str(&format!(
        "Lines {} (exact text — edit_lines matches verbatim):\n",
        c.range_label()
    ));
    for t in &c.line_texts {
        out.push_str(&format!("  > {t}\n"));
    }
    out.push_str("Instruction:\n");
    out.push_str(&normalize_text(&c.text));
    out
}

/// Many comments, grouped by page then start line, one blank line between.
pub fn format_all(comments: &[Comment]) -> String {
    let mut sorted: Vec<&Comment> = comments.iter().collect();
    sorted.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then(a.title.cmp(&b.title))
            .then(a.start.cmp(&b.start))
    });
    let numbered = sorted.len() > 1;
    sorted
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if numbered {
                let block = format_comment(c);
                // prefix each block with "N." on its first line
                let mut lines = block.lines();
                let first = lines.next().unwrap_or("");
                let mut b = format!("{}. {first}", i + 1);
                for l in lines {
                    b.push_str(&format!("\n   {l}"));
                }
                b
            } else {
                format_comment(c)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Comment body cleanup: drop \r, trim trailing space, drop blank lines so a
/// multi-line body cannot introduce a block separator.
fn normalize_text(text: &str) -> String {
    text.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Minimal percent-encoding for the title path component.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(start: usize, end: usize, texts: &[&str], ids: &[&str], body: &str) -> Comment {
        Comment {
            project: "acme".into(),
            title: "Team Tips".into(),
            start,
            end,
            line_texts: texts.iter().map(|s| s.to_string()).collect(),
            line_ids: ids.iter().map(|s| s.to_string()).collect(),
            text: body.into(),
        }
    }

    #[test]
    fn selection_normalizes_and_contains() {
        let s = Selection { anchor: 5, cursor: 2 };
        assert_eq!(s.range(), (2, 5));
        assert!(s.contains(3));
        assert!(!s.contains(6));
    }

    #[test]
    fn range_label_is_1_based() {
        assert_eq!(comment(11, 13, &[], &[], "").range_label(), "12-14");
        assert_eq!(comment(30, 30, &[], &[], "").range_label(), "31");
    }

    #[test]
    fn edit_url_deep_links_first_line_id() {
        let c = comment(0, 0, &["x"], &["6a7973f30000000000966115"], "note");
        assert_eq!(
            c.edit_url(),
            "https://scrapbox.io/acme/TAO%20Tips#6a7973f30000000000966115"
        );
        // no id → page url
        let c2 = comment(0, 0, &["x"], &[], "note");
        assert_eq!(c2.edit_url(), "https://scrapbox.io/acme/TAO%20Tips");
    }

    #[test]
    fn format_carries_exact_text_and_url_and_instruction() {
        let c = comment(
            11,
            12,
            &["元の行テキスト1", "元の行テキスト2"],
            &["6a7973f30000000000966115", "6a7973f30000000000966116"],
            "この行の誤字を直して\n\n  ",
        );
        let out = format_comment(&c);
        assert!(out.contains("Page: acme / Team Tips"));
        assert!(out.contains("#6a7973f30000000000966115"));
        assert!(out.contains("Lines 12-13 (exact text"));
        assert!(out.contains("  > 元の行テキスト1"));
        assert!(out.contains("  > 元の行テキスト2"));
        assert!(out.trim_end().ends_with("この行の誤字を直して"));
    }

    #[test]
    fn format_all_numbers_multiple_and_sorts() {
        let a = comment(5, 5, &["b"], &["i2"], "two");
        let b = comment(1, 1, &["a"], &["i1"], "one");
        let out = format_all(&[a, b]);
        // sorted by start → "one" (line 1) first, numbered
        assert!(out.starts_with("1. Page: acme / Team Tips"));
        assert!(out.contains("2. Page: acme / Team Tips"));
        let one_pos = out.find("one").unwrap();
        let two_pos = out.find("two").unwrap();
        assert!(one_pos < two_pos);
    }

    #[test]
    fn single_comment_is_not_numbered() {
        let c = comment(0, 0, &["x"], &["i"], "note");
        let out = format_all(&[c]);
        assert!(out.starts_with("Page:"));
    }
}
