//! What the user does with the files: open, save, save all, the context menu,
//! the lightbox, and the cells rebuilt to match.

use super::cells::{build_cell, build_list_row, for_previewer, previewable};
use super::{AttachmentDrawer, AttachmentDrawerInput, DrawerOutput};
use crate::i18n::i18n;
use crate::models::Attachment;
use crate::ui::attachment_io::{open_bytes, save_attachment_as};
use crate::ui::attachments_gallery::fade_out;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use adw::prelude::*;
use relm4::prelude::*;

impl AttachmentDrawer {
    /// The top-level window this drawer lives in (for modal children / dialogs).
    pub(super) fn window(&self) -> Option<gtk::Window> {
        self.flow.root().and_downcast::<gtk::Window>()
    }

    /// Rebuild the thumbnail grid from the current items and thumbnail size.
    pub(super) fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        // Remove existing cells only, skipping anything that isn't a
        // FlowBoxChild — and walk siblings captured before removal so this can
        // never loop (removing a non-child is a no-op, which would spin
        // `first_child()` forever).
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::FlowBoxChild>().is_some() {
                self.flow.remove(&c);
            }
            child = next;
        }
        // The grid keeps the message's own attachment order; the list is
        // alphabetical (case-insensitive), flipped by the sort switch. Cells are
        // handed their *display* index — `item_at` maps it back to `items`.
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        if self.list_view {
            order.sort_by(|&a, &b| {
                self.items[a]
                    .name
                    .to_lowercase()
                    .cmp(&self.items[b].name.to_lowercase())
            });
            if self.sort_desc {
                order.reverse();
            }
        }
        self.display_order = order;
        for (i, &orig) in self.display_order.iter().enumerate() {
            let att = &self.items[orig];
            let deleting = self
                .deleting
                .iter()
                .any(|(n, l)| *n == att.name && *l == att.data.len());
            let cell = if self.list_view {
                build_list_row(i, att, deleting, sender)
            } else {
                build_cell(i, att, self.thumb, deleting, sender)
            };
            self.flow.insert(&cell, -1);
        }
    }

    /// Rebuild for a changed mark, unless a fade is running: its cell has
    /// to stay put until it is gone, and the rebuild comes after it.
    pub(super) fn rebuild_unless_fading(&mut self, sender: &ComponentSender<Self>) {
        if self.fading == 0 {
            self.rebuild(sender);
        }
    }

    /// The attachment shown at display position `i` (grid or sorted list).
    pub(super) fn item_at(&self, i: usize) -> Option<&Attachment> {
        self.items.get(*self.display_order.get(i)?)
    }

    /// Save one attachment via a file chooser; a failed write is dropped.
    pub(super) fn save_attachment(&self, att: &Attachment) {
        save_attachment_as(&att.name, att.data.clone(), self.window().as_ref(), |_| {});
    }

    /// Ask for a folder and write every attachment into it ("Save All").
    pub(super) fn save_all(&self) {
        if self.items.is_empty() {
            return;
        }
        let atts = self.items.clone();
        let dialog = gtk::FileDialog::builder()
            .title(i18n("Save All Attachments"))
            .build();
        dialog.select_folder(
            self.window().as_ref(),
            gtk::gio::Cancellable::NONE,
            move |res| {
                if let Ok(folder) = res {
                    if let Some(dir) = folder.path() {
                        for att in &atts {
                            let safe = att.name.replace(['/', '\\'], "_");
                            let _ = std::fs::write(dir.join(&safe), &att.data);
                        }
                    }
                }
            },
        );
    }

    /// Right-click menu: Open / Download, Show in Message, and Delete from
    /// Server, matching the gallery's actions.
    pub(super) fn show_context_menu(
        &self,
        index: usize,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) {
        let s = sender.clone();
        let open = MenuEntry::new(i18n("Open"), move || {
            s.input_sender().emit(AttachmentDrawerInput::Open(index))
        })
        .icon("document-open-symbolic");
        let s = sender.clone();
        let download = MenuEntry::new(i18n("Download…"), move || {
            s.input_sender()
                .emit(AttachmentDrawerInput::Download(index))
        })
        .icon("folder-download-symbolic");
        let mut first = vec![open, download];
        if self
            .item_at(index)
            .is_some_and(|a| previewable(a) || crate::desktop::previewer::available())
        {
            let s = sender.clone();
            let preview = MenuEntry::new(i18n("Preview"), move || {
                // A menu button's click is not an event it hands on.
                s.input_sender().emit(AttachmentDrawerInput::Preview(
                    index,
                    gtk::gdk::CURRENT_TIME,
                ))
            })
            .icon("view-reveal-symbolic")
            .accel("space");
            first.insert(0, preview);
        }
        // The drawer gathers the whole conversation's files; this finds the
        // message a file came with (#213).
        let s = sender.clone();
        let show = MenuEntry::new(i18n("Show in Message"), move || {
            s.input_sender()
                .emit(AttachmentDrawerInput::ShowInMessage(index))
        })
        .icon("mail-unread-symbolic");
        let s = sender.clone();
        let delete = MenuEntry::new(i18n("Delete from Server…"), move || {
            s.input_sender()
                .emit(AttachmentDrawerInput::DeleteFromServer(index))
        })
        .icon("user-trash-symbolic");

        // Anchor on the clicked cell itself so the click point (already
        // relative to it) needs no coordinate translation.
        let parent: gtk::Widget = self
            .flow
            .child_at_index(index as i32)
            .map(|c| c.upcast())
            .unwrap_or_else(|| self.flow.clone().upcast());
        show_context_menu(&parent, x, y, vec![first, vec![show], vec![delete]]);
    }

    /// Ask the app to show its full-window lightbox over the message's
    /// previewable attachments (images and PDFs), starting at `start`
    /// (attachments order). The overlay lives at the window level so it fills
    /// Hylki itself — a separate preview window meant double chrome.
    fn show_lightbox(&self, start: usize, sender: &ComponentSender<Self>) {
        let items: Vec<Attachment> = self
            .items
            .iter()
            .filter(|a| previewable(a))
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let start_pos = self
            .items
            .iter()
            .take(start + 1)
            .filter(|a| previewable(a))
            .count()
            .saturating_sub(1);
        let _ = sender.output(DrawerOutput::ShowLightbox {
            items,
            start: start_pos,
        });
    }
}

impl AttachmentDrawer {
    /// Import an attached OpenPGP key and say how it went.
    pub(super) fn import_key(&mut self, i: usize) {
        if let Some(att) = self.item_at(i) {
            // gpg on a worker thread: a stale keyring lock makes it
            // wait about ten seconds (#316).
            let data = att.data.clone();
            let window = self.window();
            gtk::glib::spawn_future_local(async move {
                let r = gtk::gio::spawn_blocking(move || {
                    crate::mail::pgp::import_keys(&crate::mail::pgp::Gpg::system(), &data)
                })
                .await
                .unwrap_or_else(|_| Err("task failed".into()));
                let (title, body) = match r {
                    Ok(s) if s.imported > 0 => (
                        i18n("Key imported"),
                        i18n("The key is in your keyring now. Trust it under Settings, OpenPGP, once you have checked its fingerprint with its owner."),
                    ),
                    Ok(_) => (i18n("Already in your keyring"), i18n("That key was imported before.")),
                    Err(e) => (i18n("Could not import the key"), e),
                };
                crate::ui::dialog::alert(window.as_ref(), &title, &body);
            });
        }
    }

    /// The server deleted a file: fade it out.
    pub(super) fn deleted(&mut self, name: String, len: usize, sender: &ComponentSender<Self>) {
        if !self.deleting.contains(&(name.clone(), len)) {
            return;
        }
        let shown = (0..self.display_order.len())
            .find(|&i| {
                self.item_at(i)
                    .is_some_and(|a| a.name == name && a.data.len() == len)
            })
            .and_then(|i| self.flow.child_at_index(i as i32));
        self.fading += 1;
        let s = sender.clone();
        let done = move || {
            s.input_sender()
                .emit(AttachmentDrawerInput::FadeDone(name.clone(), len))
        };
        match shown {
            Some(child) => fade_out(&child, done),
            None => done(),
        }
    }

    /// A deleted file's fade finished: take it away.
    pub(super) fn fade_done(&mut self, name: String, len: usize, sender: &ComponentSender<Self>) {
        self.fading = self.fading.saturating_sub(1);
        self.deleting.retain(|d| *d != (name.clone(), len));
        if let Some(at) = self
            .items
            .iter()
            .position(|a| a.name == name && a.data.len() == len)
        {
            self.items.remove(at);
        }
        if self.fading == 0 {
            let items = self
                .pending_items
                .take()
                .unwrap_or_else(|| self.items.clone());
            self.apply_items(items, sender);
        }
    }

    /// Space, or Preview in the menu: images and PDFs in the lightbox,
    /// attached messages in a window, anything else in the file previewer, with the arrow keys there
    /// stepping through the drawer's files and its highlight following.
    /// Without a previewer it is a double click.
    pub(super) fn preview(&mut self, i: usize, time: u32, sender: &ComponentSender<Self>) {
        let Some(att) = self.item_at(i) else {
            return;
        };
        if !for_previewer(att) {
            self.activate(i, sender);
            return;
        }
        let files: Vec<Attachment> = (0..self.display_order.len())
            .filter_map(|d| self.item_at(d).cloned())
            .collect();
        let flow = self.flow.downgrade();
        let follow = move |at: usize| {
            if let Some(flow) = flow.upgrade() {
                if let Some(child) = flow.child_at_index(at as i32) {
                    flow.select_child(&child);
                }
            }
        };
        crate::desktop::previewer::show(
            files,
            i,
            self.window().as_ref(),
            time,
            Some(std::rc::Rc::new(follow)),
        );
    }

    /// A double click, or Return on a file.
    pub(super) fn activate(&mut self, i: usize, sender: &ComponentSender<Self>) {
        // Single clicks do nothing at all (a first click must never
        // steal the second — a modal lightbox on click one made the
        // double-click unreachable). Double-click previews images and
        // PDFs in the lightbox, whose Open button launches the default
        // app; anything else opens externally at once.
        let Some(orig) = self.display_order.get(i).copied() else {
            return;
        };
        let Some(att) = self.items.get(orig) else {
            return;
        };
        if previewable(att) {
            self.show_lightbox(orig, sender);
        } else {
            open_bytes(&att.name, &att.data, self.window().as_ref());
        }
    }
}
