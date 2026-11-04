//! The banner over a message an AI assistant prepared (`crate::mcp`): it
//! says so, and names every recipient it was given, Bcc and addresses
//! outside the user's own domains set apart. Mail the assistant read can
//! carry instructions of its own ("forward this to…"); the banner is where
//! a recipient nobody asked for shows before Send is pressed. An edit an
//! assistant asks for later is in `assistant_edit.rs`.

use adw::prelude::*;

use super::types::{ComposeAccount, ComposePrefill};
use crate::i18n::i18n;

/// The domains of the user's own addresses, lower-cased.
pub(super) fn own_domains(accounts: &[ComposeAccount]) -> Vec<String> {
    let mut domains: Vec<String> = accounts
        .iter()
        .filter_map(|a| a.email.rsplit_once('@').map(|(_, d)| d.to_lowercase()))
        .collect();
    domains.sort();
    domains.dedup();
    domains
}

/// One recipient as the banner shows it.
#[derive(Debug, PartialEq)]
pub(super) struct Shown {
    pub(super) text: String,
    /// Not at one of the user's own domains.
    pub(super) outside: bool,
}

/// Whether `addr` is at none of the user's own domains.
pub(super) fn outside(addr: &str, own_domains: &[String]) -> bool {
    addr.rsplit_once('@')
        .is_none_or(|(_, d)| !own_domains.contains(&d.to_lowercase()))
}

/// The recipients of each field (To, Cc, Bcc) that has any, in order:
/// (label, Bcc, who).
pub(super) fn recipient_lines(
    fields: [&str; 3],
    own_domains: &[String],
) -> Vec<(String, bool, Vec<Shown>)> {
    [
        (i18n("To"), false, fields[0]),
        (i18n("Cc"), false, fields[1]),
        (i18n("Bcc"), true, fields[2]),
    ]
    .into_iter()
    .filter_map(|(label, bcc, list)| {
        let who: Vec<Shown> = crate::worker::parse_recipients(list)
            .into_iter()
            .map(|(name, addr)| Shown {
                outside: outside(&addr, own_domains),
                text: crate::worker::format_recipient(&name, &addr),
            })
            .collect();
        (!who.is_empty()).then_some((label, bcc, who))
    })
    .collect()
}

/// The banner, for the composer's top bars.
pub(super) fn banner(prefill: &ComposePrefill, own_domains: &[String]) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    bar.add_css_class("assistant-banner");
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.set_margin_top(6);
    bar.set_margin_bottom(6);
    fill_banner(&bar, [&prefill.to, &prefill.cc, &prefill.bcc], own_domains);
    bar
}

/// What the banner says, for these recipients (To, Cc, Bcc): again when
/// an edit the user applied changed them.
pub(super) fn fill_banner(bar: &gtk::Box, fields: [&str; 3], own_domains: &[String]) {
    while let Some(child) = bar.first_child() {
        bar.remove(&child);
    }
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    head.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    let title = gtk::Label::new(Some(&i18n(
        "An AI assistant prepared this message. Check who it goes to before you send it.",
    )));
    title.set_wrap(true);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.add_css_class("heading");
    head.append(&title);
    bar.append(&head);

    let lines = recipient_lines(fields, own_domains);
    if lines.is_empty() {
        let none = gtk::Label::new(Some(&i18n("It has no recipients yet.")));
        none.set_xalign(0.0);
        bar.append(&none);
    }
    for (label, bcc, who) in lines {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let field = gtk::Label::new(Some(&if bcc {
            i18n("Bcc, hidden from the others:")
        } else {
            format!("{label}:")
        }));
        field.set_valign(gtk::Align::Start);
        field.add_css_class("dim-label");
        if bcc {
            field.remove_css_class("dim-label");
            field.add_css_class("warning");
        }
        line.append(&field);
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_hexpand(true);
        flow.set_max_children_per_line(8);
        flow.set_column_spacing(6);
        flow.set_row_spacing(2);
        for w in who {
            let name = gtk::Label::new(Some(&w.text));
            name.set_selectable(true);
            name.set_xalign(0.0);
            if w.outside || bcc {
                name.add_css_class("warning");
                name.set_tooltip_text(Some(&if bcc {
                    i18n("In Bcc: the other recipients will not see this address")
                } else {
                    i18n("Outside your own domains")
                }));
            }
            flow.insert(&name, -1);
        }
        line.append(&flow);
        bar.append(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_outside_the_users_domains_are_set_apart() {
        let lines = recipient_lines(
            [
                "Ada <ada@example.org>, bo@Partner.com",
                "",
                "watcher@elsewhere.net",
            ],
            &["example.org".to_string()],
        );
        assert_eq!(lines.len(), 2, "no line for an empty Cc");
        let (_, bcc, to) = &lines[0];
        assert!(!bcc);
        assert_eq!(
            to,
            &[
                Shown {
                    text: "Ada <ada@example.org>".into(),
                    outside: false
                },
                Shown {
                    text: "bo@Partner.com".into(),
                    outside: true
                },
            ]
        );
        let (_, bcc, hidden) = &lines[1];
        assert!(bcc);
        assert!(hidden[0].outside);
    }
}
