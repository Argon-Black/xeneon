//! Generic chrome wrapped around every widget's own content: a title
//! label, a delete button, a move handle that drags the widget around its
//! `WidgetGrid`, and a configure button opening the appearance popover -
//! the generic controls (opacity/background/border/corners) every widget
//! gets, plus the plugin's own settings alongside them when it has any.
//! Ported from `DashboardWidget` in grid.py, including its hover/tap
//! reveal: the label and the three corner buttons stay hidden until the
//! pointer enters the card (or, on touch, it's tapped, since touch has no
//! hover/leave events to hide them again afterwards).

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use crate::appearance_popover;
use crate::i18n_runtime as i18n;
use xeneon_core::appearance::WidgetAppearance;

/// The constructed chrome plus the interactive bits `WidgetGrid` needs to
/// wire up drag/delete/save behaviour - it owns the gestures/signals, this
/// module only builds the widgets.
pub struct DashboardWidgetHandles {
    pub root: gtk::Overlay,
    pub delete_button: gtk::Button,
    pub move_button: gtk::Button,
    /// Shared with the appearance popover, which mutates it live - read
    /// this back (`.borrow().clone()`) whenever persisting the widget.
    pub appearance: Rc<RefCell<WidgetAppearance>>,
    /// `WidgetGrid` connects this popover's "closed" signal to persist
    /// both the appearance and the plugin's own state, the same single
    /// save trigger the Python original uses for both.
    pub settings_popover: gtk::Popover,
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    title_key: &str,
    content: &impl IsA<gtk::Widget>,
    w: i32,
    h: i32,
    css_class: String,
    appearance: WidgetAppearance,
    settings: Option<&gtk::Widget>,
    on_reset: Option<Box<dyn Fn()>>,
) -> DashboardWidgetHandles {
    let root = gtk::Overlay::new();
    root.set_size_request(w, h);
    root.add_css_class("card");
    root.add_css_class(&css_class);
    root.set_child(Some(content));

    // Never steals clicks meant for the content underneath it - same fix
    // as the Python original's `_header.set_can_target(False)`. Empty
    // title_key (dummy widgets, which show their size as their own big
    // centered content instead) means no header label at all.
    let header = gtk::Label::new(None);
    header.set_halign(gtk::Align::Start);
    header.set_valign(gtk::Align::Start);
    header.set_margin_start(8);
    header.set_margin_top(6);
    header.add_css_class("caption-heading");
    header.set_can_target(false);
    if !title_key.is_empty() {
        header.set_label(&i18n::t(title_key));
        i18n::on_change({
            let header = header.clone();
            let title_key = title_key.to_string();
            move || header.set_label(&i18n::t(&title_key))
        });
    }
    root.add_overlay(&header);

    let delete_button = gtk::Button::from_icon_name("window-close-symbolic");
    delete_button.set_halign(gtk::Align::End);
    delete_button.set_valign(gtk::Align::Start);
    delete_button.set_margin_end(4);
    delete_button.set_margin_top(4);
    delete_button.add_css_class("circular");
    delete_button.add_css_class("flat");
    delete_button.set_tooltip_text(Some(&i18n::t("widgets.delete_tooltip")));
    root.add_overlay(&delete_button);

    // A plain glyph rather than an icon name - avoids depending on a
    // "drag handle" symbolic icon actually existing in whatever icon theme
    // is installed (window-close-symbolic above is safe, a drag-handle
    // icon is far less universally present).
    let move_button = gtk::Button::with_label("⠿");
    move_button.set_halign(gtk::Align::Start);
    move_button.set_valign(gtk::Align::End);
    move_button.set_margin_start(4);
    move_button.set_margin_bottom(4);
    move_button.add_css_class("circular");
    move_button.add_css_class("flat");
    move_button.set_cursor_from_name(Some("move"));
    move_button.set_tooltip_text(Some(&i18n::t("widgets.move_tooltip")));
    root.add_overlay(&move_button);

    let configure_button = gtk::Button::from_icon_name("emblem-system-symbolic");
    configure_button.set_halign(gtk::Align::End);
    configure_button.set_valign(gtk::Align::End);
    configure_button.set_margin_end(4);
    configure_button.set_margin_bottom(4);
    configure_button.add_css_class("circular");
    configure_button.add_css_class("flat");
    configure_button.set_tooltip_text(Some(&i18n::t("widgets.configure_tooltip")));
    root.add_overlay(&configure_button);

    let appearance = Rc::new(RefCell::new(appearance));
    let popover = appearance_popover::build(appearance.clone(), css_class, settings, on_reset);
    popover.set_parent(&configure_button);
    configure_button.connect_clicked({
        let popover = popover.clone();
        move |_| popover.popup()
    });

    // Closes the popover on any click elsewhere, including the first touch
    // of a carousel swipe - can't just turn autohide back on for this (see
    // the comment on `set_autohide` in appearance_popover::build, still
    // needed to survive the color/file dialogs), so instead this watches
    // the window itself in the capture phase: a `Gtk.Popover`, like the
    // dialogs it opens, renders into its own native surface, so any press
    // that reaches the window controller at all necessarily landed outside
    // the popover. Wired on "map"/"closed" rather than once at build time
    // because the card isn't attached to a window yet here, so there's no
    // root to watch.
    let outside_click: Rc<RefCell<Option<gtk::GestureClick>>> = Rc::new(RefCell::new(None));
    popover.connect_map({
        let outside_click = outside_click.clone();
        move |popover| {
            let Some(root) = popover.root() else { return };
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed({
                let popover = popover.clone();
                move |_, _, _, _| popover.popdown()
            });
            root.add_controller(click.clone());
            *outside_click.borrow_mut() = Some(click);
        }
    });
    popover.connect_closed({
        let outside_click = outside_click.clone();
        move |popover| {
            if let Some(click) = outside_click.borrow_mut().take() {
                if let Some(root) = popover.root() {
                    root.remove_controller(&click);
                }
            }
        }
    });

    // Hidden until revealed - via opacity for the label (stable allocation,
    // matches grid.py) and via visibility for the buttons (so they don't
    // intercept clicks meant for the content underneath while hidden).
    header.set_opacity(0.0);
    delete_button.set_visible(false);
    move_button.set_visible(false);
    configure_button.set_visible(false);

    let hide_source: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::new(RefCell::new(None));

    let hide_now: Rc<dyn Fn()> = Rc::new({
        let header = header.clone();
        let delete_button = delete_button.clone();
        let move_button = move_button.clone();
        let configure_button = configure_button.clone();
        let hide_source = hide_source.clone();
        move || {
            header.set_opacity(0.0);
            delete_button.set_visible(false);
            move_button.set_visible(false);
            configure_button.set_visible(false);
            *hide_source.borrow_mut() = None;
        }
    });

    // `hide_after_seconds` is only used for the touch-tap path (no "leave"
    // event to hide on, unlike hover) - see TOUCH_REVEAL_SECONDS in
    // grid.py.
    let reveal: Rc<dyn Fn(Option<u32>)> = Rc::new({
        let header = header.clone();
        let delete_button = delete_button.clone();
        let move_button = move_button.clone();
        let configure_button = configure_button.clone();
        let hide_source = hide_source.clone();
        let hide_now = hide_now.clone();
        move |hide_after_seconds| {
            header.set_opacity(1.0);
            delete_button.set_visible(true);
            move_button.set_visible(true);
            configure_button.set_visible(true);
            if let Some(source) = hide_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(seconds) = hide_after_seconds {
                let hide_now = hide_now.clone();
                let source = gtk::glib::timeout_add_seconds_local(seconds, move || {
                    hide_now();
                    gtk::glib::ControlFlow::Break
                });
                *hide_source.borrow_mut() = Some(source);
            }
        }
    });

    let hover = gtk::EventControllerMotion::new();
    hover.connect_enter({
        let reveal = reveal.clone();
        move |_, _, _| reveal(None)
    });
    hover.connect_leave({
        let hide_now = hide_now.clone();
        let popover = popover.clone();
        move |_| {
            // Stays revealed while its own settings popover is open, even
            // though the pointer has left the card to reach the popover.
            if popover.is_visible() {
                return;
            }
            hide_now();
        }
    });
    root.add_controller(hover);

    let tap = gtk::GestureClick::new();
    tap.connect_released(move |_, _, _, _| reveal(Some(3)));
    root.add_controller(tap);

    DashboardWidgetHandles { root, delete_button, move_button, appearance, settings_popover: popover }
}
