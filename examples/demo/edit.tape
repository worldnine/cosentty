# edit.tape — cosentty でページを作って書く(VHS)
#
#   COSENTTY_PROJECT=<自分の非公開プロジェクト> vhs examples/demo/edit.tape
#
# ページ一覧 → `/` で名前を打つ → 「＋ …を作成」→ そのまま EDIT で本文を書く。
# 実際にページができるので、自分の sandbox プロジェクトで撮り、終わったら消す。
# 認証は `cosense login` の PAT。

Output examples/demo/out/edit.gif
Output examples/demo/out/edit.mp4

Set Shell bash
Set Framerate 20
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 40ms

# --- 起動(隠す): プロジェクトのページ一覧 -----------------------------------------
Hide
Type "examples/demo/run.sh new $COSENTTY_PROJECT" Enter
Wait+Screen@20s /updated/
Sleep 1s
Show

# --- / で名前を打つ。一覧に無い名前なので最後に「＋ …を作成」が現れる ----------------
Sleep 1.5s
Type "/"
Sleep 600ms
Type@60ms "cosentty demo"
Sleep 1.5s
Enter
Sleep 800ms
Type "G"
Sleep 1.2s
Enter
Sleep 2s

# --- そのまま EDIT。1行書けばページが作られる ------------------------------------
Type@50ms "Cosense のページを端末から書く"
Sleep 800ms
Enter
# 1行目でページが作られる。作成の往復が返るまで待たないと、次の行の先頭が落ちる(要調査)
Sleep 1.5s
Type@50ms " [cosentty] は Cosense の TUI クライアント"
Sleep 800ms
Enter
Type@50ms "行を離れると自動でコミットされる。確認ゲートは無い"
Sleep 800ms
Enter
Type@50ms "#demo"
Sleep 1s
Escape
Sleep 2.5s

Hide
Type "q"
Type "q"
