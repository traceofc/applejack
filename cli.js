#!/usr/bin/env node
'use strict';
// Small interactive client for the engine: spawns it and speaks JSON-lines RPC.
//   node cli.js                      interactive prompt
//   node cli.js play 1440818839      run a command first, then stay interactive
// Flags (passed through to the engine): --mode=webplayer|devtoken --show --token-file=PATH
//   --verbose  show engine/Chromium stderr instead of writing it to engine.log

const path = require('path');
const fs = require('fs');
const readline = require('readline');
const { spawn } = require('child_process');

let electronBin;
try {
  electronBin = require('electron'); // path to the binary when required from plain Node
} catch {
  console.error('electron is not installed — run: npm run setup');
  process.exit(1);
}

const argv = process.argv.slice(2);
const engineFlags = argv.filter((a) => a.startsWith('--') && a !== '--verbose');
const verbose = argv.includes('--verbose');
const initial = argv.filter((a) => !a.startsWith('--'));

const logPath = path.join(__dirname, 'engine.log');
const engine = spawn(electronBin, [path.join(__dirname, 'engine', 'main.js'), ...engineFlags], {
  stdio: ['pipe', 'pipe', verbose ? 'inherit' : 'pipe'],
  // CHROME_DESKTOP inherited from another Electron app (e.g. when launched from its
  // terminal) makes the zygote of this Electron build crash at startup.
  env: (({ ELECTRON_RUN_AS_NODE, CHROME_DESKTOP, ...env }) => env)(process.env),
});
if (!verbose) engine.stderr.pipe(fs.createWriteStream(logPath, { flags: 'w' }));

// Full protocol transcript (requests, responses, events) for debugging.
const transcript = fs.createWriteStream(path.join(__dirname, 'session.log'), { flags: 'w' });
const trace = (dir, text) => transcript.write(`${new Date().toISOString()} ${dir} ${text}\n`);

let nextId = 1;
const pending = new Map();
function call(method, params) {
  const id = nextId++;
  const line = JSON.stringify({ id, method, params });
  trace('>>', line);
  engine.stdin.write(line + '\n');
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}

const fmtTime = (s) => {
  if (!Number.isFinite(s)) return '--:--';
  s = Math.max(0, Math.floor(s));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};
const fmtItem = (it) => (it ? `${it.artist || '?'} — ${it.title || it.id}` : '(nothing)');
const fmtStatus = (s) =>
  `[${s.state}] ${fmtItem(s.nowPlaying)}  ${fmtTime(s.position)} / ${fmtTime(s.duration)}` +
  `  vol ${Math.round((s.volume ?? 1) * 100)}%  ${s.authorized ? 'signed in' : 'NOT signed in'}`;

const rl = readline.createInterface({ input: process.stdin, output: process.stdout, prompt: 'am> ' });
const say = (msg) => {
  readline.clearLine(process.stdout, 0);
  readline.cursorTo(process.stdout, 0);
  console.log(msg);
  rl.prompt(true);
};

let lastState = null;
readline.createInterface({ input: engine.stdout }).on('line', (line) => {
  trace('<<', line);
  let msg;
  try { msg = JSON.parse(line); } catch { return say(`(engine) ${line}`); }
  if (msg.id !== undefined && msg.id !== null && pending.has(msg.id)) {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    return msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result);
  }
  const d = msg.data || {};
  switch (msg.event) {
    case 'engineReady': say(`engine up (mode: ${d.mode})`); break;
    case 'widevine': say(`widevine: ${JSON.stringify(d)}`); break;
    case 'ready': say(`player ready — ${fmtStatus(d)}`); break;
    case 'nowPlaying': say(`♪ ${fmtItem(d)}`); break;
    case 'state': if (d.state !== lastState) { lastState = d.state; say(`state: ${d.state}`); } break;
    case 'auth': say(d.authorized ? 'auth: authorized' : 'auth: unauthorized'); break;
    case 'loginComplete': say('signed in ✔ — window hidden; try: play 1440818839'); break;
    case 'error': say(`error: ${d.message}`); break;
    case 'time': case 'pageLoaded': break;
    default: if (msg.event) say(`${msg.event}: ${JSON.stringify(d)}`); else if (msg.error) say(`error: ${msg.error.message}`);
  }
});

engine.on('exit', (code, sig) => {
  console.log(`\nengine exited (${sig || code})${verbose ? '' : ` — see ${logPath}`}`);
  process.exit(code || 0);
});

function parseTime(t) {
  if (/^\d+:\d{1,2}$/.test(t)) { const [m, s] = t.split(':').map(Number); return m * 60 + s; }
  return Number(t);
}

let lastResults = [];

// Accepts a catalog ID, #N from the last search, or a music.apple.com link.
function resolveTarget(arg, type) {
  const n = /^#(\d+)$/.exec(arg);
  if (n) {
    const it = lastResults[Number(n[1]) - 1];
    return it ? { id: it.id, type: 'song' } : null;
  }
  if (/^https?:\/\//.test(arg)) {
    let u;
    try { u = new URL(arg); } catch { return null; }
    if (u.searchParams.get('i')) return { id: u.searchParams.get('i'), type: 'song' };
    const m = /\/(song|album|playlist|station)\/(?:[^/]+\/)?([^/?]+)$/.exec(u.pathname);
    return m ? { id: m[2], type: m[1] } : null;
  }
  return { id: arg, type: type || 'song' };
}

const HELP = `commands:
  login                     sign in (opens a window)
  logout
  search <words>            find songs in the catalog
  play #N                   play result N of the last search
  play <music.apple.com link>
  play <id> [song|album|playlist|station]   play by catalog ID (default: song)
  play | resume             resume
  pause | toggle | stop | next | prev
  seek <sec|m:ss>           absolute seek;  seek +10 / seek -10  relative
  vol <0-100>
  status | show | hide | ping
  quit`;

async function run(line) {
  trace('$', line);
  const [cmd, ...rest] = line.trim().split(/\s+/);
  switch (cmd) {
    case '': return;
    case 'help': case '?': return say(HELP);
    case 'login': {
      say('opening the sign-in window…');
      const r = await call('login');
      return say(r.authorized ? 'already signed in' : r.note);
    }
    case 'logout': await call('logout'); return say('signed out');
    case 'search': {
      lastResults = await call('search', { term: rest.join(' ') });
      if (!lastResults.length) return say('nothing found');
      return say(lastResults.map((it, n) => `  #${n + 1}  ${fmtItem(it)}  (${fmtTime(it.duration)})  id ${it.id}`).join('\n')
        + '\nplay one with: play #N');
    }
    case 'play': {
      if (!rest[0]) return say(fmtStatus(await call('resume')));
      const target = resolveTarget(rest[0], rest[1]);
      if (!target) return say('usage: play <id|#N|music.apple.com link> [song|album|playlist|station]');
      return say(fmtStatus(await call('play', target)));
    }
    case 'resume': return say(fmtStatus(await call('resume')));
    case 'pause': return say(fmtStatus(await call('pause')));
    case 'toggle': return say(fmtStatus(await call('toggle')));
    case 'stop': return say(fmtStatus(await call('stop')));
    case 'next': return say(fmtStatus(await call('next')));
    case 'prev': case 'previous': return say(fmtStatus(await call('previous')));
    case 'seek': {
      const arg = rest[0] || '';
      let seconds;
      if (/^[+-]/.test(arg)) seconds = (await call('status')).position + Number(arg);
      else seconds = parseTime(arg);
      if (!Number.isFinite(seconds)) return say('usage: seek <sec|m:ss|+N|-N>');
      return say(fmtStatus(await call('seek', { seconds: Math.max(0, seconds) })));
    }
    case 'vol': case 'volume':
      return say(fmtStatus(await call('volume', { value: Number(rest[0]) / 100 })));
    case 'status': return say(fmtStatus(await call('status')));
    case 'show': await call('show'); return;
    case 'hide': await call('hide'); return;
    case 'ping': return say(JSON.stringify(await call('ping'), null, 2));
    case 'quit': case 'exit': await call('quit').catch(() => {}); return;
    default: return say(`unknown command "${cmd}" — type help`);
  }
}

rl.on('line', (line) => run(line).catch((e) => say(`error: ${e.message}`)).finally(() => rl.prompt()));
rl.on('close', () => { call('quit').catch(() => {}); setTimeout(() => process.exit(0), 2000).unref(); });

say('applejack-engine CLI — type help');
if (initial.length) run(initial.join(' ')).catch((e) => say(`error: ${e.message}`));
