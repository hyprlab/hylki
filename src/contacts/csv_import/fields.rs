//! The fields a CSV column can be matched to in the import dialog (#655),
//! what each column is matched to at first, and the columns a matching
//! makes.

use super::{columns, Col, Kind, Part, Single};
use crate::i18n::{i18n, i18n_f};
use crate::mail::merge::Table;

/// What a column is imported as: the dialog's choice for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Ignore,
    Single(Single),
    Email(&'static str),
    Phone(&'static str),
    Website,
    /// The label (Home, Work…) of the next value of this kind in the
    /// columns after it, as Google writes "E-mail 1 - Label" before
    /// "E-mail 1 - Value".
    LabelOf(Kind),
    Address(&'static str, Part),
}

const SINGLES: [Single; 16] = [
    Single::Full,
    Single::Prefix,
    Single::Given,
    Single::Middle,
    Single::Family,
    Single::Suffix,
    Single::Nickname,
    Single::Org,
    Single::Dept,
    Single::Title,
    Single::Birthday,
    Single::BirthYear,
    Single::BirthMonth,
    Single::BirthDay,
    Single::Note,
    Single::Groups,
];

const PARTS: [Part; 8] = [
    Part::Formatted,
    Part::Street,
    Part::Extended,
    Part::PoBox,
    Part::Locality,
    Part::Region,
    Part::Code,
    Part::Country,
];

/// Every choice, in the order the dialog lists them.
pub(crate) fn targets() -> Vec<Target> {
    let mut out = vec![Target::Ignore];
    out.extend(SINGLES.map(Target::Single));
    out.extend(["", "Home", "Work"].map(Target::Email));
    out.extend(["", "Mobile", "Home", "Work", "Fax"].map(Target::Phone));
    out.push(Target::Website);
    for label in ["", "Home", "Work"] {
        out.extend(PARTS.map(|p| Target::Address(label, p)));
    }
    out.extend([Kind::Email, Kind::Phone, Kind::Address, Kind::Url].map(Target::LabelOf));
    out
}

/// A field's name with its label: "Phone (Mobile)".
fn labeled(field: String, label: &str) -> String {
    let label = match label {
        "" => return field,
        "Home" => i18n("Home"),
        "Work" => i18n("Work"),
        "Mobile" => i18n("Mobile"),
        "Fax" => i18n("Fax"),
        other => other.to_string(),
    };
    i18n_f("{field} ({label})", &[("field", &field), ("label", &label)])
}

impl Target {
    /// The choice as the dialog names it.
    pub(crate) fn name(self) -> String {
        match self {
            Target::Ignore => i18n("Do not import"),
            Target::Single(s) => match s {
                Single::Full => i18n("Full name"),
                Single::Given => i18n("First name"),
                Single::Middle => i18n("Middle name"),
                Single::Family => i18n("Last name"),
                Single::Prefix => i18n("Name prefix"),
                Single::Suffix => i18n("Name suffix"),
                Single::Nickname => i18n("Nickname"),
                Single::Org => i18n("Company"),
                Single::Dept => i18n("Department"),
                Single::Title => i18n("Job title"),
                Single::Note => i18n("Notes"),
                Single::Birthday => i18n("Birthday"),
                Single::BirthYear => i18n("Birth year"),
                Single::BirthMonth => i18n("Birth month"),
                Single::BirthDay => i18n("Birth day"),
                Single::Groups => i18n("Groups"),
            },
            Target::Email(l) => labeled(i18n("Email"), l),
            Target::Phone(l) => labeled(i18n("Phone"), l),
            Target::Website => i18n("Website"),
            Target::LabelOf(kind) => match kind {
                Kind::Email => i18n("Email label"),
                Kind::Phone => i18n("Phone label"),
                Kind::Address => i18n("Address label"),
                Kind::Url => i18n("Website label"),
            },
            Target::Address(l, p) => labeled(
                match p {
                    Part::Formatted => i18n("Address"),
                    Part::Street => i18n("Street"),
                    Part::Extended => i18n("Apartment or suite"),
                    Part::PoBox => i18n("PO box"),
                    Part::Locality => i18n("City"),
                    Part::Region => i18n("State or region"),
                    Part::Code => i18n("Postal code"),
                    Part::Country => i18n("Country"),
                },
                l,
            ),
        }
    }
}

/// What each column of `table` is matched to before anyone changes it,
/// from its header (or, for addresses, its values).
pub(crate) fn detect(table: &Table) -> Vec<Target> {
    columns(table)
        .into_iter()
        .map(|col| match col {
            None => Target::Ignore,
            Some(Col::Single(s)) => Target::Single(s),
            Some(Col::Value(Kind::Email, _, l)) => Target::Email(l),
            Some(Col::Value(Kind::Phone, _, l)) => Target::Phone(l),
            Some(Col::Value(..)) => Target::Website,
            Some(Col::Label(kind, _)) => Target::LabelOf(kind),
            Some(Col::Address(_, l, p)) => Target::Address(l, p),
        })
        .collect()
}

/// The columns a matching makes. Each email, phone or website column is a
/// value of its own; the parts of an address with one label make one
/// address until a part comes again (Street 2 right after Street goes on
/// the same street), and a label column of addresses starts a new one.
pub(super) fn resolve(targets: &[Target]) -> Vec<Option<Col>> {
    let mut cols: Vec<Option<Col>> = vec![None; targets.len()];
    // The address being filled in for each label: its key and its parts.
    let mut open: Vec<(&'static str, String, Vec<Part>)> = Vec::new();
    let mut prev = Target::Ignore;
    for (i, &t) in targets.iter().enumerate() {
        let own = format!("c{i}");
        cols[i] = match t {
            Target::Ignore | Target::LabelOf(_) => None,
            Target::Single(s) => Some(Col::Single(s)),
            Target::Email(l) => Some(Col::Value(Kind::Email, own, l)),
            Target::Phone(l) => Some(Col::Value(Kind::Phone, own, l)),
            Target::Website => Some(Col::Value(Kind::Url, own, "")),
            Target::Address(l, p) => {
                let more_street = p == Part::Street && prev == Target::Address(l, Part::Street);
                let at = open.iter().position(|(ol, ..)| *ol == l);
                let at = match at {
                    Some(j) if !open[j].2.contains(&p) || more_street => j,
                    _ => {
                        open.retain(|(ol, ..)| *ol != l);
                        open.push((l, own, Vec::new()));
                        open.len() - 1
                    }
                };
                open[at].2.push(p);
                Some(Col::Address(open[at].1.clone(), l, p))
            }
        };
        if t == Target::LabelOf(Kind::Address) {
            open.retain(|(ol, ..)| !ol.is_empty());
        }
        prev = t;
    }
    for (i, &t) in targets.iter().enumerate() {
        let Target::LabelOf(kind) = t else { continue };
        cols[i] = cols[i + 1..]
            .iter()
            .flatten()
            .find_map(|c| match c {
                Col::Value(k, key, _) if *k == kind => Some(key.clone()),
                Col::Address(key, ..) if kind == Kind::Address => Some(key.clone()),
                _ => None,
            })
            .map(|key| Col::Label(kind, key));
    }
    cols
}

/// Whether a matching says who each row is: a name, a company, an email
/// address or a phone number.
pub(crate) fn can_import(targets: &[Target]) -> bool {
    targets.iter().any(|t| {
        matches!(
            t,
            Target::Single(
                Single::Full | Single::Given | Single::Family | Single::Nickname | Single::Org
            ) | Target::Email(_)
                | Target::Phone(_)
        )
    })
}
