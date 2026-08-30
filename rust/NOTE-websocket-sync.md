# TASK: websocket push 同期（socket.io）の実装

3秒ポーリング（`spawn_web_poller`）を、`connect.sid` があるときは websocket push
（サブ秒・差分適用）に置き換える。sid が無い（PAT のみ）環境では現行ポーリングに
フォールバックする。**プロトコルは 2026-08-28 に実機で完全偵察済み**。以下が全情報。

## 偵察済みプロトコル（scrapbox.io、実測）

生の websocket（TLS 443）で以下のフレームを話すだけ。socket.io クレートは不要。
tungstenite (blocking) + 手書きフレーミングを推奨（依存は tungstenite のみ）。

```
接続:  GET /socket.io/?EIO=4&transport=websocket HTTP/1.1
       Host: scrapbox.io / Origin: https://scrapbox.io
       Cookie: connect.sid=<sid>          ← 認証はこれだけが通る（重要）
       （polling transport は "Transport unknown" で封鎖されている）

受信:  0{"sid":...,"pingInterval":25000,"pingTimeout":20000}   ← engine.io open
送信:  40                                                      ← namespace 接続
受信:  40{"sid":...}                                           ← 成功
       （sid cookie 無しだと 44{"message":"You are not logged in yet."}）

送信:  420["socket.io-request",{"method":"room:join","data":{
         "projectId":"<projectId>","pageId":"<pageId>","projectUpdatesStream":false}}]
受信:  430[{"data":{"success":true,...}}]                      ← join ack

受信:  2        → 3 を返す（engine.io ping/pong。25s間隔、20s timeout）
受信:  42["commit",{"kind":"page","parentId":"<commitId>",
         "changes":[{"_insert":"_end","lines":{"id":"...","text":"..."}},
                    {"_update":"<lineId>","lines":{"text":"..."}},
                    {"_delete":"<lineId>","lines":{"origText":"..."}},
                    {"linesCount":31},{"charsCount":385}],
         "pageId":"...","userId":"...","projectId":"..."}]
```

- **changes はうちの `EditOp` とほぼ同じワイヤ形式**。`_insert`/`_update`/`_delete` を
  `EditOp` にパースして既存の `cosense::editops::apply_ops` に食わせれば差分適用完了。
  `linesCount`/`charsCount` などメタ変更エントリは無視する。
  `_insert` の anchor は `__end` ではなく `_end`。タイトル変更 commit には
  `{"title":...}` 的なエントリが来る可能性あり → 未知キーは無視し、必要なら
  ポーリング1回で回収（下記セーフティネット）。
- projectId は `/api/projects/<name>/users` の `projectId`（PAT可。
  `/api/projects/<name>` 自体は PAT 不可 = 401。CLI と同じ回避）。
- クライアント→サーバのフレームは **必ずマスク**（RFC6455）。

## 認証の制約（実測で確定）

| 方式 | REST | websocket |
|---|---|---|
| PAT (`x-personal-access-token`) | ✓ | ✗（handshake header / auth payload / Bearer 全滅） |
| Service Account | ✓ | 未検証（おそらく✗） |
| `connect.sid` cookie | ✓ | ✓ |

→ `AuthStore`（`src/api.rs`）に sid が入っている場合のみ websocket を起動。
無ければ現行 `spawn_web_poller` を使う。**起動時のステータス行に `sync: ws` /
`sync: poll` を出す**と分かりやすい。

## 統合ポイント（現行コード）

- `rust/src/bin/view.rs`
  - `spawn_web_poller` / `PolledPage` / `apply_remote`: 現行ポーリング。
    ws 有効時はポーラーを起動しない（または间隔を60sに落として保険にする）。
  - `apply_remote` の適用ガード（同一ページ・inflight==0・dirty無し・composer無し）は
    **ws 差分適用でも同じ規則を使う**こと。適用できない間はイベントをバッファし、
    ガードが解けたら順に適用（parentId の連続性が切れたら全文リロードにフォールバック）。
  - **自己エコー除去**: 自分の commit も event で返ってくる。`userId` で切ると
    **同じアカウントでブラウザから編集した分まで落ちる**ので、`commitId` で切る。
    commit が成功したときサーバが返す id を覚えておき（`App::own_commits`）、
    同じ id の event は ops を適用せず head だけ進める。

    > 実装当初は「自分のエコーは冪等だから適用しても無害」としていたが、これは誤り。
    > エコーは**自分がその先を編集したあとに届くことがある**。例えば日本語を確定して
    > すぐ Enter で行を分割すると、分割後に1つ目（確定テキスト）のエコーが届き、
    > `Replace` が分割前の本文を書き戻す。画面上はテキストが重複し、キャレットが
    > 1行下にずれる。行が消えるエコーなら、セッションの行が消えて EDIT から抜ける。
  - **全文同期（resync）にも epoch ガード**: 取得を**始めた時点**の
    `server_epoch` を `ResyncPage` に載せ、届いたときに進んでいたら捨てて
    もう一度要求する。ポーリング（`PolledPage`）は最初からこのガードを持っていたが、
    ws の resync には無く、コミット直後に届いた古いページが「さっき作った行」を
    消してしまう（＝セッションの行が消えて EDIT から抜ける）経路が残っていた。
  - ページ遷移: `set_page` が `poll_target` を更新している。ws 版は room:leave→join
    （または接続張り直し）。`page_id` は App にある。
- `rust/src/api.rs`: `AuthStore::resolve` / `Credential::Sid`。sid の取り出しに
  `resolve_user` を使う。
- 参考実装: このファイルの元になった Python 偵察スクリプトは会話ログ参照。
  engine.io/socket.io フレーミングは上記で全部。

## 要件

1. `Credential::Sid` があるとき: websocket スレッドを起動し、commit イベントを
   `EditOp` に変換してチャネルで event loop へ。適用は `apply_remote` と同じガード。
2. 差分適用後は `rerender` + カーソル/セッションの lineId 再アンカー
   （`apply_remote` の既存ロジックを関数に切り出して共用）。
3. 再接続: 切断・ping timeout で指数バックオフ（1s→2s→…max 30s）再接続+re-join。
   再接続直後は 1 回ポーリング相当の全文取得で取りこぼしを埋める。
4. 未知イベント・パース失敗は無視（ログ用に status に一度だけ出す程度）。
   壊れても**最悪ポーリングに落ちるだけ**の設計にする。
5. テスト: フレームのエンコード/デコード、changes→EditOp パース、
   自己エコー除去、ガード付き適用。ネットワーク統合は smoke バイナリ
   （`src/bin/ws_smoke.rs` 等、edit_smoke に倣う）で
   `my-sandbox/テスト` に対して実施してよい（書き込みは必ず原状復帰）。
6. `cargo test` 全 green・警告ゼロ。KEYMAP.md の「リアルタイム反映」記述を更新。

## 受け入れ基準

- `COSENSE_SID` あり: ブラウザで編集 → 1秒以内に TUI に反映（telomere 未読表示）
- PAT のみ: 現行どおり 3 秒ポーリングで動作
- 自分の TUI 編集がエコーで二重適用されない
- 接続断後、自動復帰して差分を取りこぼさない（復帰時全文同期）
