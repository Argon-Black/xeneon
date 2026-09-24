// SPDX-License-Identifier: GPL-3.0-or-later
//! The dedicated Home Assistant page: a full-screen `webkit6::WebView`
//! pointed at the URL configured in settings (`Config.ha_page_url`), built
//! once at startup and inserted as the carousel's very first page (see
//! main.rs's append order) - set apart from the numbered widget pages the
//! same way the settings page already is (its own icon in the page
//! indicator, skipped by the numbered-dot loop, see page_indicator.rs).
//!
//! Reuses three mechanisms `widgets/youtube.rs` already proved on real
//! hardware for embedding WebKitGTK in this Relm4/GTK4 stack: a
//! persistent `NetworkSession` (so a Home Assistant login survives an app
//! restart), the scroll-wheel guard (so scrolling a tall Lovelace
//! dashboard doesn't swipe the carousel out from under it), and reloading
//! on `web-process-terminated` (a known Mesa driver bug in WebKit's own
//! process cleanup, unrelated to anything here - see that module's own
//! comment). Deliberately simpler than youtube.rs everywhere else: this
//! page always loads exactly one fixed URL (no free navigation, no
//! per-instance settings popover, no saved "last page" - there's only
//! ever one instance, built once, for as long as the app runs), and
//! turning the page on/off entirely is a full app relaunch (mirrors
//! `settings_page.rs`'s dev-mode toggle) rather than live carousel
//! surgery - see the design discussion in the memory system for why that
//! trade was made.
//!
//! Leaving this page by mouse is the page indicator's job, not this
//! module's: a mouse click-drag over the WebView can't reach
//! `Adw.Carousel`'s own swipe tracker at all - confirmed in hands-on
//! testing (`drag-begin` fires on a `GestureDrag` added here at Capture
//! phase, so the press itself is seen, but `drag-update` never once
//! fires afterward, meaning WebKit's own pointer handling takes over the
//! rest of the sequence at a level no ancestor `GtkGesture`/
//! `EventController` can contest, regardless of propagation phase or
//! claiming - likely tied to WebKit's accelerated-compositing subsurface,
//! see main.rs's DMA-BUF renderer history for the related, already
//! reverted attempt at changing that rendering path). Touch-drag is
//! unaffected and still swipes normally. See
//! `page_indicator.rs::is_on_reveal_locked_page` for the actual fix: the
//! indicator bar stays permanently visible (and clickable) while on this
//! page, the same way it already does on the settings page.

use gtk::glib;
use gtk::prelude::*;
use log::{debug, warn};
use std::cell::RefCell;
use webkit6::prelude::*;

use crate::i18n_runtime as i18n;

thread_local! {
    // Own directory, deliberately separate from `widgets/youtube.rs`'s -
    // two `webkit6::NetworkSession`s writing to the same cookies.sqlite at
    // once (this page and a YouTube widget can both be live simultaneously,
    // see the design discussion in the memory system) risks corrupting it.
    static NETWORK_SESSION: webkit6::NetworkSession = {
        let data_dir = glib::user_data_dir().join("xeneon-dashboard-rs").join("webkit-ha");
        let cache_dir = glib::user_cache_dir().join("xeneon-dashboard-rs").join("webkit-ha");
        if let Err(err) = std::fs::create_dir_all(&data_dir) {
            warn!("failed to create webkit data dir {}: {err} (Home Assistant session will not persist)", data_dir.display());
        } else {
            // Owner-only, same reasoning (and same audit finding) as
            // youtube.rs's own NETWORK_SESSION - a Home Assistant login
            // cookie is at least as sensitive as a YouTube one.
            use std::os::unix::fs::PermissionsExt;
            if let Err(err) = std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700)) {
                warn!("failed to restrict permissions on webkit data dir {}: {err}", data_dir.display());
            }
        }
        let session = webkit6::NetworkSession::new(
            Some(data_dir.to_string_lossy().as_ref()),
            Some(cache_dir.to_string_lossy().as_ref()),
        );
        if let Some(cookie_manager) = session.cookie_manager() {
            let cookie_jar = data_dir.join("cookies.sqlite");
            cookie_manager.set_persistent_storage(
                &cookie_jar.to_string_lossy(),
                webkit6::CookiePersistentStorage::Sqlite,
            );
        }
        session
    };

    // The one live instance's WebView, if `build()` has run - lets
    // `set_url()` (called from settings_page.rs when the URL field is
    // applied) navigate it live, without main.rs having to thread a
    // handle through `settings_page::populate()`'s signature just for
    // this. `None` until `build()` runs, and forever `None` if the page
    // is disabled (nothing to navigate).
    static CURRENT_WEBVIEW: RefCell<Option<webkit6::WebView>> = const { RefCell::new(None) };
}

/// Builds the real page: a `WebView` loading `url` immediately, a spinner
/// overlay hidden once the first load finishes. `url` is trusted to
/// already be http(s)-only - see settings_page.rs's `is_http_url`, the
/// only place a value ever reaches `Config.ha_page_url` - but is checked
/// again here regardless: config.json is a plain user-editable file, and
/// this is the one place that check actually protects (the previous one
/// only ever gated the settings UI, not this call). A non-conforming URL
/// falls back to the same "not configured" placeholder `build_unconfigured`
/// shows, rather than ever reaching `WebView::load_uri`.
pub fn build(url: &str) -> gtk::Widget {
    if !url.to_ascii_lowercase().starts_with("http://") && !url.to_ascii_lowercase().starts_with("https://") {
        warn!("ha_page_url {url:?} in config.json is not http(s), showing the unconfigured placeholder instead");
        return build_unconfigured();
    }

    debug!("building Home Assistant page: url={url}");
    let webview = NETWORK_SESSION.with(|session| webkit6::WebView::builder().network_session(session).build());
    webview.set_hexpand(true);
    webview.set_vexpand(true);
    webview.load_uri(url);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&webview));

    let spinner = gtk::Spinner::new();
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_width_request(48);
    spinner.set_height_request(48);
    spinner.set_spinning(true);
    overlay.add_overlay(&spinner);

    // Same fix, same reason as youtube.rs's own `block_swipe`: without
    // this, scrolling a tall Lovelace dashboard bubbles past the WebView
    // to the ancestor `Adw.Carousel`, which treats any unclaimed scroll as
    // a page-swipe request.
    let block_swipe = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    block_swipe.connect_scroll(|_controller, _dx, _dy| glib::Propagation::Stop);
    overlay.add_controller(block_swipe);

    webview.connect_load_changed({
        let spinner = spinner.clone();
        move |webview, event| {
            if event == webkit6::LoadEvent::Finished {
                debug!("load finished: {}", webview.uri().map(|u| u.to_string()).unwrap_or_default());
                spinner.set_visible(false);
                spinner.set_spinning(false);
            }
        }
    });

    // Same as youtube.rs: doesn't change behaviour (WebKit's own default
    // error page still shows), just leaves a trace of *why* in the logs.
    webview.connect_load_failed(|_webview, event, failing_uri, error| {
        warn!("load failed ({event:?}) for {failing_uri}: {error}");
        false
    });

    // Same Mesa-driver WebProcess crash recovery as youtube.rs - see that
    // module's own comment for the full explanation.
    webview.connect_web_process_terminated({
        let spinner = spinner.clone();
        let webview = webview.clone();
        move |_webview, reason| {
            warn!("web process terminated ({reason:?}), reloading");
            spinner.set_visible(true);
            spinner.set_spinning(true);
            webview.reload();
        }
    });

    CURRENT_WEBVIEW.with(|cell| *cell.borrow_mut() = Some(webview));

    overlay.upcast()
}

/// Shown instead of `build()` when the page is enabled but no URL has been
/// set yet (or the saved one doesn't pass the http(s) check above) - a
/// reachable state, since the enable switch and the URL field are two
/// independent settings (see settings_page.rs). Points the user at
/// settings rather than showing a blank WebView or silently loading
/// nothing.
pub fn build_unconfigured() -> gtk::Widget {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.set_halign(gtk::Align::Center);
    box_.set_valign(gtk::Align::Center);
    box_.set_hexpand(true);
    box_.set_vexpand(true);

    let icon = gtk::Image::from_icon_name("preferences-system-symbolic");
    icon.set_pixel_size(64);
    icon.set_opacity(0.6);
    box_.append(&icon);

    let label = gtk::Label::new(Some(&i18n::t("ha_page.not_configured")));
    label.add_css_class("title-3");
    label.set_opacity(0.6);
    box_.append(&label);

    i18n::on_change({
        let label = label.clone();
        move || label.set_label(&i18n::t("ha_page.not_configured"))
    });

    box_.upcast()
}

/// Navigates the live page to `url`, if `build()` has actually run in this
/// process (a no-op otherwise: the page is disabled, or still showing the
/// unconfigured placeholder - see its own doc comment on why that's a
/// separate, reachable state). Called from settings_page.rs's URL field
/// `connect_apply`, so typing a new address takes effect immediately
/// without needing the full relaunch the enable switch itself requires.
pub fn set_url(url: &str) {
    CURRENT_WEBVIEW.with(|cell| {
        if let Some(webview) = cell.borrow().as_ref() {
            debug!("navigating Home Assistant page to {url}");
            webview.load_uri(url);
        }
    });
}
