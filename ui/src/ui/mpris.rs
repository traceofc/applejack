// MPRIS (org.mpris.MediaPlayer2) on the session bus: GNOME's media controls, media keys
// and the lock screen control Applejack through it, which is what makes playing in the
// background (window closed) usable.
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use adw::prelude::*;
use gtk::{gio, glib};

use super::{App, app};
use crate::model::Item;

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

const XML: &str = r#"<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg direction="in" name="Offset" type="x"/></method>
    <method name="SetPosition">
      <arg direction="in" name="TrackId" type="o"/>
      <arg direction="in" name="Position" type="x"/>
    </method>
    <method name="OpenUri"><arg direction="in" name="Uri" type="s"/></method>
    <signal name="Seeked"><arg name="Position" type="x"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="Rate" type="d" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="read"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>"#;

#[derive(Default)]
struct Published {
    conn: Option<gio::DBusConnection>,
    status: &'static str,
    item: Option<Item>,
    duration: f64,
}

thread_local! {
    static MPRIS: RefCell<Published> = RefCell::new(Published { status: "Stopped", ..Default::default() });
}

/// Exports the player on the application's bus connection.
pub fn init(gapp: &adw::Application) {
    let Some(conn) = gapp.dbus_connection() else { return };
    let info = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(i) => i,
        Err(e) => return eprintln!("mpris: {e}"),
    };
    for iface in [ROOT, PLAYER] {
        let Some(ii) = info.lookup_interface(iface) else { continue };
        let reg = conn
            .register_object(PATH, &ii)
            .method_call(|_, _, _, iface, method, params, inv| {
                call(iface.unwrap_or(""), method, &params);
                inv.return_value(None);
            })
            .property(|_, _, _, iface, prop| property(iface, prop))
            .build();
        if let Err(e) = reg {
            return eprintln!("mpris: {e}");
        }
    }
    gio::bus_own_name_on_connection(&conn, "org.mpris.MediaPlayer2.applejack", gio::BusNameOwnerFlags::NONE, |_, _| {}, |_, name| {
        eprintln!("mpris: lost {name}")
    });
    MPRIS.with(|m| m.borrow_mut().conn = Some(conn));
}

fn call(iface: &str, method: &str, params: &glib::Variant) {
    let Some(app) = app() else { return };
    match (iface, method) {
        (ROOT, "Raise") => app.window.present(),
        (ROOT, "Quit") => {
            if let Some(a) = app.window.application() {
                a.quit();
            }
        }
        (PLAYER, "PlayPause") => app.toggle_play(),
        (PLAYER, "Play") if !app.st.borrow().busy => app.toggle_play(),
        (PLAYER, "Pause" | "Stop") if app.st.borrow().busy => app.toggle_play(),
        (PLAYER, "Next") => app.skip(true),
        (PLAYER, "Previous") => app.skip(false),
        (PLAYER, "Seek") => {
            if let Some((off,)) = params.get::<(i64,)>() {
                let pos = (position(&app) + off as f64 / 1e6).max(0.0);
                app.seek_to(pos);
            }
        }
        (PLAYER, "SetPosition") => {
            if let Some(pos) = params.try_child_value(1).and_then(|v| v.get::<i64>()) {
                app.seek_to(pos as f64 / 1e6);
            }
        }
        _ => {}
    }
}

fn position(app: &Rc<App>) -> f64 {
    let st = app.st.borrow();
    if st.playing { (st.pos + st.pos_at.elapsed().as_secs_f64()).min(st.duration.max(st.pos)) } else { st.pos }
}

fn property(iface: &str, prop: &str) -> glib::Variant {
    let as_ = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>().to_variant();
    match (iface, prop) {
        (_, "CanQuit" | "CanRaise") => true.to_variant(),
        (_, "HasTrackList") => false.to_variant(),
        (_, "Identity") => "Applejack".to_variant(),
        (_, "DesktopEntry") => crate::APP_ID.to_variant(),
        (_, "SupportedUriSchemes" | "SupportedMimeTypes") => as_(&[]),
        (_, "PlaybackStatus") => MPRIS.with(|m| m.borrow().status).to_variant(),
        (_, "Rate" | "MinimumRate" | "MaximumRate") => 1.0f64.to_variant(),
        (_, "Metadata") => metadata(),
        (_, "Volume") => app().map(|a| a.bar.vol.value()).unwrap_or(1.0).to_variant(),
        (_, "Position") => app().map(|a| (position(&a) * 1e6) as i64).unwrap_or(0).to_variant(),
        (_, "CanGoNext" | "CanGoPrevious" | "CanPlay" | "CanPause" | "CanSeek" | "CanControl") => true.to_variant(),
        _ => glib::Variant::from_none(glib::VariantTy::VARIANT),
    }
}

fn track_path(item: Option<&Item>) -> glib::variant::ObjectPath {
    let id: String = item
        .map(|i| i.song_play_id())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = if id.is_empty() { "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned() } else { format!("/io/github/traceofc/Applejack/track/t{id}") };
    glib::variant::ObjectPath::try_from(path).expect("valid object path")
}

fn metadata() -> glib::Variant {
    MPRIS.with(|m| {
        let m = m.borrow();
        let mut d: HashMap<String, glib::Variant> = HashMap::new();
        d.insert("mpris:trackid".into(), track_path(m.item.as_ref()).to_variant());
        if let Some(it) = &m.item {
            d.insert("xesam:title".into(), it.title().to_variant());
            if !it.artist().is_empty() {
                d.insert("xesam:artist".into(), vec![it.artist().to_owned()].to_variant());
            }
            if let Some(a) = &it.album {
                d.insert("xesam:album".into(), a.to_variant());
            }
            if let Some(url) = it.art(512) {
                d.insert("mpris:artUrl".into(), url.to_variant());
            }
            let len = if m.duration > 0.0 { m.duration } else { it.duration.unwrap_or(0.0) };
            if len > 0.0 {
                d.insert("mpris:length".into(), ((len * 1e6) as i64).to_variant());
            }
        }
        d.to_variant()
    })
}

fn changed(props: &[(&str, glib::Variant)]) {
    MPRIS.with(|m| {
        let m = m.borrow();
        let Some(conn) = &m.conn else { return };
        let dict: HashMap<String, glib::Variant> = props.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let _ = conn.emit_signal(
            None,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&(PLAYER, dict, Vec::<String>::new()).to_variant()),
        );
    });
}

/// "playing" | "paused" | ... (MusicKit state names) -> PlaybackStatus.
pub fn set_state(busy: bool, has_item: bool) {
    let status = if busy {
        "Playing"
    } else if has_item {
        "Paused"
    } else {
        "Stopped"
    };
    let same = MPRIS.with(|m| std::mem::replace(&mut m.borrow_mut().status, status) == status);
    if !same {
        changed(&[("PlaybackStatus", status.to_variant())]);
    }
}

pub fn set_track(item: Option<&Item>, duration: f64) {
    let same = MPRIS.with(|m| {
        let mut m = m.borrow_mut();
        let same = m.item.as_ref() == item && (m.duration - duration).abs() < 0.5;
        m.item = item.cloned();
        m.duration = duration;
        same
    });
    if !same {
        changed(&[("Metadata", metadata())]);
    }
}

pub fn seeked(seconds: f64) {
    MPRIS.with(|m| {
        if let Some(conn) = &m.borrow().conn {
            let _ = conn.emit_signal(None, PATH, PLAYER, "Seeked", Some(&((seconds * 1e6) as i64,).to_variant()));
        }
    });
}
