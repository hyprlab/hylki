pub mod accounts;
pub mod attachment_drawer;
pub mod attachments_gallery;
pub mod carry_over;
pub mod chip_flow;
pub mod cloud_accounts;
pub mod compose;
pub mod contacts_browser;
pub mod contacts_page;
pub mod context_menu;
pub mod folder_picker;
pub mod grab_pill;
pub mod icon_picker;
pub mod initials;
pub mod launch;
pub mod message_list;
pub mod message_view;
pub mod message_window;
pub mod notifications;
pub mod pgp_keys;
pub mod preferences;
pub mod print_preview;
pub mod rich_editor;
pub mod sidebar;
pub mod theme_picker;
pub mod welcome;

/// How long Focus Mode's parts take to slide and fade away (and back), in
/// milliseconds: the reader toolbar, the list header, the sidebar's
/// accounts and the list's avatars all move on this one clock.
pub const FOCUS_ANIM_MS: u32 = 320;
