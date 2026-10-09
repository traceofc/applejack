// Library lists (songs, albums, artists, playlists, recently added) with a filter entry.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::{gio, glib};

use super::{
    app,
    widgets::{RowMode, card_factory, item_at, song_factory},
};
use crate::{art::ArtLoader, model::Item};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibKind {
    Recent,
    Artists,
    Albums,
    Songs,
    Playlists,
}

impl LibKind {
    pub const ALL: [LibKind; 5] = [LibKind::Playlists, LibKind::Recent, LibKind::Albums, LibKind::Artists, LibKind::Songs];

    /// Engine `library` kind and cache key suffix.
    pub fn key(self) -> &'static str {
        match self {
            LibKind::Recent => "recently-added",
            LibKind::Artists => "artists",
            LibKind::Albums => "albums",
            LibKind::Songs => "songs",
            LibKind::Playlists => "playlists",
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            LibKind::Recent => "Things you add to your library will show up here",
            LibKind::Artists => "No artists in your library",
            LibKind::Albums => "No albums in your library",
            LibKind::Songs => "No songs in your library",
            LibKind::Playlists => "You have no playlists",
        }
    }

    /// Server order is meaningful for these; the rest are sorted by name.
    pub fn keep_order(self) -> bool {
        matches!(self, LibKind::Recent | LibKind::Playlists)
    }
}

pub struct LibList {
    pub kind: LibKind,
    pub store: gio::ListStore,
    view: gtk::Stack,
    status: adw::StatusPage,
    pub root: gtk::Box,
}

impl LibList {
    pub fn new(kind: LibKind, art: &Rc<ArtLoader>) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query = Rc::new(RefCell::new(String::new()));
        let q = query.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            let q = q.borrow();
            if q.is_empty() {
                return true;
            }
            let obj = obj.downcast_ref::<glib::BoxedAnyObject>().unwrap();
            let it = obj.borrow::<Item>();
            [&it.title, &it.artist, &it.album]
                .iter()
                .any(|f| f.as_deref().is_some_and(|s| s.to_lowercase().contains(q.as_str())))
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        filtered.set_incremental(true);

        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("Filter"));
        entry.connect_search_changed(move |e| {
            *query.borrow_mut() = e.text().trim().to_lowercase();
            filter.changed(gtk::FilterChange::Different);
        });
        let entry_clamp = adw::Clamp::builder()
            .maximum_size(500)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .child(&entry)
            .build();

        let selection = gtk::NoSelection::new(Some(filtered.clone()));
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        match kind {
            LibKind::Songs | LibKind::Artists => {
                let mode = if kind == LibKind::Songs { RowMode::Art } else { RowMode::Avatar };
                let lv = gtk::ListView::new(Some(selection), Some(song_factory(art, mode)));
                lv.add_css_class("songs");
                lv.set_show_separators(kind == LibKind::Songs);
                let f = filtered.clone();
                lv.connect_activate(move |_, pos| {
                    let Some(app) = app() else { return };
                    if kind == LibKind::Songs {
                        app.play_from_model(&f, pos);
                    } else if let Some(it) = item_at(&f, pos) {
                        app.activate(&it, None);
                    }
                });
                lv.set_margin_start(14);
                lv.set_margin_end(14);
                scroller.set_child(Some(&lv));
            }
            _ => {
                let gv = gtk::GridView::new(Some(selection), Some(card_factory(art)));
                gv.add_css_class("cards");
                gv.set_max_columns(12);
                gv.set_min_columns(2);
                gv.set_single_click_activate(true);
                let f = filtered.clone();
                gv.connect_activate(move |_, pos| {
                    if let (Some(app), Some(it)) = (app(), item_at(&f, pos)) {
                        app.activate(&it, None);
                    }
                });
                scroller.set_child(Some(&gv));
            }
        }

        let status = adw::StatusPage::new();
        let view = gtk::Stack::new();
        view.add_named(&scroller, Some("content"));
        view.add_named(&status, Some("status"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&entry_clamp);
        root.append(&view);

        let this = Rc::new(Self { kind, store, view, status, root });
        this.show_status("content-loading-symbolic", "Loading…", "");
        this
    }

    pub fn show_status(&self, icon: &str, title: &str, desc: &str) {
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status.set_description(Some(desc).filter(|d| !d.is_empty()));
        self.view.set_visible_child_name("status");
    }

    pub fn set_items(&self, items: &[Item]) {
        let objs: Vec<glib::BoxedAnyObject> = items.iter().map(super::widgets::boxed).collect();
        self.store.splice(0, self.store.n_items(), &objs);
        if items.is_empty() {
            self.show_status("folder-music-symbolic", self.kind.empty_text(), "");
        } else {
            self.view.set_visible_child_name("content");
        }
    }

    pub fn is_empty(&self) -> bool {
        self.store.n_items() == 0
    }
}
