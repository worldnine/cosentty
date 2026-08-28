# cosense-tui

Scrapbox / Cosense の TUI ビューア＋編集クライアント。

**現行実装は `rust/`（ratatui）。** 以下の TypeScript 版 (v0) は初期スパイク。

```bash
cd rust && cargo run --bin view -- <project> [ページタイトル]
cargo run --bin view -- https://scrapbox.io/help-jp/リンク   # URL 直貼りも可
```

## rust 版の主な機能

- **認証**: 公式 CLI（`cosense login`）の `~/.cosense/settings.json` を自動で使う
  （PAT / Service Account。`COSENSE_SID` はフォールバック）。非公開プロジェクト対応
- **関連ページリスト**: 本文枠を閉じた下側へ Links（1-hop）/ リンクごとの 2 hop グループ /
  External links を描画。`G` は本文枠末尾、そこから j/k → Enter で辿れる。各リンクのガターには
  このTUIでの既読・未読を2状態のテロメアで表示する（追加リクエストなし）
- **モードレス編集**: cosense web 同様のビュー内直接編集。`e`/`i`/`o`/ダブルクリックで
  プロジェクトのメンバーだけがセッションを開始できる。開始すると本文枠がアクセント色になり、
  キャレット行だけ生ソース表示、↑↓で何行でも連続編集、Enter で行追加。コミットは自動（行離脱時・直列キュー）、確認ゲートなし、安全網は `u`/`^r` の undo/redo。
  同時編集は 409 で検出し自動復旧（書いた文章は失われない）。`^e` で $EDITOR 全文編集
- **タイムマシン**: `←`/`→` で Page history（サーバーサイド snapshot）を行き来できる。
  過去版でも行単位の blame（`t`）が動く
- **akapen 互換の読書体験**: 行カーソル / 範囲選択 / インラインコメントカード /
  view⇄source トグル / テーマ連動 / テロメア（未読ハイライト）/ 画像インライン表示。
  テーブルは 1行=1ソース行でカーソルが効く

キーマップの詳細は [rust/KEYMAP.md](rust/KEYMAP.md)。

---

# TypeScript 版 (v0)

Scrapbox / Cosense を閲覧するための TUI ビューア。読み取り専用。

## 設計

- **データ層**: Scrapbox REST API を直接叩く（`src/api.ts`）。公開プロジェクトは認証不要、非公開は `connect.sid` を渡す。MCP/CLI を挟まないのでレイテンシが低く、`lines[]` の生記法をそのまま受け取れる。
- **レンダリング層**: Scrapbox 記法 → ANSI の自前レンダラ（`src/render.ts`）。行単位・インデント=ネストという Scrapbox の単純な構造に対応。太字・リンク・URL・ハッシュタグ・引用・コードブロックをサポート。将来 Rust に移植しても設計を流用できるよう独立させてある。
- **UI 層**: Ink（React for CLI）。一覧 / ページ表示 / 検索の3画面（`src/app.tsx`）。
- **画像**: v1 送り。gyazo URL の抽出だけ実装済み（`RenderedLine.gyazo`）。表示は kitty/iTerm2 プロトコルか、Rust 移植時に ratatui-image で対応予定。

## 使い方

```bash
npm install

# 公開プロジェクト
npx tsx src/cli.tsx help-jp
# または
COSENSE_PROJECT_NAME=help-jp npm start

# 非公開プロジェクト
npx tsx src/cli.tsx myproject --sid "s:xxxxx"
# または COSENSE_SID 環境変数
```

## キー操作

**一覧**: `j/k` 移動  `↵` 開く  `/` 検索  `r` 再読込  `q` 終了
**ページ**: `j/k` スクロール  `Space` ページ送り  `g/G` 先頭/末尾  `1-9` リンクを開く  `h`/`⌫` 戻る  `q` 終了
**検索**: 入力して `↵` で実行、結果を `j/k`＋`↵` で開く

## 既知の制約 / 次の一手

- `--json` ではなく REST 直なので構造化データはクリーン。
- 検索は API 仕様で最大 100 件。
- 画像表示は未実装（v1）。gyazo は本文中の URL として検出済み。
- v1 で本格的な画像を狙うなら Rust + ratatui + ratatui-image への移植を検討（レンダラの記法解釈ロジックはそのまま移せる）。
