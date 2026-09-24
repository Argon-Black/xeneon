// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure application logic for Xeneon Dashboard, shared by the GTK/Relm4 UI
//! (`xeneon-app`) but with no GTK dependency of its own - everything here
//! is plain data and arithmetic, so it can be built and tested without a
//! display. Ported piece by piece from the Python app (`xeneon_dashboard/`)
//! on the `rust-gtk-port` branch; see the plan doc referenced in project
//! memory for the full mapping.

pub mod appearance;
pub mod assets;
pub mod config;
pub mod grid;
pub mod i18n;
pub mod page_state;
mod persistence;
pub mod widget_state;
