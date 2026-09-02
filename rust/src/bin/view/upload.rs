use super::*;

use cosense::upload::Destination;

/// A finished background upload: which page and line it was pasted into,
/// where in the line, what the file was called, and the URL (or why not).
pub(crate) struct UploadMsg {
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) line_id: String,
    pub(crate) offset: usize,
    pub(crate) name: String,
    pub(crate) dest: Destination,
    pub(crate) result: Result<String, String>,
}

/// Compose `[URL]` into `text` at byte `offset` (clamped to a char
/// boundary). A picture butting against a word would still draw, but the
/// line reads better with a space either side, so one is added where the
/// neighbour is not already blank. Returns the new text and the byte
/// length inserted.
pub(crate) fn splice_image(text: &str, offset: usize, url: &str) -> (String, usize) {
    let mut at = offset.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
    let after = &text[at..];
    let lead = if before.is_empty() || before.ends_with(char::is_whitespace) { "" } else { " " };
    let trail = if after.is_empty() || after.starts_with(char::is_whitespace) { "" } else { " " };
    let piece = format!("{lead}[{url}]{trail}");
    let len = piece.len();
    (format!("{before}{piece}{after}"), len)
}

impl App {
    /// A pasted image path: read the file, decide where it goes, and send
    /// it on a background thread. The line and caret position are noted
    /// NOW, by line id: by the time the URL comes back the caret may be
    /// elsewhere, a resync may have renumbered the page, or the line may
    /// be gone — `drain_uploads` sorts that out.
    ///
    /// Everything that can be refused is refused here, in the foreground,
    /// with a reason: no session, no permission, too big, no Gyazo token
    /// for a Gyazo destination.
    pub(crate) fn start_upload(&mut self, ctx: &Ctx, path: &std::path::Path) {
        let Some(s) = self.session.as_ref() else {
            self.status = t!("画像は編集中に貼ってください", "paste images while editing");
            return;
        };
        if !self.editable {
            self.status = t!("読み取り専用なので画像を上げられません", "read-only: cannot upload an image");
            return;
        }
        let (line_id, offset) = (self.lines[s.line].id.clone(), s.input.cur);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > cosense::upload::MAX_BYTES {
            self.status = t!("{name} は 100MB を超えています", "{name} is over 100MB");
            return;
        }
        let dest = Destination::resolve(&ctx.config, &self.project, ctx.project_settings(&self.project).as_ref());
        // The token must match the destination (see `Ctx::gyazo_teams_token`);
        // the wrong one would put the picture where the page will not
        // point. So no falling back to the other token: refuse, and name
        // the variable that is missing.
        let token = match &dest {
            Destination::Gyazo { team } => {
                let (token, var) = match team {
                    Some(_) => (&ctx.gyazo_teams_token, "GYAZO_TEAMS_ACCESS_TOKEN"),
                    None => (&ctx.gyazo_personal_token, "GYAZO_ACCESS_TOKEN"),
                };
                match token.clone() {
                    Some(t) => Some(t),
                    None => {
                        self.status = t!("{} へ上げるには {var} が要ります", "uploading to {} needs {var}", dest.label());
                        return;
                    }
                }
            }
            Destination::Gcs => None,
        };
        self.status = t!("{name} を {} へアップロード中…", "uploading {name} to {}…", dest.label());
        if !self.uploads_on {
            return; // tests: nothing goes on the wire
        }
        let tx = self.upload_tx.clone();
        let client = ctx.client.clone();
        let (project, title) = (self.project.clone(), self.title.clone());
        let path = path.to_path_buf();
        let content_type = cosense::upload::content_type_for(&path);
        std::thread::spawn(move || {
            let result = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
                match &dest {
                    Destination::Gcs => client.upload_gcs(&project, &bytes, &name, content_type),
                    Destination::Gyazo { team } => cosense::upload::upload_gyazo(
                        client.http(),
                        token.as_deref().unwrap_or(""),
                        bytes,
                        &name,
                        content_type,
                        team.as_deref(),
                    ),
                }
                .map_err(|e| e.to_string())
            });
            let _ = tx.send(UploadMsg { project, title, line_id, offset, name, dest, result });
        });
    }

    /// Put finished uploads into the page. `true` = something changed.
    ///
    /// Where the URL lands, in order of preference: the remembered line at
    /// the remembered position, in the live edit buffer if the caret is
    /// still on that line (so it commits with whatever else is being
    /// typed) or as a Replace of the line's CURRENT text otherwise (never a
    /// snapshot — the line may have been edited meanwhile); and when the
    /// line is gone, a new last line, said so. A result for a page the
    /// reader has left is dropped: it would be edited blind.
    pub(crate) fn drain_uploads(&mut self, ctx: &Ctx) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.upload_rx.try_recv() {
            if msg.project != self.project || msg.title != self.title {
                continue;
            }
            let url = match msg.result {
                Ok(url) => url,
                Err(e) => {
                    self.status = if e.contains("402") {
                        t!("{}: プロジェクトの容量上限を超えています", "{}: project storage limit reached", msg.name)
                    } else {
                        t!("{} を上げられませんでした — {e}", "could not upload {} — {e}", msg.name)
                    };
                    changed = true;
                    continue;
                }
            };
            let where_ = msg.dest.label();
            match self.lines.iter().position(|l| l.id == msg.line_id) {
                Some(idx) => {
                    let on_line = self.session.as_ref().is_some_and(|s| s.line == idx);
                    if on_line {
                        let s = self.session.as_mut().expect("checked");
                        let (text, len) = splice_image(&s.input.buf, msg.offset, &url);
                        let at = msg.offset.min(s.input.buf.len());
                        if s.input.cur >= at {
                            s.input.cur += len;
                        }
                        s.input.buf = text;
                        s.want_col = None;
                    } else {
                        let (text, _) = splice_image(&self.lines[idx].text, msg.offset, &url);
                        do_edit(self, ctx, &t!("画像", "image"), vec![EditOp::Replace { id: msg.line_id, text }]);
                    }
                    self.status = t!("{} を貼りました ({where_})", "pasted {} ({where_})", msg.name);
                }
                None => {
                    let ops = vec![EditOp::insert("_end", &format!("[{url}]"))];
                    do_edit(self, ctx, &t!("画像", "image"), ops);
                    self.status = t!(
                        "{} を貼りました ({where_}) — 元の行が無くなっていたので末尾に置きました",
                        "pasted {} ({where_}) — its line was gone, so it went to the end",
                        msg.name
                    );
                }
            }
            self.laid_width = 0;
            changed = true;
        }
        changed
    }
}
