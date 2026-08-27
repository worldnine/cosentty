//! Resolve canonical gyazo/scrapbox image permalinks to real bytes, with cache.
//! Mirrors the proven gyazo-skill strategy:
//!   1. Gyazo API (Bearer token) -> `url` field (works for Teams/private).
//!   2. Gyazo oEmbed (no token) -> `url` field (public personal captures).
//!   3. Fallback: scrape og:image from the public page.
//! Scrapbox /files/ URLs are downloaded directly (with SID cookie if present).

use image::DynamicImage;
use std::error::Error;
use std::path::PathBuf;

pub struct ImageFetcher {
    http: reqwest::blocking::Client,
    gyazo_token: Option<String>,
    sid: Option<String>,
    cache_dir: PathBuf,
}

fn parse_gyazo(permalink: &str) -> Option<(Option<String>, String)> {
    // https://<org>.gyazo.com/<id>  or  https://gyazo.com/<id>
    let rest = permalink.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let id: String = path.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if id.len() != 32 {
        return None;
    }
    if host == "gyazo.com" {
        Some((None, id))
    } else if let Some(org) = host.strip_suffix(".gyazo.com") {
        let service = matches!(org, "i" | "t" | "thumb" | "www" | "api" | "upload");
        Some((if service { None } else { Some(org.to_string()) }, id))
    } else {
        None
    }
}

impl ImageFetcher {
    pub fn new(gyazo_token: Option<String>, sid: Option<String>) -> Result<Self, Box<dyn Error>> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("cosense-tui")
            .build()?;
        let cache_dir = dirs_cache().join("cosense-tui").join("images");
        std::fs::create_dir_all(&cache_dir).ok();
        Ok(Self { http, gyazo_token, sid, cache_dir })
    }

    /// Return a decoded image for a permalink, using the on-disk cache.
    pub fn fetch(&self, permalink: &str) -> Result<DynamicImage, Box<dyn Error>> {
        let key = cache_key(permalink);
        let cached = self.cache_dir.join(&key);
        if let Ok(bytes) = std::fs::read(&cached) {
            if !bytes.is_empty() {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    return Ok(img);
                }
            }
        }
        let bytes = self.download(permalink)?;
        // Cache only what actually decodes: a mis-resolved URL (an HTML
        // page, say) must not poison the cache for the next run.
        let img = image::load_from_memory(&bytes)?;
        std::fs::write(&cached, &bytes).ok();
        Ok(img)
    }

    fn download(&self, permalink: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        // Scrapbox uploaded file: direct download (with SID).
        if permalink.contains("scrapbox.io/files/") || permalink.contains("/files/") {
            return self.get_bytes(permalink, true);
        }
        // Gyazo permalink.
        if let Some((org, id)) = parse_gyazo(permalink) {
            let page = match &org {
                Some(o) => format!("https://{o}.gyazo.com/{id}"),
                None => format!("https://gyazo.com/{id}"),
            };
            // 1. API-first when a token is available (Teams / private). A
            //    Teams token does not see personal captures, so a failure
            //    here just falls through.
            if let Some(token) = &self.gyazo_token {
                if let Ok(url) = self.gyazo_api_url(&id, token) {
                    if let Ok(b) = self.get_bytes(&url, false) {
                        return Ok(b);
                    }
                }
            }
            // 2. oEmbed: public, token-free, and returns the ORIGINAL image
            //    URL with the right extension (jpg/png/gif) — the plain
            //    `gyazo.com/<id>` capture case.
            if let Ok(url) = self.gyazo_oembed_url(&page) {
                if let Ok(b) = self.get_bytes(&url, false) {
                    return Ok(b);
                }
            }
            // 3. og:image from the public page (a 1200px thumbnail).
            if let Ok(img_url) = self.og_image(&page) {
                return self.get_bytes(&img_url, false);
            }
            return Err(format!("gyazo fetch failed for {permalink}").into());
        }
        // Anything else: try direct.
        self.get_bytes(permalink, false)
    }

    fn gyazo_api_url(&self, id: &str, token: &str) -> Result<String, Box<dyn Error>> {
        let url = format!("https://api.gyazo.com/api/images/{id}");
        let res = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()?
            .error_for_status()?;
        let v: serde_json::Value = res.json()?;
        v.get("url")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "no url in gyazo api response".into())
    }

    /// Gyazo's oEmbed endpoint: `{"url": "https://i.gyazo.com/<id>.jpg", …}`.
    fn gyazo_oembed_url(&self, page_url: &str) -> Result<String, Box<dyn Error>> {
        let url = format!("https://api.gyazo.com/api/oembed?url={page_url}");
        let res = self.http.get(&url).send()?.error_for_status()?;
        let v: serde_json::Value = res.json()?;
        oembed_image_url(&v).ok_or_else(|| "no url in gyazo oembed response".into())
    }

    fn og_image(&self, page_url: &str) -> Result<String, Box<dyn Error>> {
        let html = self.http.get(page_url).send()?.error_for_status()?.text()?;
        og_image_from_html(&html).ok_or_else(|| "og:image not found".into())
    }

    /// Download an arbitrary URL to `dest` (a Scrapbox upload needs the SID
    /// cookie on private projects, so it is always sent for those).
    pub fn download_to(&self, url: &str, dest: &std::path::Path) -> Result<(), Box<dyn Error>> {
        let with_sid = url.contains("scrapbox.io/");
        let bytes = self.get_bytes(url, with_sid)?;
        std::fs::write(dest, bytes)?;
        Ok(())
    }

    fn get_bytes(&self, url: &str, with_sid: bool) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut req = self.http.get(url);
        if with_sid {
            if let Some(sid) = &self.sid {
                req = req.header("Cookie", format!("connect.sid={sid}"));
            }
        }
        let res = req.send()?.error_for_status()?;
        Ok(res.bytes()?.to_vec())
    }
}

/// The image URL from an oEmbed response (`type: photo` → `url`).
fn oembed_image_url(v: &serde_json::Value) -> Option<String> {
    v.get("url").and_then(|u| u.as_str()).map(str::to_string)
}

/// The `og:image` URL from a page's HTML, whichever order the `<meta>` tag
/// lists its attributes in. Gyazo writes `<meta content="…"
/// property="og:image" />` — content FIRST — so the search is confined to
/// the one tag that carries the property, never a neighbouring tag's
/// content (which used to hand back the og:url page link as the "image").
fn og_image_from_html(html: &str) -> Option<String> {
    let mut rest = html;
    while let Some(lt) = rest.find("<meta") {
        let tag_start = lt;
        let tag_end = rest[tag_start..].find('>').map(|e| tag_start + e + 1)?;
        let tag = &rest[tag_start..tag_end];
        if tag.contains("property=\"og:image\"") || tag.contains("property='og:image'") {
            return attr_value(tag, "content");
        }
        rest = &rest[tag_end..];
    }
    None
}

/// `name="value"` / `name='value'` inside one tag.
fn attr_value(tag: &str, name: &str) -> Option<String> {
    for q in ['"', '\''] {
        let key = format!("{name}={q}");
        if let Some(pos) = tag.find(&key) {
            let start = pos + key.len();
            let end = tag[start..].find(q)?;
            return Some(tag[start..start + end].to_string());
        }
    }
    None
}

fn cache_key(permalink: &str) -> String {
    // stable filename from the permalink
    let mut h: u64 = 1469598103934665603;
    for b in permalink.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}.img")
}

fn dirs_cache() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn og_image_reads_the_tag_whose_content_comes_first() {
        // Gyazo's real markup: og:url precedes og:image, and each tag puts
        // content= before property=. The old window scan returned the
        // og:url page link here.
        let html = r#"<head>
<meta content="https://gyazo.com/1d507226c261ce8cc513e8736ee5070c" property="og:url" />
<meta content="https://i.gyazo.com/thumb/1200/1d507226c261ce8cc513e8736ee5070c-jpg.jpg" property="og:image" />
<meta content="72" property="og:image:width" />
</head>"#;
        assert_eq!(
            og_image_from_html(html).as_deref(),
            Some("https://i.gyazo.com/thumb/1200/1d507226c261ce8cc513e8736ee5070c-jpg.jpg")
        );
        // property-first order and single quotes work too
        let html2 = "<meta property='og:image' content='https://x/y.png'>";
        assert_eq!(og_image_from_html(html2).as_deref(), Some("https://x/y.png"));
        assert_eq!(og_image_from_html("<meta property=\"og:title\" content=\"t\">"), None);
    }

    #[test]
    fn oembed_url_is_the_direct_image() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"version":"1.0","type":"photo","url":"https://i.gyazo.com/abc.jpg","width":72}"#,
        )
        .unwrap();
        assert_eq!(oembed_image_url(&v).as_deref(), Some("https://i.gyazo.com/abc.jpg"));
        assert_eq!(oembed_image_url(&serde_json::json!({"type": "photo"})), None);
    }

    #[test]
    fn gyazo_permalink_forms_parse() {
        let id = "1d507226c261ce8cc513e8736ee5070c";
        assert_eq!(parse_gyazo(&format!("https://gyazo.com/{id}")), Some((None, id.to_string())));
        assert_eq!(
            parse_gyazo(&format!("https://acme.gyazo.com/{id}")),
            Some((Some("acme".to_string()), id.to_string()))
        );
        assert_eq!(parse_gyazo("https://example.com/x"), None);
    }
}
