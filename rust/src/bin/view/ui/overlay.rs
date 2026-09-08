//! Help, comment/link pickers and modal menu panels.

use super::*;

/// The help panel's rows: four sections, one per place the keys work in.
/// The panel does not scroll, so this stays at 26 rows (a 30-row terminal
/// shows them all) and every row fits the 90-cell panel. The label column
/// is 12 display cells wide. Japanese labels are two cells per character,
/// so each language pads its own labels here rather than through a
/// `{:<12}` that counts bytes.
pub(crate) fn help_keys(app: &App) -> Vec<String> {
    let mut keys: Vec<String> = vec![
        t!("── READ ──", "── READ ──"),
        t!(
            "移動        j/k · g/G · ^u/^d · PgUp/PgDn · [ 戻る · ] 進む",
            "move        j/k · g/G · ^u/^d · PgUp/PgDn · [ back · ] forward"
        ),
        t!(
            "選択        Shift+↑↓ · J/K で広げる",
            "select      Shift+↑↓ · J/K extend"
        ),
        t!(
            "リンク      Enter/f で開く: ページ · 📎 ファイル → 保存先 · ↗ URL → ブラウザ",
            "link        Enter/f open: page · 📎 file → download dir · ↗ URL → browser"
        ),
        t!(
            "            Tab/S-Tab 次/前のリンク行へ",
            "            Tab/S-Tab next/previous link line"
        ),
        t!(
            "マウス      クリックでリンク/行移動 · ドラッグで選択 · ホイールでスクロール",
            "mouse       click link/open · click row/move · drag/select · wheel/scroll"
        ),
        t!(
            "表示        z ソース · w ブラウザ · t 行の詳細 · , 設定",
            "view        z source · w browser · t line detail · , settings"
        ),
        t!(
            "履歴        ← 古い版 · → 新しい版 · Esc 最新へ（履歴中は読むだけ）",
            "history     ← older · → newer · Esc NOW (read-only back there)"
        ),
        t!(
            "図          code:mmd は罫線・表で描く（テキスト優先）· R で未生成分を描く",
            "diagram     code:mmd draws as text first · R renders the missing ones"
        ),
        t!(
            "出力        y カーソル行/選択をコピー · Y ページ全体",
            "output      y copy line/selection · Y whole page"
        ),
        t!(
            "取り消し    u · ^z 取り消し · ^r やり直し（どのコミットも戻せます）",
            "undo        u · ^z undo · ^r redo (every commit is reversible)"
        ),
        t!(
            "終了        q 二度押し · ^c 即終了",
            "quit        q twice · ^c at once"
        ),
        t!("── EDIT ──", "── EDIT ──"),
    ];
    if app.editable {
        keys.extend([
            t!("編集        e 行末 · i 行頭 · o/O 行を追加 · ダブルクリック",
               "edit        e line end · i line start · o/O new line · double-click"),
            t!("            編集中: そのまま入力 · ↑↓ 行移動 · Enter 改行 · ⌫@行頭 前と結合 · Esc 終了",
               "            in session: type freely · ↑↓ lines · Enter new line · ⌫@BOL join · Esc done"),
            t!("            ^k 行を削る · ^y コピー · ^e $EDITOR で全体 · ^v 画像を貼る",
               "            ^k cut line · ^y copy · ^e whole page in $EDITOR · ^v paste image"),
            t!("構造編集    m 掴む: j/k/↑↓ 1行 · J/K 兄弟 · h/l 字下げ · Esc/Enter/m 確定（他キーも）",
               "outline     m grab: j/k/↑↓ line · J/K sibling · h/l indent · Esc/Enter/m done (any key does)"),
            t!("            Ctrl+←→↑↓ 行/範囲 · Alt+←→↑↓ ブロック · ^g h/j/k/l と H/J/K/L（選択中×）",
               "            Ctrl+arrows line/range · Alt+arrows block · ^g h/j/k/l and H/J/K/L (no selection)"),
        ]);
    } else {
        keys.push(t!(
            "編集        できません — このアカウントはプロジェクトのメンバーではありません",
            "edit        unavailable — this account is not a project member"
        ));
    }
    keys.extend([
        t!("── 一覧 ──", "── index ──"),
        t!("一覧        ^o 開く · / 絞り込み（Tab で本文検索） · s 並び順 · ^o 再びでプロジェクト",
           "index       ^o open · / filter (Tab: full-text) · s order · ^o again: projects"),
        t!("            j/k · g/G · PgDn · Tab 一覧/抜粋 · Enter 開く · ^u 戻す · [ 戻る · q 終了",
           "            j/k · g/G · PgDn · Tab list/excerpt · Enter open · ^u back · [ back · q quit"),
        t!("── オーバーレイ ──", "── overlays ──"),
        t!("コメント    c 書く（再cは編集） · s 送る · l 一覧（Enter 移動 · d 削除 · y コピー）",
           "comments    c write (again: edit) · s send · l list (Enter jump · d delete · y copy)"),
        t!("            入力中は ^j 改行 · Enter 保存 · Esc 取消 · 複数リンクも一覧から選ぶ",
           "            typing: ^j newline · Enter save · Esc cancel · several links: pick a list"),
        t!("ヘルプ      ? は一覧・オーバーレイでも開く · q/Esc 閉じる",
           "help        ? works in the index and on overlays too · q/Esc closes"),
        t!("            編集中・入力中の ? は文字になる（Esc で抜けて ?）",
           "            ? types a ? while editing/typing (Esc out, then ?)"),
    ]);
    keys
}

/// Draw the open overlay as a centered panel.
pub(crate) fn draw_overlay(f: &mut Frame, app: &App, area: Rect) {
    let (title, items, cursor): (String, Vec<String>, usize) = match app.overlay.as_ref() {
        Some(Overlay::Links { items, cursor }) => (
            t!("この行のリンク", "links on this line"),
            items.iter().map(LinkItem::label).collect(),
            *cursor,
        ),
        Some(Overlay::Comments { cursor }) => (
            format!("comments ({})", app.comments.len()),
            app.comments
                .iter()
                .map(|c| {
                    let first = c.text.lines().next().unwrap_or("");
                    match c.revision_label() {
                        Some(at) => format!("{} :{} @{at}  {}", c.title, c.range_label(), first),
                        None => format!("{} :{}  {}", c.title, c.range_label(), first),
                    }
                })
                .collect(),
            *cursor,
        ),
        Some(Overlay::LineInfo) => {
            let src = app.cursor_src();
            let items = match src.and_then(|s| app.lines.get(s).map(|l| (s, l))) {
                Some((s, l)) => {
                    let name_of = |id: &str| -> String { app.member_name(id) };
                    // `line.user_id` is the LAST UPDATER (verified against the
                    // commit log). The original author would need the commit
                    // history (~1s per page), which is deliberately not worth
                    // it here — so only the updater is named.
                    // The line's text is already on screen under the cursor,
                    // and its id is only needed by `e` (browser deep-link),
                    // so neither is repeated here.
                    vec![
                        t!("行        {}", "line      {}", s + 1),
                        t!(
                            "更新      {}  （{}前）  {}",
                            "updated   {}  ({} ago)   {}",
                            cosense::theme::format_local(l.updated),
                            relative_age(l.updated),
                            name_of(&l.user_id)
                        ),
                        t!(
                            "作成      {}  （{}前）",
                            "created   {}  ({} ago)",
                            cosense::theme::format_local(l.created),
                            relative_age(l.created)
                        ),
                    ]
                }
                None => vec![t!(
                    "カーソルの下に行がありません",
                    "no line under the cursor"
                )],
            };
            (t!("行の詳細", "line detail"), items, usize::MAX)
        }
        Some(Overlay::Help) => (t!("キー割り当て", "keys"), help_keys(app), usize::MAX),
        Some(Overlay::Settings(view)) => {
            draw_settings(f, view, area);
            return;
        }
        None => return,
    };

    let footer = if matches!(app.overlay, Some(Overlay::Comments { .. })) {
        ts!(
            " ↑/↓ 移動 · Enter 開く · d 削除 · y 全件コピー · s 送る · Esc 閉じる ",
            " ↑/↓ move · Enter open · d delete · y copy all · s send · Esc close "
        )
    } else {
        ts!(
            " ↑/↓ 移動 · Enter 開く · Esc 閉じる ",
            " ↑/↓ move · Enter open · Esc close "
        )
    };
    draw_menu_panel(f, area, &title, &items, cursor, footer);
}

/// The settings screen: the notes, then the sections and their rows; a
/// picker or a text field replaces the rows while one value is being
/// changed. The panel is the shared menu panel, so the rows are fitted to
/// its width here.
fn draw_settings(f: &mut Frame, view: &SettingsView, area: Rect) {
    let title = t!("設定", "settings");
    // The same width `draw_menu_panel` will choose, less the marker.
    let inner = area.width.saturating_sub(8).min(90).max(20) as usize - 2;
    let (items, cursor, footer) = match &view.mode {
        SettingsMode::List => {
            let (items, cursor) = settings_items(view, inner);
            (
                items,
                cursor,
                ts!(
                    " j/k 移動 · Enter 変更 · d 既定に戻す · e $EDITOR で開く · Esc 閉じる ",
                    " j/k move · Enter change · d back to default · e open in $EDITOR · Esc close "
                ),
            )
        }
        SettingsMode::Pick {
            items,
            cursor,
            field,
            ..
        } => {
            let mut all = settings_notes(view);
            let offset = all.len();
            all.extend(items.iter().cloned());
            let footer = if matches!(field, SettingField::Theme | SettingField::ProjectTheme) {
                ts!(
                    " j/k で流し見 · Enter 決定 · Esc 元に戻す ",
                    " j/k to try · Enter choose · Esc back to before "
                )
            } else {
                ts!(
                    " j/k 移動 · Enter 決定 · Esc 戻る ",
                    " j/k move · Enter choose · Esc back "
                )
            };
            (all, cursor + offset, footer)
        }
        SettingsMode::Input { target, input } => {
            let (before, after) = input.parts();
            let label = match target {
                Target::Project(cosense::config::ProjectKey::DisplayName) => {
                    t!("表示名", "display name")
                }
                Target::Project(cosense::config::ProjectKey::GyazoTeam) => t!(
                    "Gyazo Teams の組織名（空なら gyazo.com）",
                    "Gyazo Teams org (empty: gyazo.com)"
                ),
                Target::View(cosense::config::ViewKey::DownloadDir) => {
                    t!("保存先（~ 可）", "downloads (~ allowed)")
                }
                Target::Project(k) => k.as_str().to_string(),
                Target::View(k) => k.as_str().to_string(),
            };
            let mut all = settings_notes(view);
            all.push(format!("{label}: {before}▏{after}"));
            (
                all,
                usize::MAX,
                ts!(
                    " Enter 保存 · Esc 取り消し（空にすると既定に戻る） ",
                    " Enter save · Esc cancel (empty text: back to default) "
                ),
            )
        }
    };
    draw_menu_panel(f, area, &title, &items, cursor, footer);
}

/// A centered menu panel: title bar, the items with a `▸` on the cursor,
/// and one dim line of keys. `cursor == usize::MAX` marks nothing, which is
/// how the read-only panels (help, line detail) use it.
pub(crate) fn draw_menu_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    items: &[String],
    cursor: usize,
    footer: &str,
) {
    let w = area.width.saturating_sub(8).min(90).max(20);
    let want_h = items.len() as u16 + 2;
    let h = want_h.min(area.height.saturating_sub(4)).max(3);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let panel = Rect::new(x, y, w, h);
    f.render_widget(Clear, panel);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::Black)
            .bg(CHROME_ACCENT)
            .add_modifier(Modifier::BOLD),
    )));
    let visible = (h.saturating_sub(2)) as usize;
    let start = if cursor != usize::MAX && cursor >= visible {
        cursor + 1 - visible
    } else {
        0
    };
    for (i, it) in items.iter().enumerate().skip(start).take(visible) {
        let sel = i == cursor;
        let style = if sel {
            Style::default()
                .fg(Color::Black)
                .bg(CHROME_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if sel { "▸ " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{it}"), style)));
    }
    lines.push(Line::from(Span::styled(
        footer.to_string(),
        Style::default().fg(CHROME_DIM),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Black)),
        panel,
    );
}
