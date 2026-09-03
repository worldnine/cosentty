use super::*;

pub(crate) struct ImageInfo {
    /// Sliced protocol: renders row-by-row so partial vertical scroll clips
    /// cleanly (SignedPosition.y may be negative) instead of vanishing.
    pub(crate) sliced: SlicedProtocol,
    /// Display height in cells (from the sliced size), used for layout.
    pub(crate) cells_h: u16,
    /// Display width in cells, and the column cap this was encoded for. A
    /// diagram is re-encoded from its cached PNG when the pane crosses that
    /// cap, so it never paints past the text column (the draw clips to the
    /// pane, so an over-wide image simply loses its right-hand side).
    pub(crate) cells_w: u16,
    pub(crate) built_for: u16,
}

/// Widest an inline image may be drawn, in cells. Images are capped at a
/// fixed 64 columns regardless of the pane — that is long-standing
/// behaviour and is left alone. A DIAGRAM additionally has to fit the pane:
/// a clipped photo is still a photo, a clipped flowchart has lost its right
/// half.
pub(crate) const IMAGE_MAX_COLS: u16 = 64;

/// Column cap for a diagram in a pane whose text area is `text_w` wide.
pub(crate) fn diagram_max_cols(text_w: u16) -> u16 {
    text_w.min(IMAGE_MAX_COLS).max(1)
}

/// How many pictures download at once. See `start_image_loads`.
pub(crate) const IMAGE_PARALLEL: usize = 4;

/// A counting gate for the download threads: `acquire` waits for a slot
/// and hands back a guard that returns it. Std has no semaphore; this is
/// the twenty lines of one.
pub(crate) struct Slots {
    used: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
    cap: usize,
}

pub(crate) struct Slot<'a>(&'a Slots);

impl Slots {
    pub(crate) const fn new(cap: usize) -> Self {
        Slots { used: std::sync::Mutex::new(0), freed: std::sync::Condvar::new(), cap }
    }
    pub(crate) fn acquire(&self) -> Slot<'_> {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used >= self.cap {
            used = self.freed.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += 1;
        Slot(self)
    }
    pub(crate) fn in_use(&self) -> usize {
        *self.used.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut used = self.0.used.lock().unwrap_or_else(|e| e.into_inner());
        *used -= 1;
        self.0.freed.notify_one();
    }
}

pub(crate) static IMAGE_SLOTS: Slots = Slots::new(IMAGE_PARALLEL);

/// Rows a not-yet-decoded picture takes in a MIXED line (`inline_row`),
/// so the text beside it does not jump when it lands. A picture on a line
/// of its own reserves nothing: it shows as its `[URL]` row until it
/// arrives (`Row::ImageLoading`).
pub(crate) const IMAGE_PLACEHOLDER_H: u16 = 8;

/// The tallest a picture may be drawn, in rows. Beyond this the reader is
/// scrolling through one image instead of reading a page; the picture is
/// scaled down (keeping its shape) so the text around it stays reachable.
pub(crate) const MAX_IMAGE_ROWS: u16 = 20;

/// …and how wide, while a mixed line is being laid out without it.
pub(crate) const IMAGE_PLACEHOLDER_W: u16 = 24;

/// A finished background image load: the image already resized and encoded
/// for the terminal (the expensive part — done on the worker so the UI
/// never stalls while a page's images arrive), or why it failed.
pub(crate) type ImageMsg = (String, Result<ImageInfo, String>);

/// A finished background file download: the label shown to the user and
/// where it landed, or why it failed.
pub(crate) type FileMsg = (String, Result<std::path::PathBuf, String>);

/// PNG bytes -> terminal image. Only image decoding happens here: no SVG or
/// HTML from the browser is ever interpreted in this process.
pub(crate) fn decode_web_png(picker: &Picker, png: &[u8], max_cols: u16) -> Result<ImageInfo, String> {
    let img = image::load_from_memory(png).map_err(|e| e.to_string())?;
    build_image(picker, img, max_cols)
}

/// How pictures get onto the screen.
///
/// Quality first: whatever the terminal answers to the capability query.
/// Half-blocks are a fallback the reader can ask for, not a default —
/// they cost most of the picture (two pixels per cell) and that is a worse
/// trade than the problem they solve.
///
/// The problem they solve: a pixel protocol is painted by the terminal
/// EMULATOR, which knows nothing about a multiplexer's panes, so an image
/// can float over whatever is drawn next to it. kitty's graphics go
/// through UNICODE PLACEHOLDERS here (ratatui-image draws them that way),
/// which are anchored to text cells and therefore clip and scroll like
/// text; sixel and iTerm2 placements do not. So the fallback is worth
/// having, and worth being explicit about:
///
/// `COSENSE_IMAGE=halfblocks` — draw pictures as text cells
/// `COSENSE_IMAGE=auto` (default) — the terminal's own protocol
pub(crate) fn image_picker() -> Result<Picker, Box<dyn Error>> {
    use ratatui_image::picker::ProtocolType;
    let mut picker = Picker::from_query_stdio()?;
    let want = std::env::var("COSENSE_IMAGE").unwrap_or_default();
    let forced = match want.as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None, // "auto" or unset: whatever the terminal answered
    };
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    Ok(picker)
}

/// The name of the picture protocol in use, for the status line. Which one
/// is live decides whether pictures clip with the panes around them, so it
/// is worth being able to see without a debugger.
pub(crate) fn image_protocol_name(picker: &Picker) -> &'static str {
    use ratatui_image::picker::ProtocolType;
    match picker.protocol_type() {
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Halfblocks => "halfblocks",
    }
}

/// Turn a decoded image into a sliced protocol at a width-capped cell size./// Turn a decoded image into a sliced protocol at a width-capped cell size.
/// Runs on a worker thread (`Picker` is a plain clone of the terminal's
/// capabilities), never on the UI thread.
pub(crate) fn build_image(
    picker: &Picker,
    dyn_img: image::DynamicImage,
    max_cols: u16,
) -> Result<ImageInfo, String> {
    let font = picker.font_size();
    let (px_w, px_h) = (dyn_img.width(), dyn_img.height());
    let nat_cols = (px_w as f32 / font.width as f32).ceil() as u32;
    // Rows are FLOORED, not rounded up: a picture whose last row is only
    // half-covered ends mid-cell, and text placed on that row — its
    // baseline — then reads as sitting below the picture instead of level
    // with it. Losing a few pixels off the bottom is invisible; the
    // misalignment is not.
    let nat_rows = (px_h as f32 / font.height as f32).floor().max(1.0) as u32;
    let cap = max_cols.max(1) as u32;
    let (cw, ch) = if nat_cols > cap {
        let scale = cap as f32 / nat_cols as f32;
        (cap, ((nat_rows as f32) * scale).floor().max(1.0) as u32)
    } else {
        (nat_cols.max(1), nat_rows.max(1))
    };
    // A picture taller than this owns the screen: the reader scrolls
    // through one image instead of reading a page. Cosense pages are full
    // of tall screenshots, and in a browser they simply take the width
    // they are given — a terminal has to cap the HEIGHT instead, because
    // rows are the scarce direction.
    let (cw, ch) = if ch > MAX_IMAGE_ROWS as u32 {
        let scale = MAX_IMAGE_ROWS as f32 / ch as f32;
        (((cw as f32) * scale).floor().max(1.0) as u32, MAX_IMAGE_ROWS as u32)
    } else {
        (cw, ch)
    };
    let size = Size::new(cw as u16, ch as u16);
    SlicedProtocol::new(picker, dyn_img, Some(size))
        .map(|sliced| {
            let s = sliced.size();
            ImageInfo {
                sliced,
                cells_h: s.height.max(1),
                // The protocol may round down; never report more than asked
                // for, since the cap is what keeps the diagram inside the
                // pane.
                cells_w: s.width.min(max_cols.max(1)),
                built_for: max_cols.max(1),
            }
        })
        .map_err(|e| e.to_string())
}

impl App {
    /// Kick off background downloads for every image on the page. Each
    /// finishes independently and is installed by the event loop, so the page
    /// is readable immediately and images fill in as they arrive.
    pub(crate) fn start_image_loads(&mut self, ctx: &Ctx) {
        // Every picture on the page, INCLUDING the ones inside a mixed
        // text-and-picture line. Reading only `Block::Image` meant a
        // picture written beside text was laid out (its box reserved) and
        // then never fetched — it simply never appeared.
        let urls: Vec<String> = self
            .blocks
            .iter()
            .flat_map(|b| match b {
                Block::Image { url, .. } => vec![url.clone()],
                Block::Inline { parts, .. } => parts
                    .iter()
                    .filter_map(|p| match p {
                        cosense::render::InlinePart::Image(url) => Some(url.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        for url in urls {
            if self.images.contains_key(&url)
                || self.image_errors.contains_key(&url)
                || self.pending.contains(&url)
            {
                continue;
            }
            self.pending.insert(url.clone());
            let tx = self.image_tx.clone();
            let fetcher = Arc::clone(&ctx.fetcher);
            let picker = ctx.picker.clone();
            std::thread::spawn(move || {
                // A page of thirty pictures is thirty requests; fired at
                // once they get the whole site rate-limited (429 on the
                // next page list, measured). A few at a time is as fast as
                // the network lets them be anyway.
                let _slot = IMAGE_SLOTS.acquire();
                // download → decode → resize → protocol-encode, all here
                let res = fetcher
                    .fetch(&url)
                    .map_err(|e| e.to_string())
                    .and_then(|img| build_image(&picker, img, IMAGE_MAX_COLS));
                let _ = tx.send((url, res));
            });
        }
    }

    pub(crate) fn drain_images(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, res)) = self.image_rx.try_recv() {
            self.pending.remove(&url);
            match res {
                Ok(info) => {
                    self.images.insert(url, info);
                }
                Err(e) => {
                    self.image_errors
                        .insert(url.clone(), format!("[image failed: {} — {e}]", short(&url)));
                }
            }
            changed = true;
        }
        changed
    }
}
