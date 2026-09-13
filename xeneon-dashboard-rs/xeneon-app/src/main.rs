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

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Xeneon Dashboard",
                    },
                },

                #[wrap(Some)]
                set_content = &adw::Carousel {
                    #[local_ref]
                    page1_fixed -> gtk::Fixed {
                        set_visible: true,
                    },
                    #[local_ref]
                    page2_fixed -> gtk::Fixed {
                        set_visible: true,
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

        let model = AppModel { fullscreened: false, _grid1: grid1, _grid2: grid2 };

        let page1_fixed = model._grid1.widget();
        let page2_fixed = model._grid2.widget();

        let widgets = view_output!();

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

fn main() {
    let app = RelmApp::new("com.n3tlab.XeneonDashboardRust");
    app.run::<AppModel>(());
}
