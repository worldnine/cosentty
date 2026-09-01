use super::*;

/// Something Enter/f can act on from the cursor line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LinkItem {
    /// An internal page link (`[Page]`, `#tag`) in the current project.
    Page(String),
    /// A cross-project link (`[/project/Page]`): the viewer moves into that
    /// project, as the browser does.
    ProjectPage { project: String, title: String },
    /// A link to a whole project (`[/project]`), which on Cosense is its
    /// home screen. Here that is the project's index.
    ProjectIndex { project: String },
    /// An uploaded file (`[name https://scrapbox.io/files/…pdf]`).
    File { label: String, url: String },
    /// Any other http(s) URL (`[title https://…]`, bare URL) — the browser's.
    Url { label: String, url: String },
    /// A `code:` or `table:` block, saved from the page in hand.
    ///
    /// Cosense does serve both as files, but only to a session cookie:
    /// `/api/code/…` answers 401 to the token this viewer authenticates
    /// with. The page is already here, so the file is written from it — no
    /// second credential, no round trip, and what lands is exactly what is
    /// on screen.
    Export { label: String, src: usize, csv: bool },
}

impl LinkItem {
    pub(crate) fn label(&self) -> String {
        match self {
            LinkItem::Page(t) => t.clone(),
            LinkItem::ProjectPage { project, title } => format!("/{project}/{title}"),
            LinkItem::ProjectIndex { project } => format!("/{project}"),
            LinkItem::File { label, .. } => format!("↓ {label}"),
            LinkItem::Export { label, .. } => format!("↓ {label}"),
            LinkItem::Url { label, .. } => format!("↗ {label}"),
        }
    }

}

/// A Cosense page URL from the command line, as `(project, title, lineId)`:
/// `https://scrapbox.io/<project>/<title>#<lineId>` (title and fragment
/// optional; the title is percent-encoded as the browser shows it). Also
/// accepts the `cosen.se` short host and a scheme-less `scrapbox.io/…`.
/// `None` for anything that is not such a URL (a plain project name).
pub(crate) fn parse_page_url(arg: &str) -> Option<(String, Option<String>, Option<String>)> {
    let rest = arg
        .strip_prefix("https://")
        .or_else(|| arg.strip_prefix("http://"))
        .unwrap_or(arg);
    let rest = ["scrapbox.io/", "www.scrapbox.io/", "cosen.se/"]
        .iter()
        .find_map(|h| rest.strip_prefix(h))?;
    // the fragment is the line id; a query string is not part of the title
    let (path, fragment) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    let mut segs = path.splitn(2, '/');
    let project = segs.next().filter(|p| !p.is_empty())?.to_string();
    let title = segs
        .next()
        .map(|t| t.trim_end_matches('/'))
        .filter(|t| !t.is_empty())
        .map(percent_decode);
    let line_id = fragment.filter(|f| !f.is_empty()).map(str::to_string);
    Some((project, title, line_id))
}

/// Decode `%XX` escapes (UTF-8) as browsers encode page titles; a stray
/// `%` that is not an escape is kept as is.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn urlencode_component(s: &str) -> String {
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

/// Open a URL in the default browser (macOS `open`, Linux `xdg-open`).
pub(crate) fn open_in_browser(url: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(cmd)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
            true
        })
        .unwrap_or(false)
}

/// Page links on a raw Scrapbox line, left to right: `[PageName]` (not a
/// URL, decoration, or icon) and `#hashtag` in the current project, and
/// `[/project/PageName]` into another project. A bare `[/project]` (the
/// project's top page) is skipped: it is not a page.
/// The line with every inline-code span blanked out, byte offsets intact.
///
/// Backticks quote notation: a line that WRITES about `[a link]` is not a
/// line that HAS one. The renderer already knew this; link extraction did
/// not, so a page documenting the notation was covered in links to pages
/// nobody meant to name.
pub(crate) fn mask_inline_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for ch in text.chars() {
        if ch == '`' {
            in_code = !in_code;
            out.push(' ');
            continue;
        }
        if in_code {
            for _ in 0..ch.len_utf8() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn positioned_links_on_line(text: &str) -> Vec<(usize, LinkItem)> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel_start) = text[from..].find('[') {
        let start = from + rel_start;
        if let Some(rel_end) = text[start + 1..].find(']') {
            let end = start + 1 + rel_end;
            let inner = &text[start + 1..end];
            let is_deco = inner
                .find(' ')
                .map(|sp| inner[..sp].chars().all(|c| matches!(c, '*' | '/' | '_' | '-')))
                .unwrap_or(false)
                && inner.starts_with(|c| matches!(c, '*' | '/' | '_' | '-'));
            let is_url = inner.contains("http://") || inner.contains("https://");
            let is_icon = inner.contains(".icon");
            if !is_deco && !is_url && !is_icon && !inner.is_empty() {
                // One reading of what a bracket points at, shared with the
                // renderer's hits (`page_link_item`): a page here, a page
                // over there, or a whole project.
                out.push((start, page_link_item(inner)));
            }
            from = end + 1;
        } else {
            break;
        }
    }
    // hashtags
    let mut from = 0usize;
    while let Some(rel_pos) = text[from..].find('#') {
        let pos = from + rel_pos;
        let ok = pos == 0
            || text[..pos].chars().next_back().map(|c| c.is_whitespace()).unwrap_or(false);
        let tag: String = text[pos + 1..].chars().take_while(|c| !c.is_whitespace()).collect();
        if ok && !tag.is_empty() {
            out.push((pos, LinkItem::Page(tag)));
        }
        from = pos + 1;
    }
    out
}

/// What `[...]` (or a `#tag`) leads to: a page here, or a page in another
/// project when it is written `/project/title`.
pub(crate) fn page_link_item(text: &str) -> LinkItem {
    let Some(rest) = text.strip_prefix('/') else {
        return LinkItem::Page(text.to_string());
    };
    let (project, title) = match rest.split_once('/') {
        Some((p, t)) => (p, t.trim()),
        None => (rest, ""),
    };
    if project.is_empty() {
        return LinkItem::Page(text.to_string());
    }
    if title.is_empty() {
        // `[/project]` (or `[/project/]`): the project itself, whose home
        // screen here is its index.
        return LinkItem::ProjectIndex { project: project.to_string() };
    }
    LinkItem::ProjectPage { project: project.to_string(), title: title.to_string() }
}

pub(crate) fn links_on_line(text: &str) -> Vec<LinkItem> {
    positioned_links_on_line(text).into_iter().map(|(_, item)| item).collect()
}

/// Every http(s) URL on a raw Scrapbox line, as `(label, url)`, left to
/// right. The bracket form `[title https://…]` (either order) labels the
/// URL with `title`; a bare URL is labelled by its file name for uploads
/// and by the URL itself otherwise.
pub(crate) fn positioned_labelled_urls(text: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("http") {
        let start = from + rel;
        let url_end = text[start..]
            .find(|c: char| c.is_whitespace() || c == ']')
            .map(|e| start + e)
            .unwrap_or(text.len());
        let url = &text[start..url_end];
        from = url_end.max(start + 1);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        // The bracket around this URL, if any: `[label url]`.
        let bracket = text[..start]
            .rfind('[')
            .filter(|&lb| !text[lb..start].contains(']'))
            .and_then(|lb| text[url_end..].find(']').map(|rb| (lb, url_end + rb)));
        // A LINKED IMAGE (`[href imageUrl]`) is one link, not two: the
        // picture is the label and the href is where it goes. Offering
        // both, with each labelled by the other, was the reason a linked
        // image asked which link you meant — and then opened the other one.
        if let Some((lb, rb)) = bracket {
            let inner = &text[lb + 1..rb];
            if cosense::render::looks_like_image_url(url) && has_other_url(inner, url) {
                continue; // the href carries this bracket
            }
        }
        let label = bracket
            .map(|(lb, rb)| text[lb + 1..rb].replace(url, "").trim().to_string())
            .filter(|l| !l.is_empty())
            .map(|l| {
                // The other side of a linked image is the picture: say so
                // rather than printing its URL.
                if gyazo_permalink(&l).is_some() {
                    "\u{1f5bc} gyazo".to_string()
                } else if cosense::render::looks_like_image_url(&l) {
                    format!("\u{1f5bc} {}", file_name_of_url(&l))
                } else {
                    l
                }
            })
            .unwrap_or_else(|| {
                if is_scrapbox_file_url(url) {
                    file_name_of_url(url).to_string()
                } else if gyazo_permalink(url).is_some() {
                    // `link_item_for_url` names these "gyazo" — leave it to it.
                    url.to_string()
                } else if cosense::render::looks_like_image_url(url) {
                    format!("\u{1f5bc} {}", file_name_of_url(url))
                } else {
                    url.to_string()
                }
            });
        out.push((start, label, url.to_string()));
    }
    out
}

/// Does `inner` hold a URL other than `url`?
pub(crate) fn has_other_url(inner: &str, url: &str) -> bool {
    inner
        .split_whitespace()
        .any(|t| t != url && (t.starts_with("http://") || t.starts_with("https://")))
}

pub(crate) fn labelled_urls(text: &str) -> Vec<(String, String)> {
    positioned_labelled_urls(text)
        .into_iter()
        .map(|(_, label, url)| (label, url))
        .collect()
}

/// A `code:` or `table:` header line, as something to FOLLOW.
///
/// Cosense serves both as files — `/api/code/<project>/<title>/<name>` and
/// `/api/table/<project>/<title>/<name>.csv` — so the header line can be a
/// link like any other, and `Enter` saves it the way it saves an
/// attachment. No new key, no invented download path.
pub(crate) fn block_export_link(text: &str, src: usize) -> Option<LinkItem> {
    let body = text.trim_start();
    let (csv, name) = if let Some(n) = body.strip_prefix("code:") {
        (false, n.trim())
    } else if let Some(n) = body.strip_prefix("table:") {
        (true, n.trim())
    } else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let label = if csv { format!("{name}.csv") } else { name.to_string() };
    Some(LinkItem::Export { label, src, csv })
}

/// The text of the block whose header is at `src`: the code as it is
/// written, or the table as CSV.
///
/// A Cosense table is tab-separated cells, so the conversion is
/// mechanical; only a cell holding a comma, a quote or a newline needs
/// quoting (RFC 4180), and that is decided per cell rather than guessed.
pub(crate) fn block_export_body(lines: &[PageLine], src: usize, csv: bool) -> String {
    let Some(header) = lines.get(src) else { return String::new() };
    let indent = indent_of(&header.text).chars().count();
    let mut out: Vec<String> = Vec::new();
    for line in lines.iter().skip(src + 1) {
        let depth = line.text.chars().take_while(|c| c.is_whitespace()).count();
        if depth <= indent && !line.text.trim().is_empty() {
            break;
        }
        if depth <= indent {
            if csv {
                break; // a blank line ends a table
            }
            out.push(String::new());
            continue;
        }
        let body: String = line.text.chars().skip(indent + 1).collect();
        out.push(if csv { csv_row(&body) } else { body });
    }
    // Trailing blank lines belong to the page, not to the block.
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

/// One table row as CSV (RFC 4180 quoting).
pub(crate) fn csv_row(row: &str) -> String {
    row.split('\t')
        .map(|cell| {
            let cell = cell.trim_end();
            if cell.contains([',', '"', '\n']) {
                format!("\"{}\"", cell.replace('"', "\"\""))
            } else {
                cell.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn link_item_for_url(label: String, url: String) -> LinkItem {
    if is_scrapbox_file_url(&url) {
        LinkItem::File { label, url }
    } else if let Some(page) = gyazo_permalink(&url) {
        // An inline image opens its Gyazo page (comments, original, Teams
        // org), not the raw pixel URL.
        let label = if label == url { "gyazo".to_string() } else { label };
        LinkItem::Url { label, url: page }
    } else {
        LinkItem::Url { label, url }
    }
}

/// Uploaded-file links on a raw Scrapbox line (see `labelled_urls`).
#[cfg(test)]
pub(crate) fn files_on_line(text: &str) -> Vec<(String, String)> {
    labelled_urls(text).into_iter().filter(|(_, u)| is_scrapbox_file_url(u)).collect()
}

/// Where downloads go, asked in the order a reader would expect to be
/// asked: `--download-dir`, `COSENSE_DOWNLOAD_DIR`, the desktop's own
/// `XDG_DOWNLOAD_DIR`, `~/Downloads`, and finally the working directory.
///
/// `~/Downloads` was the whole rule, which is a machine-shaped assumption:
/// a server account or a stripped-down box has no such directory, and the
/// file then landed in whatever directory the viewer happened to start in
/// without anyone having chosen it.
///
/// The bool is "the user named this place". A named directory is created if
/// it is missing — naming it is the request. A directory merely FOUND has
/// to exist already, or the search moves on.
pub(crate) fn pick_download_dir(
    explicit: Option<&str>,
    env_dir: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
    cwd: std::path::PathBuf,
) -> (std::path::PathBuf, bool) {
    let named = explicit
        .into_iter()
        .chain(env_dir)
        .map(str::trim)
        .find(|s| !s.is_empty());
    if let Some(dir) = named {
        return (expand_home(dir, home), true);
    }
    let found = xdg
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| expand_home(s, home))
        .filter(|d| d.is_dir())
        .or_else(|| {
            home.map(|h| std::path::PathBuf::from(h).join("Downloads")).filter(|d| d.is_dir())
        });
    (found.unwrap_or(cwd), false)
}

/// `~` and `~/…` against a known home. Config files and shells both write
/// paths that way, and the env var arrives unexpanded when it was set by
/// hand rather than by the shell.
pub(crate) fn expand_home(path: &str, home: Option<&str>) -> std::path::PathBuf {
    let Some(home) = home else { return std::path::PathBuf::from(path) };
    match path {
        "~" => std::path::PathBuf::from(home),
        p if p.starts_with("~/") => std::path::PathBuf::from(home).join(&p[2..]),
        p => std::path::PathBuf::from(p),
    }
}

/// `<dir>/<name>`, where `<name>` is the link's label when it carries an
/// extension, else the URL's file name; an existing file is not overwritten
/// (`name (2).pdf`, `name (3).pdf`, …).
pub(crate) fn download_path_in(dir: &std::path::Path, label: &str, url: &str) -> std::path::PathBuf {
    let raw = if label.contains('.') { label } else { file_name_of_url(url) };
    let name: String = raw.chars().map(|c| if c == '/' || c == '\0' { '_' } else { c }).collect();
    let mut path = dir.join(&name);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Follow one link: pages navigate, files download, URLs go to the browser.
pub(crate) fn activate_link(app: &mut App, ctx: &Ctx, item: LinkItem) {
    match item {
        LinkItem::Page(title) => {
            let project = app.project.clone();
            navigate_to(app, ctx, &project, &title)
        }
        LinkItem::ProjectPage { project, title } => navigate_to(app, ctx, &project, &title),
        // A whole project: its index, which is what Cosense's home screen
        // is for. The place being left goes on the back stack, as it does
        // for a page.
        LinkItem::ProjectIndex { project } => {
            let from = app.here();
            open_index(app, ctx, &project, String::new());
            if app.index.is_some() {
                app.history.push(from);
                app.forward.clear();
                app.status = format!("→ /{project}");
            }
        }
        LinkItem::File { label, url } => app.start_download(ctx, label, url),
        LinkItem::Export { label, src, csv } => {
            let body = block_export_body(&app.lines, src, csv);
            let dest = download_path_in(&ctx.download_dir, &label, "");
            app.status = match std::fs::write(&dest, body) {
                Ok(()) => {
                    let shown = dest.display().to_string();
                    if open_in_browser(&shown) {
                        t!("保存しました {shown} · 開きました", "saved {shown} · opened")
                    } else {
                        t!("保存しました {shown}", "saved {shown}")
                    }
                }
                Err(e) => t!("保存に失敗しました: {label} — {e}", "save failed: {label} — {e}"),
            };
        }
        LinkItem::Url { url, .. } => {
            app.status = if open_in_browser(&url) {
                t!("開きました {url}", "opened {url}")
            } else {
                t!("ブラウザを開けません", "failed to open browser")
            };
        }
    }
}

pub(crate) fn short(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

/// What `y` copies, and what to call it in the status line.
///
/// Always the **Cosense source**: indentation, `[]` links, `code:` blocks,
/// exactly as the page stores them. That is what survives a round trip
/// into another Cosense page, an editor, or a commit message. (A rendered
/// copy would lose the notation and the structure with it.)
///
/// The caret line comes from the session buffer, so what you copy is what
/// you see, not the last version the server heard.
pub(crate) fn copy_payload(app: &App, whole_page: bool) -> Option<(String, String)> {
    if app.lines.is_empty() {
        return None;
    }
    let text_of = |i: usize| -> String {
        match app.session.as_ref() {
            Some(s) if s.line == i => s.input.buf.clone(),
            _ => app.lines[i].text.clone(),
        }
    };
    if whole_page {
        let body: Vec<String> = (0..app.lines.len()).map(text_of).collect();
        return Some((body.join("\n"), format!("page ({} lines)", body.len())));
    }
    if let Some(s) = app.session.as_ref() {
        if let Some((a, b)) = s.sel_span() {
            return Some((s.input.buf[a..b].to_string(), "selection".into()));
        }
        // A range across lines copies as text too: the anchor line's
        // tail, the lines between whole, the far line's head — what the
        // range really holds, joinable back into one paste.
        if let Some(((tl, tb), (bl, bb))) = s.sel_ends() {
            if tl != bl && bl < app.lines.len() {
                let top = text_of(tl);
                let mut parts = vec![top[floor_boundary(&top, tb)..].to_string()];
                parts.extend((tl + 1..bl).map(&text_of));
                let bot = text_of(bl);
                parts.push(bot[..floor_boundary(&bot, bb)].to_string());
                let label = format!("selection ({} lines)", parts.len());
                return Some((parts.join("\n"), label));
            }
        }
    }
    let (a, b) = match app.selection {
        Some(sel) => sel.range(),
        None => {
            let line = app.session.as_ref().map(|s| s.line).unwrap_or(app.cursor);
            (line, line)
        }
    };
    let b = b.min(app.lines.len() - 1);
    if a > b {
        return None;
    }
    let body: Vec<String> = (a..=b).map(text_of).collect();
    let label = if body.len() == 1 { "line".into() } else { format!("{} lines", body.len()) };
    Some((body.join("\n"), label))
}

/// Copy `payload` and report it on the status line — including the ways it
/// can fail, which are silent otherwise (no clipboard tool, a terminal
/// that will not take OSC 52, a copy too large to send that way).
pub(crate) fn copy_and_report(app: &mut App, payload: Option<(String, String)>) {
    let Some((text, label)) = payload else {
        app.status = t!("コピーするものがありません", "nothing to copy");
        return;
    };
    app.status = if copy_to_clipboard(&text) {
        format!("✓ copied {label}")
    } else {
        t!("コピーできません — クリップボードのコマンドが無く、端末も OSC 52 を拒否しました", "copy failed — no clipboard tool and the terminal refused OSC 52")
    };
}

pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    // Over SSH, `pbcopy` and friends would write into the clipboard of the
    // machine the viewer runs on — not the one the hands are on. OSC 52
    // hands the text to the terminal EMULATOR, which is always local, so
    // it is the only thing that works remotely.
    let remote =
        std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
    if !remote {
        for (bin, args) in [
            ("pbcopy", &[][..]),
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ] {
            if pipe_to(bin, args, text) {
                return true;
            }
        }
    }
    osc52_copy(text)
}

/// Feed `text` to a clipboard helper on stdin. False when it is not
/// installed or refuses.
pub(crate) fn pipe_to(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin).args(args).stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Hand the text to the terminal emulator itself (OSC 52). Works through
/// SSH, and through tmux when the sequence is wrapped in its passthrough.
///
/// Terminals cap what they will take (commonly ~74 KB of base64); a copy
/// too big to send is refused here rather than silently truncated.
pub(crate) fn osc52_copy(text: &str) -> bool {
    const MAX_BYTES: usize = 48 * 1024;
    if text.len() > MAX_BYTES {
        return false;
    }
    let payload = format!("\x1b]52;c;{}\x07", b64_encode(text.as_bytes()));
    let seq = if std::env::var_os("TMUX").is_some() {
        // tmux only forwards what it is told to forward.
        format!("\x1bPtmux;{}\x1b\\", payload.replace('\x1b', "\x1b\x1b"))
    } else {
        payload
    };
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok()
}

/// Standard base64, for OSC 52. (The decoder lives in `chrome`; this is
/// the only place that needs to encode.)
pub(crate) fn b64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Load `project/title` and install it, pushing the current page onto
/// history. `project` may differ from the current one (`[/project/title]`).
pub(crate) fn navigate_to(app: &mut App, ctx: &Ctx, project: &str, title: &str) {
    let from = app.here();
    // The caller has nothing to undo: it was not holding anything the way
    // the index holds its list.
    let _ = navigate_from(app, ctx, project, title, from);
}

/// `navigate_to`, saying explicitly where it is being left from — the
/// index closes before the page loads, so it has to name itself while it
/// still can.
#[must_use = "a failed navigation leaves the reader where they were"]
pub(crate) fn navigate_from(app: &mut App, ctx: &Ctx, project: &str, title: &str, from: Place) -> bool {
    let same_project = app.project == project;
    match load_page(ctx, project, title) {
        Ok(loaded) => {
            app.history.push(from);
            app.forward.clear();
            app.set_page(loaded, ctx);
            app.status = if page_is_uncreated(app) {
                // Following a link to a page nobody has written yet is how
                // a wiki grows. Say what it is, and what makes it real.
                t!("未作成のページ — e / o で書き始めると作成されます（{title}）", "an uncreated page — e / o starts writing it ({title})")
            } else if same_project {
                format!("→ {title}")
            } else {
                format!("→ /{project}/{title}")
            };
            true
        }
        Err(e) => {
            app.status = t!("開けません: /{project}/{title} — {e}", "open failed: /{project}/{title} — {e}");
            false
        }
    }
}
