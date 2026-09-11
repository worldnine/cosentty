# cosentty — handoff

Cosense(旧 Scrapbox)のページを端末で読み書きする TUI。本体は `rust/`。

## 起動

```bash
cd rust
cargo run --bin cosentty <project> [title]     # またはページ URL をそのまま渡す
```

- 引数なしは認証ユーザーのプロジェクト一覧。取得できなければ `help-jp` の索引を開く
- 非公開プロジェクトの読み・書き・検索は `cosense login` の PAT / Service Account
  で完結する。保存先は `~/.cosense/settings.json`。解決順序は `rust/KEYMAP.md` の「認証」を参照。
  `COSENSE_SID` の connect.sid が**どうしても要る**のは **ws push 同期**だけ
  (websocket は cookie 認証しか通らない。`NOTE-websocket-sync.md`)。それも
  効くのは**ブラウザ→端末の向き**だけで(未変更なら 3→6→12→30→60 秒に
  伸びるポーリング→サブ秒)、端末で打った
  文字がブラウザに出る向きは PAT だけで入力中からリアルタイムに動く
  (`PLAN-live-edit.md`)。ひとりで書く用途なら sid は要らない、が現在の位置づけ。
  **プロジェクト設定の読み取り**(テーマ・表示名・アップロード先)も sid か
  Service Account が要るが、`,` の設定画面で `config.toml` に書けば代替できる。
  **図をブラウザの画像で出す `diagrams = image`**(既定は `text`)も非公開ページには
  SID を使う。設定画面 `,` か `COSENSE_WEB_RENDER=image` で有効にする。
  無ければ 3 秒から最大 60 秒へ伸びるポーリング・アップロード先 `gcs` に
  縮退する(レンダラを上げて
  いなければ図はテキストかコードで出る)
- 自前の設定ファイルは `~/.config/cosentty/config.toml`(`[view]` `[upload]`
  `[project.<slug>]`。書き方は `rust/KEYMAP.md` の「設定ファイルと設定画面」か
  `rust/src/config.rs` 冒頭)。無くてよい。`,` の設定画面が書く。起動オプションは
  `[view]` にも書け、優先は 起動オプション > 環境変数 > ファイル > 既定
  (`view_settings.rs` の `ViewSettings::resolve`)
- 主なフラグ: `--light`/`--dark`/`--theme`、`--preview`、`--ime jp|en`、`--lang`、`--download-dir`
- キー一覧は `rust/KEYMAP.md`(READ で `?` でも引ける)

## コード構成

- lib(`cosense`): `api` `render` `wrap` `theme` `ws` `webrender` `chrome`
  `capability` `outline` `editops` `comment` `index`(一覧の状態・並び順・
  絞り込み/検索の一致) `highlight`(コードブロックの syntect。検索語の
  ハイライトとは別物) `math`(数式の組図: `code:tex` ブロックとインライン
  `[$ ... ]` の共通部) ほか
- `code:mmd` / `code:tex` は**テキスト描画が本流**。`mmd_text`(viewer側の
  アダプタ)と `rust/docs/NOTE-mmd-text.md` / `rust/docs/NOTE-math-text.md` を参照
- viewer(`rust/src/bin/view/`): `main`(起動+イベントループ)/ `app`(状態)/
  `keys` / `mouse` / `session`(EDIT)/ `editing`(コミット・undo)/ `outline` /
  `sync`(ws・resync)/ `nav` / `links` / `images` / `web` / `ui`(描画)/
  `toast`(一過性の通知バナー)/ `handoff`(コメントをエージェントへ送る: `s`、herdr / `--send-cmd`)/
  `tests/`(モジュール対応)。`ui/` と `session/` は責務別の下位モジュール、
  `tests/app/` は機能をまたぐシナリオを持つ

## ビルドとテスト

```bash
cd rust && cargo test --bin cosentty   # viewer の基本検証
cargo test                        # lib と各 bin も含めた検証
```

worktree には元の `rust/` に設定した rustup override が引き継がれない。
`mermaid-text` が必要とする Rust 1.92 を明示し、ビルド成果物は本体と分ける。
worktree の `rust/` で次のように実行する。

```bash
RUSTUP_TOOLCHAIN=1.92 CARGO_TARGET_DIR="$PWD/target" cargo test --bin cosentty
RUSTUP_TOOLCHAIN=1.92 CARGO_TARGET_DIR="$PWD/target" cargo test
```

同じ target ディレクトリを共有すると、本体と worktree がバイナリを上書きし合う。
前後の比較や実行中の本体への影響を避けるため、共有しない。
`rust/target/` はコードの検索対象からも外す。

実サーバーに接続する WebSocket テストは、通常の `cargo test` では実行しない。
有効な `COSENSE_SID` を設定した環境で、必要なときだけ実行する。

```bash
RUSTUP_TOOLCHAIN=1.92 CARGO_TARGET_DIR="$PWD/target" \
  cargo test --lib ws::tests::ws_sync_publishes_a_post_join_catch_up_snapshot -- --ignored
```

## ドキュメント索引

- `rust/docs/NOTE-codebase-review.md` — 全体レビューの所見、修正記録、モジュール対応表
- `rust/docs/PLAN-next.md` — **次にやること**(優先順)。まずこれを読む
- `rust/docs/PLAN-view-split.md` — view.rs 分割(実施済み・記録)
- `rust/docs/PLAN-mode-ux.md` — モード体系・カーソル表現の再設計(実施済み・記録)
- `rust/KEYMAP.md` — キー体系(akapen 対応表つき)
- `rust/docs/SPEC-edit-session.md` — EDIT セッションの仕様
- `rust/docs/NOTE-webrender-handoff.md` — web レンダラ(mmd 描画)MVP の詳細設計・
  調査記録(旧 HANDOFF.md の全文)
- `rust/docs/NOTE-mmd-text.md` — Mermaid のテキスト描画(本流。ブラウザ描画は
  既定でオフ)
- `rust/docs/NOTE-math-text.md` — 数式のテキスト描画。`code:tex` とインライン数式を扱う
- `rust/docs/NOTE-websocket-sync.md` / `rust/docs/NOTE-outline-editing.md` /
  `rust/docs/NOTE-edit-selection.md` — 各機能の設計メモ
- `rust/docs/SPEC-telomere-web-parity.md` — テロメアの web 仕様(実測)と対応表。
  `scripts/cosense-theme-vars.py` は同梱 app.css からテーマ色テーブルを生成する
- `rust/docs/NOTE-scrapbox-parser.md` — 本家パーサの規則と出典。
  ブロックの子は「ヘッダより深いインデントの行」のみで、空行はブロックを終端する。
  記法の解析や編集の挙動を変える前に読む。
  実装は `render.rs` の3走査と `session/structure.rs` の Enter 処理

## 検証用

- テストページ: 自分の非公開プロジェクトの `テスト`(実編集してよい。文書中の `<sandbox>` はその名前)
- `cargo run --bin ws_smoke -- <project> <title>` — ws push の実測
  (実編集して自動で元に戻す)
- 画面を確認するコマンド: `python3 rust/scripts/tui_shot.py <view のパス> [引数]`
  (要 `pip install pyte`)。pty で起動して端末をエミュレートし、キーを
  送って**実際に描かれた画面**・ハードウェアカーソルの位置・セルの属性
  (太字/前景/背景)を取り出す。テストが緑でも「そう見えるか」は別問題で、
  日本語の桁数・テロメアの太さ・一致の敷き・IME のキャレット位置は
  ここでしか確かめられない。ファイル冒頭に使い方がある
- `rust/scripts/ime.swift` — macOS の入力ソース切替ヘルパ(swiftc でビルド)
- `rust/scripts/pbimage.swift` — macOS のクリップボード画像を PNG に書き出す
  ヘルパ(同じ経路でビルド。`src/clipboard.rs` が埋め込む)
