# cosense-tui — handoff

Cosense(旧 Scrapbox)のページを端末で読み書きする TUI。本体は `rust/`。

## 起動

```bash
cd rust
cargo run --bin view <project> [title]     # またはページ URL をそのまま渡す
```

- 引数なしは `help-jp` の索引
- 非公開プロジェクトの読み・書き・検索は `cosense login` の PAT / Service Account
  で完結する(`~/.cosense/settings.json`。解決順序は `rust/KEYMAP.md` の「認証」)。
  `COSENSE_SID` の connect.sid が要るのは **ws push 同期**と**プロジェクト設定の
  読み取り**(テーマ、画像のアップロード先)、それと**既定でオフの web レンダラ**
  (`COSENSE_WEB_RENDER=manual|auto` で上げたときの非公開ページ描画)だけ。
  無ければ 3秒ポーリング・アップロード先 `gcs` に縮退する(レンダラを上げて
  いなければ図はテキストかコードで出る)
- 自前の設定ファイルは `~/.config/cosense-tui/config.toml`(画像のアップロード先の
  上書き。書き方は `rust/src/config.rs` 冒頭)。無くてよい
- 主なフラグ: `--light`/`--dark`/`--theme`、`--preview`、`--ime jp|en`、`--lang`、`--download-dir`
- キー一覧は `rust/KEYMAP.md`(READ で `?` でも引ける)

## コード構成

- lib(`cosense`): `api` `render` `wrap` `theme` `ws` `webrender` `chrome`
  `capability` `outline` `editops` `comment` `index`(一覧の状態・並び順・
  絞り込み/検索の一致) `highlight`(コードブロックの syntect。検索語の
  ハイライトとは別物) `math`(数式の組図: `code:tex` ブロックとインライン
  `[$ ... ]` の共通部) ほか
- `code:mmd` / `code:tex` は**テキスト描画が本流**。`mmd_text`(viewer側の
  アダプタ)と `NOTE-mmd-text.md` / `NOTE-math-text.md` を参照
- viewer(`rust/src/bin/view/`): `main`(起動+イベントループ)/ `app`(状態)/
  `keys` / `mouse` / `session`(EDIT)/ `editing`(コミット・undo)/ `outline` /
  `sync`(ws・resync)/ `nav` / `links` / `images` / `web` / `ui`(描画)/
  `toast`(一過性の通知バナー)/ `handoff`(コメントをエージェントへ送る: `s`、herdr / `--send-cmd`)/
  `tests/`(モジュール対応)

## ビルドとテスト

```bash
cd rust && cargo test --bin view   # viewer(336 tests)
cargo test                          # lib(197)含む全部
```

- ツールチェーンは `rust/` の rustup override で 1.90.0。
  **git worktree では override が効かない**ので `RUSTUP_TOOLCHAIN=1.90.0` を付ける
  (`CARGO_TARGET_DIR=<本体>/rust/target` を足すと依存ビルドを再利用できる)
- ただし**同じ target dir を共有するとバイナリは残らない**。worktree と本体が
  `target/debug/view` を上書きし、`cargo build` は再リンクを省いて
  `Finished in 0.79s` と出すことがある(実測。修正前の計測が修正後の
  バイナリを走らせていた)。修正の前後を画面で比べるなら `CARGO_TARGET_DIR`
  を分けるか、ビルドごとに `md5 -q target/debug/view` を取って同一性を確かめる

## ドキュメント索引

- `rust/PLAN-next.md` — **次にやること**(優先順)。まずこれを読む
- `rust/PLAN-view-split.md` — view.rs 分割(実施済み・記録)
- `rust/PLAN-mode-ux.md` — モード体系・カーソル表現の再設計(実施済み・記録)
- `rust/KEYMAP.md` — キー体系(akapen 対応表つき)
- `rust/SPEC-edit-session.md` — EDIT セッションの仕様
- `rust/NOTE-webrender-handoff.md` — web レンダラ(mmd 描画)MVP の詳細設計・
  調査記録(旧 HANDOFF.md の全文)
- `rust/NOTE-mmd-text.md` — Mermaid のテキスト描画(本流。ブラウザ描画は
  既定でオフ)
- `rust/NOTE-math-text.md` — 数式のテキスト描画(`code:tex` とインライン
  `[$ ... ]`)
- `rust/NOTE-websocket-sync.md` / `NOTE-outline-editing.md` /
  `NOTE-edit-selection.md` — 各機能の設計メモ

## 検証用

- テストページ: `my-sandbox/テスト`(実編集してよい)
- `cargo run --bin ws_smoke -- <project> <title>` — ws push の実測
  (実編集して自動で元に戻す)
- **画面そのものを読む**: `python3 rust/scripts/tui_shot.py <view のパス> [引数]`
  (要 `pip install pyte`)。pty で起動して端末をエミュレートし、キーを
  送って**実際に描かれた画面**・ハードウェアカーソルの位置・セルの属性
  (太字/前景/背景)を取り出す。テストが緑でも「そう見えるか」は別問題で、
  日本語の桁数・テロメアの太さ・一致の敷き・IME のキャレット位置は
  ここでしか確かめられない。ファイル冒頭に使い方がある
- `rust/scripts/ime.swift` — macOS の入力ソース切替ヘルパ(swiftc でビルド)
- `rust/scripts/pbimage.swift` — macOS のクリップボード画像を PNG に書き出す
  ヘルパ(同じ経路でビルド。`src/clipboard.rs` が埋め込む)
