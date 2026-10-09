// User preferences, kept in ~/.config/applejack/settings.toml (next to the engine profile).
// Read once at startup; every change is written back right away.
use std::{cell::RefCell, path::PathBuf};

use gtk::glib;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Closing the window keeps the music playing (control it from the tray or the
    /// desktop's media controls).
    pub background: bool,
    /// When started at login: stay in the background (needs `background`).
    pub start_minimized: bool,
    /// A notification when the track changes while the window is not in front.
    pub notifications: bool,
    /// Status icon (StatusNotifierItem) in the panel's tray.
    pub tray: bool,
    /// Keep the computer from suspending while music plays.
    pub inhibit_suspend: bool,

    // ---- appearance ----
    /// system | light | dark
    pub color_scheme: String,
    /// On KDE Plasma: take window, view, header and accent colours from the Plasma colour
    /// scheme (~/.config/kdeglobals).
    pub plasma_colors: bool,

    // ---- scrobbling ----
    pub lastfm_enabled: bool,
    /// The user's own API account (optional; release builds carry one).
    pub lastfm_key: String,
    pub lastfm_secret: String,
    pub lastfm_session: String,
    pub lastfm_user: String,
    pub listenbrainz_enabled: bool,
    pub listenbrainz_token: String,
    pub listenbrainz_user: String,
    /// Another server with the ListenBrainz API (Maloja, Koito, …); empty = listenbrainz.org.
    pub listenbrainz_url: String,

    // ---- Discord ----
    pub discord: bool,
    /// Discord application id (its name is what Discord shows: "Listening to …").
    pub discord_client_id: String,

    // ---- mini player ----
    pub mini_on_top: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background: true,
            start_minimized: true,
            notifications: true,
            tray: true,
            inhibit_suspend: true,
            color_scheme: "system".into(),
            plasma_colors: true,
            lastfm_enabled: true,
            lastfm_key: String::new(),
            lastfm_secret: String::new(),
            lastfm_session: String::new(),
            lastfm_user: String::new(),
            listenbrainz_enabled: true,
            listenbrainz_token: String::new(),
            listenbrainz_user: String::new(),
            listenbrainz_url: String::new(),
            discord: false,
            discord_client_id: String::new(),
            mini_on_top: true,
        }
    }
}

/// ~/.config/applejack
pub fn config_dir() -> PathBuf {
    glib::user_config_dir().join("applejack")
}

fn path() -> PathBuf {
    config_dir().join("settings.toml")
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(load());
}

fn load() -> Settings {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| toml::from_str(&s).map_err(|e| eprintln!("settings.toml: {e}")).ok())
        .unwrap_or_default()
}

pub fn get() -> Settings {
    SETTINGS.with(|s| s.borrow().clone())
}

/// Changes and saves the settings.
pub fn update(f: impl FnOnce(&mut Settings)) {
    SETTINGS.with(|s| {
        let mut s = s.borrow_mut();
        f(&mut s);
        save_toml(&path(), &*s);
    });
}

/// Writes atomically (temp file + rename), so a crash never leaves half a file.
pub fn save_toml<T: Serialize>(path: &std::path::Path, value: &T) {
    let Ok(text) = toml::to_string_pretty(value) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("toml.tmp");
    // Owner-only: settings.toml holds the scrobbling tokens.
    let write = |p: &std::path::Path| {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?.write_all(text.as_bytes())
    };
    if let Err(e) = write(&tmp).and_then(|_| std::fs::rename(&tmp, path)) {
        eprintln!("{}: {e}", path.display());
    }
}

// ---- launch at login ----

fn autostart_file() -> PathBuf {
    glib::user_config_dir().join("autostart").join(format!("{}.desktop", crate::APP_ID))
}

pub fn autostart_enabled() -> bool {
    autostart_file().is_file()
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let file = autostart_file();
    if !on {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    // The launcher on PATH (install.sh symlink) survives reinstalls; fall back to this binary.
    let exe = glib::find_program_in_path("applejack")
        .or_else(|| std::env::current_exe().ok())
        .ok_or("can’t find the applejack binary")?;
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Applejack\nComment=Music streaming player\n\
         Exec=\"{}\" --autostart\nIcon={}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        crate::APP_ID
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, text).map_err(|e| e.to_string())
}

/// Reads one setting without cloning them all.
pub fn with<R>(f: impl FnOnce(&Settings) -> R) -> R {
    SETTINGS.with(|s| f(&s.borrow()))
}
