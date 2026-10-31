//! Small widget builders for sidebar rows: headings, account initials,
//! unified sub-rows, folder rows, badges and tree indents.

use super::*;

/// A section heading's chevron (Folders, Filters, Tags) as a button of its
/// own, the size of a parent folder's arrow.
pub(super) fn heading_chevron(chevron: &gtk::Image) -> gtk::Button {
    let btn = gtk::Button::new();
    btn.set_child(Some(chevron));
    btn.add_css_class("flat");
    btn.add_css_class("tree-expander");
    btn.set_valign(gtk::Align::Center);
    btn
}

/// A section heading (Folders, Filters, Tags) around `content`. In the full
/// sidebar only its chevron folds the section, as a parent folder's arrow
/// folds its sub-folders, so the heading is a plain box padded like a
/// button: a button would take every click in its capture phase, the
/// chevron's included. In the rail, which has no chevron, it is a button
/// that folds the section.
pub(super) fn section_heading(
    content: &gtk::Box,
    chevron: Option<&gtk::Button>,
    classes: &[&str],
    msg: SidebarInput,
    sender: &ComponentSender<Sidebar>,
) -> gtk::Widget {
    let s = sender.input_sender().clone();
    let fire = move |_: &gtk::Button| {
        let _ = s.send(msg.clone());
    };
    let heading: gtk::Widget = match chevron {
        Some(btn) => {
            btn.connect_clicked(fire);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            b.add_css_class("heading-box");
            content.set_hexpand(true);
            b.append(content);
            b.upcast()
        }
        None => {
            let t = gtk::Button::new();
            t.add_css_class("flat");
            t.set_child(Some(content));
            t.connect_clicked(fire);
            t.upcast()
        }
    };
    heading.add_css_class("folders-toggle");
    for c in classes {
        heading.add_css_class(c);
    }
    heading
}

/// The Gravatar an account asked for (#189), once the app has looked it up.
/// `None` when it asked for none, when the address has none, or before the
/// answer is back — in every case the picture, emoji or initials stand.
pub(super) fn account_gravatar(email: &str) -> Option<gtk::gdk::Texture> {
    crate::services::avatar::account_face(email)
        .filter(|face| face.gravatar)
        .and_then(|_| crate::services::avatar::own_gravatar(email))
}

pub(crate) fn account_initials(name: &str, email: &str) -> String {
    let mut it = name.split_whitespace();
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.next().and_then(|w| w.chars().next());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => email
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string()),
    }
}

/// Show a tree row and slide its content open, or slide it closed and
/// drop the row once the animation is done: an empty visible row still
/// paints its chrome. The drop is skipped if it was re-expanded meanwhile.
pub(super) fn slide_tree_row(row: &gtk::ListBoxRow, rev: &gtk::Revealer, hidden: bool) {
    if !hidden {
        row.set_visible(true);
        rev.set_reveal_child(true);
    } else if row.get_visible() {
        rev.set_reveal_child(false);
        let row = row.clone();
        let rev = rev.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(220), move || {
            if !rev.reveals_child() {
                row.set_visible(false);
            }
        });
    }
}

/// A row in the "All Inboxes" sub-list: a small account pill, the account name,
/// and that inbox's unread badge. In the compact rail only the pill is shown
/// (centred), with the unread count as a corner chip. Returns the badge for
/// in-place updates.
pub(super) fn build_unified_inbox_row(
    section: &SectionData,
    inbox: &Folder,
    collapsed: bool,
    inset: bool,
) -> (gtk::ListBoxRow, gtk::Label) {
    // Show the account's configured label (defaults to its email) so accounts are
    // easy to tell apart in the All Inboxes view.
    let label = &section.account.label;

    // Small account pill (color + initials/emoji), like the header circle.
    let id = section.account.id;
    let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    circle.add_css_class("account-circle-sm");
    circle.add_css_class(&format!("acct-color-{id}"));
    circle.add_css_class(&format!("acct-ring-{id}"));
    circle.set_valign(gtk::Align::Center);
    circle.set_halign(gtk::Align::Center);
    circle.set_hexpand(false);
    circle.set_size_request(21, 21);
    let gravatar = account_gravatar(&section.account.email);
    let glyph: gtk::Widget = match (&gravatar, &section.avatar, &section.emoji) {
        (Some(texture), ..) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::picture_from_texture(texture, 21).upcast()
        }
        (None, Some(path), _) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::avatar_picture(path, 21).upcast()
        }
        (None, None, Some(em)) if !em.is_empty() => {
            crate::ui::initials::glyph_picture(em, &section.color, 0.6, 21).upcast()
        }
        _ => crate::ui::initials::glyph_picture(
            &account_initials(label, &section.account.email),
            &section.color,
            0.5,
            21,
        )
        .upcast(),
    };
    circle.append(&glyph);

    build_unified_sub_row(&circle, label, label, inbox.unread, collapsed, inset, true)
}

/// A row nested under "All Inboxes": `lead` (the account's pill), `title`,
/// and an unread badge. In the compact rail only the lead is shown
/// (centred), with the count as a corner chip and `tip` (plus the count) as
/// the tooltip. Returns the badge for in-place updates.
pub(crate) fn build_unified_sub_row(
    lead: &impl IsA<gtk::Widget>,
    title: &str,
    tip: &str,
    unread: u32,
    collapsed: bool,
    inset: bool,
    pill: bool,
) -> (gtk::ListBoxRow, gtk::Label) {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.add_css_class("folder-row");
    // The 2px pull-in centres a 21px account pill on the 16px icon column;
    // a row led by a 16px icon or disc sits on that column as it is, and
    // pulled in it read as drifting left of the header's glyph.
    if !collapsed && pill {
        hbox.add_css_class("unified-subrow");
    }

    let badge = if collapsed {
        hbox.set_halign(gtk::Align::Center);
        let tip = if unread > 0 {
            format!("{tip} ({unread})")
        } else {
            tip.to_string()
        };
        row.set_tooltip_text(Some(&tip));
        let (overlay, badge) = with_unread_overlay(lead, unread);
        hbox.append(&overlay);
        badge
    } else {
        if inset {
            lead.set_margin_start(ROW_LEFT_INSET - 6);
        }
        hbox.append(lead);
        if tip != title {
            row.set_tooltip_text(Some(tip));
        }

        let name = gtk::Label::new(Some(title));
        // The label keeps the same column either way: the icon-led row
        // gave up the 2px pull-in, so its label gives up 2px here.
        name.set_margin_start(if pill { 6 } else { 4 });
        name.set_hexpand(true);
        name.set_halign(gtk::Align::Start);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&name);

        let badge = gtk::Label::new(Some(&unread.to_string()));
        style_badge(&badge, 5);
        badge.set_visible(unread > 0);
        hbox.append(&badge);
        badge
    };

    row.set_child(Some(&hbox));
    (row, badge)
}

/// `lead` behind a spacer as wide as a tree caret, so a list's items sit
/// in from their row as the address books under Contacts do. The rail has
/// no room for it.
pub(super) fn tree_indent(lead: &impl IsA<gtk::Widget>, collapsed: bool) -> gtk::Widget {
    if collapsed {
        return lead.clone().upcast();
    }
    let indented = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_width_request(TREE_EXPANDER_WIDTH);
    indented.append(&spacer);
    indented.append(lead);
    indented.upcast()
}

/// Pins a symbolic icon to an exact, deterministic 16px box so it centres on
/// the same column as the rail's avatar circles. Left to GTK's default
/// icon-size resolution, a plain `gtk::Image`'s natural size can round a
/// fraction of a pixel off from the circles' hand-pinned, always-even
/// `set_size_request` — `Align::Center` then centres that slightly-off box
/// exactly as asked, reading as the icon column drifting right of the
/// avatar column (PR #95).
pub(crate) fn pin_icon_size(icon: &gtk::Image) {
    icon.set_pixel_size(16);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
}

/// Extra left inset on every expanded row's leading icon in the leading-
/// chevron layout, so rows don't read as shoved flush against the sidebar's
/// edge (PR #95). The classic trailing layout keeps its original geometry.
pub(super) const ROW_LEFT_INSET: i32 = 8;

/// Wrap `child` in an overlay with a small unread chip pinned to its top-right
/// corner — used in the compact rail, where there's no room for an inline badge.
/// Returns the overlay (to place in the tree) and the chip label (for updates).
pub(super) fn with_unread_overlay(
    child: &impl IsA<gtk::Widget>,
    unread: u32,
) -> (gtk::Overlay, gtk::Label) {
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(child));
    let badge = gtk::Label::new(Some(&unread.to_string()));
    style_badge(&badge, 4);
    badge.add_css_class("unread-badge-mini");
    badge.set_halign(gtk::Align::End);
    badge.set_valign(gtk::Align::Start);
    badge.set_visible(unread > 0);
    overlay.add_overlay(&badge);
    (overlay, badge)
}

/// The icon a folder row in an account wears: the filter-folder glyph in
/// the account's color where a filter rule files into it, a tag for a Gmail
/// label, otherwise its kind's own.
pub(super) fn filter_icon(section: &SectionData, folder: &Folder) -> FolderGlyph {
    if !section.filtered.iter().any(|f| f.id == folder.id) {
        // A Gmail account's own folders are labels (#374): a tag, not a
        // folder.
        if section.gmail && folder.kind == FolderKind::Custom {
            return FolderGlyph::Icon(gtk::Image::from_icon_name("tag-symbolic"));
        }
        return FolderGlyph::Plain;
    }
    if folder.kind == FolderKind::Custom {
        FolderGlyph::Icon(filtered_folder_icon(folder, section.account.id))
    } else {
        FolderGlyph::Marked(section.account.id)
    }
}

/// What a folder row shows for its icon.
pub(super) enum FolderGlyph {
    /// The folder kind's own icon.
    Plain,
    /// The caller's icon in place of it (a filter destination's tinted glyph).
    Icon(gtk::Image),
    /// The kind's own icon, grey as ever, with a small filter glyph in the
    /// account's color riding its corner: a main folder (Archive, Junk…)
    /// that a filter files into.
    Marked(u32),
}

/// The icon of a folder a filter files into: a custom folder wears the
/// filter-folder glyph, a main folder (Archive, Junk…) keeps its own —
/// either in the account's color, which is what says "part of a filter".
pub(super) fn filtered_folder_icon(folder: &Folder, account_id: u32) -> gtk::Image {
    let name = if folder.kind == FolderKind::Custom {
        "filter-folder-symbolic"
    } else {
        folder.kind.icon()
    };
    let icon = gtk::Image::from_icon_name(name);
    icon.add_css_class(&format!("acct-tint-{account_id}"));
    icon
}

/// An unread chip: the count in a pill that never pushes its row wider
/// than the sidebar — past `max_chars` digits it ends in an ellipsis.
pub(super) fn style_badge(badge: &gtk::Label, max_chars: i32) {
    badge.add_css_class("unread-badge");
    badge.set_valign(gtk::Align::Center);
    badge.set_ellipsize(gtk::pango::EllipsizeMode::End);
    badge.set_max_width_chars(max_chars);
}

/// Build one folder row. `depth` indents sub-folders to mirror the server's
/// hierarchy (0 = top level; only meaningful for custom folders).
pub(super) fn build_folder_row(
    folder: &Folder,
    collapsed: bool,
    depth: usize,
    lead: Option<&gtk::Widget>,
    inset: bool,
    glyph: FolderGlyph,
    h: &FolderHierarchy,
) -> (gtk::ListBoxRow, Option<gtk::Label>) {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.add_css_class("folder-row");
    // Custom folders show only their leaf name; the tooltip carries the whole
    // hierarchy, so nine folders all named "Archive" stay tellable apart (#51).
    if let Some(d) = h
        .delimiter
        .filter(|d| folder.kind == FolderKind::Custom && folder.path.contains(*d))
    {
        let pretty = folder
            .path
            .split(d)
            .map(|level| h.name_of(level))
            .collect::<Vec<_>>()
            .join(" › ");
        row.set_tooltip_text(Some(&pretty));
    }
    if let Some(lead) = lead {
        hbox.append(lead);
    }
    if !collapsed && depth > 0 {
        // Indent nested folders; capped so a pathological hierarchy can't push
        // the name out of the sidebar.
        hbox.set_margin_start(14 * depth.min(4) as i32);
    }

    // The folder kind's icon, unless the caller brought its own (the
    // Filters rows' account-tinted glyph).
    let img = match &glyph {
        FolderGlyph::Icon(icon) => icon.clone(),
        _ => gtk::Image::from_icon_name(folder.kind.icon()),
    };
    img.add_css_class("folder-icon");
    pin_icon_size(&img);
    // A marked folder carries the filter glyph on the icon's corner: top
    // right, or bottom right in the rail where the unread badge has the
    // top. The mark takes no room of its own.
    let visual: gtk::Widget = match glyph {
        FolderGlyph::Marked(account_id) => {
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&img));
            let mark = gtk::Image::from_icon_name("filter-symbolic");
            mark.set_pixel_size(9);
            mark.add_css_class("filter-mark");
            mark.add_css_class(&format!("acct-tint-{account_id}"));
            mark.set_halign(gtk::Align::End);
            mark.set_valign(if collapsed {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });
            overlay.add_overlay(&mark);
            overlay.set_measure_overlay(&mark, false);
            overlay.upcast()
        }
        _ => img.clone().upcast(),
    };

    // Sent wears no unread chip: what you sent is not new mail. `None` keeps
    // it off the in-place update lists too.
    let counted = folder.kind != FolderKind::Sent;
    let unread = if counted { folder.unread } else { 0 };
    let badge = if collapsed {
        hbox.set_halign(gtk::Align::Center);
        let tip = if unread > 0 {
            format!("{} ({})", folder.name, unread)
        } else {
            folder.name.clone()
        };
        row.set_tooltip_text(Some(&tip));
        // Every folder carries an unread chip; in the rail it rides the icon's
        // corner so new mail shows without expanding the sidebar.
        let (overlay, badge) = with_unread_overlay(&visual, unread);
        hbox.append(&overlay);
        counted.then_some(badge)
    } else {
        if inset {
            visual.set_margin_start(ROW_LEFT_INSET);
        }
        hbox.append(&visual);
        let name = gtk::Label::new(Some(&folder.name));
        name.set_hexpand(true);
        name.set_halign(gtk::Align::Start);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&name);

        // Every folder shows an unread count chip — present but hidden when
        // zero so it can update in place.
        let badge = gtk::Label::new(Some(&unread.to_string()));
        style_badge(&badge, 5);
        badge.set_visible(unread > 0);
        hbox.append(&badge);
        counted.then_some(badge)
    };

    row.set_child(Some(&hbox));
    (row, badge)
}

/// Move a folder row's content into a revealer, so folding its parent
/// slides it closed rather than snapping. Built with no transition (a
/// rebuild must reach full height in one pass); the caller keeps the
/// revealer to arm it, or to fold it later. `None` for a row with no content.
pub(super) fn wrap_row_in_revealer(row: &gtk::ListBoxRow, hidden: bool) -> Option<gtk::Revealer> {
    let content = row.child()?;
    row.set_child(gtk::Widget::NONE);
    let rev = gtk::Revealer::new();
    rev.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    rev.set_transition_duration(0);
    rev.set_child(Some(&content));
    rev.set_reveal_child(!hidden);
    row.set_child(Some(&rev));
    Some(rev)
}
