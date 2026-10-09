# applejack-engine — headless playback engine for Applejack

castLabs Electron (official Widevine CDM, `+wvcus` builds) + a hidden window running
MusicKit JS v3, controlled over stdio with JSON-lines RPC. Later the final UI
(Rust + GTK4/libadwaita) spawns the same engine and talks the same protocol.

No custom CDM, no DRM workarounds: decryption is done by Google's Widevine component
that castLabs Electron downloads, exactly as in Chrome.

## Modes

| mode | page | token | when to use |
|---|---|---|---|
| `webplayer` (default) | the real `https://music.apple.com` | Apple's web player itself handles everything; we only drive its `MusicKit.getInstance()` | works with just a subscription |
| `devtoken` | `engine/player.html` on `http://127.0.0.1:47821` | **your own** developer token (Apple Developer Program → MusicKit key → JWT) | the "proper" path for your own client |

Not done on purpose: pulling the developer token out of the web player bundle and
serving our own page as `music.apple.com` (origin spoofing). That's how Apple's
token gets reused outside its own site, and I didn't build it.

## Setup (Arch)

```bash
sudo pacman -S nodejs npm      # Node >= 22
cd ~/Desktop/AM_linuxdev
npm run setup                  # resolves the latest stable +wvcus tag and runs npm install
```

`npm run setup -- --tag v44.5.1+wvcus` pins a version; `--prerelease` allows alpha builds.

npm 12 notes: git/URL dependencies are off by default, so the dependency is the tag's
GitHub tarball and `.npmrc` sets `allow-remote=root`; install scripts don't run, so
setup.sh downloads the Electron binary itself (`node node_modules/electron/install.js`).

If Electron crashes on startup with `Zygote process exited prematurely`, the env var
`CHROME_DESKTOP` was inherited from another Electron app (e.g. a terminal inside the
Claude app). `cli.js` strips it; for a manual run use `env -u CHROME_DESKTOP npx electron engine/main.js`.

## Run

```bash
npm run cli                    # interactive prompt
npm run cli -- --show          # keep the window visible (debugging)
npm run cli -- --verbose       # Chromium/engine logs in the terminal (otherwise engine.log)
```

At the `am>` prompt:

```
login                 opens music.apple.com — click "Sign In" there; the window hides once signed in
search daft punk      search songs in your storefront's catalog
play #1               play result 1 of the last search
play https://music.apple.com/...   song (?i=…), album or playlist link
play <id> [album]     play by catalog ID (IDs are storefront-specific)
pause / play / toggle / stop / next / prev
seek 1:30   seek +10   seek -10
vol 50
status
quit
```

devtoken mode:

```bash
AM_DEVELOPER_TOKEN="$(cat ~/.config/applejack/devtoken.jwt)" npm run cli -- --mode=devtoken
```

The session (cookies, MusicKit user token) lives in `~/.config/applejack/`
(partition `persist:am`), so you sign in once.

## RPC protocol (stdin/stdout, one JSON object per line)

Request: `{"id":1,"method":"play","params":{"id":"1440818839","type":"song"}}`
Response: `{"id":1,"result":{...}}` or `{"id":1,"error":{"message":"..."}}`
Event: `{"event":"state","data":{...}}`

Methods: `ping`, `login`, `logout`, `status`, `search {term, limit?}`, `play {id?, type?}`, `resume`, `pause`,
`toggle`, `stop`, `next`, `previous`, `seek {seconds}`, `volume {value: 0..1}`,
`show`, `hide`, `reload`, `quit`.

Events: `widevine`, `engineReady`, `pageLoaded`, `ready`, `loginComplete`, `state`, `nowPlaying`,
`time` (≤1/s), `auth`, `error`.

Status/state shape:
`{state, authorized, position, duration, volume, nowPlaying: {id, type, title, artist, album, duration}}`

Calling it by hand: `env -u CHROME_DESKTOP npx electron engine/main.js`, then type JSON lines.

## Files

- `engine/main.js` — Electron main: Widevine, hidden window, RPC, sign-in popups
- `engine/preload.js` — minimal bridge (events from page → engine)
- `engine/inject.js` — controller over MusicKit v3, injected into the page
- `engine/player.html` — own page for devtoken mode
- `cli.js` — test client
- `scripts/setup.sh` — installs castLabs Electron

Status: verified end to end on Arch (linux-zen, Wayland) — sign-in, playback with Widevine, pause/seek/volume.

## Known risks / what to check first

- `window.MusicKit` must be reachable on music.apple.com — if Apple hides it,
  webplayer mode falls over (`MusicKit instance not available`) and the only option left is devtoken.
- Apple's site may reject the UA; we present a plain Chrome UA without `Electron/…`.
- Widevine on Linux needs no VMP signing; if the license still fails —
  `npm run cli -- --verbose` and look at the `error` events.

## GTK client (`ui/`)

Native GNOME client in Rust + GTK4/libadwaita. It starts the engine itself as a hidden
child process (no `npm run cli` needed — close the CLI first: two engines on one profile
crash on playback, so the engine now refuses to start a second time).

```bash
cd ui && cargo run --release   # or ./install.sh to install Applejack
```

- Sidebar: Search, Songs, Albums, Playlists; album/playlist pages with Play / Shuffle.
- Now-playing bar: artwork, play/pause, previous/next, seek, volume. Ctrl+Space play/pause,
  Ctrl+F search, Ctrl+R refresh library.
- Signing in: the banner's "Войти" button opens the engine's music.apple.com window.
- Library and track lists are cached in `~/.cache/applejack/cache.sqlite` (JSON per list),
  artwork in `~/.cache/applejack/art/`, engine log in `~/.cache/applejack/engine.log`.
- Engine folder: found next to the binary or via `AM_ENGINE_DIR`.

Engine methods added for the UI: `searchAll {term, limit?}`, `library {kind, offset?, limit?}`,
`tracks {id, type: album|playlist}`, `playSongs {ids, startWith?}`,
`playContainer {id, type, startWith?}`, `shuffle {on}`. Items carry `artwork` (template URL
with `{w}x{h}`) and `catalogId`.

Later additions:

- Library: `library` also takes `kind: artists | recently-added`. `libraryAll {kind}` → `{items, total}`
  returns the whole list in one call (implemented in main.js: 100-item pages, 4 in flight once the
  first page gives the total, then page by page if the list kept going).
- Pages: `home`, `browse` (charts), `radio`, `artist {id}`, `container {id, type}`,
  `songLinks {id}`, `lyrics {id}`, `api {path: "/v1/…", query?}` (`{sf}` in the path = storefront).
- Queue & playback: `queue`, `queueJump {index}`, `enqueue {id, type?, where?: next|later}`,
  `repeat {mode: 0|1|2}`, `playItem {id, type}`. Event `queue` fires when the queue changes.
- Library edits: `addToLibrary {id, type?}`, `rate {id, type?, value}`, `rating {id, type?}`.
- Queue extras: `clearQueue` (drops everything after the current track), `autoplay {on}`; queue items
  appended by autoplay carry `autoplay: true`, and `queue` / snapshots report `autoplay`.
- Search extras: `searchSuggestions {term}` → `{terms, top}`, `recentPlayed {limit?}`.
- Snapshots carry `seq`, bumped on every state / now-playing change; a client drops replies whose
  `seq` is older than the last event it saw. `pause` / `resume` / `toggle` return once the state has changed.

- Equalizer (`engine/eq.js`, injected before `inject.js`): `eq {enabled, preamp, bands: [{on, type, freq,
  gain, q, slope}]}` with `type` = bell | lowshelf | highshelf | lowcut | highcut | notch | bandpass and
  `slope` = 1–4 (×12 dB/oct, cuts only); `eqSpectrum {on}` streams `spectrum` events
  `{sr, pre, post, playing}` (200 log-spaced dB values, 20 Hz–20 kHz, ~45/s); `eqStatus`, `eqResponse {freqs}`.
  The web player's `<audio>` goes through Web Audio only once the equalizer or the analyzer was used:
  element → input → [preamp → biquads] → output → speakers, with analysers on input and output.
  Parameter moves glide; a change of band types crossfades to a new chain. The GTK client draws the curve
  from the same biquad formulas (`ui/src/ui/eq.rs`) and keeps its settings in `~/.config/applejack/eq.toml`.

## Performance notes

- While the engine window is hidden, main.js cancels the page's images, fonts, media and motion
  artwork video (`mvod.itunes.apple.com`). Audio (`aod-ssl.itunes.apple.com`), API calls and
  scripts pass. Measured on the home page: idle CPU 10–12% → 1–4%, engine memory (PSS)
  820–890 MB → ~600 MB. The window shown for sign-in loads everything.
- The page loads while Widevine registers; only playback methods (`play`, `playSongs`,
  `playContainer`, `playItem`, `queueJump`, `resume`, `toggle`, `next`, `previous`) wait for the CDM.
- `AM_ENGINE_PROFILE=/path` runs the engine on another profile folder, e.g. a copy of
  `~/.config/applejack` for benchmarks next to a running client (one engine per profile).
- GTK client: artwork comes over one keep-alive HTTP agent and is decoded off the UI thread
  (memory cache bounded to 96 MB); SQLite runs WAL + `synchronous=NORMAL` and skips rewrites of
  unchanged entries; `Cache::age` drives stale-while-revalidate refreshes.
- Never cache or store decrypted audio.

- Playlist edits (same API calls as the web player): `createPlaylist {name, description?, tracks?}`
  → the new library playlist, `addToPlaylist {id, tracks}`, `setPlaylistTracks {id, tracks}` (replaces the
  whole list: reorder / remove), `removeFromPlaylist {id, trackId}`, `renamePlaylist {id, name, description?}`.
  `tracks` = `[{id, type: songs | library-songs}]`. Library playlists carry `canEdit`.
- `lyrics` reports `syllable: true` for Apple's syllable-synced lyrics; each line then has
  `words: [{start, end, text, bg}]` (`bg` = background vocals).

GTK client extras: scrobbling (Last.fm, ListenBrainz or a ListenBrainz-compatible server; failed scrobbles wait in
`~/.config/applejack/scrobbles.json`), Discord Rich Presence over the local IPC socket, full-screen player (F11),
mini player (Ctrl+Shift+M; kept on top via a KWin script on Plasma, hyprctl / swaymsg, or wmctrl on X11),
Plasma colour scheme support. Release builds can bake in `APPLEJACK_LASTFM_KEY`, `APPLEJACK_LASTFM_SECRET`
and `APPLEJACK_DISCORD_CLIENT_ID` (environment at build time). Dev snapshots also take `AM_GTK_FULL=1`,
`AM_GTK_MINI=1`, `AM_GTK_PREFS=integrations`, `AM_GTK_EDIT=<playlist id>`.
- `capture {path}` (dev aid) saves the engine window's page as a PNG and reports its computed `display`.
  While the window is hidden the page carries `data-applejack-hidden` on `<html>` (display: none).
