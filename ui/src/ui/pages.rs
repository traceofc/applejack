// Pages built from engine data: Home / Browse / Radio shelves, search, artist,
// album / playlist. Everything is shown from the SQLite cache first and refreshed
// in the background (stale-while-revalidate).
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{
    App,
    widgets::{
        CARD_ART, RowMode, art_image, boxed, heading, items_of, label, load_art, page_scroller, pill,
        plain_text, px, sections_view, shelf, song_factory, wrap_label,
    },
};
use crate::model::{
    ArtistPage, ContainerPage, Item, Kind, SearchResults, Section, Suggestions, count,
};

const RECENT_TERMS: usize = 10;
const RECENT_ITEMS: usize = 12;

/// Wrapping row of small pill buttons.
fn chip_box() -> gtk::FlowBox {
    let f = gtk::FlowBox::new();
    f.set_selection_mode(gtk::SelectionMode::None);
    f.set_max_children_per_line(30);
    f.set_column_spacing(6);
    f.set_row_spacing(6);
    f.set_homogeneous(false);
    f.set_valign(gtk::Align::Start);
    f
}

/// Search-term chips; clicking one searches for it.
fn fill_chips(f: &gtk::FlowBox, terms: &[String], entry: &gtk::SearchEntry) {
    f.remove_all();
    for t in terms {
        let b = gtk::Button::builder().label(t.as_str()).css_classes(["chip"]).build();
        let (entry, t) = (entry.clone(), t.clone());
        b.connect_clicked(move |_| {
            entry.set_text(&t);
            entry.set_position(-1);
        });
        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(&b));
        child.set_focusable(false);
        f.append(&child);
    }
}

/// Stack with "loading", "error" and "content" children.
pub struct Loadable {
    pub root: gtk::Stack,
    error: adw::StatusPage,
    content: gtk::Box,
}

impl Loadable {
    pub fn new() -> Rc<Self> {
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        let error = adw::StatusPage::builder().icon_name("dialog-warning-symbolic").build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_vexpand(true);
        let root = gtk::Stack::new();
        root.set_transition_type(gtk::StackTransitionType::Crossfade);
        root.add_named(&spinner, Some("loading"));
        root.add_named(&error, Some("error"));
        root.add_named(&content, Some("content"));
        root.set_visible_child_name("loading");
        root.set_vexpand(true);
        Rc::new(Self { root, error, content })
    }

    pub fn set_content(&self, w: &impl IsA<gtk::Widget>) {
        while let Some(c) = self.content.first_child() {
            self.content.remove(&c);
        }
        self.content.append(w);
        self.root.set_visible_child_name("content");
    }

    pub fn set_error(&self, title: &str, desc: &str) {
        self.error.set_title(title);
        self.error.set_description(Some(desc).filter(|d| !d.is_empty()));
        self.root.set_visible_child_name("error");
    }

    pub fn has_content(&self) -> bool {
        self.root.visible_child_name().as_deref() == Some("content")
    }
}

impl App {
    /// Renders cached data at once, then fetches fresh data (unless the cache is
    /// younger than `max_age` seconds) and re-renders only if it changed.
    pub fn load_swr<T: Serialize + DeserializeOwned + 'static>(
        self: &Rc<Self>,
        key: String,
        max_age: u64,
        method: &str,
        params: Value,
        render: impl Fn(&T) + 'static,
        fail: impl Fn(&str) + 'static,
    ) {
        let cached_json = self.cache.get::<Value>(&key);
        let mut fresh_enough = false;
        if let Some(v) = &cached_json {
            if let Ok(t) = serde_json::from_value::<T>(v.clone()) {
                render(&t);
                fresh_enough = self.cache.age(&key).is_some_and(|a| a < max_age);
            }
        }
        if fresh_enough {
            return;
        }
        let app = self.clone();
        let fut = self.engine.call(method, params);
        let had_cache = cached_json.is_some();
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(v) => {
                    app.cache.put(&key, &v);
                    if cached_json.as_ref() == Some(&v) {
                        return;
                    }
                    match serde_json::from_value::<T>(v) {
                        Ok(t) => render(&t),
                        Err(e) => fail(&e.to_string()),
                    }
                }
                Err(e) if !had_cache => fail(&e),
                Err(e) => eprintln!("refresh {key}: {e}"),
            }
        });
    }

    // ---- Home / Browse / Radio ----

    pub fn sections_page(self: &Rc<Self>, method: &'static str, title: &'static str, subtitle: &'static str) -> gtk::Widget {
        let page = Loadable::new();
        let app = self.clone();
        let p = page.clone();
        let render = move |sections: &Vec<Section>| {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let t = label(title, &["title-1"]);
            t.set_margin_top(18);
            t.set_margin_start(6);
            col.append(&t);
            if !subtitle.is_empty() {
                let s = label(subtitle, &["dim-label"]);
                s.set_margin_start(6);
                col.append(&s);
            }
            col.append(&sections_view(&app.art, sections));
            p.set_content(&page_scroller(&col));
        };
        let p = page.clone();
        let fail = move |e: &str| p.set_error("Couldn’t Load", e);
        self.load_swr(format!("page:{method}"), 15 * 60, method, json!({}), render, fail);
        page.root.clone().upcast()
    }

    // ---- Search ----

    pub fn build_search(self: &Rc<Self>) -> gtk::Widget {
        let entry = self.search_entry.clone();
        entry.set_placeholder_text(Some("Artists, songs, albums, playlists, stations"));
        entry.set_search_delay(250);
        entry.set_hexpand(true);
        let entry_clamp = adw::Clamp::builder()
            .maximum_size(640)
            .margin_top(12)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .child(&entry)
            .build();
        let results = Loadable::new();
        // Suggested search terms while typing.
        let suggest = chip_box();
        suggest.set_margin_start(24);
        suggest.set_margin_end(24);
        suggest.set_margin_bottom(4);
        suggest.set_visible(false);
        let results_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        results_box.append(&suggest);
        results_box.append(&results.root);
        let view = gtk::Stack::new();
        view.add_named(&page_scroller(&self.search_home), Some("idle"));
        view.add_named(&results_box, Some("results"));
        view.set_visible_child_name("idle");
        view.set_vexpand(true);

        let serial = Rc::new(RefCell::new(0u64));
        let app = self.clone();
        let view_c = view.clone();
        entry.connect_search_changed(move |e| {
            let view = &view_c;
            let term = e.text().trim().to_owned();
            let my = {
                let mut s = serial.borrow_mut();
                *s += 1;
                *s
            };
            if term.is_empty() {
                suggest.set_visible(false);
                app.refresh_search_home();
                view.set_visible_child_name("idle");
                return;
            }
            view.set_visible_child_name("results");
            if !results.has_content() {
                results.root.set_visible_child_name("loading");
            }
            let (app2, results2, serial2) = (app.clone(), results.clone(), serial.clone());
            let render = move |r: &SearchResults| {
                if *serial2.borrow() != my {
                    return; // a newer search is on its way
                }
                app2.render_search(&results2, r);
                // A search that stays on screen for a moment counts as a recent search.
                let (app3, serial3) = (app2.clone(), serial2.clone());
                glib::timeout_add_local_once(std::time::Duration::from_millis(2500), move || {
                    if *serial3.borrow() == my && app3.in_search() {
                        app3.remember_search(None);
                    }
                });
            };
            let (results3, serial3) = (results.clone(), serial.clone());
            let fail = move |e: &str| {
                if *serial3.borrow() == my {
                    results3.set_error("Search Failed", e);
                }
            };
            let key = format!("search:{}", term.to_lowercase());
            app.load_swr(key, 30 * 60, "searchAll", json!({ "term": term, "limit": 15 }), render, fail);

            let (suggest, serial4, entry) = (suggest.clone(), serial.clone(), e.clone());
            let fut = app.engine.call("searchSuggestions", json!({ "term": term }));
            glib::spawn_future_local(async move {
                let Ok(sg) = fut.await.and_then(|v| serde_json::from_value::<Suggestions>(v).map_err(|e| e.to_string())) else { return };
                if *serial4.borrow() != my {
                    return;
                }
                let lower = term.to_lowercase();
                let terms: Vec<String> = sg.terms.into_iter().filter(|t| t.to_lowercase() != lower).take(8).collect();
                fill_chips(&suggest, &terms, &entry);
                suggest.set_visible(!terms.is_empty());
            });
        });
        let app = self.clone();
        entry.connect_activate(move |_| app.remember_search(None));

        self.refresh_search_home();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&entry_clamp);
        root.append(&view);
        root.upcast()
    }

    pub fn in_search(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("search")
            && self.nav.visible_page().as_ref() == Some(&self.root_page)
    }

    /// Records the current search term, and what was opened from its results.
    pub fn remember_search(&self, opened: Option<&Item>) {
        let term = self.search_entry.text().trim().to_owned();
        if !term.is_empty() {
            let mut terms = self.cache.get::<Vec<String>>("search:recent-terms").unwrap_or_default();
            let lower = term.to_lowercase();
            terms.retain(|t| t.to_lowercase() != lower);
            terms.insert(0, term);
            terms.truncate(RECENT_TERMS);
            self.cache.put("search:recent-terms", &terms);
        }
        if let Some(it) = opened {
            let mut items = self.cache.get::<Vec<Item>>("search:recent-items").unwrap_or_default();
            items.retain(|x| x.id != it.id);
            items.insert(0, it.clone());
            items.truncate(RECENT_ITEMS);
            self.cache.put("search:recent-items", &items);
        }
    }

    /// The empty search page: recent searches, recently viewed results, recently played.
    pub fn refresh_search_home(self: &Rc<Self>) {
        let home = &self.search_home;
        while let Some(c) = home.first_child() {
            home.remove(&c);
        }
        let terms = self.cache.get::<Vec<String>>("search:recent-terms").unwrap_or_default();
        let items = self.cache.get::<Vec<Item>>("search:recent-items").unwrap_or_default();
        if terms.is_empty() && items.is_empty() {
            let idle = adw::StatusPage::builder()
                .icon_name("system-search-symbolic")
                .title("Search the Catalog")
                .description("Find an artist, song, album, playlist or station")
                .build();
            idle.add_css_class("compact");
            home.append(&idle);
        }
        if !terms.is_empty() {
            home.append(&self.clearable_heading("Recent Searches", "search:recent-terms"));
            let chips = chip_box();
            chips.set_margin_start(6);
            fill_chips(&chips, &terms, &self.search_entry);
            home.append(&chips);
        }
        if !items.is_empty() {
            home.append(&self.clearable_heading("Recently Viewed", "search:recent-items"));
            home.append(&shelf(&self.art, &items, CARD_ART));
        }
        // Suggestions: what was played lately (kept fresh for 10 minutes).
        let played = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&played);
        let art = self.art.clone();
        let pw = played.downgrade();
        let render = move |list: &Vec<Item>| {
            let Some(p) = pw.upgrade() else { return };
            while let Some(c) = p.first_child() {
                p.remove(&c);
            }
            if !list.is_empty() {
                p.append(&heading("Recently Played"));
                p.append(&shelf(&art, list, CARD_ART));
            }
        };
        self.load_swr("search:recent-played".into(), 10 * 60, "recentPlayed", json!({ "limit": 10 }), render, |_| {});
    }

    fn clearable_heading(self: &Rc<Self>, title: &str, key: &'static str) -> gtk::Box {
        let h = heading(title);
        h.set_hexpand(true);
        let clear = gtk::Button::builder().label("Clear").css_classes(["flat"]).valign(gtk::Align::End).build();
        let app = self.clone();
        clear.connect_clicked(move |_| {
            app.cache.put(key, &Value::Array(Vec::new()));
            app.refresh_search_home();
        });
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        b.append(&h);
        b.append(&clear);
        b
    }

    fn render_search(self: &Rc<Self>, page: &Loadable, r: &SearchResults) {
        let sections = vec![
            Section { title: "Songs".into(), items: r.songs.clone(), songs: true, ..Default::default() },
            Section { title: "Artists".into(), items: r.artists.clone(), ..Default::default() },
            Section { title: "Albums".into(), items: r.albums.clone(), ..Default::default() },
            Section { title: "Playlists".into(), items: r.playlists.clone(), ..Default::default() },
            Section { title: "Stations".into(), items: r.stations.clone(), ..Default::default() },
        ];
        if sections.iter().all(|s| s.items.is_empty()) {
            page.set_error("No Results", "Try a different search");
            return;
        }
        page.set_content(&page_scroller(&sections_view(&self.art, &sections)));
        self.prefetch_containers(r.albums.iter().chain(r.playlists.iter()).take(6).cloned().collect());
    }

    // ---- Artist ----

    pub fn open_artist(self: &Rc<Self>, id: &str, name: Option<&str>) {
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        let nav_page = adw::NavigationPage::new(&tv, name.unwrap_or("Artist"));
        let np = nav_page.downgrade();
        let body = self.artist_body(id, move |t| {
            if let Some(np) = np.upgrade() {
                np.set_title(t);
            }
        });
        tv.set_content(Some(&body));
        self.push(&nav_page);
    }

    /// Artist page body; `set_title` gets the artist's name once it is known.
    pub fn artist_body(self: &Rc<Self>, id: &str, set_title: impl Fn(&str) + 'static) -> gtk::Widget {
        let page = Loadable::new();
        let app = self.clone();
        let p = page.clone();
        let render = move |a: &ArtistPage| {
            if let Some(t) = a.artist.title.as_deref() {
                set_title(t);
            }
            p.set_content(&page_scroller(&app.artist_view(a)));
        };
        let p = page.clone();
        let fail = move |e: &str| p.set_error("Couldn’t Open Artist", e);
        self.load_swr(format!("artist:{id}"), 60 * 60, "artist", json!({ "id": id }), render, fail);
        page.root.clone().upcast()
    }

    fn artist_view(self: &Rc<Self>, a: &ArtistPage) -> gtk::Box {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let img = art_image(180, true);
        load_art(&self.art, &img, a.artist.art(px(180)), || true);
        let name = wrap_label(a.artist.title(), &["title-1"]);
        let genre = label(a.artist.genre.as_deref().unwrap_or(""), &["dim-label"]);
        let top: Vec<Item> = a
            .sections
            .iter()
            .find(|s| s.key.as_deref() == Some("top-songs"))
            .map(|s| s.items.clone())
            .unwrap_or_default();
        let play = pill("media-playback-start-symbolic", "Play", true);
        let shuffle = pill("media-playlist-shuffle-symbolic", "Shuffle", false);
        play.set_sensitive(!top.is_empty());
        shuffle.set_sensitive(!top.is_empty());
        for (b, sh) in [(&play, false), (&shuffle, true)] {
            let app = self.clone();
            let ids: Vec<String> = top.iter().map(Item::song_play_id).collect();
            b.connect_clicked(move |_| {
                app.set_shuffle(sh);
                app.play_songs(ids.clone(), 0);
            });
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_margin_top(10);
        buttons.append(&play);
        buttons.append(&shuffle);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 4);
        texts.set_valign(gtk::Align::Center);
        texts.append(&name);
        texts.append(&genre);
        texts.append(&buttons);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        header.set_margin_top(18);
        header.set_margin_start(6);
        header.append(&img);
        header.append(&texts);
        col.append(&header);

        col.append(&sections_view(&self.art, &a.sections));

        if let Some(bio) = a.bio.as_deref().map(plain_text).filter(|b| !b.trim().is_empty()) {
            col.append(&heading("About"));
            let text = wrap_label(bio.trim(), &["body"]);
            text.set_lines(5);
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.set_margin_start(6);
            text.set_margin_end(6);
            let more = gtk::Button::builder().label("More").css_classes(["flat"]).halign(gtk::Align::Start).build();
            let t = text.clone();
            more.connect_clicked(move |b| {
                let expanded = t.lines() < 0;
                t.set_lines(if expanded { 5 } else { -1 });
                t.set_ellipsize(if expanded { gtk::pango::EllipsizeMode::End } else { gtk::pango::EllipsizeMode::None });
                b.set_label(if expanded { "More" } else { "Less" });
            });
            col.append(&text);
            col.append(&more);
        }
        col
    }

    // ---- Album / playlist ----

    pub fn open_container(self: &Rc<Self>, item: Item, ctype: &'static str) {
        let (body, title) = self.container_view(item.clone(), ctype);
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&body));
        let page = adw::NavigationPage::new(&tv, &title);
        self.push(&page);
    }

    /// Album / playlist body: header with artwork and actions, then the track list.
    pub fn container_view(self: &Rc<Self>, item: Item, ctype: &'static str) -> (gtk::Widget, String) {
        let tracks = gio::ListStore::new::<glib::BoxedAnyObject>();
        let numbered = ctype == "album";

        // Same size as the grid cards, so the decoded texture is reused.
        let img = art_image(CARD_ART, false);
        img.set_valign(gtk::Align::Start);
        load_art(&self.art, &img, item.art(px(CARD_ART)), || true);
        let kind = label(if numbered { "ALBUM" } else { "PLAYLIST" }, &["caption-heading", "dim-label"]);
        let title = wrap_label(item.title(), &["title-2"]);
        let artist = gtk::Button::builder()
            .label(item.artist())
            .css_classes(["flat", "artist-link"])
            .halign(gtk::Align::Start)
            .visible(!item.artist().is_empty())
            .sensitive(false)
            .build();
        let info = label("", &["dim-label", "caption"]);
        let desc = wrap_label("", &["dim-label"]);
        desc.set_lines(2);
        desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
        desc.set_visible(false);
        let play = pill("media-playback-start-symbolic", "Play", true);
        let shuffle = pill("media-playlist-shuffle-symbolic", "Shuffle", false);
        let add = pill("list-add-symbolic", "Add to Library", false);
        add.set_visible(false);
        let edit = pill("document-edit-symbolic", "Edit", false);
        edit.set_visible(false);
        let more = gtk::Button::builder()
            .icon_name("view-more-symbolic")
            .css_classes(["circular", "flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("More")
            .build();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_margin_top(8);
        for w in [play.upcast_ref::<gtk::Widget>(), shuffle.upcast_ref(), add.upcast_ref(), edit.upcast_ref(), more.upcast_ref()] {
            buttons.append(w);
        }
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_valign(gtk::Align::Center);
        texts.set_hexpand(true);
        for w in [kind.upcast_ref::<gtk::Widget>(), title.upcast_ref(), artist.upcast_ref(), info.upcast_ref(), desc.upcast_ref(), buttons.upcast_ref()] {
            texts.append(w);
        }
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        // Left-aligned and full width; 24 px matches the track rows (14 px list margin + 10 px row padding).
        header.set_margin_top(12);
        header.set_margin_bottom(6);
        header.set_margin_start(24);
        header.set_margin_end(24);
        header.append(&img);
        header.append(&texts);

        let lv = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(tracks.clone()))),
            Some(song_factory(&self.art, if numbered { RowMode::Number } else { RowMode::Art })),
        );
        lv.add_css_class("songs");
        lv.set_show_separators(true);
        lv.set_margin_start(14);
        lv.set_margin_end(14);
        let footer = label("", &["dim-label", "caption"]);
        footer.set_margin_start(24);
        footer.set_margin_bottom(6);
        footer.set_visible(false);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&lv)
            .build();
        let loading = Loadable::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&header);
        body.append(&loading.root);
        loading.set_content(&scroller);
        loading.root.set_visible_child_name("loading");
        body.append(&footer);

        let artist_id: Rc<RefCell<Option<String>>> = Default::default();
        // Name and description as shown, for the edit dialog.
        let texts_now: Rc<RefCell<(String, String)>> = Rc::new(RefCell::new((item.title().to_owned(), String::new())));
        let render = {
            let (tracks, info, desc, footer, add, artist, artist_id, loading) =
                (tracks.clone(), info.clone(), desc.clone(), footer.clone(), add.clone(), artist.clone(), artist_id.clone(), loading.clone());
            let (edit, texts_now) = (edit.clone(), texts_now.clone());
            let item = item.clone();
            let (title_l, img, art) = (title.clone(), img.clone(), self.art.clone());
            move |c: &ContainerPage| {
                // Opened from a bare id: take name, artist and artwork from the response.
                if item.title.is_none() {
                    title_l.set_label(c.info.title());
                    // The page may not be pushed yet when rendering from cache.
                    let (l, t) = (title_l.clone(), c.info.title().to_owned());
                    glib::idle_add_local_once(move || {
                        if let Some(np) = l.ancestor(adw::NavigationPage::static_type()).and_downcast::<adw::NavigationPage>() {
                            if np.tag().as_deref() != Some("root") {
                                np.set_title(&t);
                            }
                        }
                    });
                    artist.set_label(c.info.artist());
                    artist.set_visible(!c.info.artist().is_empty());
                    load_art(&art, &img, c.info.art(px(CARD_ART)), || true);
                }
                let objs: Vec<glib::BoxedAnyObject> = c.tracks.iter().map(boxed).collect();
                tracks.splice(0, tracks.n_items(), &objs);
                let total: f64 = c.tracks.iter().filter_map(|t| t.duration).sum();
                let n = c.tracks.len();
                let mut parts: Vec<String> = Vec::new();
                if let Some(g) = c.info.genre.as_deref() {
                    parts.push(g.to_owned());
                }
                if let Some(y) = c.info.year().or(item.year()) {
                    parts.push(y.to_owned());
                }
                parts.push(format!("{}, {} min", count(n, "song"), (total / 60.0).round() as u64));
                info.set_label(&parts.join(" · "));
                if let Some(d) = c.description.as_deref().map(plain_text).filter(|d| !d.trim().is_empty()) {
                    desc.set_label(d.trim());
                    desc.set_tooltip_text(Some(d.trim()));
                    desc.set_visible(true);
                }
                let mut foot = Vec::new();
                if let Some(d) = c.info.release_date.as_deref() {
                    foot.push(format!("Released {d}"));
                }
                if let Some(cp) = c.copyright.as_deref() {
                    foot.push(cp.to_owned());
                }
                footer.set_label(&foot.join("\n"));
                footer.set_visible(!foot.is_empty());
                add.set_visible(!c.in_library && c.catalog_id.is_some());
                // The user's own library playlists can be edited.
                edit.set_visible(ctype == "playlist" && item.id.starts_with("p.") && (c.info.can_edit || item.can_edit));
                *texts_now.borrow_mut() = (
                    c.info.title.clone().unwrap_or_else(|| item.title().to_owned()),
                    c.description.as_deref().map(plain_text).unwrap_or_default().trim().to_owned(),
                );
                *artist_id.borrow_mut() = c.artist_id.clone();
                artist.set_sensitive(c.artist_id.is_some());
                if n == 0 {
                    loading.set_error("No Tracks", "");
                } else {
                    loading.root.set_visible_child_name("content");
                }
            }
        };
        let fail = {
            let loading = loading.clone();
            move |e: &str| loading.set_error("Couldn’t Load", e)
        };
        self.load_swr(
            format!("container:{ctype}:{}", item.id),
            10 * 60,
            "container",
            json!({ "id": item.id, "type": ctype }),
            render,
            fail,
        );

        let app = self.clone();
        let aid = artist_id.clone();
        let aname = item.artist().to_owned();
        artist.connect_clicked(move |_| {
            if let Some(id) = aid.borrow().clone() {
                app.open_artist(&id, Some(&aname));
            }
        });
        for (button, sh) in [(&play, false), (&shuffle, true)] {
            let (app, id, tracks) = (self.clone(), item.id.clone(), tracks.clone());
            button.connect_clicked(move |_| {
                app.set_shuffle(sh);
                app.play_container(&id, ctype, 0, items_of(&tracks));
            });
        }
        let (app, it) = (self.clone(), item.clone());
        add.connect_clicked(move |b| {
            b.set_sensitive(false);
            app.add_to_library(&it);
        });
        let (app, it) = (self.clone(), item.clone());
        more.connect_clicked(move |b| app.show_menu(b.upcast_ref(), &it, None));
        let (app, id, t) = (self.clone(), item.id.clone(), tracks.clone());
        let (title_l, desc_l) = (title.clone(), desc.clone());
        edit.connect_clicked(move |_| {
            let (name, description) = texts_now.borrow().clone();
            let (t2, title_l, desc_l, texts_now) = (t.clone(), title_l.clone(), desc_l.clone(), texts_now.clone());
            app.edit_playlist(&id, &name, &description, items_of(&t), move |name, description, items| {
                // Show the result right away; Apple's copy replaces it on the next load.
                title_l.set_label(name);
                desc_l.set_label(description);
                desc_l.set_visible(!description.is_empty());
                let objs: Vec<glib::BoxedAnyObject> = items.iter().map(boxed).collect();
                t2.splice(0, t2.n_items(), &objs);
                *texts_now.borrow_mut() = (name.to_owned(), description.to_owned());
                if let Some(np) = title_l.ancestor(adw::NavigationPage::static_type()).and_downcast::<adw::NavigationPage>() {
                    if np.tag().as_deref() != Some("root") {
                        np.set_title(name);
                    }
                }
            });
        });
        let (app, id, t) = (self.clone(), item.id.clone(), tracks.clone());
        lv.connect_activate(move |_, pos| {
            app.set_shuffle(false);
            app.play_container(&id, ctype, pos, items_of(&t));
        });

        (body.upcast(), item.title().to_owned())
    }

    /// Warms the cache for albums / playlists the user is likely to open next.
    pub fn prefetch_containers(self: &Rc<Self>, items: Vec<Item>) {
        let app = self.clone();
        glib::spawn_future_local(async move {
            let mut running = Vec::new();
            for it in items {
                let ctype = match it.kind() {
                    Kind::Album => "album",
                    Kind::Playlist => "playlist",
                    _ => continue,
                };
                let key = format!("container:{ctype}:{}", it.id);
                if app.cache.age(&key).is_some_and(|a| a < 30 * 60) {
                    continue;
                }
                if let Some(u) = it.art(px(CARD_ART)) {
                    app.art.prefetch(&u);
                }
                running.push((key, app.engine.call("container", json!({ "id": it.id, "type": ctype }))));
                if running.len() == 3 {
                    for (key, fut) in running.drain(..) {
                        if let Ok(v) = fut.await {
                            app.cache.put(&key, &v);
                        }
                    }
                }
            }
            for (key, fut) in running {
                if let Ok(v) = fut.await {
                    app.cache.put(&key, &v);
                }
            }
        });
    }
}
