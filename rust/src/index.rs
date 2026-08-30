//! The project index: every page in a full-width list, with a short excerpt
//! from the one under the cursor docked below it.
//!
//! Modelled on ashiato (the picker akapen is paired with): a list you move
//! through with j/k and a preview that keeps up without ever making the
//! list feel slow. Unlike a side pane, the shallow dock promises an excerpt
//! rather than a second reading view — exactly what the pages API supplies.
//! What is here is the part that can be decided without a terminal or a
//! network: matching, cursor movement, the vertical budget, and row text.
//! The viewer owns drawing, fetching and keys.

use crate::api::PageSummary;

/// `auto` hides the preview below this terminal width. Keep ashiato's
/// established switch even though the excerpt is now below the list: at
/// narrow widths its few source lines wrap too aggressively to help.
pub const PREVIEW_MIN_WIDTH: u16 = 80;

/// The excerpt is deliberately shallow. A large region promises a reading
/// view and makes the pages API's few description lines look incomplete;
/// six rows read as the quick peek they are.
const PREVIEW_ROWS: u16 = 6;
const PREVIEW_MIN_ROWS: u16 = 2;
const LIST_MIN_ROWS: u16 = 3;

/// Excerpt dock behaviour (`--preview`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PreviewMode {
    /// Show the excerpt whenever the screen is tall enough.
    On,
    /// Never show it (the list takes the full height).
    Off,
    /// Show it, but hide it on narrow or very short terminals.
    #[default]
    Auto,
}

impl PreviewMode {
    /// Parse `--preview <on|off|auto>`; `None` for unknown values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "on" => Some(PreviewMode::On),
            "off" => Some(PreviewMode::Off),
            "auto" => Some(PreviewMode::Auto),
            _ => None,
        }
    }

    pub fn shows_preview(self, width: u16) -> bool {
        match self {
            PreviewMode::On => true,
            PreviewMode::Off => false,
            PreviewMode::Auto => width >= PREVIEW_MIN_WIDTH,
        }
    }
}

/// Which pane the keys are talking to. The list is where a picker starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    List,
    Preview,
}

/// Row budget for the index body (the header and footer are outside it).
/// `preview` is `None` when there is no excerpt; the list then takes every
/// row. The gap is blank space, not a border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexLayout {
    pub list: u16,
    pub gap: u16,
    pub preview: Option<u16>,
}

/// Stack a full-width list above a short excerpt.
///
/// Width decides whether `auto` enables the excerpt. Height decides whether
/// both parts can remain useful: below three list rows + a blank separator +
/// two excerpt rows, the list wins and takes the body alone.
pub fn layout(mode: PreviewMode, width: u16, height: u16) -> IndexLayout {
    if !mode.shows_preview(width)
        || height < LIST_MIN_ROWS + 1 + PREVIEW_MIN_ROWS
    {
        return IndexLayout { list: height, gap: 0, preview: None };
    }
    let gap = 1;
    let preview = PREVIEW_ROWS.min(height - gap - LIST_MIN_ROWS);
    IndexLayout { list: height - gap - preview, gap, preview: Some(preview) }
}

/// One page in the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    /// Server-side mtime (epoch seconds).
    pub updated: i64,
    /// The first few lines, as the list API hands them over. This is the
    /// excerpt — it costs nothing and is enough to decide whether to open.
    pub descriptions: Vec<String>,
    /// Has this page changed since it was last seen here?
    pub unread: bool,
}

impl Entry {
    pub fn from_summary(p: PageSummary, seen_at: Option<i64>) -> Self {
        Self {
            unread: seen_at.is_none_or(|seen| p.updated > seen),
            title: p.title,
            updated: p.updated,
            descriptions: p.descriptions,
        }
    }
}

/// What the index is showing, and where the reader is in it.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Every page of the project, newest first (the order the API is asked
    /// for). Filtering never reorders: the list under a filter is the same
    /// list with rows removed.
    pub entries: Vec<Entry>,
    /// What has been typed. Empty = the whole project.
    pub filter: String,
    /// Cursor over the FILTERED rows.
    pub cursor: usize,
    /// First visible row of the list.
    pub scroll: usize,
    pub focus: Pane,
    /// Keep the cursor in view on the next frame. Set by every key that
    /// moves the cursor and cleared by the wheel: scrolling with the mouse
    /// moves the VIEW and leaves the cursor where it is, exactly as it
    /// does over the page body.
    pub follow: bool,
    /// Preview scroll, in rows, kept per page under the cursor.
    pub preview_scroll: u16,
    /// How many pages the project has, when the API said so — the list may
    /// hold fewer (see the viewer's paging).
    pub total: usize,
}

/// A row of the list: a page, or the offer to create what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row<'a> {
    Page(&'a Entry),
    /// Nothing is named this yet; Enter writes it.
    Create(&'a str),
}

impl Index {
    pub fn new(entries: Vec<Entry>, total: usize) -> Self {
        Self { entries, total, focus: Pane::List, follow: true, ..Self::default() }
    }

    /// Rows matching the filter, in list order, with the create offer last
    /// when the typed name is not a page.
    ///
    /// Matching is case-insensitive substring, on the title only. Cosense's
    /// own quick search is cleverer (it scores, and it looks inside pages),
    /// but this list is answering "which of these do I mean", where the
    /// eye is already on the titles.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<Row<'_>> = self
            .entries
            .iter()
            .filter(|e| needle.is_empty() || e.title.to_lowercase().contains(&needle))
            .map(Row::Page)
            .collect();
        if self.offers_create() {
            rows.push(Row::Create(self.filter.trim()));
        }
        rows
    }

    /// Does the list offer to create what was typed? Only when nothing is
    /// named that already — an exact match IS the page, and two pages
    /// cannot share a title.
    pub fn offers_create(&self) -> bool {
        let typed = self.filter.trim();
        !typed.is_empty()
            && !self.entries.iter().any(|e| e.title.eq_ignore_ascii_case(typed))
    }

    /// The entry under the cursor, if the cursor is on a page (rather than
    /// on the create row).
    pub fn selected(&self) -> Option<&Entry> {
        match self.rows().get(self.cursor) {
            Some(Row::Page(e)) => Some(e),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.rows().len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows().is_empty()
    }

    /// Move the cursor by `delta` rows, stopping at the ends. Returns
    /// whether the cursor moved (the viewer only re-previews if it did).
    pub fn move_cursor(&mut self, delta: i32) -> bool {
        let n = self.len();
        if n == 0 {
            return false;
        }
        let want = (self.cursor as i64 + delta as i64).clamp(0, n as i64 - 1) as usize;
        let moved = want != self.cursor;
        self.cursor = want;
        self.follow = true;
        if moved {
            self.preview_scroll = 0;
        }
        moved
    }

    /// Typing (or deleting) narrows the list under the cursor, so the
    /// cursor goes back to the top: the first match is what a filter is
    /// for.
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.cursor = 0;
        self.scroll = 0;
        self.preview_scroll = 0;
        self.follow = true;
    }

    /// Wheel: move the WINDOW by `delta` rows and leave the cursor alone.
    /// The list is content, and content scrolls under the pointer — the
    /// selection is the keyboard's business (the page body behaves the
    /// same way). Returns whether anything moved, which is what decides
    /// if the scrollbar shows itself.
    pub fn scroll_by(&mut self, delta: i32, height: usize) -> bool {
        let n = self.len();
        let max = n.saturating_sub(height.max(1));
        let want = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
        let moved = want != self.scroll;
        self.scroll = want;
        // The cursor stays put, so the window must stop being dragged back
        // onto it until a key asks for that again.
        self.follow = false;
        moved
    }

    /// Keep the cursor inside the list and inside the window of `height`
    /// rows, returning the scroll offset to draw at. A cursor that walked
    /// off the bottom pulls the window down by exactly what it needs.
    pub fn follow(&mut self, height: usize) -> usize {
        let n = self.len();
        if n == 0 || height == 0 {
            self.scroll = 0;
            return 0;
        }
        self.cursor = self.cursor.min(n - 1);
        if !self.follow {
            // Scrolled away with the wheel: only keep the window inside
            // the list.
            self.scroll = self.scroll.min(n.saturating_sub(height));
            return self.scroll;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        // A list that shrank under the window (filtering) must not leave
        // the window hanging past its end.
        self.scroll = self.scroll.min(n.saturating_sub(height));
        self.scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, updated: i64) -> Entry {
        Entry {
            title: title.into(),
            updated,
            descriptions: vec![format!("{title} の本文")],
            unread: false,
        }
    }

    fn index(titles: &[&str]) -> Index {
        Index::new(
            titles.iter().enumerate().map(|(i, t)| entry(t, 100 - i as i64)).collect(),
            titles.len(),
        )
    }

    #[test]
    fn the_list_keeps_its_order_under_a_filter() {
        let mut ix = index(&["改善案", "画像表示テスト", "テスト", "kaizen"]);
        assert_eq!(ix.len(), 4);
        ix.set_filter("テスト".into());
        let titles: Vec<&str> = ix
            .rows()
            .iter()
            .filter_map(|r| match r {
                Row::Page(e) => Some(e.title.as_str()),
                Row::Create(_) => None,
            })
            .collect();
        assert_eq!(titles, vec!["画像表示テスト", "テスト"], "order is the list's, not the match's");
        // Filtering puts the cursor on the first match: that is what
        // typing is for.
        assert_eq!(ix.cursor, 0);
        assert_eq!(ix.selected().map(|e| e.title.as_str()), Some("画像表示テスト"));
    }

    #[test]
    fn a_name_nothing_is_called_yet_is_offered_as_a_new_page() {
        let mut ix = index(&["改善案"]);
        ix.set_filter("改善案".into());
        assert!(!ix.offers_create(), "an exact match is the page itself");
        assert_eq!(ix.len(), 1);

        ix.set_filter("改善案2".into());
        assert!(ix.offers_create());
        assert_eq!(ix.rows().last(), Some(&Row::Create("改善案2")));
        // The create row is not a page: the preview has nothing to show.
        ix.cursor = ix.len() - 1;
        assert_eq!(ix.selected(), None);

        ix.set_filter("   ".into());
        assert!(!ix.offers_create(), "whitespace is not a page name");
    }

    #[test]
    fn the_cursor_stays_inside_the_list_and_the_window() {
        let mut ix = index(&["a", "b", "c", "d", "e", "f"]);
        assert_eq!(ix.follow(3), 0);
        assert!(ix.move_cursor(2));
        assert_eq!(ix.follow(3), 0, "still visible: the window does not move");
        assert!(ix.move_cursor(1));
        assert_eq!(ix.follow(3), 1, "walked off the bottom: the window follows by one");
        assert!(ix.move_cursor(99));
        assert_eq!(ix.cursor, 5, "and stops at the end");
        assert_eq!(ix.follow(3), 3);
        assert!(!ix.move_cursor(9), "already there: nothing moved");

        // A filter that shrinks the list to less than a window pulls the
        // scroll back rather than leaving the window past the end.
        ix.set_filter("a".into());
        assert_eq!(ix.follow(3), 0);
        assert_eq!(ix.cursor, 0);
    }

    #[test]
    fn the_wheel_moves_the_window_and_the_keys_move_the_cursor() {
        let mut ix = index(&["a", "b", "c", "d", "e", "f"]);
        assert_eq!(ix.follow(3), 0);
        // The wheel scrolls the list under the cursor: the cursor does not
        // move, and the window is not dragged back to it.
        assert!(ix.scroll_by(2, 3));
        assert_eq!(ix.cursor, 0, "the selection is the keyboard's business");
        assert_eq!(ix.follow(3), 2, "…and the window stays where it was put");
        assert!(ix.scroll_by(9, 3), "…up to the last window");
        assert_eq!(ix.scroll, 3, "which stops with the last row in view");
        assert!(!ix.scroll_by(9, 3), "and there is nothing past it");
        // A key that moves the cursor takes the window back with it.
        ix.move_cursor(1);
        assert_eq!(ix.cursor, 1);
        assert_eq!(ix.follow(3), 1, "the window follows the cursor again");
    }

    #[test]
    fn moving_the_cursor_starts_the_new_preview_at_its_top() {
        let mut ix = index(&["a", "b"]);
        ix.preview_scroll = 12;
        assert!(ix.move_cursor(1));
        assert_eq!(ix.preview_scroll, 0, "a different page is read from its start");
        ix.preview_scroll = 5;
        assert!(!ix.move_cursor(1), "at the end");
        assert_eq!(ix.preview_scroll, 5, "…so the reader keeps their place");
    }

    #[test]
    fn the_preview_is_a_shallow_dock_when_width_and_height_allow_it() {
        let auto = PreviewMode::Auto;
        assert_eq!(
            layout(auto, 79, 20),
            IndexLayout { list: 20, gap: 0, preview: None },
            "auto keeps the established 80-column threshold"
        );
        let normal = layout(auto, 80, 22);
        assert_eq!(normal, IndexLayout { list: 15, gap: 1, preview: Some(6) });
        assert_eq!(normal.list + normal.gap + normal.preview.unwrap(), 22);

        // A short screen shrinks the excerpt before sacrificing the list.
        assert_eq!(
            layout(auto, 80, 7),
            IndexLayout { list: 3, gap: 1, preview: Some(3) }
        );
        assert_eq!(
            layout(auto, 80, 5),
            IndexLayout { list: 5, gap: 0, preview: None },
            "if both cannot be useful, the list wins"
        );

        // off is never; on bypasses the width threshold but not impossible
        // geometry.
        assert_eq!(
            layout(PreviewMode::Off, 200, 20),
            IndexLayout { list: 20, gap: 0, preview: None }
        );
        assert_eq!(
            layout(PreviewMode::On, 40, 20),
            IndexLayout { list: 13, gap: 1, preview: Some(6) }
        );
        assert_eq!(PreviewMode::parse("on"), Some(PreviewMode::On));
        assert_eq!(PreviewMode::parse("sometimes"), None);
        assert_eq!(PreviewMode::default(), PreviewMode::Auto);
    }

    #[test]
    fn a_page_is_unread_until_it_has_been_seen_since_its_last_edit() {
        let p = |updated| PageSummary {
            id: "id".into(),
            title: "t".into(),
            image: None,
            descriptions: vec![],
            updated,
            linked: 0,
        };
        assert!(Entry::from_summary(p(100), None).unread, "never opened");
        assert!(Entry::from_summary(p(100), Some(99)).unread, "edited since the visit");
        assert!(!Entry::from_summary(p(100), Some(100)).unread);
        assert!(!Entry::from_summary(p(100), Some(101)).unread);
    }
}
