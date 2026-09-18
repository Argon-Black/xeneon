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

use gtk::glib;
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
    // `webkit6::WebView::new()` with no explicit session attaches to
    // WebKit's own process-wide default `NetworkSession`, which is
    // ephemeral (in-memory cookies/local-storage, wiped on exit) - the
    // cause of youtube.com re-asking for cookie consent, and presumably
    // losing any future sign-in, on every app restart. Building our own
    // session pointed at fixed directories under this app's own XDG data/
    // cache dirs (mirrors `xeneon_core::config::config_dir`'s reasoning,
    // just data/cache rather than config - cookies and IndexedDB belong
    // in XDG_DATA_HOME, the HTTP cache in XDG_CACHE_HOME, not mixed into
    // the small JSON config/widget-layout files under XDG_CONFIG_HOME)
    // makes it persistent instead. Cached once and reused by every
    // WebView this module creates, so a second instance (once allowed)
    // shares the same cookie jar rather than each getting its own.
    static NETWORK_SESSION: webkit6::NetworkSession = {
        let data_dir = glib::user_data_dir().join("xeneon-dashboard-rs").join("webkit");
        let cache_dir = glib::user_cache_dir().join("xeneon-dashboard-rs").join("webkit");
        // Created up front rather than left to WebKit to create on demand -
        // `set_persistent_storage` below writes straight into `data_dir`
        // and there's no guarantee it tolerates a directory that doesn't
        // exist yet.
        let _ = std::fs::create_dir_all(&data_dir);
        let session = webkit6::NetworkSession::new(
            Some(data_dir.to_string_lossy().as_ref()),
            Some(cache_dir.to_string_lossy().as_ref()),
        );
        // The data/cache directories above only cover IndexedDB/local
        // storage/HTTP cache - cookies are governed separately by the
        // session's own `CookieManager`, which defaults to in-memory-only
        // regardless, and needs `set_persistent_storage` called explicitly
        // to actually write a cookie jar to disk (confirmed missing: the
        // directory-only setup above still re-asked for cookie consent on
        // every restart).
        if let Some(cookie_manager) = session.cookie_manager() {
            let cookie_jar = data_dir.join("cookies.sqlite");
            cookie_manager.set_persistent_storage(
                &cookie_jar.to_string_lossy(),
                webkit6::CookiePersistentStorage::Sqlite,
            );
        }
        session
    };
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
    let webview = NETWORK_SESSION.with(|session| webkit6::WebView::builder().network_session(session).build());
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

    // Without this, a mouse-wheel scroll over the page (e.g. youtube.com's
    // own vertically-scrolling home feed or video description) bubbles
    // straight past the WebView and reaches the ancestor `Adw.Carousel`,
    // which by default treats any unclaimed scroll as a page-swipe request
    // - same underlying issue as the move-button drag in grid_widget.rs
    // (Adw.Carousel's own recognizer stealing an event a descendant should
    // have handled), same fix: claim it before it can bubble that far.
    // `Bubble` phase (the default) still lets the WebView's own native
    // scroll handling run first, at Target phase, since this controller
    // sits on an *ancestor* (the overlay), not the WebView itself.
    let block_swipe = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    block_swipe.connect_scroll(|_controller, _dx, _dy| glib::Propagation::Stop);
    overlay.add_controller(block_swipe);

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
