//! Raccourcis: an icon-launcher grid (up to 5x5) for installed apps - a
//! straight-line port, step 1 of 4, of `widgets/shortcuts.py`. See that
//! file's own module docstring for the full design rationale (dedicated
//! move/delete buttons instead of a draggable tile, no trajectory
//! tracking, a per-instance backdrop panel...); this first step only
//! covers what's needed to place and launch an installed app:
//!
//! - a fixed 5x5 grid of cells sized to fill the widget's SIZE_L footprint
//!   (see `compute_cell_size`);
//! - a hover-revealed "+" button in the top-left corner - the same corner
//!   `DashboardWidget` would otherwise put this widget's title in (see
//!   `registry::WidgetDescriptor::card_title_key`) - opening a small
//!   dialog to pick an installed app;
//! - launching an icon on a plain click;
//! - persistence of the icon list (`content: {"icons": [...]}`).
//!
//! Deliberately NOT here yet (later steps, tracked in the
//! project_xeneon_rust_port memory): per-icon move/delete corner buttons,
//! the backdrop panel and its own settings (color/opacity), and custom
//! command/URL shortcuts with their icon picker/edit dialog. `ShortcutIcon`
//! already carries the full field shape those need (`kind`/`command`/
//! `icon_name`/`icon_path` alongside `app_id`), so adding them later is a
//! pure addition to this file, not a persisted-schema migration.
//!
//! One structural wrinkle no other widget ported so far has hit: adding an
//! icon happens straight on the canvas, with no settings popover involved
//! at all - but `WidgetGrid` only ever saves a widget when its popover
//! closes or a whole-widget drag ends. `WidgetInstance::on_change_ready`
//! (see registry.rs) is the fix added alongside this widget: `spawn`/
//! `restore` below stash the "save me now" closure `WidgetGrid` hands
//! them, and `place_icon` calls it directly after adding one - mirrors
//! `ShortcutsContent.set_change_notifier()`/`_notify()` in the Python
//! original, where the same widget-picker glue wires the plugin's own
//! change callback to the app's save-this-widget function.
//!
//! Launching goes straight through `gio::AppInfo::launch()` - no
//! flatpak-spawn/host-access dance like the Python original's sandboxed
//! build needs. The Rust port runs as a plain `cargo run` binary during
//! this whole phase, not yet packaged as a Flatpak, so there's no sandbox
//! to escape from; revisit if/when Flatpak packaging for this port is
//! scheduled (see project_xeneon_rust_port memory).
//!
//! No `gio::DesktopAppInfo` here, unlike the Python original's
//! `Gio.DesktopAppInfo.new(app_id)` - the `gio` crate this project depends
//! on doesn't bind GDesktopAppInfo at all (that lives in a separate
//! `gio-unix-2.0` C library this project doesn't link against). The
//! cross-platform `gio::AppInfo::all()` list is enough: everything a pick
//! or a launch needs (id/display name/icon/launch) is on the plain
//! `AppInfoExt` trait, so a saved `app_id` is resolved by scanning that
//! list for a matching `.id()` rather than constructing one directly.

use adw::prelude::*;
use gtk::gio;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::i18n_runtime as i18n;
use crate::widgets::registry::WidgetInstance;
use xeneon_core::grid::{Size, GAP, SIZE_L};

const GRID_COLS: i32 = 5;
const GRID_ROWS: i32 = 5;
const ICON_PIXEL_SIZE: i32 = 64;
// Bigger than a tile's own future corner buttons (step 2) - it's the main
// way to add anything to an otherwise-empty grid, so it needs to read
// clearly even though it's only a small hover overlay near the corner, not
// sized to fill reserved layout space.
const ADD_BUTTON_PIXEL_SIZE: i32 = 20;
const DEFAULT_APP_ICON: &str = "application-x-executable-symbolic";

/// One placed icon. Only `kind == "app"` is reachable yet (step 1's add
/// dialog only offers installed apps) - `kind` stays a plain string
/// (matching how `WidgetDescriptor::kind` and every other "which variant"
/// tag in this codebase are represented) rather than an enum, both for
/// that consistency and because `from_dict` already has to tolerate an
/// unrecognized value forward-compatibly, same as the Python original's
/// dict-based schema.
struct ShortcutIcon {
    col: i32,
    row: i32,
    kind: String,
    app_id: Option<String>,
    command: Option<String>,
    label: String,
    icon_name: Option<String>,
    icon_path: Option<String>,
}

impl ShortcutIcon {
    fn new_app(col: i32, row: i32, app_id: String) -> Self {
        Self { col, row, kind: "app".to_string(), app_id: Some(app_id), command: None, label: String::new(), icon_name: None, icon_path: None }
    }

    /// Looks the saved `app_id` back up in the live installed-apps list -
    /// see this module's own doc comment for why there's no
    /// `DesktopAppInfo::new(id)` shortcut available here. `None` once an
    /// app has been uninstalled since the shortcut was saved.
    fn app_info(&self) -> Option<gio::AppInfo> {
        let app_id = self.app_id.as_deref()?;
        gio::AppInfo::all().into_iter().find(|a| a.id().as_deref() == Some(app_id))
    }

    fn display_name(&self) -> String {
        if !self.label.is_empty() {
            return self.label.clone();
        }
        if let Some(info) = self.app_info() {
            return info.display_name().to_string();
        }
        self.app_id.clone().or_else(|| self.command.clone()).unwrap_or_default()
    }

    fn gicon(&self) -> Option<gio::Icon> {
        if let Some(path) = &self.icon_path {
            return Some(gio::FileIcon::new(&gio::File::for_path(path)).upcast());
        }
        if let Some(info) = self.app_info() {
            if let Some(icon) = info.icon() {
                return Some(icon);
            }
        }
        self.icon_name.as_ref().map(|name| gio::ThemedIcon::new(name).upcast())
    }

    fn launch(&self) {
        if self.kind == "app" {
            if let Some(info) = self.app_info() {
                let _ = info.launch(&[], None::<&gio::AppLaunchContext>);
            }
        }
        // Custom (command/URL) shortcuts aren't creatable yet (step 4) -
        // nothing else to launch until then.
    }

    fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "col": self.col,
            "row": self.row,
            "kind": self.kind,
            "app_id": self.app_id,
            "command": self.command,
            "label": self.label,
            "icon_name": self.icon_name,
            "icon_path": self.icon_path,
        })
    }

    fn from_dict(data: &serde_json::Value) -> Option<Self> {
        let col = data.get("col")?.as_i64()? as i32;
        let row = data.get("row")?.as_i64()? as i32;
        let kind = data.get("kind")?.as_str()?.to_string();
        if kind != "app" && kind != "custom" {
            return None;
        }
        Some(Self {
            col,
            row,
            kind,
            app_id: data.get("app_id").and_then(|v| v.as_str()).map(str::to_string),
            command: data.get("command").and_then(|v| v.as_str()).map(str::to_string),
            label: data.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            icon_name: data.get("icon_name").and_then(|v| v.as_str()).map(str::to_string),
            icon_path: data.get("icon_path").and_then(|v| v.as_str()).map(str::to_string),
        })
    }
}

static INSTALL_CSS: std::sync::Once = std::sync::Once::new();

/// A dedicated small provider (like `grid_widget.rs`'s own move-preview
/// CSS), not the shared per-widget-appearance one in `appearance_css.rs` -
/// this is static chrome styling, not a customizable appearance property.
fn ensure_css_installed() {
    INSTALL_CSS.call_once(|| {
        let Some(display) = gtk::gdk::Display::default() else { return };
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".xeneon-shortcut-tile { border-radius: 10px; padding: 4px; }
             .xeneon-shortcut-tile:hover { background-color: rgba(255, 255, 255, 0.12); }
             button.xeneon-shortcuts-add { min-width: 36px; min-height: 36px; padding: 4px; margin: 2px; }",
        );
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    });
}

/// Cell size for a `size`-footprint grid - `GRID_COLS`x`GRID_ROWS` cells
/// with a `GAP` between every one of them, including the outer margin
/// against the widget's own edges. Mirrors `_compute_cell_size` in
/// shortcuts.py.
fn compute_cell_size(size: Size) -> (f64, f64) {
    let avail_w = size.w as f64 - 2.0 * GAP as f64 - (GRID_COLS - 1) as f64 * GAP as f64;
    let avail_h = size.h as f64 - 2.0 * GAP as f64 - (GRID_ROWS - 1) as f64 * GAP as f64;
    (avail_w / GRID_COLS as f64, avail_h / GRID_ROWS as f64)
}

struct ShortcutsState {
    fixed: gtk::Fixed,
    cell_w: f64,
    cell_h: f64,
    icons: RefCell<HashMap<(i32, i32), ShortcutIcon>>,
    empty_hint: gtk::Label,
    add_button: gtk::Button,
    /// Set once, right after `WidgetGrid` places this widget - see the
    /// module doc comment on `WidgetInstance::on_change_ready`.
    change_notifier: RefCell<Option<Rc<dyn Fn()>>>,
}

impl ShortcutsState {
    fn cell_x(&self, col: i32) -> f64 {
        GAP as f64 + col as f64 * (self.cell_w + GAP as f64)
    }

    fn cell_y(&self, row: i32) -> f64 {
        GAP as f64 + row as f64 * (self.cell_h + GAP as f64)
    }

    fn tile_position(&self, col: i32, row: i32) -> (f64, f64) {
        (self.cell_x(col), self.cell_y(row))
    }

    /// Top-left, filling left to right then wrapping to the next row - new
    /// icons always land here, like a launcher/dock auto-arranging its
    /// icons rather than free placement. Mirrors `_first_free_cell`.
    fn first_free_cell(&self) -> Option<(i32, i32)> {
        let icons = self.icons.borrow();
        for row in 0..GRID_ROWS {
            for col in 0..GRID_COLS {
                if !icons.contains_key(&(col, row)) {
                    return Some((col, row));
                }
            }
        }
        None
    }

    fn update_add_button_state(&self) {
        self.add_button.set_sensitive(self.first_free_cell().is_some());
    }

    fn notify_change(&self) {
        if let Some(cb) = self.change_notifier.borrow().as_ref() {
            cb();
        }
    }

    /// Builds `icon`'s tile, places it on the canvas and records it -
    /// returns the tile's root widget (an `Overlay` already, even though
    /// step 1 gives it no overlay children yet, so step 2 can add the
    /// move/delete corner buttons without restructuring this) so the
    /// caller can wire its click gesture - see `place_icon` below for why
    /// that step needs its own `Rc<ShortcutsState>`, which only the caller
    /// has. `None` if the cell is already occupied.
    fn register_icon(&self, icon: ShortcutIcon) -> Option<gtk::Overlay> {
        let key = (icon.col, icon.row);
        if self.icons.borrow().contains_key(&key) {
            return None;
        }

        let root = gtk::Overlay::new();
        root.add_css_class("xeneon-shortcut-tile");
        root.set_cursor_from_name(Some("pointer"));
        root.set_size_request(self.cell_w.round() as i32, self.cell_h.round() as i32);

        let image = gtk::Image::new();
        image.set_pixel_size(ICON_PIXEL_SIZE);
        image.set_halign(gtk::Align::Center);
        image.set_valign(gtk::Align::Center);
        image.set_hexpand(true);
        image.set_vexpand(true);
        match icon.gicon() {
            Some(gicon) => image.set_from_gicon(&gicon),
            None => image.set_icon_name(Some(DEFAULT_APP_ICON)),
        }
        root.set_child(Some(&image));
        root.set_tooltip_text(Some(&icon.display_name()));

        let (x, y) = self.tile_position(icon.col, icon.row);
        self.fixed.put(&root, x, y);
        self.icons.borrow_mut().insert(key, icon);
        self.empty_hint.set_visible(false);
        self.update_add_button_state();
        Some(root)
    }

    fn to_dict(&self) -> serde_json::Value {
        let icons: Vec<serde_json::Value> = self.icons.borrow().values().map(ShortcutIcon::to_dict).collect();
        serde_json::json!({ "icons": icons })
    }
}

/// Places `icon` on `state`'s grid and wires its tile's plain-click launch
/// gesture. A free function rather than a `ShortcutsState` method: the
/// gesture's callback needs its own `Rc<ShortcutsState>` clone to look the
/// icon back up by cell at click time (GTK signal closures are `'static`,
/// so they can't just borrow `state`), and only a caller already holding
/// that `Rc` can hand it one - `register_icon` alone (a plain `&self`
/// method) has no way to manufacture it.
fn place_icon(state: &Rc<ShortcutsState>, icon: ShortcutIcon) {
    let key = (icon.col, icon.row);
    let Some(root) = state.register_icon(icon) else { return };

    // Simple GestureClick, no drag distance to watch - moving an icon is a
    // dedicated move button's job (step 2), not this tile's own drag, so a
    // plain click is unambiguous. GTK only fires "released" for a press
    // that both starts and ends inside the tile.
    let launch = gtk::GestureClick::new();
    launch.connect_released({
        let state = state.clone();
        move |_, _, _, _| {
            if let Some(icon) = state.icons.borrow().get(&key) {
                icon.launch();
            }
        }
    });
    root.add_controller(launch);
}

fn apply_dict(state: &Rc<ShortcutsState>, data: &serde_json::Value) {
    let Some(icons) = data.get("icons").and_then(|v| v.as_array()) else { return };
    for entry in icons {
        let Some(icon) = ShortcutIcon::from_dict(entry) else { continue };
        if !(0..GRID_COLS).contains(&icon.col) || !(0..GRID_ROWS).contains(&icon.row) {
            continue;
        }
        place_icon(state, icon);
    }
}

/// Opened from the grid's "+" button: pick an installed app, or search for
/// one - a trimmed step-1 version of `ShortcutPickerDialog` in
/// shortcuts.py (no custom-shortcut form yet, see the module doc comment).
/// `on_pick` is called once with the chosen app's id, then the dialog
/// closes itself.
fn open_add_dialog(parent: &gtk::Window, on_pick: impl Fn(String) + 'static) {
    let dialog = adw::Window::new();
    dialog.set_transient_for(Some(parent));
    dialog.set_modal(true);
    dialog.set_default_size(420, 480);
    dialog.set_title(Some(&i18n::t("widgets.shortcuts.picker.title")));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&adw::HeaderBar::new());

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root_box.set_margin_start(12);
    root_box.set_margin_end(12);
    root_box.set_margin_top(6);
    root_box.set_margin_bottom(12);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(&i18n::t("widgets.shortcuts.picker.search_placeholder")));
    root_box.append(&search_entry);

    let apps_group_label = gtk::Label::new(Some(&i18n::t("widgets.shortcuts.picker.apps_group")));
    apps_group_label.set_halign(gtk::Align::Start);
    apps_group_label.add_css_class("heading");
    root_box.append(&apps_group_label);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_min_content_height(280);
    let apps_list = gtk::ListBox::new();
    apps_list.add_css_class("boxed-list");
    apps_list.set_selection_mode(gtk::SelectionMode::None);
    apps_list.set_activate_on_single_click(true);
    scroller.set_child(Some(&apps_list));
    root_box.append(&scroller);

    toolbar_view.set_content(Some(&root_box));
    dialog.set_content(Some(&toolbar_view));

    let mut all_apps: Vec<gio::AppInfo> = gio::AppInfo::all().into_iter().filter(|a| a.should_show()).collect();
    all_apps.sort_by_key(|a| a.display_name().to_string().to_lowercase());
    let all_apps = Rc::new(all_apps);
    // Rebuilt by `populate` on every search keystroke, in the same order as
    // the rows currently shown - `row.index()` (row-activated) looks back
    // into this to find which app was picked, since a plain `ListBoxRow`
    // has nowhere of its own to stash extra data (Python attaches
    // `row.app_info` directly to the row instead; not an option here).
    let visible_apps: Rc<RefCell<Vec<gio::AppInfo>>> = Rc::new(RefCell::new(Vec::new()));

    let populate = {
        let apps_list = apps_list.clone();
        let all_apps = all_apps.clone();
        let visible_apps = visible_apps.clone();
        move |query: &str| {
            while let Some(child) = apps_list.first_child() {
                apps_list.remove(&child);
            }
            let query = query.trim().to_lowercase();
            let mut visible = Vec::new();
            for app in all_apps.iter() {
                let name = app.display_name();
                if !query.is_empty() && !name.to_lowercase().contains(&query) {
                    continue;
                }
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(&name));
                row.set_activatable(true);
                if let Some(icon) = app.icon() {
                    let image = gtk::Image::from_gicon(&icon);
                    image.set_pixel_size(28);
                    row.add_prefix(&image);
                }
                apps_list.append(&row);
                visible.push(app.clone());
            }
            *visible_apps.borrow_mut() = visible;
        }
    };
    populate("");

    search_entry.connect_search_changed({
        let populate = populate.clone();
        move |entry| populate(&entry.text())
    });

    apps_list.connect_row_activated({
        let visible_apps = visible_apps.clone();
        let dialog = dialog.clone();
        move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            if let Some(app) = visible_apps.borrow().get(index as usize) {
                if let Some(id) = app.id() {
                    on_pick(id.to_string());
                }
            }
            dialog.close();
        }
    });

    dialog.present();
}

fn build_content(size: Size) -> (Rc<ShortcutsState>, gtk::Widget) {
    ensure_css_installed();
    let (cell_w, cell_h) = compute_cell_size(size);

    let fixed = gtk::Fixed::new();
    fixed.set_size_request(size.w, size.h);

    let empty_hint = gtk::Label::new(None);
    empty_hint.add_css_class("dim-label");
    empty_hint.set_wrap(true);
    empty_hint.set_justify(gtk::Justification::Center);
    let (hint_w, hint_h) = (320, 40);
    empty_hint.set_size_request(hint_w, hint_h);
    fixed.put(&empty_hint, ((size.w - hint_w) / 2) as f64, ((size.h - hint_h) / 2) as f64);

    // Top-left corner, in the generic per-widget title's usual spot (see
    // `WidgetDescriptor::card_title_key`) - hover-revealed, floating over
    // row 0's own corner rather than reserving layout space for it, since
    // it's only ever visible transiently. The empty-state hint text is
    // what carries the "click + to add an app" discoverability instead.
    let add_button = gtk::Button::new();
    add_button.add_css_class("flat");
    add_button.add_css_class("circular");
    add_button.add_css_class("xeneon-shortcuts-add");
    let add_icon = gtk::Image::from_icon_name("list-add-symbolic");
    add_icon.set_pixel_size(ADD_BUTTON_PIXEL_SIZE);
    add_button.set_child(Some(&add_icon));
    add_button.set_tooltip_text(Some(&i18n::t("widgets.shortcuts.context.add")));
    add_button.set_visible(false);
    fixed.put(&add_button, (GAP / 2) as f64, (GAP / 2) as f64);

    let state = Rc::new(ShortcutsState {
        fixed: fixed.clone(),
        cell_w,
        cell_h,
        icons: RefCell::new(HashMap::new()),
        empty_hint: empty_hint.clone(),
        add_button: add_button.clone(),
        change_notifier: RefCell::new(None),
    });
    state.update_add_button_state();

    add_button.connect_clicked({
        let state = state.clone();
        move |button| {
            let Some((col, row)) = state.first_free_cell() else { return };
            let Some(parent) = button.root().and_downcast::<gtk::Window>() else { return };
            let state = state.clone();
            open_add_dialog(&parent, move |app_id| {
                place_icon(&state, ShortcutIcon::new_app(col, row, app_id));
                state.notify_change();
            });
        }
    });

    let hover = gtk::EventControllerMotion::new();
    hover.connect_enter({
        let add_button = add_button.clone();
        move |_, _, _| add_button.set_visible(true)
    });
    hover.connect_leave({
        let add_button = add_button.clone();
        move |_| add_button.set_visible(false)
    });
    fixed.add_controller(hover);

    i18n::on_change({
        let empty_hint = empty_hint.clone();
        let add_button = add_button.clone();
        move || {
            empty_hint.set_label(&i18n::t("widgets.shortcuts.empty_hint"));
            add_button.set_tooltip_text(Some(&i18n::t("widgets.shortcuts.context.add")));
        }
    });
    empty_hint.set_label(&i18n::t("widgets.shortcuts.empty_hint"));

    (state, fixed.upcast())
}

fn wire_change_notifier(state: &Rc<ShortcutsState>) -> Box<dyn FnOnce(Rc<dyn Fn()>)> {
    let state = state.clone();
    Box::new(move |save_now| {
        *state.change_notifier.borrow_mut() = Some(save_now);
    })
}

pub fn spawn() -> WidgetInstance {
    let (state, content) = build_content(SIZE_L);
    let to_dict_state = state.clone();
    WidgetInstance {
        content,
        settings: None,
        to_dict: Box::new(move || to_dict_state.to_dict()),
        on_reset: None,
        on_change_ready: Some(wire_change_notifier(&state)),
    }
}

pub fn restore(data: &serde_json::Value) -> WidgetInstance {
    let (state, content) = build_content(SIZE_L);
    apply_dict(&state, data);
    let to_dict_state = state.clone();
    WidgetInstance {
        content,
        settings: None,
        to_dict: Box::new(move || to_dict_state.to_dict()),
        on_reset: None,
        on_change_ready: Some(wire_change_notifier(&state)),
    }
}
