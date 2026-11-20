//! Cloud links: uploading a file too big to attach and putting its link in the
//! body.

use super::attachments::pick_files;
use super::messages::ComposeInput;
use super::types::CloudLink;
use super::{Compose, ComposeWidgets};
use crate::encoding::escape_html;
use crate::i18n::i18n;
use crate::i18n::i18n_f;
use crate::ui::dialog::alert;
use crate::ui::rich_editor::{js_escape, SourceKind};
use adw::prelude::*;
use relm4::prelude::*;

/// Which account to upload to, and how the links are made this time:
/// the expiry and the password, seeded from the account's settings and
/// changeable for this upload alone.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
fn cloud_upload_dialog(
    parent: Option<&gtk::Window>,
    accounts: &[crate::services::cloud::CloudAccount],
    paths: Vec<std::path::PathBuf>,
    sender: relm4::Sender<ComposeInput>,
) {
    if accounts.is_empty() {
        return;
    }
    let n = paths.len();
    let dialog = crate::ui::dialog::message_dialog(
        parent,
        Some(i18n("Upload to Cloud Storage").as_str()),
        Some(
            crate::i18n::ni18n_f(
                "Upload {n} file and put its share link in the message.",
                "Upload {n} files and put their share links in the message.",
                n as u32,
                &[("n", &n.to_string())],
            )
            .as_str(),
        ),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("upload", &i18n("Upload"));
    dialog.set_default_response(Some("upload"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("upload", adw::ResponseAppearance::Suggested);

    let names: Vec<String> = accounts
        .iter()
        .map(|a| {
            if a.name.trim().is_empty() {
                a.where_shown()
            } else {
                a.name.clone()
            }
        })
        .collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let combo = adw::ComboRow::new();
    combo.set_title(&i18n("Account"));
    combo.set_model(Some(&gtk::StringList::new(&name_refs)));
    combo.set_visible(accounts.len() > 1);

    // This upload's terms, starting from the account's own.
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely"));
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect with a password"));
    let password = adw::EntryRow::new();
    password.set_title(&i18n("Download password (empty: generated)"));
    let apply_defaults = {
        let (expire, protect, password) = (expire.clone(), protect.clone(), password.clone());
        let accounts = accounts.to_vec();
        move |i: usize| {
            if let Some(a) = accounts.get(i) {
                // What the service allows for this account: a row it
                // does not is greyed out, with the reason.
                expire.set_sensitive(a.expiry_allowed());
                expire.set_subtitle(&if a.expiry_allowed() {
                    i18n("Days; 0 keeps the link indefinitely")
                } else {
                    a.link_note.clone()
                });
                protect.set_sensitive(a.password_allowed());
                protect.set_subtitle(if a.password_allowed() {
                    ""
                } else {
                    a.link_note.as_str()
                });
                expire.set_value(if a.expiry_allowed() {
                    a.expire_days as f64
                } else {
                    0.0
                });
                protect.set_active(a.password_allowed() && a.password);
                password.set_text("");
            }
        }
    };
    apply_defaults(0);
    password.set_visible(protect.is_active());
    {
        let password = password.clone();
        protect.connect_active_notify(move |p| password.set_visible(p.is_active()));
    }
    {
        let apply_defaults = apply_defaults.clone();
        combo.connect_selected_notify(move |c| apply_defaults(c.selected() as usize));
    }

    let group = adw::PreferencesGroup::new();
    group.add(&combo);
    group.add(&expire);
    group.add(&protect);
    group.add(&password);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(400);
    bx.append(&group);
    dialog.set_extra_child(Some(&bx));

    let accounts = accounts.to_vec();
    let paths = std::cell::RefCell::new(Some(paths));
    dialog.connect_response(None, move |_, resp| {
        if resp != "upload" {
            return;
        }
        let (Some(paths), Some(account)) = (
            paths.borrow_mut().take(),
            accounts.get(combo.selected() as usize),
        ) else {
            return;
        };
        let mut account = account.clone();
        account.expire_days = expire.value() as u32;
        account.password = protect.is_active();
        let typed = password.text().trim().to_string();
        let link_password = (account.password && !typed.is_empty()).then_some(typed);
        let _ = sender.send(ComposeInput::CloudUpload {
            paths,
            account,
            link_password,
        });
    });
    dialog.present();
}

impl Compose {
    /// Handles [`ComposeInput::CloudAttach`].
    pub(super) fn on_cloud_attach(
        &mut self,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        let s = sender.input_sender().clone();
        pick_files(root, &i18n("Upload to Cloud Storage"), move |paths| {
            let _ = s.send(ComposeInput::CloudPicked(paths));
        });
    }

    /// Handles [`ComposeInput::CloudPicked`].
    pub(super) fn on_cloud_picked(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        let parent = root.root().and_downcast::<gtk::Window>();
        cloud_upload_dialog(
            parent.as_ref(),
            &self.cloud_accounts,
            paths,
            sender.input_sender().clone(),
        );
    }

    /// Handles [`ComposeInput::CloudUpload`].
    pub(super) fn on_cloud_upload(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        account: crate::services::cloud::CloudAccount,
        link_password: Option<String>,
        sender: &ComponentSender<Self>,
    ) {
        if !account.has_secret() {
            sender
                .input_sender()
                .emit(ComposeInput::CloudUploadSignedIn {
                    paths,
                    account,
                    link_password,
                    password: Some(String::new()),
                });
            return;
        }
        // Off the main thread: the keyring may wait on its user.
        let key = account.key();
        let s = sender.input_sender().clone();
        crate::config::keyring_then(
            move || crate::config::load_cloud_password(&key),
            move |password| {
                s.emit(ComposeInput::CloudUploadSignedIn {
                    paths,
                    account,
                    link_password,
                    password,
                })
            },
        );
    }

    /// Handles [`ComposeInput::CloudUploadSignedIn`].
    pub(super) fn on_cloud_upload_signed_in(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        account: crate::services::cloud::CloudAccount,
        link_password: Option<String>,
        password: String,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        for path in paths {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            self.cloud_busy += 1;
            self.rebuild_attachments(&widgets.attach_box, sender);
            let s = sender.input_sender().clone();
            let (a, pw, lp) = (account.clone(), password.clone(), link_password.clone());
            std::thread::spawn(move || {
                let result =
                    crate::services::cloud::upload_and_share(&a, &pw, &path, lp.as_deref());
                let _ = s.send(ComposeInput::CloudUploaded { name, result });
            });
        }
    }

    /// Handles [`ComposeInput::CloudUploaded`].
    pub(super) fn on_cloud_uploaded(
        &mut self,
        name: String,
        result: Result<crate::services::cloud::ShareResult, String>,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        self.cloud_busy = self.cloud_busy.saturating_sub(1);
        match result {
            Ok(share) => {
                let id = format!(
                    "hylki-cloud-{}",
                    crate::rng::token(8).unwrap_or_else(|_| share.size.to_string())
                );
                let mut caption = crate::models::human_size(share.size);
                if let Some(d) = &share.expires {
                    caption.push_str(&format!(
                        ", {}",
                        i18n_f("link expires {date}", &[("date", d)])
                    ));
                }
                if share.password.is_some() {
                    caption.push_str(&format!(", {}", i18n("password-protected")));
                }
                let html = format!(
                "<p id=\"{id}\" data-hylki-cloud=\"1\">\u{1F4CE} <a href=\"{url}\">{name}</a> ({caption})</p>",
                url = escape_html(&share.url),
                name = escape_html(&share.name),
                caption = escape_html(&caption),
            );
                // Into the body where the user's own text ends: above
                // the signature, and above a quoted original in a
                // reply, so the link reads as part of the message.
                if let Some(kind) = self.editor.source_kind() {
                    let line = match kind {
                        SourceKind::Markdown => format!(
                            "\u{1F4CE} [{name}]({url}) ({caption})",
                            name = share.name,
                            url = share.url,
                            caption = caption,
                        ),
                        SourceKind::Html => html.clone(),
                    };
                    // The same place, said in text: above the `-- `
                    // signature line if there is one, else at the end.
                    self.editor.run_js(&format!(
                        "(function(){{var t=document.getElementById('src');if(!t)return;\
                     var l='{}';var v=t.value;var i=v.lastIndexOf('\\n-- \\n');\
                     if(i>=0){{v=v.slice(0,i)+'\\n'+l+'\\n'+v.slice(i);}}\
                     else{{v=v.replace(/\\s*$/,'')+'\\n\\n'+l+'\\n';}}\
                     t.value=v;window.__hylkiDirty=true;}})()",
                        js_escape(&line)
                    ));
                    if let Some(p) = share.password.clone() {
                        self.cloud_passwords.push((share.name.clone(), p));
                    }
                    self.cloud_links.push(CloudLink {
                        id,
                        name: share.name,
                        url: share.url,
                    });
                    self.rebuild_attachments(&widgets.attach_box, sender);
                    return;
                }
                self.editor.run_js(&format!(
                "(function(){{var d=document.createElement('div');d.innerHTML='{}';\
                 var p=d.firstChild;var b=document.body;\
                 var first=null;var cands=b.querySelectorAll('.hylki-sig,.hylki-quote-attr,blockquote');\
                 for(var i=0;i<cands.length;i++){{var t=cands[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;\
                 if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}}\
                 if(first)b.insertBefore(p,first);else b.appendChild(p);\
                 window.__hylkiPlainFix&&window.__hylkiPlainFix();\
                 document.dispatchEvent(new Event('input'));}})()",
                js_escape(&html)
            ));
                if let Some(p) = share.password.clone() {
                    self.cloud_passwords.push((share.name.clone(), p));
                }
                self.cloud_links.push(CloudLink {
                    id,
                    name: share.name,
                    url: share.url,
                });
            }
            Err(e) => {
                let parent = root.root().and_downcast::<gtk::Window>();
                alert(
                    parent.as_ref(),
                    i18n_f("Could not upload {name}", &[("name", &name)]).as_str(),
                    &e,
                );
            }
        }
        self.rebuild_attachments(&widgets.attach_box, sender);
    }

    /// Handles [`ComposeInput::RemoveCloudLink`].
    pub(super) fn on_remove_cloud_link(
        &mut self,
        i: usize,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if i < self.cloud_links.len() {
            let link = self.cloud_links.remove(i);
            self.cloud_passwords.retain(|(n, _)| *n != link.name);
            // Source mode has no element to remove: the line that
            // names the link is what goes.
            if self.editor.source_kind().is_some() {
                self.editor.run_js(&format!(
                    "(function(){{var t=document.getElementById('src');if(!t)return;\
                 var u='{}';\
                 t.value=t.value.split('\\n').filter(function(l){{return l.indexOf(u)<0;}})\
                   .join('\\n');window.__hylkiDirty=true;}})()",
                    js_escape(&link.url)
                ));
                self.rebuild_attachments(&widgets.attach_box, sender);
                return;
            }
            self.editor.run_js(&format!(
            "(function(){{var p=document.getElementById('{}');if(p)p.remove();document.dispatchEvent(new Event('input'));}})()",
            js_escape(&link.id)
        ));
            self.rebuild_attachments(&widgets.attach_box, sender);
        }
    }

    /// Handles [`ComposeInput::CopyCloudPasswords`].
    pub(super) fn on_copy_cloud_passwords(&mut self) {
        let text = self
            .cloud_passwords
            .iter()
            .map(|(n, p)| format!("{n}: {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&text);
        }
    }
}

/// The cloud account's sign-in is missing from the keyring.
pub(super) fn not_signed_in(root: &adw::BreakpointBin, name: &str) {
    let parent = root.root().and_downcast::<gtk::Window>();
    alert(parent.as_ref(), i18n("Not signed in").as_str(), i18n_f("The sign-in for {name} is not in the keyring. Open Settings, Cloud Storage, and enter it again.", &[("name", name)]).as_str());
}
