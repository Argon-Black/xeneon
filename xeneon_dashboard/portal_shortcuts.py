"""Global keyboard shortcut via the XDG Desktop Portal GlobalShortcuts
interface (org.freedesktop.portal.GlobalShortcuts), instead of a
GNOME-specific dconf custom-keybinding: this works the same way under any
compositor implementing the portal (GNOME 45+, KDE Plasma 6.1+), and inside
a Flatpak sandbox with no extra filesystem/settings permissions.

The trade-off is a heavier async flow: create a session, ask the user (via
a portal-drawn dialog, owned by the compositor - not us) to bind a trigger
to our shortcut id, then listen for "Activated" signals. A session is
restored silently on later runs via a "restore_token" so the binding dialog
isn't shown again every startup. Rebinding later goes through
ConfigureShortcuts, which reopens that same native dialog - we don't build
our own key-capture UI for this.
"""

import gi

gi.require_version("Gio", "2.0")
gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

BUS_NAME = "org.freedesktop.portal.Desktop"
OBJECT_PATH = "/org/freedesktop/portal/desktop"
SHORTCUTS_IFACE = "org.freedesktop.portal.GlobalShortcuts"
REQUEST_IFACE = "org.freedesktop.portal.Request"

SHORTCUT_ID = "toggle-fullscreen"


class GlobalShortcutsPortal:
    def __init__(self, on_activated, on_shortcuts_changed=None):
        """on_activated(shortcut_id: str) fires whenever a bound shortcut is
        pressed, regardless of which window (if any) has focus.
        on_shortcuts_changed(bound_trigger: str | None) fires when the user
        rebinds via the portal's own UI (e.g. through ConfigureShortcuts)."""
        self._on_activated = on_activated
        self._on_shortcuts_changed = on_shortcuts_changed
        self._connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        self._sender_token = self._connection.get_unique_name()[1:].replace(".", "_")
        self._session_handle = None
        self._connection.signal_subscribe(
            BUS_NAME, SHORTCUTS_IFACE, "Activated", OBJECT_PATH, None,
            Gio.DBusSignalFlags.NONE, self._on_activated_signal,
        )
        self._connection.signal_subscribe(
            BUS_NAME, SHORTCUTS_IFACE, "ShortcutsChanged", OBJECT_PATH, None,
            Gio.DBusSignalFlags.NONE, self._on_shortcuts_changed_signal,
        )

    def _on_activated_signal(self, _conn, _sender, _path, _iface, _signal, params):
        session_handle, shortcut_id, _timestamp, _options = params.unpack()
        if session_handle == self._session_handle:
            self._on_activated(shortcut_id)

    def _on_shortcuts_changed_signal(self, _conn, _sender, _path, _iface, _signal, params):
        session_handle, shortcuts = params.unpack()
        if session_handle != self._session_handle or self._on_shortcuts_changed is None:
            return
        bound_trigger = next(
            (props.get("trigger_description") for sid, props in shortcuts if sid == SHORTCUT_ID), None
        )
        self._on_shortcuts_changed(bound_trigger)

    def _request_path(self, handle_token: str) -> str:
        return f"/org/freedesktop/portal/desktop/request/{self._sender_token}/{handle_token}"

    def _call_and_wait(self, method: str, arg_variant: GLib.Variant, on_response, timeout_seconds: int = 5):
        # Some portal backends (notably GNOME Shell's GlobalShortcuts impl,
        # as of testing on this machine) silently never answer a request
        # from a caller they don't recognise as a sandboxed app - no error,
        # no signal, forever. This is expected to work once the app is a
        # real Flatpak (portal backends key off Flatpak sandbox info to
        # identify the caller) - revalidate at packaging time. Until then,
        # give up after a timeout instead of leaving a dangling subscription.
        handle_token = Gio.dbus_generate_guid().replace("-", "_")
        path = self._request_path(handle_token)
        state = {"sub_id": None, "timeout_id": None, "done": False}

        def finish(code, results):
            if state["done"]:
                return
            state["done"] = True
            if state["sub_id"] is not None:
                self._connection.signal_unsubscribe(state["sub_id"])
            if state["timeout_id"] is not None:
                GLib.source_remove(state["timeout_id"])
            on_response(code, results)

        def on_signal(_conn, _sender, _path, _iface, _signal, params):
            code, results = params.unpack()
            finish(code, results)

        def on_timeout():
            finish(-1, {})
            return GLib.SOURCE_REMOVE

        state["sub_id"] = self._connection.signal_subscribe(
            BUS_NAME, REQUEST_IFACE, "Response", path, None,
            Gio.DBusSignalFlags.NONE, on_signal,
        )
        state["timeout_id"] = GLib.timeout_add_seconds(timeout_seconds, on_timeout)
        self._connection.call(
            BUS_NAME, OBJECT_PATH, SHORTCUTS_IFACE, method, arg_variant, None,
            Gio.DBusCallFlags.NONE, -1, None, None,
        )

    def ensure_session(self, restore_token: str | None, description: str, accelerator_hint: str | None, on_done):
        """Create (or silently restore) a portal session, then bind our one
        shortcut. on_done(new_restore_token, bound_trigger) is called once
        the whole flow settles; both may be None on failure."""
        create_options = {
            "handle_token": GLib.Variant("s", Gio.dbus_generate_guid().replace("-", "_")),
            "session_handle_token": GLib.Variant("s", Gio.dbus_generate_guid().replace("-", "_")),
        }
        if restore_token:
            create_options["restore_token"] = GLib.Variant("s", restore_token)

        def on_create_response(code, results):
            if code != 0:
                on_done(None, None)
                return
            self._session_handle = results["session_handle"]
            new_restore_token = results.get("restore_token", restore_token)
            self._bind_shortcut(description, accelerator_hint, new_restore_token, on_done)

        self._call_and_wait("CreateSession", GLib.Variant("(a{sv})", (create_options,)), on_create_response)

    def _bind_shortcut(self, description: str, accelerator_hint: str | None, restore_token: str | None, on_done):
        shortcut_props = {"description": GLib.Variant("s", description)}
        if accelerator_hint:
            shortcut_props["preferred_trigger"] = GLib.Variant("s", accelerator_hint)
        shortcuts = [(SHORTCUT_ID, shortcut_props)]

        bind_options = {"handle_token": GLib.Variant("s", Gio.dbus_generate_guid().replace("-", "_"))}

        def on_bind_response(code, results):
            bound_trigger = next(
                (props.get("trigger_description") for sid, props in results.get("shortcuts", []) if sid == SHORTCUT_ID),
                None,
            )
            on_done(restore_token, bound_trigger)

        self._call_and_wait(
            "BindShortcuts",
            GLib.Variant("(oa(sa{sv})sa{sv})", (self._session_handle, shortcuts, "", bind_options)),
            on_bind_response,
        )

    def reconfigure(self):
        """Reopens the compositor's own shortcut-assignment UI so the user
        can rebind. Fire-and-forget; the result arrives as ShortcutsChanged."""
        if self._session_handle is None:
            return
        self._connection.call(
            BUS_NAME, OBJECT_PATH, SHORTCUTS_IFACE, "ConfigureShortcuts",
            GLib.Variant("(osa{sv})", (self._session_handle, "", {})),
            None, Gio.DBusCallFlags.NONE, -1, None, None,
        )
