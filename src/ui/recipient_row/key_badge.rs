//! The OpenPGP key badge on a recipient pill: where the key the message
//! would be encrypted to comes from, or that there is none.

use super::RecipientRow;
use adw::prelude::*;
use adw::subclass::prelude::*;

/// What one pill's badge shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyBadge {
    pub(crate) icon: &'static str,
    /// The style class that colours it (`key-keyring`, `key-none`, …).
    pub(crate) class: &'static str,
    pub(crate) tooltip: String,
}

/// Every style class a badge can wear, to take the old one off.
const CLASSES: [&str; 5] = [
    "key-keyring",
    "key-found",
    "key-weak",
    "key-none",
    "key-missing",
];

impl RecipientRow {
    /// Badge every pill by its address: `badge` gives what to show, or
    /// `None` to show nothing (OpenPGP is not in use).
    pub(crate) fn set_key_badges(&self, badge: &dyn Fn(&str) -> Option<KeyBadge>) {
        for (pill, recipient) in self.imp().pills.borrow().iter() {
            let Some(pill) = pill.downcast_ref::<gtk::Box>() else {
                continue;
            };
            let addr = crate::worker::parse_recipients(recipient)
                .into_iter()
                .next()
                .map(|(_, a)| a.trim().to_ascii_lowercase())
                .unwrap_or_default();
            let want = (!addr.is_empty()).then(|| badge(&addr)).flatten();
            let current = badge_of(pill);
            match (want, current) {
                (Some(want), current) => {
                    let image = current.unwrap_or_else(|| {
                        let image = gtk::Image::new();
                        image.add_css_class("key-badge");
                        image.set_pixel_size(12);
                        image.set_margin_end(4);
                        image.set_accessible_role(gtk::AccessibleRole::Presentation);
                        pill.prepend(&image);
                        image
                    });
                    image.set_icon_name(Some(want.icon));
                    for class in CLASSES {
                        image.remove_css_class(class);
                    }
                    image.add_css_class(want.class);
                    image.set_tooltip_text(Some(&want.tooltip));
                    image.set_visible(true);
                    pill.update_property(&[gtk::accessible::Property::Description(&want.tooltip)]);
                }
                (None, Some(image)) => {
                    image.set_visible(false);
                    pill.reset_property(gtk::AccessibleProperty::Description);
                }
                (None, None) => {}
            }
        }
    }

    /// The addresses of the field's pills, lowercased, in order.
    pub(crate) fn addresses(&self) -> Vec<String> {
        self.imp()
            .pills
            .borrow()
            .iter()
            .filter_map(|(_, r)| crate::worker::parse_recipients(r).into_iter().next())
            .map(|(_, a)| a.trim().to_ascii_lowercase())
            .filter(|a| a.contains('@'))
            .collect()
    }
}

/// A pill's badge, if it has one: always its first child.
fn badge_of(pill: &gtk::Box) -> Option<gtk::Image> {
    pill.first_child()
        .filter(|c| c.has_css_class("key-badge"))
        .and_downcast::<gtk::Image>()
}
