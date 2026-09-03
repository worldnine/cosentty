# PLAN: 次に実装するもの(Cosense のメモ3ページの整理)

整理日: 2026-09-02。出典(メモは Cosense 側に残してある。消化済みでも消さない):

- [改善案](https://scrapbox.io/my-sandbox/改善案) — P0〜P5 のロードマップ本体
- [改善案4](https://scrapbox.io/my-sandbox/改善案4) — 直近の不具合・モード再設計(ほぼ消化済み)
- [一覧画面の機能拡充](https://scrapbox.io/my-sandbox/一覧画面の機能拡充) — サイトトップ刷新の具体要件

## 現在地

- 改善案の P0〜P3 は済。P4(表示とナビゲーション)が主戦場
- 改善案4 の項目は**このセッションで全部消化した**:
  行またぎドラッグのクラッシュ / 語・行クリック選択 / ソースモードの降格(`s`)/
  VIEW⇄EDIT の視覚区別(下敷き・アクセント枠・バッジ・キャレット)/ J/K 選択
- 改善案 P4 の「アウトライン編集モード」はリポジトリでは**実装済み**
  (outline.rs、移動モード、NOTE-outline-editing.md)。ページ側の記述が古い

## 次に実装すべきもの(優先順)

### 0. API 基盤の整理 — **済(2026-09-02)。実施記録は末尾へ**

### 1. サイトトップ刷新 — **済(2026-09-02)。実施記録は末尾へ**

残っていた未決「ソート順を各ページ下部の関連ページにも効かせるか」は
**済(2026-09-03)**。調べたところ関連ページ API の各エントリは `updated` の
ほかに `accessed` / `created` / `linked` を持っていた(`views` は無い。並びは
updated 降順)ので、手元で並べ替えられる。セクション内だけを並べ替え、
セクションの順(Links → ハブごとの 2 hop → External)は保つ。`views` は
サーバの順のまま。行の薄い値も並べている値に変える。生ブロックを App に
持ち、`s` で順を変えたら取り直さずに並べ直す(`rebuild_related`)。

### 2. 画像の貼り付け — **済(2026-09-03)。実施記録は末尾へ**

クリップボードの画像を貼り付け → アップロード → 記法挿入。仕様の正本は
[アウトライン編集と画像貼り付けの仕様案](https://scrapbox.io/my-sandbox/アウトライン編集と画像貼り付けの仕様案)。
本家の挙動は [help-jp/画像アップロード](https://scrapbox.io/help-jp/画像アップロード)
と [help-jp/ファイルアップロード](https://scrapbox.io/help-jp/ファイルアップロード)。

**これは1機能ではなく3つの別々の問題が縦に積まっている。** 着手前に
それぞれの現在の動作／期待する動作／完了条件を書き出すこと。

#### (a) 画像をどこから取るか — OS ごとの外部依存

端末はクリップボードの**画像**をアプリに渡せない(OSC 52 は文字列だけ)。
だから取り出し口は端末の外にある:

- macOS … NSPasteboard を読む小さなヘルパ。**`rust/scripts/ime.swift` を
  swiftc でビルドする経路が既にある**ので、同じ流儀に乗せられる
- Wayland … `wl-paste --type image/png` / X11 … `xclip -t image/png -o`
- どれも無ければ、理由を1度だけ言って何もしない

**ヘルパ無しでも動く経路を v1 に入れる**: 貼り付けられた文字列が実在する
画像ファイルのパスなら、それを画像として扱う。Finder やエクスプローラからの
ドラッグ＆ドロップは多くの端末でパスの貼り付けになるので、これだけで
かなりの場面が埋まる。**先にこちらを作れば、ヘルパのビルド経路を増やす
判断を後ろへ倒せる。**

#### (b) どこへ上げるか — Cosense のファイル保管 と Gyazo Teams

**先に決まったこと(2026-09-02、ユーザー判断)**: (a) は**パス貼り付けだけで
v1 を切る**。macOS ヘルパのビルド経路は増やさない。

アップロード先は「プロジェクトごとに変えたい」——例えば acme の
プロジェクトは acme の Gyazo(Teams)へ上げたい、という要望。

##### Gyazo Teams へ上げられるか — 調べた(2026-09-02)

**上げられる。** 確かめたこと:

- アップロード先は個人でも Teams でも同じ `POST https://upload.gyazo.com/api/upload`
  (multipart、`access_token` フィールド)。**どの Gyazo に入るかはトークンが
  決める**。エンドポイントを分ける必要はない
- `GYAZO_TEAMS_ACCESS_TOKEN` はこの環境に既にある。API 疎通も確認(200)
- **落とし穴**: 応答の `permalink_url` は Teams でも `gyazo.com/<id>` で返り、
  そのURLは 404 になる(実測。上の API 応答で確認)。Teams の permalink は
  `https://<org>.gyazo.com/<id>` に**こちらで組み直す**必要があり、そのために
  **org 名を知っていなければならない**。API は org 名を返さない
- **読む側は既に出来ている**。`render.rs` の `find_gyazo` は
  `<org>.gyazo.com/<id>` を Teams として正しく拾い(サービス用サブドメイン
  `i`/`t`/`thumb`/`www`/`api`/`upload` は除外)、`image_fetch.rs` は
  トークン付きの Gyazo API を先に叩いてから oEmbed へ落ちる。
  `main.rs` は `GYAZO_TEAMS_ACCESS_TOKEN` → `GYAZO_ACCESS_TOKEN` の順で読む。
  **つまり足りないのは書く側だけ**

##### 「web で見えるか」は確かめるまでもなかった

**本家がその経路で上げている。** プロジェクト設定に Upload タブがあり、
acme は `Upload images to: gyazo.com` + `Upload to Gyazo Teams` +
`Team name: acme-inc` になっている。実際 acme には
`https://acme-inc.gyazo.com/<id>` を貼ったページが 100件以上ある(検索で
実測)。つまり web が描くことも、メンバーに見えることも、既に日常の事実。

TUI 側も読める。その id を Teams トークンで Gyazo API に投げると `url` が
返る(実測)。`image_fetch.rs` はまさにその手順を踏んでいる。

##### アップロード先を自分で持つ必要はない — **プロジェクトが知っている**

`/api/projects/<name>` に**そのままの3フィールド**がある(実測):

| フィールド | my-sandbox | acme |
|---|---|---|
| `uploadFileTo` | `gcs` | `gcs` |
| `uploadImageTo` | `gyazo` | `gyazo` |
| `gyazoTeamsName` | `null`(= 個人 gyazo.com) | `acme-inc` |

だから設定ファイルも環境変数も要らない。**本家の設定をそのまま読んで従う**
のが正解で、ブラウザと TUI で行き先が食い違うこともなくなる。
`uploadImageTo` の語彙は `gcs` / `gyazo`。

**ただし `/api/projects/<name>` は非公開プロジェクトで PAT を弾く(401、実測)。
sid が要る。** これは `get_project_theme` と同じ制約で、**sid が必要なものの
3つめ**になる(ws push・mmd 描画・これ)。

##### 決まったこと(2026-09-02、ユーザー判断)

- **sid があれば設定は要らない。** プロジェクト設定を読んで、そのとおりに
  上げる(= ブラウザと同じ行き先)
- **既定は `gcs`(Cosense へ上げる)。** 理由は**閉じるから** — ファイルは
  プロジェクトに属し、権限もプロジェクトの権限、外部サービスに依存しない。
  行き先が分からないときに外へ出さない、という向き
- **上書きは自前の TOML で吸収する。** どのみち**キーバインドの設定**で
  設定ファイルは要るので、そこに相乗りさせる。環境変数を増やさない

つまり優先順位は **TOML > プロジェクト設定(sid 必要) > `gcs`**。

##### 設定ファイル(まだ無い。ここが初出になる)

いまリポジトリに自前の設定ファイルは無く、すべて環境変数とフラグで
できている。状態は `$XDG_STATE_HOME/cosense-tui/visits.json` に置いている
ので、設定は素直に `$XDG_CONFIG_HOME/cosense-tui/config.toml`
(既定 `~/.config/cosense-tui/config.toml`)になる。

書き味の案(実装時に詰める):

```toml
[upload]
images = "gcs"            # gcs | gyazo   (省略時: プロジェクト設定 → gcs)

[upload.project.acme]
images = "gyazo"
gyazo_team = "acme-inc"
```

- **`~/.cosense/settings.json` には書かない**。あれは公式 CLI のもの
- キーバインドの設定を足すときは同じファイルの別テーブルにする

##### sid が無いときに起きること(承知の上)

sid を持たずに acme のページへ画像を貼ると、TUI は `gcs` へ上げる。
ブラウザは Gyazo Teams へ上げるので、**同じページに2つの保管先の画像が
混じる**。壊れはしない(どちらも表示できる)が、意図しておく。だから
**上げた先をステータスに出す**こと。混ぜたくない人は TOML に書けばよい。

なお設定の読み取り結果はプロジェクト単位でキャッシュしてよい
(`Ctx::project_themes` と同じ流儀。同じ `/api/projects/<name>` を叩くので、
**1回の取得で theme と upload 設定の両方が手に入る**——いまの
`get_project_theme` を「プロジェクト設定を取る」に育てるのが素直)

#### (b-2) Cosense のファイル保管(既定・本家と同じ)

本家は「画像はデフォルトで Cosense にファイルアップロードされる。Gyazo に
することもできる」で、プロジェクト設定に Upload タブがある。仕様ページの
推奨も案1(Cosense)を既定、Gyazo はフラグ。

cosense-cli の `uploadFile` を読んで確かめた送信手順(3往復):

1. `POST /api/gcs/<projectId>/upload-request`  body: `{md5, size, contentType, name}`
   - **同じファイルが既にあると、ここで `embedUrl` が即返る**(2〜3を飛ばす)
   - 無ければ `{signedUrl, fileId}` が返る
2. `PUT <signedUrl>`  (Content-Type だけ付ける。**認証ヘッダは付けない**。
   403 のときは「しばらく待つと通る」と CLI 自身が案内している)
3. `POST /api/gcs/<projectId>/verify`  body: `{md5, fileId}` → `embedUrl`

**`projectId` の取り方に落とし穴がある。** `/api/projects/<name>` は
非公開プロジェクトで PAT を弾く(401。api.rs にも記録済み)。だが CLI が
使っているのは **`/api/projects/<name>/users`** で、こちらは PAT で通り、
応答に `projectId` が入っている(実測)。**このエンドポイントは viewer が
メンバー表のために既に叩いている**(`list_members_in`)ので、`projectId` を
一緒に拾えばよく、新しい認証の心配は要らない。

制約: 1ファイル 100MB まで。アップロードはメンバー権限が必要(401/403)、
容量上限超過は 402。公開プロジェクトのファイルは誰でも見られる。

#### (c) いつ本文へ入れるか — 非同期の合流

ネットワークなので裏で走らせる。仕様ページの決めごと:

- 貼り付けた時点の**行ID**を覚えておき、完了したらその行に差し込む
- その行が消えていたらページ末尾に置いて、そう伝える
- 進行はステータス行。402 や権限エラーはそのまま理由として見せる
- 挿入する記法は `[<URL>]`。カーソル行に文字があればその位置に差し込む
  (行内の画像は描画側が対応済み)

これは **0. でやった「本文を先に描いて関連ページを後から合流させる」のと
同じ形**(背景スレッド + チャネル + 世代/対象の照合)。`start_related_load` /
`drain_related` がそのまま手本になる。

#### 決めたいこと

- **済**: (a) はパス貼り付けだけで v1 を切る。macOS ヘルパは作らない
- **済**: アップロード先は自前で持たない。プロジェクト設定
  (`uploadImageTo` / `gyazoTeamsName`)を読んで本家に従う
- **済**: web でも見える(本家がその経路で上げていて、acme には
  既に100件以上ある)
- **済**: 既定は `gcs`(閉じるから)。sid があればプロジェクト設定に従い、
  上書きは自前の TOML(キーバインドの設定と同じファイル)で吸収する
- **済**: Gyazo へ上げるトークンは `GYAZO_TEAMS_ACCESS_TOKEN`
  (無ければ `GYAZO_ACCESS_TOKEN`)。読む側が既にその順で見ているので、
  読み書きで同じトークンになる。ブラウザはユーザーごとの **OAuth 接続**
  (User Settings の「Gyazo OAuth Upload」)で上げているので、TUI は
  **別の経路で同じ場所へ入る**ことになる——実害は無いはずだが、意図の上

**これで 2. の未決は無くなった。** 着手してよい。

### 3. コメントモードの置き場所(P3 の残り) — **済(2026-09-03)。実施記録は末尾へ**

- キーバインドが混んできたので、どのモードに属させるかを決め直す
- 前提が変わっている点に注意: **Tab は再編済み**(READ=リンク巡回 /
  EDIT=インデント・表セル / 索引=ペイン切替。PLAN-mode-ux.md 参照)。
  メモの「テーブルが Tab の意味を増やした前提」はさらに更新が要る

### 4. トースト表示(ステータスバー依存からの脱却) — **済(2026-09-03)。実施記録は末尾へ**

いまは通知がすべてフッタの status 1行に集中していて、キーヒントと場所を
奪い合い、次の通知で上書きされて消える。akapen の実装を正として移植する。

akapen の実装(読了済み。akapen/src/chrome.rs の draw_banner / draw_message、
akapen/src/effects.rs の toast_effect):

- **メッセージ行**: フッタの1行上(vim のメッセージライン位置)。スペースは
  予約せず、バナーの矩形だけ Clear して浮かべる — レイアウトは動かない
- **バナー**: 中央寄せ・1行・黒背景・太字。info=黄 / error=赤(エラーは
  flash + ビープも)。幅はメッセージ幅+2、ペイン幅で切り詰め
- **優先順位**: 応答が要る永続プロンプト(確認系)>トースト。プロンプトは
  応答まで残り、トーストは自然消滅する
- **fx**: tachyonfx で fade in 120ms → hold → fade out 120ms。合計を
  STATUS_SECS に一致させ、消滅とフレーム返却が同時になる。
  `CellFilter::BgColor(Black)` でバナー自身のセルだけに効果を限定
  (枠や本文は光らない)

こちらでやること:

- status の用途を3分類する: ①一過性の通知(コピーした・✓・保存先など)
  → トースト ②持続状態(モードタグ・位置・sync 状態・キーヒント)→ フッタ
  ③応答が要る確認 → プロンプト。①だけをトーストへ移す
- tachyonfx を依存に足す(akapen と同じ)。既存の shimmer(自前実装)を
  tachyonfx へ寄せるかは別判断 — まずはトーストだけで入れてよい
- バナー色は akapen の黒地固定でなく、端末背景から作る手もある
  (edit_backdrop と同じ流儀)。ライト端末での見えを確認して決める

### 5. ヘッダの整理 — **済(2026-09-03)。実施記録は末尾へ**

面積に限りがあるのに詰め込みすぎている。現状: project/title + 時刻バッジ +
[✎ 編集中] + [ソース] + [読み取り専用] + （コメント N） + 未読 + 選択情報。

決めたこと:

- **コメント数の常時表示はやめる**(`l` の一覧で足りる)
- **プロジェクト名は URL スラッグではなく正式名称を出す**。
  `Page`/project API の `displayName`(api.rs にフィールド定義済み)を引く

検討(実装時に決める):

- ヘッダに残す主役は project/title。バッジ類の削減候補:
  [✎ 編集中] はモードサインが下敷き・枠・フッタバッジで三重にあるので
  外せるかもしれない。選択情報・未読はトースト/フッタへ寄せる手もある
- トースト導入(上の 4)と同時にやると、ヘッダから追い出した情報の
  行き先が揃う

### 小粒(隙間にやれるもの)

- **済(2026-09-03)** `q` の終了ガード: 一度目は黄バナーで問い、4秒以内の二度目で終了
  (`App::confirm_quit`)。他のキー・Esc で取り下げ、`^c` は即終了。未送信の編集が
  あれば件数を添える。ダイアログにしなかった理由は KEYMAP の quit 行に。

- **済(2026-09-03)** 一覧の操作を続けると **429 Too Many Requests** に当たる
  (同日実測。並び順の切り替えは毎回 500 件を取り直し、本文検索も1回ずつ叩く)。
  API 呼び出しが 429 を `Retry-After` だけ待って言い直す(`SendPolite`)のと、
  画像の同時ダウンロードを4本に絞るのに加え、**並び順ごとの一覧を
  セッション内に5分キャッシュ**した(`App::index_cache`、`nav::ListCache`)。
  - 現在の動作: `s` で順を選ぶたび `list_pages_in(…, 500)` を撃つ。6つ巡ると6本
  - 期待する動作: 一度取った (project, order) の一覧は `INDEX_CACHE_SECS`(5分)の
    あいだ再利用。`^o` / `^u` / `[/project]` は今まで通り取り直し、結果で覚え直す
    (編集直後に `^o` で戻ったとき、自分の編集が一覧の先頭に居ないと嘘になる)
  - 再現手順: 一覧で `s` → 順を6つ続けて選ぶ。以前は途中で 429 の待ちが入った
  - 完了条件: 上の操作で要求が最初の1巡だけになる。5分過ぎた順は取り直す
    (`a_fetched_list_is_reused_by_an_order_switch_while_young`)
  - 依存: なし。本文検索(`/`→`?`)は毎回叩くまま(問いが毎回違うので)

- **済(2026-09-03)** Block::Inline(画像と文字が混ざった行)のリンククリック。
  レンダラが Hit を捨てていたのを、テキスト部品を通したスパン番号で残す
  (`Hit::span` の約束を追記)。レイアウト(`layout_inline`)は文字の片ごとに
  `TextPiece { part, start }` を持ち、`App::inline_link_at` が押されたセルを
  部品の折り返し前の列 → スパン → Hit と逆引きする
  - 現在の動作: 混ざった行のリンクをクリックしても何も起きない(Enter は効く)
  - 期待する動作: 通常の行と同じくクリックで開く。画像の上の段や画像の上では何もしない
  - 再現手順: `本文 [リンク] [画像URL] 後 [Docs https://…]` の行で各リンクをクリック
  - 完了条件: `links_on_a_line_of_text_and_pictures_are_mouse_hit_targets`、
    `a_mixed_text_and_picture_line_keeps_its_hits_across_its_text_parts`
  - 依存: なし
- ヘルプ画面のモード別再構成(READ / EDIT / 一覧 / オーバーレイ)

## 将来メモ(今はやらない)

- **設定ファイル(`~/.config/cosense-tui/config.toml`)**: 画像アップロード先の
  上書き(2. で初出)と**キーバインドの設定**が同じ場所を欲しがっている。
  片方だけのために作らず、最初から両方が乗る形にする

- **リモートカーソル表示**: web で編集中の相手のカーソルを TUI に出す。
  cursor イベントが**即時に届くことは実測済み**(2026-09-01 の調査)。
  web は行内の変更を行を離れるまでコミットしない仕様なので、「なぜまだ
  反映されないか」を画面で説明する手になる。やるなら本気のプレゼンス表示として
- `^e`($EDITOR 往復)直後のカーソル形状が外部エディタ設定のまま残りうる
  (PLAN-mode-ux.md に記録済み)
- P5 系: 表示プロファイル(auto / rich / text)、数式、テーマの意味的整理
- ベクトル検索(`search/vector/titles`)・プロジェクト一覧(`/api/projects`)の
  組み込み — 一覧刷新(1)と相性がよいので、その設計時に一緒に検討する

## 済(このセッションで消化)

- 開発環境の掃除(2026-09-02): `cargo clean` 実施、HANDOFF.md をスリム化
  (旧全文は rust/NOTE-webrender-handoff.md へ移動)、プロジェクト用 CLAUDE.md を追加

### 実施記録: 0. API 基盤の整理(2026-09-02)

**ページ読み取りの v1 → v2 移行**。実測してから切り替えた(villagepump/井戸端
と my-sandbox/テスト、匿名と PAT の両方):

- v2 は本文について v1 と**完全に同形**。`lines` も `persistent: false` の
  未作成テンプレートも同じで、未作成ページに仮 id を返すのは v1 も同じだった
  (計画が心配していた点は空振り)。`commitId` はどちらも未作成では返らない
- v2 読み取りは PAT で通る(非公開プロジェクトで確認)。v2 読み取りでは
  `lastAccessed` / `accessed` / `views` は動かない — v1 と同じく安定した
  「最後に見た」印のままなので、1. のソート(最終アクセス)の前提は保たれる
- **見つけた差: v2 は `relatedPages` を積んでいない**。しかもそれが v1 の
  応答のほとんどを占めていた(井戸端で 13.7KB / 0.2s 対 364KB / 0.7s)

relatedPages は下部の関連セクションだけでなく**リンク色の判定**(存在しない
リンクの赤)にも使っているので、捨てられない。v2 側に代替エンドポイントは
見つからなかった(`/related-pages` 等はいずれも 404)。そこで**二段読み込み**に
した:

1. 本文は v2 で読んで即描画。この時点で関連セクションは無く、リンクは
   全部通常色(「まだ知らない」= 通常色。既存の LinkTruth の安全側の作り)
2. 関連ブロックは背景スレッドで v1 のページエンドポイントから取り、
   届いたらセクションとリンク色を作り直す(`App::start_related_load` /
   `drain_related`)

総バイト数は減らないが、**本文が出るまでの待ちが 0.7s → 0.2s** になる。
代償は開いた直後の一拍だけ関連が無いこと。ユーザー確認済み(2026-09-02)。

派生して決めたこと:

- `PageFacts`(title / persistent / links / projectLinks)を持たせた。関連
  ブロックが届く時点で `Page` は手元にないため。`build_related` /
  `link_truth` はこれと `Option<&RelatedPages>` を受ける形に変えた
- `related_pending` の間はリンクプローブを止める。ページ自身の答えが来る
  途中で1リンク1リクエストを撃つのは本末転倒。ゲートが下りた瞬間に
  スキャンを即時化する
- **resync では関連リストを作り直さない**。resync も v2 で本文を取るので、
  作り直すと remote commit ごとにセクションが黙って空になる
- 回帰テストを2本足した(合流でセクションとリンク色が入る / 取得失敗でも
  ゲートは下りてプローブに渡る)。`Page.related` が `Option` なので、
  ここが壊れても黙って消えるだけになる形だった

#### 続き(同日、指摘を受けて足したもの)

- **書き込めないプロジェクトでは作成行を出さない**。`＋ 「名前」を作成` は
  読み取りだけのプロジェクトでは API に断られる約束でしかない。`Index` に
  `can_create` を持たせ、既定は `false`(まだ知らされていない一覧が約束を
  してよい理由はない)。ヘッダにページと同じ `[読み取り専用]` を出す
- **日本語を打てる場所すべてでハードウェアカーソルをキャレットに置く**。
  端末の IME は変換窓をハードウェアカーソルに付ける。EDIT セッションは
  元からそうしていたが、**一覧の絞り込み行とコメント入力欄は置いておらず**、
  変換中の文字が見当違いの場所に出ていた。全角=2桁なので、キャレット列は
  「打った文字まで」の表示幅から**測る**(パーツから計算し直すと必ずずれる)
- **検索語・絞り込み語に一致した部分へ印**。入れたつもりで入っていなかった
  (`highlight.rs` はコードブロックの syntect で別物、`SearchResult.words` は
  パースだけして捨てていた)。印は敷き+太字で前景色を奪わない——一覧の行は
  既に青=未読・反転=キャレット/選択・帯=カーソル行と印を使い切っている。
  抜粋は**描画したあと**の span ごとに付ける(ヒット行は `[* …]` の生記法で
  届くので、そのまま探すと括弧に印が付く)
- 一覧の通知は `app.status` ではなく `index_notice` に分けた。status は
  背後の同期も書くので、一覧を眺めている最中にキーヒントを奪われる
- 検索の件数は `100+ hits`。`search/query` は100件で頭打ちで、**`count` も
  一緒に頭打ちになる**(villagepump/"Scrapbox" が count:100・limit:100・100件)

**connect.sid のオプトイン化**は、読む・書く・検索が PAT で完結する形に
すでになっていた(`AuthStore` の解決順序、`app.caps.sid`、sid なしの
ポーリング縮退、`get_project_theme` の cosmetic 例外)。残っていたのは文言で、
HANDOFF / CLAUDE.md の「非公開プロジェクトの認証は COSENSE_SID」を直し、
KEYMAP の「認証」節に「sid が必要なのは ws push と mmd 描画だけ」を明記した。

### 実施記録: 3. コメントの整理(2026-09-03)

ユーザー判断: 「縮める」(モードに隔離しない・落とさない)。やりたいことは
**herdr 経由でエージェントに直送**と、**引用とコメントを混ぜたテキストの
クリップボード送り**。akapen(`https://github.com/worldnine/akapen`)を参照。

- 現在の動作: READ に `v` `c` `d` `^n` `^p` `l` の6キー。書き出しは終了時の
  stdout と `l`→`y` のみ。書式は `Page:` `URL:` `Lines … (exact text — …)`
  `Instruction:` のラベル付きで冗長。送る手段が無い
- 期待する動作: READ は `c`(書く。同じ範囲で再度 `c` = 編集)・`s`(送る)・
  `l`(一覧: Enter / d / y / s)。選択は Shift+↑↓ / J/K に統一。廃止キーは
  黄バナーで行き先を言う。`s` は全件をクリップボードへ + 宛先へ届け、
  **届いたときだけ消す**(akapen の `s`)。宛先は `--send-cmd` → herdr の
  タブの唯一のエージェント(`herdr agent list` / `herdr agent prompt <pane> <text>`、
  argv 直渡し)→ 無し。書式は akapen の返信形に Cosense の場所1行を足したもの
- 完了条件: `retired_comment_keys_say_where_their_job_went`、
  `the_comments_list_deletes_with_d`、`s_with_nowhere_to_send_…`、
  `s_pipes_the_export_to_the_send_command_and_clears_on_success`、
  `c_on_the_same_range_edits_the_existing_comment`、`handoff::tests`、
  `comment::tests`(書式)、`the_quit_question_counts_unsent_comments`
- 決めたこと:
  - 送信キーは akapen 通り **`s`**。ソース表示は `s` から奥の **`z`** へ移した
    (ユーザー指示「ソース表示はもっと奥へ」。使う頻度が低く、無シフトで空いていた)
  - herdr の中では**フラグ無しで直送**(akapen は `--send-agent` で opt-in)。
    `HERDR_PANE_ID` があれば herdr の中。テストは `Ctx::send_target` を
    `SendTarget::None` に固定するので、herdr の中で走らせても外へ出ない
  - 終了時の stdout 印字は残す(送り忘れの保険)。q の問いに「未送信のコメント N 件」を添える
  - 永続化(comments.json)はしていない。送ってしまえば消えるものなので、
    要るなら別の小粒として
  - **入力欄の見た目**(同日、「該当行の下に割って入る、akapen に沿って」):
    フッターの3行入力欄を撤去し、`Row::Composer` としてコメントする範囲の最終行
    (そのカードの後)に織り込む。akapen の `composer_lines` と同じシアンの罫線、
    ` comment · 12-14 ` / ` edit · 12-14 ` のラベル、`wrap_with_caret` で折り返し
    とキャレット位置を出し、ハードウェアカーソルをその桁に置く。カードも akapen の
    `comment_bar_lines` に揃えて罫線＋黄タイトルだけにした(箱と背景を廃止)ので、
    Enter で入力欄がカードに「なる」。`keep_composer_visible` が画面外に出るのを防ぐ。
    テスト: `the_composer_opens_under_the_commented_range_and_replaces_the_card_it_edits`、
    `the_composer_body_wraps_and_reports_where_the_caret_landed`
  - **左端の帯**(同日): コメントの付いた行は左フレーム列に黄の `▌`、入力中の範囲は
    シアンの `▌`。キャレット `>` より後に描くので重なれば帯が勝つ(ユーザー指示
    「キャレットとかぶっていい」)。`commented_lines_wear_a_yellow_bar_in_the_frame_column`
  - IME は既存の `ImeGuard`(`c` で日本語 → Enter/Esc で英数)のまま。ヘルパは
    `~/.cache/akapen/ime-<hash>` を共用(同じ ime.swift なのでハッシュが一致)
  - **履歴へのコメント**(同日。「現在のコメントが過去にも出るのは論理的か」→
    行番号だけの結び付けで過去版の無関係な行に出ていた。ユーザー案「履歴にも別に
    コメントを付ける」を採用): `Comment` に `page_id` と `revision: Option<Revision
    { snapshot_id, created }>` を足し、`App::comment_is_shown` で「このページ・この版」
    のものだけ織る/帯を出す/再編集の対象にする。履歴中の `c` を解禁(書いた版に固定)。
    一覧の Enter は `show_revision` でその版へ入ってから行へ(NOW のコメントなら
    `reload_page` で NOW へ)。書き出しには `Snapshot: <id> (<時刻>) — cosense
    readPageSnapshot <projectUrl> <pageId> <snapshotId>` を足す(skill が1コマンドで
    その版を読める。snapshot の行 ID は NOW と同じなので anchor もそのまま)。
    テスト: `a_comment_lives_on_the_revision_it_was_written_on`、
    `the_list_travels_to_the_comments_revision_before_landing`、
    `a_comment_on_a_past_revision_names_the_snapshot_and_how_to_read_it`
  - **cosense skill との相性**(ユーザー指示。skill は
    `~/.claude/plugins/cache/cosense-cli/cosense-cli/<ver>/skills/cosense/`):
    skill は「URL は `https://` から次の空白まで」「URL の `#<lineId>` が編集
    アンカーの最優先」「ops は lineId を anchor にする」「previewEdit は変更行の
    行末に `# <lineId>` を付ける」という語彙で動く。書式をそれに合わせた:
    URL を行頭に(空白なし・CLI の `encodeTitleForUrl` と同じ `_` 区切り)、
    引用行の末尾に `# <lineId>`。以前の `edit_lines matches verbatim` は
    別ツールの語彙で、skill には無い。受け取ったエージェントは skill の手順
    (browsePage → readPage → previewEdit → submitEdit)にそのまま乗れる
- 場所: `src/bin/view/handoff.rs`(宛先の決定・送信・エージェント解決)、
  `src/comment.rs`(書式)、keys.rs のコメント節と一覧オーバーレイ

### 実施記録: 5. ヘッダの整理(2026-09-03)

- 左 `正式名称 / タイトル`。`ProjectSettings.display_name`(`displayName`)を足し、
  `Ctx::project_display` が設定キャッシュ(テーマと同じ応答)から引く。読めなければ
  スラッグ。`Loaded.project_display` → `App.project_display`、索引は `index_display`
  (open_index と履歴復帰で埋める。テストが直接 `index` を置く場合は
  `index_project` → `project` に後退)
- 右端は状態だけ: **書かれた日時**(NOW は最新行の `updated`、履歴は快照の時刻 + `n/N`。
  同じ場所が入れ替わるので過去への遷移が続きに見える。履歴中は位置が先(`3/12 · 日時`)。
  位置は **NOW を最後の1つとして数える**(快照3つなら NOW が `4/4`、← で減る)。
  そのため快照一覧を `start_snapshots_load` でページ設置時に裏取り(関連ページと同じ
  ゲート・同じスレッド方式)し、← はそれを使う(無ければ従来どおり自分で取る)。
  幅が足りないときは右側を優先し、左は**まずサイト名を丸ごと落として `/ タイトル`**、
  それでも足りなければタイトルを `…` で削る。履歴中はヘッダと枠を
  `theme::history_header_colors`(Cosense の purple)にする。`⏪` は使わない)/
  `未同期`(`web_unsynced`。NOTE-notifications の未決に答える常駐表示)/ `読み取り専用`。`初回`・`未読 N` も一度は残したが、テロメアの色が
  言っているので同日に外した。`header_line` が幅に収まれば
  右寄せ、収まらなければ左に続けて切る
- 外したもの: コメント数、`[✎ 編集中]`、`[ソース]`、`[選択 a-b]`、未読。いずれも別の場所に
  印がある(検討欄の通り)
- 未着手: ヘッダの `未同期` は resync の一瞬でも点く(mark_desynced → reload で消える)。
  気になれば「N ms 以上続いたら」にする

### 実施記録: 4. トースト表示(2026-09-03)

- `view/toast.rs` を新設。`App::toast(msg)` / `toast_err(msg)` が
  `app.toast: Option<Toast>` に置き、`draw_toast` が **`ui()` の最後**(索引画面でも)に
  フッターの1行上へ中央寄せのバナーを描く。矩形だけ `Clear` するので
  レイアウトは動かない。4秒(`TOAST_SECS`)で `expire_toast` が落とし、
  `Esc` は `dismiss_toast` で即消す
- **寿命は最初に描かれたフレームから数える**(`Toast.shown` を描画側が埋める)。
  起動中の通知(設定ファイルが読めない)が画面の前に消えないため
- tachyonfx 0.25 を依存に足し、`fade_from → sleep → fade_to` を
  `CellFilter::BgColor(地色)` で**バナーのセルだけ**に限定した。地色は
  `theme::toast_bg(端末背景)`: 暗い端末は黒、真っ黒な端末と明るい端末は
  持ち上げた黒(44,44,48)。黒地固定の akapen と違うのは、真っ黒な端末で
  矩形が溶けるのを避けるため。他の帯(選択・EDIT の下敷き)と一致しない色で
  あることも要件(フェードの選別に使う)。ビープとフラッシュは入れていない
- **3分類の結果**: ①起きたこと(✓・コピー・`→ ページ名`・最新・undo/redo・
  開けません・コミット失敗・境界の「先頭です」・キーの拒否理由など約100箇所)
  → トースト。②いま何であるか(`選択中 …`・`N 行を選択 …`・コメント入力中・
  `⏪ 履歴位置`・`… アップロード中`・`ダウンロード中`・`ページを作成しています`・
  `コミット処理が停止しました`・起動時の `認証: …` 要約)→ `app.status` のまま。
  ③応答が要る確認 → 該当なし(確認系はオーバーレイ)。
  赤(`toast_err`)は失敗と拒否、黄は完了と境界と案内
- 付随の整理: `index_notice` を廃止(索引もトーストで言う)。`✓ label` を出す
  条件分岐(status が空/✓/EDIT のときだけ)と `WsEvent::Status` の
  「EDIT を潰さない」ガードは、トーストが status を押しのけないので**無条件**に
  なった。EDIT 開始時に status へ入れていた `EDIT — ↑↓ 移動 …` はフッターの
  バッジ・キーヒントと三重だったので消した(status を空にする)。アップロード・
  ダウンロードの「…中」は結果が届いた時点で status から消す
- tick: トーストが出ている間は 60ms(フェードのため)。web_notice(図の失敗
  メモ)は status より下位の独立枝のまま触っていない
- テスト: `tests/toast.rs`(status を押しのけない・寿命は初回描画から・矩形と
  切り詰め・TestBackend でページ/索引の両方にバナー行が乗る)。既存テストの
  `app.status` 参照は一過性のものを `app.toast_text()` へ

#### 続き(同日、「全部バナーは目立ちすぎる」の指摘を受けて)

一過性の通知を**種別5つ × 見せ方4段**に整理した。正本は `NOTE-notifications.md`。
無音(画面が答えている: ページ遷移・モード切替・選択解除)/ 薄く(`App::note`:
フッターのヒント欄に4秒、status の後ろに乗る。✓・コピー・境界・内部同期の実況)/
黄バナー(拒否と案内、他人の編集、そして**押しても何も起きない理由**——境界系は
最初「薄く」にしたが同日にここへ上げた)/ 赤バナー(失敗。8秒)。`web_notice` は「薄く」に
統合した。未決はコミット失敗を8秒で消してよいか(`未同期` の常駐表示が筋。
5. ヘッダの整理と一緒に)。
さらに同日: note はヒント欄に**上書き**(キー・status は脇に退く)へ変更、
トーストが出ている間は**カーソル行がバナーの行に入らない**よう視界をずらす
(`keep_cursor_above`)。

### 実施記録: 2. 画像の貼り付け(2026-09-03)

v1 の範囲は計画どおり「パス貼り付けだけ」。ヘルパは作っていない。

- **入口**は `session_paste` の1行分岐。`cosense::upload::image_path_from_paste`
  が「実在する画像ファイルのパス」と判定したら `App::start_upload` へ。
  端末が付ける飾り(末尾空白・`file://`・`\ `・引用符・`~`)は剥がす
- **送信**は lib の `Client::upload_gcs`(3往復。同一ファイルは upload-request
  で即 embedUrl)と `upload::upload_gyazo`(multipart。Teams は permalink を
  `<org>.gyazo.com/<id>` に組み直す)。reqwest に `multipart`、`md5`、`toml` を足した
- **行き先**は `Destination::resolve` で TOML > プロジェクト設定 > gcs。
  プロジェクト設定は `get_project_theme` を `get_project_settings` に育てて
  同じ1回の取得で theme と `uploadImageTo` / `gyazoTeamsName` を得る
  (`Ctx::project_settings` キャッシュ)。設定ファイルは `src/config.rs`
  (`~/.config/cosense-tui/config.toml`、`[upload]` と `[upload.project.<name>]`)。
  読めない TOML は既定にせず、起動時のステータスで言う
- **合流**は `start_related_load` / `drain_related` と同じ形(背景スレッド +
  チャネル + ページ照合)。行IDとキャレット位置を覚え、届いたときキャレットが
  その行なら編集バッファに差し込み(他の入力と一緒にコミット)、別の行なら
  その行の**いまの**本文へ Replace、行が無ければ末尾に Insert して理由を言う。
  前後に文字があれば空白を挟む(`splice_image`)
- **EDIT 中の status** はこれまでヒントに隠れていたので、MOVE と同じく
  ヒントの後ろに付ける形にした
- **見つけた落とし穴**: my-sandbox のプロジェクト設定は `gyazo` +
  team なし(個人)だが、環境にあるのは Teams トークンだけ。トークンで
  「どの Gyazo に入るか」が決まるのに permalink は行き先から組むので、代用
  すると 404 を指す。だから 2. で決めた「TEAMS 無ければ personal」の順は
  **読む側だけ**に残し、**上げる側は行き先に一致するトークンだけ**を使う
  (無ければ変数名を言って何もしない)。計画からの唯一の変更
- **実測**: `upload_smoke`(新規 bin)で my-sandbox へ gcs 送信し
  `https://scrapbox.io/files/<id>.png` が返ることを確認。Gyazo 側は
  トークンの所在(会社の Teams)から実送信は控え、テストで形だけ検証
- テストはネットワークに触れない: `App::uploads_on`(`related_fetch` と
  同じゲート)が実行時だけ true になる

**追記(同日)**: ユーザー要望で**クリップボードの画像そのもの**も入れた。
EDIT 中の `^v`。macOS は `scripts/pbimage.swift`(`ime.swift` と同じ経路で
初回に裏ビルド)がピクセルを PNG に書き出し、コピーされたのが画像**ファイル**
ならそのパスを返す。Linux は `wl-paste` / `xclip`。PNG は
`$TMPDIR/cosense-tui/clipboard/` に置いて上のアップロード経路に流し、読んだら
消す(`clipboard::is_scratch`)。理由(画像なし・ヘルパ準備中・swiftc なし・
ツールなし)は全部ステータスに出す。ヘルパは実クリップボードで3通り
(ピクセル・ファイル URL・テキスト)を確認済み。
なお Teams の `permalink_url` は今回の実測では正しい org の URL で返ってきた
(上の「404 になる」は再現せず)。コードはどちらでも動く形なので変更なし。

**追記(同日、ユーザー指摘)**: 「個人プロジェクトでも scrapbox に上がる」。調べると
「設定が読めない」場面が想定より広かった——非公開は sid 無しで 401、**公開でも
sid 無しの応答には `uploadImageTo` が無い**(acme-edu で実測。`gyazoTeamsName`
は入っているが、それだけでは Gyazo と決められない: 別のプロジェクト は team 名が
あって gcs)。縮退先が gcs なのは決めどおりだが、ステータスが `(gcs)` しか言わず
理由が分からなかったので、`Destination::resolve_with` で「誰が決めたか」を返し、
既定に落ちたときは「プロジェクト設定を読めないので既定。COSENSE_SID か
config.toml で決まる」と添えるようにした。

残り(やらない/後で): `uploadFileTo`(画像以外のファイル)、キーバインド設定の同居。

### 実施記録: 1. サイトトップ刷新(2026-09-02)

レイアウト(一覧+抜粋の配置・レスポンシブ)は現行のままでよいとユーザーが
判断したので、残る4つ(絞り込み・ソート・テロメア・全文検索)をやった。
参考にしたのは ashiato の `filter_active` / `on_filter_key` / `sort.next()`。

**絞り込みを `/` で開く1行にした**。従来は印字キーが全部フィルタ文字で、
(a) 日本語タイトルを IME で打つ間ずっと一覧が跳ね、(b) キーを1つも
余らせないので `q` が効かなかった。つまり PLAN の「一覧で q/^C で終了
できない」は単独では直せない不具合で、`/` と同じ1つの変更になる。

- 開いている間だけ印字キーが文字。Enter 確定(フィルタは残る)/ Esc 解除 /
  ↑↓ は打ちながらでも移動 / ^c は終了(ashiato も同じ例外を置いている)
- 閉じている間は `q`・`^c`・`/`・`s` が効き、j/k/g/G がフィルタの有無に
  よらず動く
- `^c` はページ側にも足した。raw mode ではシグナルではなくキーとして
  届くので、これが無いとどこでも死にキーだった。EDIT セッションと
  コメント入力欄には入れない(そこでの ^c は「取り消す」反射)

**ソートは `s` のメニュー**。updated / accessed / created / linked / views /
title の6つで、6つとも API の `sort` で server-side に効くのを実測した。
項目名は API の語彙なので英語のまま(ユーザーの指定)。

- **pin の落とし穴**: API は**どの sort でも**ピン留めページを先頭へ
  浮かせる(villagepump で sort=title / sort=linked とも実測)。だから
  並び順はローカルで再度かける。従来コードは「必ず updated で」
  再ソートしていて、そのまま残すと A→Z が日付順に崩れる
- 選ぶと**取り直す**。手元は最大500件なので、44927件のプロジェクトで
  ローカルに並べ替えると「間違った500件」を並べることになる
- 行の狭い欄は**いま並べている値**を出す(時刻系は相対経過、
  linked/views は件数)。見えない数字で並べても読み手には分からない

**関連ページ行のテロメアを本文と同じ2軸にした**(ユーザーの指定)。一覧の
行は既に「太さ=更新の新しさ / 色=未読」だったが、関連ページ行だけ意図的に
平坦な `▏`+色だけで、ガター1列が画面によって別の意味になっていた。日時を
持たないエントリ(プロジェクト横断リンク)は最も細い側へ寄せる。

**全文検索を `/` の1行に Tab で合流させた**(ユーザーの指定)。

- 検索応答は実測すると list と同じページメタデータを(`accessed` 以外)
  持ち、加えて**ヒットした行**を返す。だから結果はそのまま一覧の行になり、
  抜粋はヒット行になる。当初「title/words/lines だけ」と読んでいたのは
  古い構造体定義のせいで、実物はもっと返す
- 結果はサーバの関連度順のまま。`s` は効かず、押すとそう言う。`^u` で
  一覧へ戻り、`/` はそのまま「検索し直す」
- 打っている間はタイトルで絞らない。その語は本文にあるので、絞ると
  検索が見つけるはずのページを隠す
- 副産物: **一覧はステータス行を持っていなかった**ので、通知(0件・
  ソート不可・検索失敗)が完全に見えなかった。フッターのヒント欄に出して
  次のキーで消す形にした

## タスク着手時の様式(改善案の取り決め)

各タスクには「現在の動作 / 期待する動作 / 再現手順 / 完了条件 / 依存するタスク」を
書き足してから実装する。
