//! YouTube widget, step 1: a `SIZE_L` card embedding a real WebKitGTK
//! view pointed at youtube.com - a genuine mini-browser, not a fixed
//! embed URL. Deliberately not "one channel/video per widget" like a
//! typical dashboard tile: the user picked free navigation instead (see
//! the design discussion in the memory system) because YouTube's own
//! player already does the right thing for a dashboard - controls
//! auto-hide during playback and reappear on hover, so no custom overlay
//! chrome is needed here.
//!
//! This step only proves the mechanism end-to-end (does WebKitGTK render
//! correctly inside this Relm4/GTK4 stack, on the real Xeneon hardware):
//! load youtube.com, show a placeholder while it loads. Deliberately
//! deferred to later steps: the "only one instance allowed" guard (for
//! stability/lightness - a `webkit6::WebView` is its own render process,
//! unlike every other widget here which is cheap D-Bus/local-file work),
//! the "resume last page" setting, and URL persistence.

use gtk::prelude::*;
use std::cell::RefCell;
use webkit6::prelude::*;

use crate::widgets::registry::{self, WidgetInstance};

const HOME_URL: &str = "https://www.youtube.com/";

// Same reasoning and mechanism as `audio.rs`'s `EMPTY_STATE_ICON_PATH`/
// `EMPTY_STATE_TEXTURE` (resolved relative to this crate's own source
// directory so it's reliable under `cargo run`/`cargo build` regardless
// of the process's working directory; rasterized once at a fixed size
// above SIZE_L's own footprint so it stays crisp under `ContentFit::Cover`
// rather than being upscaled from whatever size a plain SVG load would
// pick; cached in a `thread_local` since the file never changes).
const LOADING_ICON_PATH: &str = "assets/youtube-empty.svg";
const LOADING_ICON_RASTER_PX: i32 = 768;

thread_local! {
    static LOADING_TEXTURE: RefCell<Option<Option<gtk::gdk::Texture>>> = RefCell::new(None);
}

fn loading_texture() -> Option<gtk::gdk::Texture> {
    LOADING_TEXTURE.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(LOADING_ICON_PATH);
            let texture = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(&path, LOADING_ICON_RASTER_PX, LOADING_ICON_RASTER_PX)
                .map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
                .inspect_err(|err| eprintln!("xeneon-dashboard: failed to load {}: {err}", path.display()))
                .ok();
            *cell = Some(texture);
        }
        cell.clone().unwrap()
    })
}

fn build_content() -> gtk::Widget {
    let webview = webkit6::WebView::new();
    webview.set_hexpand(true);
    webview.set_vexpand(true);
    webview.load_uri(HOME_URL);

    // The placeholder sits on top of the WebView (not swapped in a
    // `gtk::Stack`, which would need the WebView to already exist either
    // way) and is simply hidden once the page has actually rendered
    // something - `LoadEvent::Finished` fires once per navigation, so this
    // also covers clicking through to a new video from the home page.
    let loading = gtk::Picture::new();
    loading.set_content_fit(gtk::ContentFit::Cover);
    loading.set_can_target(false);
    loading.set_paintable(loading_texture().as_ref());

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&webview));
    overlay.add_overlay(&loading);

    webview.connect_load_changed(move |_webview, event| {
        if event == webkit6::LoadEvent::Finished {
            loading.set_visible(false);
        }
    });

    overlay.upcast()
}

pub fn spawn() -> WidgetInstance {
    registry::instance_without_settings(build_content())
}

pub fn restore(_data: &serde_json::Value) -> WidgetInstance {
    // No persisted state yet (step 1) - always starts on youtube.com.
    registry::instance_without_settings(build_content())
}

/// Widget picker tile for this kind - `WidgetDescriptor::preview`, not
/// `spawn`. A live `spawn()` here would start a second WebKit web process
/// loading youtube.com purely to show a throwaway preview, on top of
/// whatever instance is already placed on a page - exactly the kind of
/// duplicate-WebView load the user found made the app unresponsive during
/// step 1 testing. Just the same placeholder icon shown while a real
/// instance loads, static and free.
pub fn preview() -> gtk::Widget {
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_paintable(loading_texture().as_ref());
    picture.upcast()
}
