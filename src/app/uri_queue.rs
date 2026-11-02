//! Mail handed in from outside before or after the window exists: mailto: and
//! mid: links, .eml files and files to attach. Each is queued until the app
//! can take it, then sent on as an AppMsg.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::conversation::body_key;
use super::model::AppModel;
use super::AppMsg;

/// Hands `item` to the running component as `msg(item)`, or parks it in
/// `pending` until init drains it (see `MAILTO_PENDING`).
fn send_or_park<T>(pending: &std::sync::Mutex<Vec<T>>, item: T, msg: fn(T) -> AppMsg) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(msg(item));
        }
        None => parked(pending).push(item),
    }
}

/// One of the queues below, locked. Nothing that runs while one is held can
/// panic (a push, or a drain that sends each item on), so none is ever
/// poisoned.
fn parked<T>(queue: &std::sync::Mutex<Vec<T>>) -> std::sync::MutexGuard<'_, Vec<T>> {
    queue.lock().expect("a parked queue is never poisoned")
}

/// Route a `mailto:` URI to the app (from main's `connect_open`).
pub(crate) fn queue_mailto(uri: String) {
    send_or_park(&MAILTO_PENDING, uri, AppMsg::OpenMailto);
}

/// A `mailto:` handed to the app before its component was up. `queue_mailto`
/// runs on GApplication's `open` signal, which can fire before `AppModel::init`
/// installs the sender — anything early waits here and is drained by init.
static MAILTO_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

static MAILTO_SENDER: std::sync::OnceLock<relm4::Sender<AppMsg>> = std::sync::OnceLock::new();

/// Turn a `mailto:` URI into a composer prefill (RFC 6068: address part plus
/// to/cc/bcc/subject/body query keys, all percent-encoded).
pub(super) fn parse_mailto(uri: &str) -> Option<crate::ui::compose::ComposePrefill> {
    let rest = uri.strip_prefix("mailto:")?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    // Leading slashes aren't part of any address: Nautilus's "Send by email"
    // opens `mailto:///?attach=…` (#90), and sloppy generators write
    // `mailto://user@host` URL-style.
    let addr = addr.trim_start_matches('/');
    // `+` stays literal: plus-addressing (`user+tag@example.com`) is real.
    let mut to = crate::encoding::percent::decode(addr, false);
    let (mut cc, mut bcc, mut subject, mut body) =
        (String::new(), String::new(), String::new(), String::new());
    let mut attachments: Vec<std::path::PathBuf> = Vec::new();
    // The Email portal writes `attachment=` paths raw, unescaped (#339), so
    // an `&` inside a file name splits the query: a piece with no `=` after
    // an attachment is the rest of its path.
    let mut pairs: Vec<String> = Vec::new();
    for piece in query.split('&').filter(|p| !p.is_empty()) {
        let continues = !piece.contains('=')
            && pairs.last().is_some_and(|p| {
                let key = p
                    .split_once('=')
                    .map_or("", |(k, _)| k)
                    .to_ascii_lowercase();
                key == "attach" || key == "attachment"
            });
        match pairs.last_mut() {
            Some(last) if continues => {
                last.push('&');
                last.push_str(piece);
            }
            _ => pairs.push(piece.to_string()),
        }
    }
    for pair in &pairs {
        let (k, raw) = pair.split_once('=').unwrap_or((pair, ""));
        let v = crate::encoding::percent::decode(raw, false);
        match k.to_ascii_lowercase().as_str() {
            // A second `to` joins the address part, comma-separated.
            "to" if !v.is_empty() => {
                if !to.is_empty() {
                    to.push_str(", ");
                }
                to.push_str(&v);
            }
            "cc" => cc = v,
            "bcc" => bcc = v,
            "subject" => subject = v,
            "body" => body = v,
            // Nautilus's "Send by email" (and xdg-email) pass the files as
            // attach= parameters (#90), the Email portal as attachment=: an
            // absolute path or a file:// URI. Only absolute paths are
            // accepted; the caller re-checks that each names a real file
            // before attaching.
            "attach" | "attachment" if !v.is_empty() => {
                let path = if v.starts_with("file:") {
                    // A URI of its own: GIO undoes its escapes and any
                    // `localhost` host.
                    gtk::gio::File::for_uri(&v).path()
                } else {
                    Some(std::path::PathBuf::from(&v))
                };
                // A raw path with a `%` in its name decodes into another
                // name; when that one is not there and the raw one is, the
                // raw one was meant.
                let path = path.map(|p| {
                    let literal = std::path::Path::new(raw);
                    if raw != v && !p.exists() && literal.exists() {
                        literal.to_path_buf()
                    } else {
                        p
                    }
                });
                if let Some(p) = path.filter(|p| p.is_absolute()) {
                    attachments.push(p);
                }
            }
            _ => {}
        }
    }
    // The rich editor takes HTML; the mailto body is plain text.
    let body_html = if body.is_empty() {
        String::new()
    } else {
        gtk::glib::markup_escape_text(&body)
            .replace("\r\n", "\n")
            .replace('\n', "<br>")
    };
    Some(crate::ui::compose::ComposePrefill {
        to,
        cc,
        bcc,
        subject,
        body_html,
        attachments,
        ..Default::default()
    })
}

/// Route a `mid:` URI to the app (from main's `connect_open`).
pub(crate) fn queue_mid(uri: String) {
    send_or_park(&MID_PENDING, uri, AppMsg::OpenMid);
}

/// `mid:` links that arrived before the component was up (see `MAILTO_PENDING`).
static MID_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The Message-ID a `mid:` URI (RFC 2392) names, normalized the way the cache
/// stores them: percent-decoded, angle brackets and whitespace stripped,
/// lowercased, and without any `/cid` part. `None` for anything else.
fn parse_mid(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("mid:")
        .or_else(|| uri.strip_prefix("MID:"))?;
    // GLib normalizes a scheme it does not know to `mid:///…` on the way
    // through GFile (and decodes some of the escapes), so leading slashes
    // are not part of the id.
    let decoded = crate::encoding::percent::decode(rest.trim_start_matches('/'), false);
    // An optional `/content-id` follows the message-id. Slashes are legal
    // inside a message-id's left part (GitHub's have several), so only a
    // slash after the `@`, in the domain part, ends the id.
    let at = decoded.find('@')?;
    let id = match decoded[at..].find('/') {
        Some(i) => &decoded[..at + i],
        None => &decoded,
    };
    let id = id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// Route a message file (.eml) to the app (from main's `connect_open`, #398).
pub(crate) fn queue_message_file(path: std::path::PathBuf) {
    send_or_park(&FILE_PENDING, path, AppMsg::OpenMessageFile);
}

/// Message files that arrived before the component was up (see `MAILTO_PENDING`).
static FILE_PENDING: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether a file handed in is a message to read rather than one to attach:
/// a `.eml`, the usual name for a message saved whole, or Outlook's `.msg`
/// (#398).
pub(crate) fn is_message_file(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("eml") || e.eq_ignore_ascii_case("msg"))
}

/// The name Save as File offers: the subject, without what a file name
/// cannot hold, and `.eml`.
pub(super) fn eml_file_name(subject: &str) -> String {
    let stem: String = subject
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.');
    let stem: String = stem.chars().take(80).collect();
    if stem.is_empty() {
        "message.eml".to_string()
    } else {
        format!("{stem}.eml")
    }
}

/// The folder id a message read from a file carries (#398): no folder has
/// it, so nothing moves, deletes or refetches the message.
pub(super) const FILE_FOLDER: u32 = u32::MAX;

/// Route files to attach to a fresh composer (from main's `connect_open`).
pub(crate) fn queue_attach_files(paths: Vec<std::path::PathBuf>) {
    send_or_park(&ATTACH_PENDING, paths, AppMsg::OpenWithFiles);
}

/// Files handed in (via `connect_open`) before the app's component was up —
/// same early-arrival race as `MAILTO_PENDING`, queued separately since each
/// batch opens its own composer.
static ATTACH_PENDING: std::sync::Mutex<Vec<Vec<std::path::PathBuf>>> =
    std::sync::Mutex::new(Vec::new());

impl AppModel {
    /// Ask where to put a message's bytes, and write them there (#398).
    pub(super) fn save_message_file(&self, name: String, raw: Vec<u8>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title(&i18n("Save Message"));
        dialog.set_initial_name(Some(&name));
        let notifications = self.notifications.sender().clone();
        dialog.save(
            Some(&self.window),
            gtk::gio::Cancellable::NONE,
            move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                if let Err(e) = std::fs::write(&path, &raw) {
                    tracing::warn!("save {}: {e}", path.display());
                    let _ = notifications.send(NotifyInput::Push {
                        text: i18n_f("Could not save the message: {e}", &[("e", &e.to_string())]),
                        error: true,
                        connectivity: false,
                    });
                }
            },
        );
    }

    /// Pick a message file to read (#398).
    fn pick_message_file(&self, sender: &ComponentSender<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title(&i18n("Open Message File"));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Email messages")));
        filter.add_mime_type("message/rfc822");
        filter.add_mime_type("application/vnd.ms-outlook");
        filter.add_suffix("eml");
        filter.add_suffix("msg");
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        let s = sender.input_sender().clone();
        dialog.open(
            Some(&self.window),
            gtk::gio::Cancellable::NONE,
            move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    let _ = s.send(AppMsg::OpenMessageFile(path));
                }
            },
        );
    }

    /// Read a message file in a window of its own (#398), rendered as the
    /// reader renders mail from a server. It belongs to the account in view,
    /// which is where Reply and Forward send from, and to no folder.
    fn open_message_file(&mut self, path: &std::path::Path, sender: &ComponentSender<Self>) {
        // An Outlook .msg is turned into a message first (see outlook_msg).
        let read = std::fs::read(path).and_then(|bytes| {
            let outlook = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("msg"));
            if crate::mail::outlook_msg::is_msg(&bytes) {
                crate::mail::outlook_msg::to_eml(&bytes).map_err(std::io::Error::other)
            } else if outlook {
                // Other programs name files .msg too (Tcl's catalogues).
                Err(std::io::Error::other(i18n("it is not an Outlook message")))
            } else {
                Ok(bytes)
            }
        });
        let raw = match read {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!("open {}: {e}", path.display());
                self.notice_error(i18n_f(
                    "Could not open {file}: {e}",
                    &[("file", &path.display().to_string()), ("e", &e.to_string())],
                ));
                return;
            }
        };
        let id = self.next_local_id;
        self.next_local_id -= 1;
        let mut m = crate::worker::summary_from_raw(self.active_account(), FILE_FOLDER, id, &raw);
        let (body, mut check, has_attachment) = crate::worker::render_raw(&raw);
        // A file's sender checks were made by nobody this account knows:
        // it could be anyone's, headers and all.
        self.vouchers.gate(None, &mut check);
        m.unread = false;
        m.has_attachment = has_attachment;
        m.body = body.clone();
        let key = body_key(&m);
        self.body_cache.insert(key, body);
        self.attachment_cache
            .insert(key, crate::worker::extract_attachments_of(&raw));
        self.sender_cache.insert(key, Box::new(check));
        tracing::info!("open: {} as a message", path.display());
        self.open_message_window(m, Vec::new(), sender);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_uri_queue(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenMessageFile(path) => self.open_message_file(&path, &sender),
            AppMsg::PickMessageFile => self.pick_message_file(&sender),

            AppMsg::OpenMid(uri) => self.open_mid(uri, sender),

            AppMsg::MidLocated {
                account_id,
                message_id,
                hit,
            } => {
                let Some(remaining) = self.mid_searches.get_mut(&message_id) else {
                    return;
                };
                if let Some((folder_path, uid)) = hit {
                    tracing::info!(
                        "mid: {message_id} found on account {account_id}, {folder_path} uid {uid}"
                    );
                    self.mid_searches.remove(&message_id);
                    sender.input(AppMsg::OpenAttachmentMessage {
                        account_id,
                        folder_path,
                        uid,
                    });
                    return;
                }
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    tracing::info!("mid: {message_id} not found on any account");
                    self.mid_searches.remove(&message_id);
                    self.notice_error(i18n_f(
                        "No message with the ID {id} was found",
                        &[("id", &message_id)],
                    ));
                }
            }
            _ => unreachable!("not routed to on_uri_queue"),
        }
    }

    /// A `mid:` link (RFC 2392) from anywhere on the system (#130): open the
    /// message with that Message-ID.
    fn open_mid(&mut self, uri: String, sender: ComponentSender<Self>) {
        // The cache first: it holds every message Hylki has listed,
        // across accounts, and answers at once. Only a miss asks the
        // servers, one HEADER search per folder per account.
        let Some(id) = parse_mid(&uri) else {
            tracing::warn!("mid: link not understood: {uri}");
            return;
        };
        let hits: Vec<(u32, String, u32)> = crate::cache::Cache::open()
            .map(|c| c.locate_by_message_id(&id))
            .unwrap_or_default()
            .into_iter()
            .filter(|(acc, path, _)| {
                self.folders
                    .get(acc)
                    .is_some_and(|fs| fs.iter().any(|f| f.path == *path))
            })
            .collect();
        // The copy in the account currently in view wins when several
        // accounts hold the message (a mail sent to two of them).
        let preferred = self.current.as_ref().map(|m| m.account_id);
        if let Some((account_id, folder_path, uid)) = hits
            .iter()
            .find(|(acc, _, _)| Some(*acc) == preferred)
            .or_else(|| hits.first())
            .cloned()
        {
            tracing::info!("mid: {id} is cached in account {account_id}, {folder_path} uid {uid}");
            sender.input(AppMsg::OpenAttachmentMessage {
                account_id,
                folder_path,
                uid,
            });
            return;
        }
        if self.workers.is_empty() {
            self.notice_error(i18n_f(
                "No message with the ID {id} was found",
                &[("id", &id)],
            ));
            return;
        }
        self.mid_searches.insert(id.clone(), self.workers.len());
        for w in self.workers.values() {
            let _ = w.send(MailRequest::Locate {
                message_id: id.clone(),
            });
        }
    }

    /// mailto: URIs can arrive (via GApplication `open`) before this init
    /// ran — install the live sender and drain anything that queued early.
    pub(super) fn drain_hand_off_queues(&self, sender: &ComponentSender<Self>) {
        let _ = MAILTO_SENDER.set(sender.input_sender().clone());
        for uri in parked(&MAILTO_PENDING).drain(..) {
            sender.input(AppMsg::OpenMailto(uri));
        }
        for uri in parked(&MID_PENDING).drain(..) {
            sender.input(AppMsg::OpenMid(uri));
        }
        for path in parked(&FILE_PENDING).drain(..) {
            sender.input(AppMsg::OpenMessageFile(path));
        }
        for paths in parked(&ATTACH_PENDING).drain(..) {
            sender.input(AppMsg::OpenWithFiles(paths));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mailto_uris_become_composer_prefills() {
        let p = super::parse_mailto("mailto:ann@example.com").unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert!(p.subject.is_empty() && p.body_html.is_empty());

        let p = super::parse_mailto(
            "mailto:ann@example.com?subject=Hi%20there&cc=bob@x.org&body=line%20one%0Aline%20two",
        )
        .unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert_eq!(p.subject, "Hi there");
        assert_eq!(p.cc, "bob@x.org");
        assert_eq!(p.body_html, "line one<br>line two");

        // Plus-addressing survives: '+' is literal in mailto, never a space.
        let p = super::parse_mailto("mailto:user%2Btag@example.com?to=a+b@x.org").unwrap();
        assert_eq!(p.to, "user+tag@example.com, a+b@x.org");

        // A body with markup arrives escaped, not interpreted.
        let p = super::parse_mailto("mailto:a@b.c?body=%3Cscript%3E").unwrap();
        assert_eq!(p.body_html, "&lt;script&gt;");

        assert!(super::parse_mailto("https://example.com").is_none());

        // Nautilus-style attachments (#90): plain absolute paths and file://
        // URIs, percent-decoded, several allowed; relative paths refused.
        // Nautilus opens `mailto:///?…` — the slashes are not an address.
        let p = super::parse_mailto(
            "mailto:///?attach=/home/u/a%20b.pdf&attach=file:///tmp/c.png&attach=../etc/passwd",
        )
        .unwrap();
        assert_eq!(p.to, "");
        assert_eq!(
            p.attachments,
            vec![
                std::path::PathBuf::from("/home/u/a b.pdf"),
                std::path::PathBuf::from("/tmp/c.png"),
            ],
        );

        // The Email portal (#339): attachment=, raw paths, an `&` in a name,
        // and file:// URIs with their own escapes or a localhost host.
        let p = super::parse_mailto(
            "mailto:a@b.c?subject=&body=&attachment=/home/u/Mes Docs/A & B été.pdf\
             &attachment=file://localhost/tmp/x%20y.txt",
        )
        .unwrap();
        assert_eq!(
            p.attachments,
            vec![
                std::path::PathBuf::from("/home/u/Mes Docs/A & B été.pdf"),
                std::path::PathBuf::from("/tmp/x y.txt"),
            ],
        );
    }

    /// A `mid:` link names a Message-ID the way the cache stores it: no
    /// brackets, lowercase, percent-decoded, without a `/cid` part.
    #[test]
    fn mid_links_normalize_to_the_cached_form() {
        let want = Some("cah5b6q=abc123@example.com".to_string());
        assert_eq!(super::parse_mid("mid:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(super::parse_mid("mid:CAH5B6Q%3Dabc123%40example.com"), want);
        assert_eq!(
            super::parse_mid("mid:%3CCAH5B6Q%3Dabc123%40example.com%3E"),
            want
        );
        assert_eq!(
            super::parse_mid("mid:<CAH5B6Q=abc123@example.com>/part1"),
            want
        );
        assert_eq!(super::parse_mid("MID:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(
            super::parse_mid("mid:///%3CCAH5B6Q=abc123@example.com%3E"),
            want
        );
        // Slashes inside the id are part of it; one after the domain is a cid.
        assert_eq!(
            super::parse_mid("mid:hyprlab/hylki/issues/128/5558573653@github.com/att1"),
            Some("hyprlab/hylki/issues/128/5558573653@github.com".to_string())
        );
        assert_eq!(super::parse_mid("mid:"), None);
        assert_eq!(super::parse_mid("mid:not-an-id"), None);
        assert_eq!(super::parse_mid("mailto:a@b.c"), None);
    }

    #[test]
    fn save_as_file_offers_the_subject() {
        assert_eq!(
            super::eml_file_name("Re: Q3 roadmap / review?"),
            "Re- Q3 roadmap - review-.eml"
        );
        assert_eq!(super::eml_file_name("   "), "message.eml");
        assert_eq!(super::eml_file_name("..."), "message.eml");
        assert!(super::is_message_file(std::path::Path::new("/tmp/A.EML")));
        assert!(!super::is_message_file(std::path::Path::new("/tmp/a.pdf")));
    }
}
