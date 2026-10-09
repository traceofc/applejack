// Parametric equalizer (Preferences → Equalizer), drawn after FabFilter Pro-Q: bands are
// handles on a response curve over a live spectrum of the playing track.
//
// The DSP runs in the engine (engine/eq.js: Web Audio biquads between the web player's
// <audio> element and the output). This side keeps the settings (~/.config/applejack/eq.toml),
// sends them to the engine (coalesced to one message per frame while dragging) and draws
// the curve from the same biquad formulas the engine uses, so picture and sound agree.
// Spectrum frames arrive as `spectrum` events (~45/s) and are eased per display frame.
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    f64::consts::PI,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use adw::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango, subclass::prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::Engine;

// ---- model ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Bell,
    Lowshelf,
    Highshelf,
    Lowcut,
    Highcut,
    Notch,
    Bandpass,
}

const KINDS: [(Kind, &str); 7] = [
    (Kind::Bell, "Bell"),
    (Kind::Lowshelf, "Low Shelf"),
    (Kind::Highshelf, "High Shelf"),
    (Kind::Lowcut, "Low Cut"),
    (Kind::Highcut, "High Cut"),
    (Kind::Notch, "Notch"),
    (Kind::Bandpass, "Band Pass"),
];
const SLOPES: [&str; 4] = ["12 dB/oct", "24 dB/oct", "36 dB/oct", "48 dB/oct"];

impl Kind {
    fn index(self) -> u32 {
        KINDS.iter().position(|(k, _)| *k == self).unwrap_or(0) as u32
    }
    fn label(self) -> &'static str {
        KINDS[self.index() as usize].1
    }
    fn has_gain(self) -> bool {
        matches!(self, Kind::Bell | Kind::Lowshelf | Kind::Highshelf)
    }
    /// Web Audio shelves have a fixed slope, so Q does nothing there.
    fn has_q(self) -> bool {
        !matches!(self, Kind::Lowshelf | Kind::Highshelf)
    }
    fn is_cut(self) -> bool {
        matches!(self, Kind::Lowcut | Kind::Highcut)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Band {
    pub on: bool,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    /// Cut slope in 12 dB/oct steps (1..=4).
    pub slope: u8,
}

impl Default for Band {
    fn default() -> Self {
        Self { on: true, kind: Kind::Bell, freq: 1000.0, gain: 0.0, q: 1.0, slope: 2 }
    }
}

fn band(kind: Kind, freq: f64, gain: f64, q: f64) -> Band {
    Band { kind, freq, gain, q, ..Band::default() }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub preamp: f64,
    pub bands: Vec<Band>,
}

const MAX_BANDS: usize = 24;
const F_MIN: f64 = 20.0;
const F_MAX: f64 = 20000.0;
const GAIN_MAX: f64 = 24.0;
const Q_MIN: f64 = 0.1;
const Q_MAX: f64 = 30.0;
const RANGES: [f64; 5] = [6.0, 12.0, 18.0, 24.0, 30.0];
/// Played by "Test track" until the user picks another one.
const DEFAULT_TEST: (&str, &str) = ("636968288", "Daft Punk — Get Lucky (Radio Edit)");

fn builtin_presets() -> Vec<(&'static str, Preset)> {
    use Kind::*;
    let p = |preamp: f64, bands: Vec<Band>| Preset { preamp, bands };
    let cut = |kind: Kind, freq: f64, slope: u8| Band { kind, freq, q: 0.71, slope, ..Band::default() };
    vec![
        ("Flat", p(0.0, vec![])),
        ("Bass Boost", p(-5.0, vec![band(Lowshelf, 110.0, 6.0, 1.0), band(Bell, 60.0, 1.5, 1.2)])),
        ("Bass Reducer", p(0.0, vec![band(Lowshelf, 140.0, -6.0, 1.0)])),
        ("Treble Boost", p(-4.0, vec![band(Highshelf, 6000.0, 5.0, 1.0)])),
        ("Treble Reducer", p(0.0, vec![band(Highshelf, 5000.0, -5.0, 1.0)])),
        ("Loudness", p(-5.0, vec![band(Lowshelf, 90.0, 5.5, 1.0), band(Bell, 1000.0, -1.5, 0.7), band(Highshelf, 9000.0, 4.0, 1.0)])),
        ("Vocal", p(-3.0, vec![cut(Lowcut, 90.0, 2), band(Bell, 300.0, -2.0, 1.0), band(Bell, 3000.0, 3.0, 1.0), band(Highshelf, 10000.0, 1.5, 1.0)])),
        ("Rock", p(-4.0, vec![band(Bell, 80.0, 4.0, 0.9), band(Bell, 400.0, -2.0, 1.0), band(Bell, 2500.0, 2.5, 1.0), band(Highshelf, 8000.0, 3.0, 1.0)])),
        ("Electronic", p(-5.0, vec![band(Lowshelf, 60.0, 5.0, 1.0), band(Bell, 250.0, -2.0, 1.0), band(Bell, 2000.0, 1.0, 1.0), band(Highshelf, 10000.0, 3.5, 1.0)])),
        ("Hip-Hop", p(-5.0, vec![band(Bell, 55.0, 5.0, 0.8), band(Bell, 180.0, 1.5, 1.0), band(Bell, 600.0, -1.5, 1.0), band(Highshelf, 7000.0, 2.0, 1.0)])),
        ("Acoustic", p(-2.0, vec![band(Bell, 120.0, 2.0, 1.0), band(Bell, 400.0, -1.5, 1.0), band(Bell, 3000.0, 2.0, 1.0), band(Highshelf, 12000.0, 2.0, 1.0)])),
        ("Classical", p(-3.0, vec![band(Lowshelf, 100.0, 2.5, 1.0), band(Bell, 1500.0, -1.0, 0.7), band(Highshelf, 9000.0, 3.0, 1.0)])),
        ("Podcast", p(-4.0, vec![cut(Lowcut, 100.0, 3), band(Bell, 250.0, -3.0, 1.0), band(Bell, 3500.0, 4.0, 1.2), cut(Highcut, 12000.0, 2)])),
        ("Late Night", p(0.0, vec![band(Lowshelf, 100.0, -4.0, 1.0), band(Bell, 2000.0, 2.0, 0.8), band(Highshelf, 8000.0, -3.0, 1.0)])),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    enabled: bool,
    preamp: f64,
    /// Preset the bands came from ("" = custom); `modified` once they were edited.
    preset: String,
    modified: bool,
    /// Curve scale, ± dB.
    range: f64,
    analyzer: bool,
    show_pre: bool,
    test_id: String,
    test_title: String,
    bands: Vec<Band>,
    /// The user's own presets.
    presets: BTreeMap<String, Preset>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp: 0.0,
            preset: "Flat".into(),
            modified: false,
            range: 12.0,
            analyzer: true,
            show_pre: true,
            test_id: DEFAULT_TEST.0.into(),
            test_title: DEFAULT_TEST.1.into(),
            bands: vec![],
            presets: BTreeMap::new(),
        }
    }
}

fn config_path() -> std::path::PathBuf {
    glib::user_config_dir().join("applejack").join("eq.toml")
}

fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| toml::from_str(&s).map_err(|e| eprintln!("eq.toml: {e}")).ok())
        .unwrap_or_default()
}

/// Atomic write (temp file + rename).
fn save_config(cfg: &Config) {
    let path = config_path();
    let Ok(text) = toml::to_string_pretty(cfg).map_err(|e| eprintln!("eq.toml: {e}")) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("toml.tmp");
    if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)) {
        eprintln!("{}: {e}", path.display());
    }
}

// ---- filter math (Web Audio BiquadFilterNode, same as the engine) ----

#[derive(Clone, Copy)]
enum Stage {
    Peaking { f: f64, q: f64, gain: f64 },
    LowShelf { f: f64, gain: f64 },
    HighShelf { f: f64, gain: f64 },
    /// Q in dB, as Web Audio's lowpass / highpass take it.
    LowPass { f: f64, q_db: f64 },
    HighPass { f: f64, q_db: f64 },
    Notch { f: f64, q: f64 },
    BandPass { f: f64, q: f64 },
}

/// Butterworth Q of stage k in a cascade of n sections; the band's Q scales the last one.
fn cut_stage_q_db(n: u8, k: u8, user_q: f64) -> f64 {
    let n = n as f64;
    let butter = 1.0 / (2.0 * ((2.0 * k as f64 + 1.0) * PI / (4.0 * n)).cos());
    let q = if k as f64 == n - 1.0 { butter * (user_q / std::f64::consts::FRAC_1_SQRT_2) } else { butter };
    20.0 * q.max(0.01).log10()
}

fn stages(b: &Band) -> Vec<Stage> {
    let f = b.freq;
    match b.kind {
        Kind::Bell => vec![Stage::Peaking { f, q: b.q, gain: b.gain }],
        Kind::Lowshelf => vec![Stage::LowShelf { f, gain: b.gain }],
        Kind::Highshelf => vec![Stage::HighShelf { f, gain: b.gain }],
        Kind::Notch => vec![Stage::Notch { f, q: b.q }],
        Kind::Bandpass => vec![Stage::BandPass { f, q: b.q }],
        Kind::Lowcut | Kind::Highcut => {
            let n = b.slope.clamp(1, 4);
            (0..n)
                .map(|k| {
                    let q_db = cut_stage_q_db(n, k, b.q);
                    if b.kind == Kind::Lowcut { Stage::HighPass { f, q_db } } else { Stage::LowPass { f, q_db } }
                })
                .collect()
        }
    }
}

/// [b0, b1, b2, a0, a1, a2] per the Web Audio spec.
fn coefs(s: Stage, sr: f64) -> [f64; 6] {
    let w0 = |f: f64| 2.0 * PI * f.clamp(1.0, sr * 0.4999) / sr;
    match s {
        Stage::Peaking { f, q, gain } => {
            let (sn, c) = w0(f).sin_cos();
            let a = 10f64.powf(gain / 40.0);
            let al = sn / (2.0 * q);
            [1.0 + al * a, -2.0 * c, 1.0 - al * a, 1.0 + al / a, -2.0 * c, 1.0 - al / a]
        }
        Stage::LowShelf { f, gain } | Stage::HighShelf { f, gain } => {
            let (sn, c) = w0(f).sin_cos();
            let a = 10f64.powf(gain / 40.0);
            let sa = 2.0 * (sn / 2.0 * 2f64.sqrt()) * a.sqrt();
            if matches!(s, Stage::LowShelf { .. }) {
                [
                    a * ((a + 1.0) - (a - 1.0) * c + sa),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                    a * ((a + 1.0) - (a - 1.0) * c - sa),
                    (a + 1.0) + (a - 1.0) * c + sa,
                    -2.0 * ((a - 1.0) + (a + 1.0) * c),
                    (a + 1.0) + (a - 1.0) * c - sa,
                ]
            } else {
                [
                    a * ((a + 1.0) + (a - 1.0) * c + sa),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                    a * ((a + 1.0) + (a - 1.0) * c - sa),
                    (a + 1.0) - (a - 1.0) * c + sa,
                    2.0 * ((a - 1.0) - (a + 1.0) * c),
                    (a + 1.0) - (a - 1.0) * c - sa,
                ]
            }
        }
        Stage::LowPass { f, q_db } | Stage::HighPass { f, q_db } => {
            let (sn, c) = w0(f).sin_cos();
            let al = sn / (2.0 * 10f64.powf(q_db / 20.0));
            if matches!(s, Stage::LowPass { .. }) {
                [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al]
            } else {
                [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al]
            }
        }
        Stage::Notch { f, q } => {
            let (sn, c) = w0(f).sin_cos();
            let al = sn / (2.0 * q);
            [1.0, -2.0 * c, 1.0, 1.0 + al, -2.0 * c, 1.0 - al]
        }
        Stage::BandPass { f, q } => {
            let (sn, c) = w0(f).sin_cos();
            let al = sn / (2.0 * q);
            [al, 0.0, -al, 1.0 + al, -2.0 * c, 1.0 - al]
        }
    }
}

fn mag_db(k: &[f64; 6], f: f64, sr: f64) -> f64 {
    let w = 2.0 * PI * f / sr;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let nr = k[0] + k[1] * c1 + k[2] * c2;
    let ni = -(k[1] * s1 + k[2] * s2);
    let dr = k[3] + k[4] * c1 + k[5] * c2;
    let di = -(k[4] * s1 + k[5] * s2);
    10.0 * ((nr * nr + ni * ni).max(1e-30) / (dr * dr + di * di).max(1e-30)).log10()
}

/// Response of one band (dB) at each frequency.
fn band_response(b: &Band, freqs: &[f64], sr: f64) -> Vec<f64> {
    let ks: Vec<_> = stages(b).into_iter().map(|s| coefs(s, sr)).collect();
    freqs.iter().map(|&f| ks.iter().map(|k| mag_db(k, f, sr)).sum()).collect()
}

// ---- formatting ----

fn fmt_freq(f: f64) -> String {
    if f >= 10000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else if f >= 1000.0 {
        format!("{:.2} kHz", f / 1000.0)
    } else if f >= 100.0 {
        format!("{f:.0} Hz")
    } else {
        format!("{f:.1} Hz")
    }
}
fn fmt_gain(g: f64) -> String {
    format!("{:+.1} dB", if g.abs() < 0.05 { 0.0 } else { g })
}
fn fmt_q(q: f64) -> String {
    if q >= 10.0 { format!("{q:.1}") } else { format!("{q:.2}") }
}

// ---- controller (one per app) ----

struct Saved {
    ids: Vec<String>,
    index: i64,
    seconds: f64,
    playing: bool,
    repeat: u8,
}

struct Ctl {
    engine: Rc<Engine>,
    cfg: RefCell<Config>,
    /// Bumped on every change; the curve cache keys on it.
    version: Cell<u64>,
    sample_rate: Cell<f64>,
    send_queued: Cell<bool>,
    save_queued: Cell<bool>,
    spectrum_on: Cell<bool>,
    view: RefCell<Weak<View>>,
    /// What was playing before the test track started.
    test: RefCell<Option<Saved>>,
}

thread_local! {
    static CTL: RefCell<Option<Rc<Ctl>>> = const { RefCell::new(None) };
}

fn ctl() -> Option<Rc<Ctl>> {
    CTL.with(|c| c.borrow().clone())
}

/// Called once the engine object exists; the settings go out when its page is ready.
pub fn init(engine: Rc<Engine>) {
    let c = Rc::new(Ctl {
        engine,
        cfg: RefCell::new(load_config()),
        version: Cell::new(1),
        sample_rate: Cell::new(48000.0),
        send_queued: Cell::new(false),
        save_queued: Cell::new(false),
        spectrum_on: Cell::new(false),
        view: RefCell::new(Weak::new()),
        test: RefCell::new(None),
    });
    CTL.with(|x| *x.borrow_mut() = Some(c));
}

/// The engine's page (re)loaded: it starts with the equalizer off and no analyzer.
pub fn on_ready() {
    let Some(c) = ctl() else { return };
    c.send_now();
    if c.spectrum_on.get() {
        drop(c.engine.call("eqSpectrum", json!({ "on": true })));
    }
}

pub fn on_spectrum(data: &Value) {
    let Some(c) = ctl() else { return };
    let Some(view) = c.view.borrow().upgrade() else { return };
    let list = |k: &str| -> Vec<f32> {
        data.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(-120.0) as f32).collect())
            .unwrap_or_default()
    };
    if let Some(sr) = data.get("sr").and_then(Value::as_f64) {
        c.sample_rate.set(sr);
    }
    view.spec.borrow_mut().frame(list("pre"), list("post"));
}

/// The preferences dialog closed: stop the analyzer and the test track.
pub fn dialog_closed() {
    let Some(c) = ctl() else { return };
    c.set_spectrum(false);
    if c.test.borrow().is_some() {
        c.set_test(false);
    }
}

impl Ctl {
    fn payload(&self) -> Value {
        let cfg = self.cfg.borrow();
        json!({ "enabled": cfg.enabled, "preamp": cfg.preamp, "bands": cfg.bands })
    }

    fn send_now(self: &Rc<Self>) {
        let fut = self.engine.call("eq", self.payload());
        let c = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(v) => {
                    if let (Some(c), Some(sr)) = (c.upgrade(), v.get("sampleRate").and_then(Value::as_f64)) {
                        c.sample_rate.set(sr);
                    }
                }
                Err(e) => eprintln!("eq: {e}"),
            }
        });
    }

    /// One message per frame at most while a handle is dragged.
    fn queue_send(self: &Rc<Self>) {
        if self.send_queued.replace(true) {
            return;
        }
        let c = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(15), move || {
            c.send_queued.set(false);
            c.send_now();
        });
    }

    fn queue_save(self: &Rc<Self>) {
        if self.save_queued.replace(true) {
            return;
        }
        let c = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            c.save_queued.set(false);
            save_config(&c.cfg.borrow());
        });
    }

    /// After any change of the settings.
    fn commit(self: &Rc<Self>) {
        self.version.set(self.version.get() + 1);
        self.queue_send();
        self.queue_save();
        if let Some(v) = self.view.borrow().upgrade() {
            v.sync();
        }
    }

    /// Edits the bands; the preset is "modified" from then on.
    fn edit(self: &Rc<Self>, f: impl FnOnce(&mut Config)) {
        {
            let mut cfg = self.cfg.borrow_mut();
            f(&mut cfg);
            cfg.modified = true;
        }
        self.commit();
    }

    fn set_spectrum(&self, on: bool) {
        if self.spectrum_on.replace(on) != on {
            drop(self.engine.call("eqSpectrum", json!({ "on": on })));
        }
    }

    fn load_preset(self: &Rc<Self>, name: &str) {
        let preset = builtin_presets()
            .into_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p)
            .or_else(|| self.cfg.borrow().presets.get(name).cloned());
        let Some(p) = preset else { return };
        {
            let mut cfg = self.cfg.borrow_mut();
            cfg.preamp = p.preamp;
            cfg.bands = p.bands;
            cfg.preset = name.to_owned();
            cfg.modified = false;
            // Picking a preset means wanting to hear it.
            cfg.enabled = true;
        }
        self.commit();
    }

    fn set_test(self: &Rc<Self>, on: bool) {
        let Some(app) = super::app() else { return };
        if on {
            if self.test.borrow().is_some() {
                return;
            }
            let saved = {
                let st = app.st.borrow();
                let seconds = if st.playing { st.pos + st.pos_at.elapsed().as_secs_f64() } else { st.pos };
                Saved {
                    ids: st.queue.items.iter().map(|it| it.song_play_id()).collect(),
                    index: st.queue.position,
                    seconds,
                    playing: st.playing || st.busy,
                    repeat: st.repeat,
                }
            };
            *self.test.borrow_mut() = Some(saved);
            let id = self.cfg.borrow().test_id.clone();
            app.call_status("playSongs", json!({ "ids": [id] }));
            app.call_quiet("repeat", json!({ "mode": 1 }));
        } else {
            let Some(s) = self.test.borrow_mut().take() else { return };
            let engine = self.engine.clone();
            glib::spawn_future_local(async move {
                let _ = engine.call("repeat", json!({ "mode": s.repeat })).await;
                let r = if s.ids.is_empty() || s.index < 0 || s.index as usize >= s.ids.len() {
                    engine.call("stop", json!({})).await
                } else {
                    let mut r = engine.call("playSongs", json!({ "ids": s.ids, "startWith": s.index })).await;
                    if r.is_ok() && s.seconds > 1.0 {
                        r = engine.call("seek", json!({ "seconds": s.seconds })).await;
                    }
                    if r.is_ok() && !s.playing {
                        r = engine.call("pause", json!({})).await;
                    }
                    r
                };
                match r {
                    Ok(_) => app.call_status("status", json!({})),
                    Err(e) => app.error(&e),
                }
            });
        }
        if let Some(v) = self.view.borrow().upgrade() {
            v.sync();
        }
    }
}

// ---- a widget drawn with GtkSnapshot (GPU paths), for the graph and the knobs ----

type DrawFn = Box<dyn Fn(&Canvas, &gtk::Snapshot, f32, f32)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub draw: RefCell<Option<DrawFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "ApplejackEqCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if let Some(f) = self.draw.borrow().as_ref() {
                f(&obj, snapshot, w, h);
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    fn new() -> Self {
        glib::Object::new()
    }
    fn set_draw(&self, f: impl Fn(&Canvas, &gtk::Snapshot, f32, f32) + 'static) {
        *self.imp().draw.borrow_mut() = Some(Box::new(f));
    }
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(r, g, b, a)
}

fn with_alpha(c: &gdk::RGBA, a: f32) -> gdk::RGBA {
    rgba(c.red(), c.green(), c.blue(), a)
}

/// Band colours, Pro-Q-like.
const COLORS: [(f32, f32, f32); 8] = [
    (1.00, 0.78, 0.30),
    (0.45, 0.86, 0.42),
    (0.33, 0.74, 1.00),
    (0.82, 0.50, 1.00),
    (1.00, 0.46, 0.46),
    (0.33, 0.90, 0.84),
    (1.00, 0.60, 0.28),
    (0.62, 0.64, 1.00),
];

fn band_color(i: usize) -> gdk::RGBA {
    let (r, g, b) = COLORS[i % COLORS.len()];
    rgba(r, g, b, 1.0)
}

fn layout(w: &impl IsA<gtk::Widget>, text: &str, font: &str) -> pango::Layout {
    let l = w.create_pango_layout(Some(text));
    l.set_font_description(Some(&pango::FontDescription::from_string(font)));
    l
}

fn draw_text(s: &gtk::Snapshot, l: &pango::Layout, x: f32, y: f32, color: &gdk::RGBA) {
    s.save();
    s.translate(&graphene::Point::new(x, y));
    s.append_layout(l, color);
    s.restore();
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> gsk::RoundedRect {
    gsk::RoundedRect::from_rect(graphene::Rect::new(x, y, w, h), r)
}

/// Smooth curve through the points (Catmull-Rom as cubic Béziers).
fn smooth_through(pb: &gsk::PathBuilder, pts: &[(f32, f32)]) {
    for i in 0..pts.len().saturating_sub(1) {
        let p0 = pts[i.saturating_sub(1)];
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        pb.cubic_to(
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
            p2.0,
            p2.1,
        );
    }
}

// ---- knob ----

struct Knob {
    root: gtk::Box,
    canvas: Canvas,
    value_label: gtk::Label,
    value: Cell<f64>,
    min: f64,
    max: f64,
    log: bool,
    bipolar: bool,
    default: f64,
    fmt: fn(f64) -> String,
    color: RefCell<gdk::RGBA>,
    drag_from: Cell<f64>,
    on_change: RefCell<Option<Box<dyn Fn(f64)>>>,
}

impl Knob {
    fn new(title: &str, min: f64, max: f64, log: bool, default: f64, fmt: fn(f64) -> String) -> Rc<Self> {
        let canvas = Canvas::new();
        canvas.set_size_request(46, 46);
        canvas.set_halign(gtk::Align::Center);
        canvas.set_cursor_from_name(Some("ns-resize"));
        let title_label = gtk::Label::new(Some(title));
        title_label.add_css_class("caption");
        title_label.add_css_class("dim-label");
        let value_label = gtk::Label::new(None);
        value_label.add_css_class("caption");
        value_label.add_css_class("numeric");
        value_label.set_width_chars(9);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.append(&title_label);
        root.append(&canvas);
        root.append(&value_label);
        let knob = Rc::new(Self {
            root,
            canvas,
            value_label,
            value: Cell::new(default),
            min,
            max,
            log,
            bipolar: !log && min < 0.0 && max > 0.0,
            default,
            fmt,
            color: RefCell::new(rgba(0.36, 0.66, 1.0, 1.0)),
            drag_from: Cell::new(0.0),
            on_change: RefCell::new(None),
        });
        knob.set_value(default);

        let weak = Rc::downgrade(&knob);
        knob.canvas.set_draw(move |w, s, cw, ch| {
            if let Some(k) = weak.upgrade() {
                k.draw(w, s, cw, ch);
            }
        });

        let drag = gtk::GestureDrag::new();
        let weak = Rc::downgrade(&knob);
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(k) = weak.upgrade() {
                k.drag_from.set(k.norm(k.value.get()));
            }
        });
        let weak = Rc::downgrade(&knob);
        drag.connect_drag_update(move |g, _dx, dy| {
            let Some(k) = weak.upgrade() else { return };
            let fine = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
            let n = k.drag_from.get() - dy / if fine { 900.0 } else { 180.0 };
            k.user_set(k.from_norm(n.clamp(0.0, 1.0)));
        });
        knob.canvas.add_controller(drag);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(&knob);
        scroll.connect_scroll(move |_, _, dy| {
            let Some(k) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !k.canvas.is_sensitive() {
                return glib::Propagation::Proceed;
            }
            let n = (k.norm(k.value.get()) - dy * 0.02).clamp(0.0, 1.0);
            k.user_set(k.from_norm(n));
            glib::Propagation::Stop
        });
        knob.canvas.add_controller(scroll);

        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&knob);
        click.connect_pressed(move |_, n, _, _| {
            if let (Some(k), 2) = (weak.upgrade(), n) {
                k.user_set(k.default);
            }
        });
        knob.canvas.add_controller(click);
        knob
    }

    fn norm(&self, v: f64) -> f64 {
        if self.log {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    fn from_norm(&self, n: f64) -> f64 {
        if self.log {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        }
    }

    /// Shows a value; does not notify.
    fn set_value(&self, v: f64) {
        let v = v.clamp(self.min, self.max);
        self.value.set(v);
        self.value_label.set_text(&(self.fmt)(v));
        self.canvas.queue_draw();
    }

    fn user_set(&self, v: f64) {
        self.set_value(v);
        if let Some(f) = self.on_change.borrow().as_ref() {
            f(self.value.get());
        }
    }

    fn connect_change(&self, f: impl Fn(f64) + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(f));
    }

    fn set_color(&self, c: gdk::RGBA) {
        *self.color.borrow_mut() = c;
        self.canvas.queue_draw();
    }

    fn set_sensitive(&self, on: bool) {
        self.root.set_sensitive(on);
    }

    fn draw(&self, w: &Canvas, s: &gtk::Snapshot, cw: f32, ch: f32) {
        let fg = w.color();
        let on = w.is_sensitive();
        let (cx, cy) = (cw / 2.0, ch / 2.0);
        let r = cw.min(ch) / 2.0 - 3.0;
        let a0 = 0.75 * std::f32::consts::PI; // 135°, sweep 270°
        let sweep = 1.5 * std::f32::consts::PI;
        let arc = |from: f32, to: f32| {
            let pb = gsk::PathBuilder::new();
            let steps = (((to - from).abs() / sweep) * 48.0).ceil().max(1.0) as usize;
            for i in 0..=steps {
                let a = from + (to - from) * i as f32 / steps as f32;
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
            }
            pb.to_path()
        };
        let stroke = gsk::Stroke::new(3.5);
        stroke.set_line_cap(gsk::LineCap::Round);
        s.append_stroke(&arc(a0, a0 + sweep), &stroke, &with_alpha(&fg, 0.14));
        let n = self.norm(self.value.get()) as f32;
        let from = if self.bipolar { a0 + sweep / 2.0 } else { a0 };
        let to = a0 + sweep * n;
        let color = if on { self.color.borrow().clone() } else { with_alpha(&fg, 0.3) };
        if (to - from).abs() > 0.001 {
            s.append_stroke(&arc(from.min(to), from.max(to)), &stroke, &color);
        }
        // Cap with a pointer.
        let pb = gsk::PathBuilder::new();
        pb.add_circle(&graphene::Point::new(cx, cy), r - 7.0);
        s.append_fill(&pb.to_path(), gsk::FillRule::Winding, &with_alpha(&fg, 0.08));
        let pb = gsk::PathBuilder::new();
        pb.move_to(cx + (r - 15.0) * to.cos(), cy + (r - 15.0) * to.sin());
        pb.line_to(cx + (r - 8.0) * to.cos(), cy + (r - 8.0) * to.sin());
        let st = gsk::Stroke::new(2.5);
        st.set_line_cap(gsk::LineCap::Round);
        s.append_stroke(&pb.to_path(), &st, &with_alpha(&fg, if on { 0.85 } else { 0.35 }));
    }
}

// ---- spectrum easing ----

const FLOOR: f32 = -120.0;

thread_local! {
    static FPS_LOG: bool = std::env::var_os("AM_EQ_FPS").is_some();
}

#[derive(Default)]
struct Spectrum {
    pre: Vec<f32>,
    post: Vec<f32>,
    shown_pre: Vec<f32>,
    shown_post: Vec<f32>,
    last: Option<Instant>,
}

impl Spectrum {
    fn frame(&mut self, pre: Vec<f32>, post: Vec<f32>) {
        // Smoothing across neighbours (two [1 2 1] passes), like an analyzer's band averaging.
        let blur = |v: Vec<f32>| -> Vec<f32> {
            (0..v.len())
                .map(|i| {
                    let a = v[i.saturating_sub(1)];
                    let c = v[(i + 1).min(v.len() - 1)];
                    0.25 * a + 0.5 * v[i] + 0.25 * c
                })
                .collect()
        };
        self.pre = blur(blur(pre));
        self.post = blur(blur(post));
        self.last = Some(Instant::now());
    }

    /// Eases what is shown toward the last frame: quick attack, slow release. Returns
    /// whether anything moved.
    fn step(&mut self, dt: f32) -> bool {
        let stale = self.last.is_none_or(|t| t.elapsed() > Duration::from_millis(250));
        let rise = 1.0 - (-dt / 0.03).exp();
        let fall = 1.0 - (-dt / 0.28).exp();
        let mut moved = false;
        for (target, shown) in [(&self.pre, &mut self.shown_pre), (&self.post, &mut self.shown_post)] {
            if shown.len() != target.len() && !target.is_empty() {
                *shown = vec![FLOOR; target.len()];
            }
            for (i, v) in shown.iter_mut().enumerate() {
                let t = if stale { FLOOR } else { target.get(i).copied().unwrap_or(FLOOR) };
                let d = t - *v;
                if d.abs() < 0.02 {
                    continue;
                }
                *v += d * if d > 0.0 { rise } else { fall };
                moved = true;
            }
        }
        moved
    }
}

// ---- the editor ----

struct Drag {
    band: usize,
    x0: f64,
    y0: f64,
    start: Band,
}

#[derive(Default)]
struct CurveCache {
    key: (u64, i32, i32, u64),
    xs: Vec<f32>,
    total: Vec<f64>,
    bands: Vec<Vec<f64>>,
}

pub struct View {
    ctl: Rc<Ctl>,
    canvas: Canvas,
    sel: Cell<Option<usize>>,
    hover: Cell<Option<usize>>,
    pointer: Cell<Option<(f64, f64)>>,
    drag: RefCell<Option<Drag>>,
    spec: RefCell<Spectrum>,
    curve: RefCell<CurveCache>,
    last_tick: Cell<i64>,
    // AM_EQ_FPS=1 prints frame counts (dev aid)
    frames: Cell<u32>,
    draws: Cell<u32>,
    fps_at: Cell<i64>,
    popover: gtk::Popover,
    syncing: Cell<bool>,
    // band panel
    band_label: gtk::Label,
    power: gtk::ToggleButton,
    kind: gtk::DropDown,
    slope: gtk::DropDown,
    freq: Rc<Knob>,
    gain: Rc<Knob>,
    q: Rc<Knob>,
    remove: gtk::Button,
    preamp: Rc<Knob>,
    // rows
    enabled: gtk::Switch,
    preset_row: adw::ComboRow,
    preset_names: RefCell<Vec<String>>,
    delete_preset: gtk::Button,
    test_row: adw::SwitchRow,
    range_row: adw::ComboRow,
    analyzer_row: adw::SwitchRow,
    pre_row: adw::SwitchRow,
}

/// Spectrum scale: dB (after the 4.5 dB/oct tilt) at the top and bottom of the graph.
const SPEC_TOP: f32 = -18.0;
const SPEC_BOTTOM: f32 = -102.0;
const SPEC_TILT: f32 = 4.5;

impl View {
    fn range(&self) -> f64 {
        self.ctl.cfg.borrow().range
    }
    fn x_of(f: f64, w: f64) -> f64 {
        w * (f / F_MIN).ln() / (F_MAX / F_MIN).ln()
    }
    fn f_of(x: f64, w: f64) -> f64 {
        F_MIN * (F_MAX / F_MIN).powf(x / w)
    }
    fn y_of(&self, db: f64, h: f64) -> f64 {
        h / 2.0 - db / self.range() * (h / 2.0 - 18.0)
    }
    fn db_of(&self, y: f64, h: f64) -> f64 {
        (h / 2.0 - y) / (h / 2.0 - 18.0) * self.range()
    }

    fn node_pos(&self, b: &Band, w: f64, h: f64) -> (f64, f64) {
        let y = if b.kind.has_gain() { b.gain.clamp(-self.range(), self.range()) } else { 0.0 };
        (Self::x_of(b.freq, w), self.y_of(y, h))
    }

    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        let (w, h) = (self.canvas.width() as f64, self.canvas.height() as f64);
        let cfg = self.ctl.cfg.borrow();
        // Topmost (last drawn) first; the selected band wins ties.
        let mut best: Option<(usize, f64)> = None;
        for (i, b) in cfg.bands.iter().enumerate() {
            let (nx, ny) = self.node_pos(b, w, h);
            let d = ((nx - x).powi(2) + (ny - y).powi(2)).sqrt();
            let d = if Some(i) == self.sel.get() { d - 3.0 } else { d };
            if d <= 12.0 && best.is_none_or(|(_, bd)| d <= bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    fn select(&self, i: Option<usize>) {
        self.sel.set(i);
        self.sync();
    }

    fn add_band(&self, kind: Kind, freq: f64, gain: f64) {
        if self.ctl.cfg.borrow().bands.len() >= MAX_BANDS {
            return;
        }
        let mut b = Band { kind, freq: freq.clamp(F_MIN, F_MAX), q: if kind.is_cut() { 0.71 } else { 1.0 }, ..Band::default() };
        if kind.has_gain() {
            b.gain = (gain * 2.0).round() / 2.0;
        }
        let mut idx = 0;
        self.ctl.edit(|cfg| {
            cfg.enabled = true;
            cfg.bands.push(b);
            idx = cfg.bands.len() - 1;
        });
        self.select(Some(idx));
    }

    fn remove_band(&self, i: usize) {
        if i >= self.ctl.cfg.borrow().bands.len() {
            return;
        }
        self.sel.set(None);
        self.hover.set(None);
        self.ctl.edit(|cfg| {
            cfg.bands.remove(i);
        });
    }

    fn edit_band(&self, i: usize, f: impl FnOnce(&mut Band)) {
        self.ctl.edit(|cfg| {
            if let Some(b) = cfg.bands.get_mut(i) {
                f(b);
            }
        });
    }

    /// Brings every control in line with the settings.
    fn sync(&self) {
        self.syncing.set(true);
        let cfg = self.ctl.cfg.borrow().clone();
        self.enabled.set_active(cfg.enabled);
        self.preamp.set_value(cfg.preamp);

        let sel = self.sel.get().filter(|&i| i < cfg.bands.len());
        if sel != self.sel.get() {
            self.sel.set(sel);
        }
        let on = sel.is_some();
        for w in [self.power.upcast_ref::<gtk::Widget>(), self.kind.upcast_ref(), self.slope.upcast_ref(), self.remove.upcast_ref()] {
            w.set_sensitive(on);
        }
        if let Some(i) = sel {
            let b = &cfg.bands[i];
            let c = band_color(i);
            self.band_label.set_markup(&format!(
                "<span foreground=\"#{:02x}{:02x}{:02x}\">●</span>  Band {}",
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8,
                i + 1
            ));
            self.power.set_active(b.on);
            self.kind.set_selected(b.kind.index());
            self.slope.set_selected(b.slope.clamp(1, 4) as u32 - 1);
            self.slope.set_visible(b.kind.is_cut());
            self.freq.set_value(b.freq);
            self.gain.set_value(b.gain);
            self.q.set_value(b.q);
            for k in [&self.freq, &self.gain, &self.q] {
                k.set_color(c);
            }
            self.freq.set_sensitive(true);
            self.gain.set_sensitive(b.kind.has_gain());
            self.q.set_sensitive(b.kind.has_q());
        } else {
            self.band_label.set_text(if cfg.bands.is_empty() { "No bands" } else { "No band selected" });
            self.slope.set_visible(false);
            for k in [&self.freq, &self.gain, &self.q] {
                k.set_sensitive(false);
            }
        }

        // Presets
        let names = self.preset_names.borrow().clone();
        let idx = names.iter().position(|n| *n == cfg.preset).unwrap_or(names.len() - 1);
        self.preset_row.set_selected(idx as u32);
        self.preset_row.set_subtitle(if cfg.modified && !cfg.preset.is_empty() { "Modified" } else { "" });
        self.delete_preset.set_sensitive(cfg.presets.contains_key(&cfg.preset));

        self.test_row.set_active(self.ctl.test.borrow().is_some());
        self.test_row.set_subtitle(&glib::markup_escape_text(&cfg.test_title));
        self.range_row.set_selected(RANGES.iter().position(|r| *r == cfg.range).unwrap_or(1) as u32);
        self.analyzer_row.set_active(cfg.analyzer);
        self.pre_row.set_active(cfg.show_pre);
        self.pre_row.set_sensitive(cfg.analyzer);
        self.syncing.set(false);
        self.canvas.queue_draw();
    }

    fn rebuild_presets(&self) {
        let mut names: Vec<String> = builtin_presets().into_iter().map(|(n, _)| n.to_owned()).collect();
        names.extend(self.ctl.cfg.borrow().presets.keys().cloned());
        names.push("Custom".into());
        self.syncing.set(true);
        self.preset_row.set_model(Some(&gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>())));
        *self.preset_names.borrow_mut() = names;
        self.syncing.set(false);
        self.sync();
    }

    fn curve(&self, w: i32, h: i32) -> std::cell::Ref<'_, CurveCache> {
        let key = (self.ctl.version.get(), w, h, self.ctl.sample_rate.get() as u64);
        if self.curve.borrow().key != key {
            let sr = self.ctl.sample_rate.get();
            let cfg = self.ctl.cfg.borrow();
            let n = (w / 2).max(2) as usize + 1;
            let xs: Vec<f32> = (0..n).map(|i| (i as f64 * w as f64 / (n - 1) as f64) as f32).collect();
            let freqs: Vec<f64> = xs.iter().map(|&x| Self::f_of(x as f64, w as f64)).collect();
            let bands: Vec<Vec<f64>> = cfg.bands.iter().map(|b| band_response(b, &freqs, sr)).collect();
            let mut total = vec![0.0; n];
            for (b, r) in cfg.bands.iter().zip(&bands) {
                if b.on {
                    for (t, v) in total.iter_mut().zip(r) {
                        *t += v;
                    }
                }
            }
            *self.curve.borrow_mut() = CurveCache { key, xs, total, bands };
        }
        self.curve.borrow()
    }

    fn spec_y(v: f32, f: f32, h: f32) -> f32 {
        let v = v + SPEC_TILT * (f / 1000.0).log2();
        let t = (SPEC_TOP - v) / (SPEC_TOP - SPEC_BOTTOM);
        (t * h).clamp(-2.0, h + 2.0)
    }

    fn spectrum_path(values: &[f32], w: f32, h: f32) -> Option<gsk::Path> {
        if values.len() < 2 || values.iter().all(|v| *v <= FLOOR + 0.5) {
            return None;
        }
        let n = values.len();
        let pts: Vec<(f32, f32)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let t = i as f32 / (n - 1) as f32;
                let f = (F_MIN as f32) * ((F_MAX / F_MIN) as f32).powf(t);
                (t * w, Self::spec_y(*v, f, h))
            })
            .collect();
        let pb = gsk::PathBuilder::new();
        pb.move_to(0.0, h + 2.0);
        pb.line_to(pts[0].0, pts[0].1);
        smooth_through(&pb, &pts);
        pb.line_to(w, h + 2.0);
        pb.close();
        Some(pb.to_path())
    }

    fn draw(&self, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
        self.draws.set(self.draws.get() + 1);
        let (wf, hf) = (w as f64, h as f64);
        let cfg = self.ctl.cfg.borrow().clone();
        let bounds = graphene::Rect::new(0.0, 0.0, w, h);
        s.push_rounded_clip(&round_rect(0.0, 0.0, w, h, 10.0));
        s.append_linear_gradient(
            &bounds,
            &graphene::Point::new(0.0, 0.0),
            &graphene::Point::new(0.0, h),
            &[gsk::ColorStop::new(0.0, rgba(0.115, 0.125, 0.15, 1.0)), gsk::ColorStop::new(1.0, rgba(0.075, 0.08, 0.095, 1.0))],
        );

        // Grid
        let grid = |pb: &gsk::PathBuilder, x0: f32, y0: f32, x1: f32, y1: f32| {
            pb.move_to(x0, y0);
            pb.line_to(x1, y1);
        };
        let minor = gsk::PathBuilder::new();
        let major = gsk::PathBuilder::new();
        let mut decade = 10.0;
        while decade < F_MAX {
            for m in 1..10 {
                let f = decade * m as f64;
                if !(F_MIN..=F_MAX).contains(&f) {
                    continue;
                }
                let x = Self::x_of(f, wf) as f32;
                grid(if m == 1 || m == 2 || m == 5 { &major } else { &minor }, x, 0.0, x, h);
            }
            decade *= 10.0;
        }
        let range = cfg.range;
        let step = if range <= 6.0 { 2.0 } else if range <= 12.0 { 3.0 } else if range <= 18.0 { 6.0 } else { 6.0 };
        let mut db = -range;
        while db <= range + 0.01 {
            let y = self.y_of(db, hf) as f32;
            grid(&minor, 0.0, y, w, y);
            db += step;
        }
        let one = gsk::Stroke::new(1.0);
        s.append_stroke(&minor.to_path(), &one, &rgba(1.0, 1.0, 1.0, 0.035));
        s.append_stroke(&major.to_path(), &one, &rgba(1.0, 1.0, 1.0, 0.07));
        let zero = gsk::PathBuilder::new();
        let y0 = self.y_of(0.0, hf) as f32;
        grid(&zero, 0.0, y0, w, y0);
        s.append_stroke(&zero.to_path(), &one, &rgba(1.0, 1.0, 1.0, 0.14));

        // Spectrum
        if cfg.analyzer {
            let spec = self.spec.borrow();
            if cfg.show_pre {
                if let Some(p) = Self::spectrum_path(&spec.shown_pre, w, h) {
                    s.append_fill(&p, gsk::FillRule::Winding, &rgba(0.42, 0.46, 0.56, 0.16));
                }
            }
            if let Some(p) = Self::spectrum_path(&spec.shown_post, w, h) {
                s.push_fill(&p, gsk::FillRule::Winding);
                s.append_linear_gradient(
                    &bounds,
                    &graphene::Point::new(0.0, 0.0),
                    &graphene::Point::new(0.0, h),
                    &[gsk::ColorStop::new(0.0, rgba(0.52, 0.70, 1.0, 0.42)), gsk::ColorStop::new(1.0, rgba(0.30, 0.42, 0.75, 0.10))],
                );
                s.pop();
                s.append_stroke(&p, &gsk::Stroke::new(1.0), &rgba(0.70, 0.82, 1.0, 0.35));
            }
        }

        // Curves
        let curve = self.curve(w as i32, h as i32);
        let dim = if cfg.enabled { 1.0 } else { 0.4 };
        let to_y = |db: f64| self.y_of(db.clamp(-range * 1.6, range * 1.6), hf) as f32;
        for (i, (b, r)) in cfg.bands.iter().zip(&curve.bands).enumerate() {
            if !b.on {
                continue;
            }
            let pb = gsk::PathBuilder::new();
            pb.move_to(curve.xs[0], y0);
            for (x, v) in curve.xs.iter().zip(r) {
                pb.line_to(*x, to_y(*v));
            }
            pb.line_to(*curve.xs.last().unwrap(), y0);
            pb.close();
            let hot = Some(i) == self.sel.get() || Some(i) == self.hover.get();
            s.append_fill(&pb.to_path(), gsk::FillRule::Winding, &with_alpha(&band_color(i), (if hot { 0.20 } else { 0.07 }) * dim));
            if hot {
                let pb = gsk::PathBuilder::new();
                for (k, (x, v)) in curve.xs.iter().zip(r).enumerate() {
                    if k == 0 { pb.move_to(*x, to_y(*v)) } else { pb.line_to(*x, to_y(*v)) }
                }
                s.append_stroke(&pb.to_path(), &gsk::Stroke::new(1.2), &with_alpha(&band_color(i), 0.7 * dim));
            }
        }
        let pb = gsk::PathBuilder::new();
        for (k, (x, v)) in curve.xs.iter().zip(&curve.total).enumerate() {
            if k == 0 { pb.move_to(*x, to_y(*v)) } else { pb.line_to(*x, to_y(*v)) }
        }
        let total = pb.to_path();
        let glow = gsk::Stroke::new(7.0);
        glow.set_line_join(gsk::LineJoin::Round);
        s.append_stroke(&total, &glow, &rgba(1.0, 0.86, 0.45, 0.10 * dim));
        let line = gsk::Stroke::new(2.4);
        line.set_line_join(gsk::LineJoin::Round);
        s.append_stroke(&total, &line, &rgba(1.0, 0.90, 0.62, 0.95 * dim));
        drop(curve);

        // Handles
        for (i, b) in cfg.bands.iter().enumerate() {
            let (nx, ny) = self.node_pos(b, wf, hf);
            let (nx, ny) = (nx as f32, ny as f32);
            let col = band_color(i);
            let selected = Some(i) == self.sel.get();
            let hovered = Some(i) == self.hover.get();
            let r = if selected { 8.5 } else if hovered { 8.0 } else { 7.0 };
            if selected {
                let ring = gsk::PathBuilder::new();
                ring.add_circle(&graphene::Point::new(nx, ny), r + 4.0);
                s.append_stroke(&ring.to_path(), &gsk::Stroke::new(1.5), &rgba(1.0, 1.0, 1.0, 0.75));
            }
            let dot = gsk::PathBuilder::new();
            dot.add_circle(&graphene::Point::new(nx, ny), r);
            let path = dot.to_path();
            if b.on {
                s.append_fill(&path, gsk::FillRule::Winding, &with_alpha(&col, dim as f32));
                s.append_stroke(&path, &gsk::Stroke::new(1.5), &rgba(0.0, 0.0, 0.0, 0.55));
            } else {
                s.append_fill(&path, gsk::FillRule::Winding, &rgba(0.1, 0.1, 0.12, 0.9));
                s.append_stroke(&path, &gsk::Stroke::new(1.5), &with_alpha(&col, 0.6));
            }
            let num = layout(c, &(i + 1).to_string(), "Sans Bold 7");
            let (tw, th) = num.pixel_size();
            draw_text(s, &num, nx - tw as f32 / 2.0, ny - th as f32 / 2.0, &rgba(0.0, 0.0, 0.0, if b.on { 0.75 } else { 0.0 }));
        }

        // Scale labels
        let label_col = rgba(1.0, 1.0, 1.0, 0.38);
        for (f, t) in [(50.0, "50"), (100.0, "100"), (200.0, "200"), (500.0, "500"), (1000.0, "1k"), (2000.0, "2k"), (5000.0, "5k"), (10000.0, "10k")] {
            let l = layout(c, t, "Sans 8");
            let (tw, th) = l.pixel_size();
            draw_text(s, &l, Self::x_of(f, wf) as f32 - tw as f32 / 2.0, h - th as f32 - 3.0, &label_col);
        }
        let mut db = -range + step;
        while db < range - 0.01 {
            let l = layout(c, &format!("{db:+.0}").replace("+0", "0"), "Sans 8");
            let (tw, th) = l.pixel_size();
            draw_text(s, &l, w - tw as f32 - 6.0, self.y_of(db, hf) as f32 - th as f32 / 2.0, &label_col);
            db += step;
        }

        // Read-outs
        let shown = self.drag.borrow().as_ref().map(|d| d.band).or(self.hover.get());
        if let Some(i) = shown.filter(|&i| i < cfg.bands.len()) {
            let b = &cfg.bands[i];
            let mut text = format!("{}  {}", b.kind.label(), fmt_freq(b.freq));
            if b.kind.has_gain() {
                text += &format!("  {}", fmt_gain(b.gain));
            }
            if b.kind.has_q() {
                text += &format!("  Q {}", fmt_q(b.q));
            }
            if b.kind.is_cut() {
                text += &format!("  {}", SLOPES[b.slope.clamp(1, 4) as usize - 1]);
            }
            let l = layout(c, &text, "Sans 9");
            let (tw, th) = l.pixel_size();
            let (nx, ny) = self.node_pos(b, wf, hf);
            let bw = tw as f32 + 16.0;
            let bh = th as f32 + 8.0;
            let bx = (nx as f32 - bw / 2.0).clamp(4.0, w - bw - 4.0);
            let by = if ny as f32 - 22.0 - bh > 4.0 { ny as f32 - 22.0 - bh } else { ny as f32 + 22.0 };
            let pb = gsk::PathBuilder::new();
            pb.add_rounded_rect(&round_rect(bx, by, bw, bh, 6.0));
            let p = pb.to_path();
            s.append_fill(&p, gsk::FillRule::Winding, &rgba(0.05, 0.055, 0.07, 0.88));
            s.append_stroke(&p, &gsk::Stroke::new(1.0), &with_alpha(&band_color(i), 0.6));
            draw_text(s, &l, bx + 8.0, by + 4.0, &rgba(1.0, 1.0, 1.0, 0.92));
        } else if let Some((px, py)) = self.pointer.get() {
            let f = Self::f_of(px, wf);
            let l = layout(c, &format!("{}  {}", fmt_freq(f), fmt_gain(self.db_of(py, hf))), "Sans 8");
            draw_text(s, &l, 8.0, 6.0, &rgba(1.0, 1.0, 1.0, 0.5));
        }
        if cfg.bands.is_empty() {
            let l = layout(c, "Double-click to add a band", "Sans 10");
            let (tw, _) = l.pixel_size();
            draw_text(s, &l, (w - tw as f32) / 2.0, y0 - 30.0, &rgba(1.0, 1.0, 1.0, 0.35));
        }
        if !cfg.enabled {
            let l = layout(c, "Equalizer off", "Sans Bold 9");
            let (tw, _) = l.pixel_size();
            draw_text(s, &l, w - tw as f32 - 10.0, 6.0, &rgba(1.0, 0.75, 0.45, 0.8));
        }
        s.pop();
    }

    fn show_menu(self: &Rc<Self>, x: f64, y: f64, band: Option<usize>) {
        let pop = &self.popover;
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let item = |label: &str, check: bool| {
            let b = gtk::Button::new();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let mark = gtk::Image::from_icon_name("object-select-symbolic");
            mark.set_opacity(if check { 1.0 } else { 0.0 });
            row.append(&mark);
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            row.append(&l);
            b.set_child(Some(&row));
            b.add_css_class("flat");
            list.append(&b);
            b
        };
        let (w, h) = (self.canvas.width() as f64, self.canvas.height() as f64);
        match band {
            Some(i) => {
                let b = self.ctl.cfg.borrow().bands[i].clone();
                for (k, name) in KINDS {
                    let v = Rc::downgrade(self);
                    item(name, b.kind == k).connect_clicked({ let pop = pop.clone(); move |_| {
                        pop.popdown();
                        if let Some(v) = v.upgrade() {
                            v.edit_band(i, |b| set_kind(b, k));
                        }
                    }});
                }
                list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                let v = Rc::downgrade(self);
                item(if b.on { "Disable Band" } else { "Enable Band" }, false).connect_clicked({ let pop = pop.clone(); move |_| {
                    pop.popdown();
                    if let Some(v) = v.upgrade() {
                        v.edit_band(i, |b| b.on = !b.on);
                    }
                }});
                let v = Rc::downgrade(self);
                item("Delete Band", false).connect_clicked({ let pop = pop.clone(); move |_| {
                    pop.popdown();
                    if let Some(v) = v.upgrade() {
                        v.remove_band(i);
                    }
                }});
            }
            None => {
                let freq = Self::f_of(x, w);
                let gain = self.db_of(y, h).clamp(-GAIN_MAX, GAIN_MAX);
                let head = gtk::Label::new(Some(&format!("Add band at {}", fmt_freq(freq))));
                head.add_css_class("dim-label");
                head.set_margin_top(4);
                head.set_margin_bottom(4);
                list.append(&head);
                for (k, name) in KINDS {
                    let v = Rc::downgrade(self);
                    item(name, false).connect_clicked({ let pop = pop.clone(); move |_| {
                        pop.popdown();
                        if let Some(v) = v.upgrade() {
                            v.add_band(k, freq, gain);
                        }
                    }});
                }
            }
        }
        pop.set_child(Some(&list));
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.popup();
    }

    fn connect_graph(self: &Rc<Self>) {
        let c = &self.canvas;
        let weak = Rc::downgrade(self);
        c.set_draw(move |c, s, w, h| {
            if let Some(v) = weak.upgrade() {
                v.draw(c, s, w, h);
            }
        });

        // Spectrum easing, once per display frame.
        let weak = Rc::downgrade(self);
        c.add_tick_callback(move |c, clock| {
            let Some(v) = weak.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let last = v.last_tick.replace(now);
            let dt = if last == 0 { 1.0 / 60.0 } else { ((now - last) as f32 / 1e6).clamp(0.0, 0.1) };
            if v.spec.borrow_mut().step(dt) {
                c.queue_draw();
            }
            if FPS_LOG.with(|f| *f) {
                v.frames.set(v.frames.get() + 1);
                if now - v.fps_at.get() >= 1_000_000 {
                    eprintln!("eq: {} ticks/s, {} draws/s", v.frames.get(), v.draws.get());
                    v.frames.set(0);
                    v.draws.set(0);
                    v.fps_at.set(now);
                }
            }
            glib::ControlFlow::Continue
        });

        // Analyzer only while the graph is on screen.
        let weak = Rc::downgrade(self);
        c.connect_map(move |_| {
            if let Some(v) = weak.upgrade() {
                let on = v.ctl.cfg.borrow().analyzer;
                v.ctl.set_spectrum(on);
            }
        });
        let weak = Rc::downgrade(self);
        c.connect_unmap(move |_| {
            if let Some(v) = weak.upgrade() {
                v.ctl.set_spectrum(false);
            }
        });

        // Drag a handle: frequency and gain (Q for cuts, notch, band pass). Shift = fine.
        let drag = gtk::GestureDrag::new();
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |_, x, y| {
            let Some(v) = weak.upgrade() else { return };
            v.canvas.grab_focus();
            let hit = v.hit(x, y);
            if hit != v.sel.get() {
                v.select(hit);
            }
            *v.drag.borrow_mut() = hit.map(|i| {
                let b = v.ctl.cfg.borrow().bands[i].clone();
                let (w, h) = (v.canvas.width() as f64, v.canvas.height() as f64);
                let (x0, y0) = v.node_pos(&b, w, h);
                Drag { band: i, x0, y0, start: b }
            });
            if hit.is_some() {
                v.canvas.set_cursor_from_name(Some("grabbing"));
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(v) = weak.upgrade() else { return };
            let Some((i, x0, y0, start)) = v.drag.borrow().as_ref().map(|d| (d.band, d.x0, d.y0, d.start.clone())) else { return };
            let fine = if g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) { 0.2 } else { 1.0 };
            let (w, h) = (v.canvas.width() as f64, v.canvas.height() as f64);
            let freq = Self::f_of(x0 + dx * fine, w).clamp(F_MIN, F_MAX);
            let gain = v.db_of(y0 + dy * fine, h).clamp(-GAIN_MAX, GAIN_MAX);
            v.edit_band(i, |b| {
                b.freq = freq;
                if b.kind.has_gain() {
                    b.gain = gain;
                } else {
                    b.q = (start.q * 2f64.powf(-dy * fine / 45.0)).clamp(Q_MIN, Q_MAX);
                }
            });
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(v) = weak.upgrade() {
                v.drag.borrow_mut().take();
                v.canvas.set_cursor_from_name(None);
                v.canvas.queue_draw();
            }
        });
        c.add_controller(drag);

        // Double-click: add a bell (empty spot) or switch a band on/off (handle).
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, n, x, y| {
            let Some(v) = weak.upgrade() else { return };
            if n != 2 {
                return;
            }
            match v.hit(x, y) {
                Some(i) => v.edit_band(i, |b| b.on = !b.on),
                None => {
                    let (w, h) = (v.canvas.width() as f64, v.canvas.height() as f64);
                    let gain = v.db_of(y, h).clamp(-GAIN_MAX, GAIN_MAX);
                    v.add_band(Kind::Bell, Self::f_of(x, w), gain);
                }
            }
        });
        c.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(self);
        menu.connect_pressed(move |_, _, x, y| {
            let Some(v) = weak.upgrade() else { return };
            let hit = v.hit(x, y);
            if hit.is_some() {
                v.select(hit);
            }
            v.show_menu(x, y, hit);
        });
        c.add_controller(menu);

        // Wheel over a handle: Q.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |_, _, dy| {
            let Some(v) = weak.upgrade() else { return glib::Propagation::Proceed };
            let Some(i) = v.hover.get() else { return glib::Propagation::Proceed };
            if !v.ctl.cfg.borrow().bands.get(i).is_some_and(|b| b.kind.has_q()) {
                return glib::Propagation::Proceed;
            }
            v.edit_band(i, |b| b.q = (b.q * 1.12f64.powf(-dy)).clamp(Q_MIN, Q_MAX));
            glib::Propagation::Stop
        });
        c.add_controller(scroll);

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            let Some(v) = weak.upgrade() else { return };
            v.pointer.set(Some((x, y)));
            if v.drag.borrow().is_none() {
                let hit = v.hit(x, y);
                v.hover.set(hit);
                v.canvas.set_cursor_from_name(if hit.is_some() { Some("grab") } else { Some("crosshair") });
            }
            v.canvas.queue_draw();
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(v) = weak.upgrade() {
                v.pointer.set(None);
                v.hover.set(None);
                v.canvas.queue_draw();
            }
        });
        c.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(v) = weak.upgrade() else { return glib::Propagation::Proceed };
            match (key, v.sel.get()) {
                (gdk::Key::Delete | gdk::Key::BackSpace, Some(i)) => {
                    v.remove_band(i);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        c.add_controller(keys);
    }

    fn connect_panel(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let with_sel = move |f: &dyn Fn(&Rc<View>, usize)| {
            if let Some(v) = weak.upgrade() {
                if let (false, Some(i)) = (v.syncing.get(), v.sel.get()) {
                    f(&v, i);
                }
            }
        };
        let w = with_sel.clone();
        self.power.connect_toggled(move |b| {
            let on = b.is_active();
            w(&|v, i| v.edit_band(i, |b| b.on = on));
        });
        let w = with_sel.clone();
        self.kind.connect_selected_notify(move |d| {
            let k = KINDS.get(d.selected() as usize).map(|(k, _)| *k).unwrap_or_default();
            w(&|v, i| v.edit_band(i, |b| set_kind(b, k)));
        });
        let w = with_sel.clone();
        self.slope.connect_selected_notify(move |d| {
            let slope = d.selected() as u8 + 1;
            w(&|v, i| v.edit_band(i, |b| b.slope = slope));
        });
        let w = with_sel.clone();
        self.freq.connect_change(move |x| w(&|v, i| v.edit_band(i, |b| b.freq = x)));
        let w = with_sel.clone();
        self.gain.connect_change(move |x| w(&|v, i| v.edit_band(i, |b| b.gain = x)));
        let w = with_sel.clone();
        self.q.connect_change(move |x| w(&|v, i| v.edit_band(i, |b| b.q = x)));
        let w = with_sel.clone();
        self.remove.connect_clicked(move |_| w(&|v, i| v.remove_band(i)));

        let ctl = self.ctl.clone();
        let weak = Rc::downgrade(self);
        self.preamp.connect_change(move |x| {
            if weak.upgrade().is_some_and(|v| !v.syncing.get()) {
                ctl.edit(|cfg| cfg.preamp = x);
            }
        });

        let weak = Rc::downgrade(self);
        self.enabled.connect_active_notify(move |sw| {
            let Some(v) = weak.upgrade() else { return };
            if !v.syncing.get() {
                let on = sw.is_active();
                v.ctl.cfg.borrow_mut().enabled = on;
                v.ctl.commit();
            }
        });

        let weak = Rc::downgrade(self);
        self.preset_row.connect_selected_notify(move |row| {
            let Some(v) = weak.upgrade() else { return };
            if v.syncing.get() {
                return;
            }
            let name = v.preset_names.borrow().get(row.selected() as usize).cloned();
            match name {
                Some(n) if n != "Custom" => v.ctl.load_preset(&n),
                _ => {
                    v.ctl.cfg.borrow_mut().preset.clear();
                    v.ctl.commit();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.delete_preset.connect_clicked(move |_| {
            let Some(v) = weak.upgrade() else { return };
            {
                let mut cfg = v.ctl.cfg.borrow_mut();
                let name = cfg.preset.clone();
                cfg.presets.remove(&name);
                cfg.preset.clear();
            }
            v.ctl.commit();
            v.rebuild_presets();
        });

        let weak = Rc::downgrade(self);
        self.test_row.connect_active_notify(move |row| {
            if let Some(v) = weak.upgrade().filter(|v| !v.syncing.get()) {
                v.ctl.set_test(row.is_active());
            }
        });
        let weak = Rc::downgrade(self);
        self.range_row.connect_selected_notify(move |row| {
            if let Some(v) = weak.upgrade().filter(|v| !v.syncing.get()) {
                v.ctl.cfg.borrow_mut().range = RANGES[(row.selected() as usize).min(RANGES.len() - 1)];
                v.ctl.commit();
            }
        });
        let weak = Rc::downgrade(self);
        self.analyzer_row.connect_active_notify(move |row| {
            if let Some(v) = weak.upgrade().filter(|v| !v.syncing.get()) {
                let on = row.is_active();
                v.ctl.cfg.borrow_mut().analyzer = on;
                v.ctl.set_spectrum(on && v.canvas.is_mapped());
                v.ctl.commit();
            }
        });
        let weak = Rc::downgrade(self);
        self.pre_row.connect_active_notify(move |row| {
            if let Some(v) = weak.upgrade().filter(|v| !v.syncing.get()) {
                v.ctl.cfg.borrow_mut().show_pre = row.is_active();
                v.ctl.commit();
            }
        });
    }
}

/// Changes a band's type, keeping it sensible (cuts start flat-Q, others at Q 1).
fn set_kind(b: &mut Band, k: Kind) {
    if b.kind == k {
        return;
    }
    if k.is_cut() != b.kind.is_cut() {
        b.q = if k.is_cut() { 0.71 } else { 1.0 };
    }
    b.kind = k;
}

fn widen_clamps(w: &gtk::Widget) {
    if let Some(c) = w.downcast_ref::<adw::Clamp>() {
        c.set_maximum_size(1000);
        c.set_tightening_threshold(800);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        widen_clamps(&c);
        child = c.next_sibling();
    }
}

/// The "Equalizer" preferences page.
pub fn page() -> Option<adw::PreferencesPage> {
    let ctl = ctl()?;
    let page = adw::PreferencesPage::builder().title("Equalizer").name("equalizer").icon_name("audio-speakers-symbolic").build();

    // Graph + band panel
    let canvas = Canvas::new();
    canvas.set_size_request(-1, 360);
    canvas.set_hexpand(true);
    canvas.set_focusable(true);
    canvas.set_focus_on_click(true);
    let popover = gtk::Popover::new();
    popover.set_has_arrow(true);
    popover.set_parent(&canvas);
    {
        let popover = popover.clone();
        canvas.connect_destroy(move |_| popover.unparent());
    }

    let band_label = gtk::Label::new(None);
    band_label.set_xalign(0.0);
    band_label.add_css_class("heading");
    band_label.set_width_chars(16);
    let power = gtk::ToggleButton::builder().icon_name("system-shutdown-symbolic").tooltip_text("Band on / off").valign(gtk::Align::Center).build();
    power.add_css_class("circular");
    let kind = gtk::DropDown::from_strings(&KINDS.map(|(_, n)| n));
    kind.set_valign(gtk::Align::Center);
    kind.set_tooltip_text(Some("Filter type"));
    let slope = gtk::DropDown::from_strings(&SLOPES);
    slope.set_valign(gtk::Align::Center);
    slope.set_tooltip_text(Some("Slope"));
    let remove = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Delete band").valign(gtk::Align::Center).build();
    remove.add_css_class("flat");
    remove.add_css_class("circular");
    let freq = Knob::new("FREQ", F_MIN, F_MAX, true, 1000.0, fmt_freq);
    let gain = Knob::new("GAIN", -GAIN_MAX, GAIN_MAX, false, 0.0, fmt_gain);
    let q = Knob::new("Q", Q_MIN, Q_MAX, true, 1.0, fmt_q);
    let preamp = Knob::new("PREAMP", -24.0, 12.0, false, 0.0, fmt_gain);
    preamp.set_color(rgba(1.0, 0.86, 0.45, 1.0));

    let head = gtk::Box::new(gtk::Orientation::Vertical, 6);
    head.set_valign(gtk::Align::Center);
    head.append(&band_label);
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tools.append(&power);
    tools.append(&kind);
    tools.append(&remove);
    head.append(&tools);
    head.append(&slope);

    let panel = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    panel.set_margin_top(10);
    panel.set_margin_start(4);
    panel.set_margin_end(4);
    panel.append(&head);
    panel.append(&freq.root);
    panel.append(&gain.root);
    panel.append(&q.root);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    panel.append(&spacer);
    panel.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    panel.append(&preamp.root);

    let hint = gtk::Label::new(Some(
        "Double-click: add a band or switch one off · Drag: frequency and gain · Wheel: Q · Shift: fine · Right-click: type · Delete: remove",
    ));
    hint.add_css_class("caption");
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.set_margin_top(8);

    let editor = gtk::Box::new(gtk::Orientation::Vertical, 0);
    editor.append(&canvas);
    editor.append(&panel);
    editor.append(&hint);

    let enabled = gtk::Switch::builder().valign(gtk::Align::Center).tooltip_text("Equalizer on / off").build();
    let eq_group = adw::PreferencesGroup::builder()
        .title("Parametric Equalizer")
        .description("Processes the music as it plays. The analyzer shows the playing track before (grey) and after (blue) the equalizer.")
        .header_suffix(&enabled)
        .build();
    eq_group.add(&editor);
    page.add(&eq_group);

    // Presets
    let preset_row = adw::ComboRow::builder().title("Preset").build();
    let delete_preset = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text("Delete this preset").valign(gtk::Align::Center).build();
    delete_preset.add_css_class("flat");
    preset_row.add_suffix(&delete_preset);
    let save_row = adw::EntryRow::builder().title("Save current settings as preset").show_apply_button(true).build();
    let presets_group = adw::PreferencesGroup::builder().title("Presets").build();
    presets_group.add(&preset_row);
    presets_group.add(&save_row);
    page.add(&presets_group);

    // Test track
    let test_row = adw::SwitchRow::builder().title("Play Test Track").build();
    let use_current = gtk::Button::builder().label("Use Current Track").valign(gtk::Align::Center).tooltip_text("Make the playing song the test track").build();
    use_current.add_css_class("flat");
    test_row.add_suffix(&use_current);
    let test_group = adw::PreferencesGroup::builder()
        .title("Audition")
        .description("Plays a reference song on repeat while you tune; switching it off brings back what was playing.")
        .build();
    test_group.add(&test_row);
    page.add(&test_group);

    // Display
    let range_row = adw::ComboRow::builder()
        .title("Curve Range")
        .model(&gtk::StringList::new(&["±6 dB", "±12 dB", "±18 dB", "±24 dB", "±30 dB"]))
        .build();
    let analyzer_row = adw::SwitchRow::builder().title("Spectrum Analyzer").build();
    let pre_row = adw::SwitchRow::builder().title("Show Input Spectrum").subtitle("The track before the equalizer, in grey").build();
    let display_group = adw::PreferencesGroup::builder().title("Display").build();
    display_group.add(&range_row);
    display_group.add(&analyzer_row);
    display_group.add(&pre_row);
    page.add(&display_group);

    let view = Rc::new(View {
        ctl: ctl.clone(),
        canvas,
        sel: Cell::new(None),
        hover: Cell::new(None),
        pointer: Cell::new(None),
        drag: RefCell::new(None),
        spec: RefCell::new(Spectrum::default()),
        curve: RefCell::new(CurveCache::default()),
        last_tick: Cell::new(0),
        frames: Cell::new(0),
        draws: Cell::new(0),
        fps_at: Cell::new(0),
        popover,
        syncing: Cell::new(false),
        band_label,
        power,
        kind,
        slope,
        freq,
        gain,
        q,
        remove,
        preamp,
        enabled,
        preset_row,
        preset_names: RefCell::new(vec![]),
        delete_preset,
        test_row,
        range_row,
        analyzer_row,
        pre_row,
    });
    *ctl.view.borrow_mut() = Rc::downgrade(&view);
    view.connect_graph();
    view.connect_panel();
    view.rebuild_presets();
    if !ctl.cfg.borrow().bands.is_empty() {
        view.select(Some(0));
    }

    let weak = Rc::downgrade(&view);
    save_row.connect_apply(move |row| {
        let Some(v) = weak.upgrade() else { return };
        let mut name = row.text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        if builtin_presets().iter().any(|(n, _)| *n == name) || name == "Custom" {
            name += " (mine)";
        }
        {
            let mut cfg = v.ctl.cfg.borrow_mut();
            let p = Preset { preamp: cfg.preamp, bands: cfg.bands.clone() };
            cfg.presets.insert(name.clone(), p);
            cfg.preset = name;
            cfg.modified = false;
        }
        row.set_text("");
        v.ctl.commit();
        v.rebuild_presets();
    });
    let weak = Rc::downgrade(&view);
    use_current.connect_clicked(move |_| {
        let Some(v) = weak.upgrade() else { return };
        let Some(app) = super::app() else { return };
        let now = app.st.borrow().now.clone();
        match now.filter(|it| it.kind() == crate::model::Kind::Song) {
            Some(it) => {
                {
                    let mut cfg = v.ctl.cfg.borrow_mut();
                    cfg.test_id = it.song_play_id();
                    cfg.test_title = format!("{} — {}", it.artist(), it.title());
                }
                v.ctl.commit();
                app.toast("Test track set");
            }
            None => app.toast("Play a song first"),
        }
    });

    // The page's clamp keeps rows at ~600 px; the graph wants the dialog's whole width.
    widen_clamps(page.upcast_ref());

    // The view lives as long as the page.
    page.connect_destroy(move |_| {
        let _ = &view;
    });
    Some(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EQ_RESPONSE_JSON = {freqs, bands, resp} measured with the engine's `eqResponse`.
    #[test]
    fn curve_matches_engine() {
        let Ok(path) = std::env::var("EQ_RESPONSE_JSON") else { return };
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let freqs: Vec<f64> = serde_json::from_value(v["freqs"].clone()).unwrap();
        let bands: Vec<Band> = serde_json::from_value(v["bands"].clone()).unwrap();
        let engine: Vec<f64> = serde_json::from_value(v["resp"].clone()).unwrap();
        let mut total = vec![0.0; freqs.len()];
        for b in &bands {
            for (t, r) in total.iter_mut().zip(band_response(b, &freqs, 48000.0)) {
                *t += r;
            }
        }
        for ((f, ours), theirs) in freqs.iter().zip(&total).zip(&engine) {
            println!("{f:>7} Hz  ui {ours:>8.2}  engine {theirs:>8.2}");
        }
    }
}
