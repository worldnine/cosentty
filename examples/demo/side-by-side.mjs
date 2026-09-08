// side-by-side.mjs — 端末(VHS)とブラウザ(Playwright)を同時に撮って左右に並べる
//
//   COSENTTY_PROJECT=<sandbox> node examples/demo/side-by-side.mjs [tape] [url]
//
// 既定は edit.tape と、その tape が作るページ https://scrapbox.io/<project>/cosentty demo。
// ブラウザは拡張もブックマークも無い素の Chromium(ヘッドレス)を 1280x800 で開き、
// COSENSE_SID があれば cookie に入れて非公開プロジェクトも表示する。
// まだ無いページを開いておいても web は作成を拾わない(実測)ので、ブラウザは
// プロジェクトのトップで待ち、API でページができたのを見てからそのページへ移る。
// 以降の行追加は web が ws で受けてその場に現れる。
// VHS が `Show` を打った時刻と、tape を終えて動画を書き始めた(`Creating`)時刻を
// stdout から拾い、ブラウザ動画をその区間で切り出して端末動画と時間を合わせる。
// 出力は examples/demo/out/side.mp4 と side.gif。
import { chromium } from 'playwright';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, renameSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const project = process.env.COSENTTY_PROJECT;
if (!project) { console.error('COSENTTY_PROJECT を指定する'); process.exit(2); }
const tape = process.argv[2] ?? 'examples/demo/edit.tape';
const url = process.argv[3] ?? `https://scrapbox.io/${project}/cosentty%20demo`;
const apiUrl = url.replace('https://scrapbox.io/', 'https://scrapbox.io/api/pages/') + '/text';
const topUrl = `https://scrapbox.io/${project}`;
const out = 'examples/demo/out';
const termVideo = join(out, tape.replace(/^.*\//, '').replace(/\.tape$/, '.mp4'));
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

// ページができたらそこへ移る。cosentty 側の作成は VHS の途中で起きる
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

// VHS を起こす。出力行を流しつつ、Show の時刻を覚える
// VHS は各コマンドを実行する直前に 1 行ずつ出力する。その時刻を控えておく。
// 動画の時間は tape に書いた通り(Sleep 1s は 1 秒)だが、実時間はスクリーン
// ショットの分だけ不均一に伸びる(実測で約 2 倍)ので、あとで区間ごとに合わせる。
let tShow = null, tEnd = null;
const events = []; // { wall, line }
const vhs = spawn('vhs', [tape], { stdio: ['ignore', 'pipe', 'inherit'], env: process.env });
vhs.stdout.on('data', (d) => {
  for (const raw of d.toString().split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    const now = Date.now();
    if (line === 'Show' && tShow === null) tShow = now;
    if (line.startsWith('Creating') && tEnd === null) tEnd = now;
    if (tShow !== null && tEnd === null) events.push({ wall: now, line: raw.replace(/^\s+|\s+$/g, '') });
    process.stdout.write(`vhs: ${line}\n`);
  }
});
const code = await new Promise((r) => vhs.on('exit', r));
clearInterval(watcher);
if (code !== 0) { console.error(`vhs が ${code} で終わった`); }
if (tShow === null) { console.error('vhs の Show を検出できなかった'); tShow = t0; }
if (tEnd === null) tEnd = Date.now();

const video = page.video();
await ctx.close();
const rawPath = await video.path();
await browser.close();
const browserVideo = join(out, 'browser.webm');
renameSync(rawPath, browserVideo);

// 端末動画(tape の時間)とブラウザ動画(実時間)を区間ごとに合わせる。
// 各コマンドの「tape 上の長さ」(Sleep はその秒数、Type は文字数×打鍵間隔、
// キー1つは 0)を実測の動画長に比例配分し、ブラウザ側は実時間の区間を
// その長さに伸縮して繋ぐ。
const dur = (f) => parseFloat(execFileSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', f]).toString());
const termDur = dur(termVideo);
let typingMs = 50;
for (const m of readFileSync(tape, 'utf8').matchAll(/^Set TypingSpeed (\d+)ms/mg)) typingMs = parseInt(m[1]);
const nominal = (line) => {
  let m;
  if ((m = line.match(/^Sleep ([\d.]+)(ms|s)$/))) return parseFloat(m[1]) * (m[2] === 'ms' ? 0.001 : 1);
  // VHS の出力は `Type 60ms abc def` / `Type abc`(引用符なし、@ は空白になる)
  if ((m = line.match(/^Type(?: (\d+)ms)? (.*)$/))) return [...m[2]].length * (m[1] ? parseInt(m[1]) : typingMs) / 1000;
  return 0;
};
const segs = [];
for (let i = 0; i < events.length; i++) {
  const wallStart = (events[i].wall - t0) / 1000;
  const wallEnd = ((i + 1 < events.length ? events[i + 1].wall : tEnd) - t0) / 1000;
  segs.push({ wallStart, wallEnd, nominal: nominal(events[i].line) });
}
const nominalSum = segs.reduce((a, s) => a + s.nominal, 0);
const scale = termDur / nominalSum;
console.log(`term ${termDur.toFixed(1)}s / tape ${nominalSum.toFixed(1)}s / wall ${((tEnd - tShow) / 1000).toFixed(1)}s`);
const parts = [];
let filter = '';
let n = 0;
for (const sg of segs) {
  const vlen = sg.nominal * scale;
  const wlen = sg.wallEnd - sg.wallStart;
  if (vlen < 0.02 || wlen <= 0) continue;
  filter += `[1:v]trim=start=${sg.wallStart.toFixed(3)}:end=${sg.wallEnd.toFixed(3)},setpts=(PTS-STARTPTS)*${(vlen / wlen).toFixed(4)}[s${n}];`;
  parts.push(`[s${n}]`); n++;
}
filter += `${parts.join('')}concat=n=${n}:v=1:a=0,scale=-2:800,fps=20[b];[0:v]scale=-2:800,fps=20[t];[t][b]hstack=inputs=2:shortest=1[v]`;

const side = join(out, 'side.mp4');
execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', termVideo, '-i', browserVideo,
  '-filter_complex', filter, '-map', '[v]', '-an', '-pix_fmt', 'yuv420p', side]);
const gif = join(out, 'side.gif');
execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', side,
  '-filter_complex', 'fps=10,scale=1800:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=3',
  gif]);
console.log(`書いた: ${side} / ${gif}`);
