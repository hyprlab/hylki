//! Every window, page and widget below the root component: the reader, the
//! message list, the composer, settings, the sidebar, contacts, the gallery,
//! and the small widgets they share.

pub(crate) mod accounts;
pub(crate) mod attachment_drawer;
pub(crate) mod attachment_io;
pub(crate) mod attachments_gallery;
pub(crate) mod carry_over;
mod chip_flow;
mod cloud_accounts;
mod column_bin;
pub(crate) mod command_palette;
pub(crate) mod compose;
mod contact_sources;
mod contact_tree;
pub(crate) mod contacts_browser;
pub(crate) mod contacts_page;
pub(crate) mod context_menu;
pub(crate) mod dialog;
pub(crate) mod drag_card;
pub(crate) mod drop_zones;
pub(crate) mod emoji;
mod fade_label;
pub(crate) mod folder_picker;
pub(crate) mod grab_pill;
mod icon_picker;
pub(crate) mod initials;
pub(crate) mod keymap;
pub(crate) mod later;
pub(crate) mod launch;
pub(crate) mod lightbox;
pub(crate) mod message_actions;
pub(crate) mod message_list;
pub(crate) mod message_row;
pub(crate) mod message_view;
pub(crate) mod message_window;
pub(crate) mod notifications;
mod pgp_keys;
pub(crate) mod preferences;
pub(crate) mod print_preview;
mod recipient_row;
pub(crate) mod rich_editor;
pub(crate) mod send_morph;
mod sender_row;
pub(crate) mod showcase;
pub(crate) mod sidebar;
mod theme_picker;
pub(crate) mod thumbnails;
mod tool_flow;
mod translation_page;
mod web_signin;
pub(crate) mod welcome;

/// How long Focus Mode's parts take to slide and fade away (and back), in
/// milliseconds: the reader toolbar, the list header, the sidebar's
/// accounts and the list's avatars all move on this one clock.
pub(crate) const FOCUS_ANIM_MS: u32 = 320;

/// Give a Refresh button its face for the network's state: orange, with a
/// tooltip that says why, while the network is down (#442). The header's, the
/// peek's and the rail's buttons all wear it.
pub(crate) fn show_refresh_offline(button: &gtk::Button, offline: bool) {
    use gtk::prelude::*;
    let tip = if offline {
        crate::i18n::i18n(
            "Offline: there is no network connection. Mail syncs again once the network is back. Click to try now, or long-press for Status Bar",
        )
    } else {
        crate::i18n::i18n("Refresh or long-press for Status Bar")
    };
    button.set_tooltip_text(Some(tip.as_str()));
    if offline {
        button.add_css_class("refresh-offline");
    } else {
        button.remove_css_class("refresh-offline");
    }
}

/// Ctrl+W for a window of its own (Settings, a message, a composer). The
/// application's Ctrl+W reaches only windows added to it, and these are not:
/// added, they would take the main window's other keys too (zoom, Focus
/// Mode). Capture phase, so a web view with the focus cannot swallow it; it
/// runs the window's close, as its titlebar button does.
pub(crate) fn close_key() -> gtk::ShortcutController {
    use gtk::prelude::*;
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Global);
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>w"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    shortcuts
}

/// Run `close` when `entry` lost focus, was left empty, and focus did not
/// move to another widget of its own row (the scope dropdown, a button, the
/// popover a dropdown opens). The check waits for an idle turn because the
/// new focus widget is not set yet while the leave event runs.
fn close_if_focus_left_row(entry: &gtk::SearchEntry, close: impl Fn() + 'static) {
    use gtk::prelude::*;
    if !entry.text().trim().is_empty() {
        return;
    }
    let entry = entry.clone();
    gtk::glib::idle_add_local_once(move || {
        let Some(row) = entry.parent() else { return };
        let Some(root) = entry.root() else { return };
        let focus = root.focus();
        let entry_widget: &gtk::Widget = entry.upcast_ref();
        let in_row = focus.is_some_and(|f| f != *entry_widget && (f == row || f.is_ancestor(&row)));
        if !in_row {
            close();
        }
    });
}

/// A style provider for the whole display that is loaded only when what it
/// holds changes. Loading one restyles every widget in every window, the
/// message list's rows included, and with hundreds of rows that takes longer
/// than the work the reload was for (#323).
pub(crate) struct DisplayCss {
    provider: gtk::CssProvider,
    css: std::cell::RefCell<String>,
}

impl DisplayCss {
    pub(crate) fn new() -> Self {
        let provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        DisplayCss {
            provider,
            css: Default::default(),
        }
    }

    pub(crate) fn load(&self, css: String) {
        if *self.css.borrow() != css {
            self.provider.load_from_string(&css);
            *self.css.borrow_mut() = css;
        }
    }
}

/// Ctrl + scroll wheel as zoom, the way browsers have it: `step` gets 1 for
/// a step in (wheel up) and -1 for a step out. Without Ctrl, or while
/// `zooms` says no, the scroll goes on to the page. Capture phase, so a
/// WebView underneath never scrolls the same notch it zooms.
///
/// A touchpad reports pixels in small bites, so those add up to a step
/// rather than each being one: otherwise a short swipe ran the whole scale.
fn ctrl_scroll_zoom(
    zooms: impl Fn() -> bool + 'static,
    step: impl Fn(i8) + 'static,
) -> gtk::EventControllerScroll {
    use gtk::prelude::*;
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pending = std::rc::Rc::new(std::cell::Cell::new(0.0f64));
    let p = pending.clone();
    scroll.connect_scroll_begin(move |_| p.set(0.0));
    scroll.connect_scroll(move |c, _, dy| {
        if !c
            .current_event_state()
            .contains(gtk::gdk::ModifierType::CONTROL_MASK)
            || !zooms()
        {
            pending.set(0.0);
            return gtk::glib::Propagation::Proceed;
        }
        let per_step = match c.unit() {
            gtk::gdk::ScrollUnit::Wheel => 1.0,
            _ => 40.0,
        };
        let mut sum = pending.get();
        // Turning back starts afresh rather than first undoing what was owed.
        if sum * dy < 0.0 {
            sum = 0.0;
        }
        sum += dy;
        while sum.abs() >= per_step {
            step(if sum < 0.0 { 1 } else { -1 });
            sum -= per_step.copysign(sum);
        }
        pending.set(sum);
        gtk::glib::Propagation::Stop
    });
    scroll
}
