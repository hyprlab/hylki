//! The popover an event opens when it is clicked: what, when, where, who,
//! the notes, a Join button for an online meeting, the user's answer to an
//! invitation, and Edit and Delete.

use adw::prelude::*;

use super::ctx::{long_day, Anchor, Shared};
use crate::calendar::model::{Comp, Occ};
use crate::calendar::store::CalInfo;
use crate::i18n::{i18n, i18n_f};

/// What the popover's buttons ask for.
#[derive(Debug, Clone)]
pub(crate) enum Ask {
    Edit,
    Delete,
    Duplicate,
    /// The user's answer to an invitation: ACCEPTED, TENTATIVE, DECLINED.
    Answer(String),
    /// Write to everyone on the attendee list.
    Email,
}

/// Show the popover for `occ` at `anchor`.
pub(crate) fn show(
    ctx: &Shared,
    occ: &Occ,
    master: Option<&Comp>,
    cal: Option<&CalInfo>,
    anchor: &Anchor,
    on: impl Fn(Ask) + 'static,
) -> gtk::Popover {
    let c = &occ.comp;
    let (color, writable, me) = {
        let x = ctx.borrow();
        (x.color(occ), x.can_change(occ), x.me.clone())
    };
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 10);
    bx.add_css_class("calendar-inspector");
    bx.set_width_request(320);

    let title = gtk::Label::new(Some(if c.summary.is_empty() {
        i18n("Untitled")
    } else {
        c.summary.clone()
    }
    .as_str()));
    title.add_css_class("title-3");
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.set_xalign(0.0);
    title.set_max_width_chars(36);
    title.set_selectable(true);
    bx.append(&title);

    if let Some(cal) = cal {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let swatch = gtk::DrawingArea::new();
        swatch.set_content_width(10);
        swatch.set_content_height(10);
        swatch.set_valign(gtk::Align::Center);
        swatch.set_draw_func(move |_, cr, w, h| {
            super::paint::dot(cr, f64::from(w) / 2.0, f64::from(h) / 2.0, 5.0, color);
        });
        row.append(&swatch);
        let name = gtk::Label::new(Some(&cal.full_name()));
        name.add_css_class("dim-label");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&name);
        bx.append(&row);
    }

    bx.append(&line("x-office-calendar-symbolic", &when_text(occ)));
    if let Some(rule) = master.and_then(|m| m.rrule.as_ref()) {
        let start = master
            .and_then(|m| m.anchor())
            .map(|s| s.date())
            .unwrap_or(occ.first_day);
        bx.append(&line("media-playlist-repeat-symbolic", &super::repeat::describe(rule, start)));
    }
    if !c.location.is_empty() {
        bx.append(&line("find-location-symbolic", &c.location));
    }
    if !c.alarms.is_empty() {
        let mut leads: Vec<String> = c.alarms.iter().map(|s| super::editor::alarm_label(*s / 60)).collect();
        leads.dedup();
        bx.append(&line("alarm-symbolic", &leads.join(", ")));
    }

    // An online meeting's link, wherever the invitation put it.
    if let Some(link) = meeting_link(c) {
        let join = gtk::Button::with_label(&i18n("Join Meeting"));
        join.add_css_class("suggested-action");
        join.add_css_class("pill");
        join.set_halign(gtk::Align::Start);
        join.set_tooltip_text(Some(&link));
        join.connect_clicked(move |b| {
            let launcher = gtk::UriLauncher::new(&link);
            let window = b.root().and_downcast::<gtk::Window>();
            launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
        });
        bx.append(&join);
    } else if !c.url.is_empty() {
        let link = gtk::LinkButton::with_label(&c.url, &c.url);
        link.set_halign(gtk::Align::Start);
        if let Some(l) = link.child().and_downcast::<gtk::Label>() {
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            l.set_max_width_chars(36);
        }
        bx.append(&link);
    }

    if !c.attendees.is_empty() {
        bx.append(&people(c, &me));
    }

    if !c.description.trim().is_empty() {
        let notes = gtk::Label::new(Some(c.description.trim()));
        notes.set_wrap(true);
        notes.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        notes.set_xalign(0.0);
        notes.set_selectable(true);
        notes.set_max_width_chars(40);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_propagate_natural_height(true);
        scroller.set_max_content_height(180);
        scroller.set_child(Some(&notes));
        scroller.add_css_class("calendar-notes");
        bx.append(&scroller);
    }

    let pop = gtk::Popover::new();
    let on = std::rc::Rc::new(on);

    // Answering, for a meeting someone else organizes.
    if c.invited(&me) && writable {
        let mine = c.my_status(&me).unwrap_or("NEEDS-ACTION").to_string();
        let answers = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        answers.add_css_class("linked");
        answers.set_homogeneous(true);
        for (status, label) in [
            ("ACCEPTED", i18n("Accept")),
            ("TENTATIVE", i18n("Maybe")),
            ("DECLINED", i18n("Decline")),
        ] {
            let b = gtk::ToggleButton::with_label(&label);
            b.set_active(mine == status);
            let on = on.clone();
            let p = pop.clone();
            b.connect_clicked(move |_| {
                p.popdown();
                on(Ask::Answer(status.to_string()));
            });
            answers.append(&b);
        }
        bx.append(&answers);
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_margin_top(4);
    let more = gtk::MenuButton::new();
    more.set_icon_name("view-more-symbolic");
    more.add_css_class("flat");
    more.set_tooltip_text(Some(&i18n("More")));
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let more_pop = gtk::Popover::new();
    more_pop.add_css_class("menu");
    let item = |label: &str, ask: Ask| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let on = on.clone();
        let p = pop.clone();
        let mp = more_pop.clone();
        b.connect_clicked(move |_| {
            mp.popdown();
            p.popdown();
            on(ask.clone());
        });
        b
    };
    menu_box.append(&item(&i18n("Duplicate"), Ask::Duplicate));
    if !c.attendees.is_empty() {
        menu_box.append(&item(&i18n("Email Attendees"), Ask::Email));
    }
    more_pop.set_child(Some(&menu_box));
    more.set_popover(Some(&more_pop));
    actions.append(&more);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    if writable {
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some(&i18n("Delete")));
        let on2 = on.clone();
        let p = pop.clone();
        delete.connect_clicked(move |_| {
            p.popdown();
            on2(Ask::Delete);
        });
        actions.append(&delete);
    }
    let edit = gtk::Button::with_label(&if writable { i18n("Edit") } else { i18n("Details") });
    edit.add_css_class("suggested-action");
    let on3 = on.clone();
    let p = pop.clone();
    edit.connect_clicked(move |_| {
        p.popdown();
        on3(Ask::Edit);
    });
    actions.append(&edit);
    bx.append(&actions);

    pop.set_child(Some(&bx));
    pop.set_parent(&anchor.widget);
    pop.set_pointing_to(Some(&anchor.rect));
    pop.set_position(gtk::PositionType::Right);
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || {
            if p.parent().is_some() {
                p.unparent();
            }
        });
    });
    pop.popup();
    pop
}

fn line(icon: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class("dim-label");
    img.set_valign(gtk::Align::Start);
    row.append(&img);
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_selectable(true);
    label.set_max_width_chars(36);
    row.append(&label);
    row
}

/// "Friday, 27 November · 14:00 – 15:30", or the days of an all-day event.
pub(crate) fn when_text(occ: &Occ) -> String {
    let t = crate::datefmt::time;
    if occ.all_day {
        if occ.first_day == occ.last_day {
            return long_day(occ.first_day);
        }
        return format!("{} – {}", long_day(occ.first_day), long_day(occ.last_day));
    }
    if occ.first_day == occ.last_day {
        if occ.end > occ.start {
            return format!("{} · {} – {}", long_day(occ.first_day), t(occ.start), t(occ.end));
        }
        return format!("{} · {}", long_day(occ.first_day), t(occ.start));
    }
    format!(
        "{} {} – {} {}",
        long_day(occ.first_day),
        t(occ.start),
        long_day(occ.last_day),
        t(occ.end)
    )
}

/// The organizer and attendees, each with where they stand.
fn people(c: &Comp, me: &[String]) -> gtk::Box {
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let accepted = c.attendees.iter().filter(|a| a.status == "ACCEPTED").count();
    let heading = gtk::Label::new(Some(&i18n_f(
        "{n} invited, {yes} going",
        &[
            ("n", &c.attendees.len().to_string()),
            ("yes", &accepted.to_string()),
        ],
    )));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    bx.append(&heading);
    let organizer = c.organizer.as_ref().map(|o| o.email.to_lowercase());
    let mut list: Vec<_> = c.attendees.iter().collect();
    list.sort_by_key(|a| Some(a.email.to_lowercase()) != organizer);
    for (i, a) in list.iter().enumerate() {
        if i == 8 {
            let rest = gtk::Label::new(Some(&i18n_f(
                "and {n} more",
                &[("n", &(list.len() - 8).to_string())],
            )));
            rest.add_css_class("dim-label");
            rest.set_xalign(0.0);
            bx.append(&rest);
            break;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let (icon, tip) = match a.status.as_str() {
            "ACCEPTED" => ("object-select-symbolic", i18n("Going")),
            "DECLINED" => ("window-close-symbolic", i18n("Not going")),
            "TENTATIVE" => ("dialog-question-symbolic", i18n("Maybe")),
            _ => ("mail-unread-symbolic", i18n("No answer yet")),
        };
        let img = gtk::Image::from_icon_name(icon);
        img.set_tooltip_text(Some(&tip));
        img.add_css_class("dim-label");
        row.append(&img);
        let mut name = if a.name.is_empty() {
            a.email.clone()
        } else {
            a.name.clone()
        };
        if me.iter().any(|m| m.eq_ignore_ascii_case(&a.email)) {
            name = i18n_f("{name} (you)", &[("name", &name)]);
        }
        let label = gtk::Label::new(Some(&name));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_tooltip_text(Some(&a.email));
        label.set_hexpand(true);
        row.append(&label);
        if Some(a.email.to_lowercase()) == organizer {
            let o = gtk::Label::new(Some(&i18n("Organizer")));
            o.add_css_class("dim-label");
            o.add_css_class("caption");
            row.append(&o);
        }
        bx.append(&row);
    }
    bx
}

/// The address of an online meeting: the event's URL, or the first link
/// to a meeting service in its location or notes.
pub(crate) fn meeting_link(c: &Comp) -> Option<String> {
    const HOSTS: [&str; 10] = [
        "zoom.us/",
        "meet.google.com/",
        "teams.microsoft.com/",
        "teams.live.com/",
        "webex.com/",
        "whereby.com/",
        "meet.jit.si/",
        "gotomeeting.com/",
        "bluejeans.com/",
        "facetime.apple.com/",
    ];
    let links = |text: &str| -> Vec<String> {
        text.split(|ch: char| ch.is_whitespace() || ch == '<' || ch == '>' || ch == '"')
            .filter(|w| w.starts_with("https://"))
            .map(|w| w.trim_end_matches(['.', ',', ')', ';']).to_string())
            .collect()
    };
    let is_meeting = |l: &str| HOSTS.iter().any(|h| l.contains(h));
    if !c.url.is_empty() && is_meeting(&c.url) {
        return Some(c.url.clone());
    }
    // Some servers put it in an X- property of their own.
    for name in ["X-GOOGLE-CONFERENCE", "X-MICROSOFT-ONLINEMEETINGCONFLINK", "CONFERENCE"] {
        let v = c.node.text(name);
        if v.starts_with("https://") {
            return Some(v);
        }
    }
    [c.location.as_str(), c.description.as_str()]
        .iter()
        .flat_map(|t| links(t))
        .find(|l| is_meeting(l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::tree::Node;

    #[test]
    fn a_meeting_link_is_found_where_it_was_put() {
        let comp = |extra: &str| {
            Comp::read(
                &Node::parse(&format!("BEGIN:VEVENT\nUID:1\nDTSTART:20261102T090000Z\n{extra}END:VEVENT\n"))
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            meeting_link(&comp("DESCRIPTION:Join: https://us02web.zoom.us/j/123?pwd=x.\n")).as_deref(),
            Some("https://us02web.zoom.us/j/123?pwd=x")
        );
        assert_eq!(
            meeting_link(&comp("LOCATION:https://meet.google.com/abc-defg-hij\n")).as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(meeting_link(&comp("DESCRIPTION:see https://example.org\n")), None);
    }
}
