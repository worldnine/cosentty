# PLAN: view.rs のモジュール分割計画

作成日: 2026-09-01。対象: `rust/src/bin/view.rs`(現在 18,762 行 / 806KB / 関数 549 個)。
本文中の行番号は master の ab7e4ab + 未コミット変更(view.rs +146/−36)時点のもの。
分割作業が進むと行番号はずれるので、**移動対象は必ずアイテム名(関数名・型名)で特定すること**。

## 目的と非目的

目的:
- AI コーディングセッションの探索コストを下げる。1 ファイルに全機能が同居しているため、
  毎ターン grep → 部分読みの往復が発生し、応答が遅くなっている。
- 関心事ごとに 200〜1,600 行程度のモジュールへ分け、「編集対象のファイルを開けば
  関連コードが揃っている」状態を作る。

非目的:
- 動作変更・リネーム・リファクタリングは一切しない。**純粋な移動(pure move)のみ**。
- ビルド時間の短縮は主目的ではない(bin クレート内の分割ではコンパイル単位は変わらない。
  `mod tests` は `#[cfg(test)]` なので通常ビルドには元々含まれない)。

## 現状の構造(全体マップ)

| 行範囲 | 内容 | 規模 |
|---|---|---|
| 1–74 | ファイル冒頭 doc コメント + `use` 宣言 | 74 |
| 75–157 | 色・寸法などの const、`ImageInfo`、`ImageMsg`/`FileMsg` | 83 |
| 158–509 | Web レンダラワーカー: `WebJob`/`WebMsg`/`WebOutcome`、`spawn_web_worker`、`run_render_batch`、`decode_web_png` | 350 |
| 510–710 | モデル型: `LinkItem`、`RelSection`/`RelEntry`、`Row`、`Mode`、`Place`、`HeaderColors`、`OutlineSnapshot`/`OutlinePending`、`MoveMode` | 200 |
| 711–1260 | `App`(struct 定義だけで約 330 行)、`MembersCache`、`TimeMachine`、`Input`、`EditSession`、`CommitJob`/`CommitOutcome`、`PolledPage` | 550 |
| 1261–1423 | ワーカー起動: `spawn_web_poller`、`spawn_visibility_probe`、`absorb_interval`、`spawn_commit_worker`、`Overlay`、`now_secs`/`relative_age` | 160 |
| 1424–2458 | `impl App` その1 | 1,035 |
| 2459–2532 | URL ヘルパ: `parse_page_url`、`percent_decode`、`urlencode_component`、`open_in_browser` | 74 |
| 2533–3528 | `impl App` その2 | 996 |
| 3529–3857 | テキスト/表示ヘルパ: `str_width`、`SessionWrap`、`session_display` 系、`truncate_width`、`related_row`、`card_lines`、`pad`、`wrap_plain` | 330 |
| 3858–4171 | `main` | 314 |
| 4172–4237 | `Ctx` | 66 |
| 4238–4705 | インデックス/履歴/ページ読込: `Loaded`、`build_related`、visits 系、`open_from_index`、`go_history`、`open_index`、`load_page`、`link_truth`、`spawn_link_prober` | 470 |
| 4706–4797 | 画像: `image_picker`、`image_protocol_name`、`build_image` | 92 |
| 4798–5355 | リンク抽出と操作: `positioned_links_on_line` 系、`activate_link`、`copy_payload`、クリップボード(`copy_to_clipboard`/`osc52_copy`/`b64_encode`)、`navigate_to`/`navigate_from` | 560 |
| 5356–5539 | イベントループ: `Action`、`run`、`flush_commits`、`handle_paste` | 184 |
| 5540–5628 | `handle_index_key` | 89 |
| 5629–6066 | アウトライン+移動モード: `outline_*`、`edit_outline`、`enter_move_mode`/`move_mode_step`/`move_mode_edit`/`leave_move_mode` | 440 |
| 6067–6477 | `handle_key`(READ モードのキー処理) | 410 |
| 6478–6952 | 編集の配管: `rerender`、undo 履歴(`ops_replayable` 系)、`MoveShape`/`MoveRebase`、`do_edit`、`edit_focus`、`CreateState`、`queue_commit`、`undo`/`redo` | 475 |
| 6953–8091 | EDIT セッション: `enter_session`〜`session_paste`、`handle_session_key` | 1,140 |
| 8092–8227 | `editor_roundtrip`、`recover_outline_action(_with)` | 136 |
| 8228–8760 | コミット結果とリモート同期: `handle_commit_outcome`、`adopt_*`、`install_remote_lines`、`apply_remote`、`handle_ws_event`、`ws_*`、`recover_conflict` | 533 |
| 8761–8881 | タイムマシン: `travel`、`show_snapshot`、`reload_page` | 121 |
| 8882–9366 | マウス: `handle_mouse`、`handle_mouse_index`、`handle_mouse_content`、`click_caret`、`register_click`、`word_span`、`jump_comment` | 485 |
| 9367–9478 | `handle_overlay_key` | 112 |
| 9479–10628 | 描画: `draw_index`、`index_preview_lines`、`ui`、`Inline`/`Placed`/`layout_inline`、`bullet_pad`、`shimmer`、`draw_overlay`、`gutter_cell` | 1,150 |
| 10629–18762 | `mod tests`(テスト 218 個) | **8,134** |

ポイント: **ファイルの 43% はテスト**。テストの分離だけで本体は約 10,600 行になる。

## 目標のモジュール構成

`src/bin/view.rs` を Cargo のディレクトリ形式バイナリ `src/bin/view/` に変換する
(`Cargo.toml` の `[[bin]] view` の `path` を `src/bin/view/main.rs` に変更)。
lib(`cosense`)側には手を入れない。全モジュールは同一 bin クレート内なので、
可視性は `pub(crate)` で足りる。

| モジュール | 入れるもの(現在の行範囲) | 予想規模 |
|---|---|---|
| `main.rs` | 冒頭 doc コメント(1–37)、`mod` 宣言、`main`(3858–4171)、`Ctx`(4172–4237)、`MAX_EVENTS_PER_FRAME`/`Action`/`run`/`flush_commits`/`handle_paste`(5356–5539) | ~700 |
| `app.rs` | `Row`/`Mode`/`Place`/`HeaderColors`/`OutlineSnapshot`/`OutlinePending`/`MoveMode`(570–710)、`App`/`MembersCache`/`TimeMachine`(711–1071)、`Overlay`(1384–1399)、`now_secs`/`relative_age`(1400–1423)、`impl App` ×2(1424–2458, 2533–3528) | ~2,700 |
| `web.rs` | `WebJob`〜`decode_web_png` を除く 158–509、`PolledPage`(1244–1260)、`spawn_web_poller`/`spawn_visibility_probe`/`absorb_interval`(1261–1351) | ~600 |
| `images.rs` | `ImageInfo` と画像系 const/type(110–157)、`decode_web_png`(503–509)、`image_picker`/`image_protocol_name`/`build_image`(4706–4797) | ~200 |
| `links.rs` | `LinkItem`(510–548)、URL ヘルパ(2459–2532)、リンク抽出〜`navigate_from`(4798–5355) | ~700 |
| `nav.rs` | `RelSection`/`RelEntry`(549–569)、`Loaded`/`build_related`/visits 系/`open_from_index`/`go_history`/`open_index`/`load_page`/`link_truth`/`spawn_link_prober`(4238–4705) | ~500 |
| `keys.rs` | `handle_index_key`(5540–5628)、`handle_key`(6067–6477)、`handle_overlay_key`(9367–9478) | ~650 |
| `outline.rs` | 5629–6066 一式、`MoveShape`/`MoveRebase` と関連 fn(6568–6634)、`recover_outline_action(_with)`(8152–8227) | ~600 |
| `editing.rs` | `Input`(1072–1148)、`CommitJob`/`CommitOutcome`(1204–1243)、`spawn_commit_worker`(1352–1383)、6478–6952 のうち outline 向け以外(`in_input`〜`redo`) | ~700 |
| `session.rs` | `EditSession`(1149–1203)、`SessionWrap` とテキストヘルパ(3529–3752)、`enter_session`〜`session_paste`/`handle_session_key`(6953–8091)、`editor_roundtrip`(8092–8151) | ~1,500 |
| `sync.rs` | `handle_commit_outcome`〜`recover_conflict`(8228–8760)、タイムマシン(8761–8881) | ~650 |
| `mouse.rs` | 8882–9366 一式 | ~500 |
| `ui.rs` | 色・寸法 const(75–108)、表示ヘルパ `truncate_width`/`related_row`/`card_lines`/`pad`/`wrap_plain`(3753–3857)、描画一式(9479–10628) | ~1,300 |
| `tests/`(サブディレクトリ) | 現 `mod tests`(10629–18762)をモジュール対応で分割 | ~8,100 |

判断に迷う所属の指針:
- `str_width`/`floor_boundary`/`byte_at_col` などの文字幅・バイト境界ヘルパは
  session と mouse の両方が使う。まず `session.rs` に置き `pub(crate)` で共有。
- `word_span` はマウスのダブルクリック語選択用だが session バッファを対象にする。
  `mouse.rs` に置く。
- `jump_comment` はマウス・キー両方から呼ばれるならより多く呼ぶ側に置く(現状 `mouse.rs`)。
- `impl App` のメソッドは第1段階では `app.rs` にまとめて移す。同一クレート内なら
  `impl App` ブロックは複数モジュールに分散できるので、後続フェーズで
  「session 専用メソッドは session.rs へ」のような再配置ができる(フェーズ 5、任意)。

## 実行フェーズ

各フェーズの終わりに必ず `cargo build --bin view` と `cargo test --bin view` を通し、
**テスト 218 個がすべて green のままであること**を確認してからコミットする。
1 フェーズ(大きいものは 1 モジュール)= 1 コミット。

### フェーズ 0: 準備
1. 未コミットの view.rs 変更(+146/−36)を先にコミットするか退避する。混ぜない。
2. 作業用 worktree を切る(このリポジトリの運用ルール)。
3. ベースライン記録: `cargo test --bin view 2>&1 | tail -3` の結果を控える。

### フェーズ 1: ディレクトリ化とテスト分離(効果最大・リスク最小)
1. `mkdir src/bin/view` し、`git mv src/bin/view.rs src/bin/view/main.rs`。
2. `Cargo.toml` の `[[bin]] name = "view"` の `path` を `src/bin/view/main.rs` に変更。
3. ビルド・テストが通ることを確認(この時点で挙動は完全に同一)。
4. `mod tests { ... }` の中身(10629 行目以降)を `src/bin/view/tests.rs` に切り出し、
   main.rs 側は `#[cfg(test)] mod tests;` の 1 行にする。
   - tests 内の `use super::*` は、main.rs(= クレートルート)直下の `mod tests` の
     ままなら**そのまま動く**。書き換え不要。
5. コミット。ここまでで main.rs は約 10,600 行になり、テスト待ちなしで最大の減量になる。

### フェーズ 2: 葉モジュールの切り出し(依存が一方向のもの)
順に 1 モジュールずつ。`images.rs` → `links.rs` → `web.rs`。
- 各アイテムを新モジュールへ移し、`pub(crate)` を付け、main.rs に `mod xxx;` を追加。
- 呼び出し側には `use crate::xxx::*;` ではなく必要な名前だけ `use` する
  (glob は後で「どこから来た名前か」を AI が追いにくくなる)。
- const(`IMAGE_MAX_COLS` など)も一緒に移す。

### フェーズ 3: 中間層の切り出し
順序は依存の少ない順: `nav.rs` → `sync.rs` → `mouse.rs` → `outline.rs` →
`editing.rs` → `session.rs` → `keys.rs` → `ui.rs` → 残りを `app.rs` へ。
- `keys.rs` と `ui.rs` はほぼ全モジュールに依存するので後ろに回す。
- `App` の**フィールド**への直接アクセスが全域にあるため、`App` の各フィールドは
  `pub(crate)` にする(可視性の絞り込みはやらない。非目的)。

### フェーズ 4: tests の分割
`tests.rs`(約 8,100 行)を `tests/` 配下にモジュール対応で分割する。
- テスト名は `a_...`/`the_...` のような文章スタイルで、接頭辞では機械的に分類できない。
  **各テストが主に呼んでいる関数**でどのモジュールのテストかを判定すること。
- テスト内には区切りコメント(`// ----`)と、`// ---- the sticky move mode
  (NOTE-outline-editing.md §移動モード) ----`、`// ---- websocket push
  (NOTE-websocket-sync.md) ----` という 2 つの明示バナーがある。これらは
  outline / sync のテスト群の境界の手がかりになる。
- 共有のテストヘルパ(フィクスチャ生成など)は `tests/support.rs` にまとめる。
- このフェーズは分割の判断を伴うので、フェーズ 1〜3 とは独立に、余裕のあるときにやる。

### フェーズ 5(任意): impl App メソッドの再配置
`app.rs` に残った `impl App` のうち、明らかに単一の関心事に属するメソッド群を
該当モジュールの `impl App` ブロックへ移す。1 関心事 = 1 コミット。

## 実行時の厳守事項

- **純粋な移動のみ**。関数の中身・シグネチャ・コメント・空行を一切変えない。
  diff が「削除と追加が同一内容」になっていることが正しさの証拠になる。
- リネーム・可視性の最小化・dead code 除去・clippy 対応を**同時にやらない**。
  見つけたら別タスクとしてメモに残す。
- 移動対象の特定は必ずアイテム名で行う(行番号は各移動でずれる)。
- 各コミット前に `cargo build --bin view && cargo test --bin view` が green。
  テスト数が 218 から減っていないことも確認する。
- 迷ったら「呼び出し元が最も多いモジュール」に置き、`pub(crate)` で共有する。

## 完了の定義

- `src/bin/view/` 配下の各ファイルが(tests を除き)最大でも 3,000 行以下。
- `cargo test --bin view` が分割前と同じ 218 テストで green。
- `git log` 上で各コミットが 1 モジュールの純粋な移動として読める。

## 実施記録(2026-09-01)

フェーズ 0〜4 を view-split ブランチで実施し、完了した。
フェーズ 5(impl App メソッドの関心事別再配置)は未実施の任意課題として残る。

- ベースライン: cargo test --bin view = 214 passed / 3 ignored(217 テスト)。
  各コミット後も同数で green を維持。
- 実装は計画どおりだが 1 点だけ方式を変えた: モジュール間の名前解決は
  「必要な名前だけ use」ではなく、main.rs(クレートルート)に
  `mod xxx;` + `use xxx::*;` を置き、各モジュールは `use super::*;` で
  ルート経由の名前を受け取る方式にした。旧 view.rs の参照を一切書き換えずに
  済ませるため(純粋移動の維持)。名前の所在は「ファイル = 関心事」で追える。
- 所属の判断は計画の指針どおり + OutlineSnapshot/OutlinePending/MoveMode は
  app.rs ではなく outline.rs に置いた(アウトライン専用のため)。
- tests/ の分類は「テストが主に触っている名前の所属モジュール」の自動採点
  + outline/move_mode の名前ルールで行った。境界上のテスト(コミットゲート系
  など)は editing/outline のどちらとも読めるので、探すときは両方を見ること。

最終構成(実測): main 676 / app 2,445 / session 1,477 / ui 1,316 / links 685 /
sync 644 / outline 629 / keys 615 / editing 559 / mouse 493 / nav 473 /
web 454 / images 154、tests/ は最大 1,780(app)。
