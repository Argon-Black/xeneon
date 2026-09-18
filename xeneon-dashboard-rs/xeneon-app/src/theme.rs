// SPDX-License-Identifier: GPL-3.0-or-later
//! App-wide accent color - independent of each widget's own appearance
//! (see appearance_css.rs) or the page indicator's own style. Ported from
//! `theme.py`: a single GTK named color (`@define-color accent_color
//! ...`) on one shared provider, rather than the per-instance `_rules`-map
//! pattern those modules use - the accent is one global value, and GTK
//! already resolves a named color display-wide across every provider
//! attached to the same display, so anything referencing `@accent_color`
//! keeps resolving correctly across every future `apply_accent()` call
//! without needing to reload itself.

use std::cell::RefCell;
use std::sync::Once;

pub const ACCENT_PRESETS: [(&str, &str); 4] =
    [("iris", "#7e57c2"), ("glacier", "#3584e4"), ("sarcelle", "#26a269"), ("ambre", "#ff9f43")];
pub const DEFAULT_ACCENT_HEX: &str = ACCENT_PRESETS[0].1;

pub const SWATCH_CSS_CLASS: &str = "xeneon-accent-swatch";
pub const SWATCH_SELECTED_CSS_CLASS: &str = "xeneon-accent-swatch-selected";

static INSTALL: Once = Once::new();
thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
    static CURRENT_HEX: RefCell<String> = RefCell::new(DEFAULT_ACCENT_HEX.to_string());
}

fn swatch_static_css() -> String {
    let mut css = format!(
        ".{SWATCH_CSS_CLASS} {{ min-width: 22px; min-height: 22px; padding: 0; border-radius: 999px; border: 2px solid transparent; }}\n"
    );
    for (key, hex) in ACCENT_PRESETS {
        css.push_str(&format!(".{SWATCH_CSS_CLASS}-{key} {{ background-color: {hex}; }}\n"));
    }
    css.push_str(&format!(".{SWATCH_SELECTED_CSS_CLASS} {{ border-color: #ffffff; }}"));
    css
}

fn ensure_installed() {
    INSTALL.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
    });
}

/// Sets the live accent color and reloads the one shared provider - every
/// stylesheet referencing `@accent_color` picks it up immediately without
/// reloading itself.
pub fn apply_accent(hex: &str) {
    ensure_installed();
    CURRENT_HEX.with(|c| *c.borrow_mut() = hex.to_string());
    PROVIDER.with(|p| {
        if let Some(provider) = p.borrow().as_ref() {
            provider.load_from_string(&format!("{}\n@define-color accent_color {hex};", swatch_static_css()));
        }
    });
    LISTENERS.with(|listeners| {
        for callback in listeners.borrow().iter() {
            callback();
        }
    });
}

pub fn current_accent() -> String {
    CURRENT_HEX.with(|c| c.borrow().clone())
}

pub fn on_change(callback: impl Fn() + 'static) {
    LISTENERS.with(|listeners| listeners.borrow_mut().push(Box::new(callback)));
}

/// The desktop's own accent color (GNOME Réglages > Couleurs), or `None`
/// if this desktop doesn't report one at all - some environments/older
/// GNOME versions don't, so "follow system" has nothing to follow there
/// and the caller should just leave the current accent as is.
pub fn system_accent_hex() -> Option<String> {
    let style_manager = adw::StyleManager::default();
    if !style_manager.is_system_supports_accent_colors() {
        return None;
    }
    let rgba = style_manager.accent_color_rgba();
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.red() * 255.0).round() as u8,
        (rgba.green() * 255.0).round() as u8,
        (rgba.blue() * 255.0).round() as u8
    ))
}

pub fn system_accent_supported() -> bool {
    adw::StyleManager::default().is_system_supports_accent_colors()
}

static SYSTEM_ACCENT_SIGNAL_CONNECTED: Once = Once::new();

/// Lets the app re-apply the system accent whenever GNOME's own pick
/// changes while "follow system" is enabled. Connected once regardless of
/// how many times this is called.
pub fn connect_system_accent_changed(callback: impl Fn() + 'static) {
    SYSTEM_ACCENT_SIGNAL_CONNECTED.call_once(|| {
        adw::StyleManager::default().connect_accent_color_notify(move |_| callback());
    });
}
