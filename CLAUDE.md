# cosense-tui

Cosense(旧 Scrapbox)の TUI ビューワ/エディタ。本体は `rust/`(lib 名は `cosense`)。
全体像とドキュメント索引は `HANDOFF.md`、次のタスクは `rust/PLAN-next.md`。

## ビルド・テスト

- `cd rust && cargo test --bin view` が基本の検証。lib 含む全部は `cargo test`
- ツールチェーンは `rust/` の rustup override(1.90.0)。**git worktree では
  override が効かず 1.89 で依存解決に失敗する**。worktree では
  `RUSTUP_TOOLCHAIN=1.90.0` を付け、`CARGO_TARGET_DIR=<本体>/rust/target` で
  依存ビルドを再利用する
- **同じ target dir を共有するとバイナリは残らない**。worktree と本体が
  `target/debug/view` を上書きし合い、`cargo build` は再リンクを省くことがある。
  前後のビルドを比べるなら `CARGO_TARGET_DIR` を分けるか、`md5 -q` で確かめる
- `rust/target/` は検索・読み込みの対象にしない

## コードの歩き方

- viewer は `rust/src/bin/view/` 配下に関心事別で分割済み
  (`app` 状態 / `keys` / `mouse` / `session` EDIT / `editing` / `outline` /
  `sync` / `nav` / `links` / `images` / `web` / `ui` / `toast` / `handoff` / `tests/`)。
  巨大ファイルはもう無いので、まず該当モジュールを開けばよい
- 挙動を変えたら `rust/KEYMAP.md` とヘルプ文言(`ui.rs` の Overlay::Help)も追随させる
- UI の文言は `t!("日本語", "english")` で両言語を並べる。キー名・フラグ・
  記法・製品名は翻訳しない(view/main.rs 冒頭のコメント参照)

## 作業の約束

- 変更は worktree を切って行い、テスト green を確認してから master へ
  ff マージする(1関心事 = 1コミット、コミットメッセージは日本語)
- 実験・検証には `my-sandbox/テスト` ページを使ってよい(実編集OK。
  終わったら元に戻す)。非公開プロジェクトの認証は `cosense login` の PAT で足りる。
  `COSENSE_SID` が要るのは ws push 同期・web レンダラ(mmd)・プロジェクト設定の
  読み取り(テーマ・アップロード先)だけ
- 設計判断は PLAN-*.md / NOTE-*.md / SPEC-*.md に書き残す文化。実施した
  計画には「実施記録」を追記する
