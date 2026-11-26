//! The list under the sidebar's Contacts row: each account's address books
//! with their groups under them. Picking a row picks what the contact list
//! shows; contacts dragged from the list are copied onto a book (moved, with
//! Ctrl held) or put in a group.

use adw::prelude::*;

use crate::i18n::{i18n, ni18n_f};
use crate::ui::contacts_page::{
    dragged_contacts, ContactScope, ContactTree, DropOn, TreeAction, TreeBook, TreeGroup,
};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::sidebar::SidebarInput;

/// What [`fill`] made: the rows with the scope each picks, and what the
/// sidebar folds and opens them with.
pub(super) struct Filled {
    pub(super) rows: Vec<(ContactScope, gtk::ListBoxRow)>,
    pub(super) layout: Vec<TreeRow>,
    /// Each foldable node's chevron, by its key.
    pub(super) chevrons: Vec<(String, gtk::Image)>,
}

/// A row that hides while the account or book above it is folded, sliding
/// as folder rows do.
pub(super) struct TreeRow {
    pub(super) row: gtk::ListBoxRow,
    pub(super) revealer: gtk::Revealer,
    /// The keys of the nodes above it.
    pub(super) under: Vec<String>,
}

/// The key that folds a book's groups.
fn book_key(uid: &str) -> String {
    format!("book:{uid}")
}

/// Fill `list`, the Contacts row's list in the sidebar, with `tree`: the
/// Hylki book first, then each account: one of one book as that book's
/// row, its account under its name; one of several as a row of its own
/// with its books under it. Each book's groups are under the book, and an
/// account or book folds as a folder with sub-folders does (`folded` holds
/// the keys of those folded). In the icon rail (`collapsed`) the books
/// show as icons alone, with no accounts or groups.
pub(super) fn fill(
    list: &gtk::ListBox,
    tree: &ContactTree,
    collapsed: bool,
    inset: bool,
    folded: &std::collections::HashSet<String>,
    input: &relm4::Sender<SidebarInput>,
) -> Filled {
    let mut out = Filled {
        rows: Vec::new(),
        layout: Vec::new(),
        chevrons: Vec::new(),
    };
    // The Hylki book heads the list, then the books of no account; the
    // accounts keep their order, and the books of one account go together.
    let mut books: Vec<&TreeBook> = tree.books.iter().collect();
    books.sort_by_key(|b| {
        (
            b.uid != crate::contacts::local::LOCAL_BOOK_UID,
            !b.account.is_empty(),
        )
    });
    let mut accounts: Vec<(&str, Vec<&TreeBook>)> = Vec::new();
    for book in books {
        match accounts
            .iter_mut()
            .find(|(key, _)| !key.is_empty() && *key == book.key)
        {
            Some((_, books)) => books.push(book),
            None => accounts.push((book.key.as_str(), vec![book])),
        }
    }
    let ctx = Ctx {
        list,
        collapsed,
        inset,
        folded,
        input,
    };
    for (key, books) in accounts {
        // An account of one book is that book's row, the account under its
        // name; one of several is a row of its own, its books folding
        // under it.
        if books.len() < 2 || collapsed {
            for book in books {
                ctx.book(&mut out, book, 0, true, Vec::new());
            }
            continue;
        }
        let fold = account_key(key);
        let account = &books[0].account;
        // "Microsoft 365 — a@b.org": the address, the service under it.
        let (name, place) = match account.split_once(" — ") {
            Some((kind, address)) => (address.to_string(), kind.to_string()),
            None => (
                account.clone(),
                ni18n_f(
                    "{n} address book",
                    "{n} address books",
                    books.len() as u32,
                    &[("n", &books.len().to_string())],
                ),
            ),
        };
        let total = books
            .iter()
            .filter(|b| !b.directory)
            .map(|b| b.count)
            .sum::<usize>();
        let directories = books.iter().all(|b| b.directory);
        let (button, chevron) = expander(&fold, folded, input);
        out.chevrons.push((fold.clone(), chevron));
        let icon = if directories {
            "network-server-symbolic"
        } else {
            "x-office-address-book-symbolic"
        };
        let r = row(
            &name,
            &place,
            account,
            icon,
            (!directories).then_some(total),
            Some(button.upcast()),
            0,
            false,
            inset,
        );
        list.append(&r);
        out.layout.push(slider(&r, Vec::new(), folded));
        out.rows.push((ContactScope::Account(key.to_string()), r));
        for book in books {
            ctx.book(&mut out, book, 1, false, vec![fold.clone()]);
        }
    }
    // Built at full height in one pass, then sliding for the clicks after.
    let revealers: Vec<gtk::Revealer> = out.layout.iter().map(|t| t.revealer.clone()).collect();
    gtk::glib::idle_add_local_once(move || {
        for r in revealers {
            r.set_transition_duration(200);
        }
    });
    out
}

/// What every row of one fill shares.
struct Ctx<'a> {
    list: &'a gtk::ListBox,
    collapsed: bool,
    inset: bool,
    folded: &'a std::collections::HashSet<String>,
    input: &'a relm4::Sender<SidebarInput>,
}

impl Ctx<'_> {
    /// A book's row, at `depth`, with its account under its name when
    /// `place` (not under its account's own row), and its groups under it;
    /// hidden while a node it is `under` is folded.
    fn book(
        &self,
        out: &mut Filled,
        book: &TreeBook,
        depth: i32,
        place: bool,
        mut under: Vec<String>,
    ) {
        let (list, collapsed, inset, folded, input) = (
            self.list,
            self.collapsed,
            self.inset,
            self.folded,
            self.input,
        );
        let icon = if book.directory {
            "network-server-symbolic"
        } else {
            "x-office-address-book-symbolic"
        };
        // Where the book is kept, under its name: a book called "card" or
        // "Contacts" says little on its own.
        let place = if !place {
            String::new()
        } else if !book.account.is_empty() {
            book.account.clone()
        } else if book.uid == crate::contacts::local::LOCAL_BOOK_UID {
            i18n("Hylki Address Book")
        } else {
            String::new()
        };
        let tip = if book.account.is_empty() {
            book.name.clone()
        } else {
            format!("{} · {}", book.name, book.account)
        };
        let count = (!book.directory).then_some(book.count);
        let key = book_key(&book.uid);
        let expander =
            (!collapsed && !book.groups.is_empty()).then(|| expander(&key, folded, input));
        if let Some((_, chevron)) = &expander {
            out.chevrons.push((key.clone(), chevron.clone()));
        }
        let lead = expander.map(|(button, _)| button.upcast::<gtk::Widget>());
        let r = row(
            &book.name, &place, &tip, icon, count, lead, depth, collapsed, inset,
        );
        if book.writable && !book.directory {
            drop_target(&r, DropOn::Book(book.uid.clone()), input);
        }
        if !book.directory {
            book_menu(&r, book, input);
        }
        list.append(&r);
        out.layout.push(slider(&r, under.clone(), folded));
        out.rows.push((ContactScope::Book(book.uid.clone()), r));
        if collapsed {
            return;
        }
        under.push(key);
        for g in &book.groups {
            self.group(out, g, depth + 1, &under);
        }
    }

    /// A group's row under its book, at `depth`, hidden while a node it is
    /// `under` is folded.
    fn group(&self, out: &mut Filled, g: &TreeGroup, depth: i32, under: &[String]) {
        let (list, inset, folded, input) = (self.list, self.inset, self.folded, self.input);
        let r = row(
            &g.name,
            "",
            &g.name,
            "system-users-symbolic",
            Some(g.count),
            None,
            depth,
            false,
            inset,
        );
        if g.editable {
            drop_target(
                &r,
                DropOn::Group {
                    book: g.book.clone(),
                    uid: g.uid.clone(),
                },
                input,
            );
        }
        let (s, book, uid, editable, any) = (
            input.clone(),
            g.book.clone(),
            g.uid.clone(),
            g.editable,
            g.count > 0,
        );
        menu_on(&r, move || {
            let act = |action: TreeAction| {
                let s = s.clone();
                move || {
                    let _ = s.send(SidebarInput::ContactTreeMenu(action.clone()));
                }
            };
            let (b, u) = (book.clone(), uid.clone());
            let mut first = vec![MenuEntry::new(
                i18n("New Message"),
                act(TreeAction::ComposeGroup { book: b, uid: u }),
            )
            .icon("mail-message-new-symbolic")
            .enabled(any)];
            if !editable {
                return vec![first];
            }
            first.push(
                MenuEntry::new(
                    i18n("Edit Group"),
                    act(TreeAction::EditGroup {
                        book: book.clone(),
                        uid: uid.clone(),
                    }),
                )
                .icon("document-edit-symbolic"),
            );
            let delete = MenuEntry::new(
                i18n("Delete Group…"),
                act(TreeAction::DeleteGroup {
                    book: book.clone(),
                    uid: uid.clone(),
                }),
            )
            .icon("user-trash-symbolic");
            vec![first, vec![delete]]
        });
        list.append(&r);
        out.layout.push(slider(&r, under.to_vec(), folded));
        out.rows.push((
            ContactScope::Group {
                book: g.book.clone(),
                uid: g.uid.clone(),
            },
            r,
        ));
    }
}

/// A book's menu: a new contact or group in it, when it takes them, and
/// renaming it in Hylki.
fn book_menu(r: &gtk::ListBoxRow, book: &TreeBook, input: &relm4::Sender<SidebarInput>) {
    let (s, uid, writable, groups) = (
        input.clone(),
        book.uid.clone(),
        book.writable,
        book.takes_groups,
    );
    menu_on(r, move || {
        let act = |action: TreeAction| {
            let s = s.clone();
            move || {
                let _ = s.send(SidebarInput::ContactTreeMenu(action.clone()));
            }
        };
        let mut sections = Vec::new();
        if writable {
            let mut entries = vec![MenuEntry::new(
                i18n("New Contact"),
                act(TreeAction::NewContact(uid.clone())),
            )
            .icon("contact-new-symbolic")];
            if groups {
                entries.push(
                    MenuEntry::new(i18n("New Group"), act(TreeAction::NewGroup(uid.clone())))
                        .icon("system-users-symbolic"),
                );
            }
            sections.push(entries);
        }
        sections.push(vec![MenuEntry::new(
            i18n("Rename…"),
            act(TreeAction::RenameBook(uid.clone())),
        )
        .icon("document-edit-symbolic")]);
        sections
    });
}

/// The key that folds an account's books.
fn account_key(key: &str) -> String {
    format!("account:{key}")
}

/// Put `row`'s content in a revealer, shown unless a node it is `under` is
/// folded.
fn slider(
    row: &gtk::ListBoxRow,
    under: Vec<String>,
    folded: &std::collections::HashSet<String>,
) -> TreeRow {
    let hidden = under.iter().any(|k| folded.contains(k));
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(0);
    if let Some(content) = row.child() {
        row.set_child(gtk::Widget::NONE);
        revealer.set_child(Some(&content));
    }
    revealer.set_reveal_child(!hidden);
    row.set_child(Some(&revealer));
    row.set_visible(!hidden);
    TreeRow {
        row: row.clone(),
        revealer,
        under,
    }
}

/// The caret that folds node `key`, as a folder's that has sub-folders:
/// pointing on when folded, turned down when open.
fn expander(
    key: &str,
    folded: &std::collections::HashSet<String>,
    input: &relm4::Sender<SidebarInput>,
) -> (gtk::Button, gtk::Image) {
    let img = gtk::Image::from_icon_name("pan-end-symbolic");
    img.add_css_class("tree-expander-icon");
    if !folded.contains(key) {
        img.add_css_class("open");
    }
    img.set_pixel_size(12);
    let btn = gtk::Button::new();
    btn.set_child(Some(&img));
    btn.add_css_class("flat");
    btn.add_css_class("tree-expander");
    btn.set_valign(gtk::Align::Center);
    btn.set_tooltip_text(Some(i18n("Show or hide").as_str()));
    let (s, key) = (input.clone(), key.to_string());
    btn.connect_clicked(move |_| {
        let _ = s.send(SidebarInput::ToggleContactNode(key.clone()));
    });
    (btn, img)
}

/// A row shaped as the other unified rows' are: the caret slot (a caret
/// for a book with groups, else a spacer as wide), an icon, the name with
/// `place` (the book's account) under it and, when `count` is given, how
/// many it holds; `depth` 1 sets a group under its book.
#[allow(clippy::too_many_arguments)]
fn row(
    name: &str,
    place: &str,
    tip: &str,
    icon: &str,
    count: Option<usize>,
    expander: Option<gtk::Widget>,
    depth: i32,
    collapsed: bool,
    inset: bool,
) -> gtk::ListBoxRow {
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class("folder-icon");
    crate::ui::sidebar::pin_icon_size(&img);
    let lead = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    if !collapsed {
        let slot: gtk::Widget = expander.unwrap_or_else(|| {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_width_request(crate::ui::sidebar::TREE_EXPANDER_WIDTH);
            spacer.upcast()
        });
        slot.set_margin_start(depth * crate::ui::sidebar::TREE_EXPANDER_WIDTH);
        lead.append(&slot);
    }
    lead.append(&img);
    let (row, badge) =
        crate::ui::sidebar::build_unified_sub_row(&lead, name, tip, 0, collapsed, inset, false);
    badge.set_visible(false);
    if collapsed {
        return row;
    }
    let Some(hbox) = row.child().and_downcast::<gtk::Box>() else {
        return row;
    };
    // The account under the name, small and dim, as a mail row's preview
    // sits under its subject.
    if !place.is_empty() {
        if let Some(label) = lead.next_sibling().and_downcast::<gtk::Label>() {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_hexpand(true);
            column.set_valign(gtk::Align::Center);
            column.set_margin_start(label.margin_start());
            label.set_margin_start(0);
            hbox.remove(&label);
            column.append(&label);
            let sub = gtk::Label::new(Some(place));
            sub.add_css_class("dim-label");
            sub.add_css_class("caption");
            sub.set_halign(gtk::Align::Start);
            sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
            column.append(&sub);
            hbox.insert_child_after(&column, Some(&lead));
        }
    }
    // A count, not unread mail: dim, where the unread chip would be.
    if let Some(n) = count {
        let label = gtk::Label::new(Some(&n.to_string()));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        hbox.append(&label);
    }
    row
}

/// A right-click menu on `row`, made by `sections` each time it opens.
fn menu_on(row: &gtk::ListBoxRow, sections: impl Fn() -> Vec<Vec<MenuEntry>> + 'static) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    click.connect_pressed(move |gesture, _, x, y| {
        if let Some(widget) = gesture.widget() {
            show_context_menu(&widget, x, y, sections());
        }
    });
    row.add_controller(click);
}

/// Contacts dragged from the list land on `row`: copied, or moved while
/// Ctrl is held. The row is outlined while they are over it.
fn drop_target(row: &gtk::ListBoxRow, on: DropOn, input: &relm4::Sender<SidebarInput>) {
    let target = gtk::DropTarget::new(
        String::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    // Ctrl as the keyboard has it: on Wayland a drop's own event carries no
    // keys.
    let held = |t: &gtk::DropTarget| {
        let keyboard = t
            .widget()
            .and_then(|w| w.display().default_seat())
            .and_then(|seat| seat.keyboard());
        let state = keyboard
            .map(|k| k.modifier_state())
            .unwrap_or(t.current_event_state());
        (state | t.current_event_state()).contains(gtk::gdk::ModifierType::CONTROL_MASK)
    };
    {
        let row = row.downgrade();
        target.connect_enter(move |t, _, _| {
            if let Some(row) = row.upgrade() {
                row.add_css_class("drop-into");
            }
            if held(t) {
                gtk::gdk::DragAction::MOVE
            } else {
                gtk::gdk::DragAction::COPY
            }
        });
    }
    target.connect_motion(move |t, _, _| {
        if held(t) {
            gtk::gdk::DragAction::MOVE
        } else {
            gtk::gdk::DragAction::COPY
        }
    });
    {
        let row = row.downgrade();
        target.connect_leave(move |_| {
            if let Some(row) = row.upgrade() {
                row.remove_css_class("drop-into");
            }
        });
    }
    {
        let (row, s) = (row.downgrade(), input.clone());
        target.connect_drop(move |t, value, _, _| {
            if let Some(row) = row.upgrade() {
                row.remove_css_class("drop-into");
            }
            let Some(keys) = value
                .get::<String>()
                .ok()
                .as_deref()
                .and_then(dragged_contacts)
            else {
                return false;
            };
            let _ = s.send(SidebarInput::ContactTreeDrop {
                keys,
                on: on.clone(),
                moving: held(t),
            });
            true
        });
    }
    row.add_controller(target);
}
