//! The window's structure at build time: the hosted pages, the category
//! sidebar and the search, and the first page shown.

use super::*;

impl Preferences {
    /// The pages that are components of their own: Accounts, OpenPGP, Cloud
    /// Storage and Contacts.
    pub(super) fn mount_hosted_pages(
        &mut self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
    ) {
        widgets.accounts_slot.set_child(Some(&init.accounts_panel));
        let pgp = crate::ui::pgp_keys::PgpKeys::builder()
            .launch(crate::ui::pgp_keys::PgpKeysInit {
                identities: init.identities.clone(),
            })
            .detach();
        widgets.pgp_slot.set_child(Some(pgp.widget()));
        self.pgp_keys = Some(pgp);
        let cloud = crate::ui::cloud_accounts::CloudAccounts::builder()
            .launch(())
            .forward(sender.input_sender(), |o| match o {
                crate::ui::cloud_accounts::CloudAccountsOutput::EditorOpen(open) => {
                    PrefInput::CloudEditorOpen(open)
                }
            });
        widgets.cloud_slot.set_child(Some(cloud.widget()));
        widgets
            .translation_slot
            .set_child(Some(&crate::ui::translation_page::build()));
        self.cloud = Some(cloud);
        let contacts = crate::ui::contact_sources::ContactSources::builder()
            .launch(crate::ui::contact_sources::ContactSourcesInit {
                search_open: init.contacts_search_open,
                placement: init.contacts_placement,
            })
            .forward(sender.input_sender(), PrefInput::Contacts);
        widgets.contacts_slot.set_child(Some(contacts.widget()));
        self.contacts = Some(contacts);
        self.contacts_placement_row = Some(widgets.sidebar.contacts_placement_row.clone());
    }

    /// The category sidebar (#141): a heading per section, a row per
    /// category; then the narrow-window breakpoint and the chip editors.
    pub(super) fn init_side_list_and_editors(
        &mut self,
        widgets: &PreferencesWidgets,
        sender: &ComponentSender<Self>,
        root: &adw::Window,
    ) {
        for (section, pages) in SIDE_PAGES {
            let heading = gtk::ListBoxRow::new();
            heading.set_selectable(false);
            heading.set_activatable(false);
            heading.add_css_class("settings-side-heading");
            let label = gtk::Label::new(Some(&i18n(section)));
            label.set_xalign(0.0);
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            heading.set_child(Some(&label));
            widgets.side_list.append(&heading);
            for page in pages.iter() {
                let row = gtk::ListBoxRow::new();
                row.set_widget_name(&format!("page:{}", page.id));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                line.append(&gtk::Image::from_icon_name(page.icon));
                let label = gtk::Label::new(Some(&i18n(page.title)));
                label.set_xalign(0.0);
                line.append(&label);
                row.set_child(Some(&line));
                widgets.side_list.append(&row);
            }
        }
        // One pane under 640sp: the sidebar first, a category on top of it.
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            640.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&widgets.split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(narrow);
        self.panels_stack = Some(widgets.panels_stack.clone());
        self.accounts_slot = Some(widgets.accounts_slot.clone());
        self.toolbar_editor = Some(ToolbarEditor::build(
            &widgets.appearance.toolbar_editor_box,
            sender,
        ));
        let editor_box = &widgets.appearance.toolbar_editor_box;
        editor_box.reorder_child_after(
            &widgets.appearance.toolbar_fold_list,
            editor_box.last_child().as_ref(),
        );
        // The toolbar editor's zones want a row of seven chips: the page's
        // column is 40px wider than the stock preferences clamp allows.
        widen_page(widgets.appearance_page.upcast_ref(), 640);
        self.rebuild_toolbar_chips();
        self.column_editor = Some(ColumnEditor::build(&widgets.list.column_editor_box, sender));
        self.columns_group = Some(widgets.list.columns_group.clone().upcast());
        self.rebuild_column_chips();
    }

    /// The search, the close key, the first page shown, and the other pages
    /// detached until the first paint.
    pub(super) fn init_search_and_first_page(
        &mut self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
        root: &adw::Window,
    ) {
        self.side_list = Some(widgets.side_list.clone());
        self.search = Some(SettingsSearch::build(
            &widgets.side_toolbar,
            &widgets.side_header,
            &widgets.side_scroller,
            root.upcast_ref(),
            sender,
        ));
        root.add_controller(crate::ui::close_key());
        // HYLKI_SHOWCASE_SETTINGS_SEARCH=<text>[:<n>] types <text> into the
        // search 2 s after the window is built and, given <n>, picks the
        // n-th result at 4 s (#260).
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SETTINGS_SEARCH") {
            let (text, pick) = match v.rsplit_once(':') {
                Some((t, n)) if n.parse::<usize>().is_ok() => {
                    (t.to_string(), n.parse::<usize>().ok())
                }
                _ => (v.clone(), None),
            };
            let bar = self
                .search
                .as_ref()
                .map(|s| (s.bar.clone(), s.entry.clone()));
            if let Some((bar, entry)) = bar {
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    bar.set_search_mode(true);
                    entry.set_text(&text);
                });
            }
            if let Some(n) = pick {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(4, move || {
                    s.input(PrefInput::SearchPick(n))
                });
            }
        }
        self.content_page = Some(widgets.content_page.clone());
        self.split = Some(widgets.split.clone());
        let first = init
            .start_page
            .as_deref()
            .filter(|id| side_page(id).is_some())
            .unwrap_or(if init.start_on_accounts {
                "accounts"
            } else {
                "general"
            });
        self.select_row(first);
        // Every page but the one shown leaves the stack until after the
        // first paint: laid out and styled together, the pages' hundreds of
        // rows were most of the wait for the window to appear.
        {
            let stack = &widgets.panels_stack;
            let shown = stack.visible_child_name().map(|n| n.to_string());
            let pages = stack.pages();
            let mut deferred = Vec::new();
            for i in 0..pages.n_items() {
                let Some(page) = pages.item(i).and_downcast::<gtk::StackPage>() else {
                    continue;
                };
                let name = page.name().map(|n| n.to_string()).unwrap_or_default();
                if Some(&name) != shown.as_ref() {
                    deferred.push((name, page.child()));
                }
            }
            for (_, child) in &deferred {
                stack.remove(child);
            }
            *self.deferred_pages.borrow_mut() = deferred;
            let s = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                s.input(PrefInput::MountPages);
                gtk::glib::ControlFlow::Break
            });
        }
        self.host_header = Some(widgets.host_header.clone());
    }
}
