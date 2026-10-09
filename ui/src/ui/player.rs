// Now-playing bar and the side panel (queue / lyrics).
use std::rc::Rc;

use adw::prelude::*;

use super::{
    app,
    lyrics::LyricsView,
    widgets::{RowMode, art_image, connect_row_play, fill_row, label, load_art, no_fine_tune, px, row_parts, song_row, ROW_ART},
};
use crate::{
    art::ArtLoader,
    model::{Item, Lyrics, Queue},
};

pub const BAR_ART: i32 = 48;
/// Fixed width of the title / artist block, so the controls never move with the text.
const NOW_TEXT_WIDTH: i32 = 220;

/// Slider position (0..1) -> engine volume. Loudness is perceived roughly logarithmically,
/// so a cubic curve makes equal slider steps sound like equal steps (≈ -18 dB at the middle).
pub fn slider_to_volume(s: f64) -> f64 {
    s.clamp(0.0, 1.0).powi(3)
}

pub fn volume_to_slider(v: f64) -> f64 {
    v.clamp(0.0, 1.0).cbrt()
}

pub struct PlayerBar {
    pub root: gtk::Box,
    pub art: gtk::Image,
    pub title: gtk::Label,
    pub sub: gtk::Label,
    /// Favourite ("love") toggle for the current track.
    pub star: gtk::ToggleButton,
    pub prev: gtk::Button,
    pub play: gtk::Button,
    pub next: gtk::Button,
    pub shuffle: gtk::ToggleButton,
    pub repeat: gtk::Button,
    pub seek: gtk::Scale,
    pub pos: gtk::Label,
    pub dur: gtk::Label,
    pub vol: gtk::Scale,
    pub mute: gtk::ToggleButton,
    pub volume_box: gtk::Box,
    pub timeline: gtk::Box,
    pub lyrics: gtk::ToggleButton,
    pub queue: gtk::ToggleButton,
    pub mini: gtk::Button,
    pub full: gtk::Button,
}

fn flat_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tip));
    b
}

fn flat_toggle(icon: &str, tip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::builder().icon_name(icon).tooltip_text(tip).valign(gtk::Align::Center).build();
    b.add_css_class("flat");
    b.add_css_class("circular");
    b
}

impl PlayerBar {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("player-bar");

        let art = art_image(BAR_ART, false);
        art.set_valign(gtk::Align::Center);
        let title = label("Not Playing", &["now-title"]);
        let sub = label("", &["dim-label", "caption", "now-sub"]);
        // max_width_chars(1) drops the labels' natural width, so the block is exactly
        // NOW_TEXT_WIDTH wide whatever the title / artist / album length.
        title.set_max_width_chars(1);
        sub.set_max_width_chars(1);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_valign(gtk::Align::Center);
        texts.set_size_request(NOW_TEXT_WIDTH, -1);
        texts.set_hexpand(false);
        texts.append(&title);
        texts.append(&sub);
        let star = flat_toggle("non-starred-symbolic", "Favourite");
        star.set_sensitive(false);
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        left.append(&art);
        left.append(&texts);
        left.append(&star);

        let shuffle = flat_toggle("media-playlist-shuffle-symbolic", "Shuffle");
        let prev = flat_button("media-skip-backward-symbolic", "Previous");
        let play = flat_button("media-playback-start-symbolic", "Play");
        play.remove_css_class("flat");
        play.add_css_class("suggested-action");
        play.set_size_request(40, 40);
        let next = flat_button("media-skip-forward-symbolic", "Next");
        let repeat = flat_button("media-playlist-repeat-symbolic", "Repeat: Off");
        repeat.add_css_class("dim-label");
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for w in [shuffle.upcast_ref::<gtk::Widget>(), prev.upcast_ref(), play.upcast_ref(), next.upcast_ref(), repeat.upcast_ref()] {
            controls.append(w);
        }

        let pos = label("0:00", &["dim-label", "numeric", "caption"]);
        let dur = label("0:00", &["dim-label", "numeric", "caption"]);
        // Fixed widths: 9:59 -> 10:00 must not nudge the seek bar.
        pos.set_width_chars(7);
        pos.set_xalign(1.0);
        dur.set_width_chars(7);
        dur.set_xalign(0.0);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
        no_fine_tune(&seek);
        seek.set_hexpand(true);
        seek.set_draw_value(false);
        seek.set_valign(gtk::Align::Center);
        let timeline = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        timeline.set_hexpand(true);
        timeline.append(&pos);
        timeline.append(&seek);
        timeline.append(&dur);

        let lyrics = flat_toggle("media-view-subtitles-symbolic", "Lyrics");
        let queue = flat_toggle("view-list-ordered-symbolic", "Queue");
        let mute = flat_toggle("audio-volume-high-symbolic", "Mute");
        let vol = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        vol.set_draw_value(false);
        vol.set_size_request(150, -1);
        vol.set_hexpand(false);
        vol.set_valign(gtk::Align::Center);
        vol.set_increments(0.02, 0.1);
        vol.set_value(1.0);
        no_fine_tune(&vol);
        let volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        volume_box.append(&mute);
        volume_box.append(&vol);
        let mini = flat_button("applejack-mini-player-symbolic", "Mini Player (Ctrl+Shift+M)");
        mini.set_action_name(Some("win.mini"));
        let full = flat_button("view-fullscreen-symbolic", "Full Screen (F11)");
        full.set_action_name(Some("win.fullscreen"));
        let right = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        right.append(&lyrics);
        right.append(&queue);
        right.append(&mini);
        right.append(&full);
        right.append(&volume_box);

        root.append(&left);
        root.append(&controls);
        root.append(&timeline);
        root.append(&right);

        Self { root, art, title, sub, star, prev, play, next, shuffle, repeat, seek, pos, dur, vol, mute, volume_box, timeline, lyrics, queue, mini, full }
    }

    /// None = unknown (not loaded yet): shown as not favourited.
    pub fn show_favourite(&self, on: Option<bool>) {
        let on = on.unwrap_or(false);
        if self.star.is_active() != on {
            self.star.set_active(on);
        }
        self.star.set_icon_name(if on { "starred-symbolic" } else { "non-starred-symbolic" });
        self.star.set_tooltip_text(Some(if on { "Remove from Favourite Songs" } else { "Add to Favourite Songs" }));
        if on {
            self.star.add_css_class("accent");
        } else {
            self.star.remove_css_class("accent");
        }
    }

    /// Speaker icon for the slider position / mute state.
    pub fn show_volume_icon(&self) {
        let s = self.vol.value();
        let (icon, tip) = if self.mute.is_active() || s <= 0.0 {
            ("audio-volume-muted-symbolic", "Unmute")
        } else if s < 0.34 {
            ("audio-volume-low-symbolic", "Mute")
        } else if s < 0.67 {
            ("audio-volume-medium-symbolic", "Mute")
        } else {
            ("audio-volume-high-symbolic", "Mute")
        };
        self.mute.set_icon_name(icon);
        self.mute.set_tooltip_text(Some(tip));
        if self.mute.is_active() {
            self.vol.add_css_class("dim-label");
        } else {
            self.vol.remove_css_class("dim-label");
        }
    }

    pub fn show_repeat(&self, mode: u8) {
        let (icon, tip) = match mode {
            1 => ("media-playlist-repeat-song-symbolic", "Repeat: One"),
            2 => ("media-playlist-repeat-symbolic", "Repeat: All"),
            _ => ("media-playlist-repeat-symbolic", "Repeat: Off"),
        };
        self.repeat.set_icon_name(icon);
        self.repeat.set_tooltip_text(Some(tip));
        if mode == 0 {
            self.repeat.add_css_class("dim-label");
            self.repeat.remove_css_class("accent");
        } else {
            self.repeat.remove_css_class("dim-label");
            self.repeat.add_css_class("accent");
        }
    }
}

// ---- side panel -----------------------------------------------------------------------

pub struct SidePanel {
    pub root: gtk::Box,
    pub stack: gtk::Stack,
    title: gtk::Label,
    pub close: gtk::Button,
    // queue
    pub clear: gtk::Button,
    pub autoplay: gtk::ToggleButton,
    queue_list: gtk::ListBox,
    queue_view: gtk::Stack,
    pub lyrics: Rc<LyricsView>,
}

impl SidePanel {
    pub fn new() -> Rc<Self> {
        let title = label("Queue", &["heading"]);
        title.set_hexpand(true);
        let close = flat_button("window-close-symbolic", "Close");
        let clear = gtk::Button::builder().label("Clear").css_classes(["flat"]).valign(gtk::Align::Center)
            .tooltip_text("Clear Up Next").build();
        let autoplay = gtk::ToggleButton::builder()
            .child(&adw::ButtonContent::builder().icon_name("media-playlist-repeat-symbolic").label("Autoplay").build())
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Autoplay: keep playing similar music when the queue ends")
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        header.set_margin_top(8);
        header.set_margin_bottom(4);
        header.set_margin_start(14);
        header.set_margin_end(8);
        header.append(&title);
        header.append(&autoplay);
        header.append(&clear);
        header.append(&close);

        let queue_list = gtk::ListBox::new();
        queue_list.add_css_class("navigation-sidebar");
        queue_list.set_selection_mode(gtk::SelectionMode::None);
        queue_list.connect_row_activated(|_, row| {
            let idx: i32 = row.widget_name().parse().unwrap_or(-1);
            if let (Some(app), true) = (app(), idx >= 0) {
                app.queue_jump(idx as u32);
            }
        });
        let queue_status = adw::StatusPage::builder()
            .icon_name("view-list-ordered-symbolic")
            .title("Queue Is Empty")
            .build();
        queue_status.add_css_class("compact");
        let queue_view = gtk::Stack::new();
        queue_view.add_named(&gtk::ScrolledWindow::builder().child(&queue_list).vexpand(true).build(), Some("list"));
        queue_view.add_named(&queue_status, Some("status"));
        queue_view.set_visible_child_name("status");

        let lyrics = LyricsView::new("lyric-line", || app().map(|a| a.position()).unwrap_or(0.0));

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_named(&queue_view, Some("queue"));
        stack.add_named(&lyrics.root, Some("lyrics"));
        stack.set_vexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("side-panel");
        root.append(&header);
        root.append(&stack);

        Rc::new(Self {
            root,
            stack,
            title,
            close,
            clear,
            autoplay,
            queue_list,
            queue_view,
            lyrics,
        })
    }

    pub fn page(&self) -> String {
        self.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default()
    }

    pub fn set_page(&self, page: &str) {
        self.stack.set_visible_child_name(page);
        self.title.set_label(if page == "lyrics" { "Lyrics" } else { "Up Next" });
        self.clear.set_visible(page == "queue");
        self.autoplay.set_visible(page == "queue");
    }

    pub fn show_queue(&self, art: &Rc<ArtLoader>, q: &Queue) {
        while let Some(row) = self.queue_list.row_at_index(0) {
            self.queue_list.remove(&row);
        }
        let start = q.position.max(0) as usize;
        let upcoming: Vec<(usize, &Item)> = q.items.iter().enumerate().skip(start).take(300).collect();
        self.clear.set_sensitive(upcoming.iter().any(|(i, it)| *i as i64 > q.position && !it.autoplay));
        if upcoming.is_empty() {
            self.queue_view.set_visible_child_name("status");
            return;
        }
        let mut autoplay_header = false;
        for (idx, it) in upcoming {
            if it.autoplay && !autoplay_header && idx as i64 != q.position {
                autoplay_header = true;
                let h = label("Autoplay", &["heading", "dim-label"]);
                h.set_margin_top(12);
                h.set_margin_start(8);
                let sub = label("Similar music plays when your queue ends", &["dim-label", "caption"]);
                sub.set_margin_start(8);
                sub.set_margin_bottom(4);
                let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
                b.append(&h);
                b.append(&sub);
                self.queue_list.append(&gtk::ListBoxRow::builder().child(&b).activatable(false).selectable(false).build());
            }
            let row = song_row(false);
            connect_row_play(&row);
            row.set_margin_top(3);
            row.set_margin_bottom(3);
            let p = row_parts(row.upcast_ref());
            fill_row(&p, it, RowMode::Art);
            p.sub.set_label(it.artist());
            load_art(art, &p.img, it.art(px(ROW_ART)), || true);
            let lbr = gtk::ListBoxRow::builder().child(&row).name(idx.to_string()).build();
            if idx as i64 == q.position {
                lbr.add_css_class("queue-current");
            }
            self.queue_list.append(&lbr);
        }
        self.queue_view.set_visible_child_name("list");
    }

    pub fn show_lyrics_loading(&self) {
        self.lyrics.show_loading();
    }

    pub fn show_lyrics(&self, lyrics: Option<Lyrics>) {
        self.lyrics.show(lyrics);
    }
}
