//! The page's side of the JS bridge: every message the wrapper document
//! posts to the `"hylki"` handler, as `verb:account:id[:extra]`, parsed into
//! what the reader should do with it. The page only ever posts what this
//! document put there, but the message is still parsed strictly: a verb it
//! does not know, a key that is not two numbers, or an `extra` that does not
//! parse is dropped. `extra` is everything after the third colon, so it may
//! itself contain colons.

use super::types::{InviteAction, MessageViewInput, SelectMode};
use crate::ui::message_list::RowAction;

/// What one page message asks for.
#[derive(Debug)]
pub(super) enum Bridge {
    /// An input for the reader itself.
    Input(MessageViewInput),
    /// Text for the clipboard, written straight away (the copy verbs).
    Clipboard(String),
}

/// Parse one page message; `None` when it is malformed or asks for nothing.
pub(super) fn parse_bridge(msg: &str) -> Option<Bridge> {
    let mut parts = msg.splitn(4, ':');
    let (Some(verb), Some(a), Some(i)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let extra = parts.next();
    let (Ok(account_id), Ok(id)) = (a.parse::<u32>(), i.parse::<u32>()) else {
        return None;
    };
    let card = |action: RowAction| MessageViewInput::CardAction {
        action,
        account_id,
        id,
    };
    // `extra` as a non-empty trimmed address (the address menu's verbs).
    let address = || extra.map(str::trim).filter(|a| !a.is_empty());
    let input = match verb {
        // Every message frame has loaded and been sized: the layout has
        // settled, so there is something worth revealing.
        "ready" => MessageViewInput::Rendered,
        "desel" => MessageViewInput::ClearCards,
        "selall" => MessageViewInput::SelectAllCards,
        // Ctrl+C over a body frame the engine would not copy from itself:
        // the selected text, for the clipboard.
        "copy" => {
            let text = extra.filter(|t| !t.is_empty())?;
            return Some(Bridge::Clipboard(text.to_string()));
        }
        // An address was clicked (or "New Message" picked from its little
        // menu): compose to it. `extra` is the bare address.
        "composeto" => MessageViewInput::ComposeTo(address()?.to_string()),
        // "Add to Contacts" from the address menu.
        "addcontact" => MessageViewInput::AddContact(address()?.to_string()),
        // "Copy Address" from the address menu.
        "copyaddr" => return Some(Bridge::Clipboard(address()?.to_string())),
        // The header's verification seal was clicked; extra is the seal's
        // own rect plus the page's innerWidth. The rect is in CSS pixels:
        // under a GNOME text scaling factor other than 1.0 WebKit zooms the
        // page, so CSS pixels no longer match widget pixels; innerWidth lets
        // the receiver recover the real ratio.
        "senderinfo" => {
            let [x, y, w, h, vw] = numbers(extra)[..] else {
                return None;
            };
            MessageViewInput::SenderInfoAt {
                account_id,
                id,
                rect: (x, y, w, h),
                page_width: vw,
            }
        }
        "sel" => {
            let mode = match extra {
                Some("t") => SelectMode::Toggle,
                Some("r") => SelectMode::Range,
                _ => SelectMode::Plain,
            };
            MessageViewInput::CardClicked {
                account_id,
                id,
                mode,
            }
        }
        "size" => {
            let Some(Ok(height)) = extra.map(|h| h.parse::<u32>()) else {
                return None;
            };
            MessageViewInput::FrameSized {
                account_id,
                id,
                height,
            }
        }
        "open" => MessageViewInput::OpenHeader { account_id, id },
        "seen" => MessageViewInput::MarkSeen { account_id, id },
        "unsub" => MessageViewInput::Unsubscribe { account_id, id },
        // A card's read receipt banner (#520); `extra` is the button.
        "mdn" => MessageViewInput::ReceiptAnswer {
            account_id,
            id,
            send: extra.map(str::trim) == Some("send"),
        },
        // A card's template banner (#360); `extra` is the button.
        "tpl" => MessageViewInput::TemplateAction {
            account_id,
            id,
            edit: extra.map(str::trim) == Some("edit"),
        },
        // A card's invitation banner (#223); `extra` is which button was
        // pressed.
        "invite" => {
            use crate::models::Rsvp;
            let action = match extra.map(str::trim) {
                Some("yes") => InviteAction::Answer(Rsvp::Accepted),
                Some("maybe") => InviteAction::Answer(Rsvp::Tentative),
                Some("no") => InviteAction::Answer(Rsvp::Declined),
                Some("cal") => InviteAction::AddToCalendar,
                _ => return None,
            };
            MessageViewInput::InviteAction {
                account_id,
                id,
                action,
            }
        }
        // A card's attachment chip (#213); `extra` is the index.
        "attopen" | "attsave" => MessageViewInput::AttachmentAction {
            account_id,
            id,
            index: extra
                .and_then(|x| x.trim().parse::<usize>().ok())
                .unwrap_or(0),
            save: verb == "attsave",
        },
        "found" => {
            let (current, total) = extra
                .and_then(|e| e.split_once(','))
                .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                .unwrap_or((0, 0));
            MessageViewInput::FindCounted { current, total }
        }
        "reply" => card(RowAction::Reply),
        "replyall" => card(RowAction::ReplyAll),
        "forward" => card(RowAction::Forward),
        "star" => card(RowAction::ToggleStar),
        "spam" => card(RowAction::Spam),
        "remote" => MessageViewInput::ToggleRemote { account_id, id },
        "archive" => card(RowAction::Archive),
        // The ⋯ acting as a menu: the button's bottom-left corner and the
        // page width, the same shape "moveto" sends.
        "cardmenu" => {
            let [x, y, vw] = numbers(extra)[..] else {
                return None;
            };
            MessageViewInput::CardMenuFrom {
                account_id,
                id,
                x,
                y,
                page_width: vw,
            }
        }
        // Move to… on a card: extra is the button's bottom-centre and the
        // page width, in CSS pixels (see "senderinfo").
        "moveto" => {
            let [x, y, vw] = numbers(extra)[..] else {
                return None;
            };
            MessageViewInput::CardMoveAt {
                account_id,
                id,
                x,
                y,
                page_width: vw,
            }
        }
        "delete" => card(RowAction::Delete),
        "toggleread" => card(RowAction::ToggleRead),
        // Translate on a card, or its banner's button (#327).
        "translate" | "tr" => MessageViewInput::Translate { account_id, id },
        // A header folded or opened its card (#326).
        "fold" => MessageViewInput::Folded {
            account_id,
            id,
            folded: extra == Some("1"),
        },
        "viewsource" => card(RowAction::ViewSource),
        "contact" => MessageViewInput::CardContact { account_id, id },
        "senderfmt" => MessageViewInput::ToggleSenderStyle { account_id, id },
        // "scrollat:<aid>:<id>:<offset>": the topmost visible card.
        "scrollat" => {
            let Some(Ok(offset)) = extra.map(|o| o.parse::<u32>()) else {
                return None;
            };
            MessageViewInput::ScrollAnchor {
                account_id,
                id,
                offset,
            }
        }
        _ => return None,
    };
    Some(Bridge::Input(input))
}

/// `extra` as comma-separated numbers; the ones that do not parse are
/// skipped, so a caller matching on the count rejects a garbled list.
fn numbers(extra: Option<&str>) -> Vec<f64> {
    extra
        .map(|e| e.split(',').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(msg: &str) -> String {
        format!("{:?}", parse_bridge(msg))
    }

    #[test]
    fn malformed_messages_are_dropped() {
        for msg in [
            "",
            "ready",
            "ready:1",
            "ready:x:2",
            "ready:1:-2",
            "nosuchverb:1:2",
            "size:1:2",
            "size:1:2:tall",
            "senderinfo:1:2:1,2,3,4",
            "senderinfo:1:2:1,2,x,4,5",
            "cardmenu:1:2:1,2",
            "moveto:1:2:1,2,3,4",
            "scrollat:1:2",
            "invite:1:2:perhaps",
            "invite:1:2",
            "copy:1:2",
            "copy:1:2:",
            "composeto:1:2:  ",
            "addcontact:1:2",
            "copyaddr:1:2: ",
        ] {
            assert_eq!(parsed(msg), "None", "{msg}");
        }
    }

    #[test]
    fn every_verb_maps_as_today() {
        let cases = [
            ("ready:0:0", "Some(Input(Rendered))"),
            ("desel:1:2", "Some(Input(ClearCards))"),
            ("selall:1:2", "Some(Input(SelectAllCards))"),
            ("copy:1:2: a:b ", "Some(Clipboard(\" a:b \"))"),
            ("composeto:1:2: x@y ", "Some(Input(ComposeTo(\"x@y\")))"),
            ("addcontact:1:2:x@y", "Some(Input(AddContact(\"x@y\")))"),
            ("copyaddr:1:2: x@y", "Some(Clipboard(\"x@y\"))"),
            (
                "senderinfo:1:2:1,2.5,3,4,800",
                "Some(Input(SenderInfoAt { account_id: 1, id: 2, rect: (1.0, 2.5, 3.0, 4.0), page_width: 800.0 }))",
            ),
            (
                "sel:1:2:t",
                "Some(Input(CardClicked { account_id: 1, id: 2, mode: Toggle }))",
            ),
            (
                "sel:1:2:r",
                "Some(Input(CardClicked { account_id: 1, id: 2, mode: Range }))",
            ),
            (
                "sel:1:2:x",
                "Some(Input(CardClicked { account_id: 1, id: 2, mode: Plain }))",
            ),
            (
                "sel:1:2",
                "Some(Input(CardClicked { account_id: 1, id: 2, mode: Plain }))",
            ),
            (
                "size:1:2:640",
                "Some(Input(FrameSized { account_id: 1, id: 2, height: 640 }))",
            ),
            ("open:1:2", "Some(Input(OpenHeader { account_id: 1, id: 2 }))"),
            ("seen:1:2", "Some(Input(MarkSeen { account_id: 1, id: 2 }))"),
            ("unsub:1:2", "Some(Input(Unsubscribe { account_id: 1, id: 2 }))"),
            (
                "mdn:1:2:send",
                "Some(Input(ReceiptAnswer { account_id: 1, id: 2, send: true }))",
            ),
            (
                "mdn:1:2:ignore",
                "Some(Input(ReceiptAnswer { account_id: 1, id: 2, send: false }))",
            ),
            (
                "tpl:1:2: edit ",
                "Some(Input(TemplateAction { account_id: 1, id: 2, edit: true }))",
            ),
            (
                "tpl:1:2:use",
                "Some(Input(TemplateAction { account_id: 1, id: 2, edit: false }))",
            ),
            (
                "invite:1:2:yes",
                "Some(Input(InviteAction { account_id: 1, id: 2, action: Answer(Accepted) }))",
            ),
            (
                "invite:1:2: maybe",
                "Some(Input(InviteAction { account_id: 1, id: 2, action: Answer(Tentative) }))",
            ),
            (
                "invite:1:2:no",
                "Some(Input(InviteAction { account_id: 1, id: 2, action: Answer(Declined) }))",
            ),
            (
                "invite:1:2:cal",
                "Some(Input(InviteAction { account_id: 1, id: 2, action: AddToCalendar }))",
            ),
            (
                "attopen:1:2: 3",
                "Some(Input(AttachmentAction { account_id: 1, id: 2, index: 3, save: false }))",
            ),
            (
                "attsave:1:2:x",
                "Some(Input(AttachmentAction { account_id: 1, id: 2, index: 0, save: true }))",
            ),
            (
                "found:1:2:3,9",
                "Some(Input(FindCounted { current: 3, total: 9 }))",
            ),
            (
                "found:1:2:3",
                "Some(Input(FindCounted { current: 0, total: 0 }))",
            ),
            (
                "reply:1:2",
                "Some(Input(CardAction { action: Reply, account_id: 1, id: 2 }))",
            ),
            (
                "replyall:1:2",
                "Some(Input(CardAction { action: ReplyAll, account_id: 1, id: 2 }))",
            ),
            (
                "forward:1:2",
                "Some(Input(CardAction { action: Forward, account_id: 1, id: 2 }))",
            ),
            (
                "star:1:2",
                "Some(Input(CardAction { action: ToggleStar, account_id: 1, id: 2 }))",
            ),
            (
                "spam:1:2",
                "Some(Input(CardAction { action: Spam, account_id: 1, id: 2 }))",
            ),
            ("remote:1:2", "Some(Input(ToggleRemote { account_id: 1, id: 2 }))"),
            (
                "archive:1:2",
                "Some(Input(CardAction { action: Archive, account_id: 1, id: 2 }))",
            ),
            (
                "cardmenu:1:2:10,20,800",
                "Some(Input(CardMenuFrom { account_id: 1, id: 2, x: 10.0, y: 20.0, page_width: 800.0 }))",
            ),
            (
                "moveto:1:2:10,20,800",
                "Some(Input(CardMoveAt { account_id: 1, id: 2, x: 10.0, y: 20.0, page_width: 800.0 }))",
            ),
            (
                "delete:1:2",
                "Some(Input(CardAction { action: Delete, account_id: 1, id: 2 }))",
            ),
            (
                "toggleread:1:2",
                "Some(Input(CardAction { action: ToggleRead, account_id: 1, id: 2 }))",
            ),
            ("translate:1:2", "Some(Input(Translate { account_id: 1, id: 2 }))"),
            ("tr:1:2", "Some(Input(Translate { account_id: 1, id: 2 }))"),
            (
                "fold:1:2:1",
                "Some(Input(Folded { account_id: 1, id: 2, folded: true }))",
            ),
            (
                "fold:1:2:0",
                "Some(Input(Folded { account_id: 1, id: 2, folded: false }))",
            ),
            (
                "viewsource:1:2",
                "Some(Input(CardAction { action: ViewSource, account_id: 1, id: 2 }))",
            ),
            ("contact:1:2", "Some(Input(CardContact { account_id: 1, id: 2 }))"),
            (
                "senderfmt:1:2",
                "Some(Input(ToggleSenderStyle { account_id: 1, id: 2 }))",
            ),
            (
                "scrollat:1:2:120",
                "Some(Input(ScrollAnchor { account_id: 1, id: 2, offset: 120 }))",
            ),
        ];
        for (msg, want) in cases {
            assert_eq!(parsed(msg), want, "{msg}");
        }
    }
}
