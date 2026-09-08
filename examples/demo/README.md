# デモ動画の撮り方

操作は VHS の tape(`view.tape` / `edit.tape`)に書いてある。再生と録画は 2 通り。

## WezTerm で撮る(README のもの)

```bash
brew install --cask wezterm@nightly                 # 安定版は 2024 年で止まっているので nightly
cd examples/demo && npm install && cd ../..         # Playwright(初回だけ)
cd rust && cargo build --release && cd ..
node examples/demo/record-terminal.mjs examples/demo/view.tape      # 端末だけ → out/view.mp4, out/view.gif
COSENTTY_PROJECT=cosentty node examples/demo/side-by-side.mjs      # 端末 + ブラウザ → out/side.mp4, out/side.gif
```

- `record-terminal.mjs` が tape を読み、`wezterm cli send-text` でキーを流し、`wezterm cli get-text` で
  `Wait+Screen` を判定し、macOS の `screencapture -v` で窓の領域を録画する。画像は本物の端末が描くので
  くっきり出る(VHS の xterm.js では半ブロック文字になる)
- 初回は「画面収録」と「アクセシビリティ」(窓の位置取り)の許可をスクリプトを動かした端末に出す。
  撮っている間は WezTerm の窓が前面に見えている必要がある
- 端末の見た目は `wezterm.lua`(Guguru Sans Code 18pt、暗い配色、余白 12px、窓枠なし)
- 画像は `COSENSE_IMAGE=iterm2` で送る。WezTerm は kitty 画像の Unicode プレースホルダ
  (ratatui-image の kitty 経路)を描けない
- `side-by-side.mjs` は素の Chromium(拡張・ブックマーク無し、1280x800、ヘッドレス)を Playwright で
  録画し、端末の録画区間をそのまま切り出して ffmpeg で左右に並べる。どちらも実時間なので時間合わせは要らない
- ブラウザはプロジェクトのトップで待ち、API でページができたのを見てからそのページへ移る。
  まだ無いページを開いておいても web は作成を拾わない(実測)。以降の行は ws で届いてその場に現れる
- UserScript の確認バナーは CSS で隠す。`COSENSE_SID` はブラウザのログインに使う(非公開プロジェクト用)
- README のものは公開プロジェクト `cosentty` で撮っている(個人プロジェクトの一覧が映らないように)。
  `edit.tape` は本当にページ `cosentty demo` を作るので、撮ったあと消す
  (cosense CLI の `previewDelete` → `submitEdit`)
- 端末は `--dark` で撮る(`run.sh` の既定)。プロジェクトの明色テーマに追従すると EDIT の下敷きが白く浮く

## VHS で撮る

```bash
brew install vhs
COSENSE_IMAGE=halfblocks vhs examples/demo/view.tape
COSENSE_IMAGE=halfblocks COSENTTY_PROJECT=<sandbox> vhs examples/demo/edit.tape
```

VHS の端末(xterm.js)は画像を描けないので、半ブロック文字で描かせる。

## 共通

- 出力は `examples/demo/out/`(git 管理外)。README に載せるものは `docs/demo-*.gif` へコピーする
- 別のバイナリで撮るなら `COSENTTY=/path/to/cosentty`
