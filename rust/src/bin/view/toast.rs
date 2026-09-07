//! Toast: a one-shot notice that floats on the row above the footer and
//! leaves by itself (akapen's draw_banner / toast_effect, ported).
//!
//! The footer's status slot used to carry EVERYTHING — "copied", the sync
//! state, the selection hint, a commit failure — so each notice fought the
//! key hints for the one line and the next notice wiped it. The split now:
//!
//! - a **toast** is something that HAPPENED (copied, saved, undone, opened,
//!   failed). It is drawn once for a few seconds and expires on its own,
//!   or on Esc. A newer toast replaces an older one.
//! - `app.status` keeps what IS (the selection hint, the history position,
//!   an upload in flight, a stopped commit worker). It stays until the
//!   state changes, and the footer shows it in the hint slot as before.
//!
//! Not everything that happened deserves a banner. Four levels, by kind
//! (NOTE-notifications.md has the table):
//!
//! - **silent** — the screen already answers: a page opened (the header
//!   changed), a mode switched (the footer badge), a selection dropped.
//! - **note** (`App::note`) — a quiet line that overlays the footer's hint
//!   slot for a few seconds, keys and status stepping aside: "copied",
//!   "✓ line 3", "top of page", "nothing to undo". The reader who wonders
//!   finds it; nobody else is interrupted.
//! - **toast** (yellow) — must be read: "not here, do this instead", and
//!   changes the reader did not make (someone else edited the page).
//! - **toast_err** (red) — something failed. Lives twice as long.
//!
//! The banner reserves no space: its rectangle alone is cleared and
//! painted, so the layout never moves. The frame's bottom rule or the last
//! text row shows around it. tachyonfx fades it in and out; the filter
//! limits the fade to the banner's own cells by their background colour,
//! so a code block or a selection band on the same row never flashes.

use super::*;

use tachyonfx::{fx, CellFilter, Effect, EffectRenderer, Interpolation};
use unicode_width::UnicodeWidthStr;

/// How long a toast stays, fades included.
pub(crate) const TOAST_SECS: Duration = Duration::from_secs(4);
/// An error stays longer: it is the one kind a reader must not miss.
pub(crate) const TOAST_ERR_SECS: Duration = Duration::from_secs(8);
/// How long a footer note stays.
pub(crate) const NOTE_SECS: Duration = Duration::from_secs(4);
/// How long a first `q` waits for the second. Equal to the toast that asks
/// for it, so the question and its window leave together.
pub(crate) const QUIT_ARM_SECS: Duration = TOAST_SECS;
/// The fade at each end.
const FADE_MS: u32 = 120;

pub(crate) struct Toast {
    pub(crate) text: String,
    /// Red and bold, for something that failed or was refused. Info
    /// toasts are yellow: "done", "nothing to do here", a position.
    pub(crate) error: bool,
    /// When the first frame drew it. Set by the draw pass, not the writer:
    /// a toast raised during startup (a config file that would not read)
    /// must not have burnt its seconds before the screen exists.
    pub(crate) shown: Option<Instant>,
    /// The previous frame, for the effect's delta.
    last_frame: Option<Instant>,
    /// Built on the first draw, when the terminal background is known.
    fx: Option<Effect>,
}

impl Toast {
    fn life(&self) -> Duration {
        if self.error {
            TOAST_ERR_SECS
        } else {
            TOAST_SECS
        }
    }
}

impl App {
    /// The quiet level: a line in the footer's hint slot that gives the
    /// slot back after a few seconds. Never touches `status` or the toast.
    pub(crate) fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.note = if msg.is_empty() {
            None
        } else {
            Some((msg, Instant::now() + NOTE_SECS))
        };
    }

    /// The note's text, or "" — for tests; the footer reads the slot itself.
    #[cfg(test)]
    pub(crate) fn note_text(&self) -> &str {
        self.note.as_ref().map(|(m, _)| m.as_str()).unwrap_or("")
    }

    /// Drop the note once it has had its seconds, so the key hints come
    /// back. Returns whether anything changed.
    pub(crate) fn expire_note(&mut self) -> bool {
        let Some((_, until)) = self.note.as_ref() else {
            return false;
        };
        if Instant::now() < *until {
            return false;
        }
        self.note = None;
        true
    }

    /// Say that something happened. Replaces whatever toast is showing.
    pub(crate) fn toast(&mut self, msg: impl Into<String>) {
        self.raise_toast(msg.into(), false);
    }

    /// Say that something failed or could not be done.
    pub(crate) fn toast_err(&mut self, msg: impl Into<String>) {
        self.raise_toast(msg.into(), true);
    }

    fn raise_toast(&mut self, text: String, error: bool) {
        if text.is_empty() {
            self.toast = None;
            return;
        }
        self.toast = Some(Toast {
            text,
            error,
            shown: None,
            last_frame: None,
            fx: None,
        });
    }

    /// The toast's text, or "" — for tests and for the places that need
    /// to quote the last notice.
    pub(crate) fn toast_text(&self) -> &str {
        self.toast.as_ref().map(|t| t.text.as_str()).unwrap_or("")
    }

    /// Esc, and anything else that means "I have seen it".
    pub(crate) fn dismiss_toast(&mut self) {
        self.toast = None;
        self.quit_armed = None;
    }

    /// `q`: the first press asks, the second within [`QUIT_ARM_SECS`]
    /// answers. Returns true when the program should quit now. A single
    /// key is easy to hit by accident (typing into READ, thinking it is
    /// EDIT), and the press-again pattern costs one keystroke and no
    /// dialog — TUIs (vim's `:q!`, lazygit's confirmOnQuit) settle on a
    /// question in the message line, never a modal. `^c` skips this.
    pub(crate) fn confirm_quit(&mut self) -> bool {
        if self
            .quit_armed
            .is_some_and(|at| at.elapsed() <= QUIT_ARM_SECS)
        {
            return true;
        }
        self.quit_armed = Some(Instant::now());
        // Say what would be lost, when something would.
        let mut stake = String::new();
        if self.inflight > 0 {
            stake.push_str(&t!(
                "未送信の編集 {} 件 · ",
                "{} unsent edit(s) · ",
                self.inflight
            ));
        }
        if !self.comments.is_empty() {
            stake.push_str(&t!(
                "未送信のコメント {} 件 · ",
                "{} unsent comment(s) · ",
                self.comments.len()
            ));
        }
        self.toast(t!(
            "{stake}もう一度 q で終了 · Esc で戻る",
            "{stake}q again to quit · Esc to stay"
        ));
        false
    }

    /// Any key that is not `q` withdraws a pending quit: `q j q` within
    /// the window must not leave.
    pub(crate) fn disarm_quit_unless_q(&mut self, k: &event::KeyEvent) {
        let is_q = k.code == KeyCode::Char('q') && !k.modifiers.contains(KeyModifiers::CONTROL);
        if !is_q {
            self.quit_armed = None;
        }
    }

    /// Drop a toast whose time is up. Returns true when one left, so the
    /// caller can redraw.
    pub(crate) fn expire_toast(&mut self) -> bool {
        let gone = self
            .toast
            .as_ref()
            .and_then(|t| t.shown.map(|at| (at, t.life())))
            .is_some_and(|(at, life)| at.elapsed() > life);
        if gone {
            self.toast = None;
        }
        gone
    }
}

/// Fade in from the banner's own background, hold, fade out to it. The
/// total equals the toast's life, so the banner leaves exactly when the
/// toast expires. Filtered to cells of that background: only the banner
/// animates.
fn toast_effect(bg: Color, life: Duration) -> Effect {
    let total = life.as_millis() as u32;
    let hold = total.saturating_sub(FADE_MS * 2);
    let mut effect = fx::sequence(&[
        fx::fade_from(bg, bg, (FADE_MS, Interpolation::Linear)),
        fx::sleep((hold, Interpolation::Linear)),
        fx::fade_to(bg, bg, (FADE_MS, Interpolation::Linear)),
    ]);
    effect.filter(CellFilter::BgColor(bg));
    effect
}

/// The banner's rectangle: one row above the footer, centred, the text's
/// width plus a space each side, never wider than the screen less a
/// margin. `None` when the screen is too small to have a row to spare.
pub(crate) fn toast_rect(area: Rect, text: &str) -> Option<Rect> {
    if area.height < 3 || area.width < 6 {
        return None;
    }
    let w = (UnicodeWidthStr::width(text) as u16).saturating_add(2);
    let w = w.min(area.width.saturating_sub(4)).max(3);
    let x = area.x + (area.width - w) / 2;
    Some(Rect::new(x, area.y + area.height - 2, w, 1))
}

/// Cut `text` to at most `width` columns, ending in `…` when it was cut.
pub(crate) fn clip_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Draw the toast, if one is up. Called last, over everything (the page,
/// the index, an overlay), so it is never hidden by what it comments on.
pub(crate) fn draw_toast(f: &mut Frame, app: &mut App, ctx: &Ctx, area: Rect) {
    let Some(t) = app.toast.as_mut() else { return };
    let now = Instant::now();
    let shown = *t.shown.get_or_insert(now);
    if now.duration_since(shown) > t.life() {
        return; // expire_toast drops it on the next pass
    }
    let Some(rect) = toast_rect(area, &t.text) else {
        return;
    };
    let bg = cosense::theme::toast_bg(ctx.terminal_bg);
    let fg = if t.error { Color::Red } else { Color::Yellow };
    let text = clip_to_width(&t.text, rect.width.saturating_sub(2) as usize);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {text} "),
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        ))),
        rect,
    );
    let dt = t
        .last_frame
        .map(|p| now.duration_since(p))
        .unwrap_or(Duration::ZERO);
    t.last_frame = Some(now);
    let life = t.life();
    let fx = t.fx.get_or_insert_with(|| toast_effect(bg, life));
    f.render_effect(fx, Rect::new(area.x, rect.y, area.width, 1), dt);
}
