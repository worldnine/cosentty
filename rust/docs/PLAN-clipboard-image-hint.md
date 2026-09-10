# PLAN: クリップボードの画像を「⌘V で貼れない」問題の手当て

整理日: 2026-09-10。着手はまだ。`PLAN-next.md` 2.(画像の貼り付け)の追記から派生した。

## 背景(何が起きているか)

- 画像の貼り付けは EDIT 中の `^v`(Ctrl+V)。実機では正しく動く
  (2026-09-09 に tmux で確認: PNG をコピー → `^v` → 数秒で
  `[https://scrapbox.io/files/….png]` が行に入る。ヘルパ `pbimage` も
  ピクセル・ファイル URL・テキストの3通りで期待どおり)。
- それでも「動かない気がする」の正体は **⌘V**。テキストは ⌘V で貼れるので
  画像も同じ指で貼ろうとするが、⌘V は端末(Ghostty)自身のペーストで、
  クリップボードが画像だけのとき端末はアプリに何も送らない。押されたことすら
  分からないので無言になる。これは端末の原理で、⌘V に画像を載せる道はない。
- 先行例(Claude Code、neovim の img-clip)も「テキストは ⌘V、画像は Ctrl+V」で
  割り切っている。`^v` という割り当て自体は慣習と合っており変えない。
- 二次的な不具合: `start_upload` が置く「アップロード中…」の status が、
  直後に届く保存完了の `✓ new line` に上書きされて見えない
  (`rust/src/bin/view/upload.rs` の status 設定と、commit 完了の status の競合)。
  数秒の無反応が「動いていない」の印象を強める。

## 目的

1. ⌘V を押す**前**に「画像は ^v」が目に入るようにする(露出)。
2. クリップボードに画像が載っているときだけ、それを1回言う(検知)。
3. アップロード進行中の表示を保存完了の表示に負けさせない。
4. できれば ⌘V を押してしまった瞬間も捕まえる(端末の挙動次第)。

## 設計

### 1. フッタヒントに `^v 画像` を常時出す

`ui/chrome.rs` `hint_body` の EDIT 行に `^v 画像` を足す。いまは `?` の
ヘルプ(`ui/overlay.rs` `help_keys`)にしか無い。幅が足りない端末では
既存の省略規則に従う。KEYMAP の該当行も追随。

### 2. フォーカス復帰時に1回だけ probe し、あればヒント欄に言う

- **いつ**: 端末の FocusGained(`CSI I`)。画像をコピーする動作
  (スクリーンショット、ブラウザで画像をコピー)は必ず一度 cosentty から
  離れるので、戻ってきた瞬間が自然で、コストもその1回だけ。
  常時ポーリングはしない(プロセス起動が毎回数十 ms、見ていない間も
  クリップボードを覗くことになる)。
  `main.rs` の入力ループで `EnableFocusChange` を有効にし
  `Event::FocusGained` を受ける(現状は `_ => {}` で読み捨て)。
- **どの状態で**: EDIT 中(`session.is_some()` かつ `editable`)だけ。READ では
  貼れないので確かめない。
- **1回だけ**: macOS は `NSPasteboard.general.changeCount` を probe の応答に
  含め、前回見た値と同じなら言わない。同じ画像が載ったままフォーカスを
  何往復しても最初の1回で止まる。Linux は型一覧の文字列を同じ役に使う。
- **何を言う**: トーストではなく `note`(ヒント欄の上書き、次のキーで消える)。
  文言は `t!("クリップボードに画像があります — ^v で貼る", "image on the clipboard — ^v pastes it")`。
- **どう確かめる(probe と fetch を同じ判定にする)**:
  probe の完全性を追うのではなく、**probe が「ある」と言ったものは fetch が
  必ず取り出せる**ことを軸にする。型名の列挙(png / tiff)では片方だけ通る
  隙間が生まれる。
  - macOS: fetch は `NSImage(pasteboard:)` で読んでいるので、probe は
    `pasteboard.canReadObject(forClasses: [NSImage.self])` を使う
    (PNG・TIFF・JPEG・GIF・HEIC・BMP・PDF、14 以降は WebP まで同じ集合)。
    Finder のファイルコピー(`public.file-url`)は URL 文字列を読んで拡張子を
    見る——fetch 側に既にある分岐を関数に切り出して probe と共用する。
    PDF が「画像あり」になるのは fetch も同じ挙動なので揃えて通す。
    ヘルパ `scripts/pbimage.swift` に `--probe` を足し、
    `<changeCount>\t<yes|no>` を出す。ヘルパの hash が変わるので初回に再ビルド
    (`HelperBuilding` のときは何も言わない)。
  - Linux: fetch は `image/png` しか受けないので、probe も
    `wl-paste --list-types` / `xclip -t TARGETS -o` に `image/png` が
    あるときだけ「ある」。JPEG だけを載せるアプリを拾いたければ fetch 側に
    変換を足すのが先。
- **置き場所**: `cosense::clipboard::probe(prev: Option<&str>) -> Probe`
  (`Present { stamp }` / `Absent { stamp }` / `Unknown`)。`App` に
  `clipboard_stamp: Option<String>` を持つ。テストは `uploads_on` と同じ
  ゲートで OS に触れないようにする。

### 3. アップロード中の status を守る

commit 完了が status を書くとき、進行中のアップロードがあれば上書きしない
(`upload_pending: usize` を App に持ち、`drain_uploads` で減らす)。あるいは
アップロード進行を status ではなく別スロットに置く。前者が小さい。

### 4. 空の bracketed paste を捕まえる(要実測)

端末によってはクリップボードが画像だけでも空の `ESC[200~ ESC[201~` を送る。
`handle_paste` に空文字列が来たら EDIT 中なら `paste_clipboard_image` を呼ぶ
(=⌘V のまま画像が上がる)。**Ghostty が空ペーストを送るかは未確認**。
tmux 越しでは ⌘V を再現できないので、実端末で `COSENTTY_DEBUG_LOG` 相当に
Paste イベントを記録して確かめる。送らない端末では 1〜3 に頼る。

## 実装順

1. (3) status の競合 — 最小で、単体で価値がある
2. (1) ヒント露出 — 1行と KEYMAP
3. (2) probe — ヘルパ拡張・FocusGained・note
4. (4) 実測してから決める

## 検証

- tmux で実機: `set -g focus-events on` を入れないと FocusGained が
  tmux で止まる。`tmux new-session … ; tmux send-keys` の手順は
  memory の tui-verify-with-tmux と同じ。フォーカス復帰は
  `printf '\e[I'` を pane に流して模擬できる。
- ヘルパ単体: 画像・ファイル URL・テキスト・空の4通りで `--probe` の応答と
  `pbimage <out>` の成否が一致すること。
- 一度言ったら同じ changeCount では二度言わないこと。
