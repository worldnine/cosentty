# NOTE: Scrapbox 本家パーサの規則(実測と出典)

調査日: 2026-09-06。きっかけは <sandbox>/文章入力遅延テスト での
見た目の不一致(TUI だけコードブロックが空行を越えて続いていた)。

## 出典

- **progfay/scrapbox-parser**(Cosense/Scrapbox の公式パーサ。web はこれで
  描画している)
  - https://github.com/progfay/scrapbox-parser
  - `src/block/Pack.ts` の `packRows` / `isChildRowOfPack` が規則の本体
- 実装(TUI 側): `render.rs` の3走査(`code_span_at` / `code_line_flags` /
  `render_lines_with` のブロック収集)と `session.rs` の Enter

## 規則(2026-09-06 のソース実測)

```ts
const isChildRowOfPack = (pack: Pack, row: Row): boolean =>
	(pack.type === "codeBlock" || pack.type === "table") &&
	row.indent > (pack.rows[0]?.indent ?? 0);
```

- ブロックは4種: title / codeBlock / table / line。行は先頭から順に
  「直前のブロックの子か」だけを見て分配される(`packRows`)
- **子 = 「ブロック先頭行より深いインデントの行」のみ**。`code:` ヘッダと
  同じインデント、より浅いインデント、そして**空行(インデント0)は子では
  ない = そこでブロックが終わる**
- **空白のみの行(インデントを保持)は子になる** = 「空のコード」として
  ブロックの内側に積める。インデントがメンバーシップであり、それを削るのが
  脱出のジェスチャ(web の Delete/⌫ の挙動と一致する)
- `table:` も同一規則(空行で終わる。TUI の table_span_at は当初から一致)
- `code:`/`table:` の前にインデントされた行が並ぶだけなら、それは
  **箇条書き**(ブロックではなく)

## 実測との突合

- <sandbox>/文章入力遅延テスト: `code:テスト.txt` + ` これは` +
  空行 + インデント行3本 → web はブロックを `これは` で切り、残り3行を
  箇条書きで描画(Chrome の実スクショと API の生テキストで確認)
- 「Enter でずっと続いてしまう」(同ページのタイトル): web のエディタは
  コードブロック内の Enter で**インデントを継承**するため。空行は
  インデントを削らない限り出られない

## TUI 側の対応(実装済み、2026-09-06)

- 3走査を「深いインデントのみ継続、真の空行で終端」に統一
  (旧実装は「空行の後にコードが続くなら継続」だった — 不一致の原因)
- セッション: コード本文行の頭(キャレットがインデントの内側)での Enter は
  **インデントを継承した空コード行を上に挿入**。真の空行を挿入すると
  新規則の下で即ブロックが切れるため
- テスト: `a_truly_blank_line_ends_the_code_block_like_web`、
  `code_span_agrees_with_what_the_renderer_collected`、
  `blank_lines_after_a_code_block_are_all_kept`(後続の期待値を web 準拠に)

## 関連する web 実測

- テロメア・テーマ色の実測: `SPEC-telomere-web-parity.md` /
  `scripts/cosense-theme-vars.py`(同梱 `app.css` からの生成)
- 認証の非対称: pages API は PAT で通るが **settings(`/api/projects`)と
  code(`/api/code/…`)は PAT で 401、sid が必要**(UserCSS の取得も code API
  経由なので sid 依存。PLAN-next 将来メモ参照)
