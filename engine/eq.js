// Parametric equalizer + spectrum analyzer, evaluated in the page's main world before
// inject.js. The web player's <audio> element(s) are routed through Web Audio:
//
//   element ─► input ─┬─► analyserPre
//                     └─► chain (preamp ─► biquads… ─► fade) ─► output ─┬─► destination
//                                                                        └─► analyserPost
//
// Parameter moves glide (setTargetAtTime), so dragging a band never clicks. A change of
// band types / slopes / bypass builds a new chain and crossfades into it.
// The UI draws the curve itself from the same formulas (Web Audio biquads), so the
// picture and the sound always agree.
(() => {
  if (window.__amEq) return;

  const emit = (type, data) => { try { window.__amBridge && window.__amBridge.emit(type, data); } catch {} };

  const TYPES = new Set(['bell', 'lowshelf', 'highshelf', 'lowcut', 'highcut', 'notch', 'bandpass']);
  const MAX_BANDS = 24;
  const GLIDE = 0.012; // seconds, time constant of parameter moves
  const XFADE = 0.03; // seconds, crossfade between chains

  let ctx = null;
  let input = null;
  let output = null;
  let pre = null;
  let post = null;
  let chain = null; // { key, preamp, fade, nodes: [[BiquadFilterNode…] per band] }
  const sources = new WeakMap(); // HTMLMediaElement -> MediaElementAudioSourceNode
  let attachedCount = 0;
  const elements = new Set(); // attached elements, to read their volume
  let state = { enabled: false, preamp: 0, bands: [] };

  const clamp = (v, lo, hi, d) => (Number.isFinite(+v) ? Math.min(hi, Math.max(lo, +v)) : d);
  const dbToGain = (db) => Math.pow(10, db / 20);

  function sanitize(p) {
    const bands = Array.isArray(p.bands) ? p.bands.slice(0, MAX_BANDS) : [];
    return {
      enabled: !!p.enabled,
      preamp: clamp(p.preamp, -30, 30, 0),
      bands: bands.map((b) => ({
        on: b.on !== false,
        type: TYPES.has(b.type) ? b.type : 'bell',
        freq: clamp(b.freq, 10, 22000, 1000),
        gain: clamp(b.gain, -30, 30, 0),
        q: clamp(b.q, 0.025, 40, 1),
        slope: Math.round(clamp(b.slope, 1, 4, 2)), // cut stages, 12 dB/oct each
      })),
    };
  }

  // Butterworth Q of stage k (0-based) in a cascade of n second-order sections;
  // the user's Q scales the resonance of the last stage (0.7071 = flat).
  function cutStageQ(n, k, userQ) {
    const butter = 1 / (2 * Math.cos(((2 * k + 1) * Math.PI) / (4 * n)));
    const q = k === n - 1 ? butter * (userQ / Math.SQRT1_2) : butter;
    return 20 * Math.log10(Math.max(q, 0.01)); // lowpass/highpass take Q in dB
  }

  // Per band: [{type, frequency, Q, gain}] for each biquad stage.
  function stagesOf(b) {
    switch (b.type) {
      case 'bell': return [{ type: 'peaking', frequency: b.freq, Q: b.q, gain: b.gain }];
      case 'lowshelf': return [{ type: 'lowshelf', frequency: b.freq, Q: 1, gain: b.gain }];
      case 'highshelf': return [{ type: 'highshelf', frequency: b.freq, Q: 1, gain: b.gain }];
      case 'notch': return [{ type: 'notch', frequency: b.freq, Q: b.q, gain: 0 }];
      case 'bandpass': return [{ type: 'bandpass', frequency: b.freq, Q: b.q, gain: 0 }];
      case 'lowcut':
      case 'highcut': {
        const n = b.slope;
        const type = b.type === 'lowcut' ? 'highpass' : 'lowpass';
        return Array.from({ length: n }, (_, k) => ({ type, frequency: b.freq, Q: cutStageQ(n, k, b.q), gain: 0 }));
      }
    }
    return [];
  }

  const activeBands = () => (state.enabled ? state.bands.filter((b) => b.on) : []);
  const structureKey = () => (state.enabled ? 'on:' : 'off:') + activeBands().map((b) => b.type + (b.type.endsWith('cut') ? b.slope : '')).join(',');

  function ensureContext() {
    if (ctx) return ctx;
    ctx = new AudioContext({ latencyHint: 'interactive' });
    input = ctx.createGain();
    output = ctx.createGain();
    const analyser = () => {
      const a = ctx.createAnalyser();
      a.fftSize = 8192;
      a.smoothingTimeConstant = 0.5;
      a.minDecibels = -120;
      a.maxDecibels = 0;
      return a;
    };
    pre = analyser();
    post = analyser();
    input.connect(pre);
    output.connect(post);
    output.connect(ctx.destination);
    buildChain(true);
    return ctx;
  }

  function buildChain(immediate) {
    const key = structureKey();
    const now = ctx.currentTime;
    const fade = ctx.createGain();
    const preamp = ctx.createGain();
    preamp.gain.value = state.enabled ? dbToGain(state.preamp) : 1;
    let last = preamp;
    const nodes = activeBands().map((b) => stagesOf(b).map((s) => {
      const f = ctx.createBiquadFilter();
      f.type = s.type;
      f.frequency.value = s.frequency;
      f.Q.value = s.Q;
      f.gain.value = s.gain;
      last.connect(f);
      last = f;
      return f;
    }));
    last.connect(fade);
    fade.connect(output);
    input.connect(preamp);

    const old = chain;
    chain = { key, preamp, fade, nodes };
    if (immediate || !old) {
      fade.gain.value = 1;
    } else {
      fade.gain.setValueAtTime(0, now);
      fade.gain.linearRampToValueAtTime(1, now + XFADE);
      old.fade.gain.cancelScheduledValues(now);
      old.fade.gain.setValueAtTime(old.fade.gain.value, now);
      old.fade.gain.linearRampToValueAtTime(0, now + XFADE);
    }
    if (old) {
      setTimeout(() => {
        try { input.disconnect(old.preamp); } catch {}
        try { old.fade.disconnect(); } catch {}
      }, (XFADE * 1000) + 80);
    }
  }

  // Same structure: glide the parameters of the live chain.
  function updateParams() {
    const t = ctx.currentTime;
    const glide = (param, v) => {
      param.cancelScheduledValues(t);
      param.setTargetAtTime(v, t, GLIDE);
    };
    glide(chain.preamp.gain, state.enabled ? dbToGain(state.preamp) : 1);
    activeBands().forEach((b, i) => {
      stagesOf(b).forEach((s, k) => {
        const f = chain.nodes[i] && chain.nodes[i][k];
        if (!f) return;
        glide(f.frequency, s.frequency);
        glide(f.Q, s.Q);
        glide(f.gain, s.gain);
      });
    });
  }

  function apply() {
    if (!ctx) return;
    if (!chain || chain.key !== structureKey()) buildChain(false);
    else updateParams();
  }

  // Route one media element through the graph (once per element, for its lifetime).
  function attach(el) {
    if (!el || sources.has(el) || !(el instanceof HTMLMediaElement) || el instanceof HTMLVideoElement) return;
    ensureContext();
    try {
      const src = ctx.createMediaElementSource(el);
      src.connect(input);
      sources.set(el, src);
      elements.add(el);
      attachedCount++;
    } catch (e) {
      emit('error', { message: 'eq attach: ' + e.message });
    }
    if (ctx.state !== 'running') ctx.resume().catch(() => {});
  }

  let wanted = false; // EQ on or analyzer open: audio goes through Web Audio from now on
  function want() {
    if (wanted) return;
    wanted = true;
    document.querySelectorAll('audio').forEach(attach);
  }

  // The web player can create its <audio> elements at any time (and not insert them into
  // the document); catch every one when it starts playing.
  const origPlay = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function (...a) {
    if (wanted) attach(this);
    if (ctx && ctx.state !== 'running') ctx.resume().catch(() => {});
    return origPlay.apply(this, a);
  };

  // ---- spectrum ----
  const SPEC_POINTS = 200;
  const F_LO = 20;
  const F_HI = 20000;
  let specTimer = null;
  let preBuf = null;
  let postBuf = null;
  let silentFrames = 0;

  // dB spectrum (8192-point FFT) -> SPEC_POINTS log-spaced values, mean power per band;
  // below ~200 Hz there are fewer bins than points, so those interpolate between bins.
  function reduce(buf, sr, fft, offset) {
    const out = new Array(SPEC_POINTS);
    const binHz = sr / fft;
    const ratio = Math.log(F_HI / F_LO);
    for (let i = 0; i < SPEC_POINTS; i++) {
      const f0 = F_LO * Math.exp((ratio * (i - 0.5)) / (SPEC_POINTS - 1));
      const f1 = F_LO * Math.exp((ratio * (i + 0.5)) / (SPEC_POINTS - 1));
      const b0 = f0 / binHz;
      const b1 = f1 / binHz;
      let v;
      if (Math.floor(b1) - Math.ceil(b0) < 1) {
        const c = (b0 + b1) / 2;
        const lo = Math.max(0, Math.floor(c));
        const hi = Math.min(buf.length - 1, lo + 1);
        v = buf[lo] + (buf[hi] - buf[lo]) * (c - lo);
      } else {
        let sum = 0;
        let n = 0;
        for (let k = Math.ceil(b0); k <= Math.min(buf.length - 1, Math.floor(b1)); k++, n++) sum += Math.pow(10, buf[k] / 10);
        v = n ? 10 * Math.log10(sum / n + 1e-30) : -Infinity;
      }
      out[i] = Number.isFinite(v) ? Math.max(-120, Math.round((v + offset) * 10) / 10) : -120;
    }
    return out;
  }

  function anyPlaying() {
    try { const i = window.MusicKit && MusicKit.getInstance(); return !!(i && i.isPlaying); } catch { return false; }
  }

  function specTick() {
    if (!ctx || !attachedCount) return;
    const playing = anyPlaying();
    // Keep sending a few frames after a pause so the UI sees the level fall.
    silentFrames = playing ? 0 : silentFrames + 1;
    if (silentFrames > 4) return;
    if (!preBuf) {
      preBuf = new Float32Array(pre.frequencyBinCount);
      postBuf = new Float32Array(post.frequencyBinCount);
    }
    pre.getFloatFrequencyData(preBuf);
    post.getFloatFrequencyData(postBuf);
    // The element's volume is applied before Web Audio; undo it so the analyzer shows the
    // track's own level whatever the volume slider says.
    let offset = 0;
    for (const el of elements) {
      if (!el.paused && !el.muted && el.volume > 0.001) { offset = -20 * Math.log10(el.volume); break; }
    }
    emit('spectrum', {
      sr: ctx.sampleRate,
      pre: reduce(preBuf, ctx.sampleRate, pre.fftSize, offset),
      post: reduce(postBuf, ctx.sampleRate, post.fftSize, offset),
      playing,
    });
  }

  function rms(a) {
    const buf = new Float32Array(a.fftSize);
    a.getFloatTimeDomainData(buf);
    let s = 0;
    for (const v of buf) s += v * v;
    return Math.sqrt(s / buf.length);
  }

  window.__amEq = {
    MAX_BANDS,
    // Full state: {enabled, preamp, bands: [{on, type, freq, gain, q, slope}]}
    set(p = {}) {
      state = sanitize(p);
      if (state.enabled) want();
      apply();
      return window.__amEq.status();
    },
    // Analyzer frames as `spectrum` events (~45/s) while on.
    spectrum({ on } = {}) {
      clearInterval(specTimer);
      specTimer = null;
      if (on) {
        want();
        specTimer = setInterval(specTick, 22);
      }
      return { on: !!specTimer };
    },
    // Magnitude response (dB) of the live chain at the given frequencies, from Web Audio
    // itself: checks that the UI's curve math matches what plays.
    response({ freqs = [] } = {}) {
      ensureContext();
      const f = Float32Array.from(freqs);
      const total = new Float32Array(f.length);
      const mag = new Float32Array(f.length);
      const phase = new Float32Array(f.length);
      for (const band of chain.nodes) {
        for (const node of band) {
          node.getFrequencyResponse(f, mag, phase);
          for (let i = 0; i < f.length; i++) total[i] += 20 * Math.log10(Math.max(mag[i], 1e-12));
        }
      }
      return Array.from(total, (v) => Math.round(v * 100) / 100);
    },
    status() {
      return {
        enabled: state.enabled,
        bands: state.bands.length,
        attached: attachedCount,
        context: ctx ? ctx.state : null,
        sampleRate: ctx ? ctx.sampleRate : null,
        // Signal level before / after the EQ (linear RMS), for diagnostics.
        rmsPre: ctx ? rms(pre) : null,
        rmsPost: ctx ? rms(post) : null,
      };
    },
  };
})();
