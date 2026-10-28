//! The main window's remembered layout: sidebar.toml (account order,
//! open sections, folder order, favorites) and window.toml (size).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{config_file, default_on, read_toml, save_plain_toml, save_private_toml};

/// Sidebar state persisted across restarts, as sidebar.toml holds it.
///
/// A missing file gives `Default` (every section closed), while a file
/// without the section keys opens the ones marked `default_on`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct SidebarState {
    /// Account emails in the user's preferred display order.
    #[serde(default)]
    pub(crate) order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    #[serde(default)]
    pub(crate) collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    #[serde(default)]
    pub(crate) folders_expanded: Vec<String>,
    /// Whether the whole sidebar is in icon-only (collapsed) mode.
    #[serde(default)]
    pub(crate) icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    #[serde(default)]
    pub(crate) tree_collapsed: Vec<String>,
    /// Folders the user has put in an order of their own, as "email\tpath"
    /// entries: a folder sorts among its siblings by where it stands here.
    #[serde(default)]
    pub(crate) folder_order: Vec<String>,
    /// The three sections' open state; open when the file predates them,
    /// which is how they always started.
    #[serde(default = "default_on")]
    pub(crate) unified_expanded: bool,
    #[serde(default = "default_on")]
    pub(crate) filtered_expanded: bool,
    #[serde(default = "default_on")]
    pub(crate) tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists; closed
    /// until opened.
    #[serde(default)]
    pub(crate) starred_expanded: bool,
    #[serde(default)]
    pub(crate) sent_expanded: bool,
    #[serde(default)]
    pub(crate) drafts_expanded: bool,
    #[serde(default)]
    pub(crate) archive_expanded: bool,
    #[serde(default)]
    pub(crate) junk_expanded: bool,
    #[serde(default)]
    pub(crate) trash_expanded: bool,
    #[serde(default)]
    pub(crate) snoozed_expanded: bool,
    /// Folders picked as favorites (#367), as "email\tpath" entries in the
    /// order they were added, and whether their list is open.
    #[serde(default)]
    pub(crate) favorites: Vec<String>,
    #[serde(default = "default_on")]
    pub(crate) favorites_expanded: bool,
    /// Whether the Smart Folders list (#417) is open.
    #[serde(default = "default_on")]
    pub(crate) smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    #[serde(default)]
    pub(crate) contacts_expanded: bool,
    /// The accounts and books folded under the Contacts row, by key.
    #[serde(default)]
    pub(crate) contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open
    /// (closed by default, like their custom folders).
    #[serde(default)]
    pub(crate) filtered_expanded_accounts: Vec<String>,
    #[serde(default)]
    pub(crate) tags_expanded_accounts: Vec<String>,
}

/// sidebar.toml's own name for its shape, used by the settings bundle.
pub(super) type SidebarFile = SidebarState;

pub(super) fn sidebar_path() -> Option<PathBuf> {
    config_file("sidebar.toml")
}

pub(crate) fn load_sidebar_state() -> SidebarState {
    read_toml::<SidebarState>(sidebar_path()).unwrap_or_default()
}

pub(crate) fn save_sidebar_state(state: &SidebarState) {
    save_private_toml(sidebar_path(), state, "sidebar state", true);
}

// ---------------------------------------------------------------------------
// Window state (size + maximized). Position/monitor can't be persisted on
// Wayland — the compositor owns window placement.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct WindowFile {
    width: i32,
    height: i32,
    #[serde(default)]
    maximized: bool,
}

impl Default for WindowFile {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 840,
            maximized: false,
        }
    }
}

pub(super) fn window_path() -> Option<PathBuf> {
    config_file("window.toml")
}

/// Returns the saved `(width, height, maximized)`, or sensible defaults.
pub(crate) fn load_window_state() -> (i32, i32, bool) {
    let file = read_toml::<WindowFile>(window_path()).unwrap_or_default();
    // Guard against absurd/zero sizes from a bad file.
    let width = if file.width >= 360 { file.width } else { 1280 };
    let height = if file.height >= 300 { file.height } else { 840 };
    (width, height, file.maximized)
}

pub(crate) fn save_window_state(width: i32, height: i32, maximized: bool) {
    let file = WindowFile {
        width,
        height,
        maximized,
    };
    save_plain_toml(window_path(), &file);
}
