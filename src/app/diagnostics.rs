//! Diagnostics: the memory report in the exported log.

use adw::prelude::*;
use relm4::prelude::*;

use crate::desktop::memory_report::{human_bytes, human_count, messages_bytes};
use crate::i18n::i18n;
use crate::ui::notifications::NotifyInput;

use super::model::AppModel;
use super::msg::AppMsg;

/// "N messages, size" for a count and the bytes they hold.
fn count((n, b): (usize, usize)) -> String {
    format!("{} messages, {}", human_count(n), human_bytes(b as u64))
}

/// The heading, uptime, the process tree and who holds the heap.
fn memory_processes(out: &mut String) {
    out.push_str("== Memory ==\n");
    if let Some(up) = crate::desktop::memory_report::uptime() {
        out.push_str(&format!(
            "Running for {}\n",
            crate::desktop::memory_report::human_duration(up)
        ));
    }
    out.push_str("Processes:\n");
    for line in crate::desktop::memory_report::process_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    for line in crate::desktop::memory_report::web_view_lines() {
        out.push_str(&line);
        out.push('\n');
    }
    let (rust_live, rust_peak) = crate::desktop::memory_report::rust_heap();
    let (sql_live, sql_peak) = crate::desktop::memory_report::sqlite_heap();
    out.push_str(&format!(
        "  of which Rust code holds {} (peak {}); SQLite holds {} (peak {}); the rest is GTK, WebKit, the graphics driver and other libraries\n",
        human_bytes(rust_live as u64),
        human_bytes(rust_peak as u64),
        human_bytes(sql_live),
        human_bytes(sql_peak)
    ));
}

impl AppModel {
    /// The memory section of an exported log: the process tree as the
    /// kernel sees it, then what the main process is holding — the mail
    /// index (whose size follows the mailbox, not the session) apart from the
    /// session caches (which follow use). Written so a "Hylki is using N GB"
    /// report answers itself: a big index on a big mailbox is the accepted
    /// cost of keeping everything in RAM; a big cache is something to fix.
    pub(super) fn memory_report(&self) -> String {
        let mut out = String::new();
        memory_processes(&mut out);
        self.memory_graphics(&mut out);
        self.memory_mail_index(&mut out);
        self.memory_session_caches(&mut out);
        out
    }

    /// What GTK draws with and which driver libraries are mapped.
    fn memory_graphics(&self, out: &mut String) {
        // What GTK draws with. A software renderer path (lavapipe, llvmpipe)
        // runs LLVM inside this process: hundreds of megabytes of heap on a
        // VM or a machine whose GPU driver GTK could not use.
        let renderer = gtk::prelude::NativeExt::renderer(&self.window)
            .map(|r| r.type_().name().to_string())
            .unwrap_or_else(|| "none yet".to_string());
        let libs = crate::desktop::memory_report::graphics_libraries();
        // Mesa's Vulkan loader maps every installed driver to enumerate the
        // devices, so lavapipe (and with it LLVM) shows up on a machine that
        // draws with its GPU too; software rendering is the verdict only when
        // no hardware driver is there beside it.
        let hardware = libs.iter().any(|l| {
            let l = l.as_str();
            (l.starts_with("libvulkan_")
                && !l.starts_with("libvulkan_lvp")
                && !l.starts_with("libvulkan_dzn"))
                || l.contains("nvidia")
                || (l.ends_with("_dri.so") && !l.contains("swrast"))
        });
        let software = libs.iter().any(|l| {
            l.starts_with("libvulkan_lvp") || l.starts_with("libLLVM") || l.contains("swrast")
        });
        out.push_str(&format!(
            "Graphics: {renderer}; driver libraries: {}{}\n",
            if libs.is_empty() { "none loaded".to_string() } else { libs.join(", ") },
            match (hardware, software) {
                (false, true) => " (SOFTWARE rendering: shaders compile through LLVM in this process)",
                (true, true) => " (hardware driver in use; the software fallback is mapped alongside it, as Mesa's loader does)",
                _ => "",
            }
        ));
    }

    /// The mail index: what follows the mailbox's size.
    fn memory_mail_index(&self, out: &mut String) {
        out.push_str("Mail index (follows the mailbox size, kept in RAM by design):\n");
        let folders = self.message_cache.len();
        let index = messages_bytes(self.message_cache.values().flatten());
        out.push_str(&format!(
            "  {} accounts, {} folders indexed: {}\n",
            self.accounts.len(),
            human_count(folders),
            count(index)
        ));
        let unified = messages_bytes(self.unified_slices.values().flatten());
        out.push_str(&format!(
            "  unified slices ({} folders): {}\n",
            human_count(self.unified_slices.len()),
            count(unified)
        ));
        let tags = messages_bytes(self.tag_view_cache.values().flatten());
        out.push_str(&format!(
            "  tag views ({}): {}\n",
            human_count(self.tag_view_cache.len()),
            count(tags)
        ));
        let [all, shown, pool] = self.message_list.model().memory_stats();
        out.push_str(&format!(
            "  list: folder {}; rows {}; search pool {}\n",
            count(all),
            count(shown),
            count(pool)
        ));
        let (views, kept, maps) = self.message_list.model().kept_view_stats();
        out.push_str(&format!(
            "  list views kept as built ({views} of {}): {}, conversations {}\n",
            crate::ui::message_list::KEPT_VIEWS,
            count(kept),
            human_bytes(maps as u64)
        ));
    }

    /// The session caches: what follows use.
    fn memory_session_caches(&self, out: &mut String) {
        out.push_str("Session caches (follow use; each is bounded unless noted):\n");
        out.push_str(&format!(
            "  bodies: {} entries, {} of {} budget\n",
            human_count(self.body_cache.len()),
            human_bytes(self.body_cache.bytes() as u64),
            human_bytes(self.body_cache.budget() as u64)
        ));
        out.push_str(&format!(
            "  attachments: {} entries, {} of {} budget\n",
            human_count(self.attachment_cache.len()),
            human_bytes(self.attachment_cache.bytes() as u64),
            human_bytes(self.attachment_cache.budget() as u64)
        ));
        let threads = messages_bytes(self.thread_cache.values().flatten());
        out.push_str(&format!(
            "  conversations: {} of {} kept, {}; open conversation {}\n",
            self.thread_cache.len(),
            Self::THREAD_CACHE_MAX,
            count(threads),
            count(messages_bytes(&self.current_thread))
        ));
        let undo = messages_bytes(
            self.undo_stack
                .iter()
                .chain(&self.redo_stack)
                .flat_map(|e| &e.rows),
        );
        out.push_str(&format!(
            "  undo/redo: {} entries, {}\n",
            human_count(self.undo_stack.len() + self.redo_stack.len()),
            count(undo)
        ));
        let carried = messages_bytes(self.carried_threads.values().flatten());
        let carried_bodies: usize = self.carried_bodies.values().map(String::len).sum();
        out.push_str(&format!(
            "  carried replies: {} threads ({}), {} bodies ({}) (unbounded)\n",
            human_count(self.carried_threads.len()),
            count(carried),
            human_count(self.carried_bodies.len()),
            human_bytes(carried_bodies as u64)
        ));
        out.push_str(&format!(
            "  sender checks: {} (unbounded); pop-outs: {}\n",
            human_count(self.sender_cache.len()),
            self.popouts.len()
        ));
        let (items, with_data, data_bytes) = self.gallery.model().memory_stats();
        let ((thumbs, thumbs_rendered, thumb_bytes), (pdfs, pdfs_rendered, pdf_bytes)) =
            crate::ui::thumbnails::cache_stats();
        out.push_str(&format!(
            "  gallery: {} items listed, {} with file bytes ({}); thumbnails {} ({} rendered, {} of pixels), PDF pages {} ({} rendered, {} of pixels) (unbounded)\n",
            human_count(items),
            human_count(with_data),
            human_bytes(data_bytes),
            human_count(thumbs),
            human_count(thumbs_rendered),
            human_bytes(thumb_bytes),
            human_count(pdfs),
            human_count(pdfs_rendered),
            human_bytes(pdf_bytes)
        ));
        let (logos, logo_bytes, logo_misses) = crate::services::logo::cache_stats();
        out.push_str(&format!(
            "  sender logos: {} ({} of pixels), {} domains without (unbounded)\n",
            human_count(logos),
            human_bytes(logo_bytes),
            human_count(logo_misses)
        ));
        let (contacts, contact_bytes, gravatars, gravatar_bytes) =
            crate::services::avatar::cache_stats();
        out.push_str(&format!(
            "  contact photos: {} ({} of pixels); gravatars: {} ({} of pixels) (unbounded)\n",
            human_count(contacts),
            human_bytes(contact_bytes),
            human_count(gravatars),
            human_bytes(gravatar_bytes)
        ));
        let (entries, entry_bytes, photo_bytes) = self.contacts_page.model().memory_stats();
        let (thumbs, thumb_bytes) = crate::services::avatar::photo_thumb_stats();
        out.push_str(&format!(
            "  Contacts list: {} entries, {} (of which photos {}); list photos {} ({} of pixels)\n",
            human_count(entries),
            human_bytes(entry_bytes as u64),
            human_bytes(photo_bytes as u64),
            human_count(thumbs),
            human_bytes(thumb_bytes)
        ));
        let (pictures, picture_bytes, circles, circle_bytes) = crate::ui::initials::cache_stats();
        out.push_str(&format!(
            "  account pictures: {} ({} of pixels); rendered circles: {} ({}) (unbounded)\n",
            human_count(pictures),
            human_bytes(picture_bytes),
            human_count(circles),
            human_bytes(circle_bytes as u64)
        ));
        let (lines, line_bytes) = crate::desktop::console_log::stats();
        out.push_str(&format!(
            "  console: {} lines, {}\n",
            human_count(lines),
            human_bytes(line_bytes as u64)
        ));
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_diagnostics(&self, msg: AppMsg) {
        match msg {
            AppMsg::OpenConsole => {
                if self.console_mode {
                    self.notifications.emit(NotifyInput::ShowConsole);
                } else {
                    self.notice(i18n(
                        "Enable Console mode in Settings → System & Appearance",
                    ));
                }
            }

            AppMsg::ExportLogTo(path) => {
                let text = crate::desktop::console_log::export_text(&self.memory_report());
                match crate::config::write_private(&path, &text) {
                    Ok(()) => tracing::info!("log exported to {}", path.display()),
                    Err(e) => tracing::warn!("log export to {} failed: {e}", path.display()),
                }
            }
            _ => unreachable!("not routed to on_diagnostics"),
        }
    }
}

/// Return freed heap to the system now and then. What the gallery,
/// the composer or a big conversation allocated and dropped otherwise
/// stays held by the allocator, and a system monitor counts it against
/// the app: a user's log showed 1.73 GB held with 270 MB live.
pub(super) fn start_heap_trim() {
    gtk::glib::timeout_add_seconds_local(30, || {
        if let Some(held) = crate::desktop::memory_report::trim_if_worthwhile() {
            tracing::debug!(target: "hylki::memory", "trimmed the heap: {} was freed but held", crate::desktop::memory_report::human_bytes(held as u64));
        }
        gtk::glib::ControlFlow::Continue
    });
}
