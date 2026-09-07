# view.tape — cosentty でページを読む(VHS)
#
#   vhs examples/demo/view.tape        # リポジトリのルートで
#
# 公開プロジェクト cosentty の「Cosenseの使い方」を開き、
# 行カーソルで読み進め、リンクを辿って戻り、ソース表示とヘルプを見せる。
# 認証不要。要 vhs(brew install vhs)と rust/target/release/cosentty。

Output examples/demo/out/view.gif
Output examples/demo/out/view.mp4

Set Shell bash
Set Framerate 20
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 40ms

# --- 起動(隠す) ---------------------------------------------------------------
Hide
Type "examples/demo/run.sh view" Enter
Wait+Screen@20s /Cosenseの使い方/
Sleep 3s
Show

# --- 読み進める -----------------------------------------------------------------
Sleep 2s
Type "jjjjjj"
Sleep 1.2s
Type "jjjjjj"
Sleep 1.5s

# --- 「使い方のサンプル」の /help-jp/サンプル リンクへ。Enter で別プロジェクトのページへ渡り、[ で戻る
Type "G"
Sleep 1s
Type "kkkkk"
Sleep 1.5s
Enter
Sleep 3.5s
Type "["
Sleep 2s

# --- z で生ソース表示を切り替える -----------------------------------------------
Type "z"
Sleep 2.5s
Type "z"
Sleep 1s

# --- G で本文末尾、その先の関連ページリストへ -------------------------------------
Type "G"
Sleep 1.5s
Type "jjj"
Sleep 2s

# --- ? でヘルプ -----------------------------------------------------------------
Type "?"
Sleep 3s
Escape
Sleep 1s

Hide
Type "q"
Type "q"
