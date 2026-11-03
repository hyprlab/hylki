//! The Contacts page's state and messages, and the types the sidebar's
//! Contacts tree shares with it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use relm4::prelude::*;

use crate::contacts::{ContactDetails, GroupEdit, GroupFormat, Member};
use crate::i18n::i18n;

use super::forms::{AddressForm, BirthdayForm, ValueRows};
use super::menus::FilterMenu;
use super::rows::{DragKeys, ListRow};

/// Where the people found in the LDAP directories (#307) start in the
/// index space `selected` and `row_map` share with `contacts`: an index this
/// high or higher is `directory_hits[index - DIRECTORY]`.
pub(super) const DIRECTORY: usize = usize::MAX / 2;

/// At most this many directory entries are listed for one search.
pub(super) const DIRECTORY_LIMIT: usize = 50;

/// What the list shows, picked in the sidebar: every contact, one address
/// book's, or one group's people (the group first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ContactScope {
    #[default]
    All,
    Book(String),
    Group {
        book: String,
        uid: String,
    },
    /// Every book of one account, by its [`TreeBook::key`].
    Account(String),
}

/// The address books and their groups, as the sidebar lists them while
/// Contacts is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContactTree {
    /// Every contact, groups left out.
    pub(super) total: usize,
    pub(crate) books: Vec<TreeBook>,
    pub(crate) scope: ContactScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeBook {
    pub(crate) uid: String,
    /// The account the book is in, for the line under its name ("CardDAV —
    /// a@b.org", or the service of a book that is its account alone);
    /// empty for the Hylki book.
    pub(crate) account: String,
    /// What the books of one account share: the books with one key are
    /// listed under their account.
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) count: usize,
    /// Hylki can write to it, so contacts can be copied or moved in.
    pub(crate) writable: bool,
    pub(crate) takes_groups: bool,
    /// An LDAP directory, searched rather than listed.
    pub(crate) directory: bool,
    pub(crate) groups: Vec<TreeGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeGroup {
    pub(crate) book: String,
    pub(crate) uid: String,
    pub(crate) name: String,
    pub(crate) count: usize,
    pub(crate) editable: bool,
}

/// Where contacts dragged to the sidebar land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropOn {
    Book(String),
    Group { book: String, uid: String },
}

/// What a right-click on the sidebar's book or group asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TreeAction {
    NewContact(String),
    NewGroup(String),
    EditGroup {
        book: String,
        uid: String,
    },
    DeleteGroup {
        book: String,
        uid: String,
    },
    ComposeGroup {
        book: String,
        uid: String,
    },
    /// Give the book a name of its own in Hylki.
    RenameBook(String),
}

/// The start of what a contact row carries when dragged, before a line of
/// book and UID per contact.
pub(super) const DRAG_MARK: &str = "hylki-contacts";

/// The "Show only" filters of the sort and filter button, as the message
/// list has for mail. They narrow the people and leave the groups, except
/// Groups, which leaves the groups alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContactFilter {
    Groups,
    Photo,
    Email,
    Phone,
    Address,
    Birthday,
    Company,
    NoGroup,
}

impl ContactFilter {
    pub(crate) const ALL: [ContactFilter; 8] = [
        ContactFilter::Groups,
        ContactFilter::Photo,
        ContactFilter::Email,
        ContactFilter::Phone,
        ContactFilter::Address,
        ContactFilter::Birthday,
        ContactFilter::Company,
        ContactFilter::NoGroup,
    ];

    pub(super) fn label(self) -> String {
        match self {
            ContactFilter::Groups => i18n("Groups"),
            ContactFilter::Photo => i18n("Has Photo"),
            ContactFilter::Email => i18n("Has Email"),
            ContactFilter::Phone => i18n("Has Phone"),
            ContactFilter::Address => i18n("Has Address"),
            ContactFilter::Birthday => i18n("Has Birthday"),
            ContactFilter::Company => i18n("Has Company"),
            ContactFilter::NoGroup => i18n("Not in a Group"),
        }
    }

    pub(super) fn bit(self) -> u8 {
        1 << ContactFilter::ALL
            .iter()
            .position(|f| *f == self)
            .unwrap_or(0)
    }

    /// Whether `c` passes; `grouped` holds every group member's address,
    /// lower case, and the cards members are named by.
    pub(super) fn passes(
        self,
        c: &ContactDetails,
        grouped: &std::collections::HashSet<String>,
    ) -> bool {
        match self {
            ContactFilter::Groups => c.is_group(),
            ContactFilter::Photo => c.has_photo(),
            ContactFilter::Email => !c.emails.is_empty(),
            ContactFilter::Phone => !c.phones.is_empty(),
            ContactFilter::Address => !c.addresses.is_empty(),
            ContactFilter::Birthday => !c.birthday.is_empty(),
            ContactFilter::Company => !c.org.is_empty(),
            ContactFilter::NoGroup => {
                !grouped.contains(&c.grouped_key())
                    && !c
                        .emails
                        .iter()
                        .any(|e| grouped.contains(&e.value.to_lowercase()))
            }
        }
    }
}

/// How the contact list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContactSort {
    FirstName,
    LastName,
    Email,
}

/// Live handles into the open contact form.
pub(super) struct ContactForm {
    pub(super) name: adw::EntryRow,
    pub(super) nickname: adw::EntryRow,
    pub(super) org: adw::EntryRow,
    pub(super) title: adw::EntryRow,
    pub(super) note: gtk::TextView,
    pub(super) emails: ValueRows,
    pub(super) phones: ValueRows,
    pub(super) urls: ValueRows,
    pub(super) addresses: Rc<RefCell<Vec<AddressForm>>>,
    pub(super) birthday: BirthdayForm,
    /// What happens to the photo: kept, removed or a new one.
    pub(super) photo: Rc<RefCell<crate::contacts::PhotoEdit>>,
    /// The book a new contact goes to; `None` when editing one (it stays in
    /// its book) or before the books are known.
    pub(super) book: Option<adw::ComboRow>,
}

/// Live handles into the open group form.
pub(super) struct GroupForm {
    pub(super) name: adw::EntryRow,
    pub(super) hide: adw::SwitchRow,
    /// The members as the form has them, in order.
    pub(super) members: Rc<RefCell<Vec<Member>>>,
    /// A new group's address book, picked from `books`, those that keep
    /// groups.
    pub(super) book: Option<adw::ComboRow>,
    pub(super) books: Vec<crate::contacts::Book>,
}

/// The form open in the detail pane.
pub(super) enum Editor {
    Contact(ContactForm),
    Group(GroupForm),
}

pub(crate) struct ContactsPage {
    pub(super) contacts: Vec<ContactDetails>,
    pub(super) query: String,
    pub(super) sort: ContactSort,
    /// What the list shows, picked in the sidebar.
    pub(super) scope: ContactScope,
    /// The tree last handed to the sidebar, so an unchanged one is not.
    pub(super) tree: Option<ContactTree>,
    /// What a row being dragged carries: each contact's book and UID, by
    /// index, and the indexes selected together.
    pub(super) drag: Rc<RefCell<DragKeys>>,
    /// The "Show only" filters on, by [`ContactFilter::bit`].
    pub(super) filters: u8,
    /// The sort and filter button's popover, kept in step with `sort`,
    /// `filters` and `hidden_books`.
    pub(super) filter_menu: Option<FilterMenu>,
    /// The address book dropdown over the list is shown: the sidebar has
    /// no books under a Contacts row to pick from.
    pub(super) book_picker: bool,
    /// The dropdown's labels as last set, and a guard held while they or
    /// its choice are set from here, which fires its notify.
    pub(super) picker_labels: Vec<String>,
    pub(super) picker_guard: Rc<std::cell::Cell<bool>>,
    /// The search bar is shown: opened, or kept open by its setting.
    pub(super) search_open: bool,
    pub(super) search_pinned: bool,
    /// When the bar last closed: the click on the search button that took
    /// focus from an empty search, and closed the bar, must not open it
    /// again (as in the message list).
    pub(super) search_closed_at: Option<std::time::Instant>,
    /// Index into `contacts` of the contact shown in the detail pane.
    pub(super) selected: Option<usize>,
    /// The contacts selected together, as indexes into `contacts`, while
    /// more than one is: the pane then offers what can be done to them all.
    pub(super) multi: Vec<usize>,
    /// List position → index into `contacts` (or a directory hit, see
    /// [`DIRECTORY`]); `None` for a line that is nobody, such as a heading.
    pub(super) row_map: Vec<Option<usize>>,
    /// The list's lines, shown by a list view that builds rows only for the
    /// lines on screen: a thousand contacts took most of a second to build
    /// as rows, at every visit and every key typed in the search (#477).
    pub(super) store: gtk::gio::ListStore,
    pub(super) selection: gtk::MultiSelection,
    /// The row widgets the list view has made, by their list item.
    pub(super) rows: Rc<RefCell<HashMap<usize, Rc<ListRow>>>>,
    /// One list item per contact, in step with `contacts`. The same item
    /// keeps its row through a search, a sort or a re-read that leaves it
    /// as it was: the view makes rows only for items it has not shown.
    pub(super) items: Vec<gtk::glib::BoxedAnyObject>,
    /// A load is underway (the page was just opened).
    pub(super) loading: bool,
    /// The editor form, while open. `editing_target` is the contact or group
    /// being edited, `None` for a brand-new one.
    pub(super) editor: Option<Editor>,
    pub(super) editing_target: Option<usize>,
    /// EDS UIDs deleted here but possibly still in EDS's local cache — a
    /// re-read races the CardDAV flush, and without these tombstones a
    /// deleted contact pops right back into the list. A tombstone lives
    /// until a fresh read no longer contains its contact. Keyed by book and
    /// UID: an imported card keeps the UID of the book it came from.
    pub(super) deleted: std::collections::HashSet<(String, String)>,
    /// The books new contacts can go to, the chosen default first; the
    /// Hylki book is always among them.
    pub(super) books: Vec<crate::contacts::Book>,
    /// The books the sidebar lists: (book UID, name).
    pub(super) filter_books: Vec<(String, String)>,
    /// The people the LDAP directories hold for the search (#307), listed
    /// after the contacts under their own heading. Read-only: they can be
    /// written to and added to Contacts, not edited.
    pub(super) directory_hits: Vec<ContactDetails>,
    /// Bumped at every change to the search, so an answer to an older one
    /// is dropped.
    pub(super) directory_generation: u64,
    /// The directories switched on, as (UID, name): the list shows, with
    /// its search box, even with no contacts of its own, and the book
    /// filter offers each after the address books.
    pub(super) directories: Vec<(String, String)>,
    /// The directories that did not answer the search, listed under the
    /// people found, so a search that finds nobody says why (#307).
    pub(super) directory_failures: Vec<crate::contacts::directory::DirectoryFailure>,
    /// The composer's recipient suggestions (Contacts and mail history),
    /// shared with an open group form, which offers them as people to add.
    /// Read again each time a group form opens; until the first read lands
    /// the form offers the contacts listed here.
    pub(super) suggestions: Rc<RefCell<Vec<crate::contacts::Suggestion>>>,
    /// A read that came in while a form was open, applied once it closes:
    /// a sync in the background must not throw away what is being typed.
    pub(super) pending: Option<Vec<ContactDetails>>,
    /// The photos handed to a decoding thread that has not finished them.
    pub(super) decoding: std::collections::HashSet<u64>,
}

#[derive(Debug)]
pub(crate) enum ContactsPageInput {
    /// Fresh EDS read: replace everything (keeps the selection when possible).
    SetContacts(Vec<ContactDetails>),
    /// The page was just opened; a read is on its way.
    SetLoading,
    Query(String),
    SetSort(ContactSort),
    /// The sidebar picked what the list shows.
    SetScope(ContactScope),
    /// Contacts, by book and UID, dropped on a book or group in the
    /// sidebar: copied there, or moved (Ctrl held) after a question.
    DropContacts {
        keys: Vec<(String, String)>,
        on: DropOn,
        moving: bool,
    },
    /// A right-click choice on the sidebar's book or group.
    Tree(TreeAction),
    /// Take these people (indexes into `contacts`) out of the group shown.
    RemoveFromGroup(Vec<usize>),
    /// A "Show only" filter turned on or off.
    SetFilter(ContactFilter, bool),
    ClearFilters,
    /// The search button, Ctrl+F or `/`: open the search, or close it.
    FocusSearch,
    /// Esc in the search, or focus left it empty: close it and clear it.
    CloseSearch,
    /// The setting that keeps the search field open.
    SetSearchPinned(bool),
    /// Show the address book dropdown over the list, or not.
    SetBookPicker(bool),
    /// The list's selection changed.
    SelectionChanged,
    /// Select the first `n` entries of the list (a showcase).
    SelectFirst(u32),
    /// A right-click on the line for this entry, at (x, y) on the list.
    RowMenu {
        idx: usize,
        x: f64,
        y: f64,
    },
    Compose(String),
    OpenUrl(String),
    /// Edit the selected contact / a specific one (context menu).
    Edit,
    EditIndex(usize),
    /// The header's "+": a blank editor for a new contact, or a new group
    /// (always in the Hylki book).
    NewContact,
    NewGroup,
    /// Write to everyone in the group at this index.
    ComposeGroup(usize),
    /// Put a person's first address in a group (both indexes into
    /// `contacts`).
    AddToGroup {
        group: usize,
        person: usize,
    },
    /// The header's import button: choose `.vcf` files for the Hylki book.
    ImportContacts,
    CancelEdit,
    SaveEdit,
    OpenInGnome(usize),
    DeleteRequest(usize),
    /// The Delete key on the list: ask to delete the contact shown.
    DeleteSelected,
    /// The contacts to delete, by book and UID: the list may have been
    /// re-read and re-sorted while the dialog was open.
    DeleteConfirmed(Vec<(String, String)>),
    /// The entries selected together: put the people in the group at this
    /// index, ask to move them to this book or to delete them all.
    BulkAddToGroup(usize),
    BulkMoveRequest(String),
    /// Ask to move the contact at this index to the book `to`.
    MoveRequest {
        idx: usize,
        to: String,
    },
    /// Move the contact being edited, by book and UID, to the book `to`
    /// as `vcard` (its card with the edits, made for the new book),
    /// confirmed.
    EditMoveConfirmed {
        to: String,
        key: (String, String),
        vcard: String,
    },
    /// Move these people, by book and UID, to this book, confirmed.
    BulkMoveConfirmed {
        to: String,
        keys: Vec<(String, String)>,
    },
    BulkDeleteRequest,
    /// A write failed: what was deleted or moved here may still be in its
    /// book, so the next read lists whatever is there.
    WriteFailed,
    /// The card's photo was clicked: expand it in the app lightbox.
    OpenPhoto {
        name: String,
        data: Vec<u8>,
    },
    /// The books new contacts can go to, freshly read, default first.
    SetBooks(Vec<crate::contacts::Book>),
    /// The LDAP directories switched on, as (UID, name).
    DirectoriesKnown(Vec<(String, String)>),
    /// The search paused: ask the directories, unless it changed since.
    DirectoryLookup {
        generation: u64,
        query: String,
    },
    DirectoryResults {
        generation: u64,
        hits: Vec<ContactDetails>,
        failed: Vec<crate::contacts::directory::DirectoryFailure>,
    },
    /// Copy a directory entry into the default address book.
    AddDirectoryHit(usize),
    /// Put this in the search box, as if typed.
    SetSearch(String),
    /// The composer's suggestions, freshly read for a group form.
    SuggestionsLoaded(Vec<crate::contacts::Suggestion>),
    /// Show the book at this place in the sidebar (1 is the first), or
    /// every book (0).
    PickBook(u32),
    /// More list photos are decoded and cached.
    PhotosDecoded,
}

#[derive(Debug)]
pub(crate) enum ContactsPageOutput {
    /// Start a message to this address, or these (a group).
    Compose(String),
    /// Start a message with these recipients in Bcc: a group that hides
    /// its members' addresses.
    ComposeBcc(String),
    /// The header's sidebar button (same spot as the message list's).
    ToggleSidebar,
    /// Persist an edit: the patched vCard back to its book.
    SaveContact { book_uid: String, vcard: String },
    /// Create a brand-new contact in the book picked in the editor
    /// (`None`: the default book).
    CreateContact {
        book_uid: Option<String>,
        vcard: String,
    },
    /// Write a group: a change to `original`, its stored card, or a new
    /// one when there is none.
    SaveGroup {
        book_uid: String,
        format: GroupFormat,
        original: Option<String>,
        edit: GroupEdit,
    },
    /// Move a group: made in the book `to` from `edit`, then the one with
    /// UID `uid` deleted from the book `from`.
    MoveGroup {
        from: String,
        uid: String,
        to: String,
        format: GroupFormat,
        edit: GroupEdit,
    },
    /// Delete these, by book and UID, already confirmed by the user.
    DeleteContacts(Vec<(String, String)>),
    /// Move these people to the book `to`: (book, UID, the card for the
    /// new book), confirmed.
    MoveContacts {
        to: String,
        cards: Vec<(String, String, String)>,
    },
    /// Copy these cards (made for the book) into the book `to`.
    CopyContacts { to: String, cards: Vec<String> },
    /// The books and groups changed: the sidebar lists them anew.
    Tree(ContactTree),
    /// Name an address book in Hylki (`None`: its own name again).
    RenameBook { uid: String, name: Option<String> },
    /// `.vcf` files chosen for import into the local Hylki book.
    ImportContacts(Vec<std::path::PathBuf>),
    /// Show the contact's photo in the app lightbox.
    ShowPhoto { name: String, data: Vec<u8> },
    /// Read the composer's suggestions for a group form
    /// ([`ContactsPageInput::SuggestionsLoaded`] brings them).
    WantSuggestions,
}
