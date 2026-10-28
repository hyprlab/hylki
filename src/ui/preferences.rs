//! The settings window (#141): a category sidebar beside a stack of pages.
//! Most pages are built in `preferences/pages`; Accounts (`ui/accounts.rs`),
//! OpenPGP, Cloud Storage, Translation and Contacts are components of their
//! own hosted in the stack.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{
    AppTheme, ClockStyle, DateStyle, LauncherBadge, MessageTheme, ReaderToolbar, ToolbarItem,
    ToolbarSide, TrayIcon,
};
use crate::i18n::{i18n, i18n_f, i18n_noop, ni18n_f};
use crate::ui::chip_flow::ChipFlow;
use std::rc::Rc;

mod alerts;
mod appearance;
mod chips;
mod choices;
mod composing;
mod contacts;
mod init_layout;
mod init_message_rows;
mod init_rows;
mod messages;
mod navigation;
mod pages;
mod reading;
mod rows;
mod search;
mod sidebar_page;
mod spelling;
mod system;

use chips::{ColumnEditor, ToolbarEditor};
pub(crate) use choices::language_choices;
pub(crate) use choices::{
    contacts_placement_from_index, contacts_placement_index, contacts_placement_model,
};
use choices::{
    files_action_index, files_large_index, link_browser_index, placement_from_index,
    placement_index, sound_index, sound_labels, APP_THEMES, CLOCK_STYLES, DATE_STYLES,
    FETCH_INTERVALS, KEEP_DOWNLOADS_DAYS, LAUNCHER_BADGES, MESSAGE_THEMES, RENDERERS, TRAY_ICONS,
};
pub(crate) use messages::{PrefInit, PrefInput, PrefOutput};
use navigation::{side_page, SIDE_PAGES};
use pages::{
    AppearancePage, BackupPage, ComposingPage, ConversationsPage, DateTimePage, GeneralPage,
    ListPage, PrivacyPage, ReadingPage, SidebarPage, SystemPage,
};
use rows::{compose_font_setup, middle_ellipsize, widen_combo_value, widen_page};
pub(crate) use search::NOT_SEARCHED;
use search::{scroll_to, SettingsSearch};
use spelling::{rebuild_personal_words, SpellLangRows};
use system::hide_edit_icon;

// ---- Preferences window -----------------------------------------------------

pub(crate) struct Preferences {
    /// Mirrors the notifications switch, so the "show sender and subject" row
    /// below it can grey out when nothing is being posted at all.
    notifications: bool,
    /// The notification buttons, kept whole so each switch can hand the
    /// app the full set (#244).
    notification_buttons: crate::config::NotificationButtons,
    /// The new-mail sound (#292): on or off, and which one.
    sound: crate::config::NewMailSound,
    /// The custom sound file, a copy in the data directory.
    custom_sound: Option<std::path::PathBuf>,
    show_unified: bool,
    /// The unified Starred / Sent / Drafts switches, kept whole so each
    /// toggle can hand the app the full set.
    unified_kinds: crate::config::UnifiedKinds,
    /// The unified rows' unread-chip switches, likewise kept whole.
    unified_chips: crate::config::UnifiedChips,
    /// The Printing switches, kept whole so each toggle can hand the app
    /// the full set (#359).
    print_options: crate::config::PrintOptions,
    /// Mirrors the "Accounts in the sidebar" switch, which the main menu
    /// can flip too.
    show_accounts: bool,
    /// The icon rail's fold-up switches, kept whole so each toggle can hand
    /// the app the full set.
    rail_fold: crate::config::RailFold,
    /// The browsers the links combo offers, in the order it lists them
    /// (empty inside the Flatpak sandbox, which cannot see them).
    browsers: Vec<crate::ui::launch::Browser>,
    /// The reader toolbar layout being edited (the app applies every drop).
    toolbar: ReaderToolbar,
    /// Focus Mode's switches, kept whole so each toggle hands the app the
    /// full set (the main menu and Ctrl+Shift+F flip the master switch too).
    focus: crate::config::FocusMode,
    /// The three drop zones of the toolbar editor, filled from `toolbar`.
    toolbar_editor: Option<ToolbarEditor>,
    /// The single-line columns being edited (#334), and their editor.
    list_columns: Vec<crate::config::ListColumn>,
    column_editor: Option<ColumnEditor>,
    /// Message List → Columns, which the list's headings open to (#443).
    columns_group: Option<gtk::Widget>,
    /// Whether the list can be on one line at all, which the columns are
    /// for.
    list_single_possible: bool,
    /// Whether swipe actions are on (the reverse switch follows it).
    swipe_enabled: bool,
    /// Mirrors the threading switch, so the "threaded message list" row below
    /// it can grey out when conversations aren't grouped at all.
    threading: bool,
    /// Mirrors the expandable-conversations switch — "expand by default" only
    /// means anything while conversations can expand in the list at all.
    thread_expansion: bool,
    /// Mirrors the list-palette switch, so the hover row under it can grey
    /// out when there is no palette to open.
    list_palette: bool,
    /// Mirrors the card-actions mode: only its hidden-behind-a-toggle choice
    /// has a ⋯, so only there can the ⋯ stand in for a menu.
    card_actions_hover: bool,
    /// Mirrors the sender-avatars switch: with no circles drawn at all,
    /// whose face they would show doesn't arise (#189).
    avatars: bool,
    /// The content stack (one child per category, plus the accounts panel
    /// in its "accounts" slot), driven by the sidebar (#141).
    panels_stack: Option<gtk::Stack>,
    /// The accounts panel's slot, for the fresh panel each open brings.
    accounts_slot: Option<adw::Bin>,
    /// Pages taken out of the stack until after the window's first paint:
    /// only the page shown is laid out and styled for it, the rest come
    /// back a moment later (or at once, should one be asked for first).
    deferred_pages: std::cell::RefCell<Vec<(String, gtk::Widget)>>,
    /// The sidebar list, for selecting a category from update().
    side_list: Option<gtk::ListBox>,
    /// The settings search in the sidebar (#260).
    search: Option<SettingsSearch>,
    /// The content pane's page, whose title names the chosen category.
    content_page: Option<adw::NavigationPage>,
    /// The split view, to bring the content forward when collapsed.
    split: Option<adw::NavigationSplitView>,
    /// The accounts component, told which of its pages the sidebar chose.
    accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// The content header bar; hidden while the accounts editor subpage is
    /// open, whose own header takes over.
    host_header: Option<adw::HeaderBar>,
    /// Settings → Sidebar's Contacts placement row, kept in step with the
    /// same row in Settings → Contacts.
    contacts_placement_row: Option<adw::ComboRow>,
    /// The OpenPGP page (#133), kept alive with the window.
    pgp_keys: Option<Controller<crate::ui::pgp_keys::PgpKeys>>,
    /// (name, address) per enabled account and alias, the rows of the
    /// "Send new messages from" combo after its first (#157).
    identities: Vec<(String, String)>,
    /// The Cloud Storage page (#144), likewise.
    cloud: Option<Controller<crate::ui::cloud_accounts::CloudAccounts>>,
    /// The Contacts page (#408): every address book, CardDAV accounts and
    /// LDAP directories (#307) among them, likewise.
    contacts: Option<Controller<crate::ui::contact_sources::ContactSources>>,
    /// A page about one address book is pushed over the Contacts list.
    contacts_open: bool,
    /// The account editor is up in the accounts slot: leaving it for another
    /// category asks about the unsaved changes first.
    editor_open: bool,
    /// Which side page's editor is up: "accounts", "cloud" or "contacts".
    editor_page: &'static str,
    /// What downloaded mail takes up (#394): message bodies, attachment
    /// files and the whole cache file, in bytes; `None` while measuring.
    storage: Option<(u64, u64, u64)>,
    /// The GNOME Files extension (#188): installed, loaded, loader present.
    nautilus: crate::desktop::nautilus_ext::State,
    /// The terminal command that installs the nautilus-python package on
    /// this machine, for the copyable row; `None` on a distribution whose
    /// package manager is not known.
    nautilus_cmd: Option<&'static str>,
    /// What files handed in from Files open into, and the size limit.
    files: crate::config::FilesPrefs,
    /// Those rows, for the app to move when a hand-off dialog's "always
    /// do this" changes the preference while the window is up. Set only
    /// when they differ: a combo row set to its current value is a no-op,
    /// but one set from `#[watch]` on every update hung GTK's list-item
    /// manager in the pre-warmed (unmapped) window.
    files_rows: Option<(adw::ComboRow, adw::ComboRow, adw::SpinRow)>,
    /// Composing → Spelling → Languages, kept to follow a change made from
    /// a composer's right-click menu (#365).
    spell_langs: Option<SpellLangRows>,
}

/// The `sender` the view's handlers report through. Setting a row's initial
/// value fires the same signal a change by hand does: a combo row's model
/// going in moves its selection to the first entry, and the saved value then
/// moves it again. Built hidden a moment after startup (`AppMsg::PrewarmSettings`),
/// the window so reported every choice twice, the first time wrong: the tray
/// item was taken down and published again for each, which Cinnamon's status
/// applet answered by crashing (#275). Nothing is reported until `ready` is
/// set, at the end of `init`, once the saved values are in.
#[derive(Clone)]
struct ViewSender {
    inner: ComponentSender<Preferences>,
    ready: Rc<std::cell::Cell<bool>>,
}

impl ViewSender {
    fn input(&self, msg: PrefInput) {
        if self.ready.get() {
            self.inner.input(msg);
        } else {
            tracing::debug!("settings window: {msg:?} while the rows are being set, not reported");
        }
    }

    fn output(&self, msg: PrefOutput) -> Result<(), PrefOutput> {
        self.inner.output(msg)
    }
}

#[relm4::component(pub(crate))]
impl Component for Preferences {
    type Init = PrefInit;
    type Input = PrefInput;
    type Output = PrefOutput;
    type CommandOutput = ();

    // The category pages, built by `pages` before the view and placed in
    // the stack below with `#[local_ref]`.
    additional_fields! {
        general: GeneralPage,
        appearance: AppearancePage,
        sidebar: SidebarPage,
        list: ListPage,
        conversations: ConversationsPage,
        reading: ReadingPage,
        composing: ComposingPage,
        privacy: PrivacyPage,
        datetime: DateTimePage,
        system: SystemPage,
        backup: BackupPage,
    }

    view! {
        adw::Window {
            set_modal: false,
            set_default_width: 920,
            // The same size every time: the two-pane layout (#141) fits its
            // sidebar at this height, and nothing is remembered from a resize.
            set_default_height: 848,
            set_title: Some(i18n("Settings").as_str()),
            // Closing hides: the window is kept and shown again next time.
            set_hide_on_close: true,

            connect_close_request[sender] => move |_| {
                let _ = sender.output(PrefOutput::Closed);
                gtk::glib::Propagation::Proceed
            },

            // One window, two panes (#141): a sidebar of categories on the
            // left, the chosen category's groups on the right. The account
            // pages (accounts, tags, filters, senders) belong to the
            // AccountsWindow component, handed in via PrefInit with its own
            // sub-navigation for the account editor; while that editor is
            // open the content header hides, so the editor's own back/Save
            // header takes over. Narrow, the split collapses to one pane
            // (breakpoint in init) and the sidebar becomes the first page.
            #[wrap(Some)]
            #[name = "split"]
            set_content = &adw::NavigationSplitView {
                set_min_sidebar_width: 180.0,
                set_max_sidebar_width: 250.0,
                set_sidebar_width_fraction: 0.27,

                #[wrap(Some)]
                set_sidebar = &adw::NavigationPage {
                    set_title: &i18n("Settings"),

                    #[wrap(Some)]
                    #[name = "side_toolbar"]
                    set_child = &adw::ToolbarView {
                        #[name = "side_header"]
                        add_top_bar = &adw::HeaderBar {
                            set_show_end_title_buttons: false,
                        },

                        #[wrap(Some)]
                        #[name = "side_scroller"]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,

                            #[wrap(Some)]
                            #[name = "side_list"]
                            set_child = &gtk::ListBox {
                                add_css_class: "navigation-sidebar",
                                add_css_class: "settings-side",
                                set_selection_mode: gtk::SelectionMode::Single,
                                connect_row_selected[sender] => move |_, row| {
                                    if let Some(id) = row
                                        .map(|r| r.widget_name().to_string())
                                        .and_then(|n| n.strip_prefix("page:").map(str::to_string))
                                    {
                                        sender.input(PrefInput::SelectPage(id));
                                    }
                                },
                            },
                        },
                    },
                },

                #[wrap(Some)]
                #[name = "content_page"]
                set_content = &adw::NavigationPage {
                    set_title: &i18n("General"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        #[name = "host_header"]
                        add_top_bar = &adw::HeaderBar {
                            set_show_start_title_buttons: false,
                        },

                        #[wrap(Some)]
                        #[name = "panels_stack"]
                        set_content = &gtk::Stack {
                            // Sections switch outright, no fade.
                            set_transition_type: gtk::StackTransitionType::None,
                            // Only the shown page is measured: homogeneous, the
                            // stack measured every page's rows on every layout
                            // pass, which was most of the window's first paint.
                            set_hhomogeneous: false,
                            set_vhomogeneous: false,

                            #[name = "accounts_slot"]
                            add_named[Some("accounts")] = &adw::Bin {},

                            // The OpenPGP key manager (#133), its own component.
                            #[name = "pgp_slot"]
                            add_named[Some("openpgp")] = &adw::Bin {},

                            // Cloud attachment accounts (#144), its own component.
                            #[name = "cloud_slot"]
                            add_named[Some("cloud")] = &adw::Bin {},

                            // Message translation (#327), built by its own module.
                            #[name = "translation_slot"]
                            add_named[Some("translation")] = &adw::Bin {},

                            // Every address book (#408), its own component.
                            #[name = "contacts_slot"]
                            add_named[Some("contacts")] = &adw::Bin {},

                            #[local_ref]
                            add_named[Some("general")] = general_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("appearance")] = appearance_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("sidebar")] = sidebar_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("list")] = list_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("conversations")] = conversations_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("reading")] = reading_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("composing")] = composing_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("privacy")] = privacy_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("datetime")] = datetime_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("system")] = system_page -> adw::PreferencesPage {},

                            #[local_ref]
                            add_named[Some("backup")] = backup_page -> adw::PreferencesPage {},
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let t_init = std::time::Instant::now();
        let mut model = Preferences {
            storage: None,
            nautilus: crate::desktop::nautilus_ext::State::read(),
            nautilus_cmd: crate::desktop::platform::nautilus_python_install_command(),
            files: init.files,
            files_rows: None,
            spell_langs: None,
            browsers: crate::ui::launch::browsers(),
            notifications: init.notifications,
            notification_buttons: init.notification_buttons,
            sound: crate::config::load_new_mail_sound(),
            custom_sound: crate::config::custom_sound(),
            toolbar: init.reader_toolbar.clone(),
            focus: init.focus,
            toolbar_editor: None,
            list_columns: init.list_columns.clone(),
            column_editor: None,
            columns_group: None,
            list_single_possible: init.list_layout != crate::config::ListLayout::Cards,
            show_unified: init.show_unified,
            unified_kinds: init.unified_kinds,
            unified_chips: init.unified_chips,
            print_options: init.print_options,
            show_accounts: init.show_accounts,
            rail_fold: init.rail_fold,
            swipe_enabled: init.swipe_enabled,
            threading: init.threading,
            thread_expansion: init.thread_expansion,
            list_palette: init.list_palette,
            card_actions_hover: init.card_actions_hover,
            avatars: init.avatars,
            panels_stack: None,
            accounts_slot: None,
            deferred_pages: std::cell::RefCell::new(Vec::new()),
            side_list: None,
            search: None,
            content_page: None,
            split: None,
            accounts_sender: init.accounts_sender.clone(),
            host_header: None,
            contacts_placement_row: None,
            pgp_keys: None,
            identities: init.identities.clone(),
            cloud: None,
            contacts: None,
            contacts_open: false,
            editor_open: false,
            editor_page: "accounts",
        };

        // The view's handlers report through `ViewSender`, silent until the
        // rows below hold their saved values.
        let component_sender = sender;
        let sender = ViewSender {
            inner: component_sender.clone(),
            ready: Rc::new(std::cell::Cell::new(false)),
        };
        let general = GeneralPage::build(&model, &sender);
        let general_page = &general.page;
        let appearance = AppearancePage::build(&model, &sender);
        let appearance_page = &appearance.page;
        let sidebar = SidebarPage::build(&model, &sender);
        let sidebar_page = &sidebar.page;
        let list = ListPage::build(&model, &sender);
        let list_page = &list.page;
        let conversations = ConversationsPage::build(&model, &sender);
        let conversations_page = &conversations.page;
        let reading = ReadingPage::build(&model, &sender);
        let reading_page = &reading.page;
        let composing = ComposingPage::build(&sender);
        let composing_page = &composing.page;
        let privacy = PrivacyPage::build(&sender);
        let privacy_page = &privacy.page;
        let datetime = DateTimePage::build(&sender);
        let datetime_page = &datetime.page;
        let system = SystemPage::build(&model, &sender);
        let system_page = &system.page;
        let backup = BackupPage::build(&sender);
        let backup_page = &backup.page;
        let widgets = view_output!();
        let ready = sender.ready.clone();
        let sender = component_sender;
        tracing::debug!(
            "settings window: preferences view built in {:?}",
            t_init.elapsed()
        );

        model.make_combos_whole(&widgets);
        model.init_general_rows(&widgets, &init);
        model.init_notification_rows(&widgets, &init);
        model.init_sidebar_rows(&widgets, &init);
        model.init_list_rows(&widgets, &init);
        model.init_background_and_galleries(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: preferences icon strip done at {:?}",
            t_init.elapsed()
        );

        model.init_tray_rows(&widgets, &init);
        tracing::debug!(
            "settings window: prefs tail A (tray) at {:?}",
            t_init.elapsed()
        );
        model.init_keyboard_and_system_rows(&widgets, &init);
        model.init_conversation_and_reading_rows(&widgets, &init);
        model.init_files_and_links_rows(&widgets, &init, &sender);
        model.init_composing_rows(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: prefs tail B (words) at {:?}",
            t_init.elapsed()
        );

        model.init_date_and_theme_rows(&widgets, &init);
        tracing::debug!(
            "settings window: prefs tail C (themes) at {:?}",
            t_init.elapsed()
        );

        model.init_reader_fonts_and_gestures(&widgets, &init);
        model.mount_hosted_pages(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: prefs tail D (before sidebar rows) at {:?}",
            t_init.elapsed()
        );
        model.init_side_list_and_editors(&widgets, &sender, &root);
        tracing::debug!(
            "settings window: prefs tail E (sidebar rows built) at {:?}",
            t_init.elapsed()
        );
        model.init_search_and_first_page(&widgets, &init, &sender, &root);

        tracing::debug!("settings window: preferences init {:?}", t_init.elapsed());
        // From here on a row's signal is the user's doing.
        ready.set(true);
        ComponentParts { model, widgets }
    }

    /// The pages' `#[watch]` setters, in the order the single view had
    /// them; the view itself has none left.
    fn pre_view() {
        general.update_view(model);
        appearance.update_view(model);
        sidebar.update_view(model);
        list.update_view(model);
        conversations.update_view(model);
        reading.update_view(model);
        system.update_view(model);
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            PrefInput::ChangeDateStyle(i) => self.on_change_date_style(i, sender),
            PrefInput::ChangeClockStyle(i) => self.on_change_clock_style(i, sender),
            PrefInput::ChangeRenderer(i) => self.on_change_renderer(i, sender),
            PrefInput::ChangeLanguage(i) => self.on_change_language(i, sender),
            PrefInput::NautilusInstall => self.on_nautilus_install(root),
            PrefInput::MeasureStorage => self.on_measure_storage(sender),
            PrefInput::StorageMeasured {
                bodies,
                files,
                total,
            } => self.on_storage_measured(bodies, files, total),
            PrefInput::RemoveDownloads => self.on_remove_downloads(sender, root),
            PrefInput::RemoveDownloadsNow => self.on_remove_downloads_now(sender),
            PrefInput::ChangeKeepDownloads(idx) => self.on_change_keep_downloads(idx, sender),
            PrefInput::NautilusRemove => self.on_nautilus_remove(root),
            PrefInput::ChangeFilesAction(idx) => self.on_change_files_action(idx, sender),
            PrefInput::ChangeFilesLarge(idx) => self.on_change_files_large(idx, sender),
            PrefInput::ChangeLinkBrowser(idx) => self.on_change_link_browser(idx, sender),
            PrefInput::ChangeFilesLimit(mb) => self.on_change_files_limit(mb, sender),
            PrefInput::SetFilesPrefs(p) => self.on_set_files_prefs(p),
            PrefInput::SetSpellcheckLangs(langs) => self.on_set_spellcheck_langs(langs),
            PrefInput::NautilusRestartFiles => self.on_nautilus_restart_files(root),
            PrefInput::NautilusRefresh => {
                self.nautilus = crate::desktop::nautilus_ext::State::read();
            }
            PrefInput::ToggleAvatars(on) => self.on_toggle_avatars(on, sender),
            PrefInput::ToggleThreading(on) => self.on_toggle_threading(on, sender),
            PrefInput::ToggleThreadExpansion(on) => self.on_toggle_thread_expansion(on, sender),
            PrefInput::ChangeReaderDefault(idx) => self.on_change_reader_default(idx, sender),
            PrefInput::ChangeReaderZoom(idx) => self.on_change_reader_zoom(idx, sender),
            PrefInput::ChangeCardActionsMode(index) => {
                self.on_change_card_actions_mode(index, sender)
            }
            PrefInput::ToggleListPalette(on) => self.on_toggle_list_palette(on, sender),
            PrefInput::ToggleSwipeEnabled(on) => self.on_toggle_swipe_enabled(on, sender),
            PrefInput::ChangeComposeDefaultFrom(index) => {
                self.on_change_compose_default_from(index, sender)
            }
            PrefInput::ChangeFetchInterval(index) => self.on_change_fetch_interval(index, sender),
            PrefInput::ToggleNotifications(on) => self.on_toggle_notifications(on, sender),
            PrefInput::ToggleNotificationButton(button, on) => {
                self.on_toggle_notification_button(button, on, sender)
            }
            PrefInput::ChooseSound => self.on_choose_sound(sender, root),
            PrefInput::SoundChosen(path) => self.on_sound_chosen(path),
            PrefInput::PlaySound => self.on_play_sound(),
            PrefInput::ToggleSound(on) => self.on_toggle_sound(on),
            PrefInput::ToggleSoundFocused(on) => self.on_toggle_sound_focused(on),
            PrefInput::ChangeSound(index) => self.on_change_sound(index, sender),
            PrefInput::ToggleShowUnified(on) => self.on_toggle_show_unified(on, sender),
            PrefInput::TogglePrintPageNumbers(on) => {
                self.set_print_option(|s| &mut s.page_numbers, on, sender)
            }
            PrefInput::TogglePrintDate(on) => self.set_print_option(|s| &mut s.date, on, sender),
            PrefInput::TogglePrintPunchMargin(on) => {
                self.set_print_option(|s| &mut s.punch_margin, on, sender)
            }
            PrefInput::ToggleUnifiedChipAllInboxes(on) => {
                self.set_unified_chip(|s| &mut s.all_inboxes, on, sender)
            }
            PrefInput::ToggleUnifiedChipStarred(on) => {
                self.set_unified_chip(|s| &mut s.starred, on, sender)
            }
            PrefInput::ToggleUnifiedChipDrafts(on) => {
                self.set_unified_chip(|s| &mut s.drafts, on, sender)
            }
            PrefInput::ToggleUnifiedChipArchive(on) => {
                self.set_unified_chip(|s| &mut s.archive, on, sender)
            }
            PrefInput::ToggleUnifiedChipJunk(on) => {
                self.set_unified_chip(|s| &mut s.junk, on, sender)
            }
            PrefInput::ToggleUnifiedChipTrash(on) => {
                self.set_unified_chip(|s| &mut s.trash, on, sender)
            }
            PrefInput::ToggleUnifiedChipSnoozed(on) => {
                self.set_unified_chip(|s| &mut s.snoozed, on, sender)
            }
            PrefInput::ToggleUnifiedStarred(on) => {
                self.set_unified_kind(|s| &mut s.starred, on, sender)
            }
            PrefInput::ToggleUnifiedSent(on) => self.set_unified_kind(|s| &mut s.sent, on, sender),
            PrefInput::ToggleUnifiedDrafts(on) => {
                self.set_unified_kind(|s| &mut s.drafts, on, sender)
            }
            PrefInput::ToggleUnifiedArchive(on) => {
                self.set_unified_kind(|s| &mut s.archive, on, sender)
            }
            PrefInput::ToggleUnifiedJunk(on) => self.set_unified_kind(|s| &mut s.junk, on, sender),
            PrefInput::ToggleUnifiedTrash(on) => {
                self.set_unified_kind(|s| &mut s.trash, on, sender)
            }
            PrefInput::ToggleUnifiedSnoozed(on) => {
                self.set_unified_kind(|s| &mut s.snoozed, on, sender)
            }
            PrefInput::MountPages => self.mount_pages(),
            PrefInput::Search(text) => self.on_search(text),
            PrefInput::SearchPick(i) => self.on_search_pick(i, sender),
            PrefInput::ToggleSearch => self.on_toggle_search(),
            PrefInput::SetAccountsPanel {
                panel,
                sender: accounts,
            } => self.on_set_accounts_panel(panel, accounts),
            PrefInput::ToggleShowAccounts(on) => self.on_toggle_show_accounts(on, sender),
            PrefInput::ToggleFocus(part, on) => self.on_toggle_focus(part, on, sender),
            PrefInput::SetFocusMode(focus) => {
                self.focus = focus;
            }
            PrefInput::SetShowAccounts(on) => {
                self.show_accounts = on;
            }
            PrefInput::ChangeSectionStyle(idx) => self.on_change_section_style(idx, sender),
            PrefInput::ChangeFilteredPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetFilteredPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeTagsPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetTagsPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeFavoritesPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetFavoritesPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeContactsPlacement(idx) => {
                self.on_change_contacts_placement(idx, sender)
            }
            PrefInput::ChangeStartView(idx) => self.on_change_start_view(idx, sender),
            PrefInput::ChangeFolderSort(idx) => self.on_change_folder_sort(idx, sender),
            PrefInput::ChangeChevronSide(idx) => {
                let _ = sender.output(PrefOutput::SetChevronsLeft(idx == 0));
            }
            PrefInput::ToolbarDrop { key, side, index } => {
                self.on_toolbar_drop(key, side, index, sender)
            }
            PrefInput::ToolbarGapPreview { zone, index } => {
                self.on_toolbar_gap_preview(zone, index)
            }
            PrefInput::ColumnDrop { key, shown, index } => {
                self.on_column_drop(key, shown, index, sender)
            }
            PrefInput::ColumnRestore => self.on_column_restore(sender),
            PrefInput::ToolbarRestore => self.on_toolbar_restore(sender),
            PrefInput::ToggleRailFoldEnabled(on) => {
                self.set_rail_fold(|s| &mut s.enabled, on, sender)
            }
            PrefInput::ToggleRailFoldAccounts(on) => {
                self.set_rail_fold(|s| &mut s.accounts, on, sender)
            }
            PrefInput::ToggleRailFoldAllInboxes(on) => {
                self.set_rail_fold(|s| &mut s.all_inboxes, on, sender)
            }
            PrefInput::ToggleRailFoldStarred(on) => {
                self.set_rail_fold(|s| &mut s.starred, on, sender)
            }
            PrefInput::ToggleRailFoldSent(on) => self.set_rail_fold(|s| &mut s.sent, on, sender),
            PrefInput::ToggleRailFoldDrafts(on) => {
                self.set_rail_fold(|s| &mut s.drafts, on, sender)
            }
            PrefInput::ToggleRailFoldArchive(on) => {
                self.set_rail_fold(|s| &mut s.archive, on, sender)
            }
            PrefInput::ToggleRailFoldJunk(on) => self.set_rail_fold(|s| &mut s.junk, on, sender),
            PrefInput::ToggleRailFoldTrash(on) => self.set_rail_fold(|s| &mut s.trash, on, sender),
            PrefInput::ToggleRailFoldSnoozed(on) => {
                self.set_rail_fold(|s| &mut s.snoozed, on, sender)
            }
            PrefInput::ToggleRailFoldFiltered(on) => {
                self.set_rail_fold(|s| &mut s.filtered, on, sender)
            }
            PrefInput::ToggleRailFoldTags(on) => self.set_rail_fold(|s| &mut s.tags, on, sender),
            PrefInput::ChangeReadMark(idx) => self.on_change_read_mark(idx, sender),
            PrefInput::ExportLog => {
                let _ = sender.output(PrefOutput::ExportLog);
            }
            PrefInput::SetContactBooks(books) => self.on_set_contact_books(books),
            PrefInput::SetLocalContactCount(n) => self.on_set_local_contact_count(n),
            PrefInput::ExportSettings => {
                let _ = sender.output(PrefOutput::ExportSettings);
            }
            PrefInput::ImportSettings => {
                let _ = sender.output(PrefOutput::ImportSettings);
            }
            PrefInput::ChangeTrayIcon(index) => self.on_change_tray_icon(index, sender),
            PrefInput::ChangeLauncherBadge(index) => self.on_change_launcher_badge(index, sender),
            PrefInput::ChangeListLayout(index) => self.on_change_list_layout(index, sender),
            PrefInput::ChangePreviewLines(index) => self.on_change_preview_lines(index, sender),
            PrefInput::ChangeAppTheme(index) => self.on_change_app_theme(index, sender),
            PrefInput::ChangeTextScale(index) => self.on_change_text_scale(index, sender),
            PrefInput::ChangeSettingsOpen(index) => {
                let _ = sender.output(PrefOutput::SetSettingsOpenAccounts(index == 1));
            }
            PrefInput::ShowAccounts(accounts) => {
                self.select_row(if accounts { "accounts" } else { "general" });
            }
            PrefInput::SelectPage(id) => self.on_select_page(id, sender),
            PrefInput::ShowPageById(id) => self.select_row(&id),
            PrefInput::RevealColumns => self.on_reveal_columns(),
            PrefInput::LeaveEditorPrompt(id) => self.ask_to_leave_editor(&id, &sender),
            PrefInput::LeaveEditorTo(id) => self.on_leave_editor_to(id, sender),
            PrefInput::EditorOpen(page) => self.on_editor_open(page),
            PrefInput::CloudEditorOpen(open) => self.on_cloud_editor_open(open),
            PrefInput::Contacts(out) => self.on_contacts(out, sender),
            PrefInput::ChangeMessageTheme(index) => self.on_change_message_theme(index, sender),
            PrefInput::ChangeComposeFormat(idx) => self.on_change_compose_format(idx, sender),
            PrefInput::ChangeFoldMessages(idx) => self.on_change_fold_messages(idx, sender),
            PrefInput::ChangeReplyPosition(idx) => self.on_change_reply_position(idx, sender),
            PrefInput::ChangeUndoSend(idx) => self.on_change_undo_send(idx, sender),
            PrefInput::ChangeSignaturePosition(idx) => {
                self.on_change_signature_position(idx, sender)
            }
        }
    }
}

thread_local! {
    /// Whether Settings → Sidebar's Unread counts and Fold up expanded
    /// items rows are open, for this run of the app only.
    static SESSION_OPEN: std::cell::Cell<[bool; 2]> = const { std::cell::Cell::new([false; 2]) };
}
