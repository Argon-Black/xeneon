import logging

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk

from xeneon_dashboard import i18n, page_store, widget_store
from xeneon_dashboard.widget_appearance import defaults_touched_dict

logger = logging.getLogger(__name__)
from xeneon_dashboard.grid import GAP, SIZE_L, SIZE_M, SIZE_S, SIZE_SQ, DashboardWidget, WidgetGrid
from xeneon_dashboard.page_indicator import PageIndicator
from xeneon_dashboard.settings_page import SettingsPage
from xeneon_dashboard.widget_picker import WidgetPicker, build_from_state
from xeneon_dashboard.widgets.clock import ClockContent, ClockSettings
from xeneon_dashboard.widgets.dummy import DummyContent, color_for_size

# Keeps widgets off the physical screen edge (bezel / touch-hit margin).
# Deliberately the same value as GAP, the spacing between widgets, so the
# page edge reads as just another gap in the grid.
PAGE_MARGIN = GAP

# Settings is always the last page; up to this many widget pages before it.
MAX_WIDGET_PAGES = 9

# All three widget sizes share a width, so columns are simple to lay out.
_COL_W = SIZE_M[0]


def _col_x(index: int) -> int:
    return index * (_COL_W + GAP)


# Dummy widgets covering every grid size preset (S/M/L/SQ), filling the
# real panel's 3 columns exactly (see grid.py) without overflowing the
# page - each just shows its own size code, large and centered, with a
# distinct background color per preset. As (title key, size code, x, y,
# size).
_S_PITCH = SIZE_S[1] + GAP
_DUMMY_WIDGETS = [
    # Column 0: one L.
    ("widgets.dummy.title_l", "L", _col_x(0), 0, SIZE_L),
    # Column 1: two M stacked (2xM + GAP == L, per grid.py).
    ("widgets.dummy.title_m", "M", _col_x(1), 0, SIZE_M),
    ("widgets.dummy.title_m", "M", _col_x(1), SIZE_M[1] + GAP, SIZE_M),
    # Column 2: three S stacked, then a pair of SQ side by side - together
    # they line up flush with the bottom of column 0's L.
    ("widgets.dummy.title_s", "S", _col_x(2), 0, SIZE_S),
    ("widgets.dummy.title_s", "S", _col_x(2), _S_PITCH, SIZE_S),
    ("widgets.dummy.title_s", "S", _col_x(2), 2 * _S_PITCH, SIZE_S),
    ("widgets.dummy.title_sq", "SQ", _col_x(2), 3 * _S_PITCH, SIZE_SQ),
    ("widgets.dummy.title_sq", "SQ", _col_x(2) + SIZE_SQ[0] + GAP, 3 * _S_PITCH, SIZE_SQ),
]


class XeneonWindow(Adw.ApplicationWindow):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.set_title(i18n._("window.title"))
        # This app only ever runs on the Xeneon Edge bar screen - hardcoded
        # to its exact resolution (2560x720, with the display scale set to
        # 100% in GNOME Settings so logical pixels == physical pixels),
        # matching the widget grid's own coordinates (see grid.py) rather
        # than some smaller dev-convenience size. Only matters windowed
        # (dev fallback); fullscreen_on_monitor() already forces this size
        # for real on the actual hardware.
        self.set_default_size(2560, 720)

        self._toolbar_view = Adw.ToolbarView()
        self._header_bar = Adw.HeaderBar()
        self._settings_button = Gtk.Button(icon_name="preferences-system-symbolic")
        self._settings_button.set_tooltip_text(i18n._("header.settings_tooltip"))
        self._settings_button.set_action_name("app.goto-settings")
        self._header_bar.pack_end(self._settings_button)
        self._toolbar_view.add_top_bar(self._header_bar)
        self.connect("notify::fullscreened", self._on_fullscreened_changed)

        self.carousel = Adw.Carousel()
        self.carousel.set_vexpand(True)

        self._widget_pages: list[WidgetGrid] = []
        self._settings_page = SettingsPage(self)

        carousel_overlay = Gtk.Overlay()
        carousel_overlay.set_child(self.carousel)
        app = self.get_application()
        hide_delay = app.config.get("indicator_hide_delay_seconds") if app else None
        self._indicator = PageIndicator(self.carousel, self._settings_page, hide_delay_seconds=hide_delay or 2)
        # Floats over the carousel's bottom edge instead of taking flow
        # space, so it appearing/hiding never reflows page content.
        carousel_overlay.add_overlay(self._indicator)

        self._content_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        # AdwToolbarView normally tags its content "view" for a flat
        # background; without it (bypassed while fullscreen) the window's
        # raw titlebar-blend gradient shows through as a banded background.
        self._content_box.add_css_class("view")
        self._content_box.append(carousel_overlay)

        # Wraps _content_box so the widget picker (see widget_picker.py) can
        # sit above literally everything - carousel, page indicator, header
        # bar when windowed - as a full-window overlay rather than a
        # popover. Swapped in wherever _content_box used to be set as
        # content directly (here and in _on_fullscreened_changed) so this
        # covers both the windowed (behind _toolbar_view) and fullscreen
        # (bypassing it) cases the same way.
        self._root_overlay = Gtk.Overlay()
        self._root_overlay.set_child(self._content_box)
        self._widget_picker = WidgetPicker(self._on_widget_picked)
        self._root_overlay.add_overlay(self._widget_picker)

        self._toolbar_view.set_content(self._root_overlay)
        self.set_content(self._toolbar_view)

        self._build_pages()
        self._indicator.refresh()

        i18n.on_change(self._retranslate)

    def _on_fullscreened_changed(self, *_args):
        # Kiosk look while fullscreen on the Xeneon: bypass AdwToolbarView
        # entirely (not just hide its header bar) so none of its top-bar
        # chrome (undershoot shadow, reserved padding) survives. Restore it
        # in windowed mode so the window stays usable for development
        # (move/close/resize). Guarded against "notify::fullscreened" firing
        # more than once per transition.
        if self.is_fullscreen() and self.get_content() is not self._root_overlay:
            self._toolbar_view.set_content(None)
            self.set_content(self._root_overlay)
        elif not self.is_fullscreen() and self.get_content() is not self._toolbar_view:
            self._toolbar_view.set_content(self._root_overlay)
            self.set_content(self._toolbar_view)

    def goto_settings(self):
        self.carousel.scroll_to(self._settings_page, True)

    def set_indicator_hide_delay(self, seconds: int):
        self._indicator.set_hide_delay_seconds(seconds)

    def refresh_fullscreen_shortcut_label(self):
        self._settings_page.refresh_fullscreen_shortcut_label()

    def add_widget_page(
        self, *, page_id: str | None = None, name: str | None = None, background_state: dict | None = None
    ) -> WidgetGrid | None:
        """Add a new widget page, inserted just before the settings page
        (which always stays last). Returns None once MAX_WIDGET_PAGES is hit."""
        if len(self._widget_pages) >= MAX_WIDGET_PAGES:
            return None
        page = WidgetGrid(page_id=page_id, name=name, background_state=background_state)
        page.page_index = len(self._widget_pages)
        page.set_hexpand(True)
        page.set_vexpand(True)
        page.set_margin_start(PAGE_MARGIN)
        page.set_margin_end(PAGE_MARGIN)
        page.set_margin_top(PAGE_MARGIN)
        page.set_margin_bottom(PAGE_MARGIN)
        self.carousel.insert(page, len(self._widget_pages))
        self._widget_pages.append(page)
        # Keeps the settings page's page list (rename/background rows) in
        # sync the moment a page is created, whether that's at startup or
        # from a widget overflowing onto a fresh page mid-session.
        self._settings_page.refresh_pages()
        return page

    def widget_pages(self) -> list[WidgetGrid]:
        return list(self._widget_pages)

    def save_page(self, page: WidgetGrid) -> None:
        self._save_page(page)
        # A rename changes what the indicator should show for this page;
        # cheap enough to just always refresh rather than tracking exactly
        # which kind of page edit is behind this particular save.
        self._indicator.refresh()

    def _build_pages(self):
        """Restores the widget layout saved on the previous run (see
        widget_store.py / _save_widget) and each page's own name/background
        (see page_store.py / _save_page), or - on a fresh install, where
        nothing has been saved yet - falls back to the hardcoded demo
        layout. Either way the settings page always ends up last."""
        widget_states = widget_store.load_all()
        page_states = page_store.load_all()
        if widget_states or page_states:
            self._build_pages_from_state(widget_states, page_states)
        else:
            self._build_default_pages()
        self.carousel.append(self._settings_page)

    def _build_pages_from_state(self, widget_states: list[dict], page_states: list[dict]):
        # A page can have saved state (a custom name or background) with no
        # widgets on it at all, so the page count has to account for both
        # sources rather than just the highest page_index seen among widgets.
        max_widget_page = max((state.get("page_index", 0) for state in widget_states), default=-1)
        max_saved_page = max((state.get("page_index", 0) for state in page_states), default=-1)
        page_states_by_index = {state["page_index"]: state for state in page_states if "page_index" in state}

        for index in range(max(max_widget_page, max_saved_page) + 1):
            state = page_states_by_index.get(index)
            if state is not None:
                self.add_widget_page(page_id=state["id"], name=state.get("name"), background_state=state.get("background"))
            else:
                self.add_widget_page()

        for state in widget_states:
            page_index = state.get("page_index", 0)
            if not (0 <= page_index < len(self._widget_pages)):
                logger.warning("Widget %s ignoré: page_index %r hors limites", state.get("id"), page_index)
                continue
            widget = build_from_state(state, on_change=self._save_widget, on_delete=self._delete_widget)
            if widget is not None:
                self._widget_pages[page_index].add_widget(widget)

    def _build_default_pages(self):
        page = self.add_widget_page()

        clock_content = ClockContent()
        clock_settings = ClockSettings(clock_content)
        clock_widget = DashboardWidget(
            "widgets.clock.title",
            clock_content,
            x=_col_x(0),
            y=0,
            size=SIZE_M,
            settings=clock_settings,
            kind="clock",
            on_change=self._save_widget,
            on_delete=self._delete_widget,
            on_reset=lambda: (clock_content.reset(), clock_settings.sync_from_content()),
        )
        page.add_widget(clock_widget)
        self._save_widget(clock_widget)

        dummy_page = self.add_widget_page()
        for title_key, size_code, x, y, size in _DUMMY_WIDGETS:
            widget = DashboardWidget(
                title_key,
                DummyContent(size_code),
                x=x,
                y=y,
                size=size,
                kind=f"dummy_{size_code.lower()}",
                on_change=self._save_widget,
                on_delete=self._delete_widget,
            )
            widget.appearance.set_bg_color(color_for_size(size_code))
            dummy_page.add_widget(widget)
            self._save_widget(widget)

    def _save_widget(self, widget: DashboardWidget) -> None:
        if widget.grid is None:
            return
        state = {
            "kind": widget.kind,
            "page_index": widget.grid.page_index,
            "x": widget.x,
            "y": widget.y,
            "w": widget.w,
            "h": widget.h,
            "appearance": widget.appearance.to_dict(),
        }
        content_to_dict = getattr(widget.content, "to_dict", None)
        if content_to_dict is not None:
            state["content"] = content_to_dict()
        widget_store.save(widget.widget_id, state)

    def _delete_widget(self, widget: DashboardWidget) -> None:
        widget_store.delete(widget.widget_id)

    def _save_page(self, page: WidgetGrid) -> None:
        state = {
            "page_index": page.page_index,
            "name": page.custom_name,
            "background": page.background.to_dict(),
        }
        page_store.save(page.page_id, state)

    def show_widget_picker(self):
        self._widget_picker.open()

    def _on_widget_picked(self, size: tuple[int, int], spawn):
        self.add_widget(size, spawn)

    def _current_widget_page_index(self) -> int:
        if not self._widget_pages:
            return 0
        position = round(self.carousel.get_position())
        return max(0, min(position, len(self._widget_pages) - 1))

    def add_widget(self, size: tuple[int, int], spawn) -> None:
        """Places a new widget built by `spawn(x, y)` on the page currently
        shown, or - once that one (and every later existing page) is full -
        on a freshly created page right after it, exactly like an overflowing
        icon in iCUE lands on the next page instead of being dropped."""
        start_index = self._current_widget_page_index()
        page = None
        position = None
        for index in range(start_index, len(self._widget_pages)):
            candidate = self._widget_pages[index]
            candidate_position = candidate.find_free_position(size)
            if candidate_position is not None:
                page, position = candidate, candidate_position
                break

        if page is None:
            page = self.add_widget_page()
            if page is None:
                return
            position = page.find_free_position(size)
            if position is None:
                return

        x, y = position
        widget = spawn(x, y, on_change=self._save_widget, on_delete=self._delete_widget)
        # Only when the spawner itself left the appearance untouched (a
        # dummy widget's per-size demo color, e.g., already counts as
        # touched) - the global default must never override something more
        # specific. See widget_appearance.has_customizations().
        if not widget.appearance.has_customizations():
            app = self.get_application()
            if app is not None:
                widget.appearance.apply_dict(defaults_touched_dict(app.config.get("default_widget_appearance")))
        page.add_widget(widget)
        self._save_widget(widget)
        self.carousel.scroll_to(page, True)

    def apply_default_widget_appearance_to_all(self) -> None:
        """Force-overwrites every existing widget's appearance (on every
        page) with the current default-appearance setting - opt-in only,
        wired to the "Appliquer à tous les widgets" button in
        SettingsPage. Unlike add_widget()'s at-creation default, this
        deliberately ignores has_customizations(): the whole point of
        clicking that button is to overwrite per-widget customizations."""
        app = self.get_application()
        if app is None:
            return
        payload = defaults_touched_dict(app.config.get("default_widget_appearance"))
        for page in self._widget_pages:
            for widget in page.widgets():
                widget.appearance.apply_dict(payload)
                self._save_widget(widget)

    def _retranslate(self):
        self.set_title(i18n._("window.title"))
        self._settings_button.set_tooltip_text(i18n._("header.settings_tooltip"))
