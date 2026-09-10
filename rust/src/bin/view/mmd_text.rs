//! mmd テキスト描画の lib 検証 spike(mmd-lib-spike)。
//!
//! 手書き分支(mmd-text-render)と見比べるための最小配線。
//! 描画本体は `mermaid-text` に任せ、段・旗・縮退だけ持つ。

use crate::session::str_width;

/// 先頭の図型。判定とCJK補正で同じ値を使う。
fn diagram_word(code: &str) -> &str {
    code.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
}

/// lib に任せる型。未知は縮退(画像 → コード行)。
fn supported(code: &str) -> bool {
    matches!(
        diagram_word(code),
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

/// テキスト段の字種。`[view] diagram_text` / `COSENSE_MERMAID=ascii`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DiagramText {
    /// 罫線グリフ(`┌ ─ │ ▸`)。既定。
    #[default]
    Box,
    /// 罫線なし端末・欠字フォント用。lib の ASCII 変換(`+ - | > < v ^ * o x` のみ)。
    Ascii,
}

impl DiagramText {
    /// `box` / `ascii`; `None` for anything else.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "box" => Some(DiagramText::Box),
            "ascii" => Some(DiagramText::Ascii),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DiagramText::Box => "box",
            DiagramText::Ascii => "ascii",
        }
    }
}

/// `COSENSE_MERMAID=ascii`: 環境変数だけの旧い指定。設定の解決で Env として拾う。
pub(crate) fn ascii_from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<DiagramText> {
    (env("COSENSE_MERMAID").as_deref() == Some("ascii")).then_some(DiagramText::Ascii)
}

/// lib の `Grid` は表示セルを `Vec<char>` で持つ。CJK文字は幅2として
/// xを2進める一方、空けた2セル目もserializeしてしまうため、
/// `開始` が `開 始` になり、右罫線も1文字ごとに1列ずれる。
/// flowchart/sequence は全行がこのGrid由来なので、幅2文字の直後の
/// continuation cellを落として本来の端末セル列へ戻す。
fn remove_wide_continuation_cells(rendered: &str) -> String {
    let mut out = String::with_capacity(rendered.len());
    for chunk in rendered.split_inclusive('\n') {
        let (line, newline) = match chunk.strip_suffix('\n') {
            Some(line) => (line, true),
            None => (chunk, false),
        };
        let mut chars = line.chars();
        while let Some(ch) = chars.next() {
            out.push(ch);
            if unicode_width::UnicodeWidthChar::width(ch) == Some(2) {
                // 行末ではcontinuation cellがtrim済みのこともある。
                let _ = chars.next();
            }
        }
        if newline {
            out.push('\n');
        }
    }
    out
}

fn uses_char_grid(code: &str) -> bool {
    matches!(
        diagram_word(code),
        "flowchart" | "graph" | "sequenceDiagram" | "stateDiagram" | "stateDiagram-v2"
    )
}

/// テキスト段の結果。幅不足は理由を持って返し、呼び出し側が注記を出せるようにする。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TextOutcome {
    /// 描けた行。
    Drawn(Vec<String>),
    /// 描けたが指定幅に入らない。`needed` は lib が出した図の最大幅(桁)。
    TooNarrow { needed: usize },
    /// 未知の型・lib の Err・panic。理由は言わず黙って縮退する。
    Declined,
}

/// lib に描かせる。失敗・panic・幅超過は `None` で縮退せよ。
pub(crate) fn render_text(code: &str, width: usize, glyphs: DiagramText) -> Option<Vec<String>> {
    match render_text_outcome(code, width, glyphs) {
        TextOutcome::Drawn(lines) => Some(lines),
        _ => None,
    }
}

/// `render_text` の理由付き版。幅不足だけは「何桁あれば描けたか」を添える。
pub(crate) fn render_text_outcome(code: &str, width: usize, glyphs: DiagramText) -> TextOutcome {
    if !supported(code) {
        return TextOutcome::Declined;
    }
    // Mermaidパーサは入力を受ける境界。既知のCJK classDiagramを含め、
    // lib内panicをviewer全体の終了にしない。
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if glyphs == DiagramText::Ascii {
            mermaid_text::render_ascii_with_width(code, Some(width.max(1)))
        } else {
            mermaid_text::render_with_width(code, Some(width.max(1)))
        }
    }));
    let rendered = match rendered {
        Ok(Ok(r)) => r,
        _ => return TextOutcome::Declined,
    };
    let out = if uses_char_grid(code) {
        remove_wide_continuation_cells(&rendered)
    } else {
        rendered
    };
    let lines: Vec<String> = out.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return TextOutcome::Declined;
    }
    // lib の compaction が効かなかった分は欠けより縮退。ただし理由は残す:
    // subgraph の多い flowchart は lib が横並びに置くため 100 桁超になりがちで、
    // 黙って落とすと「なぜ出ないか」が読者に伝わらない。
    let needed = lines.iter().map(|l| str_width(l)).max().unwrap_or(0);
    if needed > width.max(1) {
        return TextOutcome::TooNarrow { needed };
    }
    TextOutcome::Drawn(lines)
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
            assert!(render_text(code, 80, DiagramText::Box).is_some(), "{code:?}");
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
            let lines = render_text(code, 60, DiagramText::Box).expect(code);
            for l in &lines {
                assert!(str_width(l) <= 60, "too wide: {l}");
            }
        }
    }

    #[test]
    fn cjk_grid_continuation_cells_are_removed() {
        assert_eq!(
            remove_wide_continuation_cells("│ 開 始  │\n╔═[alt]══[成═功═]══╗"),
            "│ 開始 │\n╔═[alt]══[成功]══╗"
        );
        let out = render_text("flowchart TB\n A[開始]-->B{判断?}", 60, DiagramText::Box).expect("flowchart draws");
        let joined = out.join("\n");
        assert!(joined.contains("開始"), "phantom cell remains: {joined}");
        assert!(!joined.contains("開 始"), "phantom cell remains: {joined}");
        let top = out.iter().find(|l| l.contains('┌')).expect("top");
        let middle = out.iter().find(|l| l.contains("開始")).expect("middle");
        assert_eq!(str_width(top), str_width(middle), "box sides align");
    }

    #[test]
    fn sequence_cjk_block_fill_is_not_serialized_as_text() {
        let out = render_text(
            "sequenceDiagram\n A->>B: 要件定義書\n alt 成功\n B->>A: 承認\n end",
            60,
            DiagramText::Box,
        )
        .expect("sequence draws")
        .join("\n");
        for want in ["要件定義書", "成功", "承認"] {
            assert!(out.contains(want), "{want}: {out}");
        }
        assert!(!out.contains("成═功") && !out.contains("承░認"), "{out}");
    }

    #[test]
    fn ascii_mode_drops_box_glyphs() {
        // 環境変数に触らず lib の ASCII 変換だけ確かめる(並列テストのため)。
        // ラベル(日本語)は残り、罫線・塗り・矢頭だけ ASCII になる。
        let out =
            mermaid_text::render_ascii_with_width("flowchart TB\n A[開始]-->B{判断?}", Some(60))
                .expect("ascii renders");
        for risky in ['┌', '─', '│', '░', '▸', '═', '┆', '╔'] {
            assert!(!out.contains(risky), "{risky} left in: {out}");
        }
        // lib は CJK に字間を空ける流儀なので文字単位で見る。
        assert!(
            out.contains("開") && out.contains("始"),
            "label stays: {out}"
        );
    }

    #[test]
    fn too_narrow_reports_needed_width() {
        // 3 ノード横並びは 20 桁には入らない。理由と必要幅が返る。
        let code = "flowchart LR\n A[開始する]-->B[判断する]-->C[終了する]";
        match render_text_outcome(code, 20, DiagramText::Box) {
            TextOutcome::TooNarrow { needed } => assert!(needed > 20, "{needed}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(render_text(code, 20, DiagramText::Box), None);
        // 十分な幅なら描ける。
        assert!(matches!(
            render_text_outcome(code, 200, DiagramText::Box),
            TextOutcome::Drawn(_)
        ));
        // 未知の型は幅不足ではなく Declined。
        assert_eq!(render_text_outcome("foobar\n x", 20, DiagramText::Box), TextOutcome::Declined);
    }

    #[test]
    fn unknown_types_decline() {
        assert_eq!(render_text("foobar\n x", 60, DiagramText::Box), None);
        assert_eq!(render_text("", 60, DiagramText::Box), None);
    }
}
