// record-terminal.mjs — VHS の tape を本物の端末(WezTerm)で再生し、画面収録で撮る
//
//   node examples/demo/record-terminal.mjs examples/demo/view.tape
//
// VHS の端末(xterm.js)は画像を描けないので、kitty 画像プロトコルを話す WezTerm を
// 使う。`wezterm cli send-text` でキーを流し、`wezterm cli get-text` で画面を読み、
// macOS の `screencapture -v` で窓の領域だけを録画する(初回は「画面収録」の許可が要る)。
// 対応する tape の命令: Type / Type@Nms / Enter / Escape / Tab / Left / Right / Up / Down /
// Backspace / Sleep / Hide / Show / Wait+Screen@T /re/ / Set TypingSpeed。Output と
// その他の Set は無視する。録画は Show から Hide(または tape の終わり)まで。
// 出力は examples/demo/out/<tape名>.mp4。side-by-side.mjs からは関数として使う。
import { spawn, execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// tape を命令の列に読む
export function parseTape(text) {
  const cmds = [];
  let typing = 50;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    let m;
    if ((m = line.match(/^Set TypingSpeed (\d+)ms$/))) { typing = parseInt(m[1]); continue; }
    if (/^(Set|Output|Screenshot|Require|Env) /.test(line)) continue;
    if ((m = line.match(/^Type(?:@(\d+)ms)? "(.*)"(?: (Enter))?$/))) {
      cmds.push({ op: 'type', text: JSON.parse(`"${m[2]}"`), ms: m[1] ? parseInt(m[1]) : typing });
      if (m[3]) cmds.push({ op: 'key', key: 'Enter' });
      continue;
    }
    if ((m = line.match(/^(Enter|Escape|Tab|Left|Right|Up|Down|Backspace)(?: (\d+))?$/))) {
      for (let i = 0; i < (m[2] ? parseInt(m[2]) : 1); i++) cmds.push({ op: 'key', key: m[1] });
      continue;
    }
    if ((m = line.match(/^Sleep ([\d.]+)(ms|s)?$/))) { cmds.push({ op: 'sleep', ms: parseFloat(m[1]) * (m[2] === 'ms' ? 1 : 1000) }); continue; }
    if (line === 'Hide' || line === 'Show') { cmds.push({ op: line.toLowerCase() }); continue; }
    if ((m = line.match(/^Wait(?:\+Screen)?(?:@([\d.]+)s)? \/(.*)\/$/))) { cmds.push({ op: 'wait', re: new RegExp(m[2]), ms: (m[1] ? parseFloat(m[1]) : 15) * 1000 }); continue; }
    throw new Error(`tape の行を読めない: ${line}`);
  }
  return cmds;
}

const KEYS = { Enter: '\r', Escape: '\x1b', Tab: '\t', Left: '\x1b[D', Right: '\x1b[C', Up: '\x1b[A', Down: '\x1b[B', Backspace: '\x7f' };

class Wez {
  constructor(sock) { this.sock = sock; this.env = { ...process.env, WEZTERM_UNIX_SOCKET: sock }; }
  cli(...args) { return execFileSync('wezterm', ['cli', ...args], { env: this.env, stdio: ['ignore', 'pipe', 'pipe'] }).toString(); }
  paneId() { return JSON.parse(this.cli('list', '--format', 'json'))[0].pane_id; }
  send(text) { this.cli('send-text', '--pane-id', String(this.pane), '--no-paste', '--', text); }
  screen() { return this.cli('get-text', '--pane-id', String(this.pane)); }
}

// 窓の位置と大きさ(ポイント)。System Events 経由(アクセシビリティの許可が要る)
function windowBounds() {
  const out = execFileSync('osascript', ['-e', 'tell application "System Events" to get {position, size} of window 1 of process "wezterm-gui"']).toString().trim();
  const [x, y, w, h] = out.split(',').map((s) => parseInt(s.trim()));
  return { x, y, w, h };
}

/**
 * tape を WezTerm で再生して録画する。
 * 戻り値: { video, tShow, tHide } — 録画ファイルと、録画した実時間の区間(epoch ms)
 * onCommand(line) は各命令の実行前に呼ばれる(side-by-side の同期用)。
 */
export async function recordTape(tapePath, { out = join(here, 'out'), position = 'main:300,200', onShow, onHide } = {}) {
  const cmds = parseTape(readFileSync(tapePath, 'utf8'));
  mkdirSync(out, { recursive: true });
  const name = tapePath.replace(/^.*\//, '').replace(/\.tape$/, '');
  const mov = join(out, `${name}.mov`);
  const video = join(out, `${name}.mp4`);

  // WezTerm を起こす。gui のソケットは gui-sock-<pid>
  // WezTerm は kitty 画像の Unicode プレースホルダ(ratatui-image の kitty 経路)を描けないので、
  // 画像は iTerm2 方式で送る(WezTerm はこちらをきちんと描く)
  const env = { COSENSE_IMAGE: 'iterm2', ...process.env };
  const gui = spawn('wezterm-gui', ['--config-file', join(here, 'wezterm.lua'), 'start', '--always-new-process', '--position', position, '--cwd', process.cwd(), '--', '/bin/zsh', '-f'], { stdio: 'ignore', env });
  const sock = join(homedir(), '.local/share/wezterm', `gui-sock-${gui.pid}`);
  const wez = new Wez(sock);
  try {
    for (let i = 0; i < 100 && !existsSync(sock); i++) await sleep(100);
    await sleep(500);
    wez.pane = wez.paneId();
    // 前面に出し、位置を決める(--position は macOS では効かないことがある)。
    // 画面上端には常駐ウィジェットが重なりがちなので少し下げる
    const [px, py] = position.split(',').map((v) => parseInt(v.replace(/^.*:/, '')));
    execFileSync('osascript', ['-e', `tell application "System Events" to tell process "wezterm-gui"\nset frontmost to true\nset position of window 1 to {${px}, ${py}}\nend tell`]);
    await sleep(500);
    const b = windowBounds();
    console.log(`wezterm: pane ${wez.pane}, 窓 ${b.w}x${b.h} @ ${b.x},${b.y}`);

    let cap = null, tShow = null, tHide = null;
    const startCapture = () => {
      rmSync(mov, { force: true }); // screencapture は既存ファイルに上書きしない
      cap = spawn('screencapture', ['-v', '-x', '-R', `${b.x},${b.y},${b.w},${b.h}`, mov], { stdio: ['pipe', 'ignore', 'inherit'] });
      tShow = Date.now();
      onShow?.(tShow);
    };
    const stopCapture = async () => {
      if (!cap) return;
      tHide = Date.now();
      onHide?.(tHide);
      const done = new Promise((r) => cap.on('exit', r));
      cap.kill('SIGINT');
      await done;
      cap = null;
    };
    for (const c of cmds) {
      switch (c.op) {
        case 'type':
          for (const ch of c.text) { wez.send(ch); await sleep(c.ms); }
          break;
        case 'key': wez.send(KEYS[c.key]); await sleep(60); break;
        case 'sleep': await sleep(c.ms); break;
        case 'show': if (!cap) { startCapture(); await sleep(300); } break;
        case 'hide': await stopCapture(); break;
        case 'wait': {
          const t0 = Date.now();
          while (!c.re.test(wez.screen())) {
            if (Date.now() - t0 > c.ms) throw new Error(`画面に ${c.re} が出ない`);
            await sleep(300);
          }
          break;
        }
      }
    }
    await stopCapture();
    if (tShow === null) throw new Error('tape に Show が無い');
    // Retina の 2 倍解像度で撮れるので、高さ 800 に落として mp4 にする
    execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', mov, '-vf', 'scale=-2:800,fps=20', '-an', '-pix_fmt', 'yuv420p', video]);
    return { video, tShow, tHide };
  } finally {
    gui.kill();
  }
}

if (resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const tape = process.argv[2];
  if (!tape) { console.error('usage: node record-terminal.mjs <tape>'); process.exit(2); }
  const r = await recordTape(tape);
  const gif = r.video.replace(/\.mp4$/, '.gif');
  execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', r.video, '-filter_complex', 'fps=10,scale=1280:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=3', gif]);
  console.log(`書いた: ${r.video} / ${gif}(${((r.tHide - r.tShow) / 1000).toFixed(1)}s)`);
}
