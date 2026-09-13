//! Clickable, numbered page buttons floating over the carousel's bottom
//! edge - shown only while actually navigating between pages (a swipe, or
//! tapping a dot), auto-hidden after a delay, except on the settings page
//! where it stays up. Ported from `page_indicator.py`, including the
//! opacity/color customization driven by `Config.indicator_opacity`/
//! `indicator_button_color` (see `set_style`/`set_hide_delay_seconds`,
//! wired to the "Interface" settings group).

use adw::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::grid_widget::WidgetGrid;

const DEFAULT_HIDE_DELAY_SECONDS: u32 = 2;
// Sized off the Xeneon Edge's actual pixel density (2560x720 over a 14.5"
// 32:9 panel, ~183 ppi -> ~72px/cm) so a fingertip-sized tap lands
// reliably - see page_indicator.py's own derivation and CLAUDE.md.
const TOUCH_TARGET_PX: i32 = 72;
const SETTINGS_ICON_PIXEL_SIZE: i32 = 36;
pub const DEFAULT_OPACITY_PERCENT: u32 = 55;

static INSTALL: Once = Once::new();
thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

fn ensure_installed() {
    INSTALL.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
    });
}

/// Rebuilds and reloads the indicator's CSS - called once at startup with
/// the defaults, and again whenever the "Interface" settings group's
/// opacity/color rows change. `color_hex` unset means "use the theme's own
/// foreground color" (currentColor) - same untouched-by-default convention
/// as `WidgetAppearance`. Mirrors `apply_style()` in page_indicator.py.
pub fn set_style(opacity_percent: u32, color_hex: Option<&str>) {
    ensure_installed();
    let inactive_opacity = (opacity_percent.clamp(10, 100) as f64) / 100.0;
    let color_rule = color_hex.map(|hex| format!("color: {hex};")).unwrap_or_default();
    let css = format!(
        "
        button.xeneon-page-number {{
          min-width: {tpx}px;
          min-height: {tpx}px;
          padding: 6px;
          margin: 0 8px;
          opacity: {op:.2};
          background-color: alpha(currentColor, 0.18);
          font-weight: bold;
          border-radius: 18px;
          {color_rule}
        }}
        button.xeneon-page-number.active {{
          opacity: 1;
          background-color: alpha(currentColor, 0.35);
        }}
        button.xeneon-page-settings {{
          min-width: {tpx}px;
          min-height: {tpx}px;
          padding: 6px;
          margin: 0 8px;
          opacity: {op:.2};
          border-radius: 18px;
          {color_rule}
        }}
        button.xeneon-page-settings.active {{
          opacity: 1;
        }}
        ",
        tpx = TOUCH_TARGET_PX,
        op = inactive_opacity,
    );
    PROVIDER.with(|p| {
        if let Some(provider) = p.borrow().as_ref() {
            provider.load_from_string(&css);
        }
    });
}

struct Inner {
    revealer: gtk::Revealer,
    row: gtk::Box,
    carousel: adw::Carousel,
    settings_page: gtk::Widget,
    // Keyed by the page's own gtk::Widget (glib object identity, not
    // pointer casts - GObject wrappers implement Hash/Eq that way) rather
    // than duck-typing a `custom_name` attribute onto the page the way
    // page_indicator.py does directly on the Python GObject - WidgetGrid
    // here is a plain Rust wrapper, not a Fixed subclass, so it has no
    // attribute to duck-type onto in the first place.
    named_pages: RefCell<HashMap<gtk::Widget, Rc<WidgetGrid>>>,
    hide_delay_seconds: RefCell<u32>,
    hide_source: RefCell<Option<gtk::glib::SourceId>>,
}

impl Inner {
    fn refresh(self: &Rc<Self>) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }

        for i in 0..self.carousel.n_pages() {
            let page = self.carousel.nth_page(i);
            let button = gtk::Button::new();
            button.add_css_class("flat");

            let custom_name = self.named_pages.borrow().get(&page).and_then(|grid| grid.custom_name());

            if page == self.settings_page {
                let icon = gtk::Image::from_icon_name("preferences-system-symbolic");
                icon.set_pixel_size(SETTINGS_ICON_PIXEL_SIZE);
                button.set_child(Some(&icon));
                button.add_css_class("xeneon-page-settings");
            } else if let Some(name) = custom_name {
                let label = gtk::Label::new(Some(&name));
                label.set_max_width_chars(10);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_single_line_mode(true);
                button.set_child(Some(&label));
                button.add_css_class("xeneon-page-number");
            } else {
                button.set_label(&(i + 1).to_string());
                button.add_css_class("xeneon-page-number");
            }

            let inner = self.clone();
            button.connect_clicked(move |_| {
                inner.carousel.scroll_to(&page, true);
                inner.show();
            });
            self.row.append(&button);
        }
        self.update_active();
    }

    fn update_active(self: &Rc<Self>) {
        let position = self.carousel.position().round() as i32;
        let mut i = 0;
        let mut child = self.row.first_child();
        while let Some(c) = child {
            if i == position {
                c.add_css_class("active");
            } else {
                c.remove_css_class("active");
            }
            child = c.next_sibling();
            i += 1;
        }
    }

    fn is_on_settings_page(&self) -> bool {
        let position = self.carousel.position().round().max(0.0) as u32;
        self.carousel.nth_page(position) == self.settings_page
    }

    fn show(self: &Rc<Self>) {
        self.revealer.set_reveal_child(true);
        self.revealer.set_can_target(true);
        self.update_active();

        if let Some(source) = self.hide_source.borrow_mut().take() {
            source.remove();
        }
        // Stays revealed on the settings page itself (where the future
        // opacity/hide-delay settings will live) instead of auto-hiding -
        // matches page_indicator.py's `_show()`.
        if self.is_on_settings_page() {
            return;
        }
        let inner = self.clone();
        let delay = *self.hide_delay_seconds.borrow();
        let source = gtk::glib::timeout_add_seconds_local(delay, move || {
            inner.hide();
            gtk::glib::ControlFlow::Break
        });
        *self.hide_source.borrow_mut() = Some(source);
    }

    fn hide(self: &Rc<Self>) {
        self.revealer.set_reveal_child(false);
        self.revealer.set_can_target(false);
        *self.hide_source.borrow_mut() = None;
    }
}

#[derive(Clone)]
pub struct PageIndicator {
    inner: Rc<Inner>,
}

impl PageIndicator {
    pub fn new(carousel: &adw::Carousel, settings_page: &impl IsA<gtk::Widget>) -> Self {
        set_style(DEFAULT_OPACITY_PERCENT, None);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(200);
        revealer.set_reveal_child(false);
        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::End);
        // CROSSFADE never shrinks the revealer's own allocation to 0 (unlike
        // a slide transition) - it just fades opacity, so leaving
        // can_target on permanently would keep this floating bar eating
        // taps meant for whatever's underneath it even while invisible.
        revealer.set_can_target(false);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_margin_bottom(10);
        revealer.set_child(Some(&row));

        let inner = Rc::new(Inner {
            revealer: revealer.clone(),
            row,
            carousel: carousel.clone(),
            settings_page: settings_page.clone().upcast(),
            named_pages: RefCell::new(HashMap::new()),
            hide_delay_seconds: RefCell::new(DEFAULT_HIDE_DELAY_SECONDS),
            hide_source: RefCell::new(None),
        });
        inner.refresh();

        // Reveals only on an actual page change (swipe, or a dot tap once
        // already visible mid-swipe) - deliberately not on hover/motion, so
        // just resting a finger or the pointer over the bar doesn't summon
        // it. Matches page_indicator.py's own two connections.
        {
            let inner = inner.clone();
            carousel.connect_notify_local(Some("position"), move |_, _| inner.show());
        }
        {
            let inner = inner.clone();
            carousel.connect_notify_local(Some("n-pages"), move |_, _| inner.refresh());
        }

        Self { inner }
    }

    pub fn widget(&self) -> &gtk::Revealer {
        &self.inner.revealer
    }

    /// Associates a widget-page with the `WidgetGrid` that owns its
    /// `custom_name` - call before the page is actually appended to the
    /// carousel so the very first automatic refresh (from the `n-pages`
    /// notify below) already has the name to show, rather than briefly
    /// showing a bare number first.
    pub fn register_page(&self, widget: &gtk::Fixed, grid: Rc<WidgetGrid>) {
        self.inner.named_pages.borrow_mut().insert(widget.clone().upcast(), grid);
    }

    /// Drops a page's name mapping - call when the page itself is removed
    /// (deleted, not just navigated away from) so `named_pages` doesn't
    /// keep an unused `Rc<WidgetGrid>` alive indefinitely. `refresh()`
    /// would silently skip a removed page's button anyway (it only
    /// iterates pages still in the carousel), so this is about releasing
    /// the reference, not about correctness of what's shown.
    pub fn unregister_page(&self, widget: &gtk::Fixed) {
        self.inner.named_pages.borrow_mut().remove(&widget.clone().upcast::<gtk::Widget>());
    }

    /// Forces a full rebuild - call after renaming a page, same as
    /// `window.py::save_page` unconditionally refreshing the indicator
    /// afterward ("cheap enough to just always refresh rather than
    /// tracking exactly which kind of page edit is behind this save").
    pub fn refresh(&self) {
        self.inner.refresh();
    }

    pub fn set_hide_delay_seconds(&self, seconds: u32) {
        *self.inner.hide_delay_seconds.borrow_mut() = seconds;
    }

    /// See the free function of the same name - a thin method wrapper so
    /// callers holding a `PageIndicator` don't need to import the module
    /// separately just for this one call.
    pub fn set_style(&self, opacity_percent: u32, color_hex: Option<&str>) {
        set_style(opacity_percent, color_hex);
    }
}
