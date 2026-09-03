//! mmd テキスト描画の lib 検証 spike(mmd-lib-spike)。
//!
//! 手書き分支(mmd-text-render)と見比べるための最小配線。
//! 描画本体は `mermaid-text` に任せ、段・旗・縮退だけ持つ。

use crate::session::str_width;

/// lib に任せる型。未知は縮退(画像 → コード行)。
fn supported(code: &str) -> bool {
    let first = code
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    matches!(
        first.split_whitespace().next().unwrap_or(""),
        "flowchart"
            | "graph"
            | "sequenceDiagram"
            | "pie"
            | "gantt"
            | "gitGraph"
            | "classDiagram"
            | "stateDiagram"
            | "stateDiagram-v2"
            | "erDiagram"
            | "journey"
            | "mindmap"
            | "timeline"
            | "xychart-beta"
            | "xychart"
    )
}

pub(crate) fn text_tier_off() -> bool {
    std::env::var("COSENSE_MERMAID").unwrap_or_default() == "off"
}

/// lib に描かせる。失敗・幅超過は `None` で縮退せよ。
pub(crate) fn render_text(code: &str, width: usize) -> Option<Vec<String>> {
    if !supported(code) {
        return None;
    }
    let out = mermaid_text::render_with_width(code, Some(width.max(1))).ok()?;
    let lines: Vec<String> = out.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return None;
    }
    // lib の compaction が効かなかった分は欠けより縮退。
    if lines.iter().any(|l| str_width(l) > width.max(1)) {
        return None;
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_go_to_the_lib() {
        assert!(render_text("flowchart TB\n A-->B", 60).is_some());
        assert!(render_text("sequenceDiagram\n A->>B: hi", 60).is_some());
        assert!(render_text("pie title P\n \"A\" : 1", 60).is_some());
        assert!(render_text("gantt\n title T", 60).is_some());
    }

    #[test]
    fn unknown_types_decline() {
        assert_eq!(render_text("foobar\n x", 60), None);
        assert_eq!(render_text("", 60), None);
    }
}
