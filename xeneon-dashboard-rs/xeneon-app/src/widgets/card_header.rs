// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared "icon + title + spacer" card-header row, used by every
//! SQ-footprint widget that has one (`hue.rs`, `network_sq.rs`,
//! `network_m.rs`, `system_sq.rs`). Each of those still builds and
//! appends its own trailing badge after this - the badge contents
//! genuinely differ (`hue.rs`'s/`system_sq.rs`'s are text-only pills,
//! `network_sq.rs`'s/`network_m.rs`'s carry a VPN icon+label) - but the
//! icon/title/spacer part, including the exact same header-alignment fix
//! (`set_valign(Center)` on the icon and title, since the row's own
//! default `Fill` alignment doesn't reliably center a bare-text label
//! and a padded badge pill the same way), was hand-built identically 4
//! times. That duplication is exactly why it was fixed independently 3
//! times rather than once, and why one of those 4 copies
//! (`network_m.rs`) missed a *different* fix (decoupling the title's
//! font size from `content_scale`) that its siblings got - see that
//! file's own history. Audit finding 2026-09-29.

/// Builds the header row up to (not including) the badge: an icon, a
/// title label, and a spacer that pushes whatever the caller appends
/// next (their own badge) to the far right. `spacing` and
/// `title_css_class` are still per-widget (hue.rs's header uses 8px
/// spacing where the other three use 6px - preserved rather than
/// silently unified, since that wasn't part of this audit finding) -
/// callers set the icon's paintable/pixel-size and the title's initial
/// text themselves, since those differ in both content and timing
/// (some set a fixed icon upfront, others wait for the first `refresh()`
/// to know which icon applies).
pub fn build_card_header(spacing: i32, title_css_class: &str) -> (gtk::Box, gtk::Image, gtk::Label) {
    use gtk::prelude::*;

    let header = gtk::Box::new(gtk::Orientation::Horizontal, spacing);

    let icon = gtk::Image::new();
    icon.set_valign(gtk::Align::Center);
    header.append(&icon);

    let title = gtk::Label::new(None);
    title.add_css_class(title_css_class);
    title.set_halign(gtk::Align::Start);
    title.set_valign(gtk::Align::Center);
    header.append(&title);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    (header, icon, title)
}
