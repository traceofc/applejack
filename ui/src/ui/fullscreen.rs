// Full-screen now playing: the cover over a blurred, darkened copy of itself, transport
// controls, and the lyrics (syllable by syllable where Apple has them) beside it.
// F11 or the bar's button opens it; Escape / F11 close it.
use std::{cell::Cell, rc::Rc, time::{Duration, Instant}};

use adw::prelude::*;
use gtk::{gdk, glib};

use super::{App, app, lyrics::LyricsView, widgets::{label, no_fine_tune}};
use crate::{
    art::ArtLoader,
    model::{Item, fmt_time},
};

const COVER: i32 = 440;
const COVER_NARROW: i32 = 240;

pub struct FullScreen {
    pub root: gtk::Overlay,
    bg: gtk::Picture,
    cover: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    pub play: gtk::Button,
    seek: gtk::Scale,
    pos: gtk::Label,
    dur: gtk::Label,
    pub lyrics: Rc<LyricsView>,
    lyrics_toggle: gtk::ToggleButton,
    content: gtk::Box,
    art_for: std::cell::RefCell<Option<String>>,
    seek_guard: Cell<Option<Instant>>,
    seek_held: Cell<bool>,
}

fn round_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tip));
    b
}

impl FullScreen {
    pub fn new() -> Rc<Self> {
        let bg = gtk::Picture::new();
        bg.set_content_fit(gtk::ContentFit::Cover);
        bg.set_can_shrink(true);
        bg.add_css_class("fs-bg");

        let cover = gtk::Image::new();
        cover.set_pixel_size(COVER);
        cover.add_css_class("fs-cover");
        cover.set_overflow(gtk::Overflow::Hidden);
        cover.set_halign(gtk::Align::Center);
        let title = label("Not Playing", &["fs-title"]);
        title.set_xalign(0.5);
        title.set_max_width_chars(1);
        title.set_hexpand(true);
        let artist = label("", &["fs-artist"]);
        artist.set_xalign(0.5);
        artist.set_max_width_chars(1);

        let pos = label("0:00", &["numeric", "caption", "fs-dim"]);
        let dur = label("0:00", &["numeric", "caption", "fs-dim"]);
        pos.set_width_chars(6);
        pos.set_xalign(1.0);
        dur.set_width_chars(6);
        dur.set_xalign(0.0);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
        no_fine_tune(&seek);
        seek.set_hexpand(true);
        seek.set_draw_value(false);
        let timeline = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        timeline.append(&pos);
        timeline.append(&seek);
        timeline.append(&dur);

        let prev = round_button("media-skip-backward-symbolic", "Previous");
        let play = round_button("media-playback-start-symbolic", "Play");
        play.add_css_class("fs-play");
        let next = round_button("media-skip-forward-symbolic", "Next");
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        controls.set_halign(gtk::Align::Center);
        controls.append(&prev);
        controls.append(&play);
        controls.append(&next);

        let left = gtk::Box::new(gtk::Orientation::Vertical, 10);
        left.set_valign(gtk::Align::Center);
        left.set_size_request(COVER_NARROW, -1);
        left.append(&cover);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_margin_top(14);
        texts.append(&title);
        texts.append(&artist);
        left.append(&texts);
        left.append(&timeline);
        left.append(&controls);
        // Same width as the cover, so long titles ellipsize instead of widening the column.
        let left_clamp = adw::Clamp::builder().maximum_size(COVER).tightening_threshold(COVER).child(&left).build();
        left_clamp.set_valign(gtk::Align::Center);

        let lyrics = LyricsView::new("lyric-big", || app().map(|a| a.position()).unwrap_or(0.0));
        lyrics.root.set_hexpand(true);
        lyrics.root.set_size_request(320, -1);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 64);
        content.set_margin_top(56);
        content.set_margin_bottom(40);
        content.set_margin_start(64);
        content.set_margin_end(48);
        content.append(&left_clamp);
        content.append(&lyrics.root);
        content.set_homogeneous(false);

        let close = round_button("view-restore-symbolic", "Leave Full Screen (Esc)");
        let lyrics_toggle = gtk::ToggleButton::builder()
            .icon_name("media-view-subtitles-symbolic")
            .tooltip_text("Lyrics")
            .active(true)
            .valign(gtk::Align::Center)
            .build();
        lyrics_toggle.add_css_class("flat");
        lyrics_toggle.add_css_class("circular");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.set_halign(gtk::Align::End);
        top.set_valign(gtk::Align::Start);
        top.set_margin_top(12);
        top.set_margin_end(12);
        top.append(&lyrics_toggle);
        top.append(&close);

        let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scrim.add_css_class("fs-scrim");
        let root = gtk::Overlay::new();
        root.add_css_class("fullscreen-player");
        root.set_child(Some(&bg));
        root.add_overlay(&scrim);
        root.add_overlay(&content);
        root.add_overlay(&top);
        root.set_focusable(true);

        let fs = Rc::new(Self {
            root,
            bg,
            cover,
            title,
            artist,
            play,
            seek,
            pos,
            dur,
            lyrics,
            lyrics_toggle,
            content,
            art_for: Default::default(),
            seek_guard: Cell::new(None),
            seek_held: Cell::new(false),
        });

        fs.play.connect_clicked(|_| {
            if let Some(a) = app() {
                a.toggle_play();
            }
        });
        prev.connect_clicked(|_| {
            if let Some(a) = app() {
                a.skip(false);
            }
        });
        next.connect_clicked(|_| {
            if let Some(a) = app() {
                a.skip(true);
            }
        });
        close.connect_clicked(|_| {
            if let Some(a) = app() {
                a.set_fullscreen(false);
            }
        });
        let weak = Rc::downgrade(&fs);
        fs.lyrics_toggle.connect_toggled(move |_| {
            if let Some(fs) = weak.upgrade() {
                fs.layout();
                if let Some(a) = app() {
                    a.load_lyrics();
                }
            }
        });
        let weak = Rc::downgrade(&fs);
        fs.seek.connect_change_value(move |_, _, value| {
            if let Some(fs) = weak.upgrade() {
                fs.pos.set_label(&fmt_time(value));
                fs.seek_guard.set(Some(Instant::now() + Duration::from_millis(1500)));
                if !fs.seek_held.get() {
                    if let Some(a) = app() {
                        a.seek_to(value.max(0.0));
                    }
                }
            }
            glib::Propagation::Proceed
        });
        // Seek once the slider is let go, not on every step of the drag.
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&fs);
        legacy.connect_event(move |_, ev| {
            use gdk::EventType as T;
            let Some(fs) = weak.upgrade() else { return glib::Propagation::Proceed };
            match ev.event_type() {
                T::ButtonPress | T::TouchBegin => fs.seek_held.set(true),
                T::ButtonRelease | T::TouchEnd | T::TouchCancel => {
                    fs.seek_held.set(false);
                    fs.seek_guard.set(Some(Instant::now() + Duration::from_millis(1500)));
                    if let Some(a) = app() {
                        a.seek_to(fs.seek.value().max(0.0));
                    }
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        fs.seek.add_controller(legacy);

        fs
    }

    pub fn lyrics_wanted(&self) -> bool {
        self.root.is_mapped() && self.lyrics_toggle.is_active()
    }

    /// Lays out for the window width and the lyrics toggle: narrow windows put the cover
    /// above the lyrics.
    pub fn layout(&self) {
        let w = self.root.root().map(|r| r.width()).unwrap_or(0);
        let narrow = w > 0 && w < 900;
        let show_lyrics = self.lyrics_toggle.is_active();
        self.lyrics.root.set_visible(show_lyrics);
        self.content.set_orientation(if narrow && show_lyrics { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        self.cover.set_pixel_size(if narrow && show_lyrics { COVER_NARROW } else { COVER });
        self.content.set_halign(if show_lyrics { gtk::Align::Fill } else { gtk::Align::Center });
        self.content.set_spacing(if narrow { 24 } else { 64 });
    }

    pub fn set_track(&self, art: &Rc<ArtLoader>, it: Option<&Item>) {
        let Some(it) = it else {
            self.title.set_label("Not Playing");
            self.artist.set_label("");
            self.cover.clear();
            self.bg.set_paintable(None::<&gdk::Paintable>);
            *self.art_for.borrow_mut() = None;
            return;
        };
        self.title.set_label(it.title());
        self.title.set_tooltip_text(Some(it.title()));
        let sub = match it.album.as_deref().filter(|a| !a.is_empty()) {
            Some(a) => format!("{} — {a}", it.artist()),
            None => it.artist().to_owned(),
        };
        self.artist.set_label(&sub);
        let url = it.art(1000);
        if *self.art_for.borrow() == url {
            return;
        }
        *self.art_for.borrow_mut() = url.clone();
        let Some(url) = url else {
            self.cover.clear();
            self.bg.set_paintable(None::<&gdk::Paintable>);
            return;
        };
        let (cover, bg, expected) = (self.cover.downgrade(), self.bg.downgrade(), url.clone());
        art.load(&url, move |tex| {
            let (Some(cover), Some(bg)) = (cover.upgrade(), bg.upgrade()) else { return };
            if app().is_some_and(|a| a.full.art_for.borrow().as_deref() != Some(expected.as_str())) {
                return;
            }
            cover.set_paintable(Some(tex));
            bg.set_paintable(blurred(tex).as_ref());
        });
    }

    pub fn set_playing(&self, busy: bool) {
        let (icon, tip) =
            if busy { ("media-playback-pause-symbolic", "Pause") } else { ("media-playback-start-symbolic", "Play") };
        self.play.set_icon_name(icon);
        self.play.set_tooltip_text(Some(tip));
    }

    pub fn set_time(&self, pos: f64, duration: f64) {
        if self.seek_held.get() || self.seek_guard.get().is_some_and(|g| Instant::now() < g) {
            return;
        }
        if duration > 0.0 && (self.seek.adjustment().upper() - duration).abs() > 0.5 {
            self.seek.set_range(0.0, duration);
        }
        self.seek.set_value(pos);
        self.pos.set_label(&fmt_time(pos));
        self.dur.set_label(&fmt_time(duration));
    }
}

/// A small, heavily blurred and darkened copy of the cover for the background; the GPU
/// scales it up smoothly, so the blur costs nothing per frame.
fn blurred(tex: &gdk::Texture) -> Option<gdk::Texture> {
    const N: usize = 48;
    let (w, h) = (tex.width() as usize, tex.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut src = vec![0u8; w * h * 4];
    let mut dl = gdk::TextureDownloader::new(tex);
    dl.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = dl.download_bytes();
    for y in 0..h {
        let row = &bytes[y * stride..y * stride + w * 4];
        src[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
    }
    // Area-average down to N×N.
    let mut img = vec![0f32; N * N * 3];
    for y in 0..N {
        for x in 0..N {
            let (x0, x1) = (x * w / N, ((x + 1) * w / N).max(x * w / N + 1));
            let (y0, y1) = (y * h / N, ((y + 1) * h / N).max(y * h / N + 1));
            let mut acc = [0f32; 3];
            let mut n = 0f32;
            for sy in (y0..y1).step_by(2) {
                for sx in (x0..x1).step_by(2) {
                    let i = (sy * w + sx) * 4;
                    acc[0] += src[i] as f32;
                    acc[1] += src[i + 1] as f32;
                    acc[2] += src[i + 2] as f32;
                    n += 1.0;
                }
            }
            for c in 0..3 {
                img[(y * N + x) * 3 + c] = acc[c] / n.max(1.0);
            }
        }
    }
    // Three box-blur passes ≈ a gaussian.
    let r = 4isize;
    for _ in 0..3 {
        for horizontal in [true, false] {
            let prev = img.clone();
            for y in 0..N as isize {
                for x in 0..N as isize {
                    let mut acc = [0f32; 3];
                    for d in -r..=r {
                        let (sx, sy) = if horizontal { ((x + d).clamp(0, N as isize - 1), y) } else { (x, (y + d).clamp(0, N as isize - 1)) };
                        let i = (sy as usize * N + sx as usize) * 3;
                        acc[0] += prev[i];
                        acc[1] += prev[i + 1];
                        acc[2] += prev[i + 2];
                    }
                    let i = (y as usize * N + x as usize) * 3;
                    for c in 0..3 {
                        img[i + c] = acc[c] / (2 * r + 1) as f32;
                    }
                }
            }
        }
    }
    // Darken (white text on top) and lift the saturation a little.
    let mut out = Vec::with_capacity(N * N * 3);
    for px in img.chunks(3) {
        let l = 0.3 * px[0] + 0.59 * px[1] + 0.11 * px[2];
        for c in px {
            let v = (l + (c - l) * 1.35) * 0.55;
            out.push(v.clamp(0.0, 255.0) as u8);
        }
    }
    let bytes = glib::Bytes::from_owned(out);
    Some(gdk::MemoryTexture::new(N as i32, N as i32, gdk::MemoryFormat::R8g8b8, &bytes, N * 3).upcast())
}

impl App {
    /// Shows / leaves the full-screen player (the window goes full screen with it).
    pub fn set_fullscreen(self: &Rc<Self>, on: bool) {
        if on == self.is_fullscreen() {
            return;
        }
        if on {
            self.window.present();
            self.main_stack.set_visible_child_name("full");
            self.window.fullscreen();
            self.full.root.grab_focus();
            let now = self.st.borrow().now.clone();
            self.full.set_track(&self.art, now.as_ref());
            let (pos, dur) = (self.position(), self.st.borrow().duration);
            self.full.set_time(pos, dur);
            let full = self.full.clone();
            glib::idle_add_local_once(move || full.layout());
            self.st.borrow_mut().lyrics_for = None;
            self.load_lyrics();
        } else {
            self.main_stack.set_visible_child_name("main");
            self.window.unfullscreen();
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.main_stack.visible_child_name().as_deref() == Some("full")
    }
}
