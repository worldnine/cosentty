//! URL component encoding and decoding shared by API, navigation and uploads.

/// Encode one path or query component, preserving RFC 3986 unreserved bytes.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(out, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    out
}

/// Decode UTF-8 percent escapes. Keep malformed escapes and literal `+` intact;
/// invalid decoded UTF-8 is replaced, just as in browser-pasted titles.
pub fn percent_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_round_trip_without_changing_delimiters() {
        for text in ["日本語 /?#%+", "a-z_A.Z~0", "", "🙂"] {
            assert_eq!(percent_decode(&encode_component(text)), text);
        }
        assert_eq!(encode_component("a/b c?"), "a%2Fb%20c%3F");
        assert_eq!(percent_decode("%e6%97%a5+%20"), "日+ ");
    }

    #[test]
    fn malformed_escapes_never_slice_inside_unicode() {
        for text in ["%日本語", "%a日", "%🙂", "%", "%2", "%GG", "日本%語"] {
            assert_eq!(percent_decode(text), text);
        }
        assert_eq!(percent_decode("%FF"), "�");
    }
}
