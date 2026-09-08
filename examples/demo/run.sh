#!/bin/sh
# run.sh — VHS 撮影用に cosentty を起動する
#
#   examples/demo/run.sh view          # 公開ページ「Cosenseの使い方」を開く
#   examples/demo/run.sh new <project> # プロジェクトのページ一覧から始める(新規ページ作成のデモ用)
#
# バイナリは COSENTTY 環境変数 → rust/target/release → PATH の順に探す。
# 撮影では IME 切り替えが要らないので --ime off を既定で付ける
# (COSENTTY_OPTS で差し替え可)。
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)

if [ -n "${COSENTTY:-}" ]; then
    bin=$COSENTTY
elif [ -x "$repo_root/rust/target/release/cosentty" ]; then
    bin=$repo_root/rust/target/release/cosentty
elif command -v cosentty > /dev/null 2>&1; then
    bin=cosentty
else
    echo "error: cosentty が見つからない。cd rust && cargo build --release で作るか COSENTTY=/path を渡す" >&2
    exit 1
fi

# 端末は暗い前提で撮る(プロジェクトの明色テーマに追従すると EDIT の下敷きが白くなる)
opts=${COSENTTY_OPTS:---ime off --lang ja --dark}
# VHS の端末(xterm.js)は sixel を描けないので、画像は半ブロック文字で描かせる
export COSENSE_IMAGE=${COSENSE_IMAGE:-halfblocks}
scenario=${1:-view}
shift || true

case "$scenario" in
view) exec "$bin" $opts "https://scrapbox.io/cosentty/Cosense%E3%81%AE%E4%BD%BF%E3%81%84%E6%96%B9" ;;
new)  exec "$bin" $opts "${1:?project を指定する}" ;;
*)    echo "usage: $0 view | new <project>" >&2; exit 2 ;;
esac
