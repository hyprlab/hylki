//! The reader's right-click menu: what the page reports under the pointer,
//! and the entries offered for it.

use super::links::{default_image_name, own_picture, remote_image_name};
use super::types::MessageViewInput;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

/// What a right-click in the reader landed on besides the message: a link,
/// a picture, selected text. Each adds its own entries at the top of the
/// message's menu.
#[derive(Debug, Clone, Default)]
pub(crate) struct MenuHit {
    /// A link the reader would open on a click (web and mail links only).
    pub(super) link: Option<String>,
    /// The picture's URI: a `data:` one for mail's own pictures.
    pub(super) image: Option<String>,
    /// The selected text, when the click was on it.
    pub(super) selection: Option<String>,
    /// The message's translation entry (#327), when a service is set up:
    /// what it says, and the reader to tell.
    pub(crate) translate: Option<TranslateEntry>,
    /// Expand All and Collapse All (#326), in a conversation: whether any
    /// message is folded, whether any is open, and the reader to tell.
    pub(crate) folds: Option<(bool, bool, relm4::Sender<MessageViewInput>)>,
    /// The sender's own fonts and colors (#56), while the reader's are on:
    /// whether this message shows the sender's, and the reader to tell.
    pub(crate) sender_style: Option<(bool, relm4::Sender<MessageViewInput>)>,
}

/// Translate, Show Original or Show Translation in a message's menu, sent
/// back to the reader that showed the menu (the main one or a pop-out's).
#[derive(Clone)]
pub(crate) struct TranslateEntry {
    pub(crate) label: String,
    pub(crate) reader: relm4::Sender<MessageViewInput>,
}

impl std::fmt::Debug for TranslateEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranslateEntry")
            .field("label", &self.label)
            .finish()
    }
}

/// The entries a right-click's `hit` puts at the top of the message's menu:
/// Copy for selected text, then the link's and the picture's own.
pub(crate) fn hit_menu_entries(
    hit: &MenuHit,
    window: Option<gtk::Window>,
) -> Vec<crate::ui::context_menu::MenuEntry> {
    use crate::ui::context_menu::MenuEntry;
    fn copy(text: String) -> impl Fn() + 'static {
        move || {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&text);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(text) = &hit.selection {
        out.push(
            MenuEntry::new(i18n("Copy"), copy(text.clone()))
                .icon("edit-copy-symbolic")
                .accel("<Control>c"),
        );
    }
    if let Some(uri) = &hit.link {
        let (open, w) = (uri.clone(), window.clone());
        out.push(
            MenuEntry::new(i18n("Open Link"), move || {
                crate::ui::launch::open_link(&open, w.as_ref());
            })
            .icon("web-browser-symbolic"),
        );
        // A mail link copies as the address it writes to, not as a URI.
        let address = uri
            .split_once(':')
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("mailto"))
            .map(|(_, rest)| {
                crate::encoding::percent::decode(rest.split('?').next().unwrap_or(""), false)
            });
        out.push(
            match address {
                Some(a) => MenuEntry::new(i18n("Copy Email Address"), copy(a)),
                None => MenuEntry::new(i18n("Copy Link Address"), copy(uri.clone())),
            }
            .icon("edit-copy-symbolic"),
        );
    }
    if let Some(uri) = &hit.image {
        // The mail's own pictures have their bytes in hand; a remote one is
        // only an address here.
        match own_picture(uri) {
            Some((mime, data)) => {
                let texture = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(&data)).ok();
                let bytes = data.clone();
                out.push(
                    MenuEntry::new(i18n("Save Image As…"), move || {
                        let dialog = gtk::FileDialog::builder()
                            .initial_name(default_image_name(&mime))
                            .title(i18n("Save Image"))
                            .build();
                        let bytes = bytes.clone();
                        dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                            if let Some(path) = res.ok().and_then(|f| f.path()) {
                                let _ = std::fs::write(path, &bytes);
                            }
                        });
                    })
                    .icon("document-save-as-symbolic"),
                );
                if let Some(texture) = texture {
                    out.push(
                        MenuEntry::new(i18n("Copy Image"), move || {
                            if let Some(display) = gtk::gdk::Display::default() {
                                display.clipboard().set_texture(&texture);
                            }
                        })
                        .icon("edit-copy-symbolic"),
                    );
                }
            }
            // A remote picture the reader showed through the cache (#474):
            // saved and copied from the cache, so the right-click sends
            // nothing new to the sender (#569).
            None if crate::services::remote_content::web_url(uri).is_some() => {
                let web = crate::services::remote_content::web_url(uri).unwrap_or_default();
                let (routed, w, name_url) = (uri.clone(), window.clone(), web.clone());
                out.push(
                    MenuEntry::new(i18n("Save Image As…"), move || {
                        let (w, name_url) = (w.clone(), name_url.clone());
                        crate::services::remote_content::get(&routed, move |entry| {
                            let Some(entry) = entry else {
                                tracing::warn!("save image: the picture could not be loaded");
                                return;
                            };
                            let dialog = gtk::FileDialog::builder()
                                .initial_name(remote_image_name(&name_url, &entry.content_type))
                                .title(i18n("Save Image"))
                                .build();
                            dialog.save(w.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                                if let Some(path) = res.ok().and_then(|f| f.path()) {
                                    if let Err(e) = std::fs::write(path, &entry.bytes) {
                                        tracing::warn!("save image: {e}");
                                    }
                                }
                            });
                        });
                    })
                    .icon("document-save-as-symbolic"),
                );
                let routed = uri.clone();
                out.push(
                    MenuEntry::new(i18n("Copy Image"), move || {
                        crate::services::remote_content::get(&routed, |entry| {
                            let texture = entry.and_then(|e| {
                                gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(&e.bytes))
                                    .ok()
                            });
                            match (texture, gtk::gdk::Display::default()) {
                                (Some(t), Some(display)) => display.clipboard().set_texture(&t),
                                _ => tracing::warn!("copy image: the picture could not be read"),
                            }
                        });
                    })
                    .icon("edit-copy-symbolic"),
                );
                out.push(
                    MenuEntry::new(i18n("Copy Image Address"), copy(web))
                        .icon("edit-copy-symbolic"),
                );
            }
            // Remote content blocked: only the address, never a fetch.
            None if uri.starts_with("http") => {
                out.push(
                    MenuEntry::new(i18n("Copy Image Address"), copy(uri.clone()))
                        .icon("edit-copy-symbolic"),
                );
            }
            None => {}
        }
    }
    out
}

/// The card key and selected text from `__hylkiMenuAt`'s JSON answer.
pub(super) fn menu_lookup(json: &str) -> (Option<(u32, u32)>, Option<String>) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let key = v["key"].as_str().and_then(|k| {
        let (a, i) = k.split_once(':')?;
        Some((a.parse().ok()?, i.parse().ok()?))
    });
    let sel = v["sel"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    (key, sel)
}

/// Whether `__hylkiMenuAt` said no message menu: the right-click was off
/// the cards, or on an address, which has the page's own menu.
pub(super) fn menu_skipped(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v.get("skip").is_some())
}

/// Take WebKit's Back, Forward, Stop and Reload out of a context menu. Every
/// view here shows a document loaded from a string, so there is no page to go
/// back to or fetch again, and Reload ended on "Connection refused" (#301).
pub(crate) fn strip_navigation_items(menu: &webkit6::ContextMenu) {
    use webkit6::ContextMenuAction as A;
    for item in menu.items() {
        if matches!(
            item.stock_action(),
            A::GoBack | A::GoForward | A::Stop | A::Reload
        ) {
            menu.remove(&item);
        }
    }
}
