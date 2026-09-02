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
  `COSENSE_SID` の connect.sid が要るのは **ws push 同期**と **web レンダラ(mmd)**
  だけで、無ければ 3秒ポーリングと「非公開の図は描けない」に縮退する
- 主なフラグ: `--light`/`--dark`/`--theme`、`--preview`、`--ime jp|en`、`--lang`、`--download-dir`
- キー一覧は `rust/KEYMAP.md`(READ で `?` でも引ける)

## コード構成

- lib(`cosense`): `api` `render` `wrap` `theme` `ws` `webrender` `chrome`
  `capability` `outline` `editops` `comment` `index`(一覧の状態・並び順・
  絞り込み/検索の一致) `highlight`(コードブロックの syntect。検索語の
  ハイライトとは別物) ほか
- viewer(`rust/src/bin/view/`): `main`(起動+イベントループ)/ `app`(状態)/
  `keys` / `mouse` / `session`(EDIT)/ `editing`(コミット・undo)/ `outline` /
  `sync`(ws・resync)/ `nav` / `links` / `images` / `web` / `ui`(描画)/
  `tests/`(モジュール対応)

## ビルドとテスト

```bash
cd rust && cargo test --bin view   # viewer(228 tests)
cargo test                          # lib(163)含む全部
```

- ツールチェーンは `rust/` の rustup override で 1.90.0。
  **git worktree では override が効かない**ので `RUSTUP_TOOLCHAIN=1.90.0` を付ける
  (`CARGO_TARGET_DIR=<本体>/rust/target` を足すと依存ビルドを再利用できる)

## ドキュメント索引

- `rust/PLAN-next.md` — **次にやること**(優先順)。まずこれを読む
- `rust/PLAN-view-split.md` — view.rs 分割(実施済み・記録)
- `rust/PLAN-mode-ux.md` — モード体系・カーソル表現の再設計(実施済み・記録)
- `rust/KEYMAP.md` — キー体系(akapen 対応表つき)
- `rust/SPEC-edit-session.md` — EDIT セッションの仕様
- `rust/NOTE-webrender-handoff.md` — web レンダラ(mmd 描画)MVP の詳細設計・
  調査記録(旧 HANDOFF.md の全文)
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
- `rust/scripts/ime.swift` — macOS の入力ソース切替ヘルパ(swiftc でビルド)。
  クリップボード画像のヘルパを足すなら、この経路に乗せる
