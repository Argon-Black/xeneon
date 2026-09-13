//! Xeneon Dashboard - Rust/Relm4 port. This is the Phase 1 skeleton: a
//! window sized to the Xeneon Edge panel with a header bar and a carousel,
//! one placeholder page, and F11 fullscreen toggle. No widget grid, no
//! persistence wiring, no plugins yet - those are the next steps (see the
//! plan doc referenced in project memory for the full sequence).

use adw::prelude::*;
use relm4::prelude::*;

/// Real panel resolution of the Xeneon Edge bar screen at 100% display
/// scale (logical px == physical px - see CLAUDE.md in the Python app).
/// Windowed dev builds open at exactly this size so what's on screen here
/// matches the real hardware; forcing fullscreen on the actual monitor
/// (`fullscreen_on_monitor` in the Python app's display.py) isn't ported
/// yet - this phase only has the in-window F11 toggle.
const WINDOW_WIDTH: i32 = 2560;
const WINDOW_HEIGHT: i32 = 720;

struct AppModel {
    fullscreened: bool,
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
                    // Placeholder page - WidgetGrid (a gtk::Fixed driven by
                    // the drag/snap math already ported to xeneon-core)
                    // replaces this in the next step.
                    gtk::Fixed {
                        set_width_request: WINDOW_WIDTH,
                        set_height_request: WINDOW_HEIGHT,

                        put[0.0, 0.0] = &gtk::Label {
                            set_label: "Page 1 (placeholder - WidgetGrid comes next)",
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                        },
                    },
                },
            },
        }
    }

    fn init(_init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = AppModel { fullscreened: false };
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
