mod art;
mod cache;
mod discord;
mod engine;
mod model;
mod scrobble;
mod settings;
mod ui;

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
};

use adw::prelude::*;
use gtk::{gdk, glib};

pub const APP_ID: &str = "io.github.traceofc.Applejack";

/// ~/.cache/applejack (library cache, artwork, engine log). Moves the pre-rename
/// ~/.cache/am-gtk over on first start so the library does not reload from scratch.
pub fn cache_dir() -> PathBuf {
    let base = glib::user_cache_dir();
    let dir = base.join("applejack");
    let old = base.join("am-gtk");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

/// Symbolic icons Adwaita / Breeze don't have, written next to the cache and added to the
/// icon theme (symbolic recolouring works as for any themed icon).
fn install_icons() {
    const ICONS: &[(&str, &str)] = &[(
        "applejack-mini-player-symbolic",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#222" d="M3 2C1.34 2 0 3.34 0 5v6c0 1.66 1.34 3 3 3h10c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3zm0 2h10c.55 0 1 .45 1 1v6c0 .55-.45 1-1 1H3c-.55 0-1-.45-1-1V5c0-.55.45-1 1-1z"/><rect fill="#222" x="8" y="7" width="5" height="4" rx="1"/></svg>"##,
    )];
    // Unthemed icons: straight in a search-path folder.
    let dir = cache_dir().join("icons");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, svg) in ICONS {
        let path = dir.join(format!("{name}.svg"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = std::fs::write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}

fn main() -> glib::ExitCode {
    glib::set_application_name("Applejack");
    let app = adw::Application::builder().application_id(APP_ID).build();
    // --autostart: launched by the login entry (see settings::set_autostart).
    app.add_main_option("autostart", glib::Char::from(0), glib::OptionFlags::NONE, glib::OptionArg::None, "Started at login", None);
    let autostart = Rc::new(Cell::new(false));
    let flag = autostart.clone();
    app.connect_handle_local_options(move |_, opts| {
        flag.set(opts.contains("autostart"));
        std::ops::ControlFlow::Continue(())
    });
    app.connect_startup(|_| {
        // Running from the source tree: the icon is not installed into hicolor yet.
        let dev_icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/icons");
        if let Some(display) = gdk::Display::default().filter(|_| dev_icons.is_dir()) {
            gtk::IconTheme::for_display(&display).add_search_path(dev_icons);
        }
        gtk::Window::set_default_icon_name(APP_ID);
        install_icons();
    });
    app.connect_activate(move |app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        ui::App::build(app, autostart.get());
    });
    app.run()
}
