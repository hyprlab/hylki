//! The list's rows: what a line shows, the row widgets the list view binds,
//! drag and drop of contacts, and the small widgets the cards are built
//! from.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, Labeled, Member};
use crate::i18n::i18n;

use super::types::{ContactsPageInput, DRAG_MARK};

/// One line of the contact list.
pub(super) struct Line {
    /// The entry the line shows (see `row_map`); `None` for one that is
    /// nobody and cannot be selected. Moves when a contact before it is
    /// deleted.
    pub(super) idx: Cell<Option<usize>>,
    title: String,
    subtitle: String,
    pub(super) kind: LineKind,
}

/// Lines are alike when they show the same thing, wherever they are.
impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title && self.subtitle == other.subtitle && self.kind == other.kind
    }
}

#[derive(PartialEq)]
pub(super) enum LineKind {
    /// A person or a group, with their photo's key (0: none).
    Entry { photo_key: u64, group: bool },
    /// The heading above the people found in the directories.
    Heading,
    /// A directory that did not answer the search.
    Failure,
    /// A dim line in place of an empty list.
    Note,
}

impl Line {
    pub(super) fn entry(idx: usize, c: &ContactDetails, subtitle: String) -> Self {
        Line {
            idx: Cell::new(Some(idx)),
            title: c.name.clone(),
            subtitle,
            kind: LineKind::Entry {
                photo_key: line_photo_key(c),
                group: c.is_group(),
            },
        }
    }

    pub(super) fn other(kind: LineKind, title: String, subtitle: String) -> Self {
        Line {
            idx: Cell::new(None),
            title,
            subtitle,
            kind,
        }
    }
}

/// The key a contact's photo is decoded under. A read of the address books
/// works it out off the main thread; a directory entry's is worked out here.
pub(super) fn line_photo_key(c: &ContactDetails) -> u64 {
    match (&c.photo, c.photo_key) {
        (Some(photo), 0) => crate::contacts::photo_key(photo),
        (_, key) => key,
    }
}

/// The widgets of one row, which the list view binds to line after line.
/// Kept light, since the view holds a couple of hundred: the face is an
/// image in a clipped disc rather than an `adw::Avatar`, and the parts only
/// a heading, a note or a group needs are made when one first shows here.
pub(super) struct ListRow {
    root: gtk::Box,
    header: gtk::Box,
    /// The circle the face is drawn in; its rounded corners clip a photo.
    disc: gtk::Box,
    face: gtk::Image,
    warning: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    /// For a group, or a name with no letter to draw: libadwaita's avatar.
    avatar: RefCell<Option<adw::Avatar>>,
    /// The heading or note text, for those lines.
    banner: RefCell<Option<gtk::Label>>,
    /// The line bound now, for the right-click.
    line: RefCell<Option<gtk::glib::BoxedAnyObject>>,
    /// The photo the face waits for (0: none).
    waiting: Cell<u64>,
}

/// The size of a list face.
const FACE: i32 = 34;

impl ListRow {
    pub(super) fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // `header`, as in a list box row: the selected row's text takes the
        // accent's foreground by that class (styles.css).
        // The measures of the `adw::ActionRow` the list had before.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("header");
        header.set_size_request(-1, 50);
        header.set_margin_start(12);
        header.set_margin_end(12);
        let disc = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        disc.add_css_class("contact-face");
        disc.set_overflow(gtk::Overflow::Hidden);
        disc.set_valign(gtk::Align::Center);
        let face = gtk::Image::new();
        face.set_pixel_size(FACE);
        disc.append(&face);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("warning");
        // As wide as a face, so the names line up.
        warning.set_size_request(FACE, -1);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        text.append(&subtitle);
        header.append(&disc);
        header.append(&warning);
        header.append(&text);
        root.append(&header);
        ListRow {
            root,
            header,
            disc,
            face,
            warning,
            title,
            subtitle,
            avatar: RefCell::new(None),
            banner: RefCell::new(None),
            line: RefCell::new(None),
            waiting: Cell::new(0),
        }
    }

    fn bind(&self, item: &gtk::glib::BoxedAnyObject) {
        let line = item.borrow::<Line>();
        self.line.replace(Some(item.clone()));
        self.waiting.set(0);
        let banner = matches!(line.kind, LineKind::Heading | LineKind::Note);
        self.header.set_visible(!banner);
        self.show_banner(banner.then_some(&line));
        if banner {
            return;
        }
        self.title.set_label(&line.title);
        self.subtitle.set_label(&line.subtitle);
        self.subtitle.set_visible(!line.subtitle.is_empty());
        let failure = matches!(line.kind, LineKind::Failure);
        // A failure says why in full; an address fits one line.
        self.subtitle.set_wrap(failure);
        self.subtitle.set_lines(if failure { 3 } else { 1 });
        self.warning.set_visible(failure);
        self.disc.set_visible(!failure);
        if let LineKind::Entry { photo_key, group } = line.kind {
            let initials = (!group)
                .then(|| crate::ui::initials::InitialsPaintable::for_name(&line.title))
                .flatten();
            match initials {
                Some(paintable) => {
                    self.face.set_paintable(Some(&paintable));
                    self.face.set_visible(true);
                    self.show_avatar(None);
                }
                None => {
                    self.face.set_visible(false);
                    self.show_avatar(Some((&line.title, group)));
                }
            }
            self.waiting.set(photo_key);
            self.show_photo();
        }
    }

    /// Put the awaited photo in the face, once it is decoded.
    pub(super) fn show_photo(&self) {
        let key = self.waiting.get();
        if key == 0 {
            return;
        }
        if let Some(texture) = crate::services::avatar::photo_thumbnail(key) {
            if let Some(texture) = texture {
                self.face.set_paintable(Some(&texture));
                self.face.set_visible(true);
                self.show_avatar(None);
            }
            self.waiting.set(0);
        }
    }

    /// libadwaita's avatar for `(name, group)`, in place of the face; `None`
    /// hides it.
    fn show_avatar(&self, who: Option<(&str, bool)>) {
        let mut avatar = self.avatar.borrow_mut();
        let Some((name, group)) = who else {
            if let Some(a) = avatar.as_ref() {
                a.set_visible(false);
            }
            return;
        };
        let a = avatar.get_or_insert_with(|| {
            let a = adw::Avatar::new(FACE, None, true);
            self.disc.append(&a);
            a
        });
        a.set_text(Some(name));
        a.set_show_initials(!group);
        a.set_icon_name(group.then_some("system-users-symbolic"));
        a.set_visible(true);
    }

    /// A heading's or a note's text in place of the row; `None` hides it.
    fn show_banner(&self, line: Option<&Line>) {
        let mut banner = self.banner.borrow_mut();
        let Some(line) = line else {
            if let Some(b) = banner.as_ref() {
                b.set_visible(false);
            }
            return;
        };
        let label = banner.get_or_insert_with(|| {
            let label = gtk::Label::new(None);
            label.add_css_class("dim-label");
            self.root.append(&label);
            label
        });
        let heading = matches!(line.kind, LineKind::Heading);
        label.set_label(&line.title);
        label.set_css_classes(if heading {
            &["heading", "dim-label"]
        } else {
            &["dim-label"]
        });
        label.set_xalign(if heading { 0.0 } else { 0.5 });
        label.set_wrap(!heading);
        label.set_justify(if heading {
            gtk::Justification::Left
        } else {
            gtk::Justification::Center
        });
        label.set_margin_top(if heading { 12 } else { 24 });
        label.set_margin_bottom(if heading { 4 } else { 0 });
        label.set_margin_start(12);
        label.set_margin_end(12);
        label.set_visible(true);
    }

    /// The entry the bound line shows.
    fn idx(&self) -> Option<usize> {
        self.line
            .borrow()
            .as_ref()
            .and_then(|item| item.borrow::<Line>().idx.get())
    }
}

/// What a contact row carries when dragged to the sidebar: the book and
/// UID of each contact by its index (`None` for a group, or a card EDS
/// has not named), and the indexes selected together, which go as one.
#[derive(Default)]
pub(super) struct DragKeys {
    pub(super) keys: Vec<Option<(String, String)>>,
    pub(super) multi: Vec<usize>,
}

impl DragKeys {
    /// The drag's text for the row of contact `idx`: [`DRAG_MARK`], then a
    /// line of book and UID, tab apart, per contact.
    fn payload(&self, idx: usize) -> Option<String> {
        let idxs: Vec<usize> = if self.multi.contains(&idx) {
            self.multi.clone()
        } else {
            vec![idx]
        };
        let lines: Vec<String> = idxs
            .iter()
            .filter_map(|&i| self.keys.get(i).cloned().flatten())
            .map(|(book, uid)| format!("{book}\t{uid}"))
            .collect();
        (!lines.is_empty()).then(|| format!("{DRAG_MARK}\n{}", lines.join("\n")))
    }
}

/// The contacts in a drag's text, by book and UID; `None` when it is not
/// a drag of contacts (a message, a folder).
pub(crate) fn dragged_contacts(payload: &str) -> Option<Vec<(String, String)>> {
    let mut lines = payload.lines();
    (lines.next()? == DRAG_MARK).then(|| {
        lines
            .filter_map(|l| l.split_once('\t'))
            .map(|(book, uid)| (book.to_string(), uid.to_string()))
            .collect()
    })
}

/// A book's name in its parts: the key its account's books share, the
/// account for the line under the name, and the book's own name. "Account
/// · book" is both; a book that is its account alone ("CardDAV — a@b.org")
/// is named by its address with its service under it, and an account of
/// its own; a directory is one of the directories.
pub(super) fn split_book_name(name: &str, directory: bool) -> (String, String, String) {
    if directory {
        let directories = i18n("Directories");
        return (
            directories.clone(),
            directories,
            name.trim_start_matches("LDAP — ").to_string(),
        );
    }
    match name.split_once(" · ") {
        Some((account, own)) => (account.to_string(), account.to_string(), own.to_string()),
        None => match name.split_once(" — ") {
            Some((kind, account)) => (name.to_string(), kind.to_string(), account.to_string()),
            None => (name.to_string(), String::new(), name.to_string()),
        },
    }
}

/// Whether member `m` of group `g` is the contact `p`: the card the group
/// names, in its own book, or an address of theirs.
pub(super) fn is_member(g: &ContactDetails, m: &Member, p: &ContactDetails) -> bool {
    (!m.uid.is_empty() && p.book_uid == g.book_uid && p.eds_uid == m.uid)
        || (!m.email.is_empty()
            && p.emails
                .iter()
                .any(|e| e.value.eq_ignore_ascii_case(&m.email)))
}

/// The list view's rows: made for the lines on screen and bound to each
/// line scrolled to.
pub(super) fn row_factory(
    rows: &Rc<RefCell<HashMap<usize, Rc<ListRow>>>>,
    input: &relm4::Sender<ContactsPageInput>,
    drag: &Rc<RefCell<DragKeys>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let key = |obj: &gtk::glib::Object| obj.as_ptr() as usize;
    {
        let (rows, input, drag) = (Rc::downgrade(rows), input.clone(), Rc::downgrade(drag));
        factory.connect_setup(move |_, obj| {
            let (Some(rows), Some(item)) = (rows.upgrade(), obj.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let row = Rc::new(ListRow::new());
            // Right-click: the quick actions on this entry, built when asked.
            let right_click = gtk::GestureClick::new();
            right_click.set_button(3);
            let (weak, input) = (Rc::downgrade(&row), input.clone());
            right_click.connect_pressed(move |gesture, _, x, y| {
                let (Some(row), Some(widget)) = (weak.upgrade(), gesture.widget()) else {
                    return;
                };
                let Some(idx) = row.idx() else { return };
                let Some(list) = widget.ancestor(gtk::ListView::static_type()) else {
                    return;
                };
                let Some(at) =
                    widget.compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
                else {
                    return;
                };
                let _ = input.send(ContactsPageInput::RowMenu {
                    idx,
                    x: at.x().into(),
                    y: at.y().into(),
                });
            });
            row.root.add_controller(right_click);
            // Dragged to a book or group in the sidebar: this contact, or
            // every contact selected with it.
            let source = gtk::DragSource::new();
            source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
            {
                let (weak, drag) = (Rc::downgrade(&row), drag.clone());
                source.connect_prepare(move |_, _, _| {
                    let (row, drag) = (weak.upgrade()?, drag.upgrade()?);
                    let payload = drag.borrow().payload(row.idx()?)?;
                    Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
                });
            }
            {
                let weak = Rc::downgrade(&row);
                source.connect_drag_begin(move |src, _| {
                    if let Some(row) = weak.upgrade() {
                        src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row.root))), 0, 0);
                    }
                });
            }
            row.root.add_controller(source);
            item.set_child(Some(&row.root));
            rows.borrow_mut().insert(key(obj), row);
        });
    }
    {
        let rows = Rc::downgrade(rows);
        factory.connect_bind(move |_, obj| {
            let Some(rows) = rows.upgrade() else { return };
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(line) = item.item().and_downcast::<gtk::glib::BoxedAnyObject>() else {
                return;
            };
            let someone = line.borrow::<Line>().idx.get().is_some();
            item.set_selectable(someone);
            item.set_activatable(someone);
            let row = rows.borrow().get(&key(obj)).cloned();
            if let Some(row) = row {
                row.bind(&line);
            }
        });
    }
    {
        let rows = Rc::downgrade(rows);
        factory.connect_teardown(move |_, obj| {
            if let Some(rows) = rows.upgrade() {
                rows.borrow_mut().remove(&key(obj));
            }
        });
    }
    factory
}

/// Whether a fresh read holds what the list already shows. The stored vCard
/// carries everything parsed from it, the photo key a photo kept in a file.
/// Its REV is left out: some books (Microsoft 365's Recent Contacts) stamp
/// every card anew at every sync.
pub(super) fn same_contacts(shown: &[ContactDetails], read: &[ContactDetails]) -> bool {
    fn card(c: &ContactDetails) -> impl Iterator<Item = &str> {
        c.raw_vcard
            .lines()
            .filter(|l| !l.starts_with("REV:") && !l.starts_with("REV;"))
    }
    shown.len() == read.len()
        && shown.iter().zip(read).all(|(a, b)| {
            a.book_uid == b.book_uid
                && a.book_name == b.book_name
                && a.eds_uid == b.eds_uid
                && a.name == b.name
                && a.photo_key == b.photo_key
                && card(a).eq(card(b))
        })
}

/// The contact's avatar: their GNOME Contacts photo when they have one, the
/// usual initials disc when not.
pub(super) fn avatar_for(c: &ContactDetails, size: i32) -> adw::Avatar {
    // The list's thumbnail does for a small face, without reading the photo.
    if size <= 48 {
        if let Some(Some(texture)) = crate::services::avatar::photo_thumbnail(line_photo_key(c)) {
            let avatar = avatar_with(c, size, None);
            avatar.set_custom_image(Some(&texture));
            return avatar;
        }
    }
    avatar_with(c, size, c.photo_bytes().as_deref())
}

/// [`avatar_for`] with the photo already in hand.
pub(super) fn avatar_with(c: &ContactDetails, size: i32, photo: Option<&[u8]>) -> adw::Avatar {
    let avatar = adw::Avatar::new(size, Some(&c.name), !c.is_group());
    if c.is_group() {
        avatar.set_icon_name(Some("system-users-symbolic"));
    } else if let Some(initials) = crate::ui::initials::InitialsPaintable::for_name(&c.name) {
        // Ours, not libadwaita's: it reads "Foo Bar (Baz)" as "F(" (#627).
        avatar.set_custom_image(Some(&initials));
    }
    if let Some(photo) = photo {
        let bytes = gtk::glib::Bytes::from(photo);
        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
            avatar.set_custom_image(Some(&texture));
        }
    }
    avatar
}

/// A titled boxed list appended to the detail column; returns the list.
pub(super) fn group(detail: &gtk::Box, title: &str) -> gtk::ListBox {
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("heading");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_top(20);
    heading.set_margin_bottom(6);
    detail.append(&heading);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    detail.append(&list);
    list
}

/// A value row with its (optional) "Home"/"Work" label as the subtitle.
pub(super) fn labeled_row(l: &Labeled) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(&l.value));
    if !l.label.is_empty() {
        row.set_subtitle(&gtk::glib::markup_escape_text(&l.label));
    }
    row
}

pub(super) fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}

/// A suffix button that puts `text` on the clipboard, confirming by briefly
/// swapping its icon for a checkmark.
pub(super) fn copy_button(text: &str) -> gtk::Button {
    let b = flat_button("edit-copy-symbolic", "Copy");
    let text = text.to_string();
    b.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
        b.set_icon_name("verified-checkmark-symbolic");
        let b = b.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            b.set_icon_name("edit-copy-symbolic");
        });
    });
    b
}
