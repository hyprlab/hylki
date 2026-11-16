//! Mail filters (#47): the rules, how a message is matched against their
//! conditions, and filters.toml.

use std::path::PathBuf;

use super::{config_file, is_false, read_toml, save_private_toml};

/// A mail filter rule (#47): file matching inbox arrivals into a folder,
/// Evolution-style, applied client-side whenever Hylki syncs the inbox.
///
/// A rule holds one or more conditions (#192). The first lives at the top
/// level as `field`/`matcher`/`value`, exactly where versions that knew only
/// one condition read it, so a filters file written here still loads there
/// (with the rest of the conditions ignored); `more` holds the others.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "FilterRuleDisk", into = "FilterRuleDisk")]
pub(crate) struct FilterRule {
    /// What to call the rule (#197). Optional: an unnamed rule is still
    /// listed by its conditions, the way every rule was before names
    /// existed. Kept first so it reads first in `filters.toml`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) name: String,
    /// The account this rule (and its destination folder) belongs to.
    pub(crate) account_email: String,
    /// The first condition: what it inspects…
    pub(crate) field: FilterField,
    /// …how…
    pub(crate) matcher: FilterMatch,
    /// …and against what. Commas separate alternatives, any one of which
    /// matches (#192): "invoice, receipt".
    pub(crate) value: String,
    /// The conditions after the first (#192).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) more: Vec<FilterCondition>,
    /// Whether one matching condition is enough (true) or every condition
    /// must match (false, the default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) any: bool,
    /// Destination folder path on the account. Empty leaves the mail where it
    /// is (a rule that only tags, #71).
    #[serde(default)]
    pub(crate) dest_path: String,
    /// The keyword of a [`Tag`] to put on matching mail (#71); empty tags
    /// nothing. Tagging and filing combine: the tag goes on first, and the
    /// move carries it along.
    #[serde(default)]
    pub(crate) tag: String,
    /// Whether the destination folder's unread mail counts toward the unread
    /// total (the All Inboxes chip, the tray icon and its menu, the
    /// Background Apps status), as inbox mail does (#116). Trash and Junk
    /// destinations never count, whatever this says.
    #[serde(default = "count_unread_default")]
    pub(crate) count_unread: bool,
    /// Addresses the matching mail is forwarded to, in the manual Forward's
    /// format. Empty forwards nothing. Written beside the older keys so a
    /// version that predates them ignores them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) forward_to: Vec<String>,
    /// Unix seconds from which mail is forwarded: set when the Forward was
    /// added to the rule, so old mail in the folder is never sent on. 0
    /// means unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub(crate) forward_since: i64,
    /// Mark matching mail as read.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) mark_read: bool,
    /// Permanently delete matching mail (never to Trash). Excludes a
    /// destination folder.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) delete: bool,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// The `forward_since` for a rule being saved: the start it had when the
/// destinations are the same, otherwise `now`. Mail already in the folder
/// must not go to an address added later, and an address that already got
/// it must not get it twice. `prior` is the rule's old start and addresses.
pub(crate) fn forward_start(prior: Option<(i64, &[String])>, to: &[String], now: i64) -> i64 {
    if to.is_empty() {
        return 0;
    }
    let sorted = |list: &[String]| {
        let mut v: Vec<String> = list.iter().map(|a| a.to_lowercase()).collect();
        v.sort();
        v
    };
    match prior {
        Some((since, old)) if since > 0 && sorted(old) == sorted(to) => since,
        _ => now,
    }
}

/// The addresses a Forward action names, from the comma-separated text of
/// the editor: trimmed, lowercased and de-duplicated in order, empties
/// dropped. `Err` carries the first entry that is not a plain address.
pub(crate) fn parse_forward_addresses(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let valid = part.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        }) && !part.chars().any(char::is_whitespace);
        if !valid {
            return Err(part.to_string());
        }
        let addr = part.to_lowercase();
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    Ok(out)
}

fn count_unread_default() -> bool {
    true
}

/// The subject with any leading "Re:", "Fwd:" or "Fw:" (any case, repeated)
/// taken off, and the whitespace around it.
fn strip_reply_prefixes(subject: &str) -> &str {
    const PREFIXES: [&str; 3] = ["re:", "fwd:", "fw:"];
    let mut rest = subject.trim();
    while let Some(next) = PREFIXES.iter().find_map(|p| {
        let head = rest.get(..p.len())?;
        head.eq_ignore_ascii_case(p)
            .then(|| rest[p.len()..].trim_start())
    }) {
        rest = next;
    }
    rest.trim()
}

/// One condition of a [`FilterRule`]: what to look at, how, and for what.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "FilterConditionDisk", into = "FilterConditionDisk")]
pub(crate) struct FilterCondition {
    pub(crate) field: FilterField,
    pub(crate) matcher: FilterMatch,
    /// Commas separate alternatives; any one of them matching is a match.
    pub(crate) value: String,
}

/// How `filters.toml` writes a sender-in-Contacts condition (PR #384). An
/// older Hylki knows no such field and would drop the whole file over one,
/// so the condition goes to disk as an empty From address match, which
/// matches nothing there, with `contacts` beside it saying what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContactCheck {
    InContacts,
    NotInContacts,
}

/// The field as written to disk, and the `contacts` key that goes with it.
fn field_to_disk(field: FilterField) -> (FilterField, Option<ContactCheck>) {
    match field {
        FilterField::FromInContacts => (FilterField::FromAddress, Some(ContactCheck::InContacts)),
        FilterField::FromNotInContacts => {
            (FilterField::FromAddress, Some(ContactCheck::NotInContacts))
        }
        other => (other, None),
    }
}

/// The field a condition read from disk stands for.
fn field_from_disk(field: FilterField, contacts: Option<ContactCheck>) -> FilterField {
    match contacts {
        Some(ContactCheck::InContacts) => FilterField::FromInContacts,
        Some(ContactCheck::NotInContacts) => FilterField::FromNotInContacts,
        None => field,
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FilterConditionDisk {
    field: FilterField,
    matcher: FilterMatch,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contacts: Option<ContactCheck>,
}

impl From<FilterConditionDisk> for FilterCondition {
    fn from(d: FilterConditionDisk) -> Self {
        FilterCondition {
            field: field_from_disk(d.field, d.contacts),
            matcher: d.matcher,
            value: d.value,
        }
    }
}

impl From<FilterCondition> for FilterConditionDisk {
    fn from(c: FilterCondition) -> Self {
        let (field, contacts) = field_to_disk(c.field);
        let value = if contacts.is_some() {
            String::new()
        } else {
            c.value
        };
        FilterConditionDisk {
            field,
            matcher: c.matcher,
            value,
            contacts,
        }
    }
}

/// [`FilterRule`] as `filters.toml` holds it: the same keys, plus `contacts`
/// for a first condition that checks the address books.
#[derive(serde::Serialize, serde::Deserialize)]
struct FilterRuleDisk {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    account_email: String,
    field: FilterField,
    matcher: FilterMatch,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contacts: Option<ContactCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    more: Vec<FilterCondition>,
    #[serde(default, skip_serializing_if = "is_false")]
    any: bool,
    #[serde(default)]
    dest_path: String,
    #[serde(default)]
    tag: String,
    #[serde(default = "count_unread_default")]
    count_unread: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    forward_to: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    forward_since: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    mark_read: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    delete: bool,
}

impl From<FilterRuleDisk> for FilterRule {
    fn from(d: FilterRuleDisk) -> Self {
        // A destination and a delete exclude each other; the safer of the
        // two wins.
        let delete = d.delete && d.dest_path.is_empty();
        if d.delete && !delete {
            tracing::warn!(
                "filter: a rule with a destination and Delete keeps only the destination"
            );
        }
        FilterRule {
            name: d.name,
            account_email: d.account_email,
            field: field_from_disk(d.field, d.contacts),
            matcher: d.matcher,
            value: d.value,
            more: d.more,
            any: d.any,
            dest_path: d.dest_path,
            tag: d.tag,
            count_unread: d.count_unread,
            forward_to: d.forward_to,
            forward_since: d.forward_since,
            mark_read: d.mark_read,
            delete,
        }
    }
}

impl From<FilterRule> for FilterRuleDisk {
    fn from(r: FilterRule) -> Self {
        let (field, contacts) = field_to_disk(r.field);
        let value = if contacts.is_some() {
            String::new()
        } else {
            r.value
        };
        FilterRuleDisk {
            name: r.name,
            account_email: r.account_email,
            field,
            matcher: r.matcher,
            value,
            contacts,
            more: r.more,
            any: r.any,
            dest_path: r.dest_path,
            tag: r.tag,
            count_unread: r.count_unread,
            forward_to: r.forward_to,
            forward_since: r.forward_since,
            mark_read: r.mark_read,
            delete: r.delete,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilterField {
    FromAddress,
    FromName,
    Subject,
    Recipients,
    /// The Reply-To header, or the From address when there is none (#191).
    ReplyTo,
    /// The message text (#191). Searched on the server as the inbox syncs
    /// (IMAP `SEARCH BODY`, Microsoft Graph `$search`), so nothing is
    /// downloaded for it; the list preview and a cached body are checked
    /// too. Always a "contains" match, whatever the matcher says: that is
    /// the only search a server offers.
    Body,
    /// The sender's address is in the address books: EDS and the local Hylki
    /// book. Takes no text and ignores the matcher. Never matches while the
    /// address books are unreadable or empty, so a book that failed to load
    /// cannot make every sender look unknown.
    FromInContacts,
    /// The opposite of [`FilterField::FromInContacts`], with the same guard.
    /// Neither is ever written under its own name: see [`ContactCheck`].
    FromNotInContacts,
}

impl FilterField {
    /// Whether the field needs no text and no matcher.
    pub(crate) fn is_contact_check(self) -> bool {
        matches!(
            self,
            FilterField::FromInContacts | FilterField::FromNotInContacts
        )
    }

    /// Every field, in the order the editor lists them.
    pub(crate) const ALL: [FilterField; 8] = [
        FilterField::FromAddress,
        FilterField::FromName,
        FilterField::Subject,
        FilterField::Recipients,
        FilterField::ReplyTo,
        FilterField::Body,
        FilterField::FromInContacts,
        FilterField::FromNotInContacts,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FilterMatch {
    #[default]
    Contains,
    Equals,
    StartsWith,
    EndsWith,
}

impl FilterMatch {
    /// Every matcher, in the order the editor lists them.
    pub(crate) const ALL: [FilterMatch; 4] = [
        FilterMatch::Contains,
        FilterMatch::Equals,
        FilterMatch::StartsWith,
        FilterMatch::EndsWith,
    ];

    /// Whether `hay` matches `needle` this way; both already lower case.
    pub(crate) fn test(self, hay: &str, needle: &str) -> bool {
        match self {
            FilterMatch::Contains => hay.contains(needle),
            FilterMatch::Equals => hay == needle,
            FilterMatch::StartsWith => hay.starts_with(needle),
            FilterMatch::EndsWith => hay.ends_with(needle),
        }
    }
}

/// What a message offers the conditions to look at.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FilterInput<'a> {
    pub(crate) from_addr: &'a str,
    pub(crate) from_name: &'a str,
    pub(crate) subject: &'a str,
    /// To and Cc together.
    pub(crate) recipients: &'a str,
    /// The Reply-To header; empty when absent.
    pub(crate) reply_to: &'a str,
    /// The message text on hand: the list preview, or the body when the
    /// list carries it.
    pub(crate) body: &'a str,
    /// The body alternatives the server confirmed for this message (#191),
    /// lowercased as [`FilterCondition::alternatives`] hands them out.
    pub(crate) body_hits: &'a [String],
    /// Lowercased addresses from the address books, or `None` when they are
    /// unknown (unread, empty, or no rule asked for them).
    pub(crate) contacts: Option<&'a std::collections::HashSet<String>>,
}

impl FilterCondition {
    /// The alternatives a value names (#192): its comma-separated pieces,
    /// trimmed and lowercased, empties dropped. A value without a comma is
    /// one alternative.
    pub(crate) fn alternatives(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Case-insensitive match against one message.
    pub(crate) fn matches(&self, input: &FilterInput) -> bool {
        if self.field.is_contact_check() {
            let Some(contacts) = input.contacts else {
                return false;
            };
            let known = contacts.contains(&input.from_addr.trim().to_lowercase());
            return known == (self.field == FilterField::FromInContacts);
        }
        let alts = Self::alternatives(&self.value);
        if alts.is_empty() {
            return false;
        }
        if self.field == FilterField::Body {
            // The server's word (a hit) or the text on hand; a rule set up
            // on a backend without a search still works off the preview.
            let text = input.body.to_lowercase();
            return alts
                .iter()
                .any(|a| input.body_hits.iter().any(|h| h == a) || text.contains(a.as_str()));
        }
        let hay = match self.field {
            FilterField::FromAddress => input.from_addr,
            FilterField::FromName => input.from_name,
            FilterField::Subject => input.subject,
            FilterField::Recipients => input.recipients,
            FilterField::ReplyTo if input.reply_to.trim().is_empty() => input.from_addr,
            FilterField::ReplyTo => input.reply_to,
            FilterField::Body | FilterField::FromInContacts | FilterField::FromNotInContacts => {
                unreachable!()
            }
        }
        .to_lowercase();
        // The recipients are a list. "Contains" reads it whole, names
        // included; the other matchers hold each recipient up on its own,
        // so "is x@y" matches mail sent to x@y and someone else, and "ends
        // with @y" finds any one recipient there, not only the last (#201).
        let hays = if self.field == FilterField::Recipients && self.matcher != FilterMatch::Contains
        {
            mailboxes(&hay)
        } else {
            vec![hay]
        };
        alts.iter()
            .any(|needle| hays.iter().any(|hay| self.matcher.test(hay, needle)))
    }
}

/// The pieces of a comma-separated recipient list a matcher can be held
/// against: every address on its own, and every display name on its own.
/// `Ann <ann@shop.example>, bob@shop.example` gives `ann`, `ann@shop.example`
/// and `bob@shop.example`.
fn mailboxes(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match (part.rfind('<'), part.rfind('>')) {
            (Some(lt), Some(gt)) if lt < gt => {
                let name = part[..lt].trim().trim_matches('"').trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
                out.push(part[lt + 1..gt].trim().to_string());
            }
            _ => out.push(part.to_string()),
        }
    }
    out
}

impl FilterRule {
    /// The rule the "Create Filter from Message…" entry starts the
    /// editor with: the sender's address, and the subject without its
    /// leading "Re:" / "Fwd:" so a whole thread matches. Both have to match;
    /// what the rule does is left for the user to pick. A blank subject
    /// leaves the sender as the only condition.
    pub(crate) fn draft_from_message(
        account_email: &str,
        from_addr: &str,
        subject: &str,
    ) -> FilterRule {
        let mut conditions = vec![FilterCondition {
            field: FilterField::FromAddress,
            matcher: FilterMatch::Equals,
            value: from_addr.trim().to_string(),
        }];
        let subject = strip_reply_prefixes(subject);
        if !subject.is_empty() {
            conditions.push(FilterCondition {
                field: FilterField::Subject,
                matcher: FilterMatch::Contains,
                value: subject.to_string(),
            });
        }
        let mut rule = FilterRule {
            name: String::new(),
            account_email: account_email.to_string(),
            field: FilterField::FromAddress,
            matcher: FilterMatch::Equals,
            value: String::new(),
            more: Vec::new(),
            any: false,
            dest_path: String::new(),
            tag: String::new(),
            count_unread: count_unread_default(),
            forward_to: Vec::new(),
            forward_since: 0,
            mark_read: false,
            delete: false,
        };
        rule.set_conditions(conditions);
        rule
    }

    /// Every condition, the first included.
    pub(crate) fn conditions(&self) -> Vec<FilterCondition> {
        let mut all = Vec::with_capacity(1 + self.more.len());
        all.push(FilterCondition {
            field: self.field,
            matcher: self.matcher,
            value: self.value.clone(),
        });
        all.extend(self.more.iter().cloned());
        all
    }

    /// Replace the conditions: the first goes to the top level, the rest to
    /// `more`. Returns false (and changes nothing) when `conds` is empty.
    pub(crate) fn set_conditions(&mut self, mut conds: Vec<FilterCondition>) -> bool {
        if conds.is_empty() {
            return false;
        }
        let first = conds.remove(0);
        self.field = first.field;
        self.matcher = first.matcher;
        self.value = first.value;
        self.more = conds;
        true
    }

    /// What to call the rule in a list or a log line (#197): its name, or
    /// its conditions when it has none.
    pub(crate) fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        self.conditions()
            .iter()
            .map(|c| format!("{:?} {:?} {}", c.field, c.matcher, c.value))
            .collect::<Vec<_>>()
            .join(if self.any { " or " } else { " and " })
    }

    /// Whether the rule does anything to a match: files it, tags it,
    /// forwards it, marks it read or deletes it.
    pub(crate) fn has_action(&self) -> bool {
        !self.dest_path.is_empty()
            || !self.tag.is_empty()
            || !self.forward_to.is_empty()
            || self.mark_read
            || self.delete
    }

    /// Whether any condition looks the sender up in the address books.
    pub(crate) fn needs_contacts(&self) -> bool {
        self.conditions().iter().any(|c| c.field.is_contact_check())
    }

    /// The body alternatives this rule needs a server search for (#191).
    pub(super) fn body_needles(&self) -> Vec<String> {
        self.conditions()
            .iter()
            .filter(|c| c.field == FilterField::Body)
            .flat_map(|c| FilterCondition::alternatives(&c.value))
            .collect()
    }

    /// Case-insensitive match against a message: every condition must hold,
    /// or any one of them with `any` set.
    pub(crate) fn matches(&self, input: &FilterInput) -> bool {
        let conds = self.conditions();
        if self.any {
            conds.iter().any(|c| c.matches(input))
        } else {
            conds.iter().all(|c| c.matches(input))
        }
    }
}

/// Every body alternative the rules of `email` search for (#191), lowercased
/// and deduplicated, read fresh from disk: the worker asks at each inbox
/// sync, so a rule added in Settings counts from the next sync on.
pub(crate) fn filter_body_needles(email: &str) -> Vec<String> {
    let mut out: Vec<String> = load_filters()
        .iter()
        .filter(|r| r.account_email.eq_ignore_ascii_case(email))
        .flat_map(|r| r.body_needles())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct FiltersFile {
    #[serde(default)]
    pub(super) rules: Vec<FilterRule>,
}

fn filters_path() -> Option<PathBuf> {
    config_file("filters.toml")
}

pub(crate) fn load_filters() -> Vec<FilterRule> {
    read_toml::<FiltersFile>(filters_path())
        .map(|f| f.rules)
        .unwrap_or_default()
}

pub(crate) fn save_filters(rules: &[FilterRule]) {
    let file = FiltersFile {
        rules: rules.to_vec(),
    };
    save_private_toml(filters_path(), &file, "filters", false);
}
