// Desktop integration driven by the preferences: playing in the background (closing the
// window only hides it), starting hidden at login, track notifications and keeping the
// computer awake while music plays.
use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use gtk::{gio, glib};

use super::App;
use crate::{model::Item, settings};

thread_local! {
    /// Cookie of the running "don't suspend" inhibitor (0 = none).
    static INHIBIT: Cell<u32> = const { Cell::new(0) };
}

// ---- window ---------------------------------------------------------------------------------

/// Closing the window hides it while "play in the background" is on.
pub fn connect_close(app: &Rc<App>) {
    app.window.connect_close_request(|w| {
        if settings::with(|s| s.background) {
            w.set_visible(false);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
}

/// Started from the login autostart entry with "start in the background" on.
pub fn start_hidden(autostart: bool) -> bool {
    autostart && settings::with(|s| s.background && s.start_minimized)
}

/// Shows the window, or hides it when it is already in front (tray click).
pub fn toggle_window(app: &Rc<App>) {
    if app.window.is_visible() && app.window.is_active() {
        app.window.set_visible(false);
    } else {
        app.window.present();
    }
}

// ---- playback state ---------------------------------------------------------------------------

/// Playing or not: holds a suspend inhibitor while music plays (if enabled).
pub fn on_state(app: &App, playing: bool) {
    let want = playing && settings::with(|s| s.inhibit_suspend);
    let Some(gapp) = app.window.application() else { return };
    INHIBIT.with(|c| {
        let cookie = c.get();
        if want && cookie == 0 {
            c.set(gapp.inhibit(Some(&app.window), gtk::ApplicationInhibitFlags::SUSPEND, Some("Playing music")));
        } else if !want && cookie != 0 {
            gapp.uninhibit(cookie);
            c.set(0);
        }
    });
}

/// A new track started: notification (if enabled and the window is not in front).
pub fn track_changed(app: &Rc<App>, it: &Item) {
    if !settings::with(|s| s.notifications) || app.window.is_active() {
        return;
    }
    let Some(gapp) = app.window.application() else { return };
    let n = gio::Notification::new(it.title());
    let body = [it.artist(), it.album.as_deref().unwrap_or("")]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" — ");
    n.set_body(Some(&body));
    n.set_priority(gio::NotificationPriority::Low);
    n.set_default_action("app.show-window");
    n.add_button("Next", "app.next");
    let id = it.song_play_id();
    match it.art(256) {
        Some(url) => {
            let weak = Rc::downgrade(app);
            app.art.load(&url, move |tex| {
                // Still the same track by the time the art arrived?
                let Some(app) = weak.upgrade() else { return };
                if app.st.borrow().now.as_ref().map(Item::song_play_id).as_deref() != Some(id.as_str()) {
                    return;
                }
                n.set_icon(&gio::BytesIcon::new(&tex.save_to_png_bytes()));
                gapp.send_notification(Some("now-playing"), &n);
            });
        }
        None => gapp.send_notification(Some("now-playing"), &n),
    }
}

/// App-level actions the notification buttons and the tray menu use (they work with
/// the window hidden, unlike the window's own actions).
pub fn add_app_actions(app: &Rc<App>, gapp: &adw::Application) {
    let add = |name: &str, f: fn(&Rc<App>)| {
        let a = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(app);
        a.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                f(&app);
            }
        });
        gapp.add_action(&a);
    };
    add("show-window", |app| app.window.present());
    add("toggle", |app| app.toggle_play());
    add("next", |app| app.skip(true));
    add("previous", |app| app.skip(false));
    add("quit", |app| {
        if let Some(gapp) = app.window.application() {
            gapp.quit();
        }
    });
}
