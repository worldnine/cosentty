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

/// Format one comment the way a reply reads in chat (akapen's reply
/// mode): where it is, the quoted lines, a blank line, then the comment.
///
/// ```text
/// acme/Team Tips L12-13 https://scrapbox.io/acme/TAO%20Tips#6a79...
/// > 元の行テキスト1
/// > 元の行テキスト2
///
/// この行の誤字を直して
/// ```
///
/// The quote is the EXACT line text, so an agent driving the cosense CLI
/// / MCP can hand it to `edit_lines` verbatim; the URL deep-links the
/// first line. No `Lines:` / `Instruction:` labels: a quote followed by a
/// remark is how people already write this, and the agent reads it the
/// same way. The blank line is load-bearing — CommonMark's lazy
/// continuation would otherwise pull the comment into the blockquote.
pub fn format_comment(c: &Comment) -> String {
    format_comment_numbered(c, None)
}

/// `number`: the item number when the comment is one of several. It goes
/// in FRONT of the location line (`2. acme/… L5`) and the rest of the
/// block is indented under it, so the batch reads as a numbered list of
/// distinct points. The number sits before the `> ` marker, so quoted
/// content that itself starts with `1. ` cannot collide with it.
fn format_comment_numbered(c: &Comment, number: Option<usize>) -> String {
    let location = format!("{}/{} L{} {}", c.project, c.title, c.range_label(), c.edit_url());
    let mut quote: Vec<String> = c.line_texts.iter().map(|t| format!("> {t}")).collect();
    if quote.is_empty() {
        quote.push("> ".into());
    }
    let text = normalize_text(&c.text);
    match number {
        None => format!("{location}\n{}\n\n{text}", quote.join("\n")),
        Some(n) => {
            let mut out = format!("{n}. {location}");
            for q in &quote {
                out.push_str("\n   ");
                out.push_str(q);
            }
            out.push('\n');
            for l in text.lines() {
                out.push_str("\n    ");
                out.push_str(l);
            }
            out
        }
    }
}

/// Many comments, sorted by page then start line, one blank line between
/// blocks. Two or more are numbered so the receiving agent reads a list of
/// separate points to address in order; one stays a plain remark.
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
        .map(|(i, c)| format_comment_numbered(c, numbered.then_some(i + 1)))
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

    /// 引用とコメントを混ぜた、チャットの返信の形。場所は1行、引用は
    /// `> ` の正確な行テキスト、空行を挟んでコメント(空行が無いと
    /// CommonMark はコメントを引用に飲み込む)。
    #[test]
    fn format_is_location_then_quote_then_blank_then_comment() {
        let c = comment(
            11,
            12,
            &["元の行テキスト1", "元の行テキスト2"],
            &["6a7973f30000000000966115", "6a7973f30000000000966116"],
            "この行の誤字を直して\n\n  ",
        );
        assert_eq!(
            format_comment(&c),
            "acme/Team Tips L12-13 https://scrapbox.io/acme/TAO%20Tips#6a7973f30000000000966115\n\
             > 元の行テキスト1\n\
             > 元の行テキスト2\n\
             \n\
             この行の誤字を直して"
        );
    }

    /// 複数は番号付きの箇条書きになる(順に対応すべき別々の指摘だと
    /// 読める)。番号は場所の行の前、引用とコメントはその項目の中に
    /// インデントされる。並びはページ → 開始行。
    #[test]
    fn format_all_numbers_multiple_and_sorts() {
        let a = comment(5, 5, &["b"], &["i2"], "two");
        let b = comment(1, 1, &["a"], &["i1"], "one\nmore");
        let out = format_all(&[a, b]);
        assert_eq!(
            out,
            "1. acme/Team Tips L2 https://scrapbox.io/acme/TAO%20Tips#i1\n\
             \x20  > a\n\
             \n\
             \x20   one\n\
             \x20   more\n\
             \n\
             2. acme/Team Tips L6 https://scrapbox.io/acme/TAO%20Tips#i2\n\
             \x20  > b\n\
             \n\
             \x20   two"
        );
    }

    #[test]
    fn single_comment_is_not_numbered() {
        let c = comment(0, 0, &["x"], &["i"], "note");
        let out = format_all(&[c]);
        assert!(out.starts_with("acme/Team Tips L1 "));
    }
}
