# デモ動画の撮り方

[VHS](https://github.com/charmbracelet/vhs) の tape で端末操作を再現し、GIF / mp4 を作る。
`brew install vhs` と `cd rust && cargo build --release` が前提。リポジトリのルートで実行する。

```bash
vhs examples/demo/view.tape                                   # 公開ページを読む(認証不要)
COSENTTY_PROJECT=<sandbox> vhs examples/demo/edit.tape        # ページを作って書く(要 PAT)
```

- 出力は `examples/demo/out/`(git 管理外)。README に載せるものは `docs/demo-*.gif` へコピーする

## ブラウザと並べる

```bash
cd examples/demo && npm install && cd ../..         # Playwright(初回だけ)
COSENTTY_PROJECT=<sandbox> COSENSE_SID=... node examples/demo/side-by-side.mjs
```

- 素の Chromium(拡張・ブックマーク無し、1280x800、ヘッドレス)を Playwright で録画し、
  同じスクリプトから `edit.tape` を VHS で走らせ、ffmpeg で左右に並べて `out/side.mp4` / `out/side.gif` を書く
- ブラウザはプロジェクトのトップで待ち、API でページができたのを見てからそのページへ移る。
  まだ無いページを開いておいても web は作成を拾わない(実測)。以降の行は ws で届いてその場に現れる
- VHS の動画は tape に書いた時間どおりだが実行の実時間は約 2 倍に伸びるので、VHS が出力する
  コマンド行の時刻を拾い、区間ごとにブラウザ動画を伸縮して合わせている
- UserScript の確認バナーは CSS で隠す。`COSENSE_SID` はブラウザのログインに使う(非公開プロジェクト用)
- `edit.tape` は本当にページ `cosentty demo` を作る。自分の非公開プロジェクトで撮り、
  終わったら消す(cosense CLI の `previewDelete` → `submitEdit`)
- VHS の端末(xterm.js)は sixel を描けないので、`run.sh` が `COSENSE_IMAGE=halfblocks` を渡す
- 別のバイナリで撮るなら `COSENTTY=/path/to/cosentty`
