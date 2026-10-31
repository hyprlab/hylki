//! The editor's context menu.

use webkit6::prelude::WebViewExt;

use crate::i18n::i18n;

use super::files::{detach_ctx_image, paste_into};
use super::grammar::{lead_menu, GrammarState};
use super::spelling::append_spelling_menu;
use super::{exec, js_escape, AttachCallback, SpellLangsCallback};

/// The editor's right-click menu: image actions over a picture, both
/// kinds of paste, history, the spelling submenu, and over a grammar mark
/// the problem and its suggestions.
pub(super) fn connect_context_menu(
    webview: &webkit6::WebView,
    menu_attach_cb: AttachCallback,
    menu_history: std::rc::Rc<std::cell::Cell<(bool, bool)>>,
    menu_plain: std::rc::Rc<std::cell::Cell<bool>>,
    menu_spell_cb: SpellLangsCallback,
    grammar: GrammarState,
) {
    webview.connect_context_menu(move |view, menu, hit| {
        // The problem goes on top, above Undo and the spelling guesses,
        // so it is put in after them.
        let lead = |menu: &webkit6::ContextMenu| {
            if !hit.context_is_image() {
                lead_menu(menu, view, &grammar);
            }
        };
        crate::ui::message_view::strip_navigation_items(menu);
        if hit.context_is_image() {
            // Stock image entries (copy/save/open variants) are replaced
            // by ours, which also know about the editable document.
            for item in menu.items().iter().filter(|i| {
                matches!(
                    i.stock_action(),
                    webkit6::ContextMenuAction::CopyImageToClipboard
                        | webkit6::ContextMenuAction::CopyImageUrlToClipboard
                        | webkit6::ContextMenuAction::DownloadImageToDisk
                        | webkit6::ContextMenuAction::OpenImageInNewWindow
                )
            }) {
                menu.remove(item);
            }
            // The stock Cut/Copy already act on the image — the
            // right-click selected it. The one image-specific verb is
            // demoting it to an ordinary attachment; the script that
            // lifts it declines non-embedded (remote) images itself,
            // since the hit test doesn't surface data: URIs to check
            // against here.
            let action = gtk::gio::SimpleAction::new("hylki-img-attach", None);
            let v = view.clone();
            let cb = menu_attach_cb.clone();
            action.connect_activate(move |_, _| detach_ctx_image(&v, cb.clone()));
            menu.insert(
                &webkit6::ContextMenuItem::from_gaction(
                    &action,
                    &i18n("Send as Attachment Instead"),
                    None,
                ),
                0,
            );
            menu.insert(&webkit6::ContextMenuItem::new_separator(), 1);
            // Sizes lead the menu: the fractions are of the writing
            // width, which is the width the recipient reads at, so
            // "Large" means the full column rather than some pixel count
            // that means nothing on the other end. Dragging the corner
            // handles does the same thing by hand.
            let mut at = 0;
            for (label, kind) in [
                (i18n("Small"), "small"),
                (i18n("Medium"), "medium"),
                (i18n("Large"), "large"),
                (i18n("Original Size"), "original"),
            ] {
                let action = gtk::gio::SimpleAction::new(&format!("hylki-img-{kind}"), None);
                let v = view.clone();
                action.connect_activate(move |_, _| {
                    exec(&v, &format!("window.__hylkiSetImageSize('{kind}')"));
                });
                menu.insert(
                    &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                    at,
                );
                at += 1;
            }
            {
                let action = gtk::gio::SimpleAction::new("hylki-img-fit", None);
                let v = view.clone();
                let hint = js_escape(&i18n(
                    "Will be recompressed to the size shown when this is sent",
                ));
                action.connect_activate(move |_, _| {
                    exec(&v, &format!("window.__hylkiToggleFit('{hint}')"));
                });
                menu.insert(
                    &webkit6::ContextMenuItem::from_gaction(
                        &action,
                        &i18n("Recompress to This Size on Send"),
                        None,
                    ),
                    at,
                );
                at += 1;
            }
            menu.insert(&webkit6::ContextMenuItem::new_separator(), at);
        }
        // Which dictionaries check the message, switched one at a time
        // without a trip to Settings (#365). It is the app-wide setting,
        // so every open composer follows.
        if !hit.context_is_image() && menu_spell_cb.borrow().is_some() {
            append_spelling_menu(menu, &menu_spell_cb);
        }
        let items = menu.items();
        let Some(pos) = items
            .iter()
            .position(|i| i.stock_action() == webkit6::ContextMenuAction::Paste)
        else {
            lead(menu);
            return false; // no paste here (nothing editable hit): stock menu as-is
        };
        for item in items.iter().filter(|i| {
            matches!(
                i.stock_action(),
                webkit6::ContextMenuAction::Paste | webkit6::ContextMenuAction::PasteAsPlainText
            )
        }) {
            menu.remove(item);
        }
        let mut at = pos as i32;
        // Plain text has no formatting to keep, so it offers only the
        // paste it does.
        let plain = menu_plain.get();
        for (label, rich) in [
            (i18n("Paste with Formatting"), true),
            (i18n("Paste as Plain Text"), false),
        ] {
            if rich && plain {
                continue;
            }
            let action = gtk::gio::SimpleAction::new(
                if rich {
                    "hylki-paste-rich"
                } else {
                    "hylki-paste-plain"
                },
                None,
            );
            let v = view.clone();
            let cb = menu_attach_cb.clone();
            action.connect_activate(move |_, _| paste_into(&v, rich, &cb, plain));
            menu.insert(
                &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                at,
            );
            at += 1;
        }
        // Undo and Redo lead an editable menu, as they do everywhere
        // else — the one place the body's text history is visible at
        // all, rather than only on the keys. Inserted last so the
        // paste group's index above is still the one it was measured
        // at. Over an image the sizes keep the top: that menu is about
        // the picture under the pointer, not the message.
        if !hit.context_is_image() {
            let (can_undo, can_redo) = menu_history.get();
            for (label, name, redo, on) in [
                (i18n("Undo"), "hylki-undo", false, can_undo),
                (i18n("Redo"), "hylki-redo", true, can_redo),
            ] {
                let action = gtk::gio::SimpleAction::new(name, None);
                action.set_enabled(on);
                let v = view.clone();
                action.connect_activate(move |_, _| {
                    v.execute_editing_command(if redo {
                        webkit6::EDITING_COMMAND_REDO
                    } else {
                        webkit6::EDITING_COMMAND_UNDO
                    });
                });
                menu.insert(
                    &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                    i32::from(redo),
                );
            }
            menu.insert(&webkit6::ContextMenuItem::new_separator(), 2);
        }
        lead(menu);
        false
    });
}
