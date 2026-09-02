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

use crate::api::{PageSummary, SearchResult};

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

/// What the open line is asking for.
///
/// Two genuinely different questions, so two modes rather than one clever
/// box: the title filter answers "which of these do I mean" over the list
/// already on screen and costs nothing, while the full-text search asks
/// the server about every page's body and takes a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// Narrow the titles already listed (local, live).
    #[default]
    Title,
    /// Search page bodies (`search/query`); runs on Enter.
    FullText,
}

impl FilterMode {
    pub fn toggled(self) -> Self {
        match self {
            FilterMode::Title => FilterMode::FullText,
            FilterMode::FullText => FilterMode::Title,
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

/// How the list is ordered. All six are the page API's own `sort` values
/// (verified against the server), so the order is the project's, not a
/// re-shuffle of whatever page of it happened to be fetched.
///
/// Their names stay in English on screen. They are the API's vocabulary
/// and the same words the site's own sort menu uses — names, not prose
/// (see the note at the top of `view/main.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Most recently edited first — the site top's own default.
    #[default]
    Updated,
    /// Most recently opened (by anyone in the project) first.
    Accessed,
    /// Newest page first.
    Created,
    /// Most linked-to first: the project's hubs.
    Linked,
    /// Most read first.
    Views,
    /// A→Z. The only ASCENDING order here.
    Title,
}

impl SortKey {
    /// Menu order, which is also the site's.
    pub const ALL: [SortKey; 6] = [
        SortKey::Updated,
        SortKey::Accessed,
        SortKey::Created,
        SortKey::Linked,
        SortKey::Views,
        SortKey::Title,
    ];

    /// The `sort=` value the pages API knows this by, and the label shown
    /// for it — deliberately the same string.
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Updated => "updated",
            SortKey::Accessed => "accessed",
            SortKey::Created => "created",
            SortKey::Linked => "linked",
            SortKey::Views => "views",
            SortKey::Title => "title",
        }
    }

    /// What the row's narrow right-of-telomere column says under this
    /// order. Sorting by a number the reader cannot see is no help, so the
    /// column shows the value being sorted ON.
    pub fn column(self, e: &Entry) -> String {
        match self {
            SortKey::Linked => compact_count(e.linked),
            SortKey::Views => compact_count(e.views),
            _ => relative_age(self.stamp(e)),
        }
    }

    /// The timestamp this order reads. `updated` for the orders that are
    /// not about a time at all: the age column then means what it means
    /// everywhere else in the viewer.
    pub fn stamp(self, e: &Entry) -> i64 {
        match self {
            SortKey::Accessed => e.accessed,
            SortKey::Created => e.created,
            _ => e.updated,
        }
    }
}

/// `1`, `86`, `1.2k`, `13k` — a count in at most four columns, which is
/// what the row has for it.
pub fn compact_count(n: i64) -> String {
    match n {
        ..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{}M", n / 1_000_000),
    }
}

/// "3m" / "2h" / "5d" style age from an epoch-seconds timestamp. A zero
/// stamp (a field the API did not fill) has no age to report.
pub fn relative_age(stamp: i64) -> String {
    if stamp <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let secs = (now - stamp).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 86_400 * 365 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}y", secs / (86_400 * 365))
    }
}

/// One page in the index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    /// Server-side mtime (epoch seconds).
    pub updated: i64,
    /// When the page was created, and when anyone last opened it. Both are
    /// sort keys, and both are what the row's age column shows under them.
    pub created: i64,
    pub accessed: i64,
    /// How many pages link here, and how often it has been read.
    pub linked: i64,
    pub views: i64,
    /// The first few lines, as the list API hands them over. This is the
    /// excerpt — it costs nothing and is enough to decide whether to open.
    pub descriptions: Vec<String>,
    /// Has this page changed since it was last seen here?
    pub unread: bool,
    /// The words a full-text search matched ON THIS PAGE, as the endpoint
    /// reported them. Empty for a row that came from the page list. They
    /// are what gets marked in the row and the excerpt, so the reader can
    /// see WHY this page is in the answer.
    pub matched: Vec<String>,
}

impl Entry {
    /// One full-text hit as a list row. The excerpt is the matched LINES,
    /// which is the part that answered the question — better than the
    /// page's opening lines, and the only excerpt a search reply carries.
    ///
    /// `accessed` stays zero: the search endpoint does not report it (the
    /// list endpoint does). `SortKey::Accessed` therefore has nothing to
    /// order search results by, which is one reason a result set keeps the
    /// server's relevance order instead.
    pub fn from_search(r: SearchResult, seen_at: Option<i64>) -> Self {
        Self {
            unread: seen_at.is_none_or(|seen| r.updated > seen),
            title: r.title,
            updated: r.updated,
            created: r.created,
            accessed: 0,
            linked: r.linked,
            views: r.views,
            descriptions: r.lines,
            matched: r.words,
        }
    }

    pub fn from_summary(p: PageSummary, seen_at: Option<i64>) -> Self {
        Self {
            unread: seen_at.is_none_or(|seen| p.updated > seen),
            title: p.title,
            updated: p.updated,
            created: p.created,
            accessed: p.accessed,
            linked: p.linked,
            views: p.views,
            descriptions: p.descriptions,
            matched: Vec::new(), // a listed page matched nothing: it just IS
        }
    }
}

/// What the index is showing, and where the reader is in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    /// Every page of the project, newest first (the order the API is asked
    /// for). Filtering never reorders: the list under a filter is the same
    /// list with rows removed.
    pub entries: Vec<Entry>,
    /// What has been typed. Empty = the whole project.
    pub filter: String,
    /// What the open line searches. `Tab` swaps them.
    pub filter_mode: FilterMode,
    /// May this credential write in the listed project? `false` hides the
    /// `＋ create` row: offering to write a page into a project you cannot
    /// write to is an offer that can only end in an error.
    ///
    /// Defaults to `false` on purpose — a list that has not been told is
    /// not entitled to promise. `open_index` sets it from the project's own
    /// answer (`Ctx::can_edit_in`).
    pub can_create: bool,
    /// The hit list was cut off by the search endpoint's limit, so its
    /// count is a floor. Only meaningful while `search` is set.
    pub search_capped: bool,
    /// The full-text query these entries ARE the results of, when they are.
    ///
    /// A search result set is a different list from the project's: it is
    /// ordered by the server's relevance, its excerpts are the matched
    /// lines rather than each page's opening, and `sort` has nothing to say
    /// about it. `^u` puts the project's own list back.
    pub search: Option<String>,
    /// Is the filter line open for typing? (`/` opens it, ashiato's key.)
    ///
    /// The index used to narrow on every printable key with no mode at
    /// all. Two things were wrong with that. Cosense titles are Japanese,
    /// and an IME's preedit reaches the list as the ASCII keys being
    /// composed — so the list thrashed while a word was still being
    /// written. And it spent every printable key: `q` could not quit
    /// because `q` was filter text.
    pub filter_editing: bool,
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
    /// How the list is ordered. Kept here (not only on the App) so that
    /// walking back to an index through the history restores the order it
    /// was left in, along with the filter and the cursor.
    pub sort: SortKey,
}

/// A row of the list: a page, or the offer to create what was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row<'a> {
    Page(&'a Entry),
    /// Nothing is named this yet; Enter writes it.
    Create(&'a str),
}

impl Index {
    pub fn new(entries: Vec<Entry>, total: usize, sort: SortKey) -> Self {
        Self { entries, total, sort, focus: Pane::List, follow: true, ..Self::default() }
    }

    /// Put `entries` in `sort` order.
    ///
    /// The API already answered in this order, but it floats PINNED pages
    /// to the top of every one of them (measured on villagepump: its three
    /// pinned pages head `sort=title` and `sort=linked` alike). A pinned
    /// page is still just a page in this list, so the order is re-imposed
    /// here — on the same key the server was asked for, never on a
    /// different one.
    pub fn sort_entries(entries: &mut [Entry], sort: SortKey) {
        match sort {
            // The only ascending order, and the only one that is not a
            // number. Case-insensitive, as the site's own A→Z is.
            SortKey::Title => entries.sort_by(|a, b| {
                a.title.to_lowercase().cmp(&b.title.to_lowercase())
            }),
            SortKey::Linked => entries.sort_by(|a, b| b.linked.cmp(&a.linked)),
            SortKey::Views => entries.sort_by(|a, b| b.views.cmp(&a.views)),
            _ => entries.sort_by(|a, b| sort.stamp(b).cmp(&sort.stamp(a))),
        }
    }

    /// Rows matching the filter, in list order, with the create offer last
    /// when the typed name is not a page.
    ///
    /// Matching is case-insensitive substring, on the title only. Cosense's
    /// own quick search is cleverer (it scores, and it looks inside pages),
    /// but this list is answering "which of these do I mean", where the
    /// eye is already on the titles.
    pub fn rows(&self) -> Vec<Row<'_>> {
        // A full-text query is not a local narrowing: the list stays as it
        // is until the server answers (Enter). Narrowing it by the typed
        // word as well would hide the very pages the search is about to
        // report, since the word is in their BODIES, not their titles.
        if self.filter_editing && self.filter_mode == FilterMode::FullText {
            return self.entries.iter().map(Row::Page).collect();
        }
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
    /// cannot share a title — and only where a page could be written.
    pub fn offers_create(&self) -> bool {
        if !self.can_create {
            return false; // a read-only project has nothing to offer here
        }
        if self.filter_editing && self.filter_mode == FilterMode::FullText {
            return false; // that word is a query, not a page name
        }
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

    /// `/`: open the filter line for typing.
    pub fn begin_filter(&mut self) {
        self.filter_editing = true;
    }

    /// Enter: keep what was typed and hand the keys back to the list, so
    /// j/k and Enter mean the list again while the filter still holds.
    pub fn commit_filter(&mut self) {
        self.filter_editing = false;
    }

    /// Esc: drop the filter and close the line — ashiato's 解除. Esc on a
    /// narrowed list means "show me everything again", which is why it is
    /// not also the key that leaves the index while the line is open.
    pub fn cancel_filter(&mut self) {
        self.filter_editing = false;
        self.set_filter(String::new());
    }

    /// `Tab`: swap what the open line is asking for. The typed word is
    /// kept — "I meant the body, not the title" is the whole point, and
    /// retyping it would be the cost of saying so.
    pub fn toggle_filter_mode(&mut self) {
        self.filter_mode = self.filter_mode.toggled();
        self.cursor = 0;
        self.scroll = 0;
        self.preview_scroll = 0;
        self.follow = true;
    }

    /// Is the list on screen a set of full-text hits?
    pub fn is_search(&self) -> bool {
        self.search.is_some()
    }

    /// What to mark in `e`'s row and excerpt — the reason it is on screen.
    ///
    /// A hit is marked with the words the search actually matched on that
    /// page; a narrowed list with what was typed. A full-text query still
    /// being typed marks nothing: the list is not narrowed by it yet, so
    /// there is no reason to point at.
    pub fn terms_for<'a>(&'a self, e: &'a Entry) -> Vec<&'a str> {
        if !e.matched.is_empty() {
            return e.matched.iter().map(String::as_str).collect();
        }
        if self.filter_editing && self.filter_mode == FilterMode::FullText {
            return Vec::new();
        }
        let typed = self.filter.trim();
        if typed.is_empty() { Vec::new() } else { vec![typed] }
    }


    pub fn push_filter(&mut self, c: char) {
        let mut f = self.filter.clone();
        f.push(c);
        self.set_filter(f);
    }

    pub fn pop_filter(&mut self) {
        let mut f = self.filter.clone();
        f.pop();
        self.set_filter(f);
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


/// Byte ranges of `terms` inside `text`, merged and in order.
///
/// Case-insensitive, by the SAME rule `rows()` filters with
/// (`to_lowercase().contains`), so a mark can never disagree with the
/// reason its row is listed.
///
/// The catch: lowercasing can change a string's LENGTH (`İ` becomes two
/// chars, `K` becomes `k`), and then offsets found in the lowered text do
/// not address the original. When that happens the search falls back to a
/// case-sensitive one rather than marking the wrong bytes. ASCII and every
/// CJK character keep their length, so the fallback is for the corner it
/// exists to protect.
pub fn match_ranges(text: &str, terms: &[&str]) -> Vec<(usize, usize)> {
    let lower = text.to_lowercase();
    let aligned = lower.len() == text.len();
    let hay: &str = if aligned { &lower } else { text };
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let needle = if aligned { term.to_lowercase() } else { (*term).to_string() };
        if needle.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(at) = hay[from..].find(&needle) {
            let start = from + at;
            let end = start + needle.len();
            spans.push((start, end));
            from = end;
        }
    }
    spans.sort();
    // Two terms can overlap ("案" inside "改善案"), and a mark is a mark:
    // one run of marked cells, not two fighting over the same bytes.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some((_, prev_end)) if a <= *prev_end => *prev_end = (*prev_end).max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Split `text` into `(piece, marked)` runs on `terms`. Pieces are never
/// empty, and concatenating them gives `text` back.
pub fn split_on_terms<'a>(text: &'a str, terms: &[&str]) -> Vec<(&'a str, bool)> {
    let ranges = match_ranges(text, terms);
    if ranges.is_empty() {
        return if text.is_empty() { Vec::new() } else { vec![(text, false)] };
    }
    let mut out: Vec<(&str, bool)> = Vec::new();
    let mut at = 0;
    for (a, b) in ranges {
        if a > at {
            out.push((&text[at..a], false));
        }
        out.push((&text[a..b], true));
        at = b;
    }
    if at < text.len() {
        out.push((&text[at..], false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, updated: i64) -> Entry {
        Entry {
            title: title.into(),
            updated,
            descriptions: vec![format!("{title} の本文")],
            ..Entry::default()
        }
    }

    fn index(titles: &[&str]) -> Index {
        let mut ix = Index::new(
            titles.iter().enumerate().map(|(i, t)| entry(t, 100 - i as i64)).collect(),
            titles.len(),
            SortKey::Updated,
        );
        ix.can_create = true; // a project this session may write in
        ix
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

    /// Every order is re-imposed locally, because the API floats PINNED
    /// pages to the top of all of them (measured on villagepump: its three
    /// pinned pages head `sort=title` and `sort=linked` alike). Re-imposing
    /// it on the WRONG key is the trap: the list used to sort by `updated`
    /// unconditionally, which would have shuffled an A→Z list back into
    /// date order.
    /// While a full-text query is being typed, the list must NOT be
    /// narrowed by it as well. The word is in those pages' BODIES — the one
    /// place a title filter cannot see — so narrowing on the titles would
    /// hide exactly the pages the search is about to report, and offer to
    /// CREATE the word as a page on top of that.
    /// A project this session may only read never offers to create: the
    /// row would be a promise the API is going to refuse.
    /// 一致の印は「なぜこの行がここにあるか」を指すもの。だから
    /// `rows()` が絞り込むのと同じ規則(小文字化して contains)で探す。
    #[test]
    fn matches_are_found_by_the_same_rule_the_filter_narrows_with() {
        let r = |t: &str, terms: &[&str]| match_ranges(t, terms);
        // ASCII は大文字小文字を無視する。
        assert_eq!(r("Kaizen an", &["kaizen"]), vec![(0, 6)]);
        // 日本語。バイト範囲で返る。
        assert_eq!(r("改善案", &["改善"]), vec![(0, 6)]);
        assert_eq!(r("改善案", &["案"]), vec![(6, 9)]);
        // 同じ語が何度も出れば、そのすべて。隣り合うものは1つの run に
        // なる——画面の上では隣接した印は1本の帯にしか見えない。
        assert_eq!(r("abab", &["ab"]), vec![(0, 4)]);
        assert_eq!(r("ab-ab", &["ab"]), vec![(0, 2), (3, 5)]);
        // 重なる2語は1つの run にまとめる——印は印であって、
        // 同じバイトを2つが取り合うものではない。
        assert_eq!(r("改善案", &["改善", "善案"]), vec![(0, 9)]);
        assert!(r("改善案", &["無い"]).is_empty());
        assert!(r("改善案", &[]).is_empty());

        // 分割した破片をつなぐと元に戻る。
        let pieces = split_on_terms("改善案4", &["善"]);
        assert_eq!(pieces, vec![("改", false), ("善", true), ("案4", false)]);
        let joined: String = pieces.iter().map(|(p, _)| *p).collect();
        assert_eq!(joined, "改善案4");
        assert_eq!(split_on_terms("", &["x"]), Vec::new());
        assert_eq!(split_on_terms("abc", &[]), vec![("abc", false)]);
    }

    /// 何を指すかは行の出どころで決まる。ヒットはその**ページで**一致した
    /// 語、絞り込みは打った語、打っている途中の本文検索は何も指さない
    /// (まだ絞り込まれていないので、指す理由がない)。
    #[test]
    fn what_gets_marked_is_the_reason_the_row_is_listed() {
        let mut ix = index(&["改善案", "テスト"]);
        assert!(ix.terms_for(&ix.entries[0]).is_empty(), "素の一覧は何も指さない");

        ix.set_filter("改善".into());
        assert_eq!(ix.terms_for(&ix.entries[0]), vec!["改善"]);

        ix.begin_filter();
        ix.toggle_filter_mode();
        assert!(
            ix.terms_for(&ix.entries[0]).is_empty(),
            "本文検索を打っている間は一覧を絞っていないので、指す理由がない"
        );

        // ヒットは自分が一致した語を持ってくる。
        let hit = Entry { matched: vec!["画像".into()], ..ix.entries[0].clone() };
        assert_eq!(ix.terms_for(&hit), vec!["画像"], "ヒットは絞り込みの状態に関わらず自前");
    }

    #[test]
    fn a_read_only_project_never_offers_to_create() {
        let mut ix = index(&["改善案", "テスト"]);
        ix.set_filter("まだ無いページ".into());
        assert!(ix.offers_create(), "writable: the offer is the point of typing");
        assert_eq!(ix.len(), 1);

        ix.can_create = false;
        assert!(!ix.offers_create());
        assert_eq!(ix.len(), 0, "nothing matches, and nothing is offered");
        // …and an untold list promises nothing.
        assert!(!Index::default().can_create);
    }

    #[test]
    fn a_full_text_query_does_not_narrow_the_titles_on_screen() {
        let mut ix = index(&["改善案", "画像表示テスト", "テスト"]);
        ix.begin_filter();
        ix.push_filter('図');
        assert_eq!(ix.len(), 1, "no title has 図: the create offer is all that is left");
        assert!(ix.offers_create());

        ix.toggle_filter_mode();
        assert_eq!(ix.filter_mode, FilterMode::FullText);
        assert_eq!(ix.filter, "図", "the typed word carries over");
        assert_eq!(ix.len(), 3, "the list stays as it is until the server answers");
        assert!(!ix.offers_create(), "that word is a query, not a page name");

        // Back again, and it is a title filter once more.
        ix.toggle_filter_mode();
        assert_eq!(ix.len(), 1);
    }

    #[test]
    fn every_order_is_re_imposed_on_its_own_key_not_on_updated() {
        let mk = |title: &str, updated, created, accessed, linked, views| Entry {
            title: title.into(),
            updated,
            created,
            accessed,
            linked,
            views,
            ..Entry::default()
        };
        // Shaped like a pinned page heading a list it does not belong at
        // the top of: "pinned" is oldest, least linked and least read.
        let base = vec![
            mk("pinned", 10, 10, 10, 1, 1),
            mk("Zebra", 300, 100, 200, 50, 9),
            mk("apple", 200, 300, 100, 90, 5),
        ];
        let titles = |sort| {
            let mut e = base.clone();
            Index::sort_entries(&mut e, sort);
            e.into_iter().map(|x| x.title).collect::<Vec<_>>()
        };
        assert_eq!(titles(SortKey::Updated), ["Zebra", "apple", "pinned"]);
        assert_eq!(titles(SortKey::Created), ["apple", "Zebra", "pinned"]);
        assert_eq!(titles(SortKey::Accessed), ["Zebra", "apple", "pinned"]);
        assert_eq!(titles(SortKey::Linked), ["apple", "Zebra", "pinned"]);
        assert_eq!(titles(SortKey::Views), ["Zebra", "apple", "pinned"]);
        // The one ascending order, and the one that ignores case: `apple`
        // must not sort after `Zebra` just because of its byte value.
        assert_eq!(titles(SortKey::Title), ["apple", "pinned", "Zebra"]);
    }

    /// The row's narrow column shows what the list is ordered ON. Sorting
    /// by a number the reader cannot see tells them nothing.
    #[test]
    fn the_row_column_shows_the_value_being_sorted_on() {
        let e = Entry {
            title: "t".into(),
            updated: 1,
            created: 1,
            accessed: 1,
            linked: 1_234,
            views: 17_221,
            ..Entry::default()
        };
        assert_eq!(SortKey::Linked.column(&e), "1.2k");
        assert_eq!(SortKey::Views.column(&e), "17k");
        // The time orders read as an age, and each reads its OWN stamp.
        assert!(SortKey::Updated.column(&e).ends_with('y'), "an age, not a count");
        assert_eq!(SortKey::Created.stamp(&e), 1);
        assert_eq!(SortKey::Accessed.stamp(&e), 1);
        // `title` is not a number and not a time: the column keeps the
        // meaning it has everywhere else in the viewer.
        assert_eq!(SortKey::Title.stamp(&e), e.updated);
        // A stamp the API never filled has no age to claim.
        assert_eq!(relative_age(0), "");
        // The API's own words are what the menu shows.
        assert_eq!(
            SortKey::ALL.map(SortKey::name),
            ["updated", "accessed", "created", "linked", "views", "title"]
        );
    }

    #[test]
    fn a_page_is_unread_until_it_has_been_seen_since_its_last_edit() {
        let p = |updated| PageSummary {
            id: "id".into(),
            title: "t".into(),
            image: None,
            descriptions: vec![],
            updated,
            created: 0,
            accessed: 0,
            views: 0,
            linked: 0,
        };
        assert!(Entry::from_summary(p(100), None).unread, "never opened");
        assert!(Entry::from_summary(p(100), Some(99)).unread, "edited since the visit");
        assert!(!Entry::from_summary(p(100), Some(100)).unread);
        assert!(!Entry::from_summary(p(100), Some(101)).unread);
    }
}
