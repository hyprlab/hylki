//! The showcase's own messages, which stage what a timer cannot reach from
//! outside the model.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::model::AppModel;
use crate::app::msg::AppMsg;
use crate::models::FolderKind;
use crate::ui::attachments_gallery::GalleryInput;
use crate::ui::compose::ComposeInput;
use crate::ui::preferences::PrefInput;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(in crate::app) fn on_showcase(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowcasePopout => {
                if let Some(m) = self.current.clone() {
                    let thread = self.current_thread.clone();
                    sender.input(AppMsg::OpenMessageWindow { message: m, thread });
                }
            }

            AppMsg::ShowcaseFolder { kind, account } => {
                let account = account.unwrap_or_else(|| self.active_account());
                let found = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == kind))
                    .map(|f| (f.id, f.name.clone(), f.path.clone()));
                if let Some((id, name, path)) = found {
                    self.select_folder(account, id, name, path);
                }
            }

            AppMsg::ShowcaseNotify => {
                let account = self.active_account();
                let inbox = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Inbox))
                    .map(|f| f.id);
                let newest = inbox.and_then(|f| {
                    self.message_cache
                        .get(&(account, f))
                        .and_then(|ms| ms.iter().max_by_key(|m| m.timestamp))
                        .map(|m| {
                            (
                                f,
                                m.id,
                                m.from_name.clone(),
                                m.shown_from_addr().to_string(),
                                m.subject.clone(),
                            )
                        })
                });
                match newest {
                    Some((folder, id, from, addr, subject)) => {
                        self.announce_new_mail(
                            account, folder, id, &from, &addr, &subject, 0, true,
                        );
                    }
                    None => tracing::warn!(
                        "showcase notify: no Inbox message cached for account {account}"
                    ),
                }
            }
            AppMsg::ShowcaseDeleteAttachment { name, confirmed } => {
                if let Some(name) = name.strip_prefix("gallery:") {
                    self.showcase_confirm_delete = confirmed;
                    self.gallery
                        .emit(GalleryInput::ShowcaseDelete(name.to_string()));
                    return;
                }
                match self.attachment_target(&name, None) {
                    Some(target) if confirmed => sender.input(AppMsg::DeleteAttachment(target)),
                    Some(target) => sender.input(AppMsg::AskDeleteAttachment(target)),
                    None => tracing::warn!("showcase: no attachment called {name:?} on screen"),
                }
            }
            AppMsg::ShowcaseToolbarGap { zone, index } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ToolbarGapPreview { zone, index });
                }
            }
            AppMsg::ShowcaseComposeMenu { format } => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(if format {
                        ComposeInput::FormatMenu
                    } else {
                        ComposeInput::OverflowMenu
                    });
                }
            }
            AppMsg::ShowcaseComposePreview => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::TogglePreview(true));
                }
            }
            AppMsg::ShowcaseComposeFrom(n) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseFrom(n));
                }
            }
            AppMsg::ShowcaseForward { account_id, id } => {
                let found = self
                    .current_thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned()
                    .or_else(|| {
                        self.message_cache
                            .values()
                            .flatten()
                            .find(|m| m.account_id == account_id && m.id == id)
                            .cloned()
                    });
                match found {
                    Some(m) => self.forward(m, true, &sender),
                    None => tracing::warn!("showcase forward: {account_id}:{id} is not loaded"),
                }
            }
            AppMsg::ShowcaseComposeUndo => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseHistory(0));
                }
            }
            AppMsg::ShowcaseBurger => {
                if let Some(m) = self.sidebar_menu.as_ref() {
                    // Focus first: a click on a MenuButton focuses it before
                    // the popover opens, and popup() alone skips that — the
                    // difference between the two is a bug's hiding place.
                    m.grab_focus();
                    m.popup();
                    let mut chain = Vec::new();
                    let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
                    while let Some(w) = node {
                        chain.push(format!("{}", w.type_()));
                        node = w.parent();
                    }
                    tracing::info!(
                        target: "hylki::compose::undo",
                        "burger: opened, focus now [{}]",
                        chain.join(" < ")
                    );
                }
            }
            AppMsg::ShowcaseComposeFormat(format) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::SetFormat(format));
                }
            }

            AppMsg::ShowcaseComposeClose => {
                if let Some(r) = &self.reader_compose {
                    r.controller.emit(ComposeInput::Cancel);
                }
            }

            AppMsg::ShowcaseCopy => {
                let Some(current) = self.current.clone() else {
                    return;
                };
                let Some(p) = self.provisionals.values().next() else {
                    return;
                };
                let mut copy = p.message.clone();
                copy.uid = 9_999;
                copy.message_id = "showcase-copy@hylki.local".into();
                tracing::info!(target: "hylki::morph", "showcase: the copy of {} arrives", p.message.id);
                self.merge_related(current.account_id, current.id, vec![copy]);
                self.show_thread();
                // The window once the copy has painted, beside the morph's
                // own last frame, to hold the two against each other.
                if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
                    let window: gtk::Widget = self.window.clone().upcast();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(1200),
                        move || {
                            crate::ui::send_morph::capture_next_frame(
                                &window.clone(),
                                move |still| {
                                    if let Some((tex, _)) = still {
                                        let _ = tex.save_to_png(
                                            std::path::Path::new(&dir).join("zz-copy.png"),
                                        );
                                    }
                                },
                            );
                        },
                    );
                }
            }

            AppMsg::ShowcaseSend(text) => self.showcase_send(text),

            AppMsg::ShowcaseNewAccount(provider) => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::AddAccount);
                    if let Some(label) = provider {
                        acc.emit(crate::ui::accounts::AccountsInput::DebugProvider(label));
                    }
                }
            }
            AppMsg::ShowcaseDirtyEditor => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::DebugEditLabel(
                        "Edited in the capture".into(),
                    ));
                }
            }

            AppMsg::ShowcaseEditFilter(i) => {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                }
            }
            _ => unreachable!("not routed to on_showcase"),
        }
    }

    /// Showcase only (HYLKI_SHOWCASE_SEND): write `text` in the split
    /// reply's editor and send it, to watch the send morph.
    fn showcase_send(&self, text: String) {
        // Every frame interval for a second and a half, to compare
        // the send morph with the slide it replaces.
        if let Some(clock) = self.window.frame_clock() {
            let last = std::rc::Rc::new(std::cell::Cell::new(0i64));
            let id = clock.connect_after_paint(move |c| {
                let now = c.frame_time();
                if last.get() > 0 {
                    tracing::debug!(target: "hylki::frames", "frame {} ms", (now - last.get()) / 1000);
                }
                last.set(now);
            });
            let c2 = clock.clone();
            let cell = std::rc::Rc::new(std::cell::Cell::new(Some(id)));
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(2500), move || {
                if let Some(id) = cell.take() {
                    c2.disconnect(id);
                }
            });
        }
        let Some(rc) = self.reader_compose.as_ref().map(|r| &r.controller) else {
            return;
        };
        let editor = crate::ui::send_morph::find_widget(rc.widget().upcast_ref(), &|w| {
            w.is::<webkit6::WebView>()
        })
        .and_downcast::<webkit6::WebView>();
        let Some(editor) = editor else { return };
        let js = format!(
            "document.body.focus();{}",
            text.split('\n')
                .map(|l| format!(
                    "document.execCommand('insertText',false,{});",
                    serde_json::to_string(l).unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join("document.execCommand('insertParagraph');")
        );
        webkit6::prelude::WebViewExt::evaluate_javascript(
            &editor,
            &js,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
        let c = rc.sender().clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            c.emit(ComposeInput::Send);
        });
        // The whole window every quarter second for six seconds from
        // Send, to see what the reader does while the message goes.
        if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
            let window: gtk::Widget = self.window.clone().upcast();
            for i in 0..24u64 {
                let (dir, window) = (dir.clone(), window.clone());
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600 + i * 250),
                    move || {
                        crate::ui::send_morph::capture_next_frame(&window.clone(), move |still| {
                            if let Some((tex, _)) = still {
                                let _ = tex.save_to_png(
                                    std::path::Path::new(&dir)
                                        .join(format!("win-{:05}.png", i * 250)),
                                );
                            }
                        });
                    },
                );
            }
        }
    }
}
