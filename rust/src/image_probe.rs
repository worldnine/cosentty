//! Which picture protocol the terminal speaks, and how big its cells are —
//! asked the way ratatui-image asks (kitty graphics query, primary device
//! attributes, cell size, then a device status report so that SOME answer
//! ends the exchange), but read with a deadline on the calling thread.
//!
//! ratatui-image's own `Picker::from_query_stdio` reads the answer in a
//! helper thread and gives up after a timeout — leaving that thread blocked
//! on stdin. In a terminal that never answers (a detached tmux, tmux with
//! `allow-passthrough off`, a pty layer that eats the query) the thread then
//! swallows the first bytes the user types: one keypress lost, at whatever
//! moment it comes. This module exists so the viewer never has that thread.

use ratatui_image::picker::ProtocolType;

/// What the terminal said.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Probe {
    pub kitty: bool,
    pub sixel: bool,
    /// Cell size in pixels: (width, height).
    pub cell: Option<(u16, u16)>,
    /// The device status report came back — the terminal is listening.
    pub answered: bool,
}

/// The query bytes. Inside tmux they go through a passthrough DCS so the
/// OUTER terminal answers (tmux forwards nothing without
/// `allow-passthrough on`; then the deadline is the whole cost).
pub fn query_bytes(in_tmux: bool) -> Vec<u8> {
    let (start, esc, end) = if in_tmux {
        ("\x1bPtmux;", "\x1b\x1b", "\x1b\\")
    } else {
        ("", "\x1b", "")
    };
    let mut q = String::with_capacity(96);
    q.push_str(start);
    q.push_str(&format!("{esc}_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA{esc}\\")); // kitty graphics
    q.push_str(&format!("{esc}[c")); // primary DA (sixel is parameter 4)
    q.push_str(&format!("{esc}[16t")); // cell size in pixels
    q.push_str(&format!("{esc}[5n")); // DSR: the reply that always comes
    q.push_str(end);
    q.into_bytes()
}

/// Has the exchange ended? The DSR reply (`CSI 0 n`, or `CSI 3 n` for a
/// terminal reporting a fault) is sent last by any terminal that answers
/// in order.
pub fn complete(resp: &[u8]) -> bool {
    resp.windows(4).any(|w| w == b"\x1b[0n" || w == b"\x1b[3n")
}

/// Read the answers out of whatever came back (possibly nothing).
pub fn parse(resp: &[u8]) -> Probe {
    let mut p = Probe::default();
    // kitty: APC `_Gi=31;OK` … ST
    if find(resp, b"_Gi=31;OK").is_some() {
        p.kitty = true;
    }
    // DA: CSI ? <params> c — sixel when a parameter equals 4
    let mut i = 0;
    while let Some(at) = find(&resp[i..], b"\x1b[?") {
        let start = i + at + 3;
        let Some(len) = resp[start..]
            .iter()
            .position(|b| !b.is_ascii_digit() && *b != b';')
        else {
            break;
        };
        let end = start + len;
        if resp[end] == b'c' {
            let params = std::str::from_utf8(&resp[start..end]).unwrap_or("");
            if params.split(';').any(|s| s == "4") {
                p.sixel = true;
            }
        }
        i = end + 1;
    }
    // cell size: CSI 6 ; height ; width t
    if let Some(at) = find(resp, b"\x1b[6;") {
        let start = at + 4;
        if let Some(len) = resp[start..].iter().position(|b| *b == b't') {
            let mut it = std::str::from_utf8(&resp[start..start + len])
                .unwrap_or("")
                .split(';')
                .map(|s| s.parse::<u16>().ok());
            if let (Some(Some(h)), Some(Some(w))) = (it.next(), it.next()) {
                if w > 0 && h > 0 {
                    p.cell = Some((w, h));
                }
            }
        }
    }
    p.answered = complete(resp);
    p
}

/// The protocol the answers point at, when they point at one.
pub fn protocol(p: &Probe) -> Option<ProtocolType> {
    if p.kitty {
        Some(ProtocolType::Kitty)
    } else if p.sixel {
        Some(ProtocolType::Sixel)
    } else {
        None
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_answer_is_read() {
        let resp = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;4;22c\x1b[6;20;10t\x1b[0n";
        let p = parse(resp);
        assert!(p.kitty && p.sixel && p.answered);
        assert_eq!(p.cell, Some((10, 20)));
        assert_eq!(protocol(&p), Some(ProtocolType::Kitty));
        assert!(complete(resp));
    }

    #[test]
    fn a_plain_terminal_answers_only_da_and_dsr() {
        let resp = b"\x1b[?1;2c\x1b[0n";
        let p = parse(resp);
        assert!(!p.kitty && !p.sixel && p.answered);
        assert_eq!(p.cell, None);
        assert_eq!(protocol(&p), None);
    }

    #[test]
    fn silence_is_no_answer() {
        let p = parse(b"");
        assert_eq!(p, Probe::default());
        assert!(!complete(b"\x1b[?1;2c"), "DA alone does not end it");
        assert!(!complete(b"\x1b[10n"), "not a DSR reply");
    }

    #[test]
    fn a_sixel_only_terminal() {
        let p = parse(b"\x1b[?64;1;2;4;6c\x1b[6;16;8t\x1b[0n");
        assert_eq!(protocol(&p), Some(ProtocolType::Sixel));
        assert_eq!(p.cell, Some((8, 16)));
        // `4` must be a whole parameter: 44 is not sixel.
        assert!(!parse(b"\x1b[?44c\x1b[0n").sixel);
    }

    #[test]
    fn tmux_wraps_the_query_for_the_outer_terminal() {
        let q = query_bytes(true);
        assert!(
            q.starts_with(b"\x1bPtmux;\x1b\x1b_G"),
            "{:?}",
            String::from_utf8_lossy(&q)
        );
        assert!(q.ends_with(b"\x1b\x1b[5n\x1b\\"));
        let q = query_bytes(false);
        assert!(q.starts_with(b"\x1b_G") && q.ends_with(b"\x1b[5n"));
    }
}
