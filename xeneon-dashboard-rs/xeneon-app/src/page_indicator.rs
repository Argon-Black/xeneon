// SPDX-License-Identifier: GPL-3.0-or-later
//! Clickable, numbered page buttons floating over the carousel's bottom
//! edge - shown only while actually navigating between pages (a swipe, or
//! tapping a dot), auto-hidden after a delay, except on the settings page
//! where it stays up. Ported from `page_indicator.py`, including the
//! opacity/color customization driven by `Config.indicator_opacity`/
//! `indicator_button_color` (see `set_style`/`set_hide_delay_seconds`,
//! wired to the "Interface" settings group).
//!
//! Also carries a "+" button (`.xeneon-page-add`), sitting right before the
//! settings gear icon - not tied to any carousel page itself, just an
//! action wired to `on_add_page` in `PageIndicator::new` (see
//! `AppMsg::AddPage`/`AppModel::create_page` in main.rs for what it
//! actually does: create an empty page and navigate to it). This one isn't
//! in the Python original - it was added directly in this Rust port.
//!
//! The optional Home Assistant page (see ha_page.rs) gets a similar, but
//! not identical, special treatment to the settings page: its own icon
//! button (reusing `.xeneon-page-settings`'s look, just a different
//! icon), skipped by the numbered-dot loop below, placed leftmost in the
//! row (it's also the carousel's leftmost page - see main.rs's append
//! order) rather than generalizing `settings_page`/`ha_page` into a list
//! - there are only ever these two, both fixed for the app's whole
//! lifetime (neither is ever added or removed while running - the HA
//! page's own enable switch is a full relaunch, see ha_page.rs), so a
//! list would just be indirection with no case it actually needs to
//! handle. Unlike settings, it does NOT keep the bar permanently
//! revealed while on it (see `is_on_settings_page`'s own doc comment on
//! `show()`) - a full-screen dashboard has to actually be full-screen.

use adw::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use crate::grid_widget::WidgetGrid;
use crate::i18n_runtime;

const DEFAULT_HIDE_DELAY_SECONDS: u32 = 2;
// Sized off the Xeneon Edge's actual pixel density (2560x720 over a 14.5"
// 32:9 panel, ~183 ppi -> ~72px/cm) so a fingertip-sized tap lands
// reliably - see page_indicator.py's own derivation and CLAUDE.md.
const TOUCH_TARGET_PX: i32 = 72;
const SETTINGS_ICON_PIXEL_SIZE: i32 = 36;
pub const DEFAULT_OPACITY_PERCENT: u32 = 55;

// Vertical padding baked into every indicator button's own CSS rule (see
// set_style()'s "padding: 6px" on .xeneon-page-number/.xeneon-page-settings/
// .xeneon-page-add below) - doubled since it applies above and below.
const BUTTON_VERTICAL_PADDING_PX: i32 = 6;
// Space PageIndicator::new()'s own `row` box keeps between its buttons and
// the window's bottom edge (`row.set_margin_bottom(10)`).
const ROW_MARGIN_BOTTOM_PX: i32 = 10;

/// Total height the indicator bar actually occupies at the bottom of the
/// window - button touch target plus its own vertical padding plus the
/// row's margin below it. Exposed so the settings page (where the bar
/// stays permanently visible, unlike every other page where it auto-hides
/// after a swipe) can reserve exactly this much space at its own bottom
/// edge instead of letting its content run underneath the bar - see
/// settings_page.rs's own use of this constant.
pub const RESERVED_HEIGHT_PX: i32 = TOUCH_TARGET_PX + 2 * BUTTON_VERTICAL_PADDING_PX + ROW_MARGIN_BOTTOM_PX;

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
        button.xeneon-page-add {{
          min-width: {tpx}px;
          min-height: {tpx}px;
          padding: 6px;
          margin: 0 8px;
          opacity: {op:.2};
          border-radius: 18px;
          {color_rule}
        }}
        /* Small numbered dots for the settings page's own vertical
           carousel (see settings_page.rs) - same opacity/color knobs as
           every other indicator button above, just sized for a slim
           stack of 2-3 rather than a full touch target. */
        button.xeneon-settings-page-dot {{
          min-width: 26px;
          min-height: 26px;
          padding: 2px;
          margin: 4px 0;
          opacity: {op:.2};
          background-color: alpha(currentColor, 0.18);
          font-weight: bold;
          font-size: 10px;
          border-radius: 13px;
          {color_rule}
        }}
        button.xeneon-settings-page-dot.active {{
          opacity: 1;
          background-color: alpha(currentColor, 0.35);
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
    // `None` when the Home Assistant page is disabled - see its own module
    // doc comment. Compared by identity (GObject Hash/Eq) the same way
    // `settings_page` is, everywhere both are used below.
    ha_page: Option<gtk::Widget>,
    // Keyed by the page's own gtk::Widget (glib object identity, not
    // pointer casts - GObject wrappers implement Hash/Eq that way) rather
    // than duck-typing a `custom_name` attribute onto the page the way
    // page_indicator.py does directly on the Python GObject - WidgetGrid
    // here is a plain Rust wrapper, not a Fixed subclass, so it has no
    // attribute to duck-type onto in the first place.
    named_pages: RefCell<HashMap<gtk::Widget, Rc<WidgetGrid>>>,
    hide_delay_seconds: RefCell<u32>,
    hide_source: RefCell<Option<gtk::glib::SourceId>>,
    // Called (no args) when the "+" button is tapped - see the module doc
    // comment above and AppMsg::AddPage in main.rs.
    on_add_page: Box<dyn Fn()>,
    // (button, its page) pairs rebuilt every refresh() - lets update_active()
    // below match the *current carousel page* against the button that
    // represents it, rather than assuming the row's Nth child is always the
    // Nth carousel page. That assumption broke the moment the "+" button
    // (not tied to any page) was inserted into the row between the last
    // numbered button and the settings one - only page-associated buttons
    // go in here, so the "+" button is simply never matched/never active.
    page_buttons: RefCell<Vec<(gtk::Widget, gtk::Widget)>>,
}

impl Inner {
    fn refresh(self: &Rc<Self>) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        self.page_buttons.borrow_mut().clear();

        // The Home Assistant page, when enabled, is always the carousel's
        // very first page (see main.rs's append order) - its own button is
        // built first below, right here, so it lands leftmost in the row,
        // ahead of every numbered dot.
        if let Some(ha_page) = &self.ha_page {
            let button = gtk::Button::new();
            button.add_css_class("flat");
            let icon = gtk::Image::from_icon_name("user-home-symbolic");
            icon.set_pixel_size(SETTINGS_ICON_PIXEL_SIZE);
            button.set_child(Some(&icon));
            // Reuses the settings button's own CSS class - same size,
            // same "set apart from the numbered pages" look, just a
            // different icon. See the module doc comment on why a second
            // dedicated field/button beats generalizing into a list here.
            button.add_css_class("xeneon-page-settings");
            let inner = self.clone();
            let ha_page_for_click = ha_page.clone();
            button.connect_clicked(move |_| {
                inner.carousel.scroll_to(&ha_page_for_click, true);
                inner.show();
            });
            self.page_buttons.borrow_mut().push((button.clone().upcast(), ha_page.clone()));
            self.row.append(&button);
        }

        // Settings is always the carousel's last page (see main.rs's
        // append order), but skipped here by identity rather than assumed
        // by position - it's built separately below, after the "+" button,
        // so the two don't end up interleaved by whatever order n_pages
        // happens to iterate in.
        for i in 0..self.carousel.n_pages() {
            let page = self.carousel.nth_page(i);
            if page == self.settings_page || self.ha_page.as_ref() == Some(&page) {
                continue;
            }
            let button = gtk::Button::new();
            button.add_css_class("flat");
            button.add_css_class("xeneon-page-number");

            let custom_name = self.named_pages.borrow().get(&page).and_then(|grid| grid.custom_name());
            if let Some(name) = custom_name {
                let label = gtk::Label::new(Some(&name));
                label.set_max_width_chars(10);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_single_line_mode(true);
                button.set_child(Some(&label));
            } else {
                button.set_label(&(i + 1).to_string());
            }

            let inner = self.clone();
            let page_for_click = page.clone();
            button.connect_clicked(move |_| {
                inner.carousel.scroll_to(&page_for_click, true);
                inner.show();
            });
            self.page_buttons.borrow_mut().push((button.clone().upcast(), page));
            self.row.append(&button);
        }

        // The "+" button: sits right before the settings gear icon, not
        // tied to any carousel page - see the module doc comment above.
        let add_button = gtk::Button::from_icon_name("list-add-symbolic");
        add_button.add_css_class("flat");
        add_button.add_css_class("xeneon-page-add");
        add_button.set_tooltip_text(Some(&i18n_runtime::t("carousel.add_page_tooltip")));
        let inner = self.clone();
        add_button.connect_clicked(move |_| (inner.on_add_page)());
        self.row.append(&add_button);

        let settings_button = gtk::Button::new();
        settings_button.add_css_class("flat");
        let icon = gtk::Image::from_icon_name("preferences-system-symbolic");
        icon.set_pixel_size(SETTINGS_ICON_PIXEL_SIZE);
        settings_button.set_child(Some(&icon));
        settings_button.add_css_class("xeneon-page-settings");
        let inner = self.clone();
        let settings_page_for_click = self.settings_page.clone();
        settings_button.connect_clicked(move |_| {
            inner.carousel.scroll_to(&settings_page_for_click, true);
            inner.show();
        });
        self.page_buttons.borrow_mut().push((settings_button.clone().upcast(), self.settings_page.clone()));
        self.row.append(&settings_button);

        self.update_active();
    }

    fn update_active(self: &Rc<Self>) {
        // Guards nth_page() below: it panics (asserts n < n_pages()) on an
        // empty carousel, which is exactly the state the very first
        // refresh() runs in - PageIndicator::new() calls it before main.rs
        // has appended a single page yet. The pre-existing is_on_settings_page()
        // has the same nth_page() call but was never at risk of this, since
        // it's only reached from show(), itself only ever triggered by a
        // carousel signal that can't fire before there's at least one page.
        if self.carousel.n_pages() == 0 {
            return;
        }
        let position = self.carousel.position().round().max(0.0) as u32;
        let current_page = self.carousel.nth_page(position);
        for (button, page) in self.page_buttons.borrow().iter() {
            if *page == current_page {
                button.add_css_class("active");
            } else {
                button.remove_css_class("active");
            }
        }
    }

    fn is_on_settings_page(&self) -> bool {
        // Guards nth_page() against a carousel that has no pages yet -
        // real at construction time: PageIndicator::new() runs before
        // main.rs appends a single page, and calling nth_page() on an
        // empty carousel is an out-of-bounds panic in libadwaita's own
        // Rust bindings (assert!(n < self.n_pages())), not a graceful
        // None.
        let n_pages = self.carousel.n_pages();
        if n_pages == 0 {
            return false;
        }
        let position = (self.carousel.position().round().max(0.0) as u32).min(n_pages - 1);
        self.carousel.nth_page(position) == self.settings_page
    }

    /// The mouse wheel flips between carousel pages by default
    /// (`Adw.Carousel`'s own `allow-scroll-wheel`, on unless told
    /// otherwise) - fine on a real widget page, but on the settings page
    /// it fights with scrolling a control under the pointer (the opacity
    /// slider, a spin row): the wheel event would swipe the whole page
    /// away instead of nudging that control's value. Off only while
    /// actually on the settings page, so every other page keeps the
    /// wheel-swipe convenience.
    fn update_scroll_wheel(&self) {
        self.carousel.set_allow_scroll_wheel(!self.is_on_settings_page());
    }

    fn show(self: &Rc<Self>) {
        self.revealer.set_reveal_child(true);
        self.revealer.set_can_target(true);
        self.update_active();

        if let Some(source) = self.hide_source.borrow_mut().take() {
            source.remove();
        }
        // Stays revealed on the settings page itself (its own scrollable/
        // draggable controls need it) instead of auto-hiding - matches
        // page_indicator.py's `_show()`. Deliberately NOT also kept up on
        // the Home Assistant page: a first attempt at this did exactly
        // that (see git history), and the user rejected it on sight - a
        // permanent bar defeats the point of a full-screen, unobstructed
        // dashboard. The HA page still calls `show()` (a normal, timed
        // reveal) from its own click-to-reveal affordance instead - see
        // ha_page.rs's `reveal_indicator`.
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
    /// `on_add_page` fires (no args) whenever the "+" button is tapped -
    /// the caller decides what that actually means (main.rs wires it to
    /// `AppMsg::AddPage`, which creates an empty page and navigates to it,
    /// or shows a "max pages" toast if already at the cap). `ha_page` is
    /// `None` when the Home Assistant page is disabled - see ha_page.rs
    /// and this module's own doc comment.
    pub fn new(
        carousel: &adw::Carousel,
        settings_page: &impl IsA<gtk::Widget>,
        ha_page: Option<&impl IsA<gtk::Widget>>,
        on_add_page: impl Fn() + 'static,
    ) -> Self {
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
            ha_page: ha_page.map(|w| w.clone().upcast()),
            named_pages: RefCell::new(HashMap::new()),
            hide_delay_seconds: RefCell::new(DEFAULT_HIDE_DELAY_SECONDS),
            hide_source: RefCell::new(None),
            on_add_page: Box::new(on_add_page),
            page_buttons: RefCell::new(Vec::new()),
        });
        inner.refresh();
        inner.update_scroll_wheel();

        // Reveals only on an actual page change (swipe, or a dot tap once
        // already visible mid-swipe) - deliberately not on hover/motion, so
        // just resting a finger or the pointer over the bar doesn't summon
        // it. Matches page_indicator.py's own two connections.
        {
            let inner = inner.clone();
            carousel.connect_notify_local(Some("position"), move |_, _| {
                inner.show();
                inner.update_scroll_wheel();
            });
        }
        {
            let inner = inner.clone();
            carousel.connect_notify_local(Some("n-pages"), move |_, _| {
                inner.refresh();
                // Pages append one at a time at startup while position
                // stays 0 throughout, so "position" alone might never
                // fire before the settings page itself is appended -
                // re-checked here too so allow-scroll-wheel is never left
                // stale at the true-until-now default.
                inner.update_scroll_wheel();
            });
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

    /// Reveals the bar right away, same as an actual swipe/dot-click would
    /// - see main.rs's one call site (right after every page is appended
    /// at startup) for why: the Home Assistant page needs this to ever be
    /// reachable by mouse if it's the very first page shown.
    pub fn show(&self) {
        self.inner.show();
    }

    /// See the free function of the same name - a thin method wrapper so
    /// callers holding a `PageIndicator` don't need to import the module
    /// separately just for this one call.
    pub fn set_style(&self, opacity_percent: u32, color_hex: Option<&str>) {
        set_style(opacity_percent, color_hex);
    }
}
