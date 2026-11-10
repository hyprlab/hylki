//! A reusable modal that browses the contacts to *pick* one — used by the
//! composer's address-book button. (General browsing lives in the in-app
//! contacts view, `ui/contacts_page.rs`, shown in the main content area,
//! which the modal's header button opens.)

use adw::prelude::*;

use crate::contacts::Contact;
use crate::i18n::i18n;

/// Present the contacts browser as a modal of `parent`. `on_choose` runs with
/// the chosen contact when a row is activated, `on_show_all` when the header
/// button asks for Hylki's Contacts view (the window closes either way).
pub(super) fn present(
    parent: &impl IsA<gtk::Window>,
    on_choose: impl Fn(Contact) + 'static,
    on_show_all: impl Fn() + 'static,
) {
    let on_choose = std::rc::Rc::new(on_choose);
    let mut contacts = crate::contacts::read_contacts();
    contacts.sort_by_key(|c| c.name.to_lowercase());

    let win = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(i18n("Contacts"))
        .default_width(420)
        .default_height(560)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let open_btn = gtk::Button::from_icon_name("x-office-address-book-symbolic");
    open_btn.set_tooltip_text(Some(i18n("Open Contacts").as_str()));
    {
        let w = win.clone();
        open_btn.connect_clicked(move |_| {
            w.close();
            on_show_all();
        });
    }
    header.pack_end(&open_btn);
    toolbar.add_top_bar(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(i18n("Search contacts").as_str()));
    content.append(&search);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);

    if contacts.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("x-office-address-book-symbolic")
            .title(i18n("No Contacts"))
            .description(i18n("Add contacts in the Contacts view to see them here."))
            .build();
        scroller.set_child(Some(&empty));
    } else {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for c in &contacts {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&c.name));
            if c.name != c.email {
                row.set_subtitle(&gtk::glib::markup_escape_text(&c.email));
            }
            row.set_activatable(true);
            let avatar = adw::Avatar::new(32, Some(&c.name), true);
            let initials = crate::ui::initials::InitialsPaintable::for_name(&c.name);
            avatar.set_custom_image(initials.as_ref());
            row.add_prefix(&avatar);

            let contact = c.clone();
            let cb = on_choose.clone();
            let w = win.clone();
            row.connect_activated(move |_| {
                cb(contact.clone());
                w.close();
            });
            list.append(&row);
        }

        // Live filter by name or email.
        let query = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let filter_contacts = contacts.clone();
        let q = query.clone();
        list.set_filter_func(move |row| {
            let q = q.borrow();
            if q.is_empty() {
                return true;
            }
            filter_contacts
                .get(row.index() as usize)
                .map(|c| {
                    c.name.to_lowercase().contains(q.as_str())
                        || c.email.to_lowercase().contains(q.as_str())
                })
                .unwrap_or(true)
        });
        let q = query.clone();
        let list2 = list.clone();
        search.connect_search_changed(move |e| {
            *q.borrow_mut() = e.text().to_lowercase();
            list2.invalidate_filter();
        });

        scroller.set_child(Some(&list));
    }

    content.append(&scroller);
    toolbar.set_content(Some(&content));
    win.set_content(Some(&toolbar));
    win.present();
}

/// Open GNOME Contacts showing one contact. Best effort: outside a sandbox
/// `gnome-contacts -i <uid>` selects the individual; the Flatpak D-Bus path
/// can only raise the app.
pub(super) fn launch_gnome_contacts_for(uid: &str) {
    if crate::desktop::platform::is_flatpak() || uid.is_empty() {
        launch_gnome_contacts();
        return;
    }
    let uid: String = uid
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\'')
        .collect();
    if let Ok(app) = gtk::gio::AppInfo::create_from_commandline(
        format!("gnome-contacts -i '{uid}'"),
        Some(i18n("Contacts").as_str()),
        gtk::gio::AppInfoCreateFlags::NONE,
    ) {
        let _ = app.launch(&[], gtk::gio::AppLaunchContext::NONE);
    }
}

/// Launch the GNOME Contacts application via the desktop session.
pub(crate) fn launch_gnome_contacts() {
    // Inside a Flatpak sandbox `gnome-contacts` isn't on the path, so a plain
    // exec fails. Reach the host app by D-Bus-activating it (it's a
    // DBusActivatable GApplication).
    if crate::desktop::platform::is_flatpak() {
        if let Err(e) = activate_host_contacts() {
            // No `flatpak-spawn --host` fallback: reaching it needs
            // --talk-name=org.freedesktop.Flatpak, which is a general-purpose
            // "run anything on the host" permission and not worth holding for a
            // button. D-Bus activation is the supported path.
            tracing::warn!("could not activate GNOME Contacts: {e}");
        }
        return;
    }
    if let Ok(app) = gtk::gio::AppInfo::create_from_commandline(
        "gnome-contacts",
        Some(i18n("Contacts").as_str()),
        gtk::gio::AppInfoCreateFlags::NONE,
    ) {
        let _ = app.launch(&[], gtk::gio::AppLaunchContext::NONE);
    }
}

/// Activate the host `org.gnome.Contacts` service over the session bus (the
/// standard `org.freedesktop.Application.Activate` a GApplication exposes).
fn activate_host_contacts() -> Result<(), Box<dyn std::error::Error>> {
    let conn = zbus::blocking::Connection::session()?;
    let platform_data: std::collections::HashMap<String, zbus::zvariant::Value> =
        std::collections::HashMap::new();
    conn.call_method(
        Some("org.gnome.Contacts"),
        "/org/gnome/Contacts",
        Some("org.freedesktop.Application"),
        "Activate",
        &(platform_data,),
    )?;
    Ok(())
}
