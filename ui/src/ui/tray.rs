// Tray icon: a StatusNotifierItem (org.kde.StatusNotifierItem) with a com.canonical.dbusmenu
// menu, both on the application's bus connection. KDE, XFCE, Cinnamon, MATE, Budgie and
// wlroots bars show it as is; GNOME needs the AppIndicator extension (Ubuntu ships it).
// Left click shows / hides the window, middle click plays / pauses, scrolling changes
// the volume.
use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

use adw::prelude::*;
use gtk::{gio, glib};

use super::{App, app, desktop};
use crate::{model::Item, settings};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg name="status" type="s"/></signal>
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="i" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
  </interface>
  <interface name="com.canonical.dbusmenu">
    <method name="GetLayout">
      <arg name="parentId" type="i" direction="in"/>
      <arg name="recursionDepth" type="i" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="revision" type="u" direction="out"/>
      <arg name="layout" type="(ia{sv}av)" direction="out"/>
    </method>
    <method name="GetGroupProperties">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="properties" type="a(ia{sv})" direction="out"/>
    </method>
    <method name="GetProperty">
      <arg name="id" type="i" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="Event">
      <arg name="id" type="i" direction="in"/>
      <arg name="eventId" type="s" direction="in"/>
      <arg name="data" type="v" direction="in"/>
      <arg name="timestamp" type="u" direction="in"/>
    </method>
    <method name="EventGroup">
      <arg name="events" type="a(isvu)" direction="in"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <method name="AboutToShow">
      <arg name="id" type="i" direction="in"/>
      <arg name="needUpdate" type="b" direction="out"/>
    </method>
    <method name="AboutToShowGroup">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="updatesNeeded" type="ai" direction="out"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <signal name="ItemsPropertiesUpdated">
      <arg name="updatedProps" type="a(ia{sv})"/>
      <arg name="removedProps" type="a(ias)"/>
    </signal>
    <signal name="LayoutUpdated">
      <arg name="revision" type="u"/>
      <arg name="parent" type="i"/>
    </signal>
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
  </interface>
</node>"#;

// Menu item ids.
const NOW: i32 = 1;
const TOGGLE: i32 = 3;
const NEXT: i32 = 4;
const PREVIOUS: i32 = 5;
const WINDOW: i32 = 7;
const QUIT: i32 = 8;

#[derive(Default)]
struct Tray {
    conn: Option<gio::DBusConnection>,
    regs: Vec<gio::RegistrationId>,
    owner: Option<gio::OwnerId>,
    /// Stops watching for the tray host (the id type is not nameable).
    unwatch: Option<Box<dyn FnOnce()>>,
    revision: u32,
    playing: bool,
    item: Option<Item>,
}

thread_local! {
    static TRAY: RefCell<Tray> = RefCell::new(Tray::default());
}

fn bus_name() -> String {
    format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
}

/// Shows the icon if the preference is on.
pub fn init(gapp: &adw::Application) {
    if let Some(conn) = gapp.dbus_connection() {
        TRAY.with(|t| t.borrow_mut().conn = Some(conn));
    }
    set_enabled(settings::with(|s| s.tray));
}

/// Adds or removes the icon (preference switch).
pub fn set_enabled(on: bool) {
    let Some(conn) = TRAY.with(|t| t.borrow().conn.clone()) else { return };
    let shown = TRAY.with(|t| !t.borrow().regs.is_empty());
    if on == shown {
        return;
    }
    if !on {
        let (regs, owner, watcher) = TRAY.with(|t| {
            let mut t = t.borrow_mut();
            (std::mem::take(&mut t.regs), t.owner.take(), t.unwatch.take())
        });
        for r in regs {
            let _ = conn.unregister_object(r);
        }
        if let Some(o) = owner {
            gio::bus_unown_name(o);
        }
        if let Some(unwatch) = watcher {
            unwatch();
        }
        return;
    }
    let info = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(i) => i,
        Err(e) => return eprintln!("tray: {e}"),
    };
    let mut regs = Vec::new();
    for (path, iface) in [(ITEM_PATH, ITEM_IFACE), (MENU_PATH, MENU_IFACE)] {
        let Some(ii) = info.lookup_interface(iface) else { continue };
        let reg = conn
            .register_object(path, &ii)
            .method_call(|_, _, _, iface, method, params, inv| {
                let ret = call(iface.unwrap_or(""), method, &params);
                inv.return_value(ret.as_ref());
            })
            .property(|_, _, _, iface, prop| property(iface, prop))
            .build();
        match reg {
            Ok(r) => regs.push(r),
            Err(e) => {
                for r in regs {
                    let _ = conn.unregister_object(r);
                }
                return eprintln!("tray: {e}");
            }
        }
    }
    let owner = gio::bus_own_name_on_connection(&conn, &bus_name(), gio::BusNameOwnerFlags::NONE, |_, _| {}, |_, name| {
        eprintln!("tray: lost {name}")
    });
    // (Re-)register whenever a tray host appears: panel restarts, extension enabled later.
    let watcher = gio::bus_watch_name_on_connection(&conn, WATCHER, gio::BusNameWatcherFlags::NONE, |conn, _, _| register(&conn), |_, _| {});
    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        t.regs = regs;
        t.owner = Some(owner);
        t.unwatch = Some(Box::new(move || gio::bus_unwatch_name(watcher)));
    });
}

fn register(conn: &gio::DBusConnection) {
    conn.call(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        WATCHER,
        "RegisterStatusNotifierItem",
        Some(&(bus_name(),).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
        |res| {
            if let Err(e) = res {
                eprintln!("tray: couldn’t register with the tray host: {e}");
            }
        },
    );
}

// ---- state from the player ----------------------------------------------------------------------

pub fn set_state(playing: bool) {
    let changed = TRAY.with(|t| std::mem::replace(&mut t.borrow_mut().playing, playing) != playing);
    if changed {
        menu_changed();
    }
}

pub fn set_track(item: Option<&Item>) {
    let changed = TRAY.with(|t| {
        let mut t = t.borrow_mut();
        let changed = t.item.as_ref().map(Item::song_play_id) != item.map(Item::song_play_id);
        t.item = item.cloned();
        changed
    });
    if changed {
        menu_changed();
        emit(ITEM_PATH, ITEM_IFACE, "NewToolTip", None);
    }
}

/// The window was shown or hidden: the "Show / Hide" label follows.
pub fn window_changed() {
    menu_changed();
}

fn menu_changed() {
    let rev = TRAY.with(|t| {
        let mut t = t.borrow_mut();
        t.revision += 1;
        t.revision
    });
    emit(MENU_PATH, MENU_IFACE, "LayoutUpdated", Some(&(rev, 0i32).to_variant()));
}

fn emit(path: &str, iface: &str, signal: &str, args: Option<&glib::Variant>) {
    TRAY.with(|t| {
        let t = t.borrow();
        if let (Some(conn), false) = (&t.conn, t.regs.is_empty()) {
            let _ = conn.emit_signal(None, path, iface, signal, args);
        }
    });
}

// ---- StatusNotifierItem -----------------------------------------------------------------------

fn now_text() -> (String, String) {
    TRAY.with(|t| match &t.borrow().item {
        Some(it) => (it.title().to_owned(), it.artist().to_owned()),
        None => ("Not Playing".to_owned(), String::new()),
    })
}

/// Running from the source tree the icon is not in the theme yet.
fn icon_theme_path() -> String {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/icons");
    if dev.is_dir() { dev.canonicalize().unwrap_or(dev).display().to_string() } else { String::new() }
}

fn property(iface: &str, prop: &str) -> glib::Variant {
    match (iface, prop) {
        (ITEM_IFACE, "Category") => "ApplicationStatus".to_variant(),
        (ITEM_IFACE, "Id") => "applejack".to_variant(),
        (ITEM_IFACE, "Title") => "Applejack".to_variant(),
        (ITEM_IFACE, "Status") => "Active".to_variant(),
        (ITEM_IFACE, "WindowId") => 0i32.to_variant(),
        (ITEM_IFACE, "IconName") => crate::APP_ID.to_variant(),
        (ITEM_IFACE, "IconThemePath") => icon_theme_path().to_variant(),
        (ITEM_IFACE, "OverlayIconName" | "AttentionIconName") => "".to_variant(),
        (ITEM_IFACE, "ToolTip") => {
            let (title, artist) = now_text();
            let text = if artist.is_empty() { title } else { format!("{title} — {artist}") };
            (crate::APP_ID, Vec::<(i32, i32, Vec<u8>)>::new(), "Applejack", text).to_variant()
        }
        (ITEM_IFACE, "ItemIsMenu") => false.to_variant(),
        (ITEM_IFACE, "Menu") => glib::variant::ObjectPath::try_from(MENU_PATH).expect("valid path").to_variant(),
        (MENU_IFACE, "Version") => 3u32.to_variant(),
        (MENU_IFACE, "TextDirection") => "ltr".to_variant(),
        (MENU_IFACE, "Status") => "normal".to_variant(),
        (MENU_IFACE, "IconThemePath") => Vec::<String>::new().to_variant(),
        _ => glib::Variant::from_none(glib::VariantTy::VARIANT),
    }
}

fn call(iface: &str, method: &str, params: &glib::Variant) -> Option<glib::Variant> {
    match (iface, method) {
        (ITEM_IFACE, "Activate") => {
            if let Some(app) = app() {
                desktop::toggle_window(&app);
            }
            None
        }
        (ITEM_IFACE, "SecondaryActivate") => {
            if let Some(app) = app() {
                app.toggle_play();
            }
            None
        }
        (ITEM_IFACE, "Scroll") => {
            if let (Some(app), Some((delta, orientation))) = (app(), params.get::<(i32, String)>()) {
                scroll(&app, delta, &orientation);
            }
            None
        }
        (ITEM_IFACE, _) => None,
        (MENU_IFACE, "GetLayout") => {
            let rev = TRAY.with(|t| t.borrow().revision);
            Some(glib::Variant::tuple_from_iter([rev.to_variant(), layout()]))
        }
        (MENU_IFACE, "GetGroupProperties") => {
            let ids = params.try_child_value(0).and_then(|v| v.get::<Vec<i32>>()).unwrap_or_default();
            let all = items();
            let out: Vec<(i32, HashMap<String, glib::Variant>)> =
                all.into_iter().filter(|(id, _)| ids.is_empty() || ids.contains(id)).collect();
            Some((out,).to_variant())
        }
        (MENU_IFACE, "GetProperty") => {
            let (id, name) = params.get::<(i32, String)>().unwrap_or_default();
            let v = items()
                .into_iter()
                .find(|(i, _)| *i == id)
                .and_then(|(_, mut p)| p.remove(&name))
                .unwrap_or_else(|| "".to_variant());
            Some((v,).to_variant())
        }
        (MENU_IFACE, "Event") => {
            if let Some((id, event)) = params.try_child_value(0).zip(params.try_child_value(1)) {
                if event.str() == Some("clicked") {
                    clicked(id.get::<i32>().unwrap_or(-1));
                }
            }
            None
        }
        (MENU_IFACE, "EventGroup") => {
            if let Some(events) = params.try_child_value(0) {
                for e in events.iter() {
                    if e.try_child_value(1).and_then(|v| v.str().map(str::to_owned)).as_deref() == Some("clicked") {
                        clicked(e.try_child_value(0).and_then(|v| v.get::<i32>()).unwrap_or(-1));
                    }
                }
            }
            Some((Vec::<i32>::new(),).to_variant())
        }
        (MENU_IFACE, "AboutToShow") => Some((false,).to_variant()),
        (MENU_IFACE, "AboutToShowGroup") => Some((Vec::<i32>::new(), Vec::<i32>::new()).to_variant()),
        _ => None,
    }
}

fn scroll(app: &Rc<App>, delta: i32, orientation: &str) {
    if delta == 0 || !orientation.eq_ignore_ascii_case("vertical") {
        return;
    }
    // KDE sends ±120 per wheel notch, others ±1; only the direction counts. Positive is up.
    let step = if delta > 0 { 0.05 } else { -0.05 };
    let v = (app.bar.vol.value() + step).clamp(0.0, 1.0);
    // Same path as dragging the slider: unmutes, tells the engine, remembers the level.
    app.bar.vol.emit_by_name::<bool>("change-value", &[&gtk::ScrollType::Jump, &v]);
}

fn clicked(id: i32) {
    let Some(app) = app() else { return };
    match id {
        NOW | WINDOW => {
            if id == WINDOW && app.window.is_visible() {
                app.window.set_visible(false);
            } else {
                app.window.present();
            }
        }
        TOGGLE => app.toggle_play(),
        NEXT => app.skip(true),
        PREVIOUS => app.skip(false),
        QUIT => {
            if let Some(gapp) = app.window.application() {
                gapp.quit();
            }
        }
        _ => {}
    }
}

// ---- menu ------------------------------------------------------------------------------------------

fn props(label: &str, enabled: bool) -> HashMap<String, glib::Variant> {
    let mut p = HashMap::new();
    p.insert("label".to_owned(), label.to_variant());
    p.insert("enabled".to_owned(), enabled.to_variant());
    p.insert("visible".to_owned(), true.to_variant());
    p
}

fn separator() -> HashMap<String, glib::Variant> {
    let mut p = HashMap::new();
    p.insert("type".to_owned(), "separator".to_variant());
    p.insert("visible".to_owned(), true.to_variant());
    p
}

/// The menu's items in order, with their properties.
fn items() -> Vec<(i32, HashMap<String, glib::Variant>)> {
    let (playing, has_item) = TRAY.with(|t| {
        let t = t.borrow();
        (t.playing, t.item.is_some())
    });
    let (title, artist) = now_text();
    // Underscores would turn into mnemonics.
    let now = if artist.is_empty() { title } else { format!("{title} — {artist}") }.replace('_', "__");
    let visible = app().is_some_and(|a| a.window.is_visible());
    let mut toggle = props(if playing { "Pause" } else { "Play" }, has_item);
    toggle.insert("icon-name".to_owned(), if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }.to_variant());
    let mut next = props("Next", has_item);
    next.insert("icon-name".to_owned(), "media-skip-forward-symbolic".to_variant());
    let mut prev = props("Previous", has_item);
    prev.insert("icon-name".to_owned(), "media-skip-backward-symbolic".to_variant());
    vec![
        (NOW, props(&now, has_item)),
        (2, separator()),
        (TOGGLE, toggle),
        (NEXT, next),
        (PREVIOUS, prev),
        (6, separator()),
        (WINDOW, props(if visible { "Hide Applejack" } else { "Show Applejack" }, true)),
        (QUIT, props("Quit", true)),
    ]
}

fn layout() -> glib::Variant {
    let children: Vec<glib::Variant> = items()
        .into_iter()
        .map(|(id, p)| glib::Variant::tuple_from_iter([id.to_variant(), p.to_variant(), Vec::<glib::Variant>::new().to_variant()]))
        .collect();
    let mut root = HashMap::new();
    root.insert("children-display".to_owned(), "submenu".to_variant());
    glib::Variant::tuple_from_iter([0i32.to_variant(), root.to_variant(), children.to_variant()])
}
