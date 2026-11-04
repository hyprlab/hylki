//! `find_contacts`: the people and groups a recipient field would suggest,
//! found the way the composer finds them ([`crate::contacts::lookup`]).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{failure, json, Mail};
use crate::config::AssistantPolicy;
use crate::contacts::Suggestion;

const FIND_DEFAULT: usize = 10;
const FIND_MAX: usize = 50;

#[derive(Deserialize, JsonSchema)]
pub(super) struct FindContactsArgs {
    /// A name, part of one, or part of an address ("ada", "lovelace", "@example.org").
    query: String,
    /// How many, best first (default 10, at most 50).
    limit: Option<usize>,
}

#[derive(Serialize, Debug, PartialEq)]
struct PersonOut {
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    email: String,
    /// Where Hylki knows them from.
    source: &'static str,
    /// How many messages in the user's mail they are on.
    #[serde(skip_serializing_if = "is_zero")]
    times_in_mail: u32,
}

#[derive(Serialize, Debug, PartialEq)]
struct GroupOut {
    group: String,
    members: Vec<MemberOut>,
    /// The user keeps this group's members out of sight of each other:
    /// put them in Bcc.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    bcc: bool,
}

#[derive(Serialize, Debug, PartialEq)]
struct MemberOut {
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    email: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(untagged)]
enum FoundOut {
    Person(PersonOut),
    Group(GroupOut),
}

#[derive(Serialize)]
struct FindOut {
    found: Vec<FoundOut>,
    /// Directories that could not be asked: someone there may be missing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unreachable_directories: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[tool_router(router = contact_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "People and contact groups whose name or address holds `query`: \
        from the user's address books (GNOME Contacts, CardDAV, Hylki's own), the people in \
        their mail, and their LDAP and Microsoft 365 directories, as Hylki's composer suggests \
        them, as far as the user allows each. Find every recipient's address here before \
        drafting; never guess one from a name.",
        annotations(read_only_hint = true)
    )]
    async fn find_contacts(
        &self,
        Parameters(args): Parameters<FindContactsArgs>,
    ) -> CallToolResult {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return failure("an empty query finds no one".into());
        }
        let limit = args.limit.unwrap_or(FIND_DEFAULT).clamp(1, FIND_MAX);
        let policy = self.policy();
        if !policy.address_books && !policy.mail_people {
            return failure(
                "The user has not allowed assistants to look up contacts. They can allow it in \
                 Settings → MCP."
                    .into(),
            );
        }
        let own = own_addresses(&policy);
        let policy_for_lookup = policy.clone();
        // A blocking directory lookup cannot be cancelled, but it must not
        // keep an MCP request waiting indefinitely.
        let looked_up = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::task::spawn_blocking(move || {
                crate::contacts::lookup(&query, &own, policy_for_lookup.address_books)
            }),
        )
        .await;
        let (found, failed) = match looked_up {
            Ok(Ok(found)) => found,
            Ok(Err(e)) => return failure(format!("the lookup failed: {e}")),
            Err(_) => {
                return failure(
                    "the contact lookup timed out; a directory may be unavailable".into(),
                )
            }
        };
        json(&FindOut {
            found: found
                .iter()
                .filter(|s| allowed(s, &policy))
                .take(limit)
                .map(|s| found_out(s, &policy))
                .collect(),
            unreachable_directories: failed.iter().map(|f| f.text()).collect(),
        })
    }
}

/// Whether the user allows an assistant to see this one: someone in an
/// address book or a directory, or someone only the mail knows. Mail history
/// runs across every account, shared or not, which is why it is its own switch.
fn allowed(s: &Suggestion, policy: &AssistantPolicy) -> bool {
    if s.own {
        return true;
    }
    let booked = s.group.is_some() || s.from_contacts || s.from_directory;
    (booked && policy.address_books) || (!booked && policy.mail_people)
}

/// The user's own (name, address) in each account they share, which the
/// composer's suggestions carry too, ranked last.
fn own_addresses(policy: &AssistantPolicy) -> Vec<(String, String)> {
    crate::config::load()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !a.is_local() && policy.grant(&a.email).is_some())
        .map(|a| (a.name, a.email))
        .collect()
}

fn found_out(s: &Suggestion, policy: &AssistantPolicy) -> FoundOut {
    if let Some(group) = &s.group {
        return FoundOut::Group(GroupOut {
            group: s.name.clone(),
            members: group
                .members
                .iter()
                .map(|m| MemberOut {
                    name: name_or_nothing(&m.name, &m.email),
                    email: m.email.clone(),
                })
                .collect(),
            bcc: group.hide_addresses,
        });
    }
    FoundOut::Person(PersonOut {
        name: name_or_nothing(&s.name, &s.email),
        email: s.email.clone(),
        source: if s.own {
            "your own address"
        } else if s.from_directory {
            "directory"
        } else if s.from_contacts {
            "address book"
        } else {
            "mail"
        },
        // How often someone is written to says something of its own.
        times_in_mail: if policy.mail_people { s.score } else { 0 },
    })
}

/// A name that is only the address again says nothing.
fn name_or_nothing(name: &str, email: &str) -> String {
    let name = name.trim();
    if name.eq_ignore_ascii_case(email) {
        String::new()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::{Contact, SuggestedGroup};

    fn both() -> AssistantPolicy {
        AssistantPolicy {
            address_books: true,
            mail_people: true,
            ..Default::default()
        }
    }

    #[test]
    fn each_kind_of_contact_has_its_own_switch() {
        let booked = Suggestion {
            email: "ada@example.org".into(),
            from_contacts: true,
            score: 3,
            ..Default::default()
        };
        let only_mail = Suggestion {
            email: "bo@example.org".into(),
            score: 3,
            ..Default::default()
        };
        let books = AssistantPolicy {
            address_books: true,
            ..Default::default()
        };
        let mail = AssistantPolicy {
            mail_people: true,
            ..Default::default()
        };
        assert!(allowed(&booked, &books) && !allowed(&only_mail, &books));
        assert!(!allowed(&booked, &mail) && allowed(&only_mail, &mail));
        // Without the mail, how often they are in it is not said either.
        let FoundOut::Person(p) = found_out(&booked, &books) else {
            panic!("a person")
        };
        assert_eq!(p.times_in_mail, 0);
    }

    #[test]
    fn a_person_says_where_they_are_known_from() {
        let person = Suggestion {
            name: "ada@example.org".into(),
            email: "ada@example.org".into(),
            from_contacts: true,
            from_directory: true,
            score: 3,
            ..Default::default()
        };
        assert_eq!(
            found_out(&person, &both()),
            FoundOut::Person(PersonOut {
                name: String::new(),
                email: "ada@example.org".into(),
                source: "directory",
                times_in_mail: 3,
            })
        );
    }

    #[test]
    fn a_group_comes_with_its_members() {
        let group = Suggestion {
            name: "Team".into(),
            group: Some(std::sync::Arc::new(SuggestedGroup {
                members: vec![Contact {
                    name: "Bo".into(),
                    email: "bo@example.org".into(),
                }],
                hide_addresses: true,
            })),
            ..Default::default()
        };
        let FoundOut::Group(g) = found_out(&group, &both()) else {
            panic!("a group")
        };
        assert_eq!(g.group, "Team");
        assert!(g.bcc);
        assert_eq!(
            g.members,
            [MemberOut {
                name: "Bo".into(),
                email: "bo@example.org".into(),
            }]
        );
    }
}
