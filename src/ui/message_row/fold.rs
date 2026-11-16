//! Folding the date headings of a list grouped by date (#659): the heading
//! the view draws over an open section, and the row that stands in for a
//! folded one, which the keyboard can reach and open again.

use super::row::Row;
use super::{DaySection, RowData, RowItem};
use crate::models::Message;
use crate::ui::message_list::MessageListInput;
use adw::prelude::*;

/// The account a folded heading's row claims. No account has it, so nothing
/// done by key to the row's message reaches any mail.
const STUB_ACCOUNT: u32 = u32::MAX;

/// The message a folded section's row carries in place of one: the list
/// keeps a message for every row, and this one belongs to no account.
pub(crate) fn fold_stub(head: &Message, section: DaySection) -> Message {
    Message {
        id: u32::MAX - section as u32,
        account_id: STUB_ACCOUNT,
        folder_id: u32::MAX,
        uid: 0,
        unread: false,
        starred: false,
        keywords: Vec::new(),
        labels: Vec::new(),
        message_id: String::new(),
        ..head.clone()
    }
}

/// Whether `m` is a folded heading's stand-in rather than mail.
pub(crate) fn is_fold_stub(m: &Message) -> bool {
    m.account_id == STUB_ACCOUNT
}

/// A date heading: its name, then, folded, how many conversations it holds
/// and a dot when any is unread, then a chevron. A click anywhere on it folds
/// or opens the section.
pub(super) struct FoldHeading {
    pub(super) bx: gtk::Box,
    label: gtk::Label,
    count: gtk::Label,
    dot: gtk::Box,
    chevron: gtk::Image,
}

impl FoldHeading {
    pub(super) fn new() -> Self {
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bx.add_css_class("list-day-heading");
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("heading");
        label.add_css_class("dim-label");
        let count = gtk::Label::new(None);
        count.add_css_class("dim-label");
        count.add_css_class("numeric");
        count.add_css_class("list-day-count");
        count.set_visible(false);
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("unread-dot");
        dot.set_valign(gtk::Align::Center);
        dot.set_visible(false);
        let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
        chevron.add_css_class("list-day-chevron");
        chevron.set_valign(gtk::Align::Center);
        bx.set_cursor_from_name(Some("pointer"));
        bx.append(&label);
        bx.append(&count);
        bx.append(&dot);
        bx.append(&chevron);
        FoldHeading {
            bx,
            label,
            count,
            dot,
            chevron,
        }
    }

    /// The parts of a heading built by `new`, from its box.
    fn of(bx: &gtk::Box) -> Option<Self> {
        let label = bx.first_child().and_downcast::<gtk::Label>()?;
        let count = label.next_sibling().and_downcast::<gtk::Label>()?;
        let dot = count.next_sibling().and_downcast::<gtk::Box>()?;
        let chevron = dot.next_sibling().and_downcast::<gtk::Image>()?;
        Some(FoldHeading {
            bx: bx.clone(),
            label,
            count,
            dot,
            chevron,
        })
    }

    /// Show `section`, open, or folded over `(conversations, unread)`.
    pub(super) fn show(&self, section: DaySection, folded: Option<(usize, usize)>) {
        let text = section.label();
        if self.label.label() != text {
            self.label.set_label(&text);
        }
        let (count, unread) = folded.unwrap_or_default();
        self.count.set_visible(folded.is_some());
        self.count.set_label(&count.to_string());
        self.dot.set_visible(unread > 0);
        let icon = if folded.is_some() {
            "pan-end-symbolic"
        } else {
            "pan-down-symbolic"
        };
        self.chevron.set_icon_name(Some(icon));
    }
}

/// A click on the heading runs `fire`. The press is claimed, so the list
/// neither selects nor activates the row a folded heading is.
fn on_click(bx: &gtk::Box, fire: impl Fn() + 'static) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    click.connect_pressed(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
    });
    click.connect_released(move |_, _, _, _| fire());
    bx.add_controller(click);
}

/// The heading over an open section, for the view's header factory: its
/// click on it folds the section.
pub(super) fn header_heading(
    header: &gtk::ListHeader,
    input: relm4::Sender<MessageListInput>,
) -> gtk::Box {
    let h = FoldHeading::new();
    let weak = header.downgrade();
    let fold = move || {
        let section = weak
            .upgrade()
            .and_then(|h| h.item())
            .and_downcast::<RowItem>()
            .map(|i| i.data().section);
        if let Some(section) = section.filter(|s| *s != DaySection::None) {
            let _ = input.send(MessageListInput::FoldDay {
                section,
                fold: true,
            });
        }
    };
    on_click(&h.bx, fold);
    h.bx
}

/// A heading names the section of the row it stands over. Over a folded
/// section's row it gives way: the row is the heading there.
pub(super) fn fill_header(header: &gtk::ListHeader) {
    let Some(h) = header
        .child()
        .and_downcast::<gtk::Box>()
        .and_then(|b| FoldHeading::of(&b))
    else {
        return;
    };
    let data = header
        .item()
        .and_downcast::<RowItem>()
        .map(|i| i.data());
    let stub = data.as_ref().is_some_and(|d| d.meta.folded.is_some());
    h.bx.set_visible(!stub);
    if let Some(d) = data.filter(|_| !stub) {
        h.show(d.section, None);
    }
}

impl Row {
    /// A click on a folded section's heading opens it again.
    pub(super) fn wire_fold(&self) {
        let weak = self.me.clone();
        on_click(&self.w.fold.bx, move || {
            let Some(row) = weak.upgrade() else { return };
            if let Some(d) = row.data().filter(|d| d.meta.folded.is_some()) {
                row.send(MessageListInput::FoldDay {
                    section: d.section,
                    fold: false,
                });
            }
        });
    }

    /// Show the row as a folded section's heading, or as mail. True when it
    /// is the heading, which has nothing more to fill.
    pub(super) fn refresh_fold(&self, data: &RowData) -> bool {
        let folded = data.meta.folded;
        self.w.revealer.set_visible(folded.is_none());
        self.w.fold.bx.set_visible(folded.is_some());
        if let Some(item) = self.list_item.borrow().upgrade() {
            if item.is_selectable() != folded.is_none() {
                item.set_selectable(folded.is_none());
            }
        }
        if folded.is_some() {
            self.w.fold.show(data.section, folded);
            self.sync_host_classes();
        }
        folded.is_some()
    }
}
