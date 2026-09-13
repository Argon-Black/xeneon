//! Xeneon Dashboard - Rust/Relm4 port. Phase 1: a window sized to the
//! Xeneon Edge panel, F11 fullscreen, and a carousel of two pages backed
//! by real `WidgetGrid`s (drag/snap wired to xeneon-core's grid math),
//! populated with dummy widgets across every size preset so the base
//! interaction stack - move, snap, delete, page swipe - is testable before
//! any real plugin content exists. No persistence, i18n, or appearance
//! popover yet - those are later steps.

mod dashboard_widget;
mod grid_widget;
mod widgets;

use adw::prelude::*;
use relm4::prelude::*;
use xeneon_core::grid::{PAGE_H, PAGE_W, SIZE_L, SIZE_M, SIZE_S, SIZE_SQ, SIZE_SSX, SIZE_SX};

use grid_widget::WidgetGrid;

/// Real panel resolution of the Xeneon Edge bar screen at 100% display
/// scale (logical px == physical px - see CLAUDE.md in the Python app).
const WINDOW_WIDTH: i32 = 2560;
const WINDOW_HEIGHT: i32 = 720;

struct AppModel {
    fullscreened: bool,
    // Kept alive for the app's lifetime - each owns the Rc<RefCell<_>>
    // state its drag/delete closures capture.
    _grid1: WidgetGrid,
    _grid2: WidgetGrid,
}

#[derive(Debug)]
enum AppMsg {
    ToggleFullscreen,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_default_width: WINDOW_WIDTH,
            set_default_height: WINDOW_HEIGHT,
            #[watch]
            set_fullscreened: model.fullscreened,
            // GTK gives every window an implicit default CSD titlebar
            // unless told otherwise - separate from our own HeaderBar
            // widget below, and unaffected by merely hiding that widget
            // (which is what the band surviving two different attempts at
            // hiding the HeaderBar turned out to mean). Explicitly
            // undecorating the window in fullscreen removes it outright.
            #[watch]
            set_decorated: !model.fullscreened,

            // Global F11 toggle. The Python app instead registers a
            // Gio.SimpleAction with an accelerator at the application
            // level (plus a global-shortcuts portal binding for when the
            // window isn't focused) - deferred to a later phase along with
            // the settings/add-widget actions it's registered next to.
            add_controller = gtk::EventControllerKey {
                connect_key_pressed[sender] => move |_, key, _, _| {
                    if key == gtk::gdk::Key::F11 {
                        sender.input(AppMsg::ToggleFullscreen);
                        gtk::glib::Propagation::Stop
                    } else {
                        gtk::glib::Propagation::Proceed
                    }
                }
            },

            // A plain Overlay rather than Adw.ToolbarView: the header bar
            // sits *on top of* the carousel as an overlay child instead of
            // occupying its own structural slot, so hiding it in
            // fullscreen leaves no reserved space/shadow behind (an
            // earlier version used ToolbarView's add_top_bar, which kept
            // rendering a band where the bar used to be even once the bar
            // itself was hidden - overlay children never affect layout).
            #[wrap(Some)]
            set_content = &gtk::Overlay {
                // Adw.ToolbarView normally tags its content "view" for a
                // flat background; bypassing it (as we do here, going
                // straight to an Overlay) means the window's own
                // titlebar-blend gradient shows through as a banded
                // background otherwise - see window.py in the Python app,
                // same fix, same root cause.
                add_css_class: "view",

                #[wrap(Some)]
                set_child = &adw::Carousel {
                    #[local_ref]
                    page1_fixed -> gtk::Fixed {},
                    #[local_ref]
                    page2_fixed -> gtk::Fixed {},
                },

                add_overlay = &adw::HeaderBar {
                    set_valign: gtk::Align::Start,
                    // Hidden in fullscreen so the kiosk display shows only
                    // the widget grid, nothing else. Matches the Python
                    // app's `_on_fullscreened_changed`, simplified (that
                    // version swaps the whole content tree; here the
                    // header is already a non-structural overlay, so
                    // hiding it is enough).
                    #[watch]
                    set_visible: !model.fullscreened,

                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Xeneon Dashboard",
                    },
                },
            },
        }
    }

    fn init(_init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let grid1 = WidgetGrid::new(PAGE_W, PAGE_H);
        let grid2 = WidgetGrid::new(PAGE_W, PAGE_H);

        // Page 1: the wider presets - also exercises two M's landing
        // side by side automatically via find_free_position.
        grid1.add_widget("L", SIZE_L, widgets::dummy::build("L"));
        grid1.add_widget("M", SIZE_M, widgets::dummy::build("M"));
        grid1.add_widget("M", SIZE_M, widgets::dummy::build("M"));

        // Page 2: the narrower presets, to test swiping between pages.
        grid2.add_widget("S", SIZE_S, widgets::dummy::build("S"));
        grid2.add_widget("SQ", SIZE_SQ, widgets::dummy::build("SQ"));
        grid2.add_widget("SX", SIZE_SX, widgets::dummy::build("SX"));
        grid2.add_widget("SSX", SIZE_SSX, widgets::dummy::build("SSX"));

        // Quick, temporary visual-QA aid: fullscreen straight onto the
        // real Xeneon Edge panel (identified by its distinctive
        // resolution) when one is connected, bypassing window chrome
        // entirely so what's on screen is exactly what the final kiosk
        // display will show. The real version of this (display.py in the
        // Python app) is more involved - portal-backed global shortcut to
        // toggle it, `Adw.ToolbarView` swapped out of the content tree in
        // fullscreen - and is deferred to a later phase; this is only
        // here so dragging/snapping can be judged on the actual hardware
        // while building it. Detected *before* building the model so
        // `fullscreened` starts true (hiding the header bar via its
        // `#[watch]` binding) instead of only becoming true after a
        // subsequent F11 toggle.
        let xeneon = gtk::gdk::Display::default().and_then(|d| xeneon_monitor(&d));

        let model = AppModel { fullscreened: xeneon.is_some(), _grid1: grid1, _grid2: grid2 };

        let page1_fixed = model._grid1.widget();
        let page2_fixed = model._grid2.widget();

        let widgets = view_output!();

        // set_fullscreened above (via #[watch]) requests fullscreen on
        // whichever monitor the window is currently on; this targets the
        // Xeneon specifically.
        if let Some(monitor) = xeneon {
            root.fullscreen_on_monitor(&monitor);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ToggleFullscreen => {
                self.fullscreened = !self.fullscreened;
            }
        }
    }
}

/// The Xeneon Edge bar's real panel resolution (2560x720) is distinctive
/// enough among normal monitors to identify it by geometry alone, without
/// needing to match a connector/EDID name.
fn xeneon_monitor(display: &gtk::gdk::Display) -> Option<gtk::gdk::Monitor> {
    display.monitors().iter::<gtk::gdk::Monitor>().flatten().find(|m| {
        let geo = m.geometry();
        geo.width() == WINDOW_WIDTH && geo.height() == WINDOW_HEIGHT
    })
}

fn main() {
    let app = RelmApp::new("com.n3tlab.XeneonDashboardRust");
    app.run::<AppModel>(());
}
