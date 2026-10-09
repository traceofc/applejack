'use strict';
// applejack-engine: headless playback engine for Applejack.
// castLabs Electron (Widevine) + a hidden window running MusicKit JS v3,
// controlled by JSON-lines RPC over stdio. stdout carries ONLY protocol lines.
//
// Modes:
//   webplayer (default) — loads the real https://music.apple.com web player and drives
//                         the page's own MusicKit instance. Sign-in happens on Apple's page.
//   devtoken            — loads engine/player.html from 127.0.0.1 and configures MusicKit
//                         with YOUR developer token (Apple Developer Program, MusicKit key).

const path = require('path');
const fs = require('fs');
const http = require('http');
const util = require('util');
const readline = require('readline');
const electron = require('electron');
const { app, BrowserWindow, ipcMain, session } = electron;

// ---- stdout is the protocol channel: route all logging to stderr -------------
const log = (...a) => process.stderr.write('[engine] ' + util.format(...a) + '\n');
console.log = console.info = console.debug = log;
const send = (obj) => process.stdout.write(JSON.stringify(obj) + '\n');
const emit = (event, data) => send({ event, data });

// ---- options -------------------------------------------------------------------
function parseArgs(argv) {
  const o = {};
  for (const a of argv) {
    const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
    if (m) o[m[1]] = m[2] === undefined ? true : m[2];
  }
  return o;
}
const args = parseArgs(process.argv.slice(1));

function readDevToken() {
  if (process.env.AM_DEVELOPER_TOKEN) return process.env.AM_DEVELOPER_TOKEN.trim();
  const file = args['token-file'];
  if (file) return fs.readFileSync(file, 'utf8').trim();
  return null;
}

const devToken = readDevToken();
const opts = {
  mode: args.mode || (devToken ? 'devtoken' : 'webplayer'),
  show: !!args.show,
  port: Number(args.port || 47821),
  devToken,
};
if (!['webplayer', 'devtoken'].includes(opts.mode)) {
  log('unknown --mode', opts.mode);
  process.exit(2);
}
if (opts.mode === 'devtoken' && !opts.devToken) {
  log('devtoken mode needs AM_DEVELOPER_TOKEN or --token-file=PATH');
  process.exit(2);
}

// ---- app setup (before ready) ------------------------------------------------
app.setName('applejack-engine');
// AM_ENGINE_PROFILE points the engine at another profile (benchmarks next to a running client).
// The default profile is ~/.config/applejack; the pre-rename ~/.config/am-engine (sign-in,
// Widevine) is moved over on first start so nobody has to log in again.
const defaultProfile = path.join(app.getPath('appData'), 'applejack');
const oldProfile = path.join(app.getPath('appData'), 'am-engine');
if (!process.env.AM_ENGINE_PROFILE && !fs.existsSync(defaultProfile) && fs.existsSync(oldProfile)) {
  try { fs.renameSync(oldProfile, defaultProfile); } catch (e) { log('profile migration failed:', e.message); }
}
app.setPath('userData', process.env.AM_ENGINE_PROFILE || defaultProfile);
app.commandLine.appendSwitch('autoplay-policy', 'no-user-gesture-required');
// Chromium sizes the HTTP cache from free disk space, and played tracks (encrypted audio
// segments) filled it past a gigabyte. A fixed cap keeps recent tracks and API responses.
app.commandLine.appendSwitch('disk-cache-size', String(256 * 1024 * 1024));
// The GTK client publishes MPRIS itself; Chromium's own player ("chromium.instanceN")
// would show up as a second, nameless media control.
app.commandLine.appendSwitch('disable-features', 'HardwareMediaKeyHandling');
// The web player rejects unknown browsers; present a plain Chrome UA.
app.userAgentFallback = app.userAgentFallback.replace(/ (Electron|applejack-engine)\/\S+/g, '');

// Two engines on one profile crash (SIGTRAP) as soon as both start Widevine playback.
if (!app.requestSingleInstanceLock()) {
  emit('error', { code: 'locked', message: 'another applejack-engine is already running (e.g. npm run cli); close it first' });
  process.exit(3);
}

const PARTITION = 'persist:am';
// eq.js (equalizer / analyzer) first: the controller's eq* methods call into it.
const INJECT_SRC = ['eq.js', 'inject.js'].map((f) => fs.readFileSync(path.join(__dirname, f), 'utf8')).join(';\n');
const ALLOWED_POPUP_HOSTS = /(^|\.)apple\.com$/;

let win = null;
let markWidevineReady;
const widevineReady = new Promise((r) => (markWidevineReady = r));
let markPageReady;
let pageReady = new Promise((r) => (markPageReady = r));
let loginPending = false;

// ---- local page server for devtoken mode --------------------------------------
function startLocalServer() {
  const html = fs.readFileSync(path.join(__dirname, 'player.html'));
  const server = http.createServer((req, res) => {
    if (req.url === '/' || req.url.startsWith('/player.html')) {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
      res.end(html);
    } else {
      res.writeHead(404);
      res.end();
    }
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    // Fixed port => stable origin => MusicKit keeps the user token in localStorage.
    server.listen(opts.port, '127.0.0.1', () => resolve(`http://127.0.0.1:${opts.port}/player.html`));
  });
}

// ---- trimming the hidden page --------------------------------------------------
// Hidden, the web player's home page keeps streaming motion artwork (HLS video from
// mvod.itunes.apple.com) and loading images nobody sees. Drop those while the window
// is hidden; audio (aod*.itunes.apple.com), API calls and scripts are untouched.
const HIDDEN_BLOCK_TYPES = new Set(['image', 'media', 'font']);
const VIDEO_HOST = /^https:\/\/mvod\.itunes\.apple\.com\//;
function trimHiddenPage() {
  session.fromPartition(PARTITION).webRequest.onBeforeRequest((d, cb) => {
    const visible = win && !win.isDestroyed() && win.isVisible();
    const fromMain = win && !win.isDestroyed() && d.webContentsId === win.webContents.id;
    cb({ cancel: !visible && fromMain && (VIDEO_HOST.test(d.url) || HIDDEN_BLOCK_TYPES.has(d.resourceType)) });
  });
}

// While hidden, the whole page is display:none. The web player's scripts keep running
// (MusicKit, playback, our controller), but its UI no longer costs style, layout or
// paint work, and lazy images stay unloaded. Shown (sign-in, "show"), it renders normally.
const HIDDEN_CSS = 'html { display: none !important; }';
let hiddenCssKey = null;
async function syncPageVisibility() {
  if (!win || win.isDestroyed()) return;
  const wc = win.webContents;
  try {
    if (win.isVisible() && hiddenCssKey) {
      const key = hiddenCssKey;
      hiddenCssKey = null;
      await wc.removeInsertedCSS(key);
    } else if (!win.isVisible() && !hiddenCssKey) {
      hiddenCssKey = 'pending';
      hiddenCssKey = await wc.insertCSS(HIDDEN_CSS, { cssOrigin: 'user' });
      if (win.isVisible()) syncPageVisibility(); // shown while inserting
    }
  } catch (e) {
    hiddenCssKey = null;
    log('page visibility css:', e.message);
  }
}

// ---- window --------------------------------------------------------------------
async function createWindow() {
  win = new BrowserWindow({
    show: opts.show,
    width: 1100,
    height: 800,
    title: 'Applejack',
    autoHideMenuBar: true,
    webPreferences: {
      partition: PARTITION,
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      sandbox: true,
      backgroundThrottling: false,
      spellcheck: false,
      additionalArguments: [`--am-mode=${opts.mode}`],
    },
  });

  // Hide instead of destroying when the user closes the login window.
  win.on('close', (e) => {
    if (!app.isQuitting) {
      e.preventDefault();
      win.hide();
    }
  });
  win.on('show', syncPageVisibility);
  win.on('hide', syncPageVisibility);
  // Inserted CSS belongs to the document: put it back on every (re)load, before the page lays out.
  win.webContents.on('dom-ready', () => {
    hiddenCssKey = null;
    syncPageVisibility();
  });

  // Apple's sign-in opens popups (authorize.music.apple.com / idmsa.apple.com).
  win.webContents.setWindowOpenHandler(({ url }) => {
    let host = '';
    try { host = new URL(url).hostname; } catch {}
    if (ALLOWED_POPUP_HOSTS.test(host)) {
      return {
        action: 'allow',
        overrideBrowserWindowOptions: {
          show: true, width: 520, height: 720, autoHideMenuBar: true,
          webPreferences: { partition: PARTITION },
        },
      };
    }
    log('blocked popup', url);
    return { action: 'deny' };
  });

  win.webContents.on('did-finish-load', async () => {
    try {
      await win.webContents.executeJavaScript(INJECT_SRC, true);
      markPageReady();
      emit('pageLoaded', { url: win.webContents.getURL() });
    } catch (e) {
      log('inject failed', e.message);
    }
  });
  win.webContents.on('render-process-gone', (_e, d) => {
    emit('error', { message: `renderer gone: ${d.reason}` });
    pageReady = new Promise((r) => (markPageReady = r));
    win.webContents.reload();
  });
  win.webContents.on('console-message', (e) => {
    if (e.level === 'error') log('[page]', e.message);
  });

  const url = opts.mode === 'devtoken' ? await startLocalServer() : 'https://music.apple.com/';
  log('loading', url);
  await win.loadURL(url);
}

// ---- page bridge ----------------------------------------------------------------
const NEEDS_CDM = new Set(['play', 'resume', 'toggle', 'playSongs', 'playContainer', 'playItem', 'queueJump', 'next', 'previous']);
async function pageCall(method, params) {
  if (NEEDS_CDM.has(method)) await widevineReady;
  await pageReady;
  const code = `window.__am ? window.__am.call(${JSON.stringify(method)}, ${JSON.stringify(params || {})})`
    + ` : { ok: false, error: 'controller not injected' }`;
  const r = await win.webContents.executeJavaScript(code, true);
  if (!r || !r.ok) throw new Error((r && r.error) || 'page call failed');
  return r.value;
}

ipcMain.on('am:event', (e, type, data) => {
  if (!win || e.sender !== win.webContents) return;
  emit(type, data);
  if (type === 'auth' && data && data.authorized && loginPending) finishLogin();
});
ipcMain.handle('am:config', (e) => {
  if (!win || e.sender !== win.webContents || opts.mode !== 'devtoken') return null;
  return { developerToken: opts.devToken, app: { name: 'Applejack', build: app.getVersion() } };
});

// ---- RPC ------------------------------------------------------------------------
// Sign-in goes through the web player's own "Sign In" button: calling
// MusicKit authorize() behind the page's back got the session revoked right away.
async function login() {
  win.show();
  win.focus();
  const st = await pageCall('status', {}).catch(() => null);
  if (st && st.authorized) {
    if (!opts.show) win.hide();
    return { authorized: true };
  }
  loginPending = true;
  return { authorized: false, note: 'sign in with the "Sign In" button in the window; it hides once you are signed in' };
}

// Hide the window only once authorization has held for a few seconds.
let loginTimer = null;
function finishLogin() {
  clearTimeout(loginTimer);
  loginTimer = setTimeout(async () => {
    const st = await pageCall('status', {}).catch(() => null);
    if (!st || !st.authorized) return;
    loginPending = false;
    if (!opts.show) win.hide();
    emit('loginComplete', { authorized: true });
  }, 3000);
}

// The whole library list in one call. Pages are 100 items (API maximum); once the
// first page tells the total, the rest are fetched a few at a time instead of one by one.
const LIBRARY_PAGE = 100;
const LIBRARY_PARALLEL = 4;
const LIBRARY_MAX = 50000;
async function libraryAll({ kind = 'songs' } = {}) {
  const page = (offset) => pageCall('library', { kind, offset, limit: LIBRARY_PAGE });
  const first = await page(0);
  const items = [...first.items];
  let next = first.next;
  const total = Math.min(first.total || 0, LIBRARY_MAX);
  if (next !== null && total > next) {
    const offsets = [];
    for (let o = next; o < total; o += LIBRARY_PAGE) offsets.push(o);
    const pages = new Array(offsets.length);
    let cursor = 0;
    const worker = async () => {
      while (cursor < offsets.length) {
        const i = cursor++;
        pages[i] = await page(offsets[i]);
      }
    };
    await Promise.all(Array.from({ length: Math.min(LIBRARY_PARALLEL, offsets.length) }, worker));
    for (const p of pages) items.push(...p.items);
    const last = pages[pages.length - 1];
    next = last && last.next !== null && last.items.length ? offsets[offsets.length - 1] + last.items.length : null;
  }
  // No total, or the library grew meanwhile: carry on page by page.
  while (next !== null && items.length < LIBRARY_MAX) {
    const p = await page(next);
    if (!p.items.length) break;
    items.push(...p.items);
    next = p.next !== null ? next + p.items.length : null;
  }
  return { items, total: items.length };
}

async function handle(method, params) {
  switch (method) {
    case 'ping':
      return {
        pong: true,
        mode: opts.mode,
        electron: process.versions.electron,
        chrome: process.versions.chrome,
        widevine: electron.components ? electron.components.status() : null,
      };
    case 'show': win.show(); return true;
    case 'hide': win.hide(); return true;
    case 'reload': pageReady = new Promise((r) => (markPageReady = r)); win.webContents.reload(); return true;
    case 'login': return login();
    case 'logout': return pageCall('unauthorize', params);
    case 'quit': setImmediate(quit); return true;
    case 'libraryAll': return libraryAll(params || {});
    default: return pageCall(method, params);
  }
}

function startRpc() {
  const rl = readline.createInterface({ input: process.stdin, terminal: false });
  rl.on('line', async (line) => {
    line = line.trim();
    if (!line) return;
    let msg;
    try { msg = JSON.parse(line); } catch { return send({ id: null, error: { message: 'invalid JSON' } }); }
    const { id = null, method, params } = msg;
    try {
      send({ id, result: await handle(method, params) });
    } catch (e) {
      send({ id, error: { message: e.message } });
    }
  });
  rl.on('close', quit); // parent went away
  // A killed client does not always close our stdin (the pipe can outlive it), and an
  // orphaned engine would hold the profile lock against the next start.
  const parent = process.ppid;
  setInterval(() => { if (process.ppid !== parent) quit(); }, 2000).unref();
}

function quit() {
  app.isQuitting = true;
  app.quit();
}

// ---- boot -----------------------------------------------------------------------
app.on('window-all-closed', () => {}); // keep running headless
app.whenReady().then(async () => {
  startRpc();
  trimHiddenPage();
  // The page loads while Widevine registers; only playback waits for the CDM.
  const cdm = (async () => {
    if (!electron.components) {
      emit('error', { message: 'not a castLabs Electron build: no Widevine (run npm run setup)' });
      return;
    }
    try {
      await electron.components.whenReady();
      emit('widevine', electron.components.status());
    } catch (e) {
      emit('error', { message: `Widevine component failed: ${e.message || e}` });
    }
  })().finally(markWidevineReady);
  await createWindow();
  await cdm;
  emit('engineReady', { mode: opts.mode });
}).catch((e) => {
  log('fatal', e && e.stack || e);
  process.exit(1);
});
