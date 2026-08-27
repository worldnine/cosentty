# ぶら下げインデント折り返し — akapen への移植メモ

cosense-tui で入れた「箇条書きの継続行をマーカーの右に揃える」折り返しの要点。
akapen（`highlight::wrap_spans` ＋ tui-markdown の出力）に同じものを入れるときの手引き。

## 何をするか

```
現状                                     ぶら下げ
  • TAOのIDが重複していないかのチェック    • TAOのIDが重複していないかのチェック
文字列の抽出機能                             文字列の抽出機能
```

1 行目は全幅で折り返し、2 行目以降は **前置 span 列（prefix）を付けて** `width - prefix幅` で
折り返す。prefix は行頭の空白 ＋ マーカーの扱い:

- 箇条書き `• ` → 同じ幅の**空白**（本文の 1 文字目の下に揃う）
- 引用 `┃ ` → **同じスタイルの `┃ ` を繰り返す**（折り返された引用が 1 ブロックに見える）

```
┃ 引用の一行目がここで折り返されると
┃ 二行目にも縦棒が続く
```

## アルゴリズム（`src/wrap.rs`）

```rust
pub fn wrap_line_continued(line: &Line, width: usize, prefix: &[Span]) -> Vec<Line> {
    // hang = prefix の表示幅。継続行の幅が MIN_HANGING_BODY (8) を切るなら hang = 0
    // (char, Style) に平坦化 → 1 文字ずつ幅を測って行を切る
    //   limit = if rows.is_empty() { width } else { width - hang }
    // 出来た行を同じ Style の連続で span に再結合し、
    // 2 行目以降の先頭に prefix の span 列を差し込む
}

pub fn hanging_prefix(line: &Line) -> Vec<Span> {
    // 行頭の ' ' を数えて Span::raw(空白)。次の 1 文字が HANG_MARKERS でその次が ' ' なら
    //   ('•', repeat=false) → 同じ幅の空白
    //   ('┃', repeat=true)  → そのマーカー span をスタイルごとコピー（"┃ "）
    // 描画済みの行しか受け取らないので、インデントは ASCII 空白に正規化済み。
}
```

`wrap_line(line, width)` は prefix なしの別名、`wrap_line_hanging(line, width, hang)` は
空白 prefix の別名。呼び出し側は `wrap_line_continued(line, w, &hanging_prefix(line))` の
1 行でよい。

## akapen で違うところ

- **マーカーの判定**: tui-markdown が出すリスト行の先頭は `• ` ではなく `- ` や `1. `
  のことがある（`StyleSheet::list_marker` の設定次第）。`HANG_MARKERS` に akapen が実際に
  使う文字を入れる。番号付きリストは `"12. "` のように可変幅なので、「数字の連続 + `.` + 空白」を
  1 パターンとして足す。
- **引用**: tui-markdown の blockquote は先頭に `│ ` 等の縦棒が付く。`HANG_MARKERS` に
  `('│', true)` として入れれば、継続行にも同じスタイルの縦棒が繰り返される。
- **入れ子の測り方**: tui-markdown はネストを空白インデントで出すので、cosense-tui と同じく
  「行頭空白の幅」で拾える。
- **wrap_spans の位置**: akapen は `highlight::wrap_spans(spans, width)` を view 生成
  （`ViewState::render`）と source 側で共用している。ぶら下げは **view 側だけ**に入れる
  （source は生 Markdown なので揃えない）。
- **row_segments / source_starts**: 継続行に空白 span を差し込んでも、行数と行→ソース行の
  対応は変わらない（1 論理行 → N 表示行のまま）。マウスの `line_at_position` は列で
  セグメントを引くので、前置した空白の分だけ列がずれる点に注意（空白 span は
  セグメントに入れない or 幅を足す）。

## テスト（移植時にそのまま持っていける）

- `hanging_indent`: 平文 0 / `• x` 2 / `    • x` 6 / `┃ x` 2 / `    code` 4 / `•x`（空白なし）0
- `wrap_line_hanging("  • abcdefghijkl", 10, 4)` → `["  • abcdef", "    ghijkl"]`
- CJK: `"• あいうえおかきくけこ"` 幅 10、hang 2 → `["• あいうえ", "  おかきく", "  けこ"]`、
  各行の表示幅 ≤ 10、先頭空白を除いて結合すると元の文字列
- 幅の下限: 幅 12・hang 6 → 継続幅 6 < 8 なので通常折り返しに落ちる
- 引用: `"  ┃ 長い引用…"` 幅 20 → 全行が `"  ┃ "` で始まり、`┃` の span は元のスタイルを保つ
- 統合: 折り返された箇条書きは 1 つのカーソル停止位置のまま（表示行数 = cursor_rows の幅）

## 落とし穴

- ゼロ幅文字（結合文字・VS16）は `width() == 0` として行送りの判定から除外する
  （既存の wrap と同じ）。
- `hang >= width` になる極端な幅では必ず 0 に落とす（`saturating_sub` で保護）。
- テストの寸法に注意: 幅 10・hang 4 のように「継続行 < 8 列」になる値を選ぶと
  下限に当たって通常折り返しに落ち、期待値と食い違う（実際に一度踏んだ）。
