//! The gallery's messages: what the app and the widgets send it, what it
//! sends back, and the page request it hands the cache.

use crate::models::{GalleryItem, GallerySort};

use super::GalleryAccount;

/// One page's worth of question for the cache: everything the database needs to
/// narrow and order the archive before cutting the page out of it.
#[derive(Debug, Clone)]
pub(crate) struct GalleryRequest {
    /// `(account id, folder path)` for every folder in scope.
    pub(crate) folders: Vec<(u32, String)>,
    pub(crate) account_id: Option<u32>,
    pub(crate) tokens: Vec<String>,
    pub(crate) bucket: u32,
    pub(crate) sort: GallerySort,
    pub(crate) offset: u32,
    pub(crate) limit: u32,
}

/// One attachment in the gallery: account, folder, message UID, file name.
pub(crate) type FileKey = (u32, String, u32, String);

pub(super) fn key_of(item: &GalleryItem) -> FileKey {
    (
        item.account_id,
        item.folder_path.clone(),
        item.uid,
        item.name.clone(),
    )
}

#[derive(Debug)]
pub(crate) enum GalleryInput {
    /// A page came back from the cache: `offset` 0 replaces the list, anything
    /// else appends. `total` is how many the query matches altogether.
    Page {
        items: Vec<GalleryItem>,
        total: u32,
        offset: u32,
    },
    SetLoading(bool),
    /// The scroll came near the end — ask for the next page if there is one.
    LoadMore,
    /// The scan described more attachments. Refreshes the view only while the
    /// user is still on the first page, so a scan landing mid-scroll cannot
    /// pull the ground out from under them.
    ScanProgress,
    /// How many messages the scan still has to describe, across every folder in
    /// scope; 0 once the archive is fully indexed.
    ScanStatus(u32),
    /// A file the gallery knew of but had not downloaded is being fetched, so
    /// the lightbox can show a spinner instead of an empty frame.
    SetFetching(bool),
    /// A message's attachments arrived: fill in the bytes for every loaded row
    /// of that message, so its thumbnail and preview appear in place.
    Fetched {
        account_id: u32,
        path: String,
        uid: u32,
        items: Vec<crate::models::Attachment>,
    },
    /// Filter the grid to items matching this search text (sender, subject,
    /// filename, folder, and type keywords like "pdf" or "spreadsheet").
    /// Debounced — the query itself runs on [`GalleryInput::ApplyQuery`].
    SetQuery(String),
    /// The typing settled — run the search.
    ApplyQuery,
    /// Re-sort the grid; the value is the sort dropdown's selected row index.
    SetSort(u32),
    /// A table column header was clicked: sort by that column, or flip its
    /// direction when it is already the active column (0 name, 1 sender,
    /// 2 date, 3 size, 4 type).
    SortColumn(u8),
    /// Switch between the thumbnail grid and the table.
    SetViewTable(bool),
    /// The footer size slider moved (grid thumbnail width, px).
    SetThumbWidth(f64),
    /// The size slider settled — rebuild the grid at the new width.
    ApplyThumbWidth,
    /// Show only one type bucket (the footer type dropdown's row; 0 = all).
    SetTypeFilter(u32),
    /// The accounts and folders the gallery may draw on, sent by the app when
    /// the gallery opens. Rebuilds the folder list and the account dropdown.
    SetAccounts(Vec<GalleryAccount>),
    /// Show only one account (the footer account dropdown's row; 0 = all).
    SetAccountFilter(u32),
    /// Master switch: pull from Archive folders.
    SetIncludeArchive(bool),
    /// Master switch: pull from folders that are neither Inbox nor Archive.
    SetIncludeOther(bool),
    /// One folder was ticked or unticked in the folder list.
    SetFolder {
        account_id: u32,
        path: String,
        on: bool,
    },
    /// A grid cell was activated (single click) — open the lightbox on that item.
    Activate(u32),
    Prev,
    Next,
    ClosePreview,
    /// Open the current (previewed) item's file in its default application.
    OpenCurrent,
    /// Jump to the current (previewed) item's source message.
    GoToCurrent,
    /// Open item `index` externally (double-click / context menu / lightbox).
    OpenItem(usize),
    /// Show item `index` in the file previewer (#625).
    PreviewItem(usize),
    /// Save item `index` to a file the user picks.
    DownloadItem(usize),
    /// Jump to item `index`'s source message.
    GoToItem(usize),
    /// Remove item `index` from its message on the server (#289).
    DeleteItem(usize),
    /// A file is being deleted from the server (`true`), or the server
    /// refused (`false`).
    SetDeleting(FileKey, bool),
    /// An attachment was removed from a message on the server: its item
    /// fades out and leaves the view (the next load reads the cache as it
    /// is now).
    Removed(FileKey),
    /// The fade is over: take the item away.
    RemoveNow(FileKey),
    /// Showcase only (HYLKI_SHOWCASE_DELETE_ATTACHMENT=gallery:<name>):
    /// Delete from Server… on the first file of that name.
    ShowcaseDelete(String),
    /// A cell was double-clicked: open it externally, closing any preview.
    OpenExternal(usize),
    /// Right-click on cell `index` at `(x, y)` (cell-relative) — show its menu there.
    ContextMenu {
        index: usize,
        x: f64,
        y: f64,
    },
    /// A lightbox-size PDF render finished (keyed by content hash) — show it
    /// if that PDF is still the one being previewed.
    PreviewRendered(u64),
    /// Showcase only (HYLKI_SHOWCASE_GALLERY_FOLDERS): drop the footer's
    /// folder popover open so a capture can see it.
    ShowcaseFolders,
    /// Showcase only (HYLKI_SHOWCASE_GALLERY_SEARCH): focus the search box and
    /// put text in it, as typing would, then report whether the box still has
    /// the focus once the search has run. Guards the bug where a search that
    /// matched nothing hid the toolbar the box lives in.
    ShowcaseSearch(String),
}

#[derive(Debug)]
pub(crate) enum GalleryOutput {
    /// Open the source message of an attachment in the reader.
    OpenMessage {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },
    /// Run this query against the cache and send the page back.
    Load(GalleryRequest),
    /// Fetch an attachment whose bytes were never downloaded, so it can be
    /// opened or previewed.
    Fetch {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },
    /// Take an attachment out of its message on the server (#289); the app
    /// asks first.
    DeleteFromServer {
        account_id: u32,
        folder_path: String,
        uid: u32,
        name: String,
        size: u64,
    },
}
