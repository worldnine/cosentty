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
        Self {
            anchor,
            cursor: anchor,
        }
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

/// The page revision a comment was written on, when it was not NOW: a
/// Page History snapshot. A comment is shown only on the revision it was
/// written on (akapen's model — the lines it quotes are THAT version's),
/// and the export names the snapshot so an agent can read exactly what
/// the reviewer saw (`cosense readPageSnapshot`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Revision {
    /// The snapshot's id (`listPageSnapshots` → `timestamps[].id`).
    pub snapshot_id: String,
    /// The snapshot's time, epoch seconds.
    pub created: i64,
}

/// A comment anchored to a run of a page's lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    pub project: String,
    pub title: String,
    /// The page's immutable id (`readPage` → `id`), which the snapshot
    /// command needs. Empty for a page that does not exist yet.
    pub page_id: String,
    /// `None` = written on NOW (the live page).
    pub revision: Option<Revision>,
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

    /// Deep link to the first anchored line (Cosense jumps to `#lineId`,
    /// and the cosense skill takes that fragment as the anchor for the
    /// edit — its top priority, ahead of anything the body suggests).
    pub fn edit_url(&self) -> String {
        let title = encode_title_for_url(&self.title);
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

    /// The snapshot this comment belongs to, `None` for NOW. What the
    /// viewer compares against the revision it is showing.
    pub fn snapshot_id(&self) -> Option<&str> {
        self.revision.as_ref().map(|r| r.snapshot_id.as_str())
    }

    /// The revision's time as shown on cards and in the list, `None` for
    /// NOW.
    pub fn revision_label(&self) -> Option<String> {
        self.revision
            .as_ref()
            .map(|r| crate::theme::format_local(r.created))
    }
}

/// Format one comment the way a reply reads in chat (akapen's reply
/// mode): where it is, the quoted lines, a blank line, then the comment.
///
/// ```text
/// https://scrapbox.io/acme/Team_Tips#6a79...115 L12-13
/// > 元の行テキスト1  # 6a79...115
/// > 元の行テキスト2  # 6a79...116
///
/// この行の誤字を直して
/// ```
///
/// Written in the cosense skill's own vocabulary, so the agent receiving
/// it can act without translation:
/// - the URL comes first and is whitespace-free (the skill reads a URL
///   from `https://` to the next space), in Cosense's readable form (`_`
///   for spaces, raw Japanese), with the first line's `#lineId` — the
///   fragment the skill treats as the edit's anchor;
/// - every quoted line ends in `# <lineId>`, the marker `previewEdit`
///   prints on changed lines, which is what an `insertBefore` / `replace`
///   / `delete` op takes as its anchor;
/// - the quote is the EXACT text, so a human reads what was meant and the
///   agent can check it against `readPage`;
/// - a comment written on a past revision adds a `Snapshot:` line naming
///   the snapshot and the command that reads it
///   (`cosense readPageSnapshot <projectUrl> <pageId> <snapshotId>`), so
///   "put this back the way it was in this version" is one command away
///   from the text the reviewer saw. Snapshot lines carry the same ids as
///   NOW, so the `# <lineId>` anchors still address the live page.
/// No `Lines:` / `Instruction:` labels: a quote followed by a remark is
/// how people already write this. The blank line is load-bearing —
/// CommonMark's lazy continuation would otherwise pull the comment into
/// the blockquote.
pub fn format_comment(c: &Comment) -> String {
    format_comment_numbered(c, None)
}

/// `number`: the item number when the comment is one of several. It goes
/// in FRONT of the location line (`2. acme/… L5`) and the rest of the
/// block is indented under it, so the batch reads as a numbered list of
/// distinct points. The number sits before the `> ` marker, so quoted
/// content that itself starts with `1. ` cannot collide with it.
fn format_comment_numbered(c: &Comment, number: Option<usize>) -> String {
    let mut location = format!("{} L{}", c.edit_url(), c.range_label());
    if let Some(r) = &c.revision {
        location.push_str(&format!(
            "\nSnapshot: {} ({}) — cosense readPageSnapshot https://scrapbox.io/{} {} {}",
            r.snapshot_id,
            crate::theme::format_local(r.created),
            c.project,
            c.page_id,
            r.snapshot_id
        ));
    }
    let mut quote: Vec<String> = c
        .line_texts
        .iter()
        .enumerate()
        .map(
            |(i, t)| match c.line_ids.get(i).filter(|id| !id.is_empty()) {
                Some(id) => format!("> {t}  # {id}"),
                None => format!("> {t}"),
            },
        )
        .collect();
    if quote.is_empty() {
        quote.push("> ".into());
    }
    let text = normalize_text(&c.text);
    match number {
        None => format!("{location}\n{}\n\n{text}", quote.join("\n")),
        Some(n) => {
            let mut out = format!("{n}. {}", location.replace('\n', "\n   "));
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

/// A page title as Cosense's "Copy readable link" writes it, and as the
/// cosense CLI's `encodeTitleForUrl` does: Unicode stays raw (browsers
/// pass it through as an IRI), spaces become `_` (Cosense's convention),
/// and only what would break the URL or the server's `/:project/:title`
/// route is percent-encoded (`%` `/` `?` `#`). An agent reads the page
/// name off the URL; a human does too.
pub fn encode_title_for_url(title: &str) -> String {
    title
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(start: usize, end: usize, texts: &[&str], ids: &[&str], body: &str) -> Comment {
        Comment {
            project: "acme".into(),
            title: "Team Tips".into(),
            page_id: "5803c53900000000000000a1".into(),
            revision: None,
            start,
            end,
            line_texts: texts.iter().map(|s| s.to_string()).collect(),
            line_ids: ids.iter().map(|s| s.to_string()).collect(),
            text: body.into(),
        }
    }

    #[test]
    fn selection_normalizes_and_contains() {
        let s = Selection {
            anchor: 5,
            cursor: 2,
        };
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
            "https://scrapbox.io/acme/Team_Tips#6a7973f30000000000966115"
        );
        // no id → page url
        let c2 = comment(0, 0, &["x"], &[], "note");
        assert_eq!(c2.edit_url(), "https://scrapbox.io/acme/Team_Tips");
    }

    /// タイトルは cosense CLI の encodeTitleForUrl と同じ形: 日本語は生、
    /// 空白は `_`、URL やルートを壊す `% / ? #` だけをエスケープ。
    #[test]
    fn titles_are_written_as_cosense_readable_links() {
        assert_eq!(encode_title_for_url("Team Tips"), "Team_Tips");
        assert_eq!(
            encode_title_for_url("改善案 #4 / 50%?"),
            "改善案_%234_%2F_50%25%3F"
        );
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
            "https://scrapbox.io/acme/Team_Tips#6a7973f30000000000966115 L12-13\n\
             > 元の行テキスト1  # 6a7973f30000000000966115\n\
             > 元の行テキスト2  # 6a7973f30000000000966116\n\
             \n\
             この行の誤字を直して"
        );
        // A line the API gave no id (an uncreated page) is quoted bare.
        let bare = comment(0, 0, &["x"], &[], "n");
        assert!(
            format_comment(&bare).starts_with("https://scrapbox.io/acme/Team_Tips L1\n> x\n\nn")
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
            "1. https://scrapbox.io/acme/Team_Tips#i1 L2\n\
             \x20  > a  # i1\n\
             \n\
             \x20   one\n\
             \x20   more\n\
             \n\
             2. https://scrapbox.io/acme/Team_Tips#i2 L6\n\
             \x20  > b  # i2\n\
             \n\
             \x20   two"
        );
    }

    /// 過去版に書いたコメントは、場所の次に Snapshot 行を持つ: skill が
    /// その版を読むコマンドを丸ごと(projectUrl · pageId · snapshotId)。
    #[test]
    fn a_comment_on_a_past_revision_names_the_snapshot_and_how_to_read_it() {
        let mut c = comment(0, 0, &["古い行"], &["i1"], "この版に戻して");
        c.revision = Some(Revision {
            snapshot_id: "6a98c8230000000000fed958".into(),
            created: 0,
        });
        let out = format_comment(&c);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("https://scrapbox.io/acme/Team_Tips#i1 L1"),
            "{out}"
        );
        assert!(
            lines[1].starts_with("Snapshot: 6a98c8230000000000fed958 ("),
            "{out}"
        );
        assert!(lines[1].ends_with(") — cosense readPageSnapshot https://scrapbox.io/acme 5803c53900000000000000a1 6a98c8230000000000fed958"), "{out}");
        assert_eq!(lines[2], "> 古い行  # i1");
        // 番号付きでは Snapshot 行も項目の中にインデントされる。
        let all = format_all(&[c.clone(), comment(3, 3, &["x"], &["i9"], "n")]);
        assert!(all.contains("\n   Snapshot: 6a98c823"), "{all}");
    }

    #[test]
    fn single_comment_is_not_numbered() {
        let c = comment(0, 0, &["x"], &["i"], "note");
        let out = format_all(&[c]);
        assert!(out.starts_with("https://scrapbox.io/acme/Team_Tips"));
    }
}
