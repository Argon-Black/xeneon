// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared "which interface drives this display" settings block - an
//! Auto/pin dropdown plus a rename-when-manual entry. Audit finding
//! 2026-09-29 (quality review finding 3): hand-rolled near-identically
//! across all 5 network-widget-family modules (`network.rs`,
//! `network_s.rs`, `network_sx.rs`, `network_sq.rs`, `network_m.rs`) -
//! ~70-90 lines each, differing only in the concrete state type.

use crate::i18n_runtime as i18n;
use crate::widgets::is_manual;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// What `build` needs from a widget's own state to drive the picker -
/// implemented by every caller, just delegating to its own existing
/// fields/methods (each network widget already has these).
pub trait InterfacePickable {
    fn interface_name(&self) -> Option<String>;
    fn available_interfaces(&self) -> Vec<String>;
    fn custom_label(&self) -> Option<String>;
    fn set_interface(&self, name: Option<String>);
    fn set_custom_label(&self, label: Option<String>);
}

/// The picker's own widgets, for the caller to `root.append()` in order
/// alongside whatever else its own settings panel needs (a direction
/// toggle, color pickers, ...), plus `refresh` to resync the dropdown's
/// selection/model and the entry's sensitivity from current state -
/// called once here already, and again by the caller after a language
/// change or an appearance-popover reset.
pub struct InterfacePicker {
    pub interface_label: gtk::Label,
    pub interface_dropdown: gtk::DropDown,
    pub custom_label_label: gtk::Label,
    pub custom_label_entry: gtk::Entry,
    pub refresh: Rc<dyn Fn()>,
}

pub fn build<T: InterfacePickable + 'static>(state: Rc<T>) -> InterfacePicker {
    let interface_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.interface")));
    interface_label.set_halign(gtk::Align::Start);

    // `None` at index 0 always stands for "Auto"; the rest come from
    // whatever's currently up, plus the pinned interface if for some
    // reason it's not already in that list (e.g. a saved config carried
    // over from a different machine, or an interface that's since
    // disappeared).
    let mut initial_entries: Vec<Option<String>> = vec![None];
    let current_interface = state.interface_name();
    for name in state.available_interfaces() {
        if !initial_entries.iter().any(|e| e.as_deref() == Some(name.as_str())) {
            initial_entries.push(Some(name));
        }
    }
    if let Some(name) = &current_interface {
        if !initial_entries.iter().any(|e| e.as_deref() == Some(name.as_str())) {
            initial_entries.push(Some(name.clone()));
        }
    }
    let entries = Rc::new(RefCell::new(initial_entries));

    let interface_dropdown = gtk::DropDown::new(Some(gtk::StringList::new(&[])), gtk::Expression::NONE);
    interface_dropdown.set_hexpand(true);

    // Kept insensitive rather than hidden while on Auto, so its position
    // in the popover doesn't jump around when switching back and forth.
    // Set directly (not left to the caller's own `i18n::on_change`
    // listener, which only fires on a *later* language change) so the
    // label reads correctly on first open, not just after switching
    // languages once.
    let custom_label_label = gtk::Label::new(Some(&i18n::t("widgets.network.settings.custom_label")));
    custom_label_label.set_halign(gtk::Align::Start);
    let custom_label_entry = gtk::Entry::new();
    custom_label_entry.set_text(state.custom_label().as_deref().unwrap_or(""));

    // Rebuilds the dropdown's translated option names + selection, and
    // the custom-label entry's sensitivity, from current state.
    let refresh: Rc<dyn Fn()> = {
        let entries = entries.clone();
        let state = state.clone();
        let interface_dropdown = interface_dropdown.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        Rc::new(move || {
            let entries_ref = entries.borrow();
            let names: Vec<String> = entries_ref
                .iter()
                .map(|entry| match entry {
                    None => i18n::t("widgets.network.settings.interface_auto"),
                    Some(name) => name.clone(),
                })
                .collect();
            let current = state.interface_name();
            let selected_index = entries_ref.iter().position(|e| *e == current).unwrap_or(0);
            let manual = is_manual(&entries_ref, selected_index);
            drop(entries_ref);

            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            interface_dropdown.set_model(Some(&gtk::StringList::new(&name_refs)));
            interface_dropdown.set_selected(selected_index as u32);
            custom_label_label.set_sensitive(manual);
            custom_label_entry.set_sensitive(manual);
        })
    };
    refresh();

    interface_dropdown.connect_selected_notify({
        let state = state.clone();
        let entries = entries.clone();
        let custom_label_label = custom_label_label.clone();
        let custom_label_entry = custom_label_entry.clone();
        move |dropdown| {
            let index = dropdown.selected() as usize;
            let entries_ref = entries.borrow();
            if let Some(entry) = entries_ref.get(index) {
                state.set_interface(entry.clone());
            }
            let manual = is_manual(&entries_ref, index);
            drop(entries_ref);
            custom_label_label.set_sensitive(manual);
            custom_label_entry.set_sensitive(manual);
        }
    });
    custom_label_entry.connect_changed({
        let state = state.clone();
        move |entry| state.set_custom_label(Some(entry.text().to_string()))
    });

    InterfacePicker { interface_label, interface_dropdown, custom_label_label, custom_label_entry, refresh }
}
