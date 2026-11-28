//! The attachments gallery: listing every attachment across folders.

use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::FolderKind;
use crate::ui::attachments_gallery::{GalleryAccount, GalleryFolder, GalleryInput};
use crate::ui::message_list::MessageListInput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;
use super::AppModelWidgets;

/// Largest cached attachment whose bytes ride along with a gallery page, so a
/// thumbnail or preview is instant. Anything bigger is fetched when opened.
const GALLERY_DATA_CAP: i64 = 6 * 1024 * 1024;

impl AppModel {
    /// Set the attachment scan going over every folder in the gallery's scope,
    /// newest mail first. One chunk per folder per pass; each answer asks for
    /// the next, so the archive fills in behind the user while they browse and
    /// stops as soon as a folder is fully described.
    fn start_attachment_scan(&self) {
        let folders: Vec<(u32, String)> = self
            .gallery_scope()
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .map(|f| (a.id, f.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (account_id, folder_path) in folders {
            self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
        }
    }

    /// The accounts and folders the attachments gallery may draw on: every
    /// folder the cache's gallery query can return, which is all of them bar
    /// Drafts, Junk and Trash. The folders are already in sidebar order.
    fn gallery_scope(&self) -> Vec<GalleryAccount> {
        self.accounts
            .iter()
            .map(|a| GalleryAccount {
                id: a.id,
                label: a.label.clone(),
                folders: self
                    .folders
                    .get(&a.id)
                    .map(|fs| {
                        fs.iter()
                            .filter(|f| {
                                !matches!(
                                    f.kind,
                                    FolderKind::Drafts
                                        | FolderKind::Templates
                                        | FolderKind::Junk
                                        | FolderKind::Trash
                                )
                            })
                            .map(|f| GalleryFolder {
                                path: f.path.clone(),
                                name: f.name.clone(),
                                kind: f.kind,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Leave the attachments gallery, dropping its data. The gallery loads
    /// every item's bytes eagerly (up to 300 × 6 MiB per account) and reloads
    /// from scratch on each visit anyway, so keeping two copies of that around
    /// for the rest of the session bought nothing but memory (issue #106).
    pub(super) fn leave_gallery(&mut self) {
        if std::mem::take(&mut self.showing_gallery) {
            self.gallery_total = 0;
            self.gallery.emit(GalleryInput::Page {
                items: Vec::new(),
                total: 0,
                offset: 0,
            });
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_gallery(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::ShowAttachments => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Attachments);
                self.showing_outbox = false;
                self.showing_gallery = true;
                self.showing_contacts = false;
                self.showing_calendar = false;
                self.gallery.emit(GalleryInput::SetLoading(true));
                // Handing over the folder list is what starts the load: the
                // gallery asks for its first page once it knows the scope, so
                // the query is never run against an empty one.
                self.gallery
                    .emit(GalleryInput::SetAccounts(self.gallery_scope()));
                // Keep the scan working through whatever is still unindexed, so
                // the archive fills in behind the user as they browse.
                self.start_attachment_scan();
            }

            AppMsg::GalleryFetch {
                account_id,
                folder_path,
                uid,
            } => {
                // The gallery knows this file exists because the scan described
                // it, but its bytes were never downloaded. Reuse the reader's
                // own fetch: it stores what comes back, so the next visit finds
                // it cached and the thumbnail appears.
                self.send_to(
                    account_id,
                    MailRequest::LoadAttachments {
                        message_id: uid,
                        path: folder_path,
                        uid,
                        download: true,
                    },
                );
                self.gallery.emit(GalleryInput::SetFetching(true));
            }

            AppMsg::AttachmentsScanned {
                account_id,
                folder_path,
                added,
                remaining,
            } => {
                let folder_path_key = folder_path.clone();
                // Keep going while this folder has more to describe. The reply
                // to each chunk is what asks for the next, so the scan runs at
                // whatever pace the server answers and never piles up requests.
                if remaining > 0 && self.showing_gallery {
                    self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
                }
                // Only refresh when the scan actually found something, and only
                // while the gallery is on screen — a chunk of mail with no
                // attachments after all should not reshuffle what's on show.
                if added > 0 && self.showing_gallery {
                    self.gallery.emit(GalleryInput::ScanProgress);
                }
                self.gallery_scan_left
                    .insert((account_id, folder_path_key), remaining);
                let left: u32 = self.gallery_scan_left.values().sum();
                self.gallery.emit(GalleryInput::ScanStatus(left));
            }

            AppMsg::GalleryLoad(req) => {
                let Some(cache) = self.cache.as_ref() else {
                    self.gallery.emit(GalleryInput::Page {
                        items: Vec::new(),
                        total: 0,
                        offset: req.offset,
                    });
                    return;
                };
                let q = crate::cache::GalleryQuery {
                    folders: &req.folders,
                    account_id: req.account_id,
                    tokens: &req.tokens,
                    bucket: req.bucket,
                    sort: req.sort,
                    limit: req.limit,
                    offset: req.offset,
                    data_cap: GALLERY_DATA_CAP,
                };
                let items = cache.gallery_page(&q);
                // Only worth counting for the first page: the total cannot
                // change while the user scrolls one set of results.
                let total = if req.offset == 0 {
                    cache.gallery_total(&q)
                } else {
                    self.gallery_total
                };
                self.gallery_total = total;
                self.gallery.emit(GalleryInput::Page {
                    items,
                    total,
                    offset: req.offset,
                });
            }

            AppMsg::OpenAttachmentMessage {
                account_id,
                folder_path,
                uid,
            } => {
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_calendar = false;
                self.showing_outbox = false;
                if let Some(folder) = self.folder_by_path(account_id, &folder_path).cloned() {
                    self.select_folder(
                        account_id,
                        folder.id,
                        folder.name.clone(),
                        folder.path.clone(),
                    );
                    // Messages use their UID as id, so select by (account, uid).
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, uid)));
                }
            }
            _ => unreachable!("not routed to on_gallery"),
        }
    }

    /// The attachments gallery is the content stack's second page. Wrap it in a
    /// ToolbarView + HeaderBar so it keeps the window controls (close/minimize)
    /// that otherwise live only on the reader pane's header.
    pub(super) fn init_gallery_page(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        use gtk::prelude::*;
        let gallery_tv = adw::ToolbarView::new();
        let gallery_hb = adw::HeaderBar::new();
        gallery_hb.add_css_class("flat");
        let title = gtk::Label::new(Some(i18n("Attachments").as_str()));
        title.add_css_class("pane-title");
        gallery_hb.set_title_widget(Some(&title));
        // Leftmost, same spot as the message list header's: the sidebar
        // collapse/expand toggle.
        let sidebar_btn = gtk::Button::from_icon_name("sidebar-show-symbolic");
        sidebar_btn.set_tooltip_text(Some(i18n("Toggle sidebar").as_str()));
        sidebar_btn.add_css_class("flat");
        let s = sender.input_sender().clone();
        sidebar_btn.connect_clicked(move |_| {
            let _ = s.send(AppMsg::ToggleSidebar);
        });
        gallery_hb.pack_start(&sidebar_btn);
        gallery_tv.add_top_bar(&gallery_hb);
        gallery_tv.set_content(Some(self.gallery.widget()));
        widgets
            .content_stack
            .add_named(&gallery_tv, Some("gallery"));

        // The contacts view brings its own per-pane headers (so its
        // divider runs to the very top, like the mail panes').
        widgets
            .content_stack
            .add_named(self.contacts_page.widget(), Some("contacts"));
        widgets
            .content_stack
            .add_named(self.calendar_page.widget(), Some("calendar"));
        let mut headers = Vec::new();
        for root in [gallery_tv.upcast_ref::<gtk::Widget>(), self.contacts_page.widget().upcast_ref()] {
            super::calendar::window_control_headers(root, &mut headers);
        }
        let _ = self.controls_headers.set(headers);
    }
}
