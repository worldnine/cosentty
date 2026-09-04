# NOTE-mmd-text: Mermaid の TUI テキスト描画(lib 方式)

正本。ブラウザ描画(`webrender` + headless Chrome)を置き換えるのではなく、
その手前に「テキスト描画」の段を足す。描けなければ今まで通り
画像 → 素のコード行に落ちる。`master` へのマージは完成・チェック後に
まとめて行う(途中マージなし)。

## 方式: `mermaid-text` に任せる

自前描画の分支(mmd-text-render)との見比べの末、lib 採用に決定。
理由: 25型カバー・幅 compaction・A* 配線を自前で追うより安い。
代償: toolchain 1.90 → 1.92 の bump(本 NOTE 末尾)。

- 依存: `mermaid-text = "0.57"`(MIT)。呼ぶのは `render_with_width` のみ
- 自前で持つのは段・旗・縮退だけ(`src/bin/view/mmd_text.rs`):
  型ゲート → lib 描画 → 行分割 → 幅超過チェック → `None` で縮退
- lib が Err の型・未知の型は描かない(画像 → コード行へ)

## 縮退順(1ブロックあたり)

1. 編集中(edit session がブロック内) → 素のソース(既存契約が勝つ)
2. `COSENSE_MERMAID=off`(`App::mermaid_text`) → 既存の経路(画像 → コード行)。
   `=ascii` は罫線なし描画(欠字フォント用、段自体は残る)
3. テキスト描画できる型 → lib 出力の `Row::Line`(選択・検索・yank 可)
4. 画像 artifact あり → `Row::Image`(既存)
5. どれもなし → 素のコード行(既存)

テキストが画像より先。理由: 即時・オフライン・SSH/tmux 安全、
リサイズ再レンダリング(3〜6s)なし、テキストとして残る。

## 対応表(lib 0.57 の行列表準拠)

flowchart・sequence・pie・er・gantt・gitGraph・class・journey・
mindmap・timeline・xychart・sankey・block・packet・quadrant・
requirement・architecture。state 系は lib が Err なら縮退。
sequence 箱は `░` 塗りになるのは lib の味。CJK の字間開きと罫線ずれは
lib の `Grid(Vec<char>)` が全角の continuation cell までserializeする不具合だった。
flowchart・sequence・state は `remove_wide_continuation_cells` で出力後に補正する。
補正前 `開 始` / `[成═功═]` → 補正後 `開始` / `[成功]`。`classDiagram`
などlib内panicもあるため、呼び出し境界を `catch_unwind` し、失敗時は画像へ縮退する。
Cosense webに合わせ、Mermaidブロックは先頭空白0〜2個まで図として扱う。
1・2段目は図全体を `text_column(level)` だけ右へ送るが、READではビュレットを
描かない。EDITでソースへ戻した間だけ、`code:` ヘッダーにビュレットを置く
（caretがヘッダーでも本体でも同じ）。本体のコード行には置かない。
3段目以降は `code:` ヘッダーも本体もコードブロックとして消費せず、各行自身の
空白数どおりの通常リストに戻す。`code_span_at` / `code_line_flags` も同じ境界を
使う。画像縮退側の `Row::Image` も同じインデントを使い、`item: false` とする。
また、空行の先に別の `code:` ヘッダーがある場合は、後者がより深い段でも前の
コード本文へ吸収しない。空行なしの `code:` は従来どおり本文になり、コード内の
空行はインデント付き空行で表す。

## toolchain bump(1.90.0 → 1.92)

`mermaid-text@0.57`(と `ascii-dag`)が rustc 1.92 を要求するため。
手順:

- `rustup override set 1.92-...` (本体 `rust/`)。worktree では
  `RUSTUP_TOOLCHAIN=1.92`。1.90 系と成果物を共有しないよう
  `CARGO_TARGET_DIR` は本体と分ける(worktree 既定の `rust/target` 等)
- 本分支の `CLAUDE.md` も 1.92 に書き換え済み。merge 時に本体へ反映
- `cargo test --bin view` + `cargo test` が緑なのを確認

## 旧分支

自前描画の `mmd-text-render` ブランチは残す(設計記録・見比べ用)。
merge しない。

## 終了条件

- `cargo test --bin view` green、`cargo test --lib` green(1.92)
- `tui_shot.py` で flowchart・sequence・pie の実画面を確認
- KEYMAP・`?`ヘルプ・`t!()` 両言語の追随(振る舞いが変わるもののみ)
- `my-sandbox/テスト` で実編集したら元に戻す
