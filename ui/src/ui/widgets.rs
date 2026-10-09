// Reusable pieces: song rows, cards, shelves, section lists, artwork binding.
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib, pango};

use super::app;
use crate::{
    art::ArtLoader,
    model::{Item, Kind, Section, fmt_time},
};

pub const ROW_ART: i32 = 40;
pub const CARD_ART: i32 = 160;
pub const HERO_ART: i32 = 220;

/// Artwork request size for a widget size (HiDPI-friendly).
pub fn px(size: i32) -> u32 {
    (size * 2) as u32
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

pub fn wrap_label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = label(text, classes);
    l.set_ellipsize(pango::EllipsizeMode::None);
    l.set_wrap(true);
    l.set_wrap_mode(pango::WrapMode::WordChar);
    l
}

pub fn art_image(size: i32, round: bool) -> gtk::Image {
    let img = gtk::Image::new();
    img.set_pixel_size(size);
    img.set_size_request(size, size);
    img.add_css_class(if round { "art-round" } else if size >= CARD_ART { "art-lg" } else { "art" });
    img.set_overflow(gtk::Overflow::Hidden);
    img
}

pub fn boxed(item: &Item) -> glib::BoxedAnyObject {
    glib::BoxedAnyObject::new(item.clone())
}

pub fn item_at(model: &impl IsA<gio::ListModel>, pos: u32) -> Option<Item> {
    let obj = model.item(pos)?.downcast::<glib::BoxedAnyObject>().ok()?;
    let it = obj.borrow::<Item>().clone();
    Some(it)
}

pub fn items_of(model: &impl IsA<gio::ListModel>) -> Vec<Item> {
    (0..model.n_items()).filter_map(|i| item_at(model, i)).collect()
}

/// Loads artwork into `img`, as long as `still_valid` holds when it arrives
/// (list rows get recycled while images are in flight).
pub fn load_art(art: &Rc<ArtLoader>, img: &gtk::Image, url: Option<String>, still_valid: impl Fn() -> bool + 'static) {
    img.clear();
    let Some(url) = url else { return };
    let weak = img.downgrade();
    art.load(&url, move |tex| {
        if let Some(img) = weak.upgrade() {
            if still_valid() {
                img.set_paintable(Some(tex));
            }
        }
    });
}

pub fn song_subtitle(it: &Item) -> String {
    match (it.artist.as_deref(), it.album.as_deref()) {
        (Some(a), Some(b)) if !b.is_empty() => format!("{a} — {b}"),
        (Some(a), _) => a.to_owned(),
        (None, Some(b)) => b.to_owned(),
        _ => String::new(),
    }
}

pub fn card_subtitle(it: &Item) -> String {
    match it.kind() {
        Kind::Artist => "Artist".into(),
        Kind::Station if it.is_live => "Live".into(),
        Kind::Station => it.note.clone().unwrap_or_else(|| "Station".into()),
        Kind::Album => match (it.artist.as_deref(), it.year()) {
            (Some(a), _) => a.to_owned(),
            (None, Some(y)) => y.to_owned(),
            _ => String::new(),
        },
        _ => it.artist().to_owned(),
    }
}

/// Right-click / long-press opens the item menu. `current` returns the item under the
/// widget at the time of the click (rows are recycled).
pub fn attach_menu(widget: &impl IsA<gtk::Widget>, current: impl Fn() -> Option<Item> + 'static) {
    let current = Rc::new(current);
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let w = widget.upcast_ref::<gtk::Widget>().downgrade();
    let cur = current.clone();
    click.connect_pressed(move |g, _, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        if let (Some(w), Some(it), Some(app)) = (w.upgrade(), cur(), app()) {
            app.show_menu(&w, &it, Some((x, y)));
        }
    });
    widget.add_controller(click);
    let press = gtk::GestureLongPress::new();
    press.set_touch_only(true);
    let w = widget.upcast_ref::<gtk::Widget>().downgrade();
    press.connect_pressed(move |_, x, y| {
        if let (Some(w), Some(it), Some(app)) = (w.upgrade(), current(), app()) {
            app.show_menu(&w, &it, Some((x, y)));
        }
    });
    widget.add_controller(press);
}

// ---- song rows -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum RowMode {
    /// Artwork + "artist — album"
    Art,
    /// Track number + artist (album pages)
    Number,
    /// Round avatar, title only (artists)
    Avatar,
}

pub struct RowParts {
    pub row: gtk::Widget,
    pub num: gtk::Label,
    pub img: gtk::Image,
    /// Shown on hover in place of the track number / over the artwork.
    pub play: gtk::Button,
    pub title: gtk::Label,
    pub sub: gtk::Label,
    pub badge: gtk::Label,
    pub dur: gtk::Label,
}

/// Width of the track-number column; the number is centred in it so it sits under the
/// left edge of the album artwork above the list.
const NUM_WIDTH: i32 = 28;

pub fn song_row(round: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let num = label("", &["dim-label", "numeric"]);
    num.set_xalign(0.5);
    num.set_size_request(NUM_WIDTH, -1);
    let img = art_image(ROW_ART, round);
    let lead_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    lead_box.append(&num);
    lead_box.append(&img);
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.add_css_class("flat");
    play.add_css_class("circular");
    play.add_css_class("row-play");
    play.set_halign(gtk::Align::Center);
    play.set_valign(gtk::Align::Center);
    play.set_tooltip_text(Some("Play"));
    play.set_visible(false);
    let lead = gtk::Overlay::new();
    lead.set_child(Some(&lead_box));
    lead.add_overlay(&play);
    lead.set_valign(gtk::Align::Center);
    // Hover swaps the track number (or dims the artwork) for a play button.
    let motion = gtk::EventControllerMotion::new();
    let (r, n, pb) = (row.downgrade(), num.downgrade(), play.downgrade());
    motion.connect_enter(move |_, _, _| {
        let (Some(r), Some(n), Some(pb)) = (r.upgrade(), n.upgrade(), pb.upgrade()) else { return };
        if r.has_css_class("no-play") {
            return;
        }
        pb.set_visible(true);
        pb.set_css_classes(if n.is_visible() { &["flat", "circular", "row-play"] } else { &["circular", "row-play", "on-art"] });
        n.set_opacity(0.0);
    });
    let (n, pb) = (num.downgrade(), play.downgrade());
    motion.connect_leave(move |_| {
        if let (Some(n), Some(pb)) = (n.upgrade(), pb.upgrade()) {
            pb.set_visible(false);
            n.set_opacity(1.0);
        }
    });
    row.add_controller(motion);
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_hexpand(true);
    texts.set_valign(gtk::Align::Center);
    texts.append(&label("", &[]));
    texts.append(&label("", &["dim-label", "caption"]));
    let badge = gtk::Label::new(Some("E"));
    badge.add_css_class("explicit");
    badge.set_valign(gtk::Align::Center);
    badge.set_tooltip_text(Some("Explicit"));
    let dur = label("", &["dim-label", "numeric"]);
    row.append(&lead);
    row.append(&texts);
    row.append(&badge);
    row.append(&dur);
    row
}

pub fn row_parts(row: &gtk::Widget) -> RowParts {
    let lead = row.first_child().and_downcast::<gtk::Overlay>().unwrap();
    let lead_box = lead.child().unwrap();
    let num = lead_box.first_child().unwrap();
    let img = num.next_sibling().unwrap();
    let play = lead.last_child().unwrap();
    let texts = lead.next_sibling().unwrap();
    let badge = texts.next_sibling().unwrap();
    let dur = badge.next_sibling().unwrap();
    let title = texts.first_child().unwrap();
    let sub = title.next_sibling().unwrap();
    RowParts {
        row: row.clone(),
        num: num.downcast().unwrap(),
        img: img.downcast().unwrap(),
        play: play.downcast().unwrap(),
        title: title.downcast().unwrap(),
        sub: sub.downcast().unwrap(),
        badge: badge.downcast().unwrap(),
        dur: dur.downcast().unwrap(),
    }
}

pub fn fill_row(p: &RowParts, it: &Item, mode: RowMode) {
    p.title.set_label(it.title());
    p.num.set_visible(mode == RowMode::Number);
    p.img.set_visible(mode != RowMode::Number);
    p.badge.set_visible(it.explicit);
    // A recycled row may still be in its hover state.
    p.play.set_visible(false);
    p.num.set_opacity(1.0);
    if mode == RowMode::Avatar {
        p.row.add_css_class("no-play");
    } else {
        p.row.remove_css_class("no-play");
    }
    match mode {
        RowMode::Number => {
            p.num.set_label(&it.track_number.map(|n| n.to_string()).unwrap_or_default());
            p.sub.set_label(it.artist());
            p.sub.set_visible(true);
        }
        RowMode::Art => {
            p.sub.set_label(&song_subtitle(it));
            p.sub.set_visible(true);
        }
        RowMode::Avatar => p.sub.set_visible(false),
    }
    p.dur.set_label(&it.duration.filter(|d| *d > 0.0).map(fmt_time).unwrap_or_default());
}

fn bound_item(li: &gtk::ListItem) -> Option<Item> {
    let obj = li.item()?.downcast::<glib::BoxedAnyObject>().ok()?;
    let it = obj.borrow::<Item>().clone();
    Some(it)
}

/// The hover play button of a row outside a list view: activates the list box row or the
/// button the row sits in.
pub fn connect_row_play(row: &gtk::Box) {
    let p = row_parts(row.upcast_ref());
    p.play.connect_clicked(|b| {
        if let Some(r) = b.ancestor(gtk::ListBoxRow::static_type()).and_downcast::<gtk::ListBoxRow>() {
            r.emit_activate();
        } else if let Some(outer) = b.parent().and_then(|p| p.ancestor(gtk::Button::static_type())).and_downcast::<gtk::Button>() {
            outer.emit_clicked();
        }
    });
}

pub fn song_factory(art: &Rc<ArtLoader>, mode: RowMode) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let row = song_row(mode == RowMode::Avatar);
        let weak = li.downgrade();
        attach_menu(&row, move || weak.upgrade().and_then(|li| bound_item(&li)));
        // Single click on the hover button plays the row (the list itself needs a double click).
        let weak = li.downgrade();
        row_parts(row.upcast_ref()).play.connect_clicked(move |b| {
            if let Some(li) = weak.upgrade() {
                let _ = b.activate_action("list.activate-item", Some(&li.position().to_variant()));
            }
        });
        li.set_child(Some(&row));
    });
    let art = art.clone();
    f.connect_bind(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(child), Some(item)) = (li.child(), li.item()) else { return };
        let it = item.downcast_ref::<glib::BoxedAnyObject>().unwrap().borrow::<Item>().clone();
        let p = row_parts(&child);
        fill_row(&p, &it, mode);
        if mode != RowMode::Number {
            let li_weak = li.downgrade();
            load_art(&art, &p.img, it.art(px(ROW_ART)), move || {
                li_weak.upgrade().and_then(|l| l.item()).as_ref() == Some(&item)
            });
        }
    });
    f
}

// ---- cards -------------------------------------------------------------------------

pub fn card(size: i32, round: bool) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.set_size_request(size, -1);
    b.set_halign(gtk::Align::Center);
    b.append(&art_image(size, round));
    let title = label("", &["card-title"]);
    title.set_max_width_chars(1);
    title.set_hexpand(true);
    let sub = label("", &["dim-label", "caption"]);
    sub.set_max_width_chars(1);
    if round {
        title.set_xalign(0.5);
        sub.set_xalign(0.5);
    }
    b.append(&title);
    b.append(&sub);
    b
}

pub fn card_parts(card: &gtk::Widget) -> (gtk::Image, gtk::Label, gtk::Label) {
    let img = card.first_child().unwrap();
    let title = img.next_sibling().unwrap();
    let sub = title.next_sibling().unwrap();
    (img.downcast().unwrap(), title.downcast().unwrap(), sub.downcast().unwrap())
}

pub fn card_factory(art: &Rc<ArtLoader>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let c = card(CARD_ART, false);
        let weak = li.downgrade();
        attach_menu(&c, move || weak.upgrade().and_then(|li| bound_item(&li)));
        li.set_child(Some(&c));
    });
    let art = art.clone();
    f.connect_bind(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(child), Some(item)) = (li.child(), li.item()) else { return };
        let it = item.downcast_ref::<glib::BoxedAnyObject>().unwrap().borrow::<Item>().clone();
        let (img, title, sub) = card_parts(&child);
        title.set_label(it.title());
        sub.set_label(&card_subtitle(&it));
        let li_weak = li.downgrade();
        load_art(&art, &img, it.art(px(CARD_ART)), move || {
            li_weak.upgrade().and_then(|l| l.item()).as_ref() == Some(&item)
        });
    });
    f
}

/// A clickable card for a static item (shelves, search results).
pub fn card_button(art: &Rc<ArtLoader>, it: &Item, size: i32) -> gtk::Button {
    let round = it.kind() == Kind::Artist;
    let c = card(size, round);
    // Keep shelves packed to the left instead of spreading cards across a wide window.
    c.set_hexpand(false);
    let (img, title, sub) = card_parts(c.upcast_ref());
    title.set_label(it.title());
    sub.set_label(&card_subtitle(it));
    title.set_tooltip_text(Some(it.title()));
    load_art(art, &img, it.art(px(size)), || true);
    let b = gtk::Button::builder().child(&c).css_classes(["flat", "card-button"]).build();
    let item = it.clone();
    b.connect_clicked(move |_| {
        if let Some(app) = app() {
            app.activate(&item, None);
        }
    });
    let item = it.clone();
    attach_menu(&b, move || Some(item.clone()));
    b
}

// ---- sections ----------------------------------------------------------------------

pub fn heading(title: &str) -> gtk::Label {
    let l = label(title, &["title-3"]);
    l.set_margin_top(18);
    l.set_margin_bottom(4);
    l.set_margin_start(6);
    l
}

/// Horizontal, scrollable row of cards.
pub fn shelf(art: &Rc<ArtLoader>, items: &[Item], size: i32) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    // Room for the overlay scrollbar under the cards instead of on top of the titles.
    row.set_margin_bottom(14);
    for it in items {
        row.append(&card_button(art, it, size));
    }
    gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&row)
        .css_classes(["shelf"])
        .build()
        .upcast()
}

/// Songs laid out in up to three columns; clicking one plays the section from there.
pub fn song_grid(art: &Rc<ArtLoader>, items: &[Item]) -> gtk::Widget {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(3);
    flow.set_min_children_per_line(1);
    flow.set_column_spacing(6);
    flow.add_css_class("song-grid");
    let items: Rc<Vec<Item>> = Rc::new(items.to_vec());
    for (idx, it) in items.iter().enumerate() {
        let row = song_row(false);
        connect_row_play(&row);
        let p = row_parts(row.upcast_ref());
        fill_row(&p, it, RowMode::Art);
        p.dur.set_visible(false);
        load_art(art, &p.img, it.art(px(ROW_ART)), || true);
        let b = gtk::Button::builder().child(&row).css_classes(["flat", "song-button"]).build();
        b.set_size_request(260, -1);
        let list = items.clone();
        b.connect_clicked(move |_| {
            if let Some(app) = app() {
                app.activate(&list[idx], Some((&list, idx)));
            }
        });
        let item = it.clone();
        attach_menu(&b, move || Some(item.clone()));
        flow.append(&b);
    }
    flow.upcast()
}

/// Column of titled sections: songs as a grid, everything else as card shelves.
pub fn sections_view(art: &Rc<ArtLoader>, sections: &[Section]) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for s in sections {
        if s.items.is_empty() {
            continue;
        }
        col.append(&heading(&s.title));
        let all_songs = s.items.iter().all(|it| it.kind() == Kind::Song);
        if s.songs || all_songs {
            col.append(&song_grid(art, &s.items));
        } else {
            col.append(&shelf(art, &s.items, if s.hero { HERO_ART } else { CARD_ART }));
        }
    }
    col
}

/// Scrollable page body: full width, left-aligned with the same gutter as the track lists.
pub fn page_scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let child = child.as_ref();
    child.set_margin_start(18);
    child.set_margin_end(18);
    child.set_margin_bottom(24);
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

/// GTK enters a slow "fine-tune" mode when a slider is long-pressed (the knob grows and
/// barely moves); players don't want that.
pub fn no_fine_tune(scale: &gtk::Scale) {
    let controllers = scale.observe_controllers();
    let long: Vec<gtk::EventController> = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i).and_downcast::<gtk::EventController>())
        .filter(|c| c.is::<gtk::GestureLongPress>())
        .collect();
    for c in long {
        scale.remove_controller(&c);
    }
}

pub fn pill(icon: &str, text: &str, suggested: bool) -> gtk::Button {
    let b = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().icon_name(icon).label(text).build())
        .css_classes(["pill"])
        .build();
    if suggested {
        b.add_css_class("suggested-action");
    }
    b
}

/// Strips the light HTML Apple puts in bios / notes.
pub fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ")
}
