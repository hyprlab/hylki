//! The gallery's own state handling: paging through the cache, the
//! lightbox preview, rebuilding the grid or table, and what a click on an
//! item does.

use adw::prelude::*;
use relm4::ComponentSender;

use crate::i18n::i18n;
use crate::models::GalleryItem;
use crate::ui::attachment_io::save_attachment_as;
use crate::ui::context_menu::{show_context_menu, MenuEntry};

use super::{
    build_cell, build_row, is_pdf_name, key_of, lightbox_pdf_texture, open_bytes, texture_from,
    thumb_cache_key, AttachmentsGallery, GalleryInput, GalleryOutput, GalleryRequest, PAGE_SIZE,
    PDF_PREVIEWS,
};

impl AttachmentsGallery {
    /// What the gallery holds in RAM, for the memory section of an export:
    /// items listed, of which carrying their file bytes, and those bytes.
    pub(crate) fn memory_stats(&self) -> (usize, usize, u64) {
        let with_data = self.all_items.iter().filter(|i| i.data.is_some()).count();
        let bytes = self
            .all_items
            .iter()
            .filter_map(|i| i.data.as_ref())
            .map(|d| d.len() as u64)
            .sum();
        (self.all_items.len(), with_data, bytes)
    }

    pub(super) fn page(&self) -> &'static str {
        if self.loading && self.all_items.is_empty() {
            "loading"
        } else if !self.all_items.is_empty() {
            if self.view_table {
                "table"
            } else {
                "grid"
            }
        } else if self.is_narrowed() {
            // Nothing matched, but something would without the search or the
            // filters — a different message from an empty gallery.
            "noresults"
        } else {
            "empty"
        }
    }

    /// Whether the search bar and the footer are on show. They stay up while a
    /// search narrows the gallery to nothing: the entry being typed into lives
    /// in the toolbar, and a toolbar that hid itself the moment a query matched
    /// nothing would take the focus with it mid-word.
    pub(super) fn show_chrome(&self) -> bool {
        !self.all_items.is_empty() || self.is_narrowed() || self.scan_remaining > 0
    }

    /// Whether the user has narrowed the view at all. Used only to choose
    /// between the two empty states, so an archive with no attachments in it
    /// doesn't tell the user to try a different search they never made.
    fn is_narrowed(&self) -> bool {
        !self.query.trim().is_empty() || self.type_filter != 0 || self.account_filter.is_some()
    }

    /// Ask for the first page again: what every change to the scope, the
    /// search, the type filter or the sort has to do, because all four are
    /// applied by the database and not here.
    ///
    /// What is already on screen deliberately stays there until the
    /// replacement page arrives. Emptying the list first would take the
    /// toolbar and footer down with it — and the search entry lives in the
    /// toolbar, so a search would lose the focus of whoever was typing it
    /// after the first letter.
    pub(super) fn reload(&mut self, sender: &ComponentSender<Self>) {
        self.preview = None;
        self.preview_texture = None;
        self.loading = true;
        self.request_page(0, sender);
    }

    /// Ask for the next page, unless one is already on its way or the last page
    /// has already landed.
    pub(super) fn load_more(&mut self, sender: &ComponentSender<Self>) {
        if self.loading_more || !self.has_more() {
            return;
        }
        let offset = self.all_items.len() as u32;
        self.request_page(offset, sender);
    }

    fn has_more(&self) -> bool {
        (self.all_items.len() as u32) < self.total
    }

    /// The query as it stands, for the app to run against the cache.
    fn request_page(&mut self, offset: u32, sender: &ComponentSender<Self>) {
        self.loading_more = true;
        let folders: Vec<(u32, String)> = self
            .accounts
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .filter(|f| self.folder_included(a.id, &f.path))
                    .map(|f| (a.id, f.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let _ = sender.output(GalleryOutput::Load(GalleryRequest {
            folders,
            account_id: self.account_filter,
            tokens: self
                .query
                .split_whitespace()
                .map(|t| t.to_lowercase())
                .collect(),
            bucket: self.type_filter,
            sort: self.sort,
            offset,
            limit: PAGE_SIZE,
        }));
    }

    /// The `GalleryItem` at display position `display`. The database returns
    /// rows already filtered and in order, so position is a direct index.
    pub(super) fn item_at(&self, display: usize) -> Option<&GalleryItem> {
        self.all_items.get(display)
    }

    pub(super) fn current(&self) -> Option<&GalleryItem> {
        self.preview.and_then(|i| self.item_at(i))
    }

    pub(super) fn step(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        if self.all_items.is_empty() {
            return;
        }
        if let Some(i) = self.preview {
            let n = self.all_items.len() as i32;
            self.preview = Some((((i as i32 + delta) % n + n) % n) as usize);
            self.refresh_preview(sender);
        }
    }

    /// Work out what the lightbox shows for the current item: an image decodes
    /// on the spot; a PDF's first page comes from the full-size render cache,
    /// or a worker renders it now and [`GalleryInput::PreviewRendered`] circles
    /// back here. Anything else has no texture — the file-icon page shows.
    pub(super) fn refresh_preview(&mut self, sender: &ComponentSender<Self>) {
        self.preview_texture = None;
        self.preview_failed = false;
        let Some(item) = self.current() else { return };
        let Some(data) = item.data.as_ref() else {
            // Never downloaded (or too big to have ridden along with the page):
            // ask for it, and the spinner shows until `Fetched` circles back.
            if !self.fetching {
                self.fetch_item(item, sender);
            }
            return;
        };
        if item.is_image() {
            self.preview_texture = texture_from(data);
            return;
        }
        if !is_pdf_name(&item.name) {
            return;
        }
        let key = thumb_cache_key(data);
        match PDF_PREVIEWS.with(|c| c.borrow().get(&key).cloned()) {
            Some(texture) => {
                self.preview_failed = texture.is_none();
                self.preview_texture = texture;
            }
            None => {
                let s = sender.clone();
                lightbox_pdf_texture(data, move |_| {
                    s.input(GalleryInput::PreviewRendered(key));
                });
            }
        }
    }

    /// Repopulate whichever view is showing. The other keeps stale children;
    /// switching to it rebuilds it, so only one view's widgets are ever built
    /// for a given change.
    pub(super) fn rebuild_view(&mut self, sender: &ComponentSender<Self>) {
        if self.view_table {
            self.rebuild_table(sender);
        } else {
            self.rebuild_grid(sender);
        }
    }

    fn rebuild_grid(&mut self, sender: &ComponentSender<Self>) {
        // Remove existing cells; only FlowBoxChild children (not, say, a popover
        // that happens to be parented nearby).
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::FlowBoxChild>().is_some() {
                self.flow.remove(&c);
            }
            child = next;
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                let deleting = self.deleting.contains(&key_of(item));
                self.flow.append(&build_cell(
                    display,
                    item,
                    self.thumb_width,
                    deleting,
                    sender,
                ));
            }
        }
    }

    fn rebuild_table(&mut self, sender: &ComponentSender<Self>) {
        while let Some(row) = self.table.first_child() {
            self.table.remove(&row);
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                let deleting = self.deleting.contains(&key_of(item));
                self.table
                    .append(&build_row(display, item, deleting, sender));
            }
        }
    }

    /// Open item `index` in its default application. A file whose bytes were
    /// never downloaded — most of an archive — is fetched first, and opens when
    /// [`GalleryInput::Fetched`] brings it back.
    pub(super) fn open_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else {
            return;
        };
        match &item.data {
            Some(data) => {
                let parent = self.flow.root().and_downcast::<gtk::Window>();
                open_bytes(&item.name, data, parent.as_ref());
            }
            None => self.fetch_item(item, sender),
        }
    }

    /// Show item `index` in the file previewer, fetching it first as
    /// [`Self::open_item`] does.
    pub(super) fn preview_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else {
            return;
        };
        match &item.data {
            Some(data) => {
                let file = crate::models::Attachment {
                    name: item.name.clone(),
                    data: data.clone(),
                };
                let parent = self.flow.root().and_downcast::<gtk::Window>();
                crate::desktop::previewer::show(vec![file], 0, parent.as_ref(), None);
            }
            None => self.fetch_item(item, sender),
        }
    }

    /// Ask the app to download the message this item belongs to.
    fn fetch_item(&self, item: &GalleryItem, sender: &ComponentSender<Self>) {
        let _ = sender.output(GalleryOutput::Fetch {
            account_id: item.account_id,
            folder_path: item.folder_path.clone(),
            uid: item.uid,
        });
    }

    /// Save item `index` to a file the user chooses.
    pub(super) fn download_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else {
            return;
        };
        let Some(data) = item.data.clone() else {
            // Not in hand yet: fetch it, and the user can save it once the
            // thumbnail fills in.
            self.fetch_item(item, sender);
            return;
        };
        let parent = self.flow.root().and_downcast::<gtk::Window>();
        save_attachment_as(&item.name, data, parent.as_ref(), |e| {
            tracing::warn!("could not save attachment: {e}");
        });
    }

    pub(super) fn goto_item(&mut self, index: usize, sender: &ComponentSender<Self>) {
        if let Some(item) = self.item_at(index) {
            let _ = sender.output(GalleryOutput::OpenMessage {
                account_id: item.account_id,
                folder_path: item.folder_path.clone(),
                uid: item.uid,
            });
            self.preview = None;
        }
    }

    /// Pop up the right-click menu (Download / Open / Go to Message) at the click
    /// point `(x, y)` (relative to cell `index`). Download/Open are only enabled
    /// when the file's bytes are cached.
    pub(super) fn show_context_menu(
        &self,
        index: usize,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) {
        if self.item_at(index).is_none() {
            return;
        }

        // Both act on a file that was never downloaded too: they fetch it
        // first. Greying them out would be wrong now that most of an archive's
        // attachments are known but not held.
        let s = sender.clone();
        let open = MenuEntry::new(i18n("Open"), move || s.input(GalleryInput::OpenItem(index)))
            .icon("document-open-symbolic");
        let s = sender.clone();
        let download = MenuEntry::new(i18n("Download…"), move || {
            s.input(GalleryInput::DownloadItem(index))
        })
        .icon("folder-download-symbolic");
        let s = sender.clone();
        let goto = MenuEntry::new(i18n("Go to Message"), move || {
            s.input(GalleryInput::GoToItem(index))
        })
        .icon("mail-unread-symbolic");
        let s = sender.clone();
        let delete = MenuEntry::new(i18n("Delete from Server…"), move || {
            s.input(GalleryInput::DeleteItem(index))
        })
        .icon("user-trash-symbolic");
        let mut first = vec![open, download, goto];
        // Pictures and PDFs have the gallery's own preview, a click away.
        if self.item_at(index).is_some_and(|item| {
            !crate::models::is_image_name(&item.name) && !is_pdf_name(&item.name)
        }) && crate::desktop::previewer::available()
        {
            let s = sender.clone();
            let preview = MenuEntry::new(i18n("Preview"), move || {
                s.input(GalleryInput::PreviewItem(index))
            })
            .icon("view-reveal-symbolic");
            first.insert(0, preview);
        }
        let sections = vec![first, vec![delete]];

        // Anchor on the clicked cell/row itself so the click point (already
        // relative to it) needs no coordinate translation.
        let source: Option<gtk::Widget> = if self.view_table {
            self.table.row_at_index(index as i32).map(|r| r.upcast())
        } else {
            self.flow.child_at_index(index as i32).map(|c| c.upcast())
        };
        show_context_menu(source.as_ref().unwrap_or(&self.root), x, y, sections);
    }
}
