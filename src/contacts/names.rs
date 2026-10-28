//! The names saved in the address books, shown instead of the names mail
//! gives itself when Settings → Contacts asks for it (#541).
//!
//! Only books kept by hand count: a book that fills itself from mail
//! would hand back whatever name a message once claimed. So a forged
//! name on an unknown address stays what it is, while a contact always
//! looks the same.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use super::read::read_contacts_in;
use super::registry::registry_books;

thread_local! {
    /// Lower-cased address → the contact's name. Empty while the switch
    /// is off. Read on the main thread, where everything that draws a
    /// name runs.
    static NAMES: RefCell<Rc<HashMap<String, String>>> = RefCell::default();
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The saved contacts' names by lower-cased address, read from the books.
/// A contact without a name of its own is left out, so the mail's name
/// still shows for it. Blocking: books are SQLite files.
pub(crate) fn read_saved_names() -> HashMap<String, String> {
    let mut names = HashMap::new();
    for c in read_contacts_in(&registry_books(), true).0 {
        let name = c.name.trim();
        let email = c.email.trim();
        if name.is_empty() || name.eq_ignore_ascii_case(email) {
            continue;
        }
        names.insert(email.to_lowercase(), name.to_string());
    }
    names
}

/// Use `names` from now on (empty: show the mail's own). True when they
/// differ from the ones in use, and what is on screen wants drawing again.
pub(crate) fn set_contact_names(names: HashMap<String, String>) -> bool {
    NAMES.with(|current| {
        if **current.borrow() == names {
            return false;
        }
        *current.borrow_mut() = Rc::new(names);
        GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
        true
    })
}

/// How many times the names in use have changed: a render token for views
/// that cache their output.
pub(crate) fn contact_names_generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// The saved name for `addr`, if the switch is on and it has one.
pub(crate) fn contact_name(addr: &str) -> Option<String> {
    let addr = addr.trim();
    if addr.is_empty() {
        return None;
    }
    NAMES.with(|names| {
        let names = names.borrow();
        if names.is_empty() {
            return None;
        }
        names.get(&addr.to_lowercase()).cloned()
    })
}

/// What to call the sender or recipient `name` <`addr`>: the saved name
/// when there is one, else the name the mail gives.
pub(crate) fn shown_name<'a>(name: &'a str, addr: &str) -> Cow<'a, str> {
    match contact_name(addr) {
        Some(saved) => Cow::Owned(saved),
        None => Cow::Borrowed(name),
    }
}

/// [`shown_name`], or the bare address where neither names anyone.
pub(crate) fn shown_name_or_addr<'a>(name: &'a str, addr: &'a str) -> Cow<'a, str> {
    match shown_name(name, addr) {
        n if n.trim().is_empty() => Cow::Borrowed(addr),
        n => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_name_replaces_the_mails_only_for_its_address() {
        set_contact_names(HashMap::from([(
            "ann@example.com".to_string(),
            "Ann Lee".to_string(),
        )]));
        assert_eq!(shown_name("PayPal Support", "Ann@Example.com"), "Ann Lee");
        assert_eq!(shown_name("Ann Lee", "ann@evil.example"), "Ann Lee");
        assert_eq!(
            shown_name("PayPal Support", "x@evil.example"),
            "PayPal Support"
        );
        assert_eq!(shown_name("", ""), "");
        let before = contact_names_generation();
        assert!(!set_contact_names(HashMap::from([(
            "ann@example.com".to_string(),
            "Ann Lee".to_string(),
        )])));
        assert!(set_contact_names(HashMap::new()));
        assert_eq!(contact_names_generation(), before.wrapping_add(1));
        assert_eq!(
            shown_name("PayPal Support", "ann@example.com"),
            "PayPal Support"
        );
    }
}
