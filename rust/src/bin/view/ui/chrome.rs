//! Header palette, gutter marks, mode hints and title fitting.

use super::*;

/// READ の選択バンド背景。カーソル行の帯(`CURSOR_BG` の DarkGray)と
/// 見分けがつくよう、端末背景から作った寒色を使う(theme::selection_band)。
pub(crate) fn selection_bg(terminal_bg: (u8, u8, u8)) -> Color {
    cosense::theme::selection_band(terminal_bg)
}

/// The list marker, in the one place both the drawing and its tests read.
pub(crate) const BULLET: &str = "\u{2022}";

/// What a TAB looks like on the caret line: one column, so the cells it
/// separates stay apart and the caret has somewhere to be.
pub(crate) const TAB_MARK: &str = "|";

/// Source mode's line-number gutter: `"  12 "` (4 digits + space).
pub(crate) const SOURCE_NUM_W: usize = 5;

/// Cursor-row highlight inside the page body.
pub(crate) const CURSOR_BG: Color = Color::DarkGray;

// Non-content chrome uses ANSI palette entries, never fixed RGB. Terminal
// themes own the actual values behind these role colors.
pub(crate) const CHROME_ACCENT: Color = Color::Cyan;

pub(crate) const CHROME_ACTIVE: Color = Color::Green;

pub(crate) const CHROME_CARET: Color = Color::LightBlue;

pub(crate) const CHROME_DIM: Color = Color::DarkGray;

pub(crate) const CHROME_SCROLL: Color = Color::Gray;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeaderColors {
    pub(crate) fg: Color,
    pub(crate) bg: Color,
}

impl HeaderColors {
    pub(crate) fn fallback() -> Self {
        Self {
            fg: Color::Black,
            bg: CHROME_ACCENT,
        }
    }
}

impl App {
    /// The colours the header, the page frame and the footer badge wear:
    /// the project's own while reading NOW, Cosense's purple while a past
    /// snapshot is shown — the one sign of history that is on every edge
    /// of the page rather than in a word.
    pub(crate) fn chrome_colors(&self, ctx: &Ctx) -> HeaderColors {
        if self.time.is_some() {
            let (fg, bg) = cosense::theme::history_header_colors(ctx.terminal_bg);
            HeaderColors { fg, bg }
        } else if let Some(ix) = self.index.as_ref() {
            // A list belongs to its LISTED project: the header answers to
            // it (green for acme's list), not to the page left behind.
            // The projects list is a PICKER, not a project — it wears the
            // usual menu title's look (black on the accent cyan), theme-
            // independent like the link-choice windows. The lookup is
            // cached: arriving at an index already reads the project
            // settings for its display name.
            if ix.scope == cosense::index::Scope::Projects {
                HeaderColors::fallback()
            } else {
                let theme = ctx.project_theme(&self.index_project);
                let (fg, bg) =
                    cosense::theme::project_header_colors(theme.as_deref(), ctx.terminal_bg);
                HeaderColors { fg, bg }
            }
        } else {
            self.header_colors
        }
    }

    /// When the page as shown was last written: the newest line's stamp
    /// for NOW (Cosense's `updated` is exactly that), the snapshot's for
    /// history. 0 when nothing carries a stamp (a page typed offline, a
    /// test fixture).
    pub(crate) fn shown_updated(&self) -> i64 {
        match self.time.as_ref() {
            Some(tm) => tm.points[tm.pos].created,
            None => self.lines.iter().map(|l| l.updated).max().unwrap_or(0),
        }
    }
}

/// The telomere gutter cell for a row with a source line (`age` = seconds
/// since the edit, plus the line's `TelomereState`). The cursor does not
/// compete for this cell: its `>` is painted separately on the frame column.
///
/// The SAME cell serves the related-page rows below the body (their age is
/// their page's, see `App::related_telomere`) and, through
/// `theme::telomere`, the index's list. Thickness is always "how recently
/// did this change" and colour is always "have I seen it" — a mark that
/// meant different things on different screens would be worse than no mark.
///
/// A comment marker is deliberately NOT drawn yet: akapen colors comments
/// yellow (`▌`) and reserves green for changed lines, so guessing a color
/// before the telomere/comment balance is settled would bake in a wrong
/// convention. `has_comment` is kept so the caller can still tint the row
/// without touching the marker column.
/// The thick bar in the frame's left column on commented lines (yellow)
/// and on the lines being commented on (cyan, while the composer is open).
/// akapen's marker, same glyph.
pub(crate) const COMMENT_BAR: &str = "▌";

pub(crate) fn gutter_cell(
    age: Option<(i64, cosense::theme::TelomereState)>,
    light: bool,
    tint: Option<cosense::theme::TelomereTint>,
) -> (&'static str, Style) {
    match age {
        Some((a, state)) => {
            let (glyph, color) = cosense::theme::telomere_with(state, a, light, tint);
            (glyph, Style::default().fg(color))
        }
        None => (" ", Style::default()),
    }
}

impl App {
    pub(crate) fn hint_text(&self, cursor_links: &[LinkItem]) -> String {
        match self.sync_notice() {
            Some(tag) => format!("{tag} · {}", self.hint_body(cursor_links)),
            None => self.hint_body(cursor_links),
        }
    }

    pub(crate) fn hint_body(&self, cursor_links: &[LinkItem]) -> String {
        // The quiet level overlays the slot: for its few seconds the note
        // is the whole hint — the keys and the standing status step aside
        // rather than crowd it, and come back when it expires. The one
        // exception is the ^g prefix: its next key is consumed whatever it
        // is, so the reader must see what it accepts.
        if let (Some((msg, _)), false) = (self.note.as_ref(), self.outline_prefix) {
            return msg.clone();
        }
        // The move mode says its own name and its own keys, in words: the
        // grabbed block is highlighted, but a highlight is a colour and a
        // colour alone must not be what tells the reader where they are.
        if self.move_mode.is_some() {
            let keys = t!(
                "MOVE — j/k 1行 · J/K 兄弟 · h/l 字下げ · Esc 確定",
                "MOVE — j/k line · J/K sibling · h/l indent · Esc commit"
            );
            return match self.status.is_empty() {
                true => keys,
                false => format!("{keys} · {}", self.status),
            };
        }
        if self.outline_prefix {
            return t!(
                "OUTLINE h/j/k/l 行 左/下/上/右 · H/J/K/L ブロック · Esc 取消",
                "OUTLINE h/j/k/l line left/down/up/right · H/J/K/L block · Esc cancel"
            );
        }
        if let Some(s) = self.session.as_ref() {
            let dirty = s.input.buf != s.orig;
            let keys = t!(
                "{}↑↓ 移動 · Enter 改行 · ⌫@行頭 前の行と結合 · Tab 字下げ · Esc 終了",
                "{}↑↓ move · Enter new line · ⌫@BOL join · Tab indent · Esc done",
                if dirty { "● " } else { "" }
            );
            // An upload's progress arrives while the keys are being typed;
            // it rides along behind the hint as MOVE's does.
            return match self.status.is_empty() {
                true => keys,
                false => format!("{keys} · {}", self.status),
            };
        }
        if !cursor_links.is_empty() {
            let listed: Vec<String> = cursor_links
                .iter()
                .take(9)
                .enumerate()
                .map(|(i, l)| {
                    // A link with nothing behind it does not open: Enter
                    // starts the page. Better said before it is pressed.
                    let mark = match l {
                        LinkItem::Page(t) if self.links.missing(t) => {
                            ts!("(未作成)", "(uncreated)")
                        }
                        _ => "",
                    };
                    format!("{}:{}{mark}", i + 1, l.label())
                })
                .collect();
            return t!(
                "Enter/f で開く → {}",
                "Enter/f open → {}",
                listed.join("  ")
            );
        }
        if !self.status.is_empty() {
            return self.status.clone();
        }
        if self.editable {
            t!("j/k 移動  Enter リンク  e 編集  o 行追加  u 取り消し  w ブラウザ  ? ヘルプ  q 終了", "j/k move  Enter link  e edit  o new line  u undo  w browser  ? help  q quit")
        } else {
            t!(
                "j/k 移動  Enter リンク  w ブラウザ  ? ヘルプ  q 終了  · 読み取り専用",
                "j/k move  Enter link  w browser  ? help  q quit  · read-only"
            )
        }
    }
}

/// One header row: ` name / title` at the left edge, `right` flush with
/// the right edge. When it all does not fit, things give way in this order:
/// the RIGHT side never (the states — history position, date, unsynced,
/// read-only — are what the header is for); the site NAME goes first and
/// goes whole, leaving ` / title` (the name is one word the reader already
/// knows, and cutting it to `研…` says nothing); the TITLE is cut to `…`
/// only after that.
pub(crate) fn header_line(name: &str, title: &str, right: &str, width: u16) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = width as usize;
    let rw = UnicodeWidthStr::width(right);
    let room = width.saturating_sub(rw + 1); // one blank before the badges
    let full = format!(" {name} / {title}");
    let left = if UnicodeWidthStr::width(full.as_str()) <= room {
        full
    } else {
        let bare = format!(" / {title}");
        if UnicodeWidthStr::width(bare.as_str()) <= room {
            bare
        } else {
            truncate_width(&bare, room)
        }
    };
    let pad = width.saturating_sub(UnicodeWidthStr::width(left.as_str()) + rw);
    format!("{left}{}{right}", " ".repeat(pad))
}
