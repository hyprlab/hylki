//! Settings → Focus Mode: everything about the mode, in one place. While it is
//! on these decide the layout of the sidebar, the list and the reader, in place
//! of the settings on the other pages.

use adw::prelude::*;

use super::super::choices::list_layout_index;
use super::super::rows::no_truncate;
use super::super::{PrefInput, Preferences, ViewSender};
use crate::config::FocusPart;
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct FocusPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    /// Every switch, with the part it sets: the mode can change from the main
    /// menu or a shortcut while the page is open.
    switches: Vec<(adw::SwitchRow, FocusPart)>,
}

/// One switch of the mode, added to `group` and noted in `switches`.
fn switch(
    group: &adw::PreferencesGroup,
    switches: &mut Vec<(adw::SwitchRow, FocusPart)>,
    sender: &ViewSender,
    part: FocusPart,
    title: &str,
    subtitle: Option<&str>,
) {
    let row = adw::SwitchRow::new();
    row.set_title(title);
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let sender = sender.clone();
    row.connect_active_notify(move |row| {
        sender.input(PrefInput::ToggleFocus(part, row.is_active()));
    });
    group.add(&row);
    switches.push((row, part));
}

impl FocusPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        use FocusPart as F;
        let page = adw::PreferencesPage::new();
        let mut switches = Vec::new();
        let new_group = |title: String, description: Option<String>| {
            let group = adw::PreferencesGroup::new();
            group.set_title(&title);
            group.set_description(description.as_deref());
            page.add(&group);
            group
        };

        let group = new_group(
            i18n("Focus Mode"),
            Some(i18n(
                "While it is on, this page decides the layout in place of the other settings. Ctrl+Shift+F or the main menu turns it on.",
            )),
        );
        let mut add =
            |group: &adw::PreferencesGroup, part, title: String, subtitle: Option<String>| {
                switch(
                    group,
                    &mut switches,
                    sender,
                    part,
                    &title,
                    subtitle.as_deref(),
                );
            };
        add(&group, F::Enabled, i18n("Focus Mode"), None);
        add(&group, F::StartFocused, i18n("Start in Focus Mode"), None);

        let group = new_group(i18n("Sidebar"), None);
        add(
            &group,
            F::RailSidebar,
            i18n("Fold the sidebar to the icon rail"),
            None,
        );
        add(&group, F::UnreadCounts, i18n("Unread counts"), None);
        add(
            &group,
            F::RailDots,
            i18n("Unread dots instead of counts"),
            None,
        );
        add(
            &group,
            F::HoverExpand,
            i18n("Expand the sidebar on hover"),
            None,
        );
        add(
            &group,
            F::HideAccounts,
            i18n("Hide the accounts in the sidebar"),
            Some(i18n("Only the unified section stays.")),
        );
        add(
            &group,
            F::FoldAccounts,
            i18n("Fold up the accounts"),
            Some(i18n("Collapses account avatars when Focus Mode is enabled")),
        );
        add(
            &group,
            F::ClickFolds,
            i18n("Fold accounts with one click"),
            None,
        );
        add(
            &group,
            F::FoldUnified,
            i18n("Fold up the unified rows"),
            None,
        );

        let group = new_group(i18n("Unified"), None);
        add(&group, F::ShowInboxes, i18n("Inboxes"), None);
        add(&group, F::Starred, i18n("Starred"), None);
        add(&group, F::Sent, i18n("Sent"), None);
        add(&group, F::Drafts, i18n("Drafts"), None);
        add(&group, F::Archive, i18n("Archive"), None);
        add(&group, F::Junk, i18n("Junk"), None);
        add(&group, F::Trash, i18n("Trash"), None);
        add(&group, F::Snoozed, i18n("Snoozed"), None);

        let group = new_group(i18n("Sections"), None);
        add(&group, F::ShowFavorites, i18n("Favorites"), None);
        add(&group, F::ShowFilters, i18n("Filters"), None);
        add(&group, F::ShowTags, i18n("Tags"), None);
        add(&group, F::ShowContacts, i18n("Contacts"), None);
        add(&group, F::ShowAttachments, i18n("Attachments"), None);

        let list_group = new_group(i18n("Message List"), None);
        // Set once, here: a combo's selection is never set from the view
        // again, or the two would chase each other.
        let layout = adw::ComboRow::new();
        layout.set_title(&i18n("Layout"));
        layout.set_model(Some(&gtk::StringList::new(&[
            &i18n("Cards"),
            &i18n("Single line"),
            &i18n("Automatic"),
        ])));
        no_truncate(&layout);
        layout.set_selected(list_layout_index(model.focus.layout));
        {
            let sender = sender.clone();
            layout.connect_selected_notify(move |row| {
                sender.input(PrefInput::SetFocusLayout(row.selected()));
            });
        }
        list_group.add(&layout);

        let preview = adw::ComboRow::new();
        preview.set_title(&i18n("Preview lines"));
        preview.set_model(Some(&gtk::StringList::new(&[
            &i18n("Off"),
            &i18n("1 line"),
            &i18n("2 lines"),
            &i18n("3 lines"),
        ])));
        no_truncate(&preview);
        preview.set_selected(model.focus.preview_line_count());
        {
            let sender = sender.clone();
            preview.connect_selected_notify(move |row| {
                sender.input(PrefInput::SetFocusPreviewLines(row.selected()));
            });
        }
        list_group.add(&preview);

        add(
            &list_group,
            F::ListHeader,
            i18n("Fold the message list's header"),
            Some(i18n("Everything but the sidebar toggle goes into ⋯.")),
        );
        add(
            &list_group,
            F::HideAvatars,
            i18n("Hide sender avatars"),
            None,
        );
        add(
            &list_group,
            F::HideSubject,
            i18n("Hide the subject"),
            Some(i18n("Tag chips hide with it.")),
        );
        add(
            &list_group,
            F::TagsBelow,
            i18n("Tags and labels under the text"),
            None,
        );

        let group = new_group(i18n("Reading"), None);
        add(
            &group,
            F::ReaderToolbar,
            i18n("Fold the reading pane's toolbar"),
            Some(i18n("Every button goes into its ⋯ menu.")),
        );
        add(
            &group,
            F::ReaderView,
            i18n("Reader View"),
            Some(i18n("Open messages in Reader View.")),
        );

        let page = Self { page, switches };
        page.update_view(model);
        page
    }

    /// The switches as the mode now has them.
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        for (row, part) in &self.switches {
            row.set_active(model.focus.get(*part));
            if *part == FocusPart::RailDots {
                row.set_sensitive(model.focus.unread_counts);
            }
        }
    }
}
