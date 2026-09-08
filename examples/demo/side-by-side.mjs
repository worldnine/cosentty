// side-by-side.mjs — 端末(WezTerm を画面収録)とブラウザ(Playwright)を同時に撮って左右に並べる
//
//   COSENTTY_PROJECT=cosentty node examples/demo/side-by-side.mjs [tape] [url]
//
// 既定は edit.tape と、その tape が作るページ https://scrapbox.io/<project>/cosentty demo。
// ブラウザは拡張もブックマークも無い素の Chromium(ヘッドレス)を 1280x800 で開き、
// COSENSE_SID があれば cookie に入れて非公開プロジェクトも表示する。
// まだ無いページを開いておいても web は作成を拾わない(実測)ので、ブラウザは
// プロジェクトのトップで待ち、API でページができたのを見てからそのページへ移る。
// 以降の行追加は web が ws で受けてその場に現れる。
// 端末は record-terminal.mjs が WezTerm で tape を再生して実時間で録画する。ブラウザ動画も
// 実時間なので、録画した区間(Show〜Hide)をそのまま切り出して並べれば時間が合う。
// 出力は examples/demo/out/side.mp4 と side.gif。
import { chromium } from 'playwright';
import { execFileSync } from 'node:child_process';
import { mkdirSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { recordTape } from './record-terminal.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const project = process.env.COSENTTY_PROJECT;
if (!project) { console.error('COSENTTY_PROJECT を指定する'); process.exit(2); }
const tape = process.argv[2] ?? join(here, 'edit.tape');
const url = process.argv[3] ?? `https://scrapbox.io/${project}/cosentty%20demo`;
const apiUrl = url.replace('https://scrapbox.io/', 'https://scrapbox.io/api/pages/') + '/text';
const topUrl = `https://scrapbox.io/${project}`;
const out = join(here, 'out');
mkdirSync(join(out, 'browser'), { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1280, height: 800 },
  deviceScaleFactor: 1,
  locale: 'ja-JP',
  colorScheme: 'light',
  recordVideo: { dir: join(out, 'browser'), size: { width: 1280, height: 800 } },
});
const sid = process.env.COSENSE_SID;
if (sid) {
  await ctx.addCookies([{ name: 'connect.sid', value: sid, domain: 'scrapbox.io', path: '/', httpOnly: true, secure: true }]);
}
// UserScript の確認バナーはページを移るたびに出るので、CSS で最初から隠す
await ctx.addInitScript(() => {
  document.addEventListener('DOMContentLoaded', () => {
    const st = document.createElement('style');
    st.textContent = '.userscript-alert{display:none !important}';
    document.head.appendChild(st);
  });
});
const page = await ctx.newPage();
const t0 = Date.now(); // 動画はページを作った時点から始まる
await page.goto(topUrl, { waitUntil: 'networkidle' });
console.log(`browser: ${topUrl} を開いた(${Date.now() - t0}ms)`);

// ページができたらそこへ移る。cosentty 側の作成は tape の途中で起きる
const headers = sid ? { Cookie: `connect.sid=${sid}` } : {};
let moved = false;
const watcher = setInterval(async () => {
  if (moved) return;
  try {
    const r = await fetch(apiUrl, { headers });
    if (r.ok) {
      moved = true; clearInterval(watcher);
      await page.waitForTimeout(1500); // 一覧に現れたのを一拍見せてから
      await page.goto(url, { waitUntil: 'domcontentloaded' });
      console.log(`browser: ページができたので ${url} へ移った`);
    }
  } catch {}
}, 700);

let rec;
try {
  rec = await recordTape(tape, { out });
} finally {
  clearInterval(watcher);
}

const video = page.video();
await ctx.close();
const rawPath = await video.path();
await browser.close();
const browserVideo = join(out, 'browser.webm');
renameSync(rawPath, browserVideo);

// 端末の録画区間(実時間)をブラウザ動画から切り出して並べる
const dur = (f) => parseFloat(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', f]).toString());
const termDur = dur(rec.video);
const offset = (rec.tShow - t0) / 1000;
console.log(`term ${termDur.toFixed(1)}s、ブラウザは ${offset.toFixed(1)}s から`);

const side = join(out, 'side.mp4');
execFileSync('ffmpeg', ['-loglevel', 'error', '-y',
  '-i', rec.video, '-ss', offset.toFixed(3), '-t', termDur.toFixed(3), '-i', browserVideo,
  '-filter_complex', '[0:v]scale=-2:800,fps=20[t];[1:v]scale=-2:800,fps=20[b];[t][b]hstack=inputs=2:shortest=1[v]',
  '-map', '[v]', '-an', '-pix_fmt', 'yuv420p', side]);
const gif = join(out, 'side.gif');
execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', side,
  '-filter_complex', 'fps=10,scale=1800:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=3',
  gif]);
console.log(`書いた: ${side} / ${gif}`);
