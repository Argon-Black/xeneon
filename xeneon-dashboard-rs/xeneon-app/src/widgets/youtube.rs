//! YouTube widget: a `SIZE_L` card embedding a real WebKitGTK view pointed
//! at youtube.com - a genuine mini-browser, not a fixed embed URL.
//! Deliberately not "one channel/video per widget" like a typical
//! dashboard tile: the user picked free navigation instead (see the design
//! discussion in the memory system) because YouTube's own player already
//! does the right thing for a dashboard - controls auto-hide during
//! playback and reappear on hover, so no custom overlay chrome is needed
//! here.
//!
//! Step 1 proved the mechanism end-to-end (WebKitGTK rendering correctly
//! inside this Relm4/GTK4 stack, on the real Xeneon hardware) and fixed
//! two bugs found in hands-on testing: the ancestor `Adw.Carousel`
//! stealing mouse-wheel scroll as a page-swipe, and cookies not
//! surviving an app restart (needs an explicit persistent `NetworkSession`
//! - see that `thread_local`'s own doc comment).
//!
//! This step (2) adds the "resume last page" persistence discussed with
//! the user: which URL was open, and whether to restore it at all, saved
//! like any other widget's content and restored on the next launch.
//! Deliberately *not* included: resuming to the exact playback position
//! within a video - a separate, harder feature (would need polling the
//! page's actual playback time via injected JavaScript) that the user
//! asked to keep as a later, distinct step. Also still deferred: the
//! "only one instance allowed" guard.

use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use webkit6::prelude::*;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;

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

struct YoutubeState {
    webview: webkit6::WebView,
    resume_last_page: Cell<bool>,
    // Wired by `WidgetGrid` right after this instance is placed (see
    // `WidgetInstance::on_change_ready`'s own doc comment) - called
    // whenever the WebView's own URL changes, since navigating inside the
    // page (clicking a video, searching...) happens entirely outside the
    // two save points every other widget already gets for free (the
    // appearance popover closing, a whole-widget drag ending). Mirrors
    // `ShortcutsState.change_notifier` in shortcuts.rs.
    change_notifier: RefCell<Option<Rc<dyn Fn()>>>,
}

impl YoutubeState {
    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "resume_last_page": self.resume_last_page.get(),
            "url": self.webview.uri().map(|uri| uri.to_string()).unwrap_or_default(),
        })
    }
}

/// Reads `restore`'s saved dict into `(start_url, resume_last_page)` -
/// called *before* `build_content` so the WebView can be pointed at the
/// right page from its very first `load_uri`, rather than built on
/// `HOME_URL` and redirected right after (which would flash the home page
/// first). Falls back to `HOME_URL`/`true` for a missing, partial, or
/// pre-persistence (step 1) saved dict, same "only touch keys that are
/// actually there" spirit as `ClockContent::apply_dict`.
fn start_state_from_dict(data: &serde_json::Value) -> (String, bool) {
    let resume_last_page = data.get("resume_last_page").and_then(|v| v.as_bool()).unwrap_or(true);
    let saved_url = data.get("url").and_then(|v| v.as_str()).filter(|url| !url.is_empty());
    let start_url = if resume_last_page { saved_url.unwrap_or(HOME_URL).to_string() } else { HOME_URL.to_string() };
    (start_url, resume_last_page)
}

fn build_content(start_url: &str, resume_last_page: bool) -> (Rc<YoutubeState>, gtk::Widget) {
    let webview = NETWORK_SESSION.with(|session| webkit6::WebView::builder().network_session(session).build());
    webview.set_hexpand(true);
    webview.set_vexpand(true);
    webview.load_uri(start_url);

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

    let state = Rc::new(YoutubeState {
        webview: webview.clone(),
        resume_last_page: Cell::new(resume_last_page),
        change_notifier: RefCell::new(None),
    });

    // YouTube is a single-page app - navigating (search, clicking a
    // video, going back) changes the URL via the History API rather than
    // a full page load, but `notify::uri` fires for that too, so this
    // still catches every navigation, not just the very first one.
    webview.connect_uri_notify({
        let state = state.clone();
        move |_webview| {
            if let Some(save_now) = state.change_notifier.borrow().as_ref() {
                save_now();
            }
        }
    });

    (state, overlay.upcast())
}

fn build_settings(state: Rc<YoutubeState>) -> (gtk::Widget, Box<dyn Fn()>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_size_request(240, -1);

    let label = gtk::Label::new(Some(&i18n::t("widgets.youtube.settings.resume_last_page")));
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);

    let switch = gtk::Switch::new();
    switch.set_active(state.resume_last_page.get());
    switch.set_valign(gtk::Align::Center);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&label);
    row.append(&switch);
    root.append(&row);

    switch.connect_active_notify({
        let state = state.clone();
        move |s| state.resume_last_page.set(s.is_active())
    });

    i18n::on_change({
        let label = label.clone();
        move || label.set_label(&i18n::t("widgets.youtube.settings.resume_last_page"))
    });

    // Re-reads the switch from `state` - needed after `state.reset()`-style
    // changes made outside this control (the appearance popover's reset
    // button, see `on_reset` below), same "sync_from_content" spirit as
    // every other plugin's own `resync`.
    let resync: Box<dyn Fn()> = Box::new(move || {
        switch.set_active(state.resume_last_page.get());
    });

    (root.upcast(), resync)
}

fn wire_change_notifier(state: &Rc<YoutubeState>) -> Box<dyn FnOnce(Rc<dyn Fn()>)> {
    let state = state.clone();
    Box::new(move |save_now| {
        *state.change_notifier.borrow_mut() = Some(save_now);
    })
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content(HOME_URL, true);
    let (settings, resync) = build_settings(state.clone());
    let on_reset = {
        let state = state.clone();
        move || {
            state.resume_last_page.set(true);
            resync();
        }
    };
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new({
            let state = state.clone();
            move || state.to_dict()
        }),
        on_reset: Some(Box::new(on_reset)),
        on_change_ready: Some(wire_change_notifier(&state)),
    }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (start_url, resume_last_page) = start_state_from_dict(data);
    let (state, content) = build_content(&start_url, resume_last_page);
    let (settings, resync) = build_settings(state.clone());
    let on_reset = {
        let state = state.clone();
        move || {
            state.resume_last_page.set(true);
            resync();
        }
    };
    WidgetInstance {
        content,
        settings: Some(settings),
        to_dict: Box::new({
            let state = state.clone();
            move || state.to_dict()
        }),
        on_reset: Some(Box::new(on_reset)),
        on_change_ready: Some(wire_change_notifier(&state)),
    }
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
