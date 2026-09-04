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
            | "sankey-beta"
            | "sankey"
            | "block-beta"
            | "block"
            | "packet-beta"
            | "packet"
            | "quadrantChart"
            | "requirementDiagram"
            | "architecture-beta"
            | "architecture"
    )
}

pub(crate) fn text_tier_off() -> bool {
    std::env::var("COSENSE_MERMAID").unwrap_or_default() == "off"
}

/// `COSENSE_MERMAID=ascii`: 罫線なし端末・欠字フォント用。lib の
/// ASCII 変換(`+ - | > < v ^ * o x` のみ)で出す。
pub(crate) fn ascii_mode() -> bool {
    std::env::var("COSENSE_MERMAID").unwrap_or_default() == "ascii"
}

/// lib に描かせる。失敗・幅超過は `None` で縮退せよ。
pub(crate) fn render_text(code: &str, width: usize) -> Option<Vec<String>> {
    if !supported(code) {
        return None;
    }
    let out = if ascii_mode() {
        mermaid_text::render_ascii_with_width(code, Some(width.max(1))).ok()?
    } else {
        mermaid_text::render_with_width(code, Some(width.max(1))).ok()?
    };
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
        for code in [
            "flowchart TB\n A-->B",
            "sequenceDiagram\n A->>B: hi",
            "pie title P\n \"A\" : 1",
            "gantt\n title T\n section S\n A :a1, 2026-01-01, 1d",
            "gitGraph\n commit",
            "classDiagram\n Animal <|-- Dog",
            "erDiagram\n A ||--o{ B : x",
            "journey\n title T\n section S\n A: 5: B",
            "mindmap\n root((R))\n C",
            "timeline\n title T\n S : E",
            "xychart-beta\n title T\n x-axis [a]\n bar [1]",
        ] {
            assert!(render_text(code, 80).is_some(), "{code:?}");
        }
    }

    #[test]
    fn japanese_never_panics_and_fits() {
        // 旧版で panic した入力の回帰網。
        for code in [
            "sequenceDiagram\n Alice->>Bob: こんにちは\n loop 毎分\n Alice->>Bob: Ping\n end",
            "flowchart TB\n A[開始]-->B{判断?}",
            "pie title 予算\n \"開発\" : 1",
        ] {
            let lines = render_text(code, 60).expect(code);
            for l in &lines {
                assert!(str_width(l) <= 60, "too wide: {l}");
            }
        }
    }

    #[test]
    fn ascii_mode_drops_box_glyphs() {
        // 環境変数に触らず lib の ASCII 変換だけ確かめる(並列テストのため)。
        // ラベル(日本語)は残り、罫線・塗り・矢頭だけ ASCII になる。
        let out = mermaid_text::render_ascii_with_width(
            "flowchart TB\n A[開始]-->B{判断?}",
            Some(60),
        )
        .expect("ascii renders");
        for risky in ['┌', '─', '│', '░', '▸', '═', '┆', '╔'] {
            assert!(!out.contains(risky), "{risky} left in: {out}");
        }
        // lib は CJK に字間を空ける流儀なので文字単位で見る。
        assert!(out.contains("開") && out.contains("始"), "label stays: {out}");
    }

    #[test]
    fn unknown_types_decline() {
        assert_eq!(render_text("foobar\n x", 60), None);
        assert_eq!(render_text("", 60), None);
    }
}
