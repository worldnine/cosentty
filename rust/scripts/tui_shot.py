#!/usr/bin/env python3
"""tui_shot — 端末を1枚の画面として読み取り、キーを送って TUI を確かめる。

TUI は「テストが緑」と「実際にそう見える」が別物なので、ここで実機の
画面そのものを読む。pty で view を起動し、pyte で端末エミュレーションを
して、任意の時点のスクリーンとハードウェアカーソルの位置・セルの属性
(太字・前景・背景)を取り出す。

    pip install pyte
    python3 rust/scripts/tui_shot.py <view のパス> [view の引数...]

既定は「起動して数秒待ち、画面を出して q で閉じる」。自分の手順を書く
ときは exec() で読み込んで、send()/pump()/render()/cells() を使う:

    exec(open('rust/scripts/tui_shot.py').read())
    pump(6.0)                 # 起動を待つ
    send(b"\\x0f", 3.0)        # ^o で一覧
    send(b"/", 1.0)           # / で絞り込み
    send("改善".encode(), 1.5)
    print(render())           # 画面ぜんぶ
    print(screen.cursor.x, screen.cursor.y, screen.cursor.hidden)
    print([c for c in cells(2) if c[3] != "default"])  # 敷きの付いたセル
    send(b"q", 1.0)

注意:
- 待ち時間は寛大に。ページ一覧が大きいプロジェクト(villagepump)は
  数秒かかる。短すぎるとキーが読み捨てられ、「動いていない」ように見える
- IME を触りたくないときは `--ime off` を付けて起動する。付けないと
  入力ソースを実際に切り替える
- 全角は pyte 上では「文字セル + 空セル」で並ぶので、render() の見た目は
  字間が空く。位置を測るときは cells() の x を見る
"""
import fcntl
import os
import pty
import select
import struct
import sys
import termios
import time

import pyte

ROWS, COLS = 30, 100

exe = sys.argv[1] if len(sys.argv) > 1 else "rust/target/debug/cosentty"
args = sys.argv[2:]

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.environ["LINES"] = str(ROWS)
    os.environ["COLUMNS"] = str(COLS)
    os.execv(exe, [exe] + args)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

screen = pyte.Screen(COLS, ROWS)
stream = pyte.Stream(screen)
# 端末が閉じた(= view が終了した)かどうか。`q` が効いたかを確かめるのに使う
# ——ただし判定は次の read まで遅れるので、見るなら十分に pump してから。
alive = [True]


def pump(seconds):
    """`seconds` のあいだ端末の出力を読み続ける。"""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if not r:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            alive[0] = False
            return
        if not chunk:
            alive[0] = False
            return
        stream.feed(chunk.decode("utf-8", "replace"))


def send(data, wait=0.5):
    """キーを送って、その結果が描かれるまで待つ。"""
    try:
        os.write(fd, data)
    except OSError:
        alive[0] = False
        return
    pump(wait)


def cells(y):
    """`y` 行の (x, 文字, 太字, 背景) — 空白は落とす。"""
    row = screen.buffer[y]
    return [
        (x, row[x].data, row[x].bold, row[x].bg)
        for x in range(screen.columns)
        if row[x].data and row[x].data != " "
    ]


def render():
    """画面ぜんぶを1つの文字列に。pyte の display は空セルで落ちることがある。"""
    out = []
    for y in range(screen.lines):
        line = "".join(
            (screen.buffer[y][x].data or " ") for x in range(screen.columns)
        )
        out.append(line.rstrip())
    return "\n".join(out)


if __name__ == "__main__" and not os.environ.get("TUI_SHOT_LIB"):
    pump(6.0)
    print(render())
    send(b"q", 1.0)
