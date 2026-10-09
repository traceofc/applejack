// Main window: sidebar (Discover / Library / Playlists), content navigation,
// now-playing bar at the bottom, queue / lyrics side panel. All data comes from the
// engine over JSON-RPC and is cached in SQLite so pages show up instantly.
mod desktop;
mod eq;
mod fullscreen;
mod library;
mod lyrics;
mod mini;
mod mpris;
mod pages;
mod player;
mod playlists;
mod prefs;
mod theme;
mod tray;
mod widgets;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{Value, json};

use crate::{
    art::ArtLoader,
    cache::Cache,
    discord,
    engine::{Engine, find_engine_dir},
    model::{Item, Kind, LibraryPage, Lyrics, Queue, SongLinks, Status, fmt_time},
    scrobble,
};
use fullscreen::FullScreen;
use mini::Mini;
use library::{LibKind, LibList};
use player::{BAR_ART, PlayerBar, SidePanel, slider_to_volume, volume_to_slider};
use widgets::{item_at, load_art, px, song_subtitle};

const CSS: &str = r#"
.art { border-radius: 6px; background-color: alpha(currentColor, 0.08); }
.art-lg { border-radius: 10px; background-color: alpha(currentColor, 0.08); }
.art-round { border-radius: 9999px; background-color: alpha(currentColor, 0.08); }
.player-bar { padding: 8px 12px; }
.now-title { font-weight: bold; }
.now-clickable:hover { text-decoration: underline; }
listview.songs { background: none; }
listview.songs > row { padding: 4px 10px; border-radius: 8px; }
listview.songs.separators > row:not(:last-child) { border-bottom: 1px solid alpha(currentColor, 0.07); }
.row-play { min-width: 28px; min-height: 28px; padding: 0; }
.row-play.on-art { background-color: alpha(black, 0.55); color: white; }
.row-play.on-art:hover { background-color: alpha(black, 0.7); }
.chip { padding: 4px 12px; border-radius: 9999px; }
gridview.cards { background: none; }
gridview.cards > child { padding: 10px; border-radius: 12px; }
.card-button { padding: 8px; border-radius: 12px; }
.song-button { padding: 4px 8px; border-radius: 8px; }
.song-grid > flowboxchild { padding: 0; }
.card-title { font-weight: bold; }
.explicit { font-size: 0.7em; font-weight: bold; padding: 0 4px; border-radius: 3px;
            background-color: alpha(currentColor, 0.15); }
.artist-link { padding: 0 4px; margin-left: -4px; font-size: 1.15em; color: @accent_color; min-height: 0; }
.side-panel { background-color: @sidebar_bg_color; }
.queue-current { background-color: alpha(@accent_bg_color, 0.15); }
.lyric-line { font-size: 1.35em; font-weight: 800; }
.lyric-big { font-size: 2.15em; font-weight: 800; }
.lyric-line.lyric-bg { font-size: 1em; font-weight: 700; opacity: 0.8; }
.lyric-big.lyric-bg { font-size: 1.45em; font-weight: 700; opacity: 0.8; }
.lyric-dim { opacity: 0.35; transition: opacity 200ms; }
.lyric-active { opacity: 1; }
.fullscreen-player { background-color: black; color: white; }
.fullscreen-player .fs-scrim { background: linear-gradient(to bottom, alpha(black, 0.15), alpha(black, 0.45)); }
.fullscreen-player .fs-cover { border-radius: 14px; box-shadow: 0 16px 48px alpha(black, 0.5); }
.fullscreen-player .fs-title { font-size: 1.6em; font-weight: 800; }
.fullscreen-player .fs-artist { font-size: 1.1em; opacity: 0.75; }
.fullscreen-player .fs-dim { opacity: 0.7; }
.fullscreen-player button { color: white; }
.fullscreen-player .fs-play { min-width: 64px; min-height: 64px; background-color: alpha(white, 0.18); }
.fullscreen-player .fs-play:hover { background-color: alpha(white, 0.28); }
.fullscreen-player scale trough { background-color: alpha(white, 0.2); }
.fullscreen-player scale highlight { background-color: white; }
.fullscreen-player scale slider { background-color: white; }
.fullscreen-player scrollbar { opacity: 0.5; }
.mini-progress trough, .mini-progress progress { min-height: 3px; }
"#;

/// Songs queued when a track is started from a long list.
const QUEUE_WINDOW: u32 = 150;

thread_local! {
    static APP: RefCell<Weak<App>> = RefCell::new(Weak::new());
}

/// The running app, for widgets that need to act (menus, activation).
pub fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().upgrade())
}

#[derive(Clone, Copy, PartialEq)]
enum BannerMode {
    Hidden,
    Starting,
    Login,
    Exited,
    Missing,
}

struct State {
    authorized: Option<bool>,
    banner: BannerMode,
    syncing: bool,
    /// A library change landed while a sync was running: sync again after it.
    resync: bool,
    /// Just removed from the library: Apple keeps listing them for a few seconds.
    removed: Vec<(String, Instant)>,
    seek_guard: Instant,
    seek_held: bool,
    seek_scheduled: bool,
    vol_scheduled: bool,
    last_error: Option<(String, Instant)>,
    now: Option<Item>,
    now_art: Option<String>,
    duration: f64,
    // position interpolation between the engine's ~1/s time events
    pos: f64,
    pos_at: Instant,
    playing: bool,
    /// playing, loading, waiting, seeking: the button shows Pause
    busy: bool,
    /// Last engine event counter seen (see `seq` in inject.js).
    seq: u64,
    /// Bumped to cancel a pending "Not Playing" reset.
    clear_gen: u64,
    /// Tracks user-started changes moved away from, and when. MusicKit keeps
    /// reporting the old track (in state events) until the new one loads; not shown.
    left: Vec<(String, Instant)>,
    volume_ready: bool,
    repeat: u8,
    queue: Queue,
    lyrics_for: Option<String>,
    exit_note: Option<String>,
    playlists: Vec<Item>,
    favourites: Option<Item>,
    pins: Vec<Item>,
    pins_expanded: bool,
}

pub struct App {
    engine: Rc<Engine>,
    cache: Cache,
    art: Rc<ArtLoader>,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    split: adw::NavigationSplitView,
    overlay: adw::OverlaySplitView,
    nav: adw::NavigationView,
    root_page: adw::NavigationPage,
    stack: gtk::Stack,
    sidebar: gtk::ListBox,
    bar: PlayerBar,
    /// "main" (the usual window content) | "full" (full-screen player)
    main_stack: gtk::Stack,
    full: Rc<FullScreen>,
    mini: Rc<Mini>,
    panel: Rc<SidePanel>,
    search_entry: gtk::SearchEntry,
    search_home: gtk::Box,
    lists: HashMap<&'static str, Rc<LibList>>,
    st: RefCell<State>,
}

fn sidebar_group(name: &str) -> &'static str {
    if name.starts_with("pl:") || name == "lib:playlists" || name == "fav" || name == "new-playlist" {
        "Playlists"
    } else if name.starts_with("lib:") || name == "pins" || name.starts_with("pin:") {
        "Library"
    } else {
        "Discover"
    }
}

fn sidebar_row(name: &str, icon: &str, title: &str) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&widgets::label(title, &[]));
    gtk::ListBoxRow::builder().child(&b).name(name).tooltip_text(title).build()
}

/// Apple's automatic "Favourite Songs" playlist, found by its (localised) name.
pub fn is_favourites(it: &Item) -> bool {
    matches!(it.title().to_lowercase().as_str(), "favorite songs" | "favourite songs" | "избранные песни")
}

fn kind_icon(it: &Item) -> &'static str {
    match it.kind() {
        Kind::Album => "media-optical-symbolic",
        Kind::Playlist => "view-list-symbolic",
        Kind::Artist => "avatar-default-symbolic",
        Kind::Station => "network-wireless-signal-good-symbolic",
        _ => "audio-x-generic-symbolic",
    }
}

/// "Pins" header row of the Library group: expands / collapses the pinned items under it.
fn pins_row() -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Image::from_icon_name("view-pin-symbolic"));
    let l = widgets::label("Pins", &[]);
    l.set_hexpand(true);
    b.append(&l);
    b.append(&gtk::Image::from_icon_name("pan-end-symbolic"));
    gtk::ListBoxRow::builder().child(&b).name("pins").tooltip_text("Pins").selectable(false).build()
}

impl App {
    pub fn build(gapp: &adw::Application, autostart: bool) {
        load_css();
        theme::init();

        let cache_dir = crate::cache_dir();
        let _ = std::fs::create_dir_all(&cache_dir);
        // Start the engine first: Electron boots while the widgets are built.
        let engine = Engine::new(find_engine_dir(), cache_dir.join("engine.log"));
        let engine_started = engine.start();
        eq::init(engine.clone());
        let cache = Cache::open(&cache_dir.join("cache.sqlite"));
        let art = ArtLoader::new(cache_dir.join("art"));

        // Sidebar
        let sidebar = gtk::ListBox::new();
        sidebar.add_css_class("navigation-sidebar");
        for (name, icon, title) in [
            ("search", "system-search-symbolic", "Search"),
            ("home", "go-home-symbolic", "Home"),
            ("browse", "view-app-grid-symbolic", "New"),
            ("radio", "network-wireless-signal-good-symbolic", "Radio"),
            ("pins", "", ""),
            ("lib:recently-added", "document-open-recent-symbolic", "Recently Added"),
            ("lib:artists", "system-users-symbolic", "Artists"),
            ("lib:albums", "media-optical-symbolic", "Albums"),
            ("lib:songs", "audio-x-generic-symbolic", "Songs"),
            ("lib:playlists", "view-grid-symbolic", "All Playlists"),
            ("fav", "starred-symbolic", "Favourite Songs"),
        ] {
            sidebar.append(&if name == "pins" { pins_row() } else { sidebar_row(name, icon, title) });
        }
        let new_pl = sidebar_row("new-playlist", "list-add-symbolic", "New Playlist…");
        new_pl.set_selectable(false);
        new_pl.set_tooltip_text(Some("New Playlist (Ctrl+N)"));
        sidebar.append(&new_pl);
        sidebar.set_header_func(|row, before| {
            let group = sidebar_group(&row.widget_name());
            if before.is_some_and(|b| sidebar_group(&b.widget_name()) == group) {
                row.set_header(None::<&gtk::Widget>);
            } else {
                let h = widgets::label(group, &["heading", "dim-label"]);
                h.set_margin_top(if before.is_some() { 14 } else { 4 });
                h.set_margin_bottom(4);
                h.set_margin_start(10);
                row.set_header(Some(&h));
            }
        });
        let menu = gio::Menu::new();
        menu.append(Some("New Playlist…"), Some("win.new-playlist"));
        menu.append(Some("Mini Player"), Some("win.mini"));
        menu.append(Some("Preferences"), Some("win.preferences"));
        menu.append(Some("Equalizer"), Some("win.equalizer"));
        menu.append(Some("Refresh"), Some("win.sync"));
        menu.append(Some("Show Engine Window"), Some("win.engine-window"));
        menu.append(Some("Restart Engine"), Some("win.restart"));
        menu.append(Some("Sign Out"), Some("win.logout"));
        menu.append(Some("About Applejack"), Some("win.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .build();
        let side_header = adw::HeaderBar::new();
        side_header.pack_end(&menu_button);

        let side_tv = adw::ToolbarView::new();
        side_tv.add_top_bar(&side_header);
        side_tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&sidebar).build()));
        let side_page = adw::NavigationPage::new(&side_tv, "Applejack");

        // Root content: a stack of sections; detail pages get pushed on top of it.
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        let mut lists = HashMap::new();
        for kind in LibKind::ALL {
            let list = LibList::new(kind, &art);
            stack.add_named(&list.root, Some(&format!("lib:{}", kind.key())));
            lists.insert(kind.key(), list);
        }
        stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("pl"));

        let banner = adw::Banner::new("Starting playback engine…");
        banner.set_revealed(true);
        let root_tv = adw::ToolbarView::new();
        root_tv.add_top_bar(&adw::HeaderBar::new());
        root_tv.add_top_bar(&banner);
        root_tv.set_content(Some(&stack));
        let root_page = adw::NavigationPage::with_tag(&root_tv, "Home", "root");
        let nav = adw::NavigationView::new();
        nav.add(&root_page);
        let content_page = adw::NavigationPage::new(&nav, "Applejack");

        let split = adw::NavigationSplitView::new();
        split.set_sidebar(Some(&side_page));
        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(210.0);
        split.set_max_sidebar_width(270.0);

        let panel = SidePanel::new();
        let overlay = adw::OverlaySplitView::new();
        overlay.set_sidebar_position(gtk::PackType::End);
        overlay.set_sidebar(Some(&panel.root));
        overlay.set_content(Some(&split));
        overlay.set_show_sidebar(false);
        overlay.set_min_sidebar_width(300.0);
        overlay.set_max_sidebar_width(380.0);

        let bar = PlayerBar::new();
        let outer = adw::ToolbarView::new();
        outer.set_content(Some(&overlay));
        outer.add_bottom_bar(&bar.root);
        outer.set_bottom_bar_style(adw::ToolbarStyle::Raised);
        let full = FullScreen::new();
        let main_stack = gtk::Stack::new();
        main_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        main_stack.set_transition_duration(250);
        main_stack.add_named(&outer, Some("main"));
        main_stack.add_named(&full.root, Some("full"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&main_stack));
        let mini = Mini::new(gapp);

        let window = adw::ApplicationWindow::builder()
            .application(gapp)
            .title("Applejack")
            .default_width(1240)
            .default_height(800)
            .width_request(360)
            .height_request(420)
            .content(&toasts)
            .build();
        // Below this the queue / lyrics panel floats over the content instead of
        // squeezing it.
        let medium = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1400sp").unwrap());
        medium.add_setter(&overlay, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(medium);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").unwrap());
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&overlay, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&bar.volume_box, "visible", Some(&false.to_value()));
        window.add_breakpoint(narrow);
        let tiny = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 520sp").unwrap());
        tiny.add_setter(&split, "collapsed", Some(&true.to_value()));
        tiny.add_setter(&overlay, "collapsed", Some(&true.to_value()));
        tiny.add_setter(&bar.volume_box, "visible", Some(&false.to_value()));
        tiny.add_setter(&bar.timeline, "visible", Some(&false.to_value()));
        tiny.add_setter(&bar.shuffle, "visible", Some(&false.to_value()));
        tiny.add_setter(&bar.repeat, "visible", Some(&false.to_value()));
        tiny.add_setter(&bar.mini, "visible", Some(&false.to_value()));
        tiny.add_setter(&bar.full, "visible", Some(&false.to_value()));
        window.add_breakpoint(tiny);

        let now = Instant::now();
        let app = Rc::new(App {
            engine,
            cache,
            art,
            window,
            toasts,
            banner,
            split,
            overlay,
            nav,
            root_page,
            stack,
            sidebar,
            bar,
            main_stack,
            full,
            mini,
            panel,
            search_entry: gtk::SearchEntry::new(),
            search_home: gtk::Box::new(gtk::Orientation::Vertical, 0),
            lists,
            st: RefCell::new(State {
                authorized: None,
                banner: BannerMode::Starting,
                syncing: false,
                resync: false,
                removed: Vec::new(),
                seek_guard: now,
                seek_held: false,
                seek_scheduled: false,
                vol_scheduled: false,
                last_error: None,
                now: None,
                now_art: None,
                duration: 0.0,
                pos: 0.0,
                pos_at: now,
                playing: false,
                busy: false,
                seq: 0,
                clear_gen: 0,
                left: Vec::new(),
                volume_ready: false,
                repeat: 0,
                queue: Queue::default(),
                lyrics_for: None,
                exit_note: None,
                playlists: Vec::new(),
                favourites: None,
                pins: Vec::new(),
                pins_expanded: true,
            }),
        });
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));

        let search = app.build_search();
        app.stack.add_named(&search, Some("search"));

        app.connect_signals(gapp);
        app.load_cached();
        desktop::connect_close(&app);
        desktop::add_app_actions(&app, gapp);
        mpris::init(gapp);
        tray::init(gapp);
        app.window.connect_visible_notify(|_| tray::window_changed());
        if !desktop::start_hidden(autostart) {
            app.window.present();
        }
        app.select_sidebar("home");
        match engine_started {
            Ok(()) => app.set_banner(BannerMode::Starting, "Starting playback engine…"),
            Err(e) => app.set_banner(BannerMode::Missing, &format!("Engine failed to start: {e}")),
        }
        app.start_ticker();
        app.dev_snapshot();
    }

    /// Dev aid: AM_GTK_SNAPSHOT=out.png [AM_GTK_PAGE=<sidebar name>] [AM_GTK_SEARCH=term]
    /// [AM_GTK_ARTIST=id] [AM_GTK_ALBUM=id] [AM_GTK_PANEL=lyrics|queue] [AM_GTK_DELAY=s]
    /// [AM_GTK_WIDTH=px] [AM_GTK_HEIGHT=px] [AM_GTK_PLAY=songid,…] (muted, or at AM_GTK_VOLUME=0..1)
    /// [AM_GTK_EQ=1] (opens Preferences → Equalizer) [AM_GTK_SCROLL=px/s] [AM_GTK_FULL=1] (full-screen
    /// player) [AM_GTK_MINI=1] (mini player, which is then the window captured) renders the window to
    /// a PNG after a few seconds and quits.
    fn dev_snapshot(self: &Rc<Self>) {
        let Ok(path) = std::env::var("AM_GTK_SNAPSHOT") else { return };
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if let Some(w) = env("AM_GTK_WIDTH").and_then(|w| w.parse().ok()) {
            let h = env("AM_GTK_HEIGHT").and_then(|h| h.parse().ok()).unwrap_or(800);
            self.window.set_default_size(w, h);
        }
        if let Some(page) = env("AM_GTK_PAGE") {
            self.select_sidebar(&page);
        }
        let app = self.clone();
        glib::timeout_add_seconds_local_once(6, move || {
            if let Some(term) = env("AM_GTK_SEARCH") {
                app.select_sidebar("search");
                app.search_entry.set_text(&term);
            }
            if let Some(id) = env("AM_GTK_ARTIST") {
                app.open_artist(&id, None);
            }
            if let Some(id) = env("AM_GTK_ALBUM") {
                app.open_container(Item { id, kind: Some("albums".into()), ..Default::default() }, "album");
            }
            if let Some(id) = env("AM_GTK_PLAY") {
                // Muted: this is for screenshots of the bar / lyrics / queue.
                let volume = env("AM_GTK_VOLUME").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                app.call_quiet("volume", json!({ "value": volume }));
                app.play_songs(id.split(',').map(str::to_owned).collect(), 0);
            }
            if env("AM_GTK_EQ").is_some() {
                app.show_preferences();
            }
            if let Some(p) = env("AM_GTK_PANEL") {
                let app = app.clone();
                glib::timeout_add_seconds_local_once(6, move || app.open_panel(&p));
            }
            if env("AM_GTK_FULL").is_some() {
                let app = app.clone();
                glib::timeout_add_seconds_local_once(4, move || app.set_fullscreen(true));
            }
            if env("AM_GTK_PREFS").is_some() {
                prefs::show(&app); // AM_GTK_PREFS=integrations opens that page
            }
            if let Some(id) = env("AM_GTK_EDIT") {
                let (app2, id2) = (app.clone(), id.clone());
                let fut = app.engine.call("tracks", json!({ "id": id, "type": "playlist" }));
                glib::spawn_future_local(async move {
                    let tracks = fut.await.ok().and_then(|v| serde_json::from_value::<Vec<Item>>(v).ok()).unwrap_or_default();
                    app2.edit_playlist(&id2, "Test", "", tracks, |_, _, _| {});
                });
            }
            if env("AM_GTK_MINI").is_some() {
                let app = app.clone();
                glib::timeout_add_seconds_local_once(4, move || app.set_mini(true));
            }
            if let Some(speed) = env("AM_GTK_SCROLL").and_then(|s| s.parse::<f64>().ok()) {
                app.dev_autoscroll(speed);
            }
        });
        let delay = env("AM_GTK_DELAY").and_then(|d| d.parse().ok()).unwrap_or(20);
        let app = self.clone();
        glib::timeout_add_seconds_local_once(delay, move || {
            let w: &gtk::Widget =
                if env("AM_GTK_MINI").is_some() { app.mini.window.upcast_ref() } else { app.window.upcast_ref() };
            let paintable = gtk::WidgetPaintable::new(Some(w));
            let snap = gtk::Snapshot::new();
            paintable.snapshot(&snap, w.width() as f64, w.height() as f64);
            if let (Some(node), Some(renderer)) = (snap.to_node(), w.native().and_then(|n| n.renderer())) {
                if let Err(e) = renderer.render_texture(&node, None).save_to_png(&path) {
                    eprintln!("snapshot: {e}");
                }
            }
            if env("AM_GTK_PLAY").is_some() {
                app.call_quiet("pause", json!({}));
            }
            if let Some(a) = app.window.application() {
                glib::timeout_add_local_once(Duration::from_millis(500), move || a.quit());
            }
        });
    }

    /// Dev aid (AM_GTK_SCROLL=px/s, used by perf/bench.py): scrolls the page on screen down
    /// and back up forever, one step per frame, like a user flicking through the library.
    fn dev_autoscroll(&self, speed: f64) {
        fn find(w: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
            if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
                let a = sw.vadjustment();
                if a.upper() - a.page_size() > 1.0 {
                    return Some(sw.clone());
                }
            }
            let mut c = w.first_child();
            while let Some(ch) = c {
                if ch.is_mapped() {
                    if let Some(sw) = find(&ch) {
                        return Some(sw);
                    }
                }
                c = ch.next_sibling();
            }
            None
        }
        let stack = self.stack.clone();
        let dir = Cell::new(1.0);
        let last = Cell::new(0i64);
        self.window.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let dt = if last.get() == 0 { 0.0 } else { (now - last.get()) as f64 / 1e6 };
            last.set(now);
            if let Some(sw) = stack.visible_child().and_then(|c| find(&c)) {
                let a = sw.vadjustment();
                let max = a.upper() - a.page_size();
                let v = a.value() + dir.get() * speed * dt;
                if v >= max || v <= 0.0 {
                    dir.set(-dir.get());
                }
                a.set_value(v.clamp(0.0, max));
            }
            glib::ControlFlow::Continue
        });
    }

    fn connect_signals(self: &Rc<Self>, gapp: &adw::Application) {
        let weak = Rc::downgrade(self);
        self.engine.set_event_handler(move |name, data| {
            if let Some(app) = weak.upgrade() {
                app.on_event(name, data);
            }
        });

        let engine = self.engine.clone();
        gapp.connect_shutdown(move |_| engine.stop());

        // Sidebar navigation
        let app = self.clone();
        self.sidebar.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                app.show_section(&row.widget_name());
            }
        });
        let app = self.clone();
        self.sidebar.connect_row_activated(move |_, row| {
            if row.widget_name() == "pins" {
                let expanded = !app.st.borrow().pins_expanded;
                app.set_pins_expanded(expanded);
            } else if row.widget_name() == "new-playlist" {
                app.new_playlist(Vec::new());
            } else {
                app.split.set_show_content(true);
            }
        });

        // Banner button: sign in / restart
        let app = self.clone();
        self.banner.connect_button_clicked(move |_| {
            let mode = app.st.borrow().banner;
            match mode {
                BannerMode::Login => app.login(),
                BannerMode::Exited => app.start_engine(),
                _ => {}
            }
        });

        // Player bar
        let app = self.clone();
        self.bar.play.connect_clicked(move |_| app.toggle_play());
        let app = self.clone();
        self.bar.next.connect_clicked(move |_| app.skip(true));
        let app = self.clone();
        self.bar.prev.connect_clicked(move |_| app.skip(false));
        let app = self.clone();
        self.bar.star.connect_clicked(move |b| {
            let id = app.st.borrow().now.as_ref().and_then(Item::catalog_song_id);
            match id {
                Some(id) => app.rate(&id, if b.is_active() { 1 } else { 0 }),
                None => {
                    b.set_active(false);
                    app.toast("Only catalog songs can be favourited");
                }
            }
            app.bar.show_favourite(Some(b.is_active()));
        });
        let app = self.clone();
        self.bar.shuffle.connect_clicked(move |b| app.call_status("shuffle", json!({ "on": b.is_active() })));
        let app = self.clone();
        self.bar.repeat.connect_clicked(move |_| {
            let next = match app.st.borrow().repeat {
                0 => 2,
                2 => 1,
                _ => 0,
            };
            app.st.borrow_mut().repeat = next;
            app.bar.show_repeat(next);
            app.call_status("repeat", json!({ "mode": next }));
        });
        let app = self.clone();
        self.bar.seek.connect_change_value(move |_, _, value| {
            let mut st = app.st.borrow_mut();
            st.seek_guard = Instant::now() + Duration::from_millis(1500);
            app.bar.pos.set_label(&fmt_time(value));
            if !st.seek_scheduled {
                st.seek_scheduled = true;
                let app = app.clone();
                glib::timeout_add_local_once(Duration::from_millis(150), move || {
                    app.st.borrow_mut().seek_scheduled = false;
                    app.seek_to(app.bar.seek.value().max(0.0));
                });
            }
            glib::Propagation::Proceed
        });
        // While the seek slider is held, playback updates must not move it under the pointer.
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let app = self.clone();
        legacy.connect_event(move |_, ev| {
            use gdk::EventType as T;
            match ev.event_type() {
                T::ButtonPress | T::TouchBegin => app.st.borrow_mut().seek_held = true,
                T::ButtonRelease | T::TouchEnd | T::TouchCancel => {
                    let mut st = app.st.borrow_mut();
                    st.seek_held = false;
                    st.seek_guard = Instant::now() + Duration::from_millis(1500);
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        self.bar.seek.add_controller(legacy);
        // Volume: the slider is the source of truth; the engine only gets told.
        let app = self.clone();
        self.bar.vol.connect_change_value(move |_, _, _| {
            if app.bar.mute.is_active() {
                app.bar.mute.set_active(false); // moving the slider unmutes
            }
            let mut st = app.st.borrow_mut();
            if !st.vol_scheduled {
                st.vol_scheduled = true;
                let app = app.clone();
                glib::timeout_add_local_once(Duration::from_millis(40), move || {
                    app.st.borrow_mut().vol_scheduled = false;
                    app.push_volume();
                    app.cache.put("pref:volume", &app.bar.vol.value());
                });
            }
            glib::Propagation::Proceed
        });
        let app = self.clone();
        self.bar.vol.connect_value_changed(move |_| app.bar.show_volume_icon());
        let app = self.clone();
        self.bar.mute.connect_toggled(move |_| {
            app.bar.show_volume_icon();
            app.push_volume();
        });
        // Queue panel: clear / autoplay
        let app = self.clone();
        self.panel.clear.connect_clicked(move |_| app.call_queue("clearQueue", json!({})));
        let app = self.clone();
        self.panel.autoplay.connect_clicked(move |b| app.call_queue("autoplay", json!({ "on": b.is_active() })));
        // Title -> album, artist line -> artist
        for (lbl, to_album) in [(&self.bar.title, true), (&self.bar.sub, false)] {
            lbl.add_css_class("now-clickable");
            lbl.set_cursor_from_name(Some("pointer"));
            let click = gtk::GestureClick::new();
            let app = self.clone();
            click.connect_released(move |_, _, _, _| {
                let now = app.st.borrow().now.clone();
                if let Some(it) = now {
                    app.go_to(&it, to_album);
                }
            });
            lbl.add_controller(click);
        }
        // Side panel toggles
        for (toggle, page) in [(&self.bar.lyrics, "lyrics"), (&self.bar.queue, "queue")] {
            let app = self.clone();
            toggle.connect_clicked(move |b| {
                if b.is_active() {
                    app.open_panel(page);
                } else {
                    app.overlay.set_show_sidebar(false);
                }
            });
        }
        let app = self.clone();
        self.panel.close.connect_clicked(move |_| app.overlay.set_show_sidebar(false));
        let app = self.clone();
        self.overlay.connect_show_sidebar_notify(move |o| {
            if !o.shows_sidebar() {
                app.bar.lyrics.set_active(false);
                app.bar.queue.set_active(false);
            }
        });

        // Window actions
        let add = |name: &str, f: Box<dyn Fn(&Rc<App>)>| {
            let action = gio::SimpleAction::new(name, None);
            let app = self.clone();
            action.connect_activate(move |_, _| f(&app));
            self.window.add_action(&action);
        };
        add("sync", Box::new(|app| app.refresh()));
        add("equalizer", Box::new(|app| app.show_preferences()));
        add("preferences", Box::new(|app| prefs::show(app)));
        add("engine-window", Box::new(|app| app.call_quiet("show", json!({}))));
        add("restart", Box::new(|app| app.start_engine()));
        add("logout", Box::new(|app| {
            let app = app.clone();
            glib::spawn_future_local(async move {
                match app.engine.call("logout", json!({})).await {
                    Ok(_) => app.set_authorized(false),
                    Err(e) => app.toast(&format!("Couldn’t sign out: {e}")),
                }
            });
        }));
        add("about", Box::new(|app| {
            adw::AboutDialog::builder()
                .application_name("Applejack")
                .application_icon(crate::APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments("A native music streaming player. Playback runs through the official web player in a hidden castLabs Electron window. Unofficial; not affiliated with Apple Inc.")
                .build()
                .present(Some(&app.window));
        }));
        add("search", Box::new(|app| {
            app.select_sidebar("search");
            app.search_entry.grab_focus();
        }));
        add("toggle", Box::new(|app| app.toggle_play()));
        add("next", Box::new(|app| app.skip(true)));
        add("previous", Box::new(|app| app.skip(false)));
        add("mute", Box::new(|app| app.bar.mute.set_active(!app.bar.mute.is_active())));
        add("lyrics", Box::new(|app| app.toggle_panel("lyrics")));
        add("fullscreen", Box::new(|app| app.set_fullscreen(!app.is_fullscreen())));
        add("mini", Box::new(|app| {
            let on = !app.mini.window.is_visible();
            app.set_mini(on);
        }));
        add("new-playlist", Box::new(|app| app.new_playlist(Vec::new())));
        add("queue", Box::new(|app| app.toggle_panel("queue")));
        add("back", Box::new(|app| {
            app.nav.pop();
        }));
        gapp.set_accels_for_action("win.search", &["<Control>f", "<Control>k"]);
        gapp.set_accels_for_action("win.toggle", &["<Control>space"]);
        gapp.set_accels_for_action("win.next", &["<Control>Right"]);
        gapp.set_accels_for_action("win.previous", &["<Control>Left"]);
        gapp.set_accels_for_action("win.lyrics", &["<Control>l"]);
        gapp.set_accels_for_action("win.queue", &["<Control>u"]);
        gapp.set_accels_for_action("win.sync", &["<Control>r", "F5"]);
        gapp.set_accels_for_action("win.back", &["<Alt>Left"]);
        gapp.set_accels_for_action("win.equalizer", &["<Control>e"]);
        gapp.set_accels_for_action("win.preferences", &["<Control>comma"]);
        gapp.set_accels_for_action("win.mute", &["<Control>m"]);
        gapp.set_accels_for_action("win.fullscreen", &["F11"]);
        gapp.set_accels_for_action("win.mini", &["<Control><Shift>m"]);
        gapp.set_accels_for_action("win.new-playlist", &["<Control>n"]);
        // Escape leaves the full-screen player; so does the desktop un-fullscreening the window.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let app = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape && app.is_fullscreen() {
                app.set_fullscreen(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let app = self.clone();
        self.window.connect_fullscreened_notify(move |w| {
            if !w.is_fullscreen() && app.is_fullscreen() {
                app.main_stack.set_visible_child_name("main");
            }
            let full = app.full.clone();
            glib::timeout_add_local_once(Duration::from_millis(50), move || full.layout());
        });
        let app = self.clone();
        self.window.connect_default_width_notify(move |_| app.full.layout());
    }

    // ---- navigation ----

    fn select_sidebar(&self, name: &str) {
        let mut i = 0;
        while let Some(row) = self.sidebar.row_at_index(i) {
            if row.widget_name() == name {
                self.sidebar.select_row(Some(&row));
                return;
            }
            i += 1;
        }
    }

    fn show_section(self: &Rc<Self>, name: &str) {
        let mut title = String::new();
        let mut i = 0;
        while let Some(row) = self.sidebar.row_at_index(i) {
            if row.widget_name() == name {
                title = row.tooltip_text().map(|t| t.to_string()).unwrap_or_default();
                break;
            }
            i += 1;
        }
        if let Some(id) = name.strip_prefix("pl:") {
            let item = self.st.borrow().playlists.iter().find(|p| p.id == id).cloned();
            if let Some(item) = item {
                let (body, _) = self.container_view(item, "playlist");
                self.show_in_holder(&body);
            }
        } else if name == "fav" {
            let fav = self.st.borrow().favourites.clone();
            match fav {
                Some(item) => {
                    let (body, _) = self.container_view(item, "playlist");
                    self.show_in_holder(&body);
                }
                None => {
                    let status = adw::StatusPage::builder()
                        .icon_name("emblem-favorite-symbolic")
                        .title("No Favourite Songs Yet")
                        .description("Songs you love show up here")
                        .build();
                    self.show_in_holder(&status);
                }
            }
        } else if let Some(id) = name.strip_prefix("pin:") {
            let Some(item) = self.st.borrow().pins.iter().find(|p| p.id == id).cloned() else { return };
            match item.kind() {
                Kind::Album | Kind::Playlist => {
                    let ctype = if item.kind() == Kind::Album { "album" } else { "playlist" };
                    let (body, _) = self.container_view(item.clone(), ctype);
                    self.show_in_holder(&body);
                    // The pin's action from the Music app: play / shuffle start it as well.
                    if let Some(sh) = match item.pin_action.as_deref() {
                        Some("play") => Some(false),
                        Some("shuffle") => Some(true),
                        _ => None,
                    } {
                        self.set_shuffle(sh);
                        self.play_container(&item.id, ctype, 0, Vec::new());
                    }
                }
                Kind::Artist => {
                    let body = self.artist_body(&item.id, |_| {});
                    self.show_in_holder(&body);
                }
                // Songs and stations just play; the page stays where it was.
                _ => {
                    self.activate(&item, None);
                    return;
                }
            }
            title = item.title().to_owned();
        } else {
            self.ensure_page(name);
            self.stack.set_visible_child_name(name);
        }
        self.root_page.set_title(&title);
        self.nav.pop_to_page(&self.root_page);
        self.split.set_show_content(true);
        if name == "search" {
            self.search_entry.grab_focus();
        }
    }

    /// Shows a page body for a sidebar entry that has no page of its own.
    fn show_in_holder(&self, body: &impl IsA<gtk::Widget>) {
        let holder = self.stack.child_by_name("pl").and_downcast::<gtk::Box>().unwrap();
        while let Some(c) = holder.first_child() {
            holder.remove(&c);
        }
        body.set_vexpand(true);
        holder.append(body);
        self.stack.set_visible_child_name("pl");
    }

    /// Home / Browse / Radio are built on first visit.
    fn ensure_page(self: &Rc<Self>, name: &str) {
        if self.stack.child_by_name(name).is_some() {
            return;
        }
        let page = match name {
            "home" => self.sections_page("home", "Home", "Recommendations, recently played and picks for you"),
            "browse" => self.sections_page("browse", "New", "Charts and what’s popular now"),
            "radio" => self.sections_page("radio", "Radio", "Live radio and stations for you"),
            _ => return,
        };
        self.stack.add_named(&page, Some(name));
    }

    /// Rebuilds the remote pages (after sign-in or on refresh).
    fn invalidate_pages(self: &Rc<Self>) {
        let visible = self.stack.visible_child_name().map(|s| s.to_string());
        for name in ["home", "browse", "radio"] {
            if let Some(child) = self.stack.child_by_name(name) {
                self.stack.remove(&child);
            }
        }
        if let Some(v) = visible.filter(|v| ["home", "browse", "radio"].contains(&v.as_str())) {
            self.ensure_page(&v);
            self.stack.set_visible_child_name(&v);
        }
    }

    fn refresh(self: &Rc<Self>) {
        for key in ["page:home", "page:browse", "page:radio"] {
            self.cache.put(key, &Value::Null); // forces a refetch
        }
        self.invalidate_pages();
        self.sync_library();
    }

    fn push(&self, page: &adw::NavigationPage) {
        self.nav.push(page);
        self.split.set_show_content(true);
    }

    /// Opens / plays whatever was clicked. `context` = the list a song was picked from.
    pub fn activate(self: &Rc<Self>, it: &Item, context: Option<(&[Item], usize)>) {
        if self.in_search() {
            self.remember_search(Some(it));
        }
        match it.kind() {
            Kind::Song | Kind::Other => {
                self.set_shuffle(false);
                self.show_upcoming(it);
                match context {
                    Some((list, idx)) => {
                        let ids = list.iter().map(Item::song_play_id).collect();
                        self.play_songs(ids, idx as u32);
                    }
                    None => self.play_songs(vec![it.song_play_id()], 0),
                }
            }
            Kind::Album => self.open_container(it.clone(), "album"),
            Kind::Playlist => self.open_container(it.clone(), "playlist"),
            Kind::Station => {
                self.show_upcoming(it);
                self.call_status("playItem", json!({ "id": it.id, "type": "station" }));
            }
            Kind::Artist => self.open_artist(&it.id, it.title.as_deref()),
        }
    }

    /// Go to the album (or artist) of a song.
    fn go_to(self: &Rc<Self>, it: &Item, album: bool) {
        let app = self.clone();
        let fut = self.engine.call("songLinks", json!({ "id": it.song_play_id() }));
        let artist_name = it.artist.clone();
        glib::spawn_future_local(async move {
            match fut.await.and_then(|v| serde_json::from_value::<SongLinks>(v).map_err(|e| e.to_string())) {
                Ok(SongLinks { album: Some(a), .. }) if album => app.open_container(a, "album"),
                Ok(SongLinks { artist_id: Some(id), .. }) if !album => app.open_artist(&id, artist_name.as_deref()),
                Ok(_) => app.toast("Not found in the catalog"),
                Err(e) => app.error(&e),
            }
        });
    }

    // ---- item menu ----

    pub fn show_menu(self: &Rc<Self>, widget: &gtk::Widget, it: &Item, at: Option<(f64, f64)>) {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let pop = gtk::Popover::new();
        pop.add_css_class("menu");
        pop.set_has_arrow(at.is_none());
        let add = |text: &str, f: Box<dyn Fn(&Rc<App>)>| {
            let b = gtk::Button::builder().label(text).css_classes(["flat"]).build();
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let (app, pop) = (self.clone(), pop.downgrade());
            b.connect_clicked(move |_| {
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
                f(&app);
            });
            col.append(&b);
        };
        let sep = || col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let kind = it.kind();
        let ctype = match kind {
            Kind::Album => "album",
            Kind::Playlist => "playlist",
            Kind::Station => "station",
            _ => "song",
        };
        let play_id = if kind == Kind::Song { it.song_play_id() } else { it.id.clone() };
        match kind {
            Kind::Artist => {
                let i = it.clone();
                add("Open", Box::new(move |app| app.activate(&i, None)));
            }
            Kind::Album | Kind::Playlist => {
                let i = it.clone();
                add("Open", Box::new(move |app| app.activate(&i, None)));
                let id = it.id.clone();
                add("Play", Box::new(move |app| {
                    app.set_shuffle(false);
                    app.play_container(&id, ctype, 0, Vec::new());
                }));
            }
            _ => {
                let i = it.clone();
                add("Play", Box::new(move |app| app.activate(&i, None)));
            }
        }
        if kind != Kind::Artist {
            let id = play_id.clone();
            add("Play Next", Box::new(move |app| app.enqueue(&id, ctype, "next")));
            let id = play_id.clone();
            add("Play Later", Box::new(move |app| app.enqueue(&id, ctype, "later")));
        }
        if matches!(kind, Kind::Song | Kind::Album) {
            let i = it.clone();
            add("Add to Playlist…", Box::new(move |app| app.add_to_playlist(&i)));
        }
        if kind == Kind::Song {
            sep();
            let i = it.clone();
            add("Go to Album", Box::new(move |app| app.go_to(&i, true)));
            let i = it.clone();
            add("Go to Artist", Box::new(move |app| app.go_to(&i, false)));
        }
        // Apple pins library items only.
        if kind != Kind::Station && it.is_library() {
            sep();
            let pinned = self.st.borrow().pins.iter().any(|p| p.id == it.id);
            let id = it.id.clone();
            if pinned {
                add("Unpin", Box::new(move |app| app.call_pins("unpin", json!({ "id": &id }))));
            } else {
                add("Pin", Box::new(move |app| app.call_pins("pin", json!({ "id": &id }))));
            }
        }
        if matches!(kind, Kind::Song | Kind::Album | Kind::Playlist) && !it.is_library() {
            sep();
            let i = it.clone();
            add("Add to Library", Box::new(move |app| app.add_to_library(&i)));
        }
        if matches!(kind, Kind::Song | Kind::Album | Kind::Playlist) && it.is_library() {
            let i = it.clone();
            let label = if kind == Kind::Playlist { "Delete Playlist" } else { "Remove from Library" };
            add(label, Box::new(move |app| app.confirm_remove(&i)));
        }
        if kind == Kind::Song {
            if let Some(id) = it.catalog_song_id() {
                if it.is_library() {
                    sep();
                }
                let i = id.clone();
                add("Favourite", Box::new(move |app| app.rate(&i, 1)));
                add("Suggest Less", Box::new(move |app| app.rate(&id, -1)));
            }
        }
        pop.set_child(Some(&col));
        pop.set_parent(widget);
        if let Some((x, y)) = at {
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        }
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    fn enqueue(self: &Rc<Self>, id: &str, ctype: &str, where_: &'static str) {
        let app = self.clone();
        let fut = self.engine.call("enqueue", json!({ "id": id, "type": ctype, "where": where_ }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(v) => {
                    if let Ok(q) = serde_json::from_value::<Queue>(v) {
                        app.show_queue(q);
                    }
                    app.toast(if where_ == "next" { "Playing next" } else { "Added to the end of the queue" });
                }
                Err(e) => app.error(&e),
            }
        });
    }

    fn add_to_library(self: &Rc<Self>, it: &Item) {
        let (id, t) = match it.kind() {
            Kind::Song => (it.song_play_id(), "songs"),
            Kind::Album => (it.catalog_id.clone().unwrap_or_else(|| it.id.clone()), "albums"),
            Kind::Playlist => (it.catalog_id.clone().unwrap_or_else(|| it.id.clone()), "playlists"),
            _ => return,
        };
        let app = self.clone();
        let it = it.clone();
        let fut = self.engine.call("addToLibrary", json!({ "id": id, "type": t }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(_) => {
                    app.toast("Added to library");
                    app.show_added(&it, t);
                    // Apple takes a moment to list a new add; look twice.
                    for secs in [2, 8] {
                        let app = app.clone();
                        glib::timeout_add_seconds_local_once(secs, move || app.sync_library());
                    }
                }
                Err(e) => app.error(&format!("couldn’t add: {e}")),
            }
        });
    }

    fn confirm_remove(self: &Rc<Self>, it: &Item) {
        let playlist = it.kind() == Kind::Playlist;
        let dialog = adw::AlertDialog::new(
            Some(if playlist { "Delete Playlist?" } else { "Remove from Library?" }),
            Some(&if playlist {
                format!("“{}” will be deleted from your library on all your devices.", it.title())
            } else {
                format!("“{}” will be removed from your library on all your devices.", it.title())
            }),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", if playlist { "Delete" } else { "Remove" })]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let (app, it) = (self.clone(), it.clone());
        dialog.connect_response(None, move |_, r| {
            if r == "remove" {
                app.remove_from_library(&it);
            }
        });
        dialog.present(Some(&self.window));
    }

    fn remove_from_library(self: &Rc<Self>, it: &Item) {
        let t = match it.kind() {
            Kind::Song => "songs",
            Kind::Album => "albums",
            Kind::Playlist => "playlists",
            _ => return,
        };
        let app = self.clone();
        let it = it.clone();
        let fut = self.engine.call("removeFromLibrary", json!({ "id": &it.id, "type": t }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(_) => {
                    app.toast(if t == "playlists" { "Playlist deleted" } else { "Removed from library" });
                    app.st.borrow_mut().removed.push((it.id.clone(), Instant::now()));
                    for (key, list) in &app.lists {
                        let cache_key = format!("library:{key}");
                        let Some(mut items) = app.cache.get::<Vec<Item>>(&cache_key) else { continue };
                        let n = items.len();
                        items.retain(|x| x.id != it.id);
                        if items.len() != n {
                            app.cache.put(&cache_key, &items);
                            list.set_items(&items);
                        }
                    }
                    if t == "playlists" {
                        let pls = app.cache.get::<Vec<Item>>("library:playlists").unwrap_or_default();
                        app.set_sidebar_playlists(&pls);
                    }
                    let app = app.clone();
                    glib::timeout_add_seconds_local_once(10, move || app.sync_library());
                }
                Err(e) => app.error(&format!("couldn’t remove: {e}")),
            }
        });
    }

    /// Shows a just-added item in the library lists right away (the next sync replaces it
    /// with the library's own copy) and drops the stale cached page of the item.
    fn show_added(self: &Rc<Self>, it: &Item, t: &str) {
        let mut targets = vec![t];
        if t != "songs" {
            targets.push("recently-added");
        }
        for key in targets {
            let Some(list) = self.lists.get(key) else { continue };
            let cache_key = format!("library:{key}");
            let mut items = self.cache.get::<Vec<Item>>(&cache_key).unwrap_or_default();
            if items.iter().any(|x| x.id == it.id || x.catalog_id.as_deref() == Some(it.id.as_str())) {
                continue;
            }
            if key == "recently-added" {
                items.insert(0, it.clone());
            } else {
                let title = it.title().to_lowercase();
                let at = items.partition_point(|x| x.title().to_lowercase() < title);
                items.insert(at, it.clone());
            }
            self.cache.put(&cache_key, &items);
            list.set_items(&items);
        }
        for ctype in ["album", "playlist"] {
            self.cache.put(&format!("container:{ctype}:{}", it.id), &Value::Null);
        }
    }

    /// Favourite Songs changed: drop its cached tracks and reload it if it is on screen.
    fn refresh_favourites(self: &Rc<Self>) {
        let Some(fav) = self.st.borrow().favourites.clone() else { return };
        self.cache.put(&format!("container:playlist:{}", fav.id), &Value::Null);
        if self.sidebar.selected_row().is_some_and(|r| r.widget_name() == "fav") {
            self.show_section("fav");
        }
    }

    fn rate(self: &Rc<Self>, id: &str, value: i32) {
        let app = self.clone();
        let id = id.to_owned();
        let fut = self.engine.call("rate", json!({ "id": &id, "type": "songs", "value": value }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(_) => {
                    app.toast(match value {
                        1.. => "Added to Favourite Songs",
                        0 => "Removed from Favourite Songs",
                        _ => "You’ll see less like this",
                    });
                    // The star in the bar follows if this was the current track.
                    if app.st.borrow().now.as_ref().and_then(Item::catalog_song_id).as_deref() == Some(id.as_str()) {
                        app.bar.show_favourite(Some(value > 0));
                    }
                    // Apple updates the playlist with a short delay.
                    let app = app.clone();
                    glib::timeout_add_seconds_local_once(2, move || app.refresh_favourites());
                }
                Err(e) => {
                    app.error(&e);
                    // Put the star back to what Apple has.
                    let now = app.st.borrow().now.clone();
                    if let Some(now) = now {
                        app.load_favourite(&now);
                    }
                }
            }
        });
    }

    // ---- engine lifecycle & events ----

    fn start_engine(self: &Rc<Self>) {
        match self.engine.start() {
            Ok(()) => self.set_banner(BannerMode::Starting, "Starting playback engine…"),
            Err(e) => self.set_banner(BannerMode::Missing, &format!("Engine failed to start: {e}")),
        }
    }

    fn on_event(self: &Rc<Self>, name: &str, data: &Value) {
        match name {
            "engineReady" => {
                if self.st.borrow().banner == BannerMode::Starting {
                    self.set_banner(BannerMode::Starting, "Loading your music…");
                }
            }
            "ready" | "state" => {
                if let Ok(s) = serde_json::from_value::<Status>(data.clone()) {
                    if name == "ready" {
                        // A (re)loaded page counts its events from zero again.
                        self.st.borrow_mut().seq = 0;
                        self.init_volume(s.volume);
                        eq::on_ready();
                    }
                    self.apply_status(&s);
                }
            }
            "auth" => self.set_authorized(data.get("authorized").and_then(Value::as_bool).unwrap_or(false)),
            "loginComplete" => {
                self.set_authorized(true);
                self.window.present();
                self.toast("Signed in");
            }
            "nowPlaying" => {
                // Followed by a "state" event with the same data and the event counter;
                // that one is applied. A null item between tracks is not shown.
                if let Some(it) = serde_json::from_value::<Option<Item>>(data.clone()).ok().flatten() {
                    if !self.is_left(&it) {
                        self.show_now_playing(Some(&it));
                    }
                }
            }
            "spectrum" => eq::on_spectrum(data),
            "time" => {
                let f = |k: &str| data.get(k).and_then(Value::as_f64);
                self.show_time(f("position"), f("duration"));
            }
            "queue" => {
                if let Ok(q) = serde_json::from_value::<Queue>(data.clone()) {
                    self.show_queue(q);
                }
            }
            "error" if data.get("code").and_then(Value::as_str) == Some("locked") => {
                self.st.borrow_mut().exit_note =
                    Some("The engine is already running in another program (e.g. npm run cli). Close it and press Restart".into());
            }
            "error" => {
                if let Some(m) = data.get("message").and_then(Value::as_str) {
                    self.error(m);
                }
            }
            "exit" => {
                let note = self.st.borrow_mut().exit_note.take();
                self.set_banner(BannerMode::Exited, note.as_deref().unwrap_or("The playback engine stopped"));
            }
            _ => {}
        }
    }

    fn set_banner(&self, mode: BannerMode, title: &str) {
        self.st.borrow_mut().banner = mode;
        self.banner.set_title(title);
        let button = match mode {
            BannerMode::Login => Some("Sign In"),
            BannerMode::Exited => Some("Restart"),
            _ => None,
        };
        self.banner.set_button_label(button);
        self.banner.set_revealed(mode != BannerMode::Hidden);
    }

    fn set_authorized(self: &Rc<Self>, authorized: bool) {
        let prev = self.st.borrow_mut().authorized.replace(authorized);
        if authorized {
            if self.st.borrow().banner != BannerMode::Exited {
                self.set_banner(BannerMode::Hidden, "");
            }
            if prev != Some(true) {
                self.sync_library();
                if prev == Some(false) {
                    self.invalidate_pages();
                }
            }
        } else {
            self.set_banner(BannerMode::Login, "You’re not signed in");
            for list in self.lists.values() {
                if list.is_empty() {
                    list.show_status("avatar-default-symbolic", "Sign In", "Your library will appear after you sign in");
                }
            }
        }
    }

    fn login(self: &Rc<Self>) {
        let app = self.clone();
        glib::spawn_future_local(async move {
            match app.engine.call("login", json!({})).await {
                Ok(v) if v.get("authorized").and_then(Value::as_bool) == Some(true) => app.set_authorized(true),
                Ok(_) => app.toast("Click Sign In in the window that opened; it closes by itself afterwards"),
                Err(e) => app.toast(&format!("Couldn’t open sign-in: {e}")),
            }
        });
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Engine errors, de-duplicated so a burst does not flood the screen.
    fn error(&self, msg: &str) {
        let mut st = self.st.borrow_mut();
        if let Some((last, at)) = &st.last_error {
            if last == msg && at.elapsed() < Duration::from_secs(10) {
                return;
            }
        }
        st.last_error = Some((msg.to_owned(), Instant::now()));
        drop(st);
        eprintln!("error: {msg}");
        self.toast(&format!("Error: {msg}"));
    }

    /// Calls a method that returns a player snapshot and applies it.
    fn call_status(self: &Rc<Self>, method: &str, params: Value) {
        let app = self.clone();
        let fut = self.engine.call(method, params);
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(v) => {
                    if let Ok(s) = serde_json::from_value::<Status>(v) {
                        app.apply_status(&s);
                    }
                }
                Err(e) => {
                    // A failed track change: let the engine's (old) track show again.
                    app.st.borrow_mut().left.clear();
                    app.error(&e);
                }
            }
        });
    }

    fn call_quiet(self: &Rc<Self>, method: &str, params: Value) {
        let app = self.clone();
        let fut = self.engine.call(method, params);
        glib::spawn_future_local(async move {
            if let Err(e) = fut.await {
                app.error(&e);
            }
        });
    }

    // ---- now playing ----

    fn apply_status(self: &Rc<Self>, s: &Status) {
        if s.state.is_empty() {
            return; // not a snapshot (e.g. {shuffle: true})
        }
        self.set_authorized(s.authorized);
        if let Some(seq) = s.seq {
            let mut st = self.st.borrow_mut();
            if seq < st.seq {
                // Read before a state change the UI has already seen (e.g. the reply to
                // "play" arriving after the "paused" event): playback fields are stale.
                return;
            }
            st.seq = seq;
        }
        if let Some(it) = &s.now_playing {
            if !self.is_left(it) {
                self.show_now_playing(Some(it));
            }
        } else if matches!(s.state.as_str(), "none" | "stopped" | "completed") {
            self.clear_now_playing_later();
        }
        self.show_state(&s.state);
        self.show_time(s.position, s.duration);
        if let Some(a) = s.autoplay {
            self.panel.autoplay.set_active(a);
        }
        if let Some(r) = s.repeat {
            self.st.borrow_mut().repeat = r;
            self.bar.show_repeat(r);
        }
        if let Some(sh) = s.shuffle {
            self.bar.shuffle.set_active(sh);
        }
    }

    fn show_state(&self, state: &str) {
        let busy = matches!(state, "playing" | "loading" | "waiting" | "seeking");
        {
            let mut st = self.st.borrow_mut();
            let playing = state == "playing";
            if st.playing != playing {
                // Freeze (or restart) the interpolated position where it is now.
                if st.playing {
                    st.pos = (st.pos + st.pos_at.elapsed().as_secs_f64()).min(st.duration.max(st.pos));
                }
                st.pos_at = Instant::now();
            }
            st.playing = playing;
            st.busy = busy;
        }
        let has_item = self.st.borrow().now.is_some();
        mpris::set_state(busy, has_item);
        tray::set_state(busy);
        self.full.set_playing(busy);
        self.mini.set_playing(busy);
        scrobble::playing(state == "playing");
        self.push_presence();
        desktop::on_state(self, state == "playing");
        let (icon, tip) = if busy {
            ("media-playback-pause-symbolic", "Pause")
        } else {
            ("media-playback-start-symbolic", "Play")
        };
        self.bar.play.set_icon_name(icon);
        self.bar.play.set_tooltip_text(Some(tip));
    }

    /// Updates the bar; also called optimistically with the clicked item.
    fn show_now_playing(self: &Rc<Self>, it: Option<&Item>) {
        self.st.borrow_mut().clear_gen += 1; // cancels a pending reset
        let Some(it) = it else {
            self.bar.show_favourite(None);
            self.bar.star.set_sensitive(false);
            self.bar.title.set_label("Not Playing");
            self.bar.sub.set_label("");
            self.bar.art.clear();
            {
                let mut st = self.st.borrow_mut();
                st.now = None;
                st.now_art = None;
            }
            mpris::set_track(None, 0.0);
            tray::set_track(None);
            self.full.set_track(&self.art, None);
            self.mini.set_track(&self.art, None);
            scrobble::track(None, 0.0);
            self.push_presence();
            return;
        };
        let changed = self.st.borrow().now.as_ref().map(|n| n.song_play_id()) != Some(it.song_play_id());
        self.bar.star.set_sensitive(true);
        self.bar.title.set_label(it.title());
        self.bar.title.set_tooltip_text(Some(it.title()));
        self.bar.sub.set_label(&song_subtitle(it));
        self.st.borrow_mut().now = Some(it.clone());
        let duration = if changed { 0.0 } else { self.st.borrow().duration };
        mpris::set_track(Some(it), duration);
        tray::set_track(Some(it));
        self.full.set_track(&self.art, Some(it));
        self.mini.set_track(&self.art, Some(it));
        scrobble::track(Some(it), duration);
        if changed {
            desktop::track_changed(self, it);
            {
                let mut st = self.st.borrow_mut();
                st.pos = 0.0;
                st.pos_at = Instant::now();
            }
            self.push_presence();
            if self.panel_visible("lyrics") || self.full.lyrics_wanted() {
                self.load_lyrics();
            }
            self.load_favourite(it);
        }
        let url = it.art(px(BAR_ART));
        if url.is_none() || self.st.borrow().now_art == url {
            return;
        }
        self.st.borrow_mut().now_art = url.clone();
        let weak = Rc::downgrade(self);
        let expected = url.clone();
        load_art(&self.art, &self.bar.art, url, move || {
            weak.upgrade().is_some_and(|app| app.st.borrow().now_art == expected)
        });
    }

    /// Shows a track the user just started before the engine has switched to it.
    fn show_upcoming(self: &Rc<Self>, it: &Item) {
        {
            let mut st = self.st.borrow_mut();
            let (prev, next) = (st.now.as_ref().map(Item::song_play_id), it.song_play_id());
            // Rapid skips: every track passed over stays stale until the engine catches up.
            st.left.retain(|(id, _)| *id != next);
            if let Some(p) = prev.filter(|p| *p != next) {
                st.left.push((p, Instant::now()));
            }
        }
        self.show_now_playing(Some(it));
    }

    /// True for a track a user-started change just left: until the engine reports
    /// another one (or the change has had time to fail), its snapshots are stale.
    fn is_left(&self, it: &Item) -> bool {
        let mut st = self.st.borrow_mut();
        st.left.retain(|(_, at)| at.elapsed() < Duration::from_secs(8));
        let id = it.song_play_id();
        if st.left.iter().any(|(l, _)| *l == id) {
            return true;
        }
        st.left.clear();
        false
    }

    /// Fills the bar's star from the track's rating.
    fn load_favourite(self: &Rc<Self>, it: &Item) {
        self.bar.show_favourite(None);
        let Some(id) = it.catalog_song_id() else { return };
        let app = self.clone();
        let fut = self.engine.call("rating", json!({ "id": &id, "type": "songs" }));
        glib::spawn_future_local(async move {
            let Ok(v) = fut.await else { return };
            let current = app.st.borrow().now.as_ref().and_then(Item::catalog_song_id);
            if current.as_deref() == Some(id.as_str()) {
                app.bar.show_favourite(Some(v.get("value").and_then(Value::as_i64).unwrap_or(0) > 0));
            }
        });
    }

    /// Between two tracks MusicKit briefly reports no item; only say "Not Playing" if
    /// nothing has started a moment later.
    fn clear_now_playing_later(self: &Rc<Self>) {
        let generation = {
            let mut st = self.st.borrow_mut();
            st.clear_gen += 1;
            st.clear_gen
        };
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            let Some(app) = weak.upgrade() else { return };
            let stale = {
                let st = app.st.borrow();
                st.clear_gen != generation || st.busy
            };
            if !stale {
                app.show_now_playing(None);
            }
        });
    }

    /// Play / pause by what the button shows, updating the bar right away.
    fn toggle_play(self: &Rc<Self>) {
        let busy = self.st.borrow().busy;
        if busy {
            self.show_state("paused");
            self.call_status("pause", json!({}));
        } else {
            self.show_state("loading");
            self.call_status("resume", json!({}));
        }
        // Belt and braces: re-read the real state once the command has settled.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(app) = weak.upgrade() {
                app.call_status("status", json!({}));
            }
        });
    }

    /// Next / previous; "next" shows the upcoming track at once instead of an empty bar.
    fn skip(self: &Rc<Self>, forward: bool) {
        if forward {
            let next = {
                let st = self.st.borrow();
                let p = st.queue.position;
                (p >= 0).then(|| st.queue.items.get(p as usize + 1).cloned()).flatten()
            };
            if let Some(it) = next {
                self.show_upcoming(&it);
            }
        }
        self.call_status(if forward { "next" } else { "previous" }, json!({}));
    }

    /// Sends the slider's volume (through the loudness curve), or 0 when muted.
    fn push_volume(self: &Rc<Self>) {
        let v = if self.bar.mute.is_active() { 0.0 } else { slider_to_volume(self.bar.vol.value()) };
        self.call_quiet("volume", json!({ "value": v }));
    }

    /// On engine start: restore the saved slider position, or adopt the page's volume.
    fn init_volume(self: &Rc<Self>, engine_volume: Option<f64>) {
        let first = !std::mem::replace(&mut self.st.borrow_mut().volume_ready, true);
        match self.cache.get::<f64>("pref:volume") {
            Some(s) => {
                if first {
                    self.bar.vol.set_value(s.clamp(0.0, 1.0));
                }
                self.push_volume();
            }
            None if first => {
                if let Some(v) = engine_volume.filter(|v| v.is_finite()) {
                    self.bar.vol.set_value(volume_to_slider(v));
                }
            }
            None => self.push_volume(),
        }
        self.bar.show_volume_icon();
    }

    fn show_time(&self, position: Option<f64>, duration: Option<f64>) {
        let mut st = self.st.borrow_mut();
        if let Some(d) = duration.filter(|d| d.is_finite() && *d > 0.0) {
            if (d - st.duration).abs() > 0.5 {
                st.duration = d;
                self.bar.seek.set_range(0.0, d);
                mpris::set_track(st.now.as_ref(), d);
                scrobble::track(st.now.as_ref(), d);
                let app = app();
                glib::idle_add_local_once(move || {
                    if let Some(a) = app {
                        a.push_presence();
                    }
                });
            }
            self.bar.dur.set_label(&fmt_time(d));
        }
        if let Some(p) = position.filter(|p| p.is_finite()) {
            st.pos = p;
            st.pos_at = Instant::now();
        }
        if st.seek_held || Instant::now() < st.seek_guard {
            return;
        }
        if let Some(p) = position.filter(|p| p.is_finite()) {
            self.bar.seek.set_value(p);
            self.bar.pos.set_label(&fmt_time(p));
            let d = st.duration;
            drop(st);
            if self.is_fullscreen() {
                self.full.set_time(p, d);
            }
            if self.mini.window.is_visible() {
                self.mini.set_time(p, d);
            }
        }
    }

    /// Playback position now, interpolated between the engine's time events.
    pub fn position(&self) -> f64 {
        let st = self.st.borrow();
        if st.playing { (st.pos + st.pos_at.elapsed().as_secs_f64()).min(st.duration.max(st.pos)) } else { st.pos }
    }

    /// Discord status for what plays now.
    fn push_presence(&self) {
        let (now, playing, dur) = {
            let st = self.st.borrow();
            (st.now.clone(), st.playing, st.duration)
        };
        discord::update(now.as_ref(), playing, self.position(), dur);
    }

    /// Smooth seek bar and lyrics between the engine's ~1/s time events.
    fn start_ticker(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(200), move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            let (pos, guard) = {
                let st = app.st.borrow();
                if !st.playing {
                    return glib::ControlFlow::Continue;
                }
                let est = (st.pos + st.pos_at.elapsed().as_secs_f64()).min(st.duration.max(0.0));
                (est, st.seek_held || Instant::now() < st.seek_guard)
            };
            if !guard {
                app.bar.seek.set_value(pos);
                app.bar.pos.set_label(&fmt_time(pos));
            }
            let dur = app.st.borrow().duration;
            if app.is_fullscreen() {
                app.full.set_time(pos, dur);
            }
            if app.mini.window.is_visible() {
                app.mini.set_time(pos, dur);
            }
            scrobble::tick();
            glib::ControlFlow::Continue
        });
    }

    pub fn seek_to(self: &Rc<Self>, seconds: f64) {
        {
            let mut st = self.st.borrow_mut();
            st.pos = seconds;
            st.pos_at = Instant::now();
        }
        mpris::seeked(seconds);
        self.push_presence();
        self.call_status("seek", json!({ "seconds": seconds }));
    }

    pub fn set_shuffle(self: &Rc<Self>, on: bool) {
        if self.bar.shuffle.is_active() != on {
            self.bar.shuffle.set_active(on);
        }
        self.call_quiet("shuffle", json!({ "on": on }));
    }

    // ---- side panel ----

    fn panel_visible(&self, page: &str) -> bool {
        self.overlay.shows_sidebar() && self.panel.page() == page
    }

    fn toggle_panel(self: &Rc<Self>, page: &str) {
        if self.panel_visible(page) {
            self.overlay.set_show_sidebar(false);
        } else {
            self.open_panel(page);
        }
    }

    fn open_panel(self: &Rc<Self>, page: &str) {
        self.panel.set_page(page);
        self.overlay.set_show_sidebar(true);
        self.bar.lyrics.set_active(page == "lyrics");
        self.bar.queue.set_active(page == "queue");
        if page == "lyrics" {
            self.st.borrow_mut().lyrics_for = None;
            self.load_lyrics();
        } else {
            let q = self.st.borrow().queue.clone();
            self.panel.show_queue(&self.art, &q);
            let app = self.clone();
            let fut = self.engine.call("queue", json!({}));
            glib::spawn_future_local(async move {
                if let Ok(q) = fut.await.and_then(|v| serde_json::from_value::<Queue>(v).map_err(|e| e.to_string())) {
                    app.show_queue(q);
                }
            });
        }
    }

    fn show_queue(&self, q: Queue) {
        if let Some(a) = q.autoplay {
            self.panel.autoplay.set_active(a);
        }
        if self.panel_visible("queue") {
            self.panel.show_queue(&self.art, &q);
        }
        self.st.borrow_mut().queue = q;
    }

    /// Queue edits (clear, autoplay) that answer with the new queue.
    fn call_queue(self: &Rc<Self>, method: &'static str, params: Value) {
        let app = self.clone();
        let fut = self.engine.call(method, params);
        glib::spawn_future_local(async move {
            match fut.await.and_then(|v| serde_json::from_value::<Queue>(v).map_err(|e| e.to_string())) {
                Ok(q) => app.show_queue(q),
                Err(e) => app.error(&e),
            }
        });
    }

    pub fn queue_jump(self: &Rc<Self>, index: u32) {
        let it = self.st.borrow().queue.items.get(index as usize).cloned();
        if let Some(it) = it {
            self.show_upcoming(&it);
        }
        self.call_status("queueJump", json!({ "index": index }));
    }

    pub fn load_lyrics(self: &Rc<Self>) {
        let now = self.st.borrow().now.clone();
        let Some(id) = now.as_ref().and_then(Item::catalog_song_id) else {
            self.panel.show_lyrics(None);
            self.full.lyrics.show(None);
            return;
        };
        if self.st.borrow().lyrics_for.as_deref() == Some(id.as_str()) {
            return;
        }
        self.st.borrow_mut().lyrics_for = Some(id.clone());
        self.panel.show_lyrics_loading();
        self.full.lyrics.show_loading();
        let weak = Rc::downgrade(self);
        let expected = id.clone();
        let render = move |l: &Lyrics| {
            if let Some(app) = weak.upgrade() {
                if app.st.borrow().lyrics_for.as_deref() == Some(expected.as_str()) {
                    app.panel.show_lyrics(Some(l.clone()));
                    app.full.lyrics.show(Some(l.clone()));
                }
            }
        };
        let weak = Rc::downgrade(self);
        let fail = move |_: &str| {
            if let Some(app) = weak.upgrade() {
                app.panel.show_lyrics(None);
                app.full.lyrics.show(None);
            }
        };
        // lyrics2: entries with syllable timings (older cache entries have lines only).
        self.load_swr(format!("lyrics2:{id}"), 7 * 24 * 3600, "lyrics", json!({ "id": id }), render, fail);
    }

    // ---- playback ----

    fn play_songs(self: &Rc<Self>, ids: Vec<String>, start: u32) {
        if ids.is_empty() {
            return;
        }
        self.call_status("playSongs", json!({ "ids": ids, "startWith": start }));
    }

    /// Starts a track from a (possibly long) list: queues a window of songs from there.
    pub fn play_from_model(self: &Rc<Self>, model: &impl IsA<gio::ListModel>, pos: u32) {
        let end = (pos + QUEUE_WINDOW).min(model.n_items());
        let items: Vec<Item> = (pos..end).filter_map(|i| item_at(model, i)).collect();
        if let Some(first) = items.first() {
            self.set_shuffle(false);
            self.show_upcoming(first);
        }
        self.play_songs(items.iter().map(Item::song_play_id).collect(), 0);
    }

    /// Plays an album/playlist from track `start`; falls back to queueing its songs.
    fn play_container(self: &Rc<Self>, id: &str, ctype: &str, start: u32, tracks: Vec<Item>) {
        if let Some(t) = tracks.get(start as usize) {
            self.show_upcoming(t);
        }
        let app = self.clone();
        let fut = self.engine.call("playContainer", json!({ "id": id, "type": ctype, "startWith": start }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(v) => {
                    if let Ok(s) = serde_json::from_value::<Status>(v) {
                        app.apply_status(&s);
                    }
                }
                Err(e) if !tracks.is_empty() => {
                    eprintln!("playContainer failed ({e}), queueing songs instead");
                    let ids: Vec<String> = tracks.iter().skip(start as usize).map(Item::song_play_id).collect();
                    app.play_songs(ids, 0);
                }
                Err(e) => app.error(&e),
            }
        });
    }

    // ---- library ----

    fn load_cached(self: &Rc<Self>) {
        self.st.borrow_mut().pins_expanded = self.cache.get::<bool>("pins:expanded").unwrap_or(true);
        let pins = self.cache.get::<Vec<Item>>("pins").unwrap_or_default();
        self.set_pins(pins);
        let expanded = self.st.borrow().pins_expanded;
        self.set_pins_expanded(expanded);
        for (key, list) in &self.lists {
            if let Some(items) = self.cache.get::<Vec<Item>>(&format!("library:{key}")) {
                list.set_items(&items);
                if *key == "playlists" {
                    self.set_sidebar_playlists(&items);
                }
            }
        }
    }

    fn set_sidebar_playlists(&self, items: &[Item]) {
        if self.st.borrow().playlists == items {
            return;
        }
        // "Favourite Songs" has its own fixed row under All Playlists.
        self.st.borrow_mut().favourites = items.iter().find(|it| is_favourites(it)).cloned();
        let mut i = 0;
        while let Some(row) = self.sidebar.row_at_index(i) {
            if row.widget_name().starts_with("pl:") {
                self.sidebar.remove(&row);
            } else {
                i += 1;
            }
        }
        for it in items.iter().filter(|it| !is_favourites(it)) {
            self.sidebar.append(&sidebar_row(&format!("pl:{}", it.id), "view-list-symbolic", it.title()));
        }
        self.st.borrow_mut().playlists = items.to_vec();
    }

    // ---- pins ----

    /// pins / pin / unpin: all answer with the current pins from Apple.
    fn call_pins(self: &Rc<Self>, method: &'static str, params: Value) {
        let app = self.clone();
        let fut = self.engine.call(method, params);
        glib::spawn_future_local(async move {
            match fut.await.and_then(|v| serde_json::from_value::<Vec<Item>>(v).map_err(|e| e.to_string())) {
                Ok(pins) => {
                    app.set_pins(pins);
                    match method {
                        "pin" => {
                            app.set_pins_expanded(true);
                            app.toast("Pinned");
                        }
                        "unpin" => app.toast("Unpinned"),
                        _ => {}
                    }
                }
                Err(e) if method == "pin" && e.contains("400") => app.toast("Can’t pin more items"),
                Err(e) => app.error(&format!("pins: {e}")),
            }
        });
    }

    /// Rebuilds the pinned rows under "Pins" and saves them.
    fn set_pins(&self, pins: Vec<Item>) {
        if self.st.borrow().pins == pins && pins.iter().all(|p| self.sidebar_has(&format!("pin:{}", p.id))) {
            return;
        }
        let mut i = 0;
        let mut at = None;
        while let Some(row) = self.sidebar.row_at_index(i) {
            let name = row.widget_name();
            if name.starts_with("pin:") {
                self.sidebar.remove(&row);
                continue;
            }
            if name == "pins" {
                at = Some(i + 1);
            }
            i += 1;
        }
        let expanded = self.st.borrow().pins_expanded;
        if let Some(at) = at {
            for (n, it) in pins.iter().enumerate() {
                let row = sidebar_row(&format!("pin:{}", it.id), kind_icon(it), it.title());
                if let Some(b) = row.child() {
                    b.set_margin_start(16);
                }
                row.set_visible(expanded);
                self.sidebar.insert(&row, at + n as i32);
            }
        }
        self.cache.put("pins", &pins);
        self.st.borrow_mut().pins = pins;
    }

    fn sidebar_has(&self, name: &str) -> bool {
        let mut i = 0;
        while let Some(row) = self.sidebar.row_at_index(i) {
            if row.widget_name() == name {
                return true;
            }
            i += 1;
        }
        false
    }

    fn set_pins_expanded(&self, expanded: bool) {
        self.st.borrow_mut().pins_expanded = expanded;
        self.cache.put("pins:expanded", &expanded);
        let mut i = 0;
        while let Some(row) = self.sidebar.row_at_index(i) {
            let name = row.widget_name();
            if name == "pins" {
                // Arrow at the end of the row.
                if let Some(arrow) = row.child().and_then(|b| b.last_child()).and_downcast::<gtk::Image>() {
                    arrow.set_icon_name(Some(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" }));
                }
            } else if name.starts_with("pin:") {
                row.set_visible(expanded);
            }
            i += 1;
        }
    }

    // ---- preferences ----

    /// Preferences: just the equalizer for now.
    fn show_preferences(&self) {
        let dialog = adw::PreferencesDialog::builder().title("Equalizer").build();
        if let Some(eq_page) = eq::page() {
            dialog.add(&eq_page);
        }
        dialog.set_content_width(980);
        dialog.set_content_height(860);
        dialog.connect_closed(|_| eq::dialog_closed());
        dialog.present(Some(&self.window));
    }

    fn sync_library(self: &Rc<Self>) {
        {
            let mut st = self.st.borrow_mut();
            if st.authorized != Some(true) {
                return;
            }
            if st.syncing {
                st.resync = true;
                return;
            }
            st.syncing = true;
        }
        self.call_pins("pins", json!({}));
        // All kinds are requested at once; the engine pages each one in parallel.
        let calls: Vec<_> = LibKind::ALL
            .iter()
            .map(|k| (*k, self.engine.call("libraryAll", json!({ "kind": k.key() }))))
            .collect();
        for list in self.lists.values() {
            if list.is_empty() {
                list.show_status("content-loading-symbolic", "Loading library…", "");
            }
        }
        let app = self.clone();
        glib::spawn_future_local(async move {
            let mut prefetch = Vec::new();
            for (kind, fut) in calls {
                let list = app.lists[kind.key()].clone();
                match fut.await.and_then(|v| serde_json::from_value::<LibraryPage>(v).map_err(|e| e.to_string())) {
                    Ok(page) => {
                        let mut items = page.items;
                        {
                            let mut st = app.st.borrow_mut();
                            st.removed.retain(|(_, at)| at.elapsed() < Duration::from_secs(90));
                            items.retain(|x| !st.removed.iter().any(|(id, _)| *id == x.id));
                        }
                        if !kind.keep_order() {
                            items.sort_by_cached_key(|it| it.title().to_lowercase());
                        }
                        app.cache.put(&format!("library:{}", kind.key()), &items);
                        list.set_items(&items);
                        match kind {
                            LibKind::Playlists => {
                                app.set_sidebar_playlists(&items);
                                prefetch.extend(items.iter().take(6).cloned());
                            }
                            LibKind::Recent => prefetch.extend(items.iter().take(6).cloned()),
                            _ => {}
                        }
                    }
                    Err(e) => {
                        app.error(&format!("library ({}): {e}", kind.key()));
                        if list.is_empty() {
                            list.show_status("dialog-warning-symbolic", "Couldn’t Load", &e);
                        }
                    }
                }
            }
            let again = {
                let mut st = app.st.borrow_mut();
                st.syncing = false;
                std::mem::take(&mut st.resync)
            };
            if again {
                app.sync_library();
            }
            app.prefetch_containers(prefetch);
        });
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}
