// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared bundled-SVG-to-texture loader/cache, used by every widget that
//! shows a small bundled icon, optionally tinted per-instance -
//! `hue.rs`'s on/off bulb glyph, `network_sq.rs`'s/`network_m.rs`'s
//! Wi-Fi/Ethernet header icon and fixed-color VPN badge icon. Audit
//! finding 2026-09-29: hand-rolled 3 times, byte-identical between
//! `network_sq.rs`/`network_m.rs` and functionally the same algorithm a
//! third time in `hue.rs` (there just specialized to its own two fixed
//! paths, with a single-texture cache for the untinted one instead of a
//! one-entry `HashMap` - not a meaningful difference, since the cache
//! key here already includes the path, so every caller's own icons
//! share these two caches without colliding).

use log::warn;
use std::cell::RefCell;
use std::collections::HashMap;

/// The fill color these bundled SVGs use for their main glyph - what
/// `load_tinted_icon` looks for and replaces with the caller's chosen
/// color.
pub const ICON_SOURCE_FILL: &str = "#ffffff";
/// Rasterized well above the small sizes these icons actually display
/// at, so they stay crisp rather than looking like upscaled bitmaps -
/// same reasoning as `audio.rs`'s `EMPTY_STATE_ICON_RASTER_PX`, just a
/// much smaller target size since these are small corner icons, not
/// hero art.
pub const ICON_RASTER_PX: i32 = 96;

thread_local! {
    static ICON_TEXTURES: RefCell<HashMap<&'static str, Option<gtk::gdk::Texture>>> = RefCell::new(HashMap::new());
    static TINTED_ICON_TEXTURES: RefCell<HashMap<(&'static str, String), Option<gtk::gdk::Texture>>> = RefCell::new(HashMap::new());
}

/// Loads and caches the bundled SVG at `path` unmodified - for an icon
/// whose color never changes. `None` if it failed to load, in which
/// case the caller just shows no icon rather than a broken image. A
/// failed load is cached too, so a missing/corrupt file doesn't get
/// retried on every widget construction.
pub fn load_icon_texture(path: &'static str) -> Option<gtk::gdk::Texture> {
    ICON_TEXTURES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(texture) = cache.get(path) {
            return texture.clone();
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let texture = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(&full_path, ICON_RASTER_PX, ICON_RASTER_PX)
            .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
            .inspect_err(|err| warn!("failed to load {}: {err}", full_path.display()))
            .ok();
        cache.insert(path, texture.clone());
        texture
    })
}

/// Loads the bundled SVG at `path`, substitutes `ICON_SOURCE_FILL` for
/// `hex_color` in its source text, then rasterizes and caches the
/// result - for an icon whose color is chosen per-instance (a user
/// color picker, or a light's own reported color). `None` if the file
/// couldn't be read or the (already-tinted) SVG couldn't be rasterized.
pub fn load_tinted_icon(path: &'static str, hex_color: &str) -> Option<gtk::gdk::Texture> {
    let key = (path, hex_color.to_string());
    TINTED_ICON_TEXTURES.with(|cache| {
        if let Some(texture) = cache.borrow().get(&key) {
            return texture.clone();
        }
        let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let texture = std::fs::read_to_string(&full_path)
            .inspect_err(|err| warn!("failed to read {}: {err}", full_path.display()))
            .ok()
            .and_then(|svg_text| {
                let tinted = svg_text.replace(ICON_SOURCE_FILL, hex_color);
                let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from_owned(tinted.into_bytes()));
                gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, ICON_RASTER_PX, ICON_RASTER_PX, true, gtk::gio::Cancellable::NONE)
                    .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
                    .inspect_err(|err| warn!("failed to rasterize {} tinted {hex_color}: {err}", full_path.display()))
                    .ok()
            });
        cache.borrow_mut().insert(key, texture.clone());
        texture
    })
}
