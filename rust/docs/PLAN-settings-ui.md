# PLAN: 設定画面(`,`)と config.toml の拡張

起案日: 2026-09-08。動機は「`COSENSE_SID` なしでも使える範囲を広げる」調査
(同日)の結論から。SID が要る3用途のうち、**プロジェクト設定の読み取り**
(テーマ・表示名・アップロード先)は手元の設定ファイルで代替でき、その編集口
として TUI の設定画面を用意する。ws push 同期と web レンダラは SID 必須のまま
(Cosense サーバ側の制約。`NOTE-websocket-sync.md` の認証表を参照)。

## 現在地(何が、どこで決まっているか)

| 項目 | 現在の出どころ | SID なし・非公開プロジェクトでの挙動 |
|---|---|---|
| サイトテーマ(配色) | `/api/projects/<name>` の `theme`(`Ctx::project_theme`) | 端末配色のまま |
| 表示名(ヘッダ) | 同 `displayName`(`Ctx::project_display`) | URL スラッグを出す |
| 画像アップロード先 | `config.toml` `[upload]` > 同 `uploadImageTo` > `gcs`(`upload::Destination::resolve_with`) | **config.toml があれば決まる**(済) |

- `/api/projects/<name>` は公開プロジェクトなら無認証で読める。非公開では
  sid か Service Account だけ通る(`Client::get_project_settings`)
- 設定ファイルは `~/.config/cosentty/config.toml`(`cosense::config::Config`)。
  現在は `[upload]` セクションのみ。`deny_unknown_fields` で typo を弾く
- 読み込みは起動時1回、失敗はステータス行に1度だけ出す(`Ctx::config_error`)
- 依存は `toml = "0.8"`(読み専用の使い方)。`toml_edit` は間接依存にある

## ゴール

1. config.toml に **プロジェクトごとのテーマと表示名** を書けるようにし、
   API が読めないときの代わりにする(読めるときは API を優先。理由は後述)
2. TUI から `,` で **設定画面** を開き、いま見ているプロジェクトの
   テーマ・表示名・アップロード先を選んで保存できる
3. 保存は config.toml への**書き戻し**。手書きの並び・コメントを壊さない
4. 保存した瞬間に画面へ反映される(再起動不要)

やらないこと(このPLANの範囲外):

- 認証情報(PAT / SID)の入力。保存先と漏えい面の設計が別に要る
- 同期方式・ポーリング間隔の変更。API 負荷の上限設計が要る
- キーバインドの編集。`config.rs` のコメントで予告済みだが別タスク

## 1. config.toml の拡張

```toml
[upload]
images = "gcs"

[project.acme]              # 新設。キーは URL スラッグ
theme = "paper-dark"        # Cosense のテーマ名(下記の一覧から)
display_name = "ACME 社内wiki"
images = "gyazo"            # [upload.project.acme] と同じ意味(移行用の別名)
gyazo_team = "acme-inc"
```

- 新セクション `[project.<slug>]` に `theme` / `display_name` を持たせる。
  `Config` に `project: HashMap<String, ProjectSection>` を足す
- **アップロード先の置き場所を1つにまとめる**: 既存の `[upload.project.<slug>]`
  は残して読み続ける(後方互換)が、設定画面が書くのは `[project.<slug>]` 側。
  両方にあれば `[project.<slug>]` が勝つ。`upload_choice` の解決順に1段足すだけ
- テーマ名の検証はしない(未知の名前は「色付けなし」として扱う。
  `theme::tinted_page_palette` が `None` を返す経路がすでにある)。
  ただし設定画面の候補は既知の名前から出す(次節)

### 優先順位: API が読めるときは API

テーマと表示名は「本家の設定を写す」性質のものなので、**API > file > なし**。
アップロード先は現行どおり **file > API > gcs**(手元で Gyazo を避けたい等、
本家と違えたい理由が実在する。`upload.rs` のコメント参照)。
この非対称は `PLAN` で決めた意図なので、実装時に `Ctx::project_theme` の
doc コメントに残す。

具体的には `Ctx::project_settings` の結果が `None`(読めなかった)か、
`theme` が空のときだけ `config.project.get(slug).theme` を見る。

## 2. 設定画面(Overlay::Settings)

### 起動と場所

- READ と一覧で `,` を押す(未使用。vim 系エディタが設定に使う慣習に寄せる)。
  EDIT セッション中は印字キーなので開かない(`?` と同じ扱い)
- 既存の `Overlay` に `Settings { project: String, cursor: usize, editing: Option<Field> }`
  を足す。描画は `ui/overlay.rs`、キーは `keys.rs` のオーバーレイ節
- 対象プロジェクトは **いま表示しているページのプロジェクト**(一覧なら一覧の
  プロジェクト)。プロジェクトを切り替える UI は持たない(別プロジェクトの
  設定は、そのプロジェクトを開いて `,`)

### 画面

```
┌ 設定: acme ──────────────────────────────────────────────┐
│ 出どころ: API は読めません(非公開・sid なし)。file の値を使います │
│                                                            │
│ > テーマ         paper-dark         ← file                 │
│   表示名         ACME 社内wiki      ← file                 │
│   画像の保存先   gyazo (acme-inc)   ← file                 │
│                                                            │
│ j/k 移動 · Enter 変更 · d 既定に戻す · Esc/q 閉じる           │
│ 保存先 ~/.config/cosentty/config.toml                        │
└────────────────────────────────────────────────────────────┘
```

- 各行に **いま効いている値**と、**どこから来たか**(`api` / `file` / `既定`)を
  並べる。`Decided` 列挙(`upload.rs`)と同じ考え方をテーマ・表示名にも広げる。
  API の値が効いている行で `Enter` を押しても file に書けるが、
  「API が読める間は API が優先」の注記を出す
- テーマ行の `Enter` → 候補ピッカー(既存の `Overlay::Links` と同じ j/k+Enter)。
  候補は `theme.rs` の match 腕に現れる Cosense テーマ名を1か所の定数
  `COSENSE_THEMES: &[&str]` に集めて使う(`scripts/cosense-theme-vars.py` の
  生成物と揃える。手で増やさない)。先頭に「(設定しない)」
- 表示名行の `Enter` → 1行入力。既存の編集セッションの1行入力
  (ページピッカーの絞り込み行と同じ入力部品)を再利用する。IME 前提の操作系
  (`KEYMAP.md` 「日本語環境(IME)前提の操作体系」)に従い、確定は Enter、
  取り消しは Esc
- 画像の保存先行の `Enter` → `gcs` / `gyazo` の2択、`gyazo` なら続けて
  Teams 名の1行入力(空なら gyazo.com)
- `d` でその行の file の値を消す(「既定に戻す」= API か既定へ)。
  破壊的操作を大文字1キーに置かない方針(`KEYMAP.md`)に合わせ小文字 `d`

### 保存

- 各変更は**確定した時点で即保存**(設定画面に「保存」ボタンを置かない。
  Cosense 本家の設定画面と同じ即時反映の感触)
- 書き戻しは `toml_edit` を直接依存に足して行う。読み込みは `toml` のまま
  (型付き読みは `serde`、書きは文書編集、と役割を分ける)。手書きのコメント・
  並び・他セクションを保つ。ファイルが無ければ作る(ディレクトリも)
- 書き込みは一時ファイルに書いて `rename`(途中で落ちて空ファイルにしない)
- 失敗(権限・パース不能な既存ファイル)は赤バナーで理由を言い、画面の値は
  戻す。`config_error` がある状態(壊れたファイル)では設定画面を**開けるが
  保存を拒む**。上書きして手書きの内容を消してはいけない
- 保存後は `Ctx.config` を差し替え、`project_settings` キャッシュの当該
  プロジェクトを消してヘッダ・パレットを再計算する(`tinted_page_palette`
  はフレームごとに呼ばれているので、キャッシュ無効化だけで反映される)

### 他ファイルへの追随

- `KEYMAP.md` 対応表に `,` を1行、`## 設定ファイル` 節を新設して
  `[project.<slug>]` の書式を書く
- `help_keys` の READ 節に「設定  , プロジェクトの設定」を足す
- `README.md` の「認証」に、SID なしでもテーマ等は設定ファイルで出せると1行
- `HANDOFF.md` の SID の説明を更新(「プロジェクト設定の読み取り」は
  代替できる、と)

## 3. 実装の刻み方(1関心事 = 1コミット)

1. `config.rs`: `[project.<slug>]` の読み込みと `upload_choice` の解決順。
   テストは既存パターン(`parse` した結果の比較)で。`Config::path` の
   `XDG_CONFIG_HOME` 上書きで実ファイルに触れない
2. `theme.rs`: `COSENSE_THEMES` 定数と、match 腕が定数の部分集合であることの
   テスト
3. `main.rs` `Ctx`: `project_theme` / `project_display` に file フォールバック。
   `Decided` 相当の「出どころ」を返す `project_theme_with` を足す
4. `config.rs`(書き): `Config::write_project(slug, patch)`。`toml_edit` を
   直接依存に追加。テストは文字列→文字列で、コメントと他セクションが残ること
5. `Overlay::Settings` の状態・描画・キー。まず読み取り専用(値と出どころの表示)
6. 各行の変更操作と保存、反映
7. KEYMAP / help / README / HANDOFF

各段で `cargo test --bin cosentty` を green にしてから次へ。5 以降は
`<sandbox>` の非公開プロジェクトを **`COSENSE_SID` を外して**開き、
テーマが file から出ることを目で確かめる(SID を付けると API が勝つので、
その切り替わりも確認する)。

## 未決(実装時に決める)

- `,` は一覧画面でも開くか。一覧に「プロジェクト」の概念があるので開ける
  のが自然だが、一覧の絞り込み行が開いている間は絞り込み文字になる(`?` と同じ規則)
- `display_name` を file に書いたときのヘッダ表示。API が読めない間だけ
  使われるので、本家と食い違っても害は小さい。`[file]` の印を出すかは
  ヘッダの幅を見て決める
- 既存の `[upload.project.<slug>]` を設定画面が書き換えたときの扱い。
  「読むだけ、書くのは `[project.<slug>]`」で始め、両方に値があると
  紛らわしければ保存時に古い方を消す(`toml_edit` なら消せる)

## 実施記録

(未着手)
