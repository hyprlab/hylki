//! Settings → System helpers: the GNOME Files extension, downloaded mail,
//! links and error reports.

use super::*;

/// What the Links group says above the combo — which depends on whether
/// there are any browsers to name.
pub(super) fn link_group_description(browsers: &[crate::ui::launch::Browser]) -> String {
    if browsers.is_empty() {
        i18n(
            "Inside the Flatpak sandbox, links open through the desktop's chooser. \"Ask each time\" picks the browser per link with the option to remember it.",
        )
    } else {
        i18n("\"Ask each time\" lets the desktop ask before each link.")
    }
}

/// The Files extension row's subtitle: where it stands, and where it lives.
pub(super) fn nautilus_status_text(state: &crate::desktop::nautilus_ext::State) -> String {
    use crate::desktop::nautilus_ext::Status;
    let state_text = match state.status {
        Status::NotInstalled => i18n("Not installed"),
        Status::Installed if state.loaded => i18n("Installed and loaded by Files"),
        Status::Installed if state.loader == Some(false) => {
            i18n("Installed, but nautilus-python is missing")
        }
        Status::Installed => i18n("Installed. Restart Files to load it; it needs nautilus-python"),
        Status::Outdated => i18n("Installed, but from another version"),
    };
    let state = state_text;
    match crate::desktop::nautilus_ext::path() {
        Some(p) => {
            let shown = p.display().to_string();
            let shown = match std::env::var("HOME") {
                Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
                    format!("~{}", &shown[home.len()..])
                }
                _ => shown,
            };
            format!("{state} — {shown}")
        }
        None => state,
    }
}

/// Hide the "editable" pencil an `adw::EntryRow` draws whatever its
/// `editable` says (the icon carries the `edit-icon` style class).
pub(super) fn hide_edit_icon(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(c) = child {
        if c.has_css_class("edit-icon") {
            c.set_visible(false);
        } else {
            hide_edit_icon(&c);
        }
        child = c.next_sibling();
    }
}

/// What the Downloaded mail row says.
pub(super) fn storage_text(storage: Option<(u64, u64, u64)>) -> String {
    match storage {
        None => i18n("Measuring…"),
        Some((bodies, files, total)) => i18n_f(
            "Messages {bodies}, attachments {files}. {total} in all, with the message list.",
            &[
                ("bodies", gtk::glib::format_size(bodies).as_str()),
                ("files", gtk::glib::format_size(files).as_str()),
                ("total", gtk::glib::format_size(total).as_str()),
            ],
        ),
    }
}

/// A plain error alert over the settings window.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
fn report(parent: &adw::Window, heading: &str, body: &str) {
    let dialog = crate::ui::dialog::message_dialog(Some(parent), Some(heading), Some(body));
    dialog.add_response("ok", &i18n("OK"));
    dialog.set_close_response("ok");
    dialog.present();
}

impl Preferences {
    pub(super) fn on_change_renderer(&mut self, i: u32, sender: ComponentSender<Self>) {
        if let Some(r) = RENDERERS.get(i as usize) {
            let _ = sender.output(PrefOutput::SetRenderer((*r).to_string()));
        }
    }

    pub(super) fn on_change_language(&mut self, i: u32, sender: ComponentSender<Self>) {
        if let Some((_, code)) = language_choices().get(i as usize) {
            let _ = sender.output(PrefOutput::SetLanguage(code.clone()));
        }
    }

    pub(super) fn on_nautilus_install(&mut self, root: &adw::Window) {
        if let Err(e) = crate::desktop::nautilus_ext::install() {
            report(root, &i18n("Could not install the extension"), &e);
        }
        self.nautilus = crate::desktop::nautilus_ext::State::read();
    }

    pub(super) fn on_measure_storage(&mut self, sender: ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        gtk::glib::spawn_future_local(async move {
            let measured = gtk::gio::spawn_blocking(|| {
                let (bodies, files) = crate::cache::Cache::open()
                    .map(|c| c.downloaded_size())
                    .unwrap_or((0, 0));
                (bodies, files, crate::cache::Cache::file_size())
            })
            .await;
            if let Ok((bodies, files, total)) = measured {
                let _ = s.send(PrefInput::StorageMeasured {
                    bodies,
                    files,
                    total,
                });
            }
        });
    }

    pub(super) fn on_storage_measured(&mut self, bodies: u64, files: u64, total: u64) {
        self.storage = Some((bodies, files, total))
    }

    pub(super) fn on_remove_downloads(
        &mut self,
        sender: ComponentSender<Self>,
        root: &adw::Window,
    ) {
        let dialog = adw::AlertDialog::new(
            Some(&i18n("Remove downloaded mail?")),
            Some(&i18n("Mail and attachments on this computer are removed and fetched again when opened. Nothing on the server changes.")),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("remove", &i18n("Remove"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let s = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "remove" {
                let _ = s.send(PrefInput::RemoveDownloadsNow);
            }
        });
        dialog.present(Some(root));
    }

    pub(super) fn on_remove_downloads_now(&mut self, sender: ComponentSender<Self>) {
        self.storage = None;
        let s = sender.input_sender().clone();
        gtk::glib::spawn_future_local(async move {
            let _ = gtk::gio::spawn_blocking(|| {
                match crate::cache::Cache::open().and_then(|c| {
                    let n = c.remove_downloaded(None)?;
                    let _ = c.compact();
                    Ok(n)
                }) {
                    Ok(n) => tracing::info!("storage: removed {n} downloaded message(s)"),
                    Err(e) => {
                        tracing::warn!("storage: could not remove downloaded mail: {e}")
                    }
                }
            })
            .await;
            let _ = s.send(PrefInput::MeasureStorage);
        });
    }

    pub(super) fn on_change_keep_downloads(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let days = KEEP_DOWNLOADS_DAYS.get(idx as usize).copied().unwrap_or(0);
        let _ = sender.output(PrefOutput::SetKeepDownloads(days));
    }

    pub(super) fn on_nautilus_remove(&mut self, root: &adw::Window) {
        if let Err(e) = crate::desktop::nautilus_ext::remove() {
            report(root, &i18n("Could not remove the extension"), &e);
        }
        self.nautilus = crate::desktop::nautilus_ext::State::read();
    }

    pub(super) fn on_change_files_action(&mut self, idx: u32, sender: ComponentSender<Self>) {
        use crate::config::FilesAction as A;
        let action = match idx {
            1 => A::New,
            2 => A::Draft,
            3 => A::Reply,
            _ => A::Ask,
        };
        if self.files.action != action {
            self.files.action = action;
            let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
        }
    }

    pub(super) fn on_change_files_large(&mut self, idx: u32, sender: ComponentSender<Self>) {
        use crate::config::FilesLarge as L;
        let large = match idx {
            1 => L::Attach,
            2 => L::Cloud,
            _ => L::Ask,
        };
        if self.files.large != large {
            self.files.large = large;
            let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
        }
    }

    pub(super) fn on_change_link_browser(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let choice = match idx {
            0 => String::new(),
            1 => crate::ui::launch::ASK.to_string(),
            n => self
                .browsers
                .get(n as usize - 2)
                .map(|b| b.id.clone())
                .unwrap_or_default(),
        };
        let _ = sender.output(PrefOutput::SetLinkBrowser(choice));
    }

    pub(super) fn on_change_files_limit(&mut self, mb: u32, sender: ComponentSender<Self>) {
        let mb = mb.max(1);
        if self.files.limit_mb != mb {
            self.files.limit_mb = mb;
            let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
        }
    }

    pub(super) fn on_set_files_prefs(&mut self, p: crate::config::FilesPrefs) {
        self.files = p;
        if let Some((action, large, limit)) = self.files_rows.as_ref() {
            if action.selected() != files_action_index(p.action) {
                action.set_selected(files_action_index(p.action));
            }
            if large.selected() != files_large_index(p.large) {
                large.set_selected(files_large_index(p.large));
            }
            if limit.value().round() as u32 != p.limit_mb {
                limit.set_value(p.limit_mb as f64);
            }
        }
    }

    pub(super) fn on_nautilus_restart_files(&mut self, root: &adw::Window) {
        if let Err(e) = crate::desktop::nautilus_ext::quit_files() {
            report(root, &i18n("Could not restart Files"), &e);
        }
        self.nautilus = crate::desktop::nautilus_ext::State::read();
    }

    pub(super) fn on_change_tray_icon(&mut self, index: u32, sender: ComponentSender<Self>) {
        let icon = TRAY_ICONS
            .get(index as usize)
            .map(|(_, t)| *t)
            .unwrap_or_default();
        let _ = sender.output(PrefOutput::SetTrayIcon(icon));
    }

    pub(super) fn on_change_launcher_badge(&mut self, index: u32, sender: ComponentSender<Self>) {
        let badge = LAUNCHER_BADGES
            .get(index as usize)
            .map(|(_, b)| *b)
            .unwrap_or_default();
        let _ = sender.output(PrefOutput::SetLauncherBadge(badge));
    }
}

#[cfg(test)]
mod tests {
    use super::super::choices::{KEEP_DOWNLOADS_DAYS, RENDERERS};
    use super::*;

    #[test]
    fn system_page_choices() {
        assert_eq!(RENDERERS, ["", "vulkan", "gl", "cairo"]);
        assert_eq!(KEEP_DOWNLOADS_DAYS, [0, 365, 90, 30]);
        assert_eq!(storage_text(None), "Measuring…");
        assert_eq!(
            storage_text(Some((1000, 2000, 3000))),
            "Messages 1.0\u{a0}kB, attachments 2.0\u{a0}kB. 3.0\u{a0}kB in all, with the message list."
        );
    }
}
