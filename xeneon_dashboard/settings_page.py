import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

from xeneon_dashboard import gnome_extensions, i18n, theme
from xeneon_dashboard.widget_appearance import IMAGE_MIME_TYPES, hex_to_rgba, rgba_to_hex


class SettingsPage(Gtk.Box):
    """The always-last carousel page holding app settings."""

    def __init__(self, window, **kwargs):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, **kwargs)
        self._window = window
        self._app = window.get_application()
        self._page_rows: list[Adw.ExpanderRow] = []
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.set_margin_top(24)
        self.set_margin_bottom(24)
        self.set_margin_start(24)
        self.set_margin_end(24)

        self._title = Gtk.Label(label=i18n._("settings.title"))
        self._title.add_css_class("title-1")
        self._title.set_halign(Gtk.Align.START)
        self._title.set_margin_bottom(12)
        self.append(self._title)

        columns_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=24, homogeneous=True)
        columns_box.set_hexpand(True)

        interface_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24, valign=Gtk.Align.START)
        appearance_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24, valign=Gtk.Align.START)
        shortcuts_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24, valign=Gtk.Align.START)
        info_column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24, valign=Gtk.Align.START)

        self._shortcuts_group = Adw.PreferencesGroup(title=i18n._("settings.shortcuts_group"))

        self._fullscreen_row = Adw.ActionRow(title=i18n._("settings.fullscreen_row.title"))
        self._fullscreen_row.set_subtitle(i18n._("settings.fullscreen_row.subtitle"))
        self._fullscreen_shortcut_label = Gtk.Label()
        self._fullscreen_shortcut_label.add_css_class("dim-label")
        self._fullscreen_shortcut_label.set_valign(Gtk.Align.CENTER)
        self._fullscreen_row.add_suffix(self._fullscreen_shortcut_label)
        self._reconfigure_button = Gtk.Button(label=i18n._("settings.reconfigure_button"))
        self._reconfigure_button.set_valign(Gtk.Align.CENTER)
        self._reconfigure_button.connect("clicked", self._on_reconfigure_clicked)
        self._fullscreen_row.add_suffix(self._reconfigure_button)
        self._shortcuts_group.add(self._fullscreen_row)

        self._goto_row = Adw.ActionRow(title=i18n._("settings.goto_row.title"))
        goto_shortcut_label = Gtk.ShortcutLabel(accelerator="<Primary>comma")
        goto_shortcut_label.set_valign(Gtk.Align.CENTER)
        self._goto_row.add_suffix(goto_shortcut_label)
        self._shortcuts_group.add(self._goto_row)

        shortcuts_column.append(self._shortcuts_group)

        self._shortcuts_widget_group = Adw.PreferencesGroup(title=i18n._("settings.shortcuts_widget_group"))
        self._host_access_row = Adw.ActionRow(title=i18n._("settings.host_access_row.title"))
        self._host_access_row.set_subtitle(i18n._("settings.host_access_row.subtitle"))
        self._host_access_switch = Gtk.Switch()
        self._host_access_switch.set_active(self._app.config.get("shortcuts_host_access"))
        self._host_access_switch.set_valign(Gtk.Align.CENTER)
        self._host_access_switch.connect("notify::active", self._on_host_access_toggled)
        self._host_access_row.add_suffix(self._host_access_switch)
        self._shortcuts_widget_group.add(self._host_access_row)
        shortcuts_column.append(self._shortcuts_widget_group)

        self._interface_group = Adw.PreferencesGroup(title=i18n._("settings.interface_group"))

        self._hide_delay_row = Adw.SpinRow.new_with_range(1, 30, 1)
        self._hide_delay_row.set_title(i18n._("settings.hide_delay_row.title"))
        self._hide_delay_row.set_subtitle(i18n._("settings.hide_delay_row.subtitle"))
        self._hide_delay_row.set_value(self._app.config.get("indicator_hide_delay_seconds"))
        self._hide_delay_row.connect("notify::value", self._on_hide_delay_changed)
        self._interface_group.add(self._hide_delay_row)

        self._indicator_opacity_row = Adw.SpinRow.new_with_range(10, 100, 5)
        self._indicator_opacity_row.set_title(i18n._("settings.indicator_opacity_row.title"))
        self._indicator_opacity_row.set_subtitle(i18n._("settings.indicator_opacity_row.subtitle"))
        self._indicator_opacity_row.set_value(self._app.config.get("indicator_opacity"))
        self._indicator_opacity_row.connect("notify::value", self._on_indicator_opacity_changed)
        self._interface_group.add(self._indicator_opacity_row)

        self._indicator_color_row = Adw.ActionRow(title=i18n._("settings.indicator_color_row.title"))
        self._indicator_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        saved_color = self._app.config.get("indicator_button_color")
        initial_color = Gdk.RGBA()
        initial_color.parse(saved_color or "#ffffff")
        self._indicator_color_button.set_rgba(initial_color)
        self._indicator_color_button.set_valign(Gtk.Align.CENTER)
        self._indicator_color_button.connect("notify::rgba", self._on_indicator_color_changed)
        self._indicator_color_row.add_suffix(self._indicator_color_button)
        self._indicator_color_reset_button = Gtk.Button(icon_name="edit-undo-symbolic")
        self._indicator_color_reset_button.add_css_class("flat")
        self._indicator_color_reset_button.set_valign(Gtk.Align.CENTER)
        self._indicator_color_reset_button.set_tooltip_text(i18n._("settings.indicator_color_row.reset_tooltip"))
        self._indicator_color_reset_button.connect("clicked", self._on_indicator_color_reset)
        self._indicator_color_row.add_suffix(self._indicator_color_reset_button)
        self._interface_group.add(self._indicator_color_row)

        interface_column.append(self._interface_group)

        self._language_group = Adw.PreferencesGroup(title=i18n._("settings.language_group"))
        self._language_codes = list(i18n.available_languages().keys())
        language_names = list(i18n.available_languages().values())
        self._language_row = Adw.ComboRow(
            title=i18n._("settings.language_row.title"),
            model=Gtk.StringList.new(language_names),
        )
        current_lang = i18n.get_language()
        if current_lang in self._language_codes:
            self._language_row.set_selected(self._language_codes.index(current_lang))
        self._language_row.connect("notify::selected", self._on_language_changed)
        self._language_group.add(self._language_row)

        interface_column.append(self._language_group)

        self._theme_group = Adw.PreferencesGroup(title=i18n._("settings.theme_group.title"))

        self._accent_row = Adw.ActionRow(title=i18n._("settings.theme_group.accent_row.title"))
        self._accent_buttons: list[tuple[Gtk.Button, str]] = []
        accent_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        accent_box.set_valign(Gtk.Align.CENTER)
        for key, hex_value in theme.ACCENT_PRESETS:
            button = Gtk.Button()
            button.add_css_class("flat")
            button.add_css_class(theme.SWATCH_CSS_CLASS)
            button.add_css_class(f"{theme.SWATCH_CSS_CLASS}-{key}")
            button.set_tooltip_text(i18n._(f"settings.theme_group.accent.{key}"))
            button.connect("clicked", self._on_accent_preset_clicked, key, hex_value)
            accent_box.append(button)
            self._accent_buttons.append((button, key, hex_value))
        self._accent_row.add_suffix(accent_box)
        self._theme_group.add(self._accent_row)

        self._accent_follow_row = Adw.ActionRow(
            title=i18n._("settings.theme_group.follow_system_row.title"),
            subtitle=i18n._("settings.theme_group.follow_system_row.subtitle"),
        )
        self._accent_follow_switch = Gtk.Switch()
        self._accent_follow_switch.set_active(self._app.config.get("accent_follow_system"))
        self._accent_follow_switch.set_valign(Gtk.Align.CENTER)
        self._accent_follow_switch.set_sensitive(theme.system_accent_supported())
        self._accent_follow_switch.connect("notify::active", self._on_accent_follow_toggled)
        self._accent_follow_row.add_suffix(self._accent_follow_switch)
        self._theme_group.add(self._accent_follow_row)

        interface_column.append(self._theme_group)
        self._sync_accent_controls()
        theme.on_change(self._sync_accent_controls)

        self._appearance_group = Adw.PreferencesGroup(
            title=i18n._("settings.appearance_group.title"),
            description=i18n._("settings.appearance_group.subtitle"),
        )
        defaults = self._app.config.get("default_widget_appearance")

        self._appearance_opacity_row = Adw.ActionRow(title=i18n._("widgets.appearance.opacity"))
        self._appearance_opacity_scale = Gtk.Scale(orientation=Gtk.Orientation.HORIZONTAL)
        self._appearance_opacity_scale.set_range(0, 100)
        self._appearance_opacity_scale.set_value(round(defaults["opacity"] * 100))
        self._appearance_opacity_scale.set_draw_value(True)
        self._appearance_opacity_scale.set_value_pos(Gtk.PositionType.RIGHT)
        self._appearance_opacity_scale.set_size_request(140, -1)
        self._appearance_opacity_scale.set_hexpand(True)
        self._appearance_opacity_scale.set_valign(Gtk.Align.CENTER)
        self._appearance_opacity_scale.connect("value-changed", self._on_default_appearance_changed)
        self._appearance_opacity_row.add_suffix(self._appearance_opacity_scale)
        self._appearance_group.add(self._appearance_opacity_row)

        self._appearance_bg_color_row = Adw.ActionRow(title=i18n._("widgets.appearance.bg_color"))
        self._appearance_bg_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._appearance_bg_color_button.set_rgba(hex_to_rgba(defaults["bg_color"]))
        self._appearance_bg_color_button.set_valign(Gtk.Align.CENTER)
        self._appearance_bg_color_button.connect("notify::rgba", self._on_default_appearance_changed)
        self._appearance_bg_color_row.add_suffix(self._appearance_bg_color_button)
        self._appearance_group.add(self._appearance_bg_color_row)

        self._appearance_border_row = Adw.ActionRow(title=i18n._("widgets.appearance.border_enabled"))
        self._appearance_border_switch = Gtk.Switch()
        self._appearance_border_switch.set_active(defaults["border_enabled"])
        self._appearance_border_switch.set_valign(Gtk.Align.CENTER)
        self._appearance_border_switch.connect("notify::active", self._on_default_appearance_changed)
        self._appearance_border_row.add_suffix(self._appearance_border_switch)
        self._appearance_group.add(self._appearance_border_row)

        self._appearance_border_width_row = Adw.SpinRow.new_with_range(1, 12, 1)
        self._appearance_border_width_row.set_title(i18n._("widgets.appearance.border_width"))
        self._appearance_border_width_row.set_value(defaults["border_width"])
        self._appearance_border_width_row.connect("notify::value", self._on_default_appearance_changed)
        self._appearance_group.add(self._appearance_border_width_row)

        self._appearance_border_color_row = Adw.ActionRow(title=i18n._("widgets.appearance.border_color"))
        self._appearance_border_color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        self._appearance_border_color_button.set_rgba(hex_to_rgba(defaults["border_color"]))
        self._appearance_border_color_button.set_valign(Gtk.Align.CENTER)
        self._appearance_border_color_button.connect("notify::rgba", self._on_default_appearance_changed)
        self._appearance_border_color_row.add_suffix(self._appearance_border_color_button)
        self._appearance_group.add(self._appearance_border_color_row)

        self._appearance_apply_row = Adw.ActionRow(
            title=i18n._("settings.appearance_apply_row.title"),
            subtitle=i18n._("settings.appearance_apply_row.subtitle"),
        )
        self._appearance_apply_button = Gtk.Button(label=i18n._("settings.appearance_apply_row.button"))
        self._appearance_apply_button.add_css_class("xeneon-reset-button")
        self._appearance_apply_button.set_valign(Gtk.Align.CENTER)
        self._appearance_apply_button.connect("clicked", self._on_apply_default_appearance_clicked)
        self._appearance_apply_row.add_suffix(self._appearance_apply_button)
        self._appearance_group.add(self._appearance_apply_row)
        self._appearance_apply_flash_source: int | None = None

        appearance_column.append(self._appearance_group)

        self._pages_group = Adw.PreferencesGroup(title=i18n._("settings.pages_group.title"))
        interface_column.append(self._pages_group)

        self._extensions_group = Adw.PreferencesGroup(title=i18n._("settings.extensions_group"))
        self._deja_window_row = Adw.ActionRow()
        self._deja_window_icon = Gtk.Image()
        self._deja_window_icon.set_valign(Gtk.Align.CENTER)
        self._deja_window_row.add_prefix(self._deja_window_icon)
        self._deja_window_refresh_button = Gtk.Button()
        self._deja_window_refresh_button.set_icon_name("view-refresh-symbolic")
        self._deja_window_refresh_button.set_valign(Gtk.Align.CENTER)
        self._deja_window_refresh_button.connect("clicked", lambda _b: self._refresh_deja_window_status())
        self._deja_window_row.add_suffix(self._deja_window_refresh_button)
        self._extensions_group.add(self._deja_window_row)
        info_column.append(self._extensions_group)

        columns_box.append(interface_column)
        columns_box.append(appearance_column)
        columns_box.append(shortcuts_column)
        columns_box.append(info_column)

        self.append(columns_box)
        self.refresh_pages()

        self.refresh_fullscreen_shortcut_label()
        self._refresh_deja_window_status()
        i18n.on_change(self._retranslate)

    def _on_hide_delay_changed(self, row, _pspec):
        self._app.set_indicator_hide_delay(int(row.get_value()))

    def _on_indicator_opacity_changed(self, row, _pspec):
        self._app.set_indicator_opacity(int(row.get_value()))

    def _on_indicator_color_changed(self, button, _pspec):
        rgba = button.get_rgba()
        color_hex = "#{:02x}{:02x}{:02x}".format(
            round(rgba.red * 255), round(rgba.green * 255), round(rgba.blue * 255)
        )
        self._app.set_indicator_button_color(color_hex)

    def _on_indicator_color_reset(self, _button):
        self._app.set_indicator_button_color(None)
        reset_color = Gdk.RGBA()
        reset_color.parse("#ffffff")
        self._indicator_color_button.set_rgba(reset_color)

    def _on_language_changed(self, row, _pspec):
        index = row.get_selected()
        if 0 <= index < len(self._language_codes):
            self._app.set_language(self._language_codes[index])

    def _on_accent_preset_clicked(self, _button, _key: str, accent_hex: str):
        if self._accent_follow_switch.get_active():
            # Picking a preset explicitly overrides "follow system" - matches
            # how GNOME's own accent picker treats a manual pick.
            self._accent_follow_switch.set_active(False)
        self._app.set_accent_color(accent_hex)

    def _on_accent_follow_toggled(self, switch, _pspec):
        self._app.set_accent_follow_system(switch.get_active())

    def _sync_accent_controls(self):
        """Re-reads the swatch selection from theme.get_accent() - called
        once at startup and again on theme.on_change (e.g. the system
        accent changing while "follow system" is on, see app.py)."""
        current = theme.get_accent().lower()
        for button, _key, hex_value in self._accent_buttons:
            if hex_value.lower() == current:
                button.add_css_class(theme.SWATCH_SELECTED_CSS_CLASS)
            else:
                button.remove_css_class(theme.SWATCH_SELECTED_CSS_CLASS)

    def _on_host_access_toggled(self, switch, _pspec):
        enabled = switch.get_active()
        if enabled and not self._app.config.get("shortcuts_host_access"):
            self._confirm_host_access(switch)
        else:
            self._app.set_shortcuts_host_access(enabled)

    def _confirm_host_access(self, switch: Gtk.Switch):
        dialog = Adw.AlertDialog(
            heading=i18n._("settings.host_access_dialog.heading"),
            body=i18n._("settings.host_access_dialog.body"),
        )
        dialog.add_response("cancel", i18n._("settings.host_access_dialog.cancel"))
        dialog.add_response("confirm", i18n._("settings.host_access_dialog.confirm"))
        dialog.set_response_appearance("confirm", Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response("cancel")
        dialog.set_close_response("cancel")
        dialog.connect("response", self._on_host_access_dialog_response, switch)
        dialog.present(self._window)

    def _on_host_access_dialog_response(self, _dialog, response: str, switch: Gtk.Switch):
        if response == "confirm":
            self._app.set_shortcuts_host_access(True)
        else:
            switch.set_active(False)

    def _on_default_appearance_changed(self, *_args):
        self._app.set_default_widget_appearance(
            {
                "opacity": self._appearance_opacity_scale.get_value() / 100,
                "bg_color": rgba_to_hex(self._appearance_bg_color_button.get_rgba()),
                "border_enabled": self._appearance_border_switch.get_active(),
                "border_width": int(self._appearance_border_width_row.get_value()),
                "border_color": rgba_to_hex(self._appearance_border_color_button.get_rgba()),
            }
        )

    def _on_apply_default_appearance_clicked(self, _button):
        self._window.apply_default_widget_appearance_to_all()
        # The click itself gives no feedback (button stays red before and
        # after), so swap the label to a confirmation and disable it
        # briefly - the only visible sign the overwrite actually happened.
        self._appearance_apply_button.set_label(i18n._("settings.appearance_apply_row.button_applied"))
        self._appearance_apply_button.set_sensitive(False)
        if self._appearance_apply_flash_source is not None:
            GLib.source_remove(self._appearance_apply_flash_source)
        self._appearance_apply_flash_source = GLib.timeout_add_seconds(2, self._reset_apply_button)

    def _reset_apply_button(self):
        self._appearance_apply_button.set_label(i18n._("settings.appearance_apply_row.button"))
        self._appearance_apply_button.set_sensitive(True)
        self._appearance_apply_flash_source = None
        return GLib.SOURCE_REMOVE

    def refresh_pages(self):
        """Rebuilds one row per widget page currently on the window - called
        whenever a page is created (see XeneonWindow.add_widget_page) and
        also reused by _retranslate() below, since rebuilding is simplest
        way to keep a dynamic-length list translated."""
        for row in self._page_rows:
            self._pages_group.remove(row)
        self._page_rows.clear()
        for page in self._window.widget_pages():
            row = self._build_page_row(page)
            self._pages_group.add(row)
            self._page_rows.append(row)

    def _build_page_row(self, page) -> Adw.ExpanderRow:
        expander = Adw.ExpanderRow(
            title=page.display_name(),
            subtitle=i18n._("settings.pages_group.subtitle", n=page.page_index + 1),
        )

        name_row = Adw.EntryRow(title=i18n._("settings.pages_group.name_row.title"))
        name_row.set_text(page.custom_name or "")
        name_row.set_show_apply_button(True)

        restore_button = Gtk.Button(icon_name="edit-undo-symbolic")
        restore_button.add_css_class("flat")
        restore_button.set_valign(Gtk.Align.CENTER)
        restore_button.set_tooltip_text(i18n._("settings.pages_group.restore_name_tooltip"))
        name_row.add_suffix(restore_button)

        def on_apply(row):
            page.custom_name = row.get_text().strip() or None
            expander.set_title(page.display_name())
            self._window.save_page(page)

        def on_restore(_button):
            page.custom_name = None
            name_row.set_text("")
            expander.set_title(page.display_name())
            self._window.save_page(page)

        name_row.connect("apply", on_apply)
        restore_button.connect("clicked", on_restore)
        expander.add_row(name_row)

        bg_row = Adw.ActionRow(title=i18n._("settings.pages_group.background_row.title"))

        color_button = Gtk.ColorDialogButton.new(Gtk.ColorDialog.new())
        color_button.set_rgba(page.background.color)
        color_button.set_valign(Gtk.Align.CENTER)
        color_button.connect("notify::rgba", lambda b, _p: self._on_page_color_changed(page, b))
        bg_row.add_suffix(color_button)

        image_button = Gtk.Button(label=i18n._("settings.pages_group.background_row.choose_image"))
        image_button.set_valign(Gtk.Align.CENTER)
        bg_row.add_suffix(image_button)

        image_clear_button = Gtk.Button(label=i18n._("settings.pages_group.background_row.clear_image"))
        image_clear_button.set_valign(Gtk.Align.CENTER)
        bg_row.add_suffix(image_clear_button)

        # Only one of the two is shown at a time: with an image already set,
        # "choose" is hidden so the only way forward is "clear" first - makes
        # it obvious that changing the image means removing it, not
        # overwriting it in place.
        def sync_image_buttons():
            has_image = bool(page.background.image_path)
            image_button.set_visible(not has_image)
            image_clear_button.set_visible(has_image)

        image_button.connect("clicked", lambda _b: self._choose_page_image(page, sync_image_buttons))
        image_clear_button.connect("clicked", lambda _b: self._clear_page_image(page, sync_image_buttons))
        sync_image_buttons()

        expander.add_row(bg_row)
        return expander

    def _on_page_color_changed(self, page, button):
        page.background.set_color(button.get_rgba())
        self._window.save_page(page)

    def _choose_page_image(self, page, on_done):
        dialog = Gtk.FileDialog()
        image_filter = Gtk.FileFilter()
        image_filter.set_name(i18n._("widgets.appearance.bg_image_filter"))
        for mime_type in IMAGE_MIME_TYPES:
            image_filter.add_mime_type(mime_type)
        filters = Gio.ListStore.new(Gtk.FileFilter)
        filters.append(image_filter)
        dialog.set_filters(filters)
        dialog.open(self.get_root(), None, lambda d, r: self._on_page_image_chosen(d, r, page, on_done))

    def _on_page_image_chosen(self, dialog, result, page, on_done):
        try:
            file = dialog.open_finish(result)
        except GLib.Error:
            return
        if file is not None:
            page.background.set_image(file.get_path())
            self._window.save_page(page)
            on_done()

    def _clear_page_image(self, page, on_done):
        page.background.set_image(None)
        self._window.save_page(page)
        on_done()

    def refresh_fullscreen_shortcut_label(self):
        trigger = self._app.config.get("fullscreen_shortcut_bound_trigger")
        self._fullscreen_shortcut_label.set_label(trigger or i18n._("settings.fullscreen_shortcut.unconfigured"))

    def _on_reconfigure_clicked(self, _button):
        self._app.reconfigure_fullscreen_shortcut()

    def _refresh_deja_window_status(self):
        detected = gnome_extensions.is_extension_enabled(gnome_extensions.DEJA_WINDOW_UUID)
        title_key = "settings.deja_window_row.title_detected" if detected else "settings.deja_window_row.title_missing"
        self._deja_window_row.set_title(i18n._(title_key))
        self._deja_window_row.set_subtitle(i18n._("settings.deja_window_row.subtitle"))
        self._deja_window_icon.set_from_icon_name("emblem-ok-symbolic" if detected else "dialog-warning-symbolic")
        self._deja_window_refresh_button.set_tooltip_text(i18n._("settings.deja_window_row.refresh_tooltip"))

    def _retranslate(self):
        self._title.set_label(i18n._("settings.title"))
        self._shortcuts_group.set_title(i18n._("settings.shortcuts_group"))
        self._fullscreen_row.set_title(i18n._("settings.fullscreen_row.title"))
        self._fullscreen_row.set_subtitle(i18n._("settings.fullscreen_row.subtitle"))
        self._reconfigure_button.set_label(i18n._("settings.reconfigure_button"))
        self._goto_row.set_title(i18n._("settings.goto_row.title"))
        self._shortcuts_widget_group.set_title(i18n._("settings.shortcuts_widget_group"))
        self._host_access_row.set_title(i18n._("settings.host_access_row.title"))
        self._host_access_row.set_subtitle(i18n._("settings.host_access_row.subtitle"))
        self._interface_group.set_title(i18n._("settings.interface_group"))
        self._hide_delay_row.set_title(i18n._("settings.hide_delay_row.title"))
        self._hide_delay_row.set_subtitle(i18n._("settings.hide_delay_row.subtitle"))
        self._indicator_opacity_row.set_title(i18n._("settings.indicator_opacity_row.title"))
        self._indicator_opacity_row.set_subtitle(i18n._("settings.indicator_opacity_row.subtitle"))
        self._indicator_color_row.set_title(i18n._("settings.indicator_color_row.title"))
        self._indicator_color_reset_button.set_tooltip_text(i18n._("settings.indicator_color_row.reset_tooltip"))
        self._language_group.set_title(i18n._("settings.language_group"))
        self._language_row.set_title(i18n._("settings.language_row.title"))
        self._theme_group.set_title(i18n._("settings.theme_group.title"))
        self._accent_row.set_title(i18n._("settings.theme_group.accent_row.title"))
        for button, key, _hex_value in self._accent_buttons:
            button.set_tooltip_text(i18n._(f"settings.theme_group.accent.{key}"))
        self._accent_follow_row.set_title(i18n._("settings.theme_group.follow_system_row.title"))
        self._accent_follow_row.set_subtitle(i18n._("settings.theme_group.follow_system_row.subtitle"))
        self._appearance_group.set_title(i18n._("settings.appearance_group.title"))
        self._appearance_group.set_description(i18n._("settings.appearance_group.subtitle"))
        self._appearance_opacity_row.set_title(i18n._("widgets.appearance.opacity"))
        self._appearance_bg_color_row.set_title(i18n._("widgets.appearance.bg_color"))
        self._appearance_border_row.set_title(i18n._("widgets.appearance.border_enabled"))
        self._appearance_border_width_row.set_title(i18n._("widgets.appearance.border_width"))
        self._appearance_border_color_row.set_title(i18n._("widgets.appearance.border_color"))
        self._appearance_apply_row.set_title(i18n._("settings.appearance_apply_row.title"))
        self._appearance_apply_row.set_subtitle(i18n._("settings.appearance_apply_row.subtitle"))
        self._appearance_apply_button.set_label(i18n._("settings.appearance_apply_row.button"))
        self._extensions_group.set_title(i18n._("settings.extensions_group"))
        self._pages_group.set_title(i18n._("settings.pages_group.title"))
        self.refresh_pages()
        self.refresh_fullscreen_shortcut_label()
        self._refresh_deja_window_status()
