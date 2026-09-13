import gi

gi.require_version("Gdk", "4.0")
from gi.repository import Gdk

# Reported via EDID as model "XENEON EDGE" / manufacturer "CRX" (Corsair).
# Matching on this is unambiguous, unlike aspect ratio: this rig also has a
# second ultrawide (Philips 499P9) at the same 32:9-ish aspect ratio.
XENEON_MODEL_HINTS = ("XENEON EDGE",)
XENEON_MANUFACTURER_HINTS = ("CRX", "CORSAIR")

# Fallback only, used if EDID strings aren't exposed by the compositor.
XENEON_MIN_ASPECT = 2.5


def find_xeneon_monitor(display: Gdk.Display) -> Gdk.Monitor | None:
    """Return the Xeneon Edge bar screen among connected monitors, if any."""
    monitors = display.get_monitors()
    items = [monitors.get_item(i) for i in range(monitors.get_n_items())]
    if not items:
        return None

    def matches_edid(monitor: Gdk.Monitor) -> bool:
        model = (monitor.get_model() or "").upper()
        manufacturer = (monitor.get_manufacturer() or "").upper()
        return any(hint in model for hint in XENEON_MODEL_HINTS) or manufacturer in XENEON_MANUFACTURER_HINTS

    for monitor in items:
        if matches_edid(monitor):
            return monitor

    def aspect(monitor: Gdk.Monitor) -> float:
        geometry = monitor.get_geometry()
        return geometry.width / geometry.height if geometry.height else 0

    widest = max(items, key=aspect)
    return widest if aspect(widest) >= XENEON_MIN_ASPECT else None
