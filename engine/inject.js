// Player controller, evaluated in the page's main world (both modes).
// Wraps the page's MusicKit v3 instance; the engine calls window.__am.call(method, params).
(() => {
  if (window.__am) return;

  const emit = (type, data) => { try { window.__amBridge && window.__amBridge.emit(type, data); } catch {} };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  async function instance(timeoutMs = 30000) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      try {
        const i = window.MusicKit && MusicKit.getInstance && MusicKit.getInstance();
        if (i) return i;
      } catch {}
      await sleep(50);
    }
    throw new Error('MusicKit instance not available on this page');
  }

  const stateName = (s) => {
    const S = (window.MusicKit && MusicKit.PlaybackStates) || {};
    return Object.keys(S).find((k) => S[k] === s) || String(s);
  };

  const attr = (it) => (it && it.attributes) || {};
  const rel = (it, name) => (it && it.relationships && it.relationships[name] && it.relationships[name].data) || [];
  const artworkOf = (it) => {
    const a = attr(it).artwork || (it && it.artwork) || attr(rel(it, 'catalog')[0]).artwork;
    return (a && a.url) || (it && it.artworkURL) || null;
  };
  const noteOf = (it) => {
    const n = attr(it).editorialNotes || attr(it).description;
    return (n && (n.short || n.tagline || n.standard)) || null;
  };
  // Normalized item shared by every method; `kind` is the API resource type
  // (songs, library-songs, albums, library-playlists, ...).
  const item = (it) => it && {
    id: it.id,
    type: it.type,
    kind: it.type,
    title: it.title || attr(it).name,
    artist: it.artistName || attr(it).artistName || attr(it).curatorName || null,
    album: it.albumName || attr(it).albumName || null,
    duration: (attr(it).durationInMillis || 0) / 1000,
    artwork: artworkOf(it),
    catalogId: (attr(it).playParams && attr(it).playParams.catalogId) || null,
    trackCount: attr(it).trackCount || null,
    trackNumber: attr(it).trackNumber || null,
    discNumber: attr(it).discNumber || null,
    releaseDate: attr(it).releaseDate || null,
    genre: (attr(it).genreNames || [])[0] || null,
    explicit: attr(it).contentRating === 'explicit',
    isLive: !!attr(it).isLive,
    note: noteOf(it),
    canEdit: attr(it).canEdit || undefined,
  };
  const items = (list) => (list || []).map(item).filter((x) => x && x.id);

  // Bumped on every state / now-playing change; the UI drops snapshots older than the
  // last event it saw (an RPC reply can be read before a pause lands, then arrive after it).
  let seq = 0;
  const snapshot = (i) => ({
    seq,
    state: stateName(i.playbackState),
    authorized: !!i.isAuthorized,
    position: i.currentPlaybackTime,
    duration: i.currentPlaybackDuration,
    volume: i.volume,
    nowPlaying: item(i.nowPlayingItem) || null,
    repeat: i.repeatMode || 0,
    shuffle: i.shuffleMode === 1,
    autoplay: !!i.autoplayEnabled,
  });
  // Queue items carry `autoplay: true` for the recommendations MusicKit appends at the end.
  const queueOf = (i) => {
    const q = i.queue;
    if (!q) return { position: -1, items: [], autoplay: !!i.autoplayEnabled };
    const out = [];
    (q.items || []).forEach((m, idx) => {
      const x = item(m);
      if (!x || !x.id) return;
      const qi = q.queueItem ? q.queueItem(idx) : null;
      x.autoplay = !!(qi && qi.isAutoplay);
      out.push(x);
    });
    return { position: q.position, items: out, autoplay: !!i.autoplayEnabled };
  };
  // Waits (briefly) until a transport command has actually landed.
  async function settle(cond, ms) {
    const t0 = Date.now();
    while (!cond() && Date.now() - t0 < ms) await sleep(40);
  }

  let wired = null;
  function wire(i) {
    if (wired === i) return;
    wired = i;
    const E = MusicKit.Events;
    let lastTick = 0;
    const queueChanged = () => emit('queue', queueOf(i));
    i.addEventListener(E.playbackStateDidChange, () => { seq++; emit('state', snapshot(i)); });
    i.addEventListener(E.nowPlayingItemDidChange, () => {
      seq++;
      emit('nowPlaying', item(i.nowPlayingItem) || null);
      emit('state', snapshot(i)); // carries the new seq
    });
    if (E.autoplayEnabledDidChange) i.addEventListener(E.autoplayEnabledDidChange, () => { emit('state', snapshot(i)); queueChanged(); });
    if (E.queueItemsDidChange) i.addEventListener(E.queueItemsDidChange, queueChanged);
    if (E.queuePositionDidChange) i.addEventListener(E.queuePositionDidChange, queueChanged);
    if (E.repeatModeDidChange) i.addEventListener(E.repeatModeDidChange, () => emit('state', snapshot(i)));
    if (E.shuffleModeDidChange) i.addEventListener(E.shuffleModeDidChange, () => emit('state', snapshot(i)));
    i.addEventListener(E.authorizationStatusDidChange, () => emit('auth', { authorized: !!i.isAuthorized }));
    i.addEventListener(E.mediaPlaybackError, (e) =>
      emit('error', { message: String((e && (e.message || e.errorCode || e.name)) || e) }));
    i.addEventListener(E.playbackTimeDidChange, () => {
      const now = Date.now();
      if (now - lastTick < 1000) return;
      lastTick = now;
      emit('time', { position: i.currentPlaybackTime, duration: i.currentPlaybackDuration });
    });
  }

  const QUEUE_KEYS = { song: 'song', album: 'album', playlist: 'playlist', station: 'station' };
  // songs / library-songs / albums ... -> song / album ...
  const queueKey = (type) => QUEUE_KEYS[String(type || '').replace(/^library-/, '').replace(/s$/, '')];
  // [{id, type}] for playlist edits: library ids (i.…) are library-songs, the rest catalog songs.
  const trackRefs = (list) => (list || []).filter((t) => t && t.id).map((t) => {
    const id = String(t.id);
    const type = t.type || t.kind || (/^i\./.test(id) ? 'library-songs' : 'songs');
    return { id, type: /^library-/.test(type) || type === 'songs' ? type : 'songs' };
  });

  // Writes (ratings, library adds) go straight to the music API with the page's
  // tokens: MusicKit's api.music() did not carry the method / body, so they never stuck.
  async function write(i, method, path, query = {}, body) {
    const url = new URL('https://amp-api.music.apple.com' + path.replace('{sf}', i.storefrontId || 'us'));
    for (const [k, v] of Object.entries(query)) url.searchParams.set(k, v);
    const headers = { authorization: 'Bearer ' + i.developerToken, 'music-user-token': i.musicUserToken };
    if (body !== undefined) headers['content-type'] = 'application/json';
    const r = await fetch(url, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: 'include' });
    if (!r.ok) throw new Error(`${method} ${path}: HTTP ${r.status}`);
    const text = await r.text();
    return text ? JSON.parse(text) : {};
  }
  async function api(i, path, query = {}, fetchOptions) {
    const p = path.replace('{sf}', i.storefrontId || 'us');
    // The user's own data is read fresh: MusicKit caches responses, and a library add or a
    // pin only showed up after a restart.
    if (!fetchOptions && p.startsWith('/v1/me/')) return write(i, 'GET', p, query);
    const r = await i.api.music(p, query, fetchOptions ? { fetchOptions } : undefined);
    return (r && r.data) || {};
  }
  // Recommendation / chart groups -> [{title, items}]
  const section = (title, list, extra = {}) => ({ title, items: items(list), ...extra });
  const nonEmpty = (secs) => secs.filter((x) => x && x.items && x.items.length);

  // TTML lyrics -> [{start, end, text}] (seconds; start = null when not time-synced)
  const clock = (t) => {
    if (!t) return null;
    const parts = String(t).replace(/s$/, '').split(':').map(Number);
    return parts.reduce((acc, x) => acc * 60 + x, 0);
  };
  // Syllable-synced TTML puts every syllable in a timed <span> (spaces are text nodes
  // between them); background vocals sit in <span ttm:role="x-bg">. Each line then carries
  // `words: [{start, end, text, bg}]`, where `text` keeps its trailing space.
  function syllables(node, bg, out) {
    for (const n of node.childNodes) {
      if (n.nodeType === 3) {
        const t = n.nodeValue.replace(/\s+/g, ' ');
        if (!t) continue;
        if (out.length) out[out.length - 1].text += t;
        continue;
      }
      if (n.nodeType !== 1) continue;
      const role = n.getAttribute('ttm:role') || n.getAttributeNS('http://www.w3.org/ns/ttml#metadata', 'role');
      if (role === 'x-bg') { syllables(n, true, out); continue; }
      if (role === 'x-translation' || role === 'x-roman') continue;
      const begin = n.getAttribute('begin');
      if (begin !== null && !n.getElementsByTagName('span').length) {
        out.push({ start: clock(begin), end: clock(n.getAttribute('end')), text: n.textContent.replace(/\s+/g, ' '), bg });
      } else {
        syllables(n, bg, out);
      }
    }
    return out;
  }
  function parseTtml(ttml) {
    const doc = new DOMParser().parseFromString(ttml, 'application/xml');
    const lines = [];
    for (const div of doc.getElementsByTagName('div')) {
      for (const p of div.getElementsByTagName('p')) {
        const words = syllables(p, false, []).filter((w) => w.text.trim() || w.text === ' ');
        const main = words.filter((w) => !w.bg).map((w) => w.text).join('').replace(/\s+/g, ' ').trim();
        lines.push({
          start: clock(p.getAttribute('begin')),
          end: clock(p.getAttribute('end')),
          text: words.length ? main : p.textContent.trim(),
          words: words.length ? words : undefined,
        });
      }
      lines.push({ start: null, end: null, text: '' }); // stanza break
    }
    while (lines.length && !lines[lines.length - 1].text) lines.pop();
    return lines;
  }

  const methods = {
    async status() { return snapshot(await instance()); },
    // Equalizer and analyzer (engine/eq.js).
    async eq(p) { return window.__amEq.set(p); },
    async eqSpectrum(p) { return window.__amEq.spectrum(p); },
    async eqStatus() { return window.__amEq.status(); },
    async eqResponse(p) { return window.__amEq.response(p); },
    async authorize() {
      const i = await instance();
      if (!i.isAuthorized) await i.authorize();
      return { authorized: !!i.isAuthorized };
    },
    async unauthorize() { const i = await instance(); await i.unauthorize(); return { authorized: !!i.isAuthorized }; },
    async play({ id, type = 'song' } = {}) {
      const i = await instance();
      if (id === undefined || id === null || id === '') { await i.play(); return snapshot(i); }
      const key = QUEUE_KEYS[type];
      if (!key) throw new Error(`unsupported type "${type}"`);
      await i.setQueue({ [key]: String(id), startPlaying: true });
      return snapshot(i);
    },
    async search({ term, limit = 10 } = {}) {
      const i = await instance();
      if (!term) throw new Error('search needs a term');
      const sf = i.storefrontId || 'us';
      const r = await i.api.music(`/v1/catalog/${sf}/search`, { term, types: 'songs', limit });
      const songs = (r && r.data && r.data.results && r.data.results.songs && r.data.results.songs.data) || [];
      return songs.map(item);
    },
    // Catalog search across types: {songs, albums, artists, playlists}.
    async searchAll({ term, limit = 15 } = {}) {
      const i = await instance();
      if (!term) throw new Error('search needs a term');
      const sf = i.storefrontId || 'us';
      const r = await i.api.music(`/v1/catalog/${sf}/search`, { term, types: 'songs,albums,artists,playlists,stations', limit });
      const res = (r && r.data && r.data.results) || {};
      const pick = (k) => ((res[k] && res[k].data) || []).map(item);
      return { songs: pick('songs'), albums: pick('albums'), artists: pick('artists'), playlists: pick('playlists'), stations: pick('stations') };
    },
    // One page of the user's library: kind = songs | albums | playlists | artists.
    async library({ kind = 'songs', offset = 0, limit = 100 } = {}) {
      const i = await instance();
      if (!i.isAuthorized) throw new Error('not signed in');
      if (!['songs', 'albums', 'playlists', 'artists', 'recently-added'].includes(kind)) throw new Error(`bad library kind "${kind}"`);
      const query = { limit: kind === 'recently-added' ? Math.min(limit, 25) : limit, offset };
      if (kind === 'artists') query.include = 'catalog';
      const r = { data: await write(i, 'GET', `/v1/me/library/${kind}`, query) };
      const d = (r && r.data) || {};
      const items = (d.data || []).map(item);
      return { items, next: d.next ? offset + items.length : null, total: (d.meta && d.meta.total) || null };
    },
    // All tracks of an album or playlist (library ids start with l. / p.).
    async tracks({ id, type = 'album' } = {}) {
      const i = await instance();
      if (!id) throw new Error('tracks needs an id');
      if (!['album', 'playlist'].includes(type)) throw new Error(`bad type "${type}"`);
      const lib = /^[lp]\./.test(String(id));
      const sf = i.storefrontId || 'us';
      const base = lib ? `/v1/me/library/${type}s/${id}/tracks` : `/v1/catalog/${sf}/${type}s/${id}/tracks`;
      const out = [];
      for (let offset = 0; offset < 5000;) {
        const r = lib ? { data: await write(i, 'GET', base, { limit: 100, offset }) } : await i.api.music(base, { limit: 100, offset });
        const d = (r && r.data) || {};
        const page = (d.data || []).map(item);
        out.push(...page);
        if (!d.next || !page.length) break;
        offset += page.length;
      }
      return out;
    },
    // Queue a list of songs (catalog or library ids) and start at index `startWith`.
    async playSongs({ ids, startWith = 0 } = {}) {
      const i = await instance();
      if (!Array.isArray(ids) || !ids.length) throw new Error('playSongs needs ids');
      await i.setQueue({ songs: ids.map(String), startWith: Number(startWith) || 0, startPlaying: true });
      return snapshot(i);
    },
    // Queue a whole album / playlist / station, optionally starting at a track index.
    async playContainer({ id, type = 'album', startWith = 0 } = {}) {
      const i = await instance();
      const key = QUEUE_KEYS[type];
      if (!key || type === 'song') throw new Error(`unsupported type "${type}"`);
      await i.setQueue({ [key]: String(id), startWith: Number(startWith) || 0, startPlaying: true });
      return snapshot(i);
    },
    async shuffle({ on } = {}) {
      const i = await instance();
      i.shuffleMode = on ? 1 : 0;
      return { shuffle: i.shuffleMode === 1 };
    },
    // ---- pages ----
    // Home ("Listen Now"): personal recommendation shelves.
    async home() {
      const i = await instance();
      if (!i.isAuthorized) throw new Error('not signed in');
      const r = await api(i, '/v1/me/recommendations', { limit: 30 });
      return nonEmpty((r.data || []).map((g) => section(
        (attr(g).title && attr(g).title.stringForDisplay) || '',
        rel(g, 'contents'),
        { hero: attr(g).display && attr(g).display.kind === 'MusicNotesHeroShelf' })));
    },
    // Browse: charts of the storefront.
    async browse() {
      const i = await instance();
      const r = await api(i, '/v1/catalog/{sf}/charts', {
        types: 'songs,albums,playlists', limit: 30, with: 'dailyGlobalTopCharts,cityCharts',
      });
      const res = r.results || {};
      const out = [];
      for (const c of res.songs || []) out.push(section(c.name || 'Top Songs', c.data, { songs: true }));
      for (const c of res.albums || []) out.push(section(c.name || 'Top Albums', c.data));
      for (const c of res.playlists || []) out.push(section(c.name || 'Top Playlists', c.data));
      for (const c of res.dailyGlobalTopCharts || []) out.push(section(c.name || 'Daily Top 100', c.data));
      for (const c of res.cityCharts || []) out.push(section(c.name || 'City Charts', c.data));
      return nonEmpty(out);
    },
    // Radio: live stations, stations for you, recently played stations.
    async radio() {
      const i = await instance();
      const [live, recent, recs] = await Promise.all([
        api(i, '/v1/catalog/{sf}/stations', { 'filter[featured]': 'apple-music-live-radio' }).catch(() => ({})),
        i.isAuthorized ? api(i, '/v1/me/recent/radio-stations', { limit: 20 }).catch(() => ({})) : {},
        i.isAuthorized ? api(i, '/v1/me/recommendations', { limit: 30 }).catch(() => ({})) : {},
      ]);
      const out = [section('Live Radio', live.data, { hero: true })];
      for (const g of recs.data || []) {
        const c = rel(g, 'contents');
        if (c.length && c.every((x) => x.type === 'stations')) out.push(section(attr(g).title.stringForDisplay, c));
      }
      out.push(section('Recently Played', recent.data));
      return nonEmpty(out);
    },
    // Artist page: library ids (r.…) are resolved to the catalog artist.
    async artist({ id } = {}) {
      const i = await instance();
      if (!id) throw new Error('artist needs an id');
      let catalogId = String(id);
      let libraryAlbums = [];
      if (/^r\./.test(catalogId)) {
        const [cat, albums] = await Promise.all([
          api(i, `/v1/me/library/artists/${id}/catalog`).catch(() => ({})),
          api(i, `/v1/me/library/artists/${id}/albums`, { limit: 100 }).catch(() => ({})),
        ]);
        libraryAlbums = albums.data || [];
        catalogId = cat.data && cat.data[0] && cat.data[0].id;
        if (!catalogId) {
          return { artist: { id, kind: 'library-artists', title: null }, bio: null, sections: nonEmpty([section('In Your Library', libraryAlbums)]) };
        }
      }
      const views = ['top-songs', 'latest-release', 'full-albums', 'singles', 'featured-playlists',
        'live-albums', 'compilation-albums', 'appears-on-albums', 'similar-artists'];
      const r = await api(i, `/v1/catalog/{sf}/artists/${catalogId}`, { views: views.join(','), extend: 'artistBio' });
      const a = (r.data || [])[0];
      if (!a) throw new Error('artist not found');
      const out = [];
      if (libraryAlbums.length) out.push(section('In Your Library', libraryAlbums, { key: 'library' }));
      for (const v of views) {
        const view = a.views && a.views[v];
        if (view) out.push(section((view.attributes && view.attributes.title) || v, view.data, { key: v, songs: v === 'top-songs' }));
      }
      return { artist: item(a), bio: attr(a).artistBio || null, sections: nonEmpty(out) };
    },
    // Album / playlist with its tracks and the artist to link to.
    async container({ id, type = 'album' } = {}) {
      const i = await instance();
      if (!id) throw new Error('container needs an id');
      const lib = /^[lp]\./.test(String(id));
      const path = lib ? `/v1/me/library/${type}s/${id}` : `/v1/catalog/{sf}/${type}s/${id}`;
      const r = await api(i, path, { include: lib ? 'catalog,artists' : 'artists' });
      const c = (r.data || [])[0];
      if (!c) throw new Error(`${type} not found`);
      const catalog = rel(c, 'catalog')[0];
      const artists = rel(c, 'artists').length ? rel(c, 'artists') : rel(catalog, 'artists');
      let artistId = artists[0] && artists[0].id;
      if (!artistId && catalog && type === 'album') {
        const cr = await api(i, `/v1/catalog/{sf}/albums/${catalog.id}`, { include: 'artists' }).catch(() => ({}));
        const ca = rel((cr.data || [])[0], 'artists')[0];
        artistId = ca && ca.id;
      }
      const tracks = await methods.tracks({ id, type });
      const info = item(c);
      if (!info.artwork && catalog) info.artwork = artworkOf(catalog);
      const a = { ...attr(catalog), ...attr(c) };
      return {
        info, tracks, artistId: artistId || null, catalogId: (catalog && catalog.id) || (lib ? null : c.id),
        copyright: a.copyright || null, description: noteOf(c) || noteOf(catalog), inLibrary: lib || !!a.inLibrary,
      };
    },
    // Album and artist of a song, for "go to album / artist".
    async songLinks({ id } = {}) {
      const i = await instance();
      let sid = String(id || '');
      if (/^i\./.test(sid)) {
        const r = await api(i, `/v1/me/library/songs/${sid}/catalog`).catch(() => ({}));
        sid = r.data && r.data[0] && r.data[0].id;
        if (!sid) throw new Error('song is not in the catalog');
      }
      const r = await api(i, `/v1/catalog/{sf}/songs/${sid}`, { include: 'albums,artists' });
      const s = (r.data || [])[0];
      const album = rel(s, 'albums')[0];
      const artist = rel(s, 'artists')[0];
      return { songId: sid, album: album ? item(album) : null, artistId: artist ? artist.id : null };
    },
    async lyrics({ id } = {}) {
      const i = await instance();
      const sid = String(id || '');
      if (!sid || /^i\./.test(sid)) throw new Error('lyrics need a catalog song id');
      for (const kind of ['syllable-lyrics', 'lyrics']) {
        const r = await api(i, `/v1/catalog/{sf}/songs/${sid}/${kind}`).catch(() => null);
        const ttml = r && r.data && r.data[0] && attr(r.data[0]).ttml;
        if (ttml) {
          const lines = parseTtml(ttml);
          return { synced: lines.some((l) => l.start !== null), syllable: lines.some((l) => l.words), lines };
        }
      }
      return { synced: false, lines: [] };
    },

    // ---- queue & modes ----
    async queue() { return queueOf(await instance()); },
    async queueJump({ index } = {}) {
      const i = await instance();
      await i.changeToMediaAtIndex(Number(index));
      return snapshot(i);
    },
    // where = next | later; type = song | album | playlist | station (or API types)
    async enqueue({ id, type = 'song', where = 'next' } = {}) {
      const i = await instance();
      const key = queueKey(type);
      if (!key) throw new Error(`unsupported type "${type}"`);
      const desc = { [key]: String(id) };
      if (where === 'later') await i.playLater(desc); else await i.playNext(desc);
      return queueOf(i);
    },
    // Removes everything after the current track (MusicKit refills autoplay if it is on).
    async clearQueue() {
      const i = await instance();
      const q = i.queue;
      if (q && q.clearAfterCurrent) q.clearAfterCurrent();
      else if (q && q.removeQueueItems) q.removeQueueItems((_x, idx) => idx > q.position);
      return queueOf(i);
    },
    async autoplay({ on } = {}) {
      const i = await instance();
      i.autoplayEnabled = !!on;
      return queueOf(i);
    },
    async repeat({ mode } = {}) {
      const i = await instance();
      i.repeatMode = Math.max(0, Math.min(2, Number(mode) || 0));
      return snapshot(i);
    },
    // Station / song / album / playlist by API type, e.g. from a shelf card.
    async playItem({ id, type } = {}) {
      const i = await instance();
      const key = queueKey(type);
      if (!key) throw new Error(`unsupported type "${type}"`);
      await i.setQueue({ [key]: String(id), startPlaying: true });
      return snapshot(i);
    },

    // ---- library edits ----
    async addToLibrary({ id, type = 'songs' } = {}) {
      const i = await instance();
      const t = String(type).replace(/^library-/, '');
      await write(i, 'POST', '/v1/me/library', { [`ids[${t}]`]: String(id) });
      return true;
    },
    // Library ids only (l. albums / i. songs / p. playlists). A playlist the user made is deleted.
    async removeFromLibrary({ id, type } = {}) {
      const i = await instance();
      const t = String(type || '').replace(/^library-/, '');
      if (!id || !['songs', 'albums', 'playlists'].includes(t)) throw new Error('removeFromLibrary needs a library id and type');
      await write(i, 'DELETE', `/v1/me/library/${t}/${id}`);
      return true;
    },
    // ---- playlist edits (the same API calls the web player makes) ----
    // tracks: [{id, type}] with type songs | library-songs (as listed by `tracks`).
    async createPlaylist({ name, description = '', tracks = [] } = {}) {
      const i = await instance();
      if (!name) throw new Error('createPlaylist needs a name');
      const body = { attributes: { name: String(name), description: String(description || '') } };
      const data = trackRefs(tracks);
      if (data.length) body.relationships = { tracks: { data } };
      const r = await write(i, 'POST', '/v1/me/library/playlists', {}, body);
      const pl = (r.data || [])[0];
      return pl ? item(pl) : null;
    },
    async addToPlaylist({ id, tracks = [] } = {}) {
      const i = await instance();
      const data = trackRefs(tracks);
      if (!/^p\./.test(String(id || '')) || !data.length) throw new Error('addToPlaylist needs a library playlist id and tracks');
      await write(i, 'POST', `/v1/me/library/playlists/${id}/tracks`, { representation: 'resources' }, { data });
      return true;
    },
    // Replaces the whole track list: this is how reordering and removing are saved.
    async setPlaylistTracks({ id, tracks = [] } = {}) {
      const i = await instance();
      if (!/^p\./.test(String(id || ''))) throw new Error('setPlaylistTracks needs a library playlist id');
      await write(i, 'PUT', `/v1/me/library/playlists/${id}/tracks`, {}, { data: trackRefs(tracks) });
      return true;
    },
    async removeFromPlaylist({ id, trackId } = {}) {
      const i = await instance();
      if (!id || !trackId) throw new Error('removeFromPlaylist needs id and trackId');
      await write(i, 'DELETE', `/v1/me/library/playlists/${id}/tracks`, { 'ids[library-songs]': String(trackId), mode: 'all' });
      return true;
    },
    async renamePlaylist({ id, name, description } = {}) {
      const i = await instance();
      if (!id || !name) throw new Error('renamePlaylist needs id and name');
      const attributes = { name: String(name) };
      if (description !== undefined) attributes.description = String(description);
      await write(i, 'PATCH', `/v1/me/library/playlists/${id}`, {}, { attributes });
      return true;
    },

    // value: 1 love, -1 dislike, 0 clear
    async rate({ id, type = 'songs', value = 1 } = {}) {
      const i = await instance();
      const t = String(type).replace(/^library-/, '');
      const path = `/v1/me/ratings/${t}/${id}`;
      if (!Number(value)) await write(i, 'DELETE', path);
      else await write(i, 'PUT', path, {}, { type: 'rating', attributes: { value: Number(value) > 0 ? 1 : -1 } });
      return { value: Number(value) };
    },
    async rating({ id, type = 'songs' } = {}) {
      const i = await instance();
      const t = String(type).replace(/^library-/, '');
      // Read without MusicKit's response cache, so a fresh love shows right away.
      const r = await write(i, 'GET', `/v1/me/ratings/${t}`, { ids: String(id) }).catch(() => ({}));
      const d = (r.data || [])[0];
      return { value: (d && attr(d).value) || 0 };
    },

    // ---- library pins (synced with the Music app's "Pins") ----
    async pins() {
      const i = await instance();
      if (!i.isAuthorized) throw new Error('not signed in');
      const r = await api(i, '/v1/me/library/pins', {
        meta: 'libraryPin', limit: 25,
        'include[library-songs]': 'albums,playlists,artists',
        'include[library-artists]': 'catalog', 'fields[artists]': 'artwork',
      });
      return (r.data || []).map((d) => {
        const x = item(d);
        if (x) x.pinAction = (d.meta && d.meta.libraryPin && d.meta.libraryPin.action) || null;
        return x;
      }).filter((x) => x && x.id);
    },
    // Library ids only (l. / p. / i. / r.): Apple pins items that are in the library.
    async pin({ id } = {}) {
      const i = await instance();
      if (!id) throw new Error('pin needs an id');
      await write(i, 'POST', `/v1/me/library/pins/${id}`);
      return methods.pins();
    },
    async unpin({ id } = {}) {
      const i = await instance();
      if (!id) throw new Error('unpin needs an id');
      await write(i, 'DELETE', `/v1/me/library/pins/${id}`);
      return methods.pins();
    },

    // ---- search helpers ----
    // Autocomplete while typing: suggested search terms plus a few top results.
    async searchSuggestions({ term, limit = 8 } = {}) {
      const i = await instance();
      if (!term) return { terms: [], top: [] };
      const r = await api(i, '/v1/catalog/{sf}/search/suggestions', {
        term, kinds: 'terms,topResults', types: 'songs,albums,artists,playlists,stations', limit,
      });
      const list = (r.results && r.results.suggestions) || [];
      return {
        terms: list.filter((x) => x.kind === 'terms').map((x) => x.displayTerm || x.searchTerm).filter(Boolean),
        top: items(list.filter((x) => x.kind === 'topResults').map((x) => x.content)),
      };
    },
    // Things played recently, shown on the empty search page as suggestions.
    async recentPlayed({ limit = 10 } = {}) {
      const i = await instance();
      if (!i.isAuthorized) return [];
      const r = await api(i, '/v1/me/recent/played', { limit: Math.min(limit, 10) });
      return items(r.data);
    },

    // Raw music API call through the page's MusicKit (path may contain {sf}).
    async api({ path, query = {} } = {}) {
      const i = await instance();
      if (!path || !String(path).startsWith('/v1/')) throw new Error('api needs a /v1/ path');
      const r = await i.api.music(String(path).replace('{sf}', i.storefrontId || 'us'), query);
      return r && r.data;
    },
    async pause() {
      const i = await instance();
      await i.pause();
      await settle(() => !i.isPlaying, 1500);
      return snapshot(i);
    },
    async resume() {
      const i = await instance();
      await i.play();
      await settle(() => i.isPlaying, 800);
      return snapshot(i);
    },
    async toggle() {
      const i = await instance();
      if (i.isPlaying) return methods.pause();
      return methods.resume();
    },
    async seek({ seconds } = {}) {
      const i = await instance();
      const s = Number(seconds);
      if (!Number.isFinite(s) || s < 0) throw new Error('seek needs seconds >= 0');
      await i.seekToTime(s);
      return snapshot(i);
    },
    async stop() { const i = await instance(); await i.stop(); return snapshot(i); },
    async next() { const i = await instance(); await i.skipToNextItem(); return snapshot(i); },
    async previous() { const i = await instance(); await i.skipToPreviousItem(); return snapshot(i); },
    async volume({ value } = {}) {
      const i = await instance();
      const v = Number(value);
      if (!(v >= 0 && v <= 1)) throw new Error('volume must be 0..1');
      i.volume = v;
      return snapshot(i);
    },
  };

  window.__am = {
    async call(method, params) {
      try {
        const f = Object.prototype.hasOwnProperty.call(methods, method) && methods[method];
        if (!f) throw new Error(`unknown method "${method}"`);
        return { ok: true, value: await f(params || {}) };
      } catch (e) {
        return { ok: false, error: String((e && e.message) || e) };
      }
    },
  };

  instance()
    .then((i) => { wire(i); emit('ready', snapshot(i)); })
    .catch((e) => emit('error', { message: e.message }));
})();
