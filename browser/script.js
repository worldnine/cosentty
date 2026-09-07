// Cosense UserScript: 選択行を AI への指図文にしてクリップボードへ
//
// 使い方: Alt+C でその場にメモ欄が開く(何件でも)→ Alt+L で一覧(移動・編集・削除)
// → Alt+S でまとめてコピー → Alt+X で破棄。
// リロードやページ移動をまたいで溜まる(localStorage)。
//
// 置き場所: 自分のページに `code:script.js` のコードブロックとして貼る。
// 有効化: 右上メニュー Edit Profile → UserScript を Enabled → ブラウザをリロード。
// 自分にだけ効く(メンバーやゲストには影響なし)。使うプロジェクトごとに
// 自分のページへ置く。複数プロジェクトで共通化したい場合は /api/code/ の
// import 技(https://scrapbox.io/bbr-scrapbox-memo/UserScript:%E5%A4%96%E9%83%A8JavaScript%E3%82%92import%E3%81%99%E3%82%8B)で1か所から引く。
//
// 出る形は TUI の comment と同じ約束(rust/src/comment.rs の format_comment):
//
//   https://scrapbox.io/<project>/<title>#<lineId> L12-14
//   > 原文1  # <lineId1>
//   > 原文2  # <lineId2>
//
//   直して
//
// - URL が行頭・空白なし・先頭行の #lineId 付き(skill がアンカーに読む)
// - 引用は原文のまま、行末に `# <lineId>`
// - 引用と指示のあいだは空行1つ
// 送り先はなし。貼るのは人間(TUI の `s`/herdr 直送は次の段階)。
// 複数は format_all と同じ形に番号付きで並ぶ(1件なら素のまま)。
(() => {
  const KEY_MEMO = "KeyC"; // Alt+C: この範囲を1件メモる
  const KEY_SEND = "KeyS"; // Alt+S: 溜めた全部をコピーして空にする
  const KEY_DROP = "KeyX"; // Alt+X: 溜めた全部を捨てる
  const KEY_LIST = "KeyL"; // Alt+L: 一覧(移動・編集・削除)
  const STORE_KEY = "cosense-ai-comments-v1";

  // 選択範囲は scrapbox.Page.selection(行番号付きの公式情報)から取る。
  // DOM に聞くのはエディタ外フォーカスなどで selection が無いときの予備だけ。
  // 行要素の id は `L` + lineId(TUI の headless Chrome 側が `L{line_id}` で
  // 掴んでいるのと同じもの)。
  const LINE_SEL = '.lines .line[id^="L"]';

  function toast(msg, isErr) {
    document.getElementById("cosense-ai-comment-toast")?.remove();
    const el = document.createElement("div");
    el.id = "cosense-ai-comment-toast";
    el.textContent = msg;
    el.style.cssText = [
      "position:fixed",
      "left:50%",
      "bottom:48px",
      "transform:translateX(-50%)",
      "z-index:999999",
      "padding:6px 14px",
      "border-radius:6px",
      "font-size:13px",
      "background:" + (isErr ? "#7f1d1d" : "#1f2937"),
      "color:#fff",
      "box-shadow:0 2px 12px rgba(0,0,0,.4)",
      "max-width:80vw",
    ].join(";");
    document.body.appendChild(el);
    setTimeout(() => el.remove(), isErr ? 5000 : 3000);
  }

  function closeComposer() {
    document.getElementById("cosense-ai-composer")?.remove();
  }

  function lineRectById(id) {
    const el = document.getElementById("L" + id);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 ? r : null;
  }

  // TUI の Row::Composer 相当: 範囲の最終行の下にその場で開く入力欄。
  // エディタの DOM には触らず fixed 配置。Ctrl+Enter=メモ、Enter=改行(IME とぶつけない)。
  function openComposer(draft, rect, opts = {}) {
    closeComposer();
    const count = loadDrafts().length;
    const box = document.createElement("div");
    box.id = "cosense-ai-composer";
    const W = Math.min(560, window.innerWidth - 32);
    let top = 120;
    let left = Math.max(16, (window.innerWidth - W) / 2);
    if (rect) {
      left = Math.max(16, Math.min(rect.left, window.innerWidth - W - 16));
      top = rect.bottom + 8;
      if (top + 220 > window.innerHeight) top = Math.max(8, rect.top - 228);
    }
    box.style.cssText = [
      "position:fixed",
      `top:${top}px`,
      `left:${left}px`,
      `width:${W}px`,
      "z-index:999999",
      "background:#1f2937",
      "color:#fff",
      "border-radius:8px",
      "box-shadow:0 4px 24px rgba(0,0,0,.5)",
      "padding:10px 12px",
      "font-size:13px",
    ].join(";");

    const head = document.createElement("div");
    head.style.cssText =
      "display:flex;align-items:center;gap:8px;margin-bottom:6px;";
    const badge = document.createElement("span");
    badge.textContent = `L${draft.label}`;
    badge.style.cssText =
      "background:#0ea5e9;color:#fff;border-radius:4px;padding:1px 8px;font-weight:bold;";
    const sub = document.createElement("span");
    sub.textContent = opts.title ?? `AIへの指示 (メモ${count + 1}件目)`;
    sub.style.cssText = "color:#d1d5db;";
    const x = document.createElement("button");
    x.textContent = "✕";
    x.title = "やめる (Esc)";
    x.style.cssText =
      "margin-left:auto;background:none;border:none;color:#9ca3af;cursor:pointer;font-size:13px;";
    x.onclick = () => closeComposer();
    head.append(badge, sub, x);

    const ta = document.createElement("textarea");
    ta.rows = 3;
    ta.placeholder = "指示を書く…";
    ta.value = opts.initial ?? "";
    ta.style.cssText = [
      "width:100%",
      "box-sizing:border-box",
      "background:#111827",
      "color:#fff",
      "border:1px solid #374151",
      "border-radius:6px",
      "padding:6px 8px",
      "font-size:13px",
      "line-height:1.5",
      "resize:vertical",
    ].join(";");
    const save = () => {
      const instr = normalizeText(ta.value);
      if (opts.onSave) {
        if (!opts.onSave(instr)) return;
        closeComposer();
        return;
      }
      const all = loadDrafts();
      all.push({ ...draft, id: crypto.randomUUID(), instr });
      if (!saveDrafts(all)) return;
      closeComposer();
      toast(
        `✓ L${draft.label} をメモ (計${all.length}件。Alt+Sでまとめてコピー)`,
      );
    };
    // Enter 保存はやめた(IME とぶつかるので)。保存は Ctrl+Enter(⌘+Enter 可)。
    // 変換中の Esc は変換の取り消しに譲り、欄を閉じない。
    let comp = false;
    ta.addEventListener("compositionstart", () => {
      comp = true;
    });
    ta.addEventListener("compositionend", () => {
      comp = false;
    });
    ta.addEventListener("keydown", (ev) => {
      ev.stopPropagation();
      const composing = comp || ev.isComposing || ev.keyCode === 229;
      if ((ev.ctrlKey || ev.metaKey) && ev.key === "Enter") {
        ev.preventDefault();
        if (!composing) save();
      } else if (ev.key === "Escape" && !composing) {
        ev.preventDefault();
        closeComposer();
      } else if (ev.ctrlKey && (ev.key === "j" || ev.key === "J")) {
        // TUI の ^j と同じ改行。textarea は素では何もしないので自前で割る。
        ev.preventDefault();
        const s = ta.selectionStart ?? ta.value.length;
        const e = ta.selectionEnd ?? s;
        ta.value = ta.value.slice(0, s) + "\n" + ta.value.slice(e);
        ta.selectionStart = ta.selectionEnd = s + 1;
      }
    });

    const foot = document.createElement("div");
    foot.style.cssText =
      "display:flex;align-items:center;gap:8px;margin-top:6px;color:#9ca3af;font-size:12px;";
    const hint = document.createElement("span");
    hint.textContent = "Ctrl+Enterでメモ · Enterで改行 · Escでやめる";
    const btn = document.createElement("button");
    btn.textContent = opts.saveLabel ?? "メモる";
    btn.style.cssText =
      "margin-left:auto;background:#0ea5e9;color:#fff;border:none;border-radius:4px;padding:3px 14px;cursor:pointer;";
    btn.onclick = () => save();
    foot.append(hint, btn);

    box.append(head, ta, foot);
    document.body.appendChild(box);
    ta.focus();
  }

  function lineElFromNode(node) {
    const el = node instanceof Element ? node : node?.parentElement;
    return el?.closest?.(".line") ?? null;
  }

  function allLineEls() {
    return [...document.querySelectorAll(LINE_SEL)];
  }

  // ドラッグ選択の両端が属する行で範囲を決める。collapsed なら null。
  function selectedIds() {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || sel.isCollapsed) return null;
    const a = lineElFromNode(sel.anchorNode);
    const f = lineElFromNode(sel.focusNode);
    if (!a || !f) return null;
    const els = allLineEls();
    const ia = els.indexOf(a);
    const ib = els.indexOf(f);
    if (ia < 0 || ib < 0) return null;
    return els
      .slice(Math.min(ia, ib), Math.max(ia, ib) + 1)
      .map((e) => e.id.slice(1));
  }

  function caretId() {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return null;
    const el = lineElFromNode(sel.anchorNode);
    return el && el.id.startsWith("L") ? [el.id.slice(1)] : null;
  }

  // comment.rs の normalize_text と同じ: \r を捨て、行末空白を刈り、空行を落とす。
  function normalizeText(t) {
    return t
      .replace(/\r/g, "")
      .split("\n")
      .map((l) => l.replace(/[ \t]+$/g, ""))
      .filter((l) => l.trim() !== "")
      .join("\n");
  }

  // 溜めた分。localStorage に置くのでリロード・ページ移動で消えない。
  // 1件 = {id, base, firstIdx, firstId, label, quote, instr}。引用は原文で
  // スナップショットするので、溜めた後にページが変わっても壊れない。
  function loadDrafts() {
    try {
      const v = JSON.parse(localStorage.getItem(STORE_KEY) ?? "[]");
      return Array.isArray(v) ? v : [];
    } catch {
      return [];
    }
  }
  function saveDrafts(ds) {
    try {
      localStorage.setItem(STORE_KEY, JSON.stringify(ds));
      return true;
    } catch {
      toast("メモを保存できません。ストレージの空き容量や設定を確認してください", true);
      return false;
    }
  }

  // comment.rs の format_all と同じ: ページ→行の順、2件以上は番号付き。
  function formatDrafts(ds) {
    const sorted = [...ds].sort((a, b) =>
      a.base < b.base ? -1 : a.base > b.base ? 1 : a.firstIdx - b.firstIdx,
    );
    const numbered = sorted.length > 1;
    return sorted
      .map((d, i) => {
        const head = `${d.base}#${d.firstId} L${d.label}`;
        if (!numbered)
          return d.instr
            ? `${head}\n${d.quote}\n\n${d.instr}`
            : `${head}\n${d.quote}`;
        let out = `${i + 1}. ${head}`;
        for (const q of d.quote.split("\n")) out += `\n   ${q}`;
        out += "\n";
        for (const l of d.instr.split("\n")) out += `\n    ${l}`;
        return out;
      })
      .join("\n\n");
  }

  async function copyText(body) {
    try {
      await navigator.clipboard.writeText(body);
    } catch {
      // clipboard API が使えない場面の逃げ道。
      const ta = document.createElement("textarea");
      ta.value = body;
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      if (!ok) throw new Error("copy failed");
    }
  }

  // Alt+C: いまの範囲を1件メモる。送らない、置くだけ。
  async function memo() {
    if (typeof scrapbox === "undefined" || !scrapbox.Page?.lines) {
      toast("ページを開いてから (一覧では使えません)", true);
      return;
    }
    const lines = scrapbox.Page.lines;
    const byId = new Map(lines.map((l, i) => [l.id, i]));

    // 0-based の行 index 列を作る。公式の selection が第一、DOM は予備。
    let idx = [];
    const sel = scrapbox.Page.selection;
    if (sel) {
      const a = Math.min(sel.start.line, sel.end.line);
      const b = Math.max(sel.start.line, sel.end.line);
      for (let i = a; i <= b && i < lines.length; i++) if (i >= 0) idx.push(i);
    }
    if (idx.length === 0) {
      const ids = selectedIds() || caretId();
      if (ids)
        idx = ids.map((id) => byId.get(id)).filter((i) => i !== undefined);
    }
    if (idx.length === 0) {
      toast("行を選択してから (ドラッグ選択かキャレット行)", true);
      return;
    }
    const label =
      idx.length === 1
        ? `${idx[0] + 1}`
        : `${idx[0] + 1}-${idx[idx.length - 1] + 1}`;

    const quote = idx
      .map((i) => {
        const l = lines[i];
        // 空行も `# id` を残す。comment.rs の format と同一(`">   # id"`)。
        // anchor が途切れると insertBefore 等でその行を指せなくなる。
        return `> ${l.text}  # ${l.id}`;
      })
      .join("\n");
    const quoteBlock = quote.trim() === "" ? ">" : quote;

    const base = location.origin + location.pathname;
    openComposer(
      {
        base,
        firstIdx: idx[0],
        firstId: lines[idx[0]].id,
        label,
        quote: quoteBlock,
      },
      lineRectById(lines[idx[idx.length - 1]].id),
    );
  }

  // Alt+S: 溜めた全部をコピーして空にする。TUI の `s` と同じ約束。
  async function send() {
    const ds = loadDrafts();
    if (ds.length === 0) {
      toast("メモはありません (Alt+Cでためてから)", true);
      return;
    }
    try {
      await copyText(formatDrafts(ds));
      if (!saveDrafts([])) {
        toast("コピーは完了しましたが、保存済みメモを消去できませんでした", true);
        return;
      }
      toast(`✓ ${ds.length}件をコピーしました`);
    } catch (e) {
      toast(`コピーできません: ${e.message}`, true);
    }
  }

  // Alt+X: 溜めた全部を捨てる。送らない。
  function drop() {
    const n = loadDrafts().length;
    if (!saveDrafts([])) return;
    toast(n === 0 ? "メモはありません" : `${n}件を捨てました`);
  }

  // ---- 一覧(TUI の `l` 相当): 移動・編集・削除。localStorage なので他ページへも飛べる。
  function closeList() {
    document.getElementById("cosense-ai-list")?.remove();
  }

  function sortedDrafts() {
    return loadDrafts()
      .map((d, ref) => ({ d, ref }))
      .sort((a, b) =>
        a.d.base < b.d.base ? -1 : a.d.base > b.d.base ? 1 : a.d.firstIdx - b.d.firstIdx,
      );
  }

  function shortPage(base) {
    try {
      return decodeURIComponent(new URL(base).pathname).replace(/^\//, "");
    } catch {
      return base;
    }
  }

  function flashLine(id) {
    const el = document.getElementById("L" + id);
    if (!el) return false;
    el.scrollIntoView({ block: "center" });
    el.style.outline = "2px solid #0ea5e9";
    setTimeout(() => {
      if (el.style.outline === "2px solid #0ea5e9") el.style.outline = "";
    }, 1500);
    return true;
  }

  function jumpDraft(ref) {
    const d = loadDrafts()[ref];
    if (!d) return;
    if (d.base !== location.origin + location.pathname) {
      location.href = d.base + "#" + d.firstId; // 溜めは残るので越境できる
      return;
    }
    closeList();
    if (!flashLine(d.firstId)) toast("行が見つかりません", true);
  }

  function editDraft(ref) {
    const drafts = loadDrafts();
    const d = drafts[ref];
    if (!d) return;
    // 旧形式のメモにも、編集を始める前に永続的なIDを付ける。
    if (!d.id) {
      d.id = crypto.randomUUID();
      if (!saveDrafts(drafts)) return;
    }
    closeList();
    const same = d.base === location.origin + location.pathname;
    openComposer({ ...d }, same ? lineRectById(d.firstId) : null, {
      initial: d.instr,
      title: "AIへの指示 (編集中)",
      saveLabel: "更新",
      onSave: (instr) => {
        const all = loadDrafts();
        const i = all.findIndex((x) => x.id === d.id);
        if (i >= 0) all[i] = { ...all[i], instr };
        else all.push({ ...d, instr });
        if (!saveDrafts(all)) return false;
        toast("✓ 更新しました");
        return true;
      },
    });
  }

  function deleteDraft(ref, box) {
    const all = loadDrafts();
    all.splice(ref, 1);
    if (!saveDrafts(all)) return;
    if (all.length === 0) {
      closeList();
      toast("メモは空になりました");
      return;
    }
    renderList(box);
    toast(`1件消しました (残り${all.length}件)`);
  }

  function renderList(box) {
    box.innerHTML = "";
    const items = sortedDrafts();
    const head = document.createElement("div");
    head.style.cssText =
      "display:flex;align-items:center;gap:8px;margin-bottom:8px;color:#fff;font-weight:bold;";
    const title = document.createElement("span");
    title.textContent = `メモ ${items.length}件`;
    const x = document.createElement("button");
    x.textContent = "✕";
    x.title = "閉じる (Esc)";
    x.style.cssText =
      "margin-left:auto;background:none;border:none;color:#9ca3af;cursor:pointer;font-size:13px;";
    x.onclick = () => closeList();
    head.append(title, x);
    box.append(head);

    items.forEach(({ d, ref }, i) => {
      const row = document.createElement("div");
      row.style.cssText = "border-top:1px solid #374151;padding:6px 0;";
      const loc = document.createElement("div");
      loc.textContent = `${i + 1}. ${shortPage(d.base)} L${d.label}`;
      loc.style.cssText = "color:#7dd3fc;font-size:12px;word-break:break-all;";
      const ins = document.createElement("div");
      ins.textContent = d.instr.split("\n")[0] || "(指示なし)";
      ins.style.cssText = "color:#e5e7eb;margin:2px 0 4px;";
      const btns = document.createElement("div");
      btns.style.cssText = "display:flex;gap:6px;";
      const mk = (label, fn) => {
        const b = document.createElement("button");
        b.textContent = label;
        b.style.cssText =
          "background:#374151;color:#fff;border:none;border-radius:4px;padding:2px 10px;cursor:pointer;font-size:12px;";
        b.onclick = fn;
        return b;
      };
      btns.append(
        mk("移動", () => jumpDraft(ref)),
        mk("編集", () => editDraft(ref)),
        mk("消す", () => deleteDraft(ref, box)),
      );
      row.append(loc, ins, btns);
      box.append(row);
    });

    const foot = document.createElement("div");
    foot.textContent = "Alt+Sでまとめてコピー · Escで閉じる";
    foot.style.cssText =
      "border-top:1px solid #374151;margin-top:6px;padding-top:6px;color:#9ca3af;font-size:12px;";
    box.append(foot);
  }

  function openList() {
    if (document.getElementById("cosense-ai-list")) {
      closeList();
      return;
    }
    closeComposer();
    if (loadDrafts().length === 0) {
      toast("メモはありません (Alt+Cでためてから)", true);
      return;
    }
    const box = document.createElement("div");
    box.id = "cosense-ai-list";
    const W = Math.min(480, window.innerWidth - 32);
    box.style.cssText = [
      "position:fixed",
      "top:64px",
      "right:16px",
      `width:${W}px`,
      "max-height:70vh",
      "overflow-y:auto",
      "z-index:999999",
      "background:#1f2937",
      "color:#fff",
      "border-radius:8px",
      "box-shadow:0 4px 24px rgba(0,0,0,.5)",
      "padding:10px 12px",
      "font-size:13px",
    ].join(";");
    renderList(box);
    document.body.appendChild(box);
  }

  document.addEventListener("keydown", (e) => {
    // macOS は Option+C で e.key が 'ç' になるので e.code で見る。
    if (!e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.code === KEY_MEMO) {
      e.preventDefault();
      const open = document.getElementById("cosense-ai-composer");
      if (open) open.querySelector("textarea")?.focus();
      else memo();
    } else if (e.code === KEY_SEND) {
      e.preventDefault();
      send();
    } else if (e.code === KEY_DROP) {
      e.preventDefault();
      drop();
    } else if (e.code === KEY_LIST) {
      e.preventDefault();
      openList();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !e.altKey && !e.ctrlKey && !e.metaKey) {
      if (document.getElementById("cosense-ai-list")) closeList();
    }
  });
})();
