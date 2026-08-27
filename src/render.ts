// Scrapbox notation -> ANSI renderer (line-oriented subset).
// Scrapbox is NOT markdown: structure is per-line, indent = nesting depth.
import chalk from "chalk";

export interface RenderedLine {
  text: string; // ANSI-decorated text for display
  links: string[]; // internal page links found on this line (for navigation)
  gyazo: string[]; // gyazo image URLs found on this line (v1: render inline)
}

const GYAZO_RE = /https?:\/\/(?:i\.)?gyazo\.com\/([0-9a-f]{32})/gi;
const URL_RE = /https?:\/\/[^\s\]]+/g;

// --- East-Asian width awareness (for table column alignment) ---
function charWidth(cp: number): number {
  if (cp === 0) return 0;
  // combining marks
  if ((cp >= 0x300 && cp <= 0x36f) || (cp >= 0x1ab0 && cp <= 0x1aff)) return 0;
  // wide ranges: CJK, Hangul, Kana, fullwidth forms, etc.
  if (
    (cp >= 0x1100 && cp <= 0x115f) || // Hangul Jamo
    (cp >= 0x2e80 && cp <= 0x303e) || // CJK radicals / Kangxi
    (cp >= 0x3041 && cp <= 0x33ff) || // Hiragana..CJK symbols
    (cp >= 0x3400 && cp <= 0x4dbf) || // CJK Ext A
    (cp >= 0x4e00 && cp <= 0x9fff) || // CJK Unified
    (cp >= 0xa000 && cp <= 0xa4cf) || // Yi
    (cp >= 0xac00 && cp <= 0xd7a3) || // Hangul syllables
    (cp >= 0xf900 && cp <= 0xfaff) || // CJK compat
    (cp >= 0xfe30 && cp <= 0xfe4f) || // CJK compat forms
    (cp >= 0xff00 && cp <= 0xff60) || // Fullwidth forms
    (cp >= 0xffe0 && cp <= 0xffe6) ||
    (cp >= 0x1f300 && cp <= 0x1faff) || // emoji/symbols
    (cp >= 0x20000 && cp <= 0x3fffd) // CJK Ext B+
  )
    return 2;
  return 1;
}

export function displayWidth(s: string): number {
  let w = 0;
  for (const ch of s) w += charWidth(ch.codePointAt(0)!);
  return w;
}

function padTo(s: string, width: number): string {
  const pad = width - displayWidth(s);
  return s + " ".repeat(Math.max(0, pad));
}

// Decorate the inline content of a single line (bold, links, urls, code).
function decorateInline(s: string, links: string[], gyazo: string[]): string {
  let m: RegExpExecArray | null;
  GYAZO_RE.lastIndex = 0;
  while ((m = GYAZO_RE.exec(s)) !== null) gyazo.push(`https://i.gyazo.com/${m[1]}.png`);

  // Inline code `...`
  s = s.replace(/`([^`]+)`/g, (_all, code) => chalk.bgGray.white(` ${code} `));

  // Bracket forms: [* bold], [url title], [url], [PageName], [name.icon]
  s = s.replace(/\[([^\]]+)\]/g, (_all, inner: string) => {
    // decoration: [* text] [** text] [*/ text] [- strike] [_ underline]
    const deco = inner.match(/^([*/_\-]+)\s+([\s\S]*)$/);
    if (deco) {
      const flags = deco[1];
      let body = deco[2];
      let styled = body;
      if (flags.includes("*")) styled = chalk.bold(styled);
      if (flags.includes("/")) styled = chalk.italic(styled);
      if (flags.includes("-")) styled = chalk.strikethrough(styled);
      if (flags.includes("_")) styled = chalk.underline(styled);
      return styled;
    }
    // icon: [name.icon]
    if (/\.icon\*?\d*$/.test(inner)) {
      const name = inner.replace(/\.icon\*?\d*$/, "");
      return chalk.yellow(`@${name}`);
    }
    // external link with optional title
    const urlInner = inner.match(URL_RE);
    if (urlInner) {
      const url = urlInner[0];
      const title = inner.replace(url, "").trim();
      if (/gyazo\.com/.test(url)) return chalk.magenta(`🖼  ${title || "[gyazo]"}`);
      return chalk.cyan.underline(title || url);
    }
    // internal page link
    links.push(inner);
    return chalk.blue.underline(inner);
  });

  // Bare URLs
  s = s.replace(URL_RE, (u) => chalk.cyan.underline(u));

  // #hashtag
  s = s.replace(/(^|\s)#(\S+)/g, (_all, pre, tag) => {
    links.push(tag);
    return `${pre}${chalk.green("#" + tag)}`;
  });

  return s;
}

// Leading whitespace is indentation (tab, half-width space, full-width space U+3000).
// Two independent measures are derived from it:
//   - rawLen: character count, for block containment (table/code) comparisons.
//   - level:  display nesting depth. A tab or a full-width space is one level each;
//             a *run* of consecutive half-width spaces is one level (so "    " typed
//             as a single indent step is +1, not +4). This keeps mixed tab/full/half
//             indentation from over-nesting.
function indentInfo(raw: string): { level: number; rawLen: number; rest: string } {
  const m = raw.match(/^[\t \u3000]*/);
  const ws = m ? m[0] : "";
  let level = 0;
  for (let i = 0; i < ws.length; i++) {
    const c = ws[i];
    if (c === " ") {
      level++;
      while (i + 1 < ws.length && ws[i + 1] === " ") i++;
    } else {
      level++; // tab or full-width space
    }
  }
  return { level, rawLen: ws.length, rest: raw.slice(ws.length) };
}

// Render a Scrapbox table block into box-drawn lines.
function renderTable(name: string, rows: string[][], indentPrefix: string): RenderedLine[] {
  const cols = Math.max(0, ...rows.map((r) => r.length));
  const widths: number[] = [];
  for (let c = 0; c < cols; c++) {
    let w = 0;
    for (const r of rows) w = Math.max(w, displayWidth(r[c] ?? ""));
    widths[c] = w;
  }
  const hline = (l: string, mid: string, r: string) =>
    indentPrefix + chalk.gray(l + widths.map((w) => "─".repeat(w + 2)).join(mid) + r);

  const out: RenderedLine[] = [];
  if (name) out.push({ text: indentPrefix + chalk.yellow.bold(`▤ ${name}`), links: [], gyazo: [] });
  if (cols === 0) return out;

  out.push({ text: hline("┌", "┬", "┐"), links: [], gyazo: [] });
  rows.forEach((r, ri) => {
    const cells = widths.map((w, c) => " " + padTo(r[c] ?? "", w) + " ");
    const styled = cells.map((cell, c) => (ri === 0 ? chalk.bold(cell) : cell));
    out.push({
      text: indentPrefix + chalk.gray("│") + styled.join(chalk.gray("│")) + chalk.gray("│"),
      links: [],
      gyazo: [],
    });
    if (ri === 0) out.push({ text: hline("├", "┼", "┤"), links: [], gyazo: [] });
  });
  out.push({ text: hline("└", "┴", "┘"), links: [], gyazo: [] });
  return out;
}

export function renderLines(lines: { text: string }[]): RenderedLine[] {
  const out: RenderedLine[] = [];
  let inCodeBlock = false;
  let codeIndent = 0;

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i].text;
    const links: string[] = [];
    const gyazo: string[] = [];
    const { level, rawLen, rest: body } = indentInfo(raw);
    // level 1 sits flush-left; each deeper level adds 2 columns.
    const indent = "  ".repeat(Math.max(0, level - 1));

    // ---- blank line: preserve it (unless inside a code block, handled below) ----
    if (raw.trim() === "" && !inCodeBlock) {
      out.push({ text: "", links, gyazo });
      continue;
    }

    // ---- table block ----
    const tableMatch = body.match(/^table:(.*)$/);
    if (tableMatch) {
      const name = tableMatch[1].trim();
      const rows: string[][] = [];
      let j = i + 1;
      while (j < lines.length) {
        const info = indentInfo(lines[j].text);
        if (info.rawLen <= rawLen || lines[j].text.trim() === "") break;
        rows.push(info.rest.split("\t").map((c) => c.replace(/\s+$/, "")));
        j++;
      }
      // decorate cell contents (links inside cells still navigable)
      const decoRows = rows.map((r) =>
        r.map((c) => decorateInline(c, links, gyazo))
      );
      for (const rl of renderTable(name, decoRows, indent)) out.push(rl);
      // attach any links/gyazo found in cells to the table header line
      if (out.length && (links.length || gyazo.length)) {
        out[out.length - 1] = { ...out[out.length - 1], links, gyazo };
      }
      i = j - 1;
      continue;
    }

    // ---- code block ----
    if (/^code:/.test(body)) {
      inCodeBlock = true;
      codeIndent = rawLen;
      out.push({ text: indent + chalk.yellow(body), links, gyazo });
      continue;
    }
    if (inCodeBlock) {
      if (rawLen > codeIndent || body === "") {
        out.push({ text: indent + "  " + chalk.gray(body), links, gyazo });
        continue;
      }
      inCodeBlock = false;
    }

    // ---- command line ($ / %) ----
    const cmd = body.match(/^([$%])\s+(.*)$/);
    if (cmd) {
      out.push({
        text: indent + chalk.green(cmd[1] + " ") + chalk.whiteBright(cmd[2]),
        links,
        gyazo,
      });
      continue;
    }

    // ---- title (first line) ----
    if (i === 0) {
      out.push({ text: chalk.bold.underline.whiteBright(body), links, gyazo });
      continue;
    }

    // ---- quote ----
    if (body.startsWith(">")) {
      const q = decorateInline(body.slice(1).trim(), links, gyazo);
      out.push({ text: indent + chalk.gray("┃ ") + chalk.italic(q), links, gyazo });
      continue;
    }

    // ---- section heading: a whole line wrapped in [* ...] ----
    const headingOnly = body.match(/^\[(\*+)\s+([\s\S]+)\]$/);
    if (headingOnly && level === 0) {
      const size = headingOnly[1].length;
      const txt = headingOnly[2];
      const styled = size >= 2 ? chalk.bold.underline.cyanBright(txt) : chalk.bold.cyanBright(txt);
      out.push({ text: styled, links, gyazo });
      continue;
    }

    // ---- bullet / plain ----
    const decorated = decorateInline(body, links, gyazo);
    if (level > 0) {
      out.push({ text: indent + chalk.dim("• ") + decorated, links, gyazo });
    } else {
      out.push({ text: decorated, links, gyazo });
    }
  }
  return out;
}
