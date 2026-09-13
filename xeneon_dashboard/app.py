import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, Gio

from xeneon_dashboard import config, i18n, log, page_indicator, theme
from xeneon_dashboard.display import find_xeneon_monitor
from xeneon_dashboard.portal_shortcuts import SHORTCUT_ID, GlobalShortcutsPortal
from xeneon_dashboard.window import XeneonWindow


class XeneonApp(Adw.Application):
    def __init__(self):
        super().__init__(application_id="com.n3tlab.XeneonDashboard")
        log.init()
        self.config = config.load()
        i18n.init(self.config.get("language"))
        self._portal = None

    def do_startup(self):
        Adw.Application.do_startup(self)
        page_indicator.install_css(
            self.config.get("indicator_opacity"),
            self.config.get("indicator_button_color"),
        )
        theme.connect_system_accent_changed(self._on_system_accent_changed)
        theme.install_css(self.config.get("accent_color"))
        self._apply_configured_accent()

        toggle_action = Gio.SimpleAction.new("toggle-fullscreen", None)
        toggle_action.connect("activate", self._on_toggle_fullscreen)
        self.add_action(toggle_action)

        goto_settings_action = Gio.SimpleAction.new("goto-settings", None)
        goto_settings_action.connect("activate", self._on_goto_settings)
        self.add_action(goto_settings_action)
        self.set_accels_for_action("app.goto-settings", ["<Primary>comma"])

        add_widget_action = Gio.SimpleAction.new("add-widget", None)
        add_widget_action.connect("activate", self._on_add_widget)
        self.add_action(add_widget_action)
        # "equal" too - most keyboards type "+" as shift+"=", but GNOME apps
        # conventionally also accept the bare "=" key for a "+" shortcut
        # (same as browser zoom-in) since reaching for shift is easy to skip.
        self.set_accels_for_action("app.add-widget", ["<Primary>plus", "<Primary>KP_Add", "<Primary>equal"])

        # F11 only works while the window has focus; the real "works from
        # anywhere" shortcut is granted through the GlobalShortcuts portal.
        self.set_accels_for_action("app.toggle-fullscreen", ["F11"])

        self._portal = GlobalShortcutsPortal(
            on_activated=self._on_portal_shortcut_activated,
            on_shortcuts_changed=self._on_portal_shortcuts_changed,
        )
        self._portal.ensure_session(
            restore_token=self.config.get("fullscreen_shortcut_restore_token"),
            description=i18n._("portal.fullscreen_description"),
            accelerator_hint=self.config.get("fullscreen_shortcut_hint"),
            on_done=self._on_portal_session_ready,
        )

    def do_activate(self):
        win = self.props.active_window
        if not win:
            win = XeneonWindow(application=self)
            monitor = find_xeneon_monitor(Gdk.Display.get_default())
            if monitor is not None:
                win.fullscreen_on_monitor(monitor)
        win.present()

    def reconfigure_fullscreen_shortcut(self) -> None:
        if self._portal is not None:
            self._portal.reconfigure()

    def set_indicator_hide_delay(self, seconds: int) -> None:
        self.config["indicator_hide_delay_seconds"] = seconds
        config.save(self.config)
        win = self.props.active_window
        if win is not None:
            win.set_indicator_hide_delay(seconds)

    def set_indicator_opacity(self, percent: int) -> None:
        self.config["indicator_opacity"] = percent
        config.save(self.config)
        page_indicator.apply_style(percent, self.config.get("indicator_button_color"))

    def set_indicator_button_color(self, color_hex: str | None) -> None:
        self.config["indicator_button_color"] = color_hex
        config.save(self.config)
        page_indicator.apply_style(self.config.get("indicator_opacity"), color_hex)

    def set_language(self, lang: str) -> None:
        self.config["language"] = lang
        config.save(self.config)
        i18n.set_language(lang)

    def set_default_widget_appearance(self, appearance: dict) -> None:
        self.config["default_widget_appearance"] = appearance
        config.save(self.config)

    def set_shortcuts_host_access(self, enabled: bool) -> None:
        self.config["shortcuts_host_access"] = enabled
        config.save(self.config)

    def set_accent_color(self, accent_hex: str) -> None:
        self.config["accent_color"] = accent_hex
        config.save(self.config)
        theme.apply_accent(accent_hex)

    def set_accent_follow_system(self, enabled: bool) -> None:
        self.config["accent_follow_system"] = enabled
        config.save(self.config)
        self._apply_configured_accent()

    def _apply_configured_accent(self) -> None:
        if self.config.get("accent_follow_system"):
            system_hex = theme.system_accent_hex()
            if system_hex is not None:
                theme.apply_accent(system_hex)
                return
        theme.apply_accent(self.config.get("accent_color"))

    def _on_system_accent_changed(self) -> None:
        if self.config.get("accent_follow_system"):
            self._apply_configured_accent()

    def _on_portal_session_ready(self, restore_token, bound_trigger):
        self.config["fullscreen_shortcut_restore_token"] = restore_token
        self.config["fullscreen_shortcut_bound_trigger"] = bound_trigger
        config.save(self.config)
        win = self.props.active_window
        if win is not None:
            win.refresh_fullscreen_shortcut_label()

    def _on_portal_shortcuts_changed(self, bound_trigger):
        self.config["fullscreen_shortcut_bound_trigger"] = bound_trigger
        config.save(self.config)
        win = self.props.active_window
        if win is not None:
            win.refresh_fullscreen_shortcut_label()

    def _on_portal_shortcut_activated(self, shortcut_id):
        if shortcut_id == SHORTCUT_ID:
            self._on_toggle_fullscreen(None, None)

    def _on_toggle_fullscreen(self, _action, _param):
        win = self.props.active_window
        if win is None:
            return
        if win.is_fullscreen():
            win.unfullscreen()
        else:
            monitor = find_xeneon_monitor(Gdk.Display.get_default())
            if monitor is not None:
                win.fullscreen_on_monitor(monitor)
            else:
                win.fullscreen()

    def _on_goto_settings(self, _action, _param):
        win = self.props.active_window
        if win is not None:
            win.goto_settings()

    def _on_add_widget(self, _action, _param):
        win = self.props.active_window
        if win is not None:
            win.show_widget_picker()


def main() -> int:
    app = XeneonApp()
    return app.run(sys.argv)
