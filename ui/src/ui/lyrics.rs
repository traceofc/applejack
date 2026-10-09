// Lyrics view shared by the side panel and the full-screen player. Line-synced lyrics
// highlight the line being sung; syllable-synced lyrics (Apple's "syllable-lyrics") also
// light up each syllable as it is sung, fading it in over its own duration. While the view
// is on screen it follows the playback clock every frame.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::prelude::*;
use gtk::glib;

use super::{app, widgets::wrap_label};
use crate::model::{LyricLine, Lyrics};

/// Opacity of lines (and syllables) not being sung.
const DIM: f64 = 0.35;

struct Line {
    main: gtk::Label,
    bg: Option<gtk::Label>,
    /// Last markup set, so unchanged frames don't relayout.
    shown: RefCell<String>,
}

pub struct LyricsView {
    pub root: gtk::Stack,
    status: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    column: gtk::Box,
    lines: RefCell<Vec<Line>>,
    lyrics: RefCell<Lyrics>,
    active: Cell<Option<usize>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    /// Paused auto-scroll after the user scrolled by hand (frame time, µs).
    hold_until: Cell<i64>,
    line_class: &'static str,
}

impl LyricsView {
    /// `clock` gives the playback position; `line_class` sizes the text.
    pub fn new(line_class: &'static str, clock: impl Fn() -> f64 + 'static) -> Rc<Self> {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
        column.set_margin_top(24);
        column.set_margin_bottom(240);
        column.set_margin_start(18);
        column.set_margin_end(18);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&column)
            .build();
        let status = adw::StatusPage::builder().icon_name("media-view-subtitles-symbolic").title("No Lyrics").build();
        status.add_css_class("compact");
        let root = gtk::Stack::new();
        root.set_transition_type(gtk::StackTransitionType::Crossfade);
        root.add_named(&scroller, Some("lyrics"));
        root.add_named(&status, Some("status"));
        root.set_visible_child_name("status");

        let view = Rc::new(Self {
            root,
            status,
            scroller,
            column,
            lines: Default::default(),
            lyrics: Default::default(),
            active: Cell::new(None),
            tick: Default::default(),
            hold_until: Cell::new(0),
            line_class,
        });

        // Scrolling by hand pauses following the song for a few seconds.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = Rc::downgrade(&view);
        scroll.connect_scroll(move |c, _, _| {
            if let (Some(v), Some(clock)) = (weak.upgrade(), c.widget().and_then(|w| w.frame_clock())) {
                v.hold_until.set(clock.frame_time() + 4_000_000);
            }
            glib::Propagation::Proceed
        });
        view.scroller.add_controller(scroll);

        // Follow the clock only while on screen.
        let clock = Rc::new(clock);
        let weak = Rc::downgrade(&view);
        view.root.connect_map(move |w| {
            let Some(v) = weak.upgrade() else { return };
            let (weak, clock) = (Rc::downgrade(&v), clock.clone());
            let id = w.add_tick_callback(move |_, fc| {
                if let Some(v) = weak.upgrade() {
                    v.update(clock(), fc.frame_time());
                }
                glib::ControlFlow::Continue
            });
            if let Some(old) = v.tick.replace(Some(id)) {
                old.remove();
            }
        });
        let weak = Rc::downgrade(&view);
        view.root.connect_unmap(move |_| {
            if let Some(id) = weak.upgrade().and_then(|v| v.tick.take()) {
                id.remove();
            }
        });
        view
    }

    pub fn show_loading(&self) {
        self.status.set_title("Loading Lyrics…");
        self.root.set_visible_child_name("status");
    }

    pub fn show(&self, lyrics: Option<Lyrics>) {
        while let Some(c) = self.column.first_child() {
            self.column.remove(&c);
        }
        self.lines.borrow_mut().clear();
        self.active.set(None);
        let Some(lyrics) = lyrics.filter(|l| l.lines.iter().any(|x| !x.text.is_empty())) else {
            self.status.set_title("This song has no lyrics");
            self.root.set_visible_child_name("status");
            *self.lyrics.borrow_mut() = Lyrics::default();
            return;
        };
        let mut lines = Vec::new();
        for line in &lyrics.lines {
            let main = wrap_label(&line.text, &[self.line_class]);
            let bg_text: String = line.words.iter().filter(|w| w.bg).map(|w| w.text.as_str()).collect();
            let bg = (!bg_text.trim().is_empty()).then(|| wrap_label(bg_text.trim(), &[self.line_class, "lyric-bg"]));
            let holder = gtk::Box::new(gtk::Orientation::Vertical, 2);
            holder.append(&main);
            if let Some(b) = &bg {
                holder.append(b);
            }
            if line.text.is_empty() {
                holder.set_size_request(-1, 8);
            }
            if lyrics.synced {
                holder.add_css_class("lyric-dim");
                if let Some(start) = line.start {
                    let click = gtk::GestureClick::new();
                    click.connect_released(move |_, _, _, _| {
                        if let Some(app) = app() {
                            app.seek_to(start);
                        }
                    });
                    holder.add_controller(click);
                    holder.set_cursor_from_name(Some("pointer"));
                }
            }
            self.column.append(&holder);
            lines.push(Line { main, bg, shown: Default::default() });
        }
        *self.lines.borrow_mut() = lines;
        *self.lyrics.borrow_mut() = lyrics;
        self.scroller.vadjustment().set_value(0.0);
        self.root.set_visible_child_name("lyrics");
    }

    /// Highlights what is being sung at `t` and keeps it in view.
    fn update(&self, t: f64, frame_time: i64) {
        let lyrics = self.lyrics.borrow();
        if !lyrics.synced {
            return;
        }
        let lead = if lyrics.syllable { 0.05 } else { 0.25 };
        let idx = lyrics.lines.iter().rposition(|l| l.start.is_some_and(|s| s <= t + lead) && !l.text.is_empty());
        let lines = self.lines.borrow();
        if self.active.get() != idx {
            if let Some(old) = self.active.get().and_then(|i| lines.get(i)) {
                holder(old).remove_css_class("lyric-active");
                reset(old, &lyrics.lines[self.active.get().unwrap()]);
            }
            self.active.set(idx);
            if let Some(l) = idx.and_then(|i| lines.get(i)) {
                holder(l).add_css_class("lyric-active");
                if frame_time >= self.hold_until.get() {
                    self.scroll_to(&holder(l));
                }
            }
        }
        if let (true, Some(i)) = (lyrics.syllable, idx) {
            if let (Some(w), Some(src)) = (lines.get(i), lyrics.lines.get(i)) {
                paint(w, src, t);
            }
        }
    }

    fn scroll_to(&self, w: &gtk::Widget) {
        // Active line about a third from the top.
        let adj = self.scroller.vadjustment();
        if let Some(p) = w.compute_point(&self.column, &gtk::graphene::Point::new(0.0, 0.0)) {
            let target = (p.y() as f64 - adj.page_size() / 3.0).max(0.0);
            animate_scroll(&adj, target);
        }
    }
}

fn holder(l: &Line) -> gtk::Widget {
    l.main.parent().unwrap()
}

/// Back to plain text once a line is done.
fn reset(w: &Line, src: &LyricLine) {
    if !w.shown.borrow().is_empty() {
        w.main.set_text(&src.text);
        if let Some(bg) = &w.bg {
            let t: String = src.words.iter().filter(|x| x.bg).map(|x| x.text.as_str()).collect();
            bg.set_text(t.trim());
        }
        w.shown.borrow_mut().clear();
    }
}

/// Syllables sung so far at full strength, the current one fading in, the rest dim.
fn paint(w: &Line, src: &LyricLine, t: f64) {
    let markup = |bg: bool| {
        let mut s = String::new();
        let words: Vec<_> = src.words.iter().filter(|x| x.bg == bg).collect();
        for (n, x) in words.iter().enumerate() {
            let (start, end) = (x.start.unwrap_or(0.0), x.end.unwrap_or(0.0).max(x.start.unwrap_or(0.0) + 0.05));
            let k = ((t - start) / (end - start)).clamp(0.0, 1.0);
            let a = (DIM + (1.0 - DIM) * k) * 100.0;
            let text = if n == 0 { x.text.trim_start() } else if n + 1 == words.len() { x.text.trim_end() } else { &x.text };
            s.push_str(&format!("<span alpha=\"{}%\">{}</span>", a.round() as u32, glib::markup_escape_text(text)));
        }
        s
    };
    let main = markup(false);
    let bg = if w.bg.is_some() { markup(true) } else { String::new() };
    let key = format!("{main}\u{1}{bg}");
    if *w.shown.borrow() == key {
        return;
    }
    w.main.set_markup(&main);
    if let Some(l) = &w.bg {
        l.set_markup(&bg);
    }
    *w.shown.borrow_mut() = key;
}

fn animate_scroll(adj: &gtk::Adjustment, target: f64) {
    let from = adj.value();
    if (from - target).abs() < 1.0 {
        return;
    }
    let adj = adj.clone();
    let steps = 14;
    let step = Rc::new(Cell::new(0));
    glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        let n = step.get() + 1;
        step.set(n);
        let t = n as f64 / steps as f64;
        let ease = 1.0 - (1.0 - t).powi(3);
        adj.set_value(from + (target - from) * ease);
        if n >= steps { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
    });
}
