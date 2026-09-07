# デモ動画の撮り方

[VHS](https://github.com/charmbracelet/vhs) の tape で端末操作を再現し、GIF / mp4 を作る。
`brew install vhs` と `cd rust && cargo build --release` が前提。リポジトリのルートで実行する。

```bash
vhs examples/demo/view.tape                                   # 公開ページを読む(認証不要)
COSENTTY_PROJECT=<sandbox> vhs examples/demo/edit.tape        # ページを作って書く(要 PAT)
```

- 出力は `examples/demo/out/`(git 管理外)。README に載せるものは `docs/demo-*.gif` へコピーする
- `edit.tape` は本当にページ `cosentty demo` を作る。自分の非公開プロジェクトで撮り、
  終わったら消す(cosense CLI の `previewDelete` → `submitEdit`)
- VHS の端末(xterm.js)は sixel を描けないので、`run.sh` が `COSENSE_IMAGE=halfblocks` を渡す
- 別のバイナリで撮るなら `COSENTTY=/path/to/cosentty`
