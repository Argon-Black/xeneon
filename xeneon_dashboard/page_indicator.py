import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, GLib, Gtk, Pango

from xeneon_dashboard import i18n

DEFAULT_HIDE_DELAY_SECONDS = 2
# Sized off the Xeneon Edge's actual pixel density, not a round guess: it's
# a 14.5" 32:9 panel at 2560x720, so diagonal = sqrt(2560^2 + 720^2) ~=
# 2659px over 14.5in -> ~183.4 ppi -> ~72.2px/cm (183.4 / 2.54). A fingertip
# contact patch is roughly 1cm across, hence 72px - both indicator buttons
# are sized to that so a tap lands reliably instead of clipping a neighbor.
TOUCH_TARGET_PX = 72
SETTINGS_ICON_PIXEL_SIZE = 36

DEFAULT_OPACITY_PERCENT = 55

# Rules that never change with the user's indicator style settings (see
# _build_indicator_css below for the ones that do).
_STATIC_CSS = """
button.xeneon-widget-configure, button.xeneon-widget-delete, button.xeneon-widget-move {
  min-width: 56px;
  min-height: 56px;
  padding: 4px;
  margin: 2px;
}
"""

_provider = Gtk.CssProvider()
_installed = False


def _build_indicator_css(opacity_percent: int, color_hex: str | None) -> str:
    """The indicator buttons' own CSS, rebuilt whenever the user changes the
    carousel transparency/color settings (see XeneonApp.set_indicator_opacity
    / set_indicator_button_color). `color_hex` unset means "use the theme's
    own foreground color" (currentColor) - same untouched-by-default
    convention as WidgetAppearance/PageBackground. Setting `color` here also
    retints the backgrounds below (they're alpha(currentColor, ...)) and the
    settings icon (a symbolic icon follows the widget's `color`) for free."""
    inactive_opacity = max(10, min(100, opacity_percent)) / 100
    color_rule = f"color: {color_hex};" if color_hex else ""
    return f"""
button.xeneon-page-number {{
  min-width: {TOUCH_TARGET_PX}px;
  min-height: {TOUCH_TARGET_PX}px;
  padding: 6px;
  margin: 0 8px;
  opacity: {inactive_opacity:.2f};
  background-color: alpha(currentColor, 0.18);
  font-weight: bold;
  border-radius: 18px;
  {color_rule}
}}
button.xeneon-page-number.active {{
  opacity: 1;
  background-color: alpha(currentColor, 0.35);
}}
button.xeneon-page-settings {{
  min-width: {TOUCH_TARGET_PX}px;
  min-height: {TOUCH_TARGET_PX}px;
  padding: 6px;
  margin: 0 8px;
  opacity: {inactive_opacity:.2f};
  border-radius: 18px;
  {color_rule}
}}
button.xeneon-page-settings.active {{
  opacity: 1;
}}
button.xeneon-page-add {{
  min-width: {TOUCH_TARGET_PX}px;
  min-height: {TOUCH_TARGET_PX}px;
  padding: 6px;
  margin: 0 8px;
  opacity: {inactive_opacity:.2f};
  border-radius: 18px;
  {color_rule}
}}
"""


class PageIndicator(Gtk.Revealer):
    """Clickable, numbered page buttons, floating over the carousel's bottom
    edge, shown only while actually swiping/scrolling between pages (not on
    mere hover - see the "notify::position" connection below) and
    auto-hidden otherwise, except on the settings page where it always stays
    up (see _show/_is_on_settings_page)."""

    def __init__(
        self,
        carousel: Adw.Carousel,
        settings_page: Gtk.Widget,
        hide_delay_seconds: int = DEFAULT_HIDE_DELAY_SECONDS,
        *,
        on_add_page=None,
        max_widget_pages: int | None = None,
    ):
        super().__init__()
        self._carousel = carousel
        self._settings_page = settings_page
        self._hide_delay_seconds = hide_delay_seconds
        self._hide_source_id: int | None = None
        # Called (no args) when the add-page button is tapped - see
        # XeneonWindow._on_add_page_clicked, which creates the page and
        # navigates to it. max_widget_pages, if given, hides that button
        # once XeneonWindow.MAX_WIDGET_PAGES worth of pages already exist,
        # rather than leaving a button that would silently no-op on tap.
        self._on_add_page = on_add_page
        self._max_widget_pages = max_widget_pages

        self.set_transition_type(Gtk.RevealerTransitionType.CROSSFADE)
        self.set_transition_duration(200)
        self.set_reveal_child(False)
        self.set_halign(Gtk.Align.CENTER)
        self.set_valign(Gtk.Align.END)
        # CROSSFADE never shrinks the revealer's own allocation to 0 (unlike
        # a slide transition) - it just fades opacity, so with can_target
        # left on permanently this floating bar would keep eating taps
        # meant for whatever's underneath it (e.g. a widget with something
        # low on its own page) even while fully invisible. Toggled alongside
        # reveal_child in _show()/_hide() instead, so it only accepts input
        # while actually shown (or fading in/out).
        self.set_can_target(False)

        self._box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
        self._box.set_margin_bottom(10)
        self.set_child(self._box)

        # Reveals only on an actual page change (swipe, or a dot tap once
        # already visible mid-swipe) - deliberately not on hover/motion, so
        # just resting a finger or the pointer over the bar doesn't summon it.
        carousel.connect("notify::position", lambda *_a: self._show())
        carousel.connect("notify::n-pages", lambda *_a: self.refresh())

        self.refresh()

    def set_hide_delay_seconds(self, seconds: int):
        self._hide_delay_seconds = seconds

    def refresh(self):
        child = self._box.get_first_child()
        while child is not None:
            next_child = child.get_next_sibling()
            self._box.remove(child)
            child = next_child

        n_pages = self._carousel.get_n_pages()
        n_widget_pages = 0
        for i in range(n_pages):
            page = self._carousel.get_nth_page(i)
            if page is self._settings_page:
                continue
            n_widget_pages += 1
            button = Gtk.Button()
            button.add_css_class("flat")
            button.add_css_class("xeneon-page-number")
            # A renamed page shows its name instead of a bare number -
            # getattr rather than an import/isinstance check since the
            # only thing this module needs from a WidgetGrid page is
            # this one attribute. Falls back to the number (by carousel
            # position, not page.page_index - always the same value
            # since settings is always last, but the carousel is the
            # thing actually being counted here) for an unnamed page,
            # same as before.
            custom_name = getattr(page, "custom_name", None)
            if custom_name:
                label = Gtk.Label(label=custom_name)
                label.set_max_width_chars(10)
                label.set_ellipsize(Pango.EllipsizeMode.END)
                label.set_single_line_mode(True)
                button.set_child(label)
            else:
                button.set_label(str(i + 1))
            button._xeneon_page = page
            button.connect("clicked", self._on_dot_clicked, page)
            self._box.append(button)

        # Sits right before the settings button, not tied to any carousel
        # page itself - just an action. Hidden once at capacity rather than
        # left there to silently no-op on tap.
        at_capacity = self._max_widget_pages is not None and n_widget_pages >= self._max_widget_pages
        if self._on_add_page is not None and not at_capacity:
            add_button = Gtk.Button(icon_name="list-add-symbolic")
            add_button.add_css_class("flat")
            add_button.add_css_class("xeneon-page-add")
            add_button.set_tooltip_text(i18n._("carousel.add_page_tooltip"))
            add_button.connect("clicked", lambda _b: self._on_add_page())
            self._box.append(add_button)

        settings_button = Gtk.Button()
        settings_button.add_css_class("flat")
        # Rounded square rather than GTK's "circular" style class (which
        # would force a perfect circle/pill via its own border-radius) -
        # a squarer shape is more forgiving of an off-center tap on a
        # touch target this size, and stays legible with a name label
        # (see xeneon-page-number's own border-radius above/below).
        icon = Gtk.Image.new_from_icon_name("preferences-system-symbolic")
        icon.set_pixel_size(SETTINGS_ICON_PIXEL_SIZE)
        settings_button.set_child(icon)
        settings_button.add_css_class("xeneon-page-settings")
        settings_button._xeneon_page = self._settings_page
        settings_button.connect("clicked", self._on_dot_clicked, self._settings_page)
        self._box.append(settings_button)

        self._update_active()

    def _on_dot_clicked(self, _button, page):
        self._carousel.scroll_to(page, True)
        self._show()

    def _update_active(self):
        position = round(self._carousel.get_position())
        current_page = self._carousel.get_nth_page(position)
        for child in self._iter_children():
            if getattr(child, "_xeneon_page", None) is current_page:
                child.add_css_class("active")
            else:
                child.remove_css_class("active")

    def _iter_children(self):
        child = self._box.get_first_child()
        while child is not None:
            yield child
            child = child.get_next_sibling()

    def _show(self):
        self.set_reveal_child(True)
        self.set_can_target(True)
        self._update_active()
        if self._hide_source_id is not None:
            GLib.source_remove(self._hide_source_id)
            self._hide_source_id = None
        # On the settings page itself, stay revealed instead of scheduling
        # the usual auto-hide - it's where the transparency/hide-delay
        # settings live, so hiding the very thing being configured would be
        # self-defeating. carousel's "notify::position" already calls
        # _show() on every page change (including landing on settings), so
        # this alone is enough to both show it there and resume normal
        # auto-hide the moment the user swipes away again.
        if self._is_on_settings_page():
            return
        self._hide_source_id = GLib.timeout_add_seconds(self._hide_delay_seconds, self._hide)

    def _is_on_settings_page(self) -> bool:
        position = round(self._carousel.get_position())
        return self._carousel.get_nth_page(position) is self._settings_page

    def _hide(self):
        self.set_reveal_child(False)
        self.set_can_target(False)
        self._hide_source_id = None
        return GLib.SOURCE_REMOVE


def _ensure_installed():
    global _installed
    if _installed:
        return
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(),
        _provider,
        Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
    )
    _installed = True


def install_css(opacity_percent: int = DEFAULT_OPACITY_PERCENT, color_hex: str | None = None):
    _ensure_installed()
    apply_style(opacity_percent, color_hex)


def apply_style(opacity_percent: int, color_hex: str | None) -> None:
    """Rebuilds and reloads the indicator's CSS - called at startup (see
    install_css) and again whenever the carousel transparency/color settings
    change (see XeneonApp.set_indicator_opacity/set_indicator_button_color)."""
    _ensure_installed()
    _provider.load_from_string(_STATIC_CSS + _build_indicator_css(opacity_percent, color_hex))
