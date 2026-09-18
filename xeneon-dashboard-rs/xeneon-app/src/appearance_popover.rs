// SPDX-License-Identifier: GPL-3.0-or-later
//! The configure button's popup: the appearance controls every widget
//! gets for free (background transparency/color/image, border, corners)
//! on the left, plus - when a plugin passes one - that plugin's own
//! settings on the right, separated by a vertical bar. Side by side rather
//! than stacked so adding plugin-specific settings widens the popover
//! instead of making it taller (a taller popover is more likely to need
//! repositioning over, and visually colliding with, another widget lower
//! on the page). Ported from `AppearancePopover` in widget_appearance.py.

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use crate::appearance_css;
use crate::i18n_runtime as i18n;
use xeneon_core::appearance::WidgetAppearance;

pub(crate) const IMAGE_MIME_TYPES: [&str; 7] =
    ["image/png", "image/jpeg", "image/webp", "image/bmp", "image/gif", "image/tiff", "image/svg+xml"];

fn add_color_row(box_: &gtk::Box, initial: &gtk::gdk::RGBA) -> (gtk::Label, gtk::ColorDialogButton) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let label = gtk::Label::new(None);
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    row.append(&label);
    let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    button.set_rgba(initial);
    row.append(&button);
    box_.append(&row);
    (label, button)
}

pub(crate) fn hex_to_rgba(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::BLACK)
}

pub(crate) fn rgba_to_hex(rgba: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.red() * 255.0).round() as u8,
        (rgba.green() * 255.0).round() as u8,
        (rgba.blue() * 255.0).round() as u8
    )
}

/// `appearance` is shared with whoever needs to read it back for
/// persistence (the caller reads `appearance.borrow()` when it wants to
/// save, typically on this popover's "closed" signal - this module only
/// mutates it live and re-renders the CSS). `on_reset`, if given, runs
/// right after the appearance itself resets - used by plugins (Clock) that
/// need to reset their *own* settings and resync their controls too, since
/// this reset button is the only reset affordance a widget gets.
pub fn build(
    appearance: Rc<RefCell<WidgetAppearance>>,
    css_class: String,
    extra_settings: Option<&gtk::Widget>,
    on_reset: Option<Box<dyn Fn()>>,
) -> gtk::Popover {
    appearance_css::ensure_reset_button_css_installed();
    appearance_css::apply(&css_class, &appearance.borrow());

    let popover = gtk::Popover::new();
    // GTK's autohide popover closes itself the instant a sub-dialog (the
    // color or file chooser) grabs focus, before the user can pick
    // anything - the explicit close button below is how it's meant to be
    // dismissed instead, same as the Python original.
    popover.set_autohide(false);
    popover.set_position(gtk::PositionType::Right);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
    outer.set_margin_top(6);
    outer.set_margin_bottom(12);
    outer.set_margin_start(12);
    outer.set_margin_end(12);

    let close_button = gtk::Button::from_icon_name("window-close-symbolic");
    close_button.add_css_class("flat");
    close_button.add_css_class("circular");
    close_button.set_halign(gtk::Align::End);
    close_button.connect_clicked({
        let popover = popover.clone();
        move |_| popover.popdown()
    });
    outer.append(&close_button);

    let generic = gtk::Box::new(gtk::Orientation::Vertical, 10);
    generic.set_size_request(240, -1);

    let opacity_label = gtk::Label::new(None);
    opacity_label.set_halign(gtk::Align::Start);
    generic.append(&opacity_label);
    let opacity_scale = gtk::Scale::new(gtk::Orientation::Horizontal, gtk::Adjustment::NONE);
    opacity_scale.set_range(0.0, 100.0);
    opacity_scale.set_value(appearance.borrow().opacity * 100.0);
    opacity_scale.set_draw_value(true);
    opacity_scale.set_value_pos(gtk::PositionType::Right);
    generic.append(&opacity_scale);

    let (bg_color_label, bg_color_button) = add_color_row(&generic, &hex_to_rgba(&appearance.borrow().bg_color));

    let image_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let image_button = gtk::Button::new();
    image_row.append(&image_button);
    let image_clear_button = gtk::Button::new();
    image_row.append(&image_clear_button);
    generic.append(&image_row);

    generic.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let border_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let border_label = gtk::Label::new(None);
    border_label.set_hexpand(true);
    border_label.set_halign(gtk::Align::Start);
    border_row.append(&border_label);
    let border_switch = gtk::Switch::new();
    border_switch.set_active(appearance.borrow().border_enabled);
    border_switch.set_valign(gtk::Align::Center);
    border_row.append(&border_switch);
    generic.append(&border_row);

    let border_width_label = gtk::Label::new(None);
    border_width_label.set_halign(gtk::Align::Start);
    generic.append(&border_width_label);
    let border_width_spin = gtk::SpinButton::with_range(1.0, 12.0, 1.0);
    border_width_spin.set_value(appearance.borrow().border_width as f64);
    generic.append(&border_width_spin);

    let (border_color_label, border_color_button) =
        add_color_row(&generic, &hex_to_rgba(&appearance.borrow().border_color));

    generic.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let corner_label = gtk::Label::new(None);
    corner_label.set_halign(gtk::Align::Start);
    generic.append(&corner_label);
    let corner_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let round_button = gtk::ToggleButton::new();
    let square_button = gtk::ToggleButton::new();
    square_button.set_group(Some(&round_button));
    round_button.set_active(appearance.borrow().rounded);
    square_button.set_active(!appearance.borrow().rounded);
    corner_row.append(&round_button);
    corner_row.append(&square_button);
    generic.append(&corner_row);

    generic.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let reset_button = gtk::Button::new();
    reset_button.add_css_class("xeneon-reset-button");
    reset_button.set_halign(gtk::Align::Center);
    reset_button.set_margin_top(6);
    generic.append(&reset_button);

    match extra_settings {
        None => outer.append(&generic),
        Some(extra) => {
            let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            root.append(&generic);
            root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            root.append(extra);
            outer.append(&root);
        }
    }

    popover.set_child(Some(&outer));

    // --- signal wiring: controls push one-way into `appearance`, then
    // re-render its CSS live (persistence-on-close is the caller's job) ---
    opacity_scale.connect_value_changed({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |scale| {
            let mut a = appearance.borrow_mut();
            a.opacity = scale.value() / 100.0;
            a.touched.insert(xeneon_core::appearance::TouchedField::Bg);
            appearance_css::apply(&css_class, &a);
        }
    });
    bg_color_button.connect_rgba_notify({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |b| {
            let mut a = appearance.borrow_mut();
            a.bg_color = rgba_to_hex(&b.rgba());
            a.touched.insert(xeneon_core::appearance::TouchedField::Bg);
            appearance_css::apply(&css_class, &a);
        }
    });
    image_button.connect_clicked({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        let popover = popover.clone();
        move |_| {
            let dialog = gtk::FileDialog::new();
            let image_filter = gtk::FileFilter::new();
            image_filter.set_name(Some(&i18n::t("widgets.appearance.bg_image_filter")));
            for mime in IMAGE_MIME_TYPES {
                image_filter.add_mime_type(mime);
            }
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&image_filter);
            dialog.set_filters(Some(&filters));
            let root = popover.root().and_downcast::<gtk::Window>();
            let appearance = appearance.clone();
            let css_class = css_class.clone();
            dialog.open(root.as_ref(), gtk::gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let mut a = appearance.borrow_mut();
                        a.bg_image_path = Some(path.display().to_string());
                        a.touched.insert(xeneon_core::appearance::TouchedField::Bg);
                        appearance_css::apply(&css_class, &a);
                    }
                }
                // A dismissed picker (Err) just means the user closed it
                // without choosing anything - nothing to do.
            });
        }
    });
    image_clear_button.connect_clicked({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |_| {
            let mut a = appearance.borrow_mut();
            a.bg_image_path = None;
            a.touched.insert(xeneon_core::appearance::TouchedField::Bg);
            appearance_css::apply(&css_class, &a);
        }
    });
    border_switch.connect_active_notify({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |s| {
            let mut a = appearance.borrow_mut();
            a.border_enabled = s.is_active();
            a.touched.insert(xeneon_core::appearance::TouchedField::Border);
            appearance_css::apply(&css_class, &a);
        }
    });
    border_width_spin.connect_value_changed({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |spin| {
            let mut a = appearance.borrow_mut();
            a.border_width = spin.value() as u32;
            a.touched.insert(xeneon_core::appearance::TouchedField::Border);
            appearance_css::apply(&css_class, &a);
        }
    });
    border_color_button.connect_rgba_notify({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |b| {
            let mut a = appearance.borrow_mut();
            a.border_color = rgba_to_hex(&b.rgba());
            a.touched.insert(xeneon_core::appearance::TouchedField::Border);
            appearance_css::apply(&css_class, &a);
        }
    });
    round_button.connect_toggled({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        move |b| {
            let mut a = appearance.borrow_mut();
            a.rounded = b.is_active();
            a.touched.insert(xeneon_core::appearance::TouchedField::Corner);
            appearance_css::apply(&css_class, &a);
        }
    });
    reset_button.connect_clicked({
        let appearance = appearance.clone();
        let css_class = css_class.clone();
        let opacity_scale = opacity_scale.clone();
        let bg_color_button = bg_color_button.clone();
        let border_switch = border_switch.clone();
        let border_width_spin = border_width_spin.clone();
        let border_color_button = border_color_button.clone();
        let round_button = round_button.clone();
        let square_button = square_button.clone();
        move |_| {
            // Re-reads every control's displayed value from `appearance`
            // after resetting it - needed since reset() changes the model
            // directly and these controls otherwise only push edits
            // one-way. Extracted into owned locals *before* touching any
            // control: each set_value/set_active below fires that
            // control's own "changed" signal synchronously, which would
            // try to borrow_mut() `appearance` again while a live
            // `Ref` from a held `.borrow()` was still outstanding here -
            // a guaranteed panic (Python has no such borrow-checking, so
            // this reentrancy is harmless there; matches its
            // _sync_controls() re-pushing the same reset values through
            // each control's normal change handler too).
            let (opacity, bg_color, border_enabled, border_width, border_color, rounded) = {
                let mut a = appearance.borrow_mut();
                a.reset();
                appearance_css::apply(&css_class, &a);
                (a.opacity, a.bg_color.clone(), a.border_enabled, a.border_width, a.border_color.clone(), a.rounded)
            };
            opacity_scale.set_value(opacity * 100.0);
            bg_color_button.set_rgba(&hex_to_rgba(&bg_color));
            border_switch.set_active(border_enabled);
            border_width_spin.set_value(border_width as f64);
            border_color_button.set_rgba(&hex_to_rgba(&border_color));
            round_button.set_active(rounded);
            square_button.set_active(!rounded);
            if let Some(on_reset) = &on_reset {
                on_reset();
            }
        }
    });

    let retranslate = {
        let close_button = close_button.clone();
        let opacity_label = opacity_label.clone();
        let bg_color_label = bg_color_label.clone();
        let image_button = image_button.clone();
        let image_clear_button = image_clear_button.clone();
        let border_label = border_label.clone();
        let border_width_label = border_width_label.clone();
        let border_color_label = border_color_label.clone();
        let corner_label = corner_label.clone();
        let round_button = round_button.clone();
        let square_button = square_button.clone();
        let reset_button = reset_button.clone();
        move || {
            close_button.set_tooltip_text(Some(&i18n::t("widgets.appearance.close_tooltip")));
            opacity_label.set_label(&i18n::t("widgets.appearance.opacity"));
            bg_color_label.set_label(&i18n::t("widgets.appearance.bg_color"));
            image_button.set_label(&i18n::t("widgets.appearance.bg_image_choose"));
            image_clear_button.set_label(&i18n::t("widgets.appearance.bg_image_clear"));
            border_label.set_label(&i18n::t("widgets.appearance.border_enabled"));
            border_width_label.set_label(&i18n::t("widgets.appearance.border_width"));
            border_color_label.set_label(&i18n::t("widgets.appearance.border_color"));
            corner_label.set_label(&i18n::t("widgets.appearance.corner"));
            round_button.set_label(&i18n::t("widgets.appearance.corner_round"));
            square_button.set_label(&i18n::t("widgets.appearance.corner_square"));
            reset_button.set_label(&i18n::t("widgets.appearance.reset"));
        }
    };
    retranslate();
    i18n::on_change(retranslate);

    popover
}
