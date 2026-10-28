//! GNOME Contacts integration via Evolution Data Server (EDS).
//!
//! Reading is done straight from the EDS address-book SQLite caches (covers
//! both the local address book and CardDAV books, which EDS mirrors locally).
//! Writing goes through the EDS D-Bus API so changes are tracked by EDS and
//! synced back to CardDAV servers.
//!
//! It is also the parent of the other places contacts come from: Hylki's own
//! address book, LDAP directories, CardDAV accounts, the Microsoft 365
//! directory, and the list of sources the settings show.

use std::collections::HashSet;
use std::sync::Arc;

use crate::i18n::ni18n_f;

mod demo;
mod details;
mod edit;
mod eds_write;
mod names;
mod photos;
mod read;
mod registry;
mod vcard;

pub(crate) mod carddav;
pub(crate) mod contact_sources;
pub(crate) mod directory;
pub(crate) mod graph_directory;
pub(crate) mod local;

pub(crate) use demo::*;
pub(crate) use details::*;
pub(crate) use edit::*;
pub(crate) use eds_write::*;
pub(crate) use names::*;
pub(crate) use photos::*;
pub(crate) use read::*;
pub(crate) use registry::*;
pub(crate) use vcard::*;

#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vcard_tests;

/// A name + email pair from the address book.
#[derive(Debug, Clone)]
pub(crate) struct Contact {
    pub(crate) name: String,
    pub(crate) email: String,
}

/// A recipient autocomplete suggestion (from Contacts or mail history).
/// `Default` is a person from mail history seen no times.
#[derive(Debug, Clone, Default)]
pub(crate) struct Suggestion {
    pub(crate) name: String,
    pub(crate) email: String,
    /// True when this came from the address book.
    pub(crate) from_contacts: bool,
    /// Found in an LDAP or organisation directory: marked as such even when
    /// mail history knows the address too (#307).
    pub(crate) from_directory: bool,
    /// How often this address appears in mail history (0 for contacts-only).
    pub(crate) score: u32,
    /// One of the user's own account addresses: offered, since people do
    /// mail themselves, but ranked after everyone else so it does not sit
    /// on top of every list (it is on nearly every message received).
    pub(crate) own: bool,
    /// Set for a contact group: picking it puts in its members, and `email`
    /// is empty.
    pub(crate) group: Option<Arc<SuggestedGroup>>,
}

/// "3 people", and "in Bcc" for a group whose people go there.
pub(crate) fn group_reach(people: usize, hide_addresses: bool) -> String {
    let n = people as u32;
    let count = n.to_string();
    if hide_addresses {
        ni18n_f(
            "{n} person, in Bcc",
            "{n} people, in Bcc",
            n,
            &[("n", &count)],
        )
    } else {
        ni18n_f("{n} person", "{n} people", n, &[("n", &count)])
    }
}

/// What a group suggestion stands for.
#[derive(Debug, Clone)]
pub(crate) struct SuggestedGroup {
    pub(crate) members: Vec<Contact>,
    /// The members go in Bcc whichever field the group was picked in.
    pub(crate) hide_addresses: bool,
}

impl Suggestion {
    /// "Name <email>" for inserting into a recipient field (email alone if no name).
    pub(crate) fn display(&self) -> String {
        crate::worker::format_recipient(&self.name, &self.email)
    }

    /// Whether the suggestion matches a typed fragment (name or email).
    pub(crate) fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        self.email.to_lowercase().contains(&q) || self.name.to_lowercase().contains(&q)
    }
}

/// Order the suggestions matching `typed` best first: one whose name or
/// address starts with it, then the most used, then by name. The user's own
/// addresses come after everyone else's, prefix match or not.
pub(crate) fn rank(matches: &mut [Suggestion], typed: &str) {
    let q = typed.to_lowercase();
    matches.sort_by(|a, b| {
        let pa = a.email.to_lowercase().starts_with(&q) || a.name.to_lowercase().starts_with(&q);
        let pb = b.email.to_lowercase().starts_with(&q) || b.name.to_lowercase().starts_with(&q);
        a.own
            .cmp(&b.own)
            .then(pb.cmp(&pa))
            .then(b.score.cmp(&a.score))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Combined, de-duplicated recipient suggestions from Contacts and mail history
/// (addresses sent to / received from). History frequency becomes each
/// suggestion's `score` so frequently used addresses — contact or not — can
/// rank ahead. The user's `own` (name, email) account addresses are in the
/// list too, flagged and with no score, so they come last.
pub(crate) fn suggestions(own: &[(String, String)]) -> Vec<Suggestion> {
    let own_keys: HashSet<String> = own.iter().map(|(_, e)| e.to_lowercase()).collect();
    let mut map: std::collections::HashMap<String, Suggestion> = std::collections::HashMap::new();

    // The registry is asked once for both reads: this runs as a composer opens.
    let active = registry_books();
    for c in read_contacts_in(&active, false).0 {
        let key = c.email.to_lowercase();
        if own_keys.contains(&key) {
            continue;
        }
        map.entry(key).or_insert(Suggestion {
            name: c.name,
            email: c.email,
            from_contacts: true,
            ..Default::default()
        });
    }

    if let Ok(cache) = crate::cache::Cache::open() {
        for (name, email, count) in cache.address_history() {
            let key = email.to_lowercase();
            if own_keys.contains(&key) {
                continue;
            }
            map.entry(key)
                .and_modify(|s| s.score = s.score.max(count))
                .or_insert(Suggestion {
                    name,
                    email,
                    score: count,
                    ..Default::default()
                });
        }
    }

    for (name, email) in own {
        let key = email.to_lowercase();
        if key.is_empty() || !key.contains('@') {
            continue;
        }
        map.entry(key).or_insert(Suggestion {
            name: if name.trim().is_empty() {
                email.clone()
            } else {
                name.clone()
            },
            email: email.clone(),
            own: true,
            ..Default::default()
        });
    }

    let mut out: Vec<Suggestion> = map.into_values().collect();
    out.extend(group_suggestions(read_groups_in(&active)));
    // Default order: most-used first, then name (the field filter re-ranks live).
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// The groups that have someone to send to, as suggestions; the same group
/// in two books is offered once.
fn group_suggestions(groups: Vec<ContactDetails>) -> Vec<Suggestion> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for g in groups {
        let members: Vec<Contact> = g
            .reachable()
            .map(|m| Contact {
                name: m.name.clone(),
                email: m.email.clone(),
            })
            .collect();
        let mut key: Vec<String> = members.iter().map(|m| m.email.to_lowercase()).collect();
        key.sort();
        key.push(g.name.to_lowercase());
        if members.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(Suggestion {
            name: g.name,
            email: String::new(),
            from_contacts: true,
            group: Some(Arc::new(SuggestedGroup {
                members,
                hide_addresses: g.hide_addresses,
            })),
            ..Default::default()
        });
    }
    out
}
