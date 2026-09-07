# cosense-tui

Cosense(旧 Scrapbox)の TUI ビューワ/エディタ。本体は `rust/`。lib 名は `cosense`。
全体像とドキュメント索引は `HANDOFF.md`、次のタスクは `rust/PLAN-next.md`。

## ビルド・テスト

- `cd rust && cargo test --bin view` が基本の検証。lib 含む全部は `cargo test`
- ツールチェーンは `rust/` の rustup override(1.92: `mermaid-text` が要求)。
  **git worktree では
override が効かず古い系で依存解決に失敗する**。worktree では
  `RUSTUP_TOOLCHAIN=1.92` を付け、1.90 系の成果物と混ざらないよう
  `CARGO_TARGET_DIR` は本体と**分ける**(`rust/target` のまま等)
- **同じ target dir を共有するとバイナリは残らない**。worktree と本体が
  `target/debug/view` を上書きし合い、`cargo build` は再リンクを省くことがある。
  前後のビルドを比べるなら `CARGO_TARGET_DIR` を分けるか、`md5 -q` で確かめる
- `rust/target/` は検索・読み込みの対象にしない

## コードの歩き方

- viewer は `rust/src/bin/view/` 配下に関心事別で分割済み。
  `app` が状態、`keys` / `mouse` が入力、`editing` / `sync` が保存と同期を扱う。
  `ui/` は描画の責務別、`session/` は編集操作別に下位モジュールを持つ。
  詳しい対応表は `rust/NOTE-codebase-review.md` を参照する。
- 挙動を変えたら `rust/KEYMAP.md` とヘルプ文言(`ui/overlay.rs` の `help_keys` / Overlay::Help)も追随させる
- UI の文言は `t!("日本語", "english")` で両言語を並べる。キー名・フラグ・
  記法・製品名は翻訳しない(view/main.rs 冒頭のコメント参照)

## 作業の約束

- 変更は worktree を切って行い、テスト green を確認してから master へ
  ff マージする(1関心事 = 1コミット、コミットメッセージは日本語)
- 実験・検証は自分の非公開プロジェクト(無料で作れる)の `テスト` ページで行う
  (実編集OK。終わったら元に戻す)。プロジェクト名は git 管理外の
  `CLAUDE.local.md` に書く。文書中の `<sandbox>` はその名前を指す。
  非公開プロジェクトの認証は `cosense login` の PAT で足りる。
  `COSENSE_SID` が要るのは ws push 同期・web レンダラ(mmd)・プロジェクト設定の
  読み取り(テーマ・アップロード先)だけ
- 設計判断は PLAN-*.md / NOTE-*.md / SPEC-*.md に書き残す文化。実施した
  計画には「実施記録」を追記する
