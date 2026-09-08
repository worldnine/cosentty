# PLAN: 端末側の設定 `[view]` と設定画面の2段構成

起案日: 2026-09-08。`PLAN-settings-ui.md`(プロジェクトごとの設定。実装済み)の続き。
動機は3つ。

1. **不具合**: プロジェクト一覧(`^o` を2回押した階層)で `,` を押すと、対象の
   スラッグが空のまま設定画面が開く。そこで保存すると `[project.""]` が書かれる
2. **グローバルな設定**を config.toml に置きたい。いま起動オプションと環境変数で
   しか決められない言語・配色・一覧の抜粋・IME・保存先を、ファイルにも書けるようにする
3. それを `,` の設定画面から変えたい。**起動オプションが与えられていればそれが優先**

## 現在地(どこで決まっているか)

| 項目 | 起動オプション | 環境変数 | 既定 | 適用点 |
|---|---|---|---|---|
| 言語 | `--lang ja\|en` | `COSENSE_LANG` → `LC_ALL` → `LC_MESSAGES` → `LANG` | en | `lang::detect` → `set_for_thread`。文言は描画時に `t!` が引くので切替は即時 |
| 配色テーマ | `--theme NAME` | — | 端末配色 | `Highlighter::new(theme, light)` と `Palette::for_light`。**Ctx に固定** |
| 明暗 | `--light` / `--dark` | — | OSC 11 で自動判定 | `Ctx.light` / `Ctx.terminal_bg`。**Ctx に固定** |
| 一覧の抜粋 | `--preview on\|off\|auto` | — | auto | `Ctx.preview`。描画時に参照 |
| IME | `--ime jp\|off` | — | jp | `Ctx.ime_mode`。入力欄を開くたびに参照 |
| 保存先 | `--download-dir` | `COSENSE_DOWNLOAD_DIR` → `XDG_DOWNLOAD_DIR` | `~/Downloads` | `Ctx.download_dir` |
| コメントの送り先 | `--send-cmd` | (herdr 検出) | なし | `Ctx.send_target` |
| 図の描画 | — | `COSENSE_WEB_RENDER=off\|manual\|auto` | manual | `RenderPolicy::from_env` |

`Ctx` は全ハンドラに `&Ctx` で渡っていて、いまは `config` だけが `Mutex` で差し替え
可能(前回の実装)。他の項目を実行中に変えるには同じ扱いにする必要がある。

## 1. 設計判断: 優先順位

```
起動オプション  >  環境変数  >  config.toml [view]  >  自動判定 / 既定
```

- ユーザーの要望どおり**オプションが最優先**。環境変数はシェルや herdr の
  ワークスペースごとに切り替える用途があるので、ファイルより上に置く
  (git・ripgrep・bat 等の慣習と同じ。`XDG_*` もこの位置)
- ファイルは「いつもの自分の設定」。何も指定しなければこれが効く
- **出どころは必ず画面に見せる**。前回の `Origin`(api / file / -)を拡張し、
  `flag` / `env` / `file` / `auto`(OSC 11 の判定など)/ `-`(既定)の5種にする。
  `flag` や `env` で決まっている行は、file に書いても**いまは効かない**ことを
  その行の注記で言う(保存自体は許す。次にオプション無しで起動したときに効く)

## 2. config.toml の `[view]`

```toml
[view]
lang = "ja"                  # ja | en
theme = "Solarized (dark)"   # --theme と同じ名前(two-face 同梱テーマ)
appearance = "auto"          # light | dark | auto(OSC 11 で判定。既定)
preview = "auto"             # on | off | auto
ime = "jp"                   # jp | off
download_dir = "~/Downloads" # ~ と $VAR を展開する
diagrams = "manual"          # off | manual | auto(COSENSE_WEB_RENDER と同じ)
```

- セクション名は `[view]`。バイナリ名は `cosentty` だが、KEYMAP の起動節が
  `view <project>` と呼んでいる「見る側の設定」に合わせる。`[upload]` / `[project.*]`
  と並んで3つ目のテーブル
- `send_cmd` は**入れない**。シェルコマンドを設定ファイルから実行する経路は、
  ファイルを書ける第三者にコマンド実行を許すことになる(lazygit の
  `customCommands` と同じ議論)。起動オプションのままにする
- 認証(`COSENSE_PAT` / `COSENSE_SID`)も入れない。前回の PLAN と同じ理由
  (`~/.cosense/settings.json` は公式 CLI の領分。秘密はこのファイルに置かない)
- `deny_unknown_fields` は維持。typo が黙って既定に落ちない
- 値の検証は **読み込み時にまとめて**行い、不正な値はその項目だけ既定に落として
  起動時に1度だけ言う(`config_error` と同じ経路。ファイル全体を捨てない)。
  設定画面からはピッカーで選ぶので不正値は入らない

### 解決の実装

`main.rs` の引数解析の直後、いまはオプション→環境変数→既定の順で各値を
決めている。ここに file を1段差し込む。

```rust
// 例: 言語
let (lang, origin) = pick(
    flag_lang.map(|v| (v, Origin::Flag)),
    env("COSENSE_LANG").or_else(locale_env).map(|v| (v, Origin::Env)),
    config.view.lang.map(|v| (v, Origin::File)),
    (Lang::En, Origin::Default),
);
```

`pick` は最初の `Some` を返すだけの小さな関数。項目ごとに `(値, Origin)` を持ち、
`ViewSettings` にまとめて `Ctx` に置く。

## 3. Ctx の持ち方: 「実行中に変わるもの」を1つの構造体にまとめる

いま `Ctx` に直接ある `hl` / `palette` / `light` / `terminal_bg` / `ime_mode` /
`preview` / `download_dir` を `ViewSettings` に移し、`Ctx.view: RwLock<ViewSettings>`
にする。`Ctx.config` と同じく、読むときは短く借りてコピーを取る。

```rust
pub(crate) struct ViewSettings {
    pub lang: (Lang, Origin),
    pub theme: (Option<String>, Origin),
    pub appearance: (Appearance, Origin),   // Light | Dark | Auto
    pub preview: (PreviewMode, Origin),
    pub ime: (ImeMode, Origin),
    pub download_dir: (PathBuf, Origin),
    pub diagrams: (RenderPolicy, Origin),
    // 派生値(設定から計算するもの)
    pub light: bool,                        // appearance と OSC 11 から
    pub terminal_bg: (u8, u8, u8),
    pub hl: Arc<Highlighter>,               // theme と light から
    pub palette: Palette,
}
```

- `hl` は `Arc` にする。`Highlighter` はテーマ集合を持つので複製したくないが、
  ページ描画のたびに `RwLock` を長く握るのも避けたい
- **変更時の再計算は1か所** `ViewSettings::recompute(&mut self)`。theme か
  appearance が変わったら `hl` / `palette` を作り直す
- 呼び出し側は `ctx.hl` を `ctx.view().hl` に書き換える程度で、`nav.rs` /
  `editing.rs` / `sync.rs` / `ui/*` に散らばる参照は機械的に直る
  (`rg 'ctx\.(hl|palette|light|terminal_bg|ime_mode|preview|download_dir)'`)

### 変えたときに何を作り直すか

| 項目 | 反映 |
|---|---|
| lang | `set_for_thread` を呼ぶだけ。文言は描画時に引く。**ステータス行の固定文字列**(`app.status` に入れ済みのもの)は次の更新まで旧言語のまま残る。許容する |
| theme / appearance | `recompute` → `app.palette` を作り直し(`tinted_page_palette` を通す)→ `rerender`。ヘッダ色も `project_header_colors(theme, terminal_bg)` で再計算。一覧を開いていれば一覧も次フレームで追随 |
| preview | 次フレームで効く(描画時に参照) |
| ime | 次に入力欄を開いたときから効く |
| download_dir | 次の保存から効く。存在しないディレクトリは保存時に作る(いまの `pick_download_dir` の挙動を踏襲) |
| diagrams | `RenderPolicy` の差し替え。`off` → `manual|auto` はバックエンド起動が要るので、**この項目だけ「再起動後に有効」と注記**して保存のみ行う。逆(`auto` → `off`)は次の描画から止められる |

## 4. 設定画面の2段構成

```
┌ 設定 ────────────────────────────────────────────────────────────────────┐
│ ── この端末 (config.toml [view]) ──                                        │
│ > 言語          ja                    ← flag   起動オプションが優先          │
│   配色テーマ    Solarized (dark)      ← file                                │
│   明暗          auto (dark)           ← auto                                │
│   一覧の抜粋    auto                  ← -                                   │
│   IME           jp                    ← -                                   │
│   保存先        ~/Downloads           ← env                                 │
│   図の描画      manual                ← -      変更は次回起動から            │
│ ── プロジェクト: acme ──                                                   │
│   テーマ        paper-dark            ← file                                │
│   表示名        ACME 社内wiki         ← file                                │
│   画像の保存先  gcs                   ← -                                   │
│ j/k 移動 · Enter 変更 · d 既定に戻す · e config.toml を $EDITOR で · Esc    │
└──────────────────────────────────────────────────────────────────────────┘
```

- **1枚のパネルに2つの節**。節見出しはヘルプ(`── READ ──`)と同じ記法で、
  カーソルは見出しを飛ばす。パネルの位置・幅は前回と同じ(中央、最大90桁)
- **プロジェクト一覧では下の節を出さない**(項目1の修正)。上の節だけになる
- 節の順は「端末 → プロジェクト」。上が自分の環境、下が見ている対象、という
  空間の記憶を固定する(TUI の原則: パネルの位置を状況で入れ替えない)
- 出どころ列の後ろに**短い注記**を1つだけ置ける。`flag` / `env` の行は
  「起動オプションが優先」「環境変数が優先」、`diagrams` は「次回起動から」
- `d` は file の値を消す。`flag` の行で押しても file の値が消えるだけで、
  効いている値は変わらない(注記がそう言う)
- **`e` で config.toml を `$EDITOR` で開く**(lazygit 流)。ピッカーに無い
  細かい編集や、コメント付きで書きたい人の逃げ道。戻ってきたら再読込して
  画面に反映する(`$EDITOR` 往復は `editor_roundtrip` の経路を再利用)

### 項目ごとの操作

| 項目 | 操作 | 候補 |
|---|---|---|
| 言語 | ピッカー | (設定しない) / ja / en |
| 配色テーマ | ピッカー | (設定しない) + two-face 同梱テーマ名。`Highlighter` にテーマ名の一覧関数を足す。**選んだ瞬間に本文へ適用**するので、j/k で流し見して決められる(プレビュー = 本番。Esc で元に戻す) |
| 明暗 | ピッカー | (設定しない) / light / dark / auto |
| 一覧の抜粋 | ピッカー | (設定しない) / on / off / auto |
| IME | ピッカー | (設定しない) / jp / off |
| 保存先 | 1行入力 | `~` と `$VAR` は保存時に展開しない(そのまま書く)。表示は展開後 |
| 図の描画 | ピッカー | (設定しない) / off / manual / auto |

配色テーマの「流し見して決める」は、ピッカーのカーソル移動で `apply_preview`
を呼び、Enter で確定・Esc で開く前の値に戻す。**確定前は file に書かない**。
htop の F2 と同じ live-apply で、保存は確定時に暗黙に行う(前回と同じ方針)。

## 5. TUI としての判断(ベストプラクティスとの照合)

`tui-design` スキルの原則に当てて、この設計で採る/採らないを決めた。

- **Save/Cancel を置かない**。TUI で定着している設定画面は「即時反映(htop)」か
  「`$EDITOR` に委ねる(lazygit)」の2つで、GUI 風の保存・取消・変更フラグは
  前例がない。前回どおり即時反映を主、`e` で `$EDITOR` を副として両方を置く
- **検証は確定時**。ピッカーで選ぶ項目は不正値が入らない。1行入力(保存先)は
  Enter で検証し、打鍵ごとには何も言わない
- **色だけで伝えない**。出どころは `flag` / `env` / `file` の文字で言う。
  効いていない行を dim にするのは補助
- **足元の確認(80×24)**: パネル幅は 80 桁なら 72。行は
  `ラベル(12) 値(22) ← 出どころ(5) 注記` で 72 に収まるよう値を末尾省略する。
  高さは 見出し2 + 行10 + 注記2 + 脚注1 + 枠2 = 17 で 24 行に入る。
  60 桁の tmux 分割では注記を落とし、値を 14 桁で省略する。
  `draw_menu_panel` にはこの省略が無いので、行を組む側(`settings_lines`)で
  幅を受け取って切る
- **`,` の意味を固定**する。どこで押しても「設定」。プロジェクト一覧では
  上の節だけが出る、という差だけ
- **フッタは5つまで**。`j/k` `Enter` `d` `e` `Esc`。`?` はオーバーレイ共通なので書かない
- 配色テーマの流し見は、helix の `:theme` 補完中プレビューと同じ体験。
  Esc で元に戻ることを脚注で言う

## 6. 実装の刻み(1関心事 = 1コミット)

1. **不具合修正**: プロジェクト一覧で `,` → 何も開かずトースト「プロジェクトを
   開いてから」。テスト1件。**これだけ先に main へ**(他と独立)
2. `config.rs`: `[view]` の読み込み(`ViewSection`)。値は文字列のまま持ち、
   解釈は 3 で。`with_key` を `[view]` にも使えるよう `save_key(table, key, value)`
   に一般化(`[project.<slug>]` は table = `project.<slug>`)
3. `ViewSettings` と `Origin` の拡張(flag / env / auto)。`main.rs` の解決を
   `pick` に寄せ、`Ctx.view: RwLock<ViewSettings>` に移す。参照の置き換え。
   **ここが最大の差分**だが挙動は変わらないので、既存テスト green が確認になる
4. `Highlighter::theme_names()`。two-face の埋め込み集合から列挙
5. 設定画面の2段化: 節見出し、上の節の行、一覧では下を出さない。読み取り専用で先に
6. 各行の変更と即時反映。配色テーマの流し見
7. `e` で `$EDITOR`。戻ったら再読込
8. KEYMAP(起動節に「設定ファイルでも決められる」と優先順位、設定節に `[view]`)、
   README、HANDOFF、この PLAN の実施記録

各段で `cargo test --bin cosentty` green。5 以降は tmux で実機確認
(`XDG_CONFIG_HOME` を scratch に向ける。手順はメモリ `tui-verify-with-tmux`)。

## 未決(実装時に決める)

- `appearance = auto` のとき、OSC 11 の判定は起動時の1回だけ。設定画面で
  `auto` に戻したときに再問い合わせするか、起動時の値を使い続けるか。
  再問い合わせは端末との往復が要るので、**起動時の値を使い続ける**方に寄せる
- 配色テーマの候補は two-face の全テーマ(数十個)。ピッカーが長くなるので、
  `/` で絞り込めるようにするか。まず無しで出し、長ければ足す
- `lang` を切り替えたとき、ヘルプの幅テスト(26行・88桁)は両言語で通っているので
  描画は崩れない。`app.status` の残りは許容と決めたが、切替時に `status.clear()`
  だけ呼ぶ手もある

## 実施記録

2026-09-08 実装(同日起案。ブランチ `settings-global`)。刻みは計画の 1〜8 を
5コミットに集約(4 と 5〜7 をまとめた):

1. 不具合修正: プロジェクト一覧で `,` はトースト → **後で上書き**(2段化で「上の節
   だけ出す」に変えた。計画の項目1は最終形では「開くが下の節が無い」)。単独で main へ
2. `config.rs`: `[view]`(`ViewSection`。値は文字列のまま)、`Origin` に flag / env /
   auto、`pick`、`with_key` / `save_key` をテーブル一般に
3. `view_settings.rs`(新規): `ViewSettings::resolve` と `recompute`、`ViewFlags`。
   `Ctx` の `hl` / `palette` / `light` / `terminal_bg` / `ime_mode` / `preview` /
   `download_dir` を `RwLock<ViewSettings>` とアクセサに置換。参照は機械的に
   `ctx.x` → `ctx.x()`。`flags` と `default_download` を `ViewSettings` に持たせ、
   保存後の `reresolve` で flag / env が引き続き勝つ
4. `settings.rs` 書き直し: `SettingField` 10 種、`Target`(View / Project)、
   `settings_items(view, width)` が幅に合わせて行を組む。配色テーマの流し見は
   `Pick.revert` に開く前の file 値を持ち Esc で `preview_theme` に戻す。
   `e` は `SettingsOutcome::EditConfig` → `Action::EditConfig` →
   `config_editor_roundtrip`(paste.rs の `editor_roundtrip` と同じ手順)→
   `reload_config_from`。`config_error` も Mutex にして直した後に保存できるように
5. KEYMAP / ヘルプ / README / HANDOFF

計画からの変更点:

- IME の変更は「次回起動から完全反映」ではなく、入力欄を開くときの切替
  (`ImeGuard::enter(ctx.ime_mode())`)は即時。セッション全体の IME 復元
  (`SessionIme`)だけは起動時の値のまま(差し替えると保存した入力ソースの復元が
  走るため触らない)
- `appearance = auto` に戻したときは起動時の OSC 11 の値を使う(未決どおり)
- 配色テーマのピッカーに絞り込みは付けていない(同梱テーマは 30 弱で足りた)
- `pick_download_dir` は「名前が無いときの探索」にだけ残し、名前の解決は
  `ViewSettings::resolve` 側に寄せた

追記(同日、実装後の指摘から): 配色テーマの候補は依存クレート two-face(bat の
テーマ集)の 32 種で、選別はしていない。Tokyo Night は bat に無いので入っていない。
また `--theme` は昔から名前を検証せず、未知の名前は**黙って**既定に落ちていた。
両方を直した(コミット「ユーザー定義テーマ」):

- `highlight::UserThemes`: `~/.config/cosentty/themes/*.tmTheme` を起動時に1度読む。
  名前は `name` かファイル名、同梱と同名ならユーザー側が勝つ。読めないファイルは
  `user_theme_errors()` に集めて起動時に1度トースト
- `Highlighter::theme_names()` はユーザー→同梱の順、`theme_exists(name)` を追加
- `ViewSettings.theme_missing`: 名前が無いとき `resolve` が注記を返し、設定画面の
  テーマ行に「見つからず既定を使用」と出る。値は書いた名前のまま見せる
  (何を書いたかが分かるように)。Tokyo Night の同梱は見送り(ユーザー定義で足りる)

追記(同日): `.tmTheme` を自分で置く先は実質 bat の `themes/` に集約されている
(delta も bat のを使う)ので、cosentty 専用ディレクトリだけだと二重管理になる。
`highlight::theme_dirs()` で cosentty の themes/ → bat の themes/(`$BAT_CONFIG_DIR` →
`$XDG_CONFIG_HOME/bat` → `~/.config/bat` → macOS は `~/Library/Application Support/bat`)
の順に読み、先のディレクトリが同名で勝つ。`bat` コマンドの有無は問わない。
名前は当初ファイル内の `name` を優先していたが、Tokyo Night の4変種(night / storm /
day / moon)が全部 `TokyoNight` で衝突して1つしか出なかったので、**ファイル名**に
統一した(bat と同じ。`bat --theme tokyonight_night` の名前がそのまま使える)。
出どころの明示は「全行に印」ではなく、ピッカーの**節見出し**(ディレクトリ名 /
同梱)と、**同梱と同名のときだけ**の行注記にした(`Pick.headings` で見出しを飛ぶ。
`Highlighter::theme_groups` / `user_theme_source`)。読めたテーマの数は起動時に言わない。

テスト: `cargo test --bin cosentty` 386 件 green(設定画面 13 件・解決順 1 件・
config 11 件・highlight 1 件)。実機は tmux で `XDG_CONFIG_HOME` を scratch に向けて確認。
