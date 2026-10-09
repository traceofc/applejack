// Appearance: light / dark / follow the system, and on KDE Plasma the Plasma colour scheme.
//
// libadwaita already follows the system's dark / light preference and accent colour through
// the settings portal (xdg-desktop-portal-kde provides both). With "Plasma colours" on,
// window, view, header bar, sidebar and accent colours are read from ~/.config/kdeglobals
// (what System Settings → Colours writes) and laid over libadwaita's palette, and the
// file is watched, so switching schemes in Plasma recolours Applejack right away.
use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::settings;

thread_local! {
    static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    static MONITOR: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
}

pub fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase().contains("KDE")
        || std::env::var_os("KDE_FULL_SESSION").is_some()
}

fn kdeglobals() -> PathBuf {
    glib::user_config_dir().join("kdeglobals")
}

pub fn init() {
    if let Some(display) = gdk::Display::default() {
        PROVIDER.with(|p| {
            // Above libadwaita's theme, below Applejack's own stylesheet.
            gtk::style_context_add_provider_for_display(&display, p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1)
        });
    }
    if is_kde() {
        let file = gio::File::for_path(kdeglobals());
        if let Ok(m) = file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            m.set_rate_limit(500);
            m.connect_changed(|_, _, _, ev| {
                if matches!(ev, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created) {
                    apply();
                }
            });
            MONITOR.with(|c| *c.borrow_mut() = Some(m));
        }
    }
    apply();
}

/// Applies the current appearance settings.
pub fn apply() {
    let (scheme, plasma) = settings::with(|s| (s.color_scheme.clone(), s.plasma_colors));
    let palette = (plasma && is_kde()).then(read_kdeglobals).flatten();
    let style = adw::StyleManager::default();
    style.set_color_scheme(match scheme.as_str() {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        // A Plasma palette is either light or dark; match libadwaita's own colours to it.
        _ => match &palette {
            Some(p) if p.dark => adw::ColorScheme::ForceDark,
            Some(_) => adw::ColorScheme::ForceLight,
            None => adw::ColorScheme::Default,
        },
    });
    // A forced light / dark style next to a Plasma scheme of the other kind would mix
    // light text with light backgrounds: the palette is used only when they agree.
    let css = match &palette {
        Some(p) if scheme == "system" || (scheme == "dark") == p.dark => p.css.clone(),
        _ => String::new(),
    };
    PROVIDER.with(|p| p.load_from_string(&css));
}

struct Palette {
    dark: bool,
    css: String,
}

/// "r,g,b" -> (r, g, b)
fn rgb(v: &str) -> Option<(u8, u8, u8)> {
    let mut it = v.split(',').map(|x| x.trim().parse::<u8>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    (0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64) / 255.0
}

fn read_kdeglobals() -> Option<Palette> {
    let text = std::fs::read_to_string(kdeglobals()).ok()?;
    let mut groups: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            cur = g.to_owned();
        } else if let Some((k, v)) = line.split_once('=') {
            groups.entry(cur.clone()).or_default().insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    let get = |group: &str, key: &str| groups.get(group).and_then(|g| g.get(key)).and_then(|v| rgb(v));
    let window_bg = get("Colors:Window", "BackgroundNormal")?;
    let window_fg = get("Colors:Window", "ForegroundNormal")?;
    let view_bg = get("Colors:View", "BackgroundNormal").unwrap_or(window_bg);
    let view_fg = get("Colors:View", "ForegroundNormal").unwrap_or(window_fg);
    let header_bg = get("Colors:Header", "BackgroundNormal").unwrap_or(window_bg);
    let header_fg = get("Colors:Header", "ForegroundNormal").unwrap_or(window_fg);
    let accent_bg = get("Colors:Selection", "BackgroundNormal");
    let accent_fg = get("Colors:Selection", "ForegroundNormal");
    let accent = get("Colors:View", "DecorationFocus").or(accent_bg);
    let button_bg = get("Colors:Button", "BackgroundNormal");
    let negative = get("Colors:View", "ForegroundNegative");
    let positive = get("Colors:View", "ForegroundPositive");
    let neutral = get("Colors:View", "ForegroundNeutral");
    let dark = luminance(window_bg) < 0.5;

    // libadwaita ≥ 1.6 reads CSS variables; older versions the @define-color names.
    let mut vars: Vec<(&str, String)> = vec![
        ("window_bg_color", hex(window_bg)),
        ("window_fg_color", hex(window_fg)),
        ("view_bg_color", hex(view_bg)),
        ("view_fg_color", hex(view_fg)),
        ("headerbar_bg_color", hex(header_bg)),
        ("headerbar_fg_color", hex(header_fg)),
        ("headerbar_backdrop_color", hex(header_bg)),
        ("sidebar_bg_color", hex(window_bg)),
        ("sidebar_fg_color", hex(window_fg)),
        ("sidebar_backdrop_color", hex(window_bg)),
        ("secondary_sidebar_bg_color", hex(window_bg)),
        ("secondary_sidebar_fg_color", hex(window_fg)),
        ("dialog_bg_color", hex(window_bg)),
        ("dialog_fg_color", hex(window_fg)),
        ("popover_bg_color", hex(view_bg)),
        ("popover_fg_color", hex(view_fg)),
        ("thumbnail_bg_color", hex(view_bg)),
        ("thumbnail_fg_color", hex(view_fg)),
    ];
    if let Some(b) = button_bg {
        vars.push(("card_bg_color", hex(b)));
    }
    if let Some(a) = accent_bg {
        vars.push(("accent_bg_color", hex(a)));
    }
    if let Some(a) = accent_fg {
        vars.push(("accent_fg_color", hex(a)));
    }
    if let Some(a) = accent {
        vars.push(("accent_color", hex(a)));
    }
    for (name, c) in [("destructive_color", negative), ("error_color", negative), ("success_color", positive), ("warning_color", neutral)] {
        if let Some(c) = c {
            vars.push((name, hex(c)));
        }
    }
    let mut css = String::new();
    for (k, v) in &vars {
        css.push_str(&format!("@define-color {k} {v};\n"));
    }
    // CSS variables need GTK 4.16; older versions would warn about the unknown syntax.
    if gtk::check_version(4, 16, 0).is_none() {
        css.push_str(":root {\n");
        for (k, v) in &vars {
            css.push_str(&format!("  --{}: {v};\n", k.replace('_', "-")));
        }
        css.push_str("}\n");
    }
    Some(Palette { dark, css })
}
