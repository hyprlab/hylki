//! The people in the mail who are not contacts yet, to pick from and import
//! (#677). The addresses come from the cache's mail history, with how many
//! messages each is on.

use std::collections::HashSet;

use super::details::Labeled;
use super::edit::{new_vcard, ContactEdit};
use super::read::Book;
use crate::cache::{AddressScope, Cache};

/// What the Import from Mail window opens with: what the app knows at
/// once. The books and the saved contacts take a while to read, so the
/// window reads them itself, with [`books_and_saved`].
#[derive(Debug)]
pub(crate) struct FromMailStart {
    /// The demo's books and contacts, not the system's.
    pub(crate) demo: bool,
    /// The book to start on, when the import was asked for in one.
    pub(crate) book: Option<String>,
    /// The accounts and folders to look in, each account followed by its
    /// folders.
    pub(crate) places: Vec<Place>,
    /// The user's own addresses, lower case.
    pub(crate) own: HashSet<String>,
    /// Trash and Junk, by account and path.
    pub(crate) skip: HashSet<(u32, String)>,
}

/// An account, or one of its folders, to look in.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub(crate) account: u32,
    /// The folder's path; `None` for the whole account.
    pub(crate) folder: Option<String>,
    /// What the list of places shows: the account's label, or the
    /// folder's name under it.
    pub(crate) name: String,
    /// What the row shows once it is picked.
    pub(crate) label: String,
}

/// Someone the mail has an address for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MailPerson {
    /// The name mail gives them, empty when it gives none.
    pub(crate) name: String,
    /// Lower case.
    pub(crate) email: String,
    /// Messages the address is on, sent or received.
    pub(crate) seen: u32,
}

/// The people in the mail `scope` takes in, most seen first, less the
/// user's `own` addresses, everyone in `saved` (lower case) and addresses
/// that only send. Blocking: it reads every message in the cache.
pub(crate) fn people_in_mail(
    own: &HashSet<String>,
    saved: &HashSet<String>,
    scope: &AddressScope,
) -> Vec<MailPerson> {
    let Ok(cache) = Cache::open() else {
        return Vec::new();
    };
    cache
        .address_history_in(own, scope)
        .into_iter()
        .filter(|a| !own.contains(&a.email) && !saved.contains(&a.email))
        .filter(|a| !is_automated(&a.email))
        .map(|a| MailPerson {
            name: if a.name == a.email {
                String::new()
            } else {
                a.name
            },
            email: a.email,
            seen: a.seen,
        })
        .collect()
}

/// The books contacts can go to, the default first and the Hylki book
/// among them, and the saved contacts' addresses (blocking: both go to
/// Evolution Data Server).
pub(crate) fn books_and_saved(demo: bool) -> (Vec<Book>, HashSet<String>) {
    let (mut books, saved) = if demo {
        let saved = super::demo_contacts()
            .iter()
            .flat_map(|c| c.emails.iter().map(|e| e.value.to_lowercase()))
            .collect();
        (super::demo_books(), saved)
    } else {
        (super::writable_books(), saved_addresses())
    };
    super::import::with_local_book(&mut books);
    (books, saved)
}

/// The saved contacts' addresses, lower case, read now (blocking). The
/// books that fill themselves from mail are not counted, or everyone
/// Evolution ever collected would look like a contact already.
pub(crate) fn saved_addresses() -> HashSet<String> {
    super::read::saved_contacts_now()
        .into_iter()
        .map(|c| c.email.trim().to_lowercase())
        .collect()
}

/// Whether an address is one nobody replies to: no-reply senders, bounce
/// handlers and the mail system itself.
pub(crate) fn is_automated(email: &str) -> bool {
    let local = email
        .rsplit_once('@')
        .map_or(email, |(l, _)| l)
        .to_ascii_lowercase();
    let squashed: String = local
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    squashed.contains("noreply")
        || squashed.contains("donotreply")
        || squashed.starts_with("bounce")
        || matches!(
            local.as_str(),
            "mailer-daemon" | "postmaster" | "mailerdaemon"
        )
}

/// One vCard per person, named as the mail names them.
pub(crate) fn cards_for(people: &[MailPerson]) -> Vec<String> {
    people
        .iter()
        .map(|p| {
            new_vcard(&ContactEdit {
                name: if p.name.trim().is_empty() {
                    p.email.clone()
                } else {
                    p.name.trim().to_string()
                },
                emails: vec![Labeled {
                    label: String::new(),
                    value: p.email.clone(),
                }],
                ..ContactEdit::default()
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automated_addresses_are_recognised() {
        for a in [
            "noreply@example.org",
            "no-reply@example.org",
            "do_not_reply@example.org",
            "DoNotReply@example.org",
            "bounces+123@example.org",
            "MAILER-DAEMON@example.org",
            "postmaster@example.org",
        ] {
            assert!(is_automated(a), "{a}");
        }
        for a in [
            "ann@example.org",
            "reply@example.org",
            "bob.bounce@example.org",
        ] {
            assert!(!is_automated(a), "{a}");
        }
    }

    #[test]
    fn a_card_carries_the_name_and_address() {
        let cards = cards_for(&[
            MailPerson {
                name: "Ann Lee".into(),
                email: "ann@example.org".into(),
                seen: 3,
            },
            MailPerson {
                name: String::new(),
                email: "bob@example.org".into(),
                seen: 1,
            },
        ]);
        let ann = super::super::parse_vcard_fields(&cards[0]).unwrap();
        assert_eq!(ann.name, "Ann Lee");
        assert_eq!(ann.primary_email(), Some("ann@example.org"));
        let bob = super::super::parse_vcard_fields(&cards[1]).unwrap();
        assert_eq!(bob.name, "bob@example.org");
    }
}
