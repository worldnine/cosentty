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
  `capability` `outline` `editops` `comment` `highlight` ほか
- viewer(`rust/src/bin/view/`): `main`(起動+イベントループ)/ `app`(状態)/
  `keys` / `mouse` / `session`(EDIT)/ `editing`(コミット・undo)/ `outline` /
  `sync`(ws・resync)/ `nav` / `links` / `images` / `web` / `ui`(描画)/
  `tests/`(モジュール対応)

## ビルドとテスト

```bash
cd rust && cargo test --bin view   # viewer(220+ tests)
cargo test                          # lib 含む全部
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
