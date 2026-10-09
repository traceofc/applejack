// Preferences dialog (Ctrl+,): how Applejack behaves on the desktop, how it looks, and the
// services it reports to (Last.fm, ListenBrainz, Discord).
use std::{cell::Cell, rc::Rc, time::Duration};

use adw::prelude::*;
use gtk::glib;

use super::{App, desktop, theme, tray};
use crate::{scrobble, settings};

fn switch(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build()
}

pub fn show(app: &Rc<App>) {
    let s = settings::get();
    let page = adw::PreferencesPage::builder().title("General").icon_name("preferences-system-symbolic").build();

    let window = adw::PreferencesGroup::builder().title("Window").build();
    let background = switch("Play in the Background", "Closing the window keeps the music playing", s.background);
    let tray_row = switch("Show in the Tray", "On GNOME this needs the AppIndicator extension", s.tray);
    window.add(&background);
    window.add(&tray_row);

    let startup = adw::PreferencesGroup::builder().title("Startup").build();
    let autostart = switch("Launch at Login", "", settings::autostart_enabled());
    let minimized = switch("Start in the Background", "Only the tray icon and media controls at login", s.start_minimized);
    startup.add(&autostart);
    startup.add(&minimized);

    let playback = adw::PreferencesGroup::builder().title("Playback").build();
    let notifications = switch("Track Notifications", "When a new track starts and the window is not in front", s.notifications);
    let inhibit = switch("Prevent Sleep While Playing", "The screen can still lock and turn off", s.inhibit_suspend);
    playback.add(&notifications);
    playback.add(&inhibit);

    page.add(&window);
    page.add(&startup);
    page.add(&playback);
    page.add(&appearance_group());

    let sync_minimized = {
        let (background, autostart, minimized) = (background.clone(), autostart.clone(), minimized.clone());
        move || minimized.set_sensitive(background.is_active() && autostart.is_active())
    };
    sync_minimized();

    let sync = sync_minimized.clone();
    background.connect_active_notify(move |r| {
        settings::update(|s| s.background = r.is_active());
        sync();
    });
    tray_row.connect_active_notify(|r| {
        settings::update(|s| s.tray = r.is_active());
        tray::set_enabled(r.is_active());
    });
    let weak = Rc::downgrade(app);
    let sync = sync_minimized.clone();
    autostart.connect_active_notify(move |r| {
        if let Err(e) = settings::set_autostart(r.is_active()) {
            if let Some(app) = weak.upgrade() {
                app.toast(&format!("Couldn’t change launch at login: {e}"));
            }
        }
        sync();
    });
    minimized.connect_active_notify(|r| settings::update(|s| s.start_minimized = r.is_active()));
    notifications.connect_active_notify(|r| settings::update(|s| s.notifications = r.is_active()));
    let weak = Rc::downgrade(app);
    inhibit.connect_active_notify(move |r| {
        settings::update(|s| s.inhibit_suspend = r.is_active());
        if let Some(app) = weak.upgrade() {
            let playing = app.st.borrow().playing;
            desktop::on_state(&app, playing);
        }
    });

    let dialog = adw::PreferencesDialog::builder().title("Preferences").build();
    dialog.add(&page);
    dialog.add(&integrations_page(app));
    if std::env::var("AM_GTK_PREFS").as_deref() == Ok("integrations") {
        dialog.set_visible_page_name("integrations");
    }
    dialog.present(Some(&app.window));
}

fn appearance_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Appearance").build();
    let schemes = ["system", "light", "dark"];
    let style = adw::ComboRow::builder()
        .title("Style")
        .model(&gtk::StringList::new(&["Follow System", "Light", "Dark"]))
        .build();
    let cur = settings::with(|s| s.color_scheme.clone());
    style.set_selected(schemes.iter().position(|x| *x == cur).unwrap_or(0) as u32);
    style.connect_selected_notify(move |r| {
        let v = schemes.get(r.selected() as usize).copied().unwrap_or("system");
        settings::update(|s| s.color_scheme = v.into());
        theme::apply();
    });
    group.add(&style);
    if theme::is_kde() {
        let plasma = switch(
            "Use Plasma Colours",
            "Window, sidebar and accent colours from the Plasma colour scheme",
            settings::with(|s| s.plasma_colors),
        );
        plasma.connect_active_notify(|r| {
            settings::update(|s| s.plasma_colors = r.is_active());
            theme::apply();
        });
        group.add(&plasma);
    }
    group
}

fn link_row(title: &str, subtitle: &str, url: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row.connect_activated(move |r| {
        let win = r.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(url).launch(win.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
    });
    row
}

fn integrations_page(app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("integrations")
        .title("Integrations")
        .icon_name("network-transmit-receive-symbolic")
        .build();
    page.add(&lastfm_group(app));
    page.add(&listenbrainz_group(app));
    page.add(&discord_group(app));
    page
}

fn lastfm_group(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Last.fm")
        .description("Scrobbles a song once you have listened to half of it (or four minutes)")
        .build();
    let s = settings::get();
    let enabled = switch("Scrobble to Last.fm", "", s.lastfm_enabled);
    let account = adw::ActionRow::builder().title("Account").build();
    let connect = gtk::Button::builder().valign(gtk::Align::Center).build();
    account.add_suffix(&connect);
    let own = adw::ExpanderRow::builder()
        .title("Your Own API Account")
        .subtitle("Needed when this build has no Last.fm API key built in")
        .build();
    let key = adw::EntryRow::builder().title("API Key").text(&s.lastfm_key).show_apply_button(true).build();
    let secret = adw::PasswordEntryRow::builder().title("Shared Secret").text(&s.lastfm_secret).show_apply_button(true).build();
    own.add_row(&key);
    own.add_row(&secret);
    own.add_row(&link_row("Create an API Account", "last.fm/api/account/create", "https://www.last.fm/api/account/create"));
    own.set_expanded(scrobble::lastfm_credentials().0.is_empty());
    group.add(&enabled);
    group.add(&account);
    group.add(&own);

    let waiting = Rc::new(Cell::new(false));
    let show = {
        let (account, connect, enabled, waiting) = (account.clone(), connect.clone(), enabled.clone(), waiting.clone());
        move || {
            let (user, connected) = settings::with(|s| (s.lastfm_user.clone(), !s.lastfm_session.is_empty()));
            if waiting.get() {
                account.set_subtitle("Allow access in your browser, then come back here");
                connect.set_label("Cancel");
                connect.remove_css_class("suggested-action");
            } else if connected {
                account.set_subtitle(&format!("Connected as {user}"));
                connect.set_label("Disconnect");
                connect.remove_css_class("suggested-action");
            } else {
                account.set_subtitle("Not connected");
                connect.set_label("Connect");
                connect.add_css_class("suggested-action");
            }
            enabled.set_sensitive(connected);
        }
    };
    let show = Rc::new(show);
    show();
    enabled.connect_active_notify(|r| settings::update(|s| s.lastfm_enabled = r.is_active()));
    key.connect_apply(|e| settings::update(|s| s.lastfm_key = e.text().trim().to_owned()));
    secret.connect_apply(|e| settings::update(|s| s.lastfm_secret = e.text().trim().to_owned()));

    let weak = Rc::downgrade(app);
    let own_w = own.downgrade();
    connect.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        if waiting.get() {
            waiting.set(false);
            show();
            return;
        }
        if settings::with(|s| !s.lastfm_session.is_empty()) {
            settings::update(|s| {
                s.lastfm_session.clear();
                s.lastfm_user.clear();
            });
            show();
            return;
        }
        if scrobble::lastfm_credentials().0.is_empty() {
            app.toast("Enter your Last.fm API key and secret first");
            if let Some(o) = own_w.upgrade() {
                o.set_expanded(true);
            }
            return;
        }
        waiting.set(true);
        show();
        let (show, waiting) = (show.clone(), waiting.clone());
        glib::spawn_future_local(async move {
            let (token, url) = match scrobble::off_thread(scrobble::lastfm_begin_auth).await {
                Ok(t) => t,
                Err(e) => {
                    app.toast(&format!("Last.fm: {e}"));
                    waiting.set(false);
                    show();
                    return;
                }
            };
            gtk::UriLauncher::new(&url).launch(Some(&app.window), gtk::gio::Cancellable::NONE, |_| {});
            // Wait for the user to allow access (up to five minutes).
            for _ in 0..100 {
                glib::timeout_future(Duration::from_secs(3)).await;
                if !waiting.get() {
                    return;
                }
                let t = token.clone();
                match scrobble::off_thread(move || scrobble::lastfm_finish_auth(&t)).await {
                    Ok((sk, name)) => {
                        settings::update(|s| {
                            s.lastfm_session = sk;
                            s.lastfm_user = name.clone();
                            s.lastfm_enabled = true;
                        });
                        app.toast(&format!("Scrobbling to Last.fm as {name}"));
                        break;
                    }
                    Err(None) => continue,
                    Err(Some(e)) => {
                        app.toast(&format!("Last.fm: {e}"));
                        break;
                    }
                }
            }
            waiting.set(false);
            show();
        });
    });
    group
}

fn listenbrainz_group(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("ListenBrainz").build();
    let s = settings::get();
    let enabled = switch("Submit Listens to ListenBrainz", "", s.listenbrainz_enabled);
    let token = adw::PasswordEntryRow::builder().title("User Token").text(&s.listenbrainz_token).show_apply_button(true).build();
    let status = adw::ActionRow::builder().title("Account").build();
    let server = adw::EntryRow::builder()
        .title("Server (empty = listenbrainz.org; Maloja, Koito … work too)")
        .text(&s.listenbrainz_url)
        .show_apply_button(true)
        .build();
    group.add(&enabled);
    group.add(&token);
    group.add(&status);
    group.add(&link_row("Get Your Token", "listenbrainz.org/settings", "https://listenbrainz.org/settings/"));
    group.add(&server);
    server.connect_apply(|e| settings::update(|s| s.listenbrainz_url = e.text().trim().to_owned()));
    let show = {
        let (status, enabled) = (status.clone(), enabled.clone());
        move || {
            let (user, connected) = settings::with(|s| (s.listenbrainz_user.clone(), !s.listenbrainz_token.is_empty()));
            status.set_subtitle(&if connected { format!("Connected as {user}") } else { "Not connected".into() });
            enabled.set_sensitive(connected);
        }
    };
    show();
    enabled.connect_active_notify(|r| settings::update(|s| s.listenbrainz_enabled = r.is_active()));
    let weak = Rc::downgrade(app);
    token.connect_apply(move |e| {
        let value = e.text().trim().to_owned();
        let show = show.clone();
        if value.is_empty() {
            settings::update(|s| {
                s.listenbrainz_token.clear();
                s.listenbrainz_user.clear();
            });
            show();
            return;
        }
        let weak = weak.clone();
        glib::spawn_future_local(async move {
            let (v, server) = (value.clone(), settings::with(|s| s.listenbrainz_url.clone()));
            let r = scrobble::off_thread(move || scrobble::listenbrainz_validate(&server, &v)).await;
            let Some(app) = weak.upgrade() else { return };
            match r {
                Ok(user) => {
                    settings::update(|s| {
                        s.listenbrainz_token = value;
                        s.listenbrainz_user = user.clone();
                        s.listenbrainz_enabled = true;
                    });
                    app.toast(&format!("Submitting listens to ListenBrainz as {user}"));
                }
                Err(e) => app.toast(&format!("ListenBrainz: {e}")),
            }
            show();
        });
    });
    group
}

fn discord_group(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Discord")
        .description("Shows the song in your Discord status while it plays. Discord (or Vesktop with arRPC) must be running on this computer.")
        .build();
    let s = settings::get();
    let enabled = switch("Show What You’re Listening To", "", s.discord);
    let id = adw::EntryRow::builder().title("Application ID").text(&s.discord_client_id).show_apply_button(true).build();
    group.add(&enabled);
    group.add(&id);
    group.add(&link_row(
        "Create a Discord Application",
        "Name it “Applejack” (Discord shows that name), then paste its Application ID above",
        "https://discord.com/developers/applications",
    ));
    let builtin = option_env!("APPLEJACK_DISCORD_CLIENT_ID").is_some_and(|s| !s.is_empty());
    if builtin {
        id.set_title("Application ID (optional)");
    }
    let weak = Rc::downgrade(app);
    enabled.connect_active_notify(move |r| {
        settings::update(|s| s.discord = r.is_active());
        if let Some(app) = weak.upgrade() {
            if r.is_active() && crate::discord::client_id().is_empty() {
                app.toast("Paste a Discord Application ID below");
            }
            app.push_presence();
        }
    });
    let weak = Rc::downgrade(app);
    id.connect_apply(move |e| {
        settings::update(|s| s.discord_client_id = e.text().trim().to_owned());
        if let Some(app) = weak.upgrade() {
            app.push_presence();
        }
    });
    group
}
