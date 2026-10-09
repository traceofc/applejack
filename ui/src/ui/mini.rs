// Mini player: a small window with the cover, the track and the transport controls, kept
// above other windows where the desktop allows it.
//
// Wayland has no way for an app to ask for "always on top", so it is done per desktop:
// KDE Plasma (Wayland and X11) through a tiny KWin script loaded over D-Bus, Hyprland and
// Sway through their IPC tools, other X11 desktops through wmctrl. GNOME offers it only in
// the window menu (Alt+Space → Always on Top), which the window's menu button points to.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::{App, app, widgets::{art_image, label}};
use crate::{art::ArtLoader, model::Item, settings};

pub const TITLE: &str = "Applejack Mini Player";
const ART: i32 = 84;

pub struct Mini {
    pub window: adw::Window,
    art: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    play: gtk::Button,
    progress: gtk::ProgressBar,
    art_for: RefCell<Option<String>>,
}

fn button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tip));
    b
}

impl Mini {
    pub fn new(gapp: &adw::Application) -> Rc<Self> {
        let art = art_image(ART, false);
        art.set_valign(gtk::Align::Center);
        let title = label("Not Playing", &["heading"]);
        let artist = label("", &["dim-label", "caption"]);
        title.set_max_width_chars(1);
        artist.set_max_width_chars(1);
        let prev = button("media-skip-backward-symbolic", "Previous");
        let play = button("media-playback-start-symbolic", "Play");
        let next = button("media-skip-forward-symbolic", "Next");
        let expand = button("view-fullscreen-symbolic", "Back to the Full Window");
        let on_top = gtk::ToggleButton::builder()
            .icon_name("view-pin-symbolic")
            .tooltip_text("Keep on Top")
            .active(settings::with(|s| s.mini_on_top))
            .valign(gtk::Align::Center)
            .build();
        on_top.add_css_class("flat");
        on_top.add_css_class("circular");
        let close = button("window-close-symbolic", "Close");
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        controls.append(&prev);
        controls.append(&play);
        controls.append(&next);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        controls.append(&spacer);
        controls.append(&on_top);
        controls.append(&expand);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_hexpand(true);
        texts.set_valign(gtk::Align::Center);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let tcol = gtk::Box::new(gtk::Orientation::Vertical, 2);
        tcol.set_hexpand(true);
        tcol.append(&title);
        tcol.append(&artist);
        head.append(&tcol);
        close.set_valign(gtk::Align::Start);
        head.append(&close);
        texts.append(&head);
        texts.append(&controls);
        let progress = gtk::ProgressBar::new();
        progress.add_css_class("osd");
        progress.add_css_class("mini-progress");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(10);
        row.set_margin_bottom(6);
        row.set_margin_start(10);
        row.set_margin_end(8);
        row.append(&art);
        row.append(&texts);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        col.append(&row);
        col.append(&progress);
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&col));

        let window = adw::Window::builder()
            .application(gapp)
            .title(TITLE)
            .default_width(380)
            // libadwaita's default minimum (360×200) would leave a gap under the controls.
            .width_request(300)
            .height_request(80)
            .resizable(false)
            .content(&handle)
            .build();
        window.add_css_class("mini-player");

        let mini = Rc::new(Self { window, art, title, artist, play, progress, art_for: Default::default() });
        mini.play.connect_clicked(|_| {
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
        expand.connect_clicked(|_| {
            if let Some(a) = app() {
                a.set_mini(false);
            }
        });
        close.connect_clicked(|_| {
            if let Some(a) = app() {
                a.set_mini(false);
            }
        });
        on_top.connect_toggled(|b| {
            settings::update(|s| s.mini_on_top = b.is_active());
            keep_above(b.is_active(), true);
        });
        mini.window.connect_close_request(|_| {
            if let Some(a) = app() {
                a.set_mini(false);
            }
            glib::Propagation::Stop
        });
        // Clicking the cover opens the full window as well.
        let click = gtk::GestureClick::new();
        click.connect_released(|_, n, _, _| {
            if n == 2 {
                if let Some(a) = app() {
                    a.set_mini(false);
                }
            }
        });
        mini.art.add_controller(click);
        mini
    }

    pub fn set_track(&self, art: &Rc<ArtLoader>, it: Option<&Item>) {
        let Some(it) = it else {
            self.title.set_label("Not Playing");
            self.artist.set_label("");
            self.art.clear();
            *self.art_for.borrow_mut() = None;
            return;
        };
        self.title.set_label(it.title());
        self.title.set_tooltip_text(Some(it.title()));
        self.artist.set_label(it.artist());
        let url = it.art((ART * 2) as u32);
        if *self.art_for.borrow() == url {
            return;
        }
        *self.art_for.borrow_mut() = url.clone();
        super::widgets::load_art(art, &self.art, url.clone(), move || {
            app().is_some_and(|a| a.mini.art_for.borrow().as_deref() == url.as_deref())
        });
    }

    pub fn set_playing(&self, busy: bool) {
        let (icon, tip) =
            if busy { ("media-playback-pause-symbolic", "Pause") } else { ("media-playback-start-symbolic", "Play") };
        self.play.set_icon_name(icon);
        self.play.set_tooltip_text(Some(tip));
    }

    pub fn set_time(&self, pos: f64, duration: f64) {
        self.progress.set_fraction(if duration > 0.0 { (pos / duration).clamp(0.0, 1.0) } else { 0.0 });
    }
}

impl App {
    /// Swaps the main window for the mini player and back.
    pub fn set_mini(self: &Rc<Self>, on: bool) {
        if on {
            if self.is_fullscreen() {
                self.set_fullscreen(false);
            }
            let now = self.st.borrow().now.clone();
            self.mini.set_track(&self.art, now.as_ref());
            self.mini.set_playing(self.st.borrow().busy);
            let (pos, dur) = (self.position(), self.st.borrow().duration);
            self.mini.set_time(pos, dur);
            self.mini.window.present();
            self.window.set_visible(false);
            if settings::with(|s| s.mini_on_top) {
                // After the window is mapped, so the compositor knows it.
                glib::timeout_add_local_once(std::time::Duration::from_millis(300), || keep_above(true, false));
            }
        } else {
            self.mini.window.set_visible(false);
            self.window.present();
        }
    }
}

fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase()
}

fn is_x11() -> bool {
    gdk::Display::default().is_some_and(|d| d.type_().name().contains("X11"))
}

/// Asks the desktop to keep the mini player above other windows (or not). `explain`: tell
/// the user when the desktop can't do it.
pub fn keep_above(on: bool, explain: bool) {
    let d = desktop();
    if d.contains("KDE") {
        kwin_keep_above(on);
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let sel = format!("title:^({TITLE})$");
        let cmd = if on {
            format!("dispatch setfloating {sel}; dispatch pin {sel}")
        } else {
            format!("dispatch pin {sel}")
        };
        run(&["hyprctl", "--batch", &cmd]);
    } else if std::env::var_os("SWAYSOCK").is_some() {
        let crit = format!("[title=\"^{TITLE}$\"]");
        run(&["swaymsg", &format!("{crit} floating {}", if on { "enable" } else { "disable" })]);
    } else if is_x11() && glib::find_program_in_path("wmctrl").is_some() {
        run(&["wmctrl", "-r", TITLE, "-b", if on { "add,above" } else { "remove,above" }]);
    } else if explain && on {
        if let Some(a) = app() {
            if d.contains("GNOME") {
                a.toast("GNOME doesn’t let apps stay on top: press Alt+Space on the mini player and pick Always on Top");
            } else {
                a.toast("This desktop doesn’t let apps keep a window on top");
            }
        }
    }
}

fn run(argv: &[&str]) {
    if let Err(e) = gio::Subprocess::newv(&argv.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>(), gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE) {
        eprintln!("{}: {e}", argv[0]);
    }
}

/// KWin: load a script that sets keepAbove on the mini player's window (Plasma 5 and 6).
fn kwin_keep_above(on: bool) {
    let script = format!(
        r#"function apply(w) {{ if (w && w.caption === "{TITLE}") w.keepAbove = {on}; }}
var list = workspace.windowList ? workspace.windowList() : workspace.clientList();
for (var i = 0; i < list.length; i++) apply(list[i]);
"#
    );
    let path = crate::cache_dir().join("kwin-mini-on-top.js");
    if std::fs::write(&path, script).is_err() {
        return;
    }
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return };
    const PLUGIN: &str = "applejack-mini-on-top";
    let call = |path: &str, iface: &str, method: &str, args: Option<&glib::Variant>| {
        bus.call_sync(Some("org.kde.KWin"), path, iface, method, args, None, gio::DBusCallFlags::NONE, 3000, gio::Cancellable::NONE)
    };
    let _ = call("/Scripting", "org.kde.kwin.Scripting", "unloadScript", Some(&(PLUGIN,).to_variant()));
    let id = match call("/Scripting", "org.kde.kwin.Scripting", "loadScript", Some(&(path.to_string_lossy().as_ref(), PLUGIN).to_variant())) {
        Ok(v) => v.child_value(0).get::<i32>().unwrap_or(-1),
        Err(e) => {
            eprintln!("kwin: {e}");
            return;
        }
    };
    // Plasma 6 puts scripts at /Scripting/Script<id>, Plasma 5 at /<id>.
    if call(&format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", None).is_err() {
        let _ = call(&format!("/{id}"), "org.kde.kwin.Script", "run", None);
    }
}
