"""Detects whether a GNOME Shell extension is installed and enabled, via the
org.gnome.Shell.Extensions D-Bus interface (ships with gnome-shell itself, no
extra dependency needed - and, unlike shelling out to the `gnome-extensions`
CLI, reachable from inside a Flatpak sandbox with nothing more than
--talk-name=org.gnome.Shell.Extensions: a sandboxed app has no access to the
host's PATH/binaries, only to whatever D-Bus names its manifest allows.
Used by settings_page.py to point out when a recommended extension isn't
there yet - e.g. Deja Window, which lets apps launched from the Raccourcis
widget land on a specific screen instead of the Xeneon Edge bar (Wayland
gives neither this app nor the launched one any say over which monitor a new
window opens on - only the compositor, via an extension like this one, can
decide that).

Auto Move Windows (the other well-known "move this app" extension) was
tried first and confirmed *not* to work for this: it only reassigns a
window's workspace (gnome-shell-extensions' own auto-move-windows source,
extension.js, calls nothing but change_workspace_by_index()), and
workspace and monitor are independent properties in Mutter - changing one
never moves a window to a different screen. Deja Window instead saves and
restores actual per-window state including which monitor it was on, which
is what this needs. It also requires "Workspaces span all displays" ("Sur
tous les écrans") - Settings > Multitasking > Multi-Monitor - since
"primary display only" makes secondary-monitor windows workspace-invariant
in a way that broke Deja Window's monitor restore during testing."""

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

DEJA_WINDOW_UUID = "deja-window@mcast.gnomext.com"

_BUS_NAME = "org.gnome.Shell.Extensions"
_OBJECT_PATH = "/org/gnome/Shell/Extensions"
_INTERFACE = "org.gnome.Shell.Extensions"
_CALL_TIMEOUT_MS = 2000

# GNOME Shell's ExtensionState enum (js/misc/extensionUtils.js) - only the
# one value this needs.
_STATE_ENABLED = 1


def is_extension_enabled(uuid: str) -> bool:
    try:
        connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        result = connection.call_sync(
            _BUS_NAME,
            _OBJECT_PATH,
            _INTERFACE,
            "ListExtensions",
            None,
            GLib.VariantType.new("(a{sa{sv}})"),
            Gio.DBusCallFlags.NONE,
            _CALL_TIMEOUT_MS,
            None,
        )
        (extensions,) = result.unpack()
    except GLib.Error:
        return False
    info = extensions.get(uuid)
    return bool(info) and info.get("state") == _STATE_ENABLED
