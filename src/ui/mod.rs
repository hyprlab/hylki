pub mod accounts;
pub mod attachment_drawer;
pub mod attachments_gallery;
pub mod carry_over;
pub mod chip_flow;
pub mod column_bin;
pub mod fade_label;
pub mod fade_scroll;
pub mod cloud_accounts;
pub mod translation_page;
pub mod compose;
pub mod contacts_browser;
pub mod contacts_page;
pub mod context_menu;
pub mod directories;
pub mod drop_zones;
pub mod emoji;
pub mod folder_picker;
pub mod grab_pill;
pub mod icon_picker;
pub mod initials;
pub mod launch;
pub mod lightbox;
pub mod message_list;
pub mod message_row;
pub mod message_view;
pub mod message_window;
pub mod notifications;
pub mod pgp_keys;
pub mod preferences;
pub mod print_preview;
pub mod rich_editor;
pub mod sidebar;
pub mod theme_picker;
pub mod web_signin;
pub mod welcome;

/// How long Focus Mode's parts take to slide and fade away (and back), in
/// milliseconds: the reader toolbar, the list header, the sidebar's
/// accounts and the list's avatars all move on this one clock.
pub const FOCUS_ANIM_MS: u32 = 320;

/// Run `close` when `entry` lost focus, was left empty, and focus did not
/// move to another widget of its own row (the scope dropdown, a button, the
/// popover a dropdown opens). The check waits for an idle turn because the
/// new focus widget is not set yet while the leave event runs.
pub fn close_if_focus_left_row(entry: &gtk::SearchEntry, close: impl Fn() + 'static) {
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
pub struct DisplayCss {
    provider: gtk::CssProvider,
    css: std::cell::RefCell<String>,
}

impl DisplayCss {
    pub fn new() -> Self {
        let provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        DisplayCss { provider, css: Default::default() }
    }

    pub fn load(&self, css: String) {
        if *self.css.borrow() != css {
            self.provider.load_from_string(&css);
            *self.css.borrow_mut() = css;
        }
    }
}
