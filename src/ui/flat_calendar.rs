//! A calendar drawn like the popover or dialog around it rather than as a
//! boxed view (#696): the date filter's, the Snooze / Send Later picker's,
//! and the Create Event and Create Task dialogs' date popovers. Its look is
//! the `flat-calendar` class in `styles.css`.

use adw::prelude::*;

/// Give `cal` the flat look, its days centered in their columns.
pub(crate) fn style(cal: &gtk::Calendar) {
    cal.add_css_class("flat-calendar");
    center_days(cal);
}

/// Keeps each day label at its CSS size in the middle of its column. Left to
/// fill the column, a day is as wide as the popover or dialog makes it (34
/// or 35 px in the date filter, depending on the column), so its circle came
/// out an oval off its digits.
fn center_days(cal: &gtk::Calendar) {
    let grid = std::iter::successors(cal.first_child(), |w| w.next_sibling())
        .find(|w| w.css_name() == "grid");
    let days = std::iter::successors(grid.and_then(|g| g.first_child()), |w| w.next_sibling());
    for day in days.filter(|w| w.has_css_class("day-number")) {
        day.set_halign(gtk::Align::Center);
        day.set_valign(gtk::Align::Center);
    }
}
