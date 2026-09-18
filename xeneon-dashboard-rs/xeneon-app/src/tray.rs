//! System tray icon (StatusNotifierItem, via the `ksni` crate) - a
//! right-click menu mirroring some of the app's own shortcuts: open the
//! widget picker, jump to Settings, show help (see help_overlay.rs - a
//! keyboard-shortcuts overlay), relaunch, quit. Left-click is left at ksni's
//! default no-op `Tray::activate` - this version of ksni doesn't expose
//! the SNI `ItemIsMenu` property that would let us ask hosts to treat
//! left-click the same as right-click, so whether left-click also opens
//! the menu is entirely up to the host/desktop environment's own default.
//!
//! Spawned once from `AppModel::init()`; the returned `Handle` is kept
//! alive on the model (dropping it tears the tray down - see `_tray` in
//! `AppModel`).
//!
//! `ksni`'s "blocking" feature runs the D-Bus service on its own
//! background OS thread (spawned internally, with its own tiny tokio
//! runtime) - entirely separate from GTK's main loop thread. That has two
//! consequences baked into this module:
//! - `menu()` below must never touch GTK/glib state (not thread-safe) or
//!   call into `i18n_runtime` (its `STATE` is a `thread_local!`, so this
//!   thread would only ever see an empty, uninitialized catalog) - it only
//!   reads labels already resolved on the main thread and cached here.
//! - every menu action reports back via `relm4::Sender::emit` (a plain
//!   channel, safe from any thread) rather than acting directly - e.g.
//!   `relm4::main_application().quit()` is a GObject call and must stay on
//!   the thread that owns the main context, so it happens in
//!   `AppModel::update()` instead, triggered by the message.

use relm4::Sender;

use crate::AppMsg;

/// One label per menu entry, refreshed via [`retranslate`](Self::retranslate)
/// whenever the UI language changes - mirrors every other live-retranslated
/// widget in this app (see `i18n_runtime::on_change`). `menu()` only ever
/// reads these already-resolved strings, never `i18n_runtime::t()` itself
/// (see the module doc comment on why).
pub(crate) struct XeneonTray {
    sender: Sender<AppMsg>,
    icon: ksni::Icon,
    label_add_widget: String,
    label_settings: String,
    label_help: String,
    label_relaunch: String,
    label_quit: String,
}

impl XeneonTray {
    fn new(sender: Sender<AppMsg>, icon: ksni::Icon) -> Self {
        let mut tray = Self {
            sender,
            icon,
            label_add_widget: String::new(),
            label_settings: String::new(),
            label_help: String::new(),
            label_relaunch: String::new(),
            label_quit: String::new(),
        };
        tray.retranslate();
        tray
    }

    /// Re-fetches every label from the active `i18n_runtime` catalog.
    /// Main-thread only (see the module doc comment) - called once here at
    /// construction time and again by `AppModel`'s `i18n_runtime::on_change`
    /// listener through `Handle::update`, which marshals the closure over
    /// to the tray's own background thread.
    pub(crate) fn retranslate(&mut self) {
        self.label_add_widget = crate::i18n_runtime::t("tray.add_widget");
        self.label_settings = crate::i18n_runtime::t("tray.settings");
        self.label_help = crate::i18n_runtime::t("tray.help");
        self.label_relaunch = crate::i18n_runtime::t("tray.relaunch");
        self.label_quit = crate::i18n_runtime::t("tray.quit");
    }
}

impl ksni::Tray for XeneonTray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }

    fn title(&self) -> String {
        "Xeneon Dashboard".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: self.label_add_widget.clone(),
                activate: Box::new(|this: &mut Self| this.sender.emit(AppMsg::ShowWidgetPicker)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.label_settings.clone(),
                activate: Box::new(|this: &mut Self| this.sender.emit(AppMsg::GotoSettings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.label_help.clone(),
                activate: Box::new(|this: &mut Self| this.sender.emit(AppMsg::ShowHelp)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.label_relaunch.clone(),
                activate: Box::new(|this: &mut Self| this.sender.emit(AppMsg::Relaunch)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.label_quit.clone(),
                activate: Box::new(|this: &mut Self| this.sender.emit(AppMsg::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Rasterizes the app's existing symbolic SVG (already used for the window
/// icon, see `register_app_icon` in main.rs) into the raw ARGB32 pixmap
/// `ksni` wants, rather than pointing at it by icon-theme name: there's no
/// packaging/install step yet (see CLAUDE.md's Flatpak notes), so a host
/// looking the app id up in its own icon theme would find nothing outside
/// a source checkout. Rendered through gdk-pixbuf - already a transitive
/// dependency via gtk4 (which re-exports it as `gtk::gdk_pixbuf`) - rather
/// than adding a dedicated image/SVG crate just for this.
fn load_icon() -> ksni::Icon {
    // Rendered above the SVG's native 24x24 for a less blurry result on
    // panels that scale tray icons up (most do, to at least their own
    // panel-icon size).
    const SIZE: i32 = 48;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/icons/hicolor/symbolic/apps/com.n3tlab.XeneonDashboardRust-symbolic.svg"
    );
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(path, SIZE, SIZE)
        .expect("bundled tray icon SVG should always load");

    let width = pixbuf.width();
    let height = pixbuf.height();
    let channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    let has_alpha = pixbuf.has_alpha();
    let bytes = pixbuf.read_pixel_bytes();
    let src: &[u8] = bytes.as_ref();

    // gdk-pixbuf rows can be padded to `rowstride` (>= width * channels) -
    // walk row by row rather than treating the buffer as one flat RGBA
    // array. ksni wants ARGB32 in network (big-endian) byte order, i.e.
    // each pixel as [A, R, G, B] - the same rotate-right-by-one transform
    // ksni's own doc example applies to the `image` crate's RGBA output.
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = &src[y * rowstride..];
        for x in 0..width as usize {
            let pixel = &row[x * channels..];
            let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
            let a = if has_alpha { pixel[3] } else { 255 };
            data.extend_from_slice(&[a, r, g, b]);
        }
    }

    ksni::Icon { width, height, data }
}

/// Spawns the tray's D-Bus service on its own background thread (see the
/// module doc comment) and returns the handle to keep alive on `AppModel`.
/// `sender` is what every menu action reports back through, since `menu()`
/// itself runs off the GTK main thread.
///
/// Returns `None` (after logging) if no StatusNotifierWatcher is running -
/// e.g. a desktop with no tray support at all - so the app stays fully
/// usable without one; this is a convenience, not a requirement.
pub(crate) fn spawn(sender: Sender<AppMsg>) -> Option<ksni::blocking::Handle<XeneonTray>> {
    use ksni::blocking::TrayMethods;
    let tray = XeneonTray::new(sender, load_icon());
    match tray.spawn() {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("xeneon-dashboard: system tray unavailable, skipping: {err}");
            None
        }
    }
}
