# NOTE-mmd-text: Mermaid の TUI テキスト描画(lib 方式)

正本。当初はブラウザ描画(`webrender` + headless Chrome)の手前に「テキスト描画」
の段を足す位置づけだったが、**ブラウザ描画は 2026-09-12 に撤去した**(末尾の実施記録)。
いまは テキスト描画 → 素のコード行 の2段。以下の本文は当時の記述のまま残す。

## 方式: `mermaid-text` に任せる

自前描画の分支(mmd-text-render)との見比べの末、lib 採用に決定。
理由: 25型カバー・幅 compaction・A* 配線を自前で追うより安い。
代償: toolchain 1.90 → 1.92 の bump(本 NOTE 末尾)。

- 依存: `mermaid-text = "0.57"`(MIT)。呼ぶのは `render_with_width` のみ
- 自前で持つのは段・旗・縮退だけ(`src/bin/view/mmd_text.rs`):
  型ゲート → lib 描画 → 行分割 → 幅超過チェック → `None` で縮退
- lib が Err の型・未知の型は描かない(画像 → コード行へ)

## 位置づけ: テキスト段が本流、ブラウザは既定でオフ

ブラウザ描画は「テキストで描けないものの受け皿」に降りたうえで、**既定では動かない**
(`RenderPolicy::Off`。`COSENSE_WEB_RENDER=manual|auto` で上げる)。テキストで
読めてしまうもののために Chrome を起こすのは高すぎるし、オフならキャッシュ I/O も
Chrome の検出も起きない——**sid への依存が websocket 同期だけになる**。端末で読む
ぶんにはテキストが正本で、**本物を見たければ `w` で web を開けばいい**——ページの
deep-link はもともとそのためにある。コード(ワーカー・backend・キャッシュ・認証の
縮退)は全部生きていて、上げれば即使える。

その帰結として、**自動の経路はテキストで描ける図の絵を取りに行かない**
(`App::drawn_as_text`)。描けている図のために Chrome を起こすのは、この
viewer で一番高い「何もしない仕事」だった。`R` は別で、あれは
「ブラウザの本物を見せて」という明示なので、押せば今までどおり取りに行く。

## 縮退順(1ブロックあたり)

1. 編集中(edit session がブロック内) → 素のソース(既存契約が勝つ)
2. `COSENSE_MERMAID=off`(`App::mermaid_text`) → 既存の経路(画像 → コード行)。
   `=ascii` は罫線なし描画(欠字フォント用、段自体は残る)
3. テキスト描画できる型 → lib 出力の `Row::Line`(選択・検索・yank 可)
4. 画像 artifact あり → `Row::Image`(既存)
5. どれもなし → 素のコード行(既存)。3 が**幅不足**で落ちた場合だけ、コード行の
   上に薄字の注記 `▏ 図はペイン幅に入らないためソースを表示(必要 N 桁 / 幅 M 桁)`
   を `Row::Aside` で添える(`mmd_text::TextOutcome::TooNarrow`)。未知の型や lib の
   Err は今までどおり黙る。画像に落ちた場合も注記しない(絵が出ていれば十分)

テキストが画像より先。理由: 即時・オフライン・SSH/tmux 安全、
リサイズ再レンダリング(3〜6s)なし、テキストとして残る。

## 対応表(lib 0.57 の行列表準拠)

flowchart・sequence・pie・er・gantt・gitGraph・class・journey・
mindmap・timeline・xychart・sankey・block・packet・quadrant・
requirement・architecture。state 系は lib が Err なら縮退。
sequence 箱の `░` 塗りは lib の味。
CJK の字間開きと罫線ずれは、lib の `Grid(Vec<char>)` が全角の continuation cell
までserializeする不具合だった(出力後の補正で対処)。
flowchart・sequence・state は `remove_wide_continuation_cells` で出力後に補正する。
補正前 `開 始` / `[成═功═]` → 補正後 `開始` / `[成功]`。
`classDiagram` などlib内panicもあるため、呼び出し境界を `catch_unwind` し、
失敗時は画像へ縮退する。
Cosense webに合わせ、Mermaidブロックは先頭空白0〜2個まで図として扱う。
1・2段目は図全体を `text_column(level)` だけ右へ送るが、READではビュレットを
描かない。EDITでソースへ戻した間だけ、`code:` ヘッダーにビュレットを置く
（caretがヘッダーにあっても本体にあっても同じ）。本体のコード行には置かない。
3段目以降は `code:` ヘッダーも本体もコードブロックとして消費せず、
各行自身の空白数どおりの通常リストに戻す。`code_span_at` / `code_line_flags` も同じ境界を
使う。画像縮退側の `Row::Image` も同じインデントを使い、`item: false` とする。
また、空行の先に別の `code:` ヘッダーがある場合は、後者がより深い段でも前の
コード本文へ吸収しない。空行なしの `code:` は従来どおり本文になり、コード内の
空行はインデント付き空行で表現する。

## toolchain bump(1.90.0 → 1.92)

`mermaid-text@0.57`(と `ascii-dag`)が rustc 1.92 を要求するため。
手順:

- `rustup override set 1.92-...`(本体 `rust/`)。worktree では
  `RUSTUP_TOOLCHAIN=1.92`。1.90 系と成果物を共有しないよう
  `CARGO_TARGET_DIR` は本体と分ける(worktree 既定の `rust/target` 等)
- 本分支の `CLAUDE.md` も 1.92 に書き換え済み。merge 時に本体へ反映
- `cargo test --bin cosentty` + `cargo test` が緑なのを確認

## 旧分支

自前描画の `mmd-text-render` ブランチは残す(設計記録・見比べ用)。
merge しない。

## 終了条件

- `cargo test --bin cosentty` green、`cargo test --lib` green(1.92)
- `tui_shot.py` で flowchart・sequence・pie の実画面を確認
- KEYMAP・`?`ヘルプ・`t!()` 両言語の追随(振る舞いが変わるもののみ)
- `<sandbox>/テスト` で実編集したら元に戻す

## 色

図は二層で読む: 形を保つ**罫線**と、箱の中の**言葉**。Cosense の SVG は
太さと色でそれを言うが、端末では罫線を薄字(`DarkGray`)にして言う——
`table:` ブロックが罫線でやっているのと同じ作法。言葉は本文のインクの
ままなので、ノードの日本語が飾りではなく文章として読める。

薄字にするのは**ラベルに出てこない字だけ**(box drawing・ブロック・矢印の
Unicode 範囲)。`COSENSE_MERMAID=ascii` の罫線は `- | + > v` で、ラベルにも
出る字なので二層を字で見分けられない——だから ascii では何も薄字にしない。

## 編集中のライブプレビュー

編集セッションがブロックの中にいる間、ソースの下に**いま打っている内容の組図**を
出す(cosense web と同じ)。打鍵のたびに再組図する(実測 0.4ms)。違いの演出は、
プレビューの全行に付く薄い左罫 `▏` と、先頭の薄字ラベル「プレビュー」——本文と
読み違えさせないため。lib が組めない式・ペイン幅に入らない式は黙る(ソースが
もう見えているので、何かを出す必要がない)。

## 実施記録: 幅不足の注記(2026-09-07)

`ある業務ページ` の 1 つめの図(subgraph 4 つの flowchart TB)が
「TUI で出ない」と報告された。調べると lib は描けているが、subgraph を
横並びに置く流儀のため CJK 補正後で最小約 110 桁あり、それ未満のペインでは
幅超過チェックで `None` に落ちてコード行になっていた。広いウインドウでは出る。
落ち方が設計どおりでも「なぜ出ないか」が読者に伝わらないので、幅不足だけは
理由を残す `render_text_outcome` を足し、コード行の上に必要桁数を添えた注記を
出すようにした。`render_text` は従来どおり `Option` のまま(web 段の
`drawn_as_text` などはそれで足りる)。

なお lib の `needed` は要求幅によって揺れる(60 桁要求で 83、120 桁要求で 110
など。compaction の結果がそのときの配置に依る)。注記の数字は「その幅で組んだ
ときの最大幅」であり、厳密な下限ではない。

## 実施記録: `diagrams = text | image`(2026-09-11)

「設定に図の描画があるのに効いていない」という指摘から。原因は縮退順が
テキスト段 → 画像で固定だったこと——`COSENSE_WEB_RENDER=manual|auto` で
Chrome が PNG を描いても、テキスト段が描ける型では画面が変わらなかった。
加えて off からの切り替えは再起動が要り、`R` の案内は環境変数しか指していなかった。

直し方は設定を「いつ描くか」から**「何を見せるか」**に作り替えること:

- `[view] diagrams = text | image`(`capability::RenderPolicy::{Text, Image}`)。
  既定 `text`。旧値は `off` → text、`manual` / `auto` → image と読む(`manual` は廃止。
  絵を選んだ人に毎ページ `R` を押させる意味が薄く、状態が 3 つあると「設定したのに
  出ない」の原因が増える)
- `image` では縮退順の 3 と 4 が入れ替わる: 画像 artifact があれば `Row::Image`、
  無ければ(届くまで・失敗したとき)テキスト段、それも無理ならコード行。
  `text` では画像を一切見ない(手元に PNG があっても)。`ui/content.rs` の `picture`
- `image` ではテキスト段が描ける図も取りに行く(`drawn_as_text` による自動パスの
  スキップを撤去)。`text` は `decide` が `Nothing` を返すので何も出ない
- `R` は「描き直し」: `web_errors` を空にしてから要求する
- ワーカーは設定に関わらず起動時から待機(`main.rs`)。`ArtifactCache::deferred` で
  ディレクトリの作成・sweep を最初の読み書きまで遅らせるので、`text` の会話は
  ディスクを触らない約束はそのまま。これで設定画面の切り替えが即時に効く
  (`settings::apply_view_change` が rerender と `start_web_renders(Auto)` を呼ぶ)
- 設定行の注記に前提を出す: Chrome が無い(`COSENSE_CHROME`)、sid が無い
  (`COSENSE_SID`。非公開の図は描けない)
- `[view] diagram_text = box | ascii` を追加(`mmd_text::DiagramText`)。
  `COSENSE_MERMAID=ascii` は Env 由来として同じ値に解決する。テキスト段の呼び出しは
  引数で受け取り、環境変数を描画時に読まない

## 実施記録: ブラウザ描画(`diagrams = image`)の撤去(2026-09-12)

sid の位置づけを「ws 受信を速くするだけ」に絞る整理の一環で、headless Chrome に
よる画像描画の経路をまるごと外した。既定はもともと `text` で、`image` は非公開
ページに sid が要る隠れ機能だった。後方互換は取らない(ユーザーがまだ少ないため。
作者判断)。`config.toml` に `diagrams = …` が残っていても無視される(未知のキーは
読み飛ばす)。

消したもの:

- lib: `chrome.rs`(CDP クライアント・ブラウザ検出・cookie 注入)、`webrender.rs`
  (要求・成果物・ディスクキャッシュ・FakeBackend)、`capability.rs` の
  `RenderPolicy` / `Visibility` / `Capabilities` / `decide`(残ったのは `SyncState` と
  ポーリング間隔の定数だけ)、`Client::probe_visibility`、`ArtifactKind::web`
- viewer: `web.rs` の描画ワーカーと `App` の `web_*` 状態(`web_unsynced` はヘッダの
  `未同期` 表示に使うので残し、`web_gen` はページ設置の世代として `install_gen` に改名)。
  ポーリング側(`PollCadence` / `spawn_web_poller`)は `poll.rs` に移した。
  `R`(描き直し)のキー、設定画面の「図の表示」行、`[view] diagrams` キー、
  `COSENSE_WEB_RENDER` / `COSENSE_CHROME` / `COSENSE_WEB_TIMEOUT` / `COSENSE_WEB_DEBUG`
- dev-tools: `web_smoke`、`outline_probe`(Chrome で本家のアウトライン操作の wire を
  覗く道具。成果は `NOTE-outline-editing.md` と実装に落ちている)
- 文書: `NOTE-webrender-handoff.md`(git 履歴には残る)。KEYMAP の「Mermaid の描画の
  仕組み」節
- 図の描画中に流していた縦の明るさの帯(`shimmer`)。画像ダウンロード待ちの横の帯
  (`shimmer_across`)は残る

残したもの: `Block::Artifact` と `last_src`(テキスト描画の帰属行に使う)、
`diagram_text = box | ascii`、`COSENSE_MERMAID` / `COSENSE_MATH`。
`~/.cache/cosentty/webrender/` は消さないので、不要なら手で消す。
