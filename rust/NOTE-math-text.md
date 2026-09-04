# NOTE-math-text: 数式の TUI テキスト描画

`code:tex` / `code:latex` のブロックを、端末の文字だけで2次元に組む。
Mermaid のテキスト段(NOTE-mmd-text.md)と同じ骨格を使い、違うのは
**ブラウザ段が無い**ことだけ。

## 方式: `term-maths` に任せる

- 依存: `term-maths = "1.0"`(MIT OR Apache-2.0)。呼ぶのは `render()` のみ
- 依存の依存は `rust-latex-parser` / `unicode-segmentation` / `unicode-width` の3つ
- 自前で持つのは整形・非対応の見分け・幅の見張りだけ
  (`src/bin/view/math_text.rs`)

`mermaid-text` と違い、`RenderedBlock` はセルを `String` で持ち幅を
`unicode-width` で数える。`\frac{速度}{時間}` の全行が同じ表示幅で揃うので、
mmd 側でやった continuation cell の補正は要らない。

## 縮退順(1ブロックあたり)

1. 編集中(edit session がブロック内) → 素のソース(既存契約が勝つ)
2. `COSENSE_MATH=off`(`App::math_text`) → 素のコード行
3. テキスト描画できる式 → `Row::Line`(選択・検索・yank が効く)
4. 描けない → 素のコード行

Mermaid にある「ブラウザ画像」の段が無い。Cosense は KaTeX で数式を描いて
いるが、`#mermaid-preview-<lineId>` のように**要素を実地で確かめていない**。
見当違いの場所を撮るくらいなら、テキストとソースで止める。
`ArtifactKind::web()` が `None` を返すことでその意思を型に載せてある
(`Block::Artifact` は Mermaid と数式で共有する)。

## 入力の整形

`\end{...}` の直前と式の末尾にある `\\` を落とす。行区切りの記号なので、
最後の行に付いていると lib が空の行をもう一段組み、`⎝  ⎠` だけの行が浮く。
Cosense の作例(ヘルプ「数式」)がまさにその書き方なので、素通しにはできない。

## 非対応の見分け

lib は知らない命令をエラーにせず**そのまま吐く**(`\begin{align}` が
`\begin{align}` という字面で出る)。読めない字面を数式のふりで見せるより
コードブロックのほうがましなので、出力にバックスラッシュ＋英字が残って
いたらパース失敗とみなして降りる(`looks_unparsed`)。

`\begin{align}` / `\qquad` は非対応。`pmatrix` / `bmatrix` / `cases` /
`\frac` / `\sqrt` / `\int` / `\sum` / `\lim` / ギリシャ文字 / 上下付きは通る。

## 段: 左端だけ

図は2段まで入れ子にできるが、数式は**0段だけ**(`MATH_MAX_INDENT`)。
箇条書きのインデントが1段でも付いた時点で、コードブロックとしても認識せず、
`code:tex` の行も中身の行もそれぞれ普通のリストの行になる
(mmd の3段目以降と同じ扱い)。境界は `artifact_too_deep` が一箇所で持ち、
`code_span_at` / `code_line_flags` / `render_lines_with` の3つが同じ答えを見る。

## 幅

数式は折り返せない(2次元の組みが崩れる)。ペイン幅に入らなければ描かず、
ソースを見せる。実測ではヘルプ掲載の式はすべて80桁に収まる。

## panic

LaTeX パーサは入力を受ける境界。mmd と同じく `catch_unwind` で囲み、
lib 内の panic を viewer 全体の終了にしない。

## まだやっていない: インライン `[$ ... ]`

いまは記法の外側だけ剥がれて `$ E = mc^2` と出る。段を分けて考える:

- 1行で組める式(`E = mc²`、`α + β ≤ γ`)は、その場の文字列置換で済む。
  `[* 太字]` が装飾を落とすのと同じ層の仕事
- `\frac` のように複数行になる式は、行の高さが変わる。折り返し・選択・
  カーソル列の計算に手が入るので、ブロックとは別の段として扱う

## 検証ページ

`https://scrapbox.io/my-sandbox/数式テスト`(公式ヘルプ「数式」の
対応範囲そのまま)。
