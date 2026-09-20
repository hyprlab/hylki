//! Settings window: privacy options (remote-content allowlist).
//!
//! Account credentials are managed in their own window (see `ui/accounts.rs`).

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{AppTheme, ClockStyle, DateStyle, MessageTheme, ReaderToolbar, ToolbarItem, ToolbarSide, TrayIcon};
use crate::ui::chip_flow::ChipFlow;
use std::rc::Rc;
use crate::i18n::{i18n, i18n_f, i18n_noop};

/// Initial data for the settings window.
#[derive(Debug)]
pub struct PrefInit {
    pub auto_remote_content: bool,
    pub show_remote_banner: bool,
    pub gravatar: bool,
    pub avatars: bool,
    /// Your own mail wears its mailbox's face, not a sender's circle (#189).
    pub own_mailbox_face: bool,
    pub sender_logos: bool,
    pub date_style: DateStyle,
    pub clock_style: ClockStyle,
    /// The chosen interface language code; empty = the system's (#179).
    pub language: String,
    pub fetch_interval_secs: u64,
    pub push: bool,
    pub palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open — its own, not
    /// the message list's.
    pub card_palette_collapse_secs: u64,
    pub threading: bool,
    pub threads_expanded: bool,
    /// Reading pane shows conversations newest-message-first.
    pub thread_newest_first: bool,
    /// Reader always shows the recipients line under the sender.
    pub always_show_recipients: bool,
    /// Lone messages render as inset cards, like conversation messages.
    pub single_message_card: bool,
    /// The Reader View switch is shown in the reader header.
    pub reader_switch: bool,
    /// What Reader View does when a message is opened.
    pub reader_default: crate::config::ReaderDefault,
    /// Each conversation message lists its own attachments (#213).
    pub card_attachments: bool,
    /// The attachment drawer beneath the reader is shown (#213).
    pub attachment_drawer: bool,
    /// Conversation rows may expand into their members in the message list.
    pub thread_expansion: bool,
    /// Deleting a whole selected conversation asks for confirmation.
    pub confirm_thread_delete: bool,
    /// Conversation card actions hide until the card is hovered.
    pub card_actions_hover: bool,
    /// With the ⋯ toggle off: card actions appear automatically on hover.
    pub card_actions_auto: bool,
    /// The list rows carry an actions palette line at all.
    pub list_palette: bool,
    /// The list's actions palette opens on row hover (no ⋯ click).
    pub list_palette_hover: bool,
    /// A message card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    pub card_palette_menu: bool,
    /// Message rows take a sideways swipe to archive / delete (#92).
    pub swipe_enabled: bool,
    /// The message list's swipe-gesture sides are swapped (#swipe).
    pub swipe_reversed: bool,
    /// How far a trackpad two-finger swipe has to travel to fire the action.
    pub swipe_sensitivity: f64,
    /// "New message" composes inline over the reading pane (vs a window).
    pub compose_inline: bool,
    pub reply_fields: bool,
    /// Settings → System → GNOME Files: what handed-in files open into.
    pub files: crate::config::FilesPrefs,
    /// Settings → System → Links: the browser links open in (#232). Empty =
    /// the desktop's default, `ask` = its app chooser, otherwise a desktop
    /// entry id.
    pub link_browser: String,
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account. One of `identities`' addresses.
    pub compose_default_from: String,
    pub paste_plain: bool,
    pub spellcheck: bool,
    pub spellcheck_langs: String,
    pub message_theme: MessageTheme,
    /// Set every message in the reader's own font (#56).
    pub override_fonts: bool,
    /// That font, as a Pango description; empty = the interface font.
    pub reader_font: String,
    /// Ignore the senders' text and background colours (#56).
    pub override_colors: bool,
    /// Plain-text messages in monospace (#181), and the font ("" = the
    /// desktop's monospace font).
    pub plain_monospace: bool,
    pub plain_font: String,
    /// New messages start as plain text (#180).
    pub compose_format: crate::config::ComposeFormat,
    /// Where the split reply opens in the reading pane (#212).
    pub reply_position: crate::config::ReplyPosition,
    pub app_theme: AppTheme,
    /// The appearance theme's id ("system" for the stock GNOME colours).
    pub theme: String,
    pub notifications: bool,
    pub notification_content: bool,
    pub show_attachments: bool,
    pub show_contacts: bool,
    pub show_unified: bool,
    pub unified_chips: crate::config::UnifiedChips,
    pub unified_filtered: bool,
    pub unified_kinds: crate::config::UnifiedKinds,
    pub unified_tags: bool,
    pub show_accounts: bool,
    pub filtered_placement: crate::config::SectionPlacement,
    pub tags_placement: crate::config::SectionPlacement,
    pub chevrons_left: bool,
    pub console_mode: bool,
    pub read_mark: crate::config::ReadMark,
    pub sidebar_hover_expand: bool,
    pub remember_sidebar: bool,
    pub remember_rail: bool,
    pub rail_dots: bool,
    pub rail_fold: crate::config::RailFold,
    /// The reader header's buttons, per side and in order.
    pub reader_toolbar: ReaderToolbar,
    /// Focus Mode: the master switch and the parts it strips.
    pub focus: crate::config::FocusMode,
    pub preview_lines: u32,
    pub single_key_shortcuts: bool,
    pub run_in_background: bool,
    pub autostart: bool,
    pub tray: bool,
    pub tray_icon: TrayIcon,
    pub tray_mail: bool,
    /// The chosen app icon (an `app_icon::catalog` id).
    pub app_icon: String,
    /// The accounts panel (built by the AccountsWindow component), shown
    /// behind the window's "Accounts" tab.
    pub accounts_panel: gtk::Widget,
    /// Open showing the Accounts tab instead of Preferences.
    pub start_on_accounts: bool,
    /// The category to open on, when the app remembers one from earlier
    /// this session; overrides `start_on_accounts`.
    pub start_page: Option<String>,
    /// The persisted "this window opens to" choice (true = Accounts).
    pub settings_open_accounts: bool,
    /// The accounts component's inbox, for the sidebar to pick its pages.
    pub accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// (name, address) per account and alias, for the OpenPGP page's
    /// key generator.
    pub identities: Vec<(String, String)>,
}


/// App-chrome appearance options, in combo order.
const TRAY_ICONS: &[(&str, TrayIcon)] = &[
    (i18n_noop("Hylki icon"), TrayIcon::Hylki),
    (i18n_noop("Envelope, white"), TrayIcon::EnvelopeLight),
    (i18n_noop("Envelope, black"), TrayIcon::EnvelopeDark),
];

const APP_THEMES: &[(&str, AppTheme)] = &[
    (i18n_noop("Follow system"), AppTheme::System),
    (i18n_noop("Light"), AppTheme::Light),
    (i18n_noop("Dark"), AppTheme::Dark),
];

/// Message-content appearance options, in combo order.
const MESSAGE_THEMES: &[(&str, MessageTheme)] = &[
    (i18n_noop("Follow system"), MessageTheme::System),
    (i18n_noop("Light"), MessageTheme::Light),
    (i18n_noop("Dark"), MessageTheme::Dark),
];

/// Date arrangements, in combo order. The examples are what each writes.
const DATE_STYLES: &[(&str, DateStyle)] = &[
    (i18n_noop("Follow system"), DateStyle::System),
    (i18n_noop("Aug 23, 2026"), DateStyle::MonthFirst),
    (i18n_noop("23 Aug 2026"), DateStyle::DayFirst),
    (i18n_noop("2026 Aug 23"), DateStyle::YearFirst),
];

/// Clock options, in combo order.
/// The Language row's choices: (label, locale code). The system's own
/// first, then English (the source language), then every catalogue in
/// po/LINGUAS by its own name (#179).
pub fn language_choices() -> Vec<(String, String)> {
    let mut out = vec![(i18n("System"), String::new()), ("English".to_string(), "en".to_string())];
    for code in include_str!("../../po/LINGUAS").lines() {
        let code = code.trim();
        if code.is_empty() || code.starts_with('#') || code == "en" {
            continue;
        }
        out.push((native_language_name(code).to_string(), code.to_string()));
    }
    out
}

/// A language's name in itself, for the Language row.
fn native_language_name(code: &str) -> &str {
    match code {
        "fr" => "Français",
        "el" => "Ελληνικά",
        "hu" => "Magyar",
        "ru" => "Русский",
        "de" => "Deutsch",
        "es" => "Español",
        "it" => "Italiano",
        "pt" => "Português",
        "pt_PT" => "Português (Portugal)",
        "pt_BR" => "Português (Brasil)",
        "nl" => "Nederlands",
        "pl" => "Polski",
        "cs" => "Čeština",
        "sv" => "Svenska",
        "da" => "Dansk",
        "nb" | "no" => "Norsk",
        "fi" => "Suomi",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "ja" => "日本語",
        "zh_CN" => "简体中文",
        "zh_TW" => "繁體中文",
        "ko" => "한국어",
        other => other,
    }
}

const CLOCK_STYLES: &[(&str, ClockStyle)] = &[
    (i18n_noop("Follow system"), ClockStyle::System),
    (i18n_noop("12-hour (5:40 PM)"), ClockStyle::Twelve),
    (i18n_noop("24-hour (17:40)"), ClockStyle::TwentyFour),
];

/// Selectable mail-check intervals (label, seconds). 0 = manual only.
const FETCH_INTERVALS: &[(&str, u64)] = &[
    (i18n_noop("Manually"), 0),
    (i18n_noop("Every minute"), 60),
    (i18n_noop("Every 5 minutes"), 300),
    (i18n_noop("Every 15 minutes"), 900),
    (i18n_noop("Every 30 minutes"), 1800),
];

// ---- Allowed-sender row -----------------------------------------------------

pub struct SenderRow {
    addr: String,
}

#[derive(Debug)]
pub enum SenderRowOutput {
    Remove(String),
}

#[relm4::factory(pub)]
impl FactoryComponent for SenderRow {
    type Init = String;
    type Input = ();
    type Output = SenderRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.addr,
            add_suffix = &gtk::Button {
                set_icon_name: "co.hyprlab.Hylki-user-trash-symbolic",
                set_valign: gtk::Align::Center,
                set_tooltip_text: Some(i18n("Remove").as_str()),
                add_css_class: "flat",
                connect_clicked[sender, addr = self.addr.clone()] => move |_| {
                    let _ = sender.output(SenderRowOutput::Remove(addr.clone()));
                },
            },
        }
    }

    fn init_model(addr: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { addr }
    }
}

// ---- Preferences window -----------------------------------------------------

pub struct Preferences {
    /// Mirrors the notifications switch, so the "show sender and subject" row
    /// below it can grey out when nothing is being posted at all.
    notifications: bool,
    show_unified: bool,
    /// The unified Starred / Sent / Drafts switches, kept whole so each
    /// toggle can hand the app the full set.
    unified_kinds: crate::config::UnifiedKinds,
    /// The unified rows' unread-chip switches, likewise kept whole.
    unified_chips: crate::config::UnifiedChips,
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
    /// The content pane's page, whose title names the chosen category.
    content_page: Option<adw::NavigationPage>,
    /// The split view, to bring the content forward when collapsed.
    split: Option<adw::NavigationSplitView>,
    /// The accounts component, told which of its pages the sidebar chose.
    accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// The content header bar; hidden while the accounts editor subpage is
    /// open, whose own header takes over.
    host_header: Option<adw::HeaderBar>,
    /// The OpenPGP page (#133), kept alive with the window.
    pgp_keys: Option<Controller<crate::ui::pgp_keys::PgpKeys>>,
    /// (name, address) per enabled account and alias, the rows of the
    /// "Send new messages from" combo after its first (#157).
    identities: Vec<(String, String)>,
    /// The Cloud Storage page (#144), likewise.
    cloud: Option<Controller<crate::ui::cloud_accounts::CloudAccounts>>,
    /// The account editor is up in the accounts slot: leaving it for another
    /// category asks about the unsaved changes first.
    editor_open: bool,
    /// Which side page's editor is up: "accounts" or "cloud".
    editor_page: &'static str,
    /// The GNOME Files extension (#188): installed, loaded, loader present.
    nautilus: crate::nautilus_ext::State,
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
}

/// The reader toolbar editor (Settings → Appearance → Toolbar): one drop zone per
/// side plus a "not shown" pool, each a wrapping row of draggable chips
/// that slide apart under a drag to show where the drop will land.
struct ToolbarEditor {
    zones: Vec<ToolbarZone>,
    /// The size of the chip being dragged (zero when none), written by the
    /// chip's drag source and read by every zone for its gap.
    drag_size: Rc<std::cell::Cell<(i32, i32)>>,
}

struct ToolbarZone {
    side: Option<ToolbarSide>,
    flow: ChipFlow,
    /// Shown while the zone is empty, so there is still something to aim at.
    empty: gtk::Label,
}

impl ToolbarEditor {
    fn build(host: &gtk::Box, sender: &ComponentSender<Preferences>) -> Self {
        let mut zones = Vec::new();
        // One chip's size, learnt from whichever zone has chips, so an
        // empty zone opens a gap of the right size too.
        let chip_size = Rc::new(std::cell::Cell::new((0, 0)));
        let drag_size = Rc::new(std::cell::Cell::new((0, 0)));
        // The zones are rows of one raised card, like the rows of the
        // groups around it, not loose elements on the page.
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        host.append(&list);
        for (side, title, hint) in [
            (Some(ToolbarSide::Left), i18n("Left group"), i18n("Always shown · up to 6")),
            (Some(ToolbarSide::Right), i18n("Right group"), i18n("Folds into ⋯ when narrow · up to 6")),
            (None, i18n("Not shown"), i18n("Drop a button here to hide it")),
        ] {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
            column.set_margin_top(10);
            column.set_margin_bottom(12);
            column.set_margin_start(12);
            column.set_margin_end(12);
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let label = gtk::Label::new(Some(&title));
            label.add_css_class("heading");
            label.set_halign(gtk::Align::Start);
            heading.append(&label);
            let hint = gtk::Label::new(Some(&hint));
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            hint.set_halign(gtk::Align::Start);
            hint.set_valign(gtk::Align::Baseline);
            heading.append(&hint);
            column.append(&heading);

            let flow = ChipFlow::new(chip_size.clone(), drag_size.clone());
            flow.set_halign(gtk::Align::Fill);
            flow.set_valign(gtk::Align::Start);
            flow.add_css_class("toolbar-zone");

            let empty = gtk::Label::new(Some(&i18n("Empty")));
            empty.add_css_class("dim-label");
            empty.set_can_target(false);
            empty.set_halign(gtk::Align::Center);
            empty.set_valign(gtk::Align::Center);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&flow));
            overlay.add_overlay(&empty);
            column.append(&overlay);
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_can_focus(false);
            row.set_child(Some(&column));
            list.append(&row);

            // The zone takes a chip from any zone (itself included). While
            // the drag moves over it, a gap opens at the slot the pointer
            // is nearest and the other chips slide aside; the drop takes
            // that slot. The dragged chip is hidden in its own zone for
            // the duration, so the hole it left closes the same way.
            let drop = gtk::DropTarget::new(gtk::glib::Type::STRING, gtk::gdk::DragAction::MOVE);
            let input = sender.input_sender().clone();
            let fb = flow.clone();
            let empty_label = empty.clone();
            drop.connect_drop(move |_, value, x, y| {
                let Ok(key) = value.get::<String>() else {
                    return false;
                };
                if !zone_has_room(&fb, side) {
                    fb.set_gap(None);
                    return false;
                }
                let index = fb.insertion_index(x, y);
                fb.set_gap(None);
                let _ = input.send(PrefInput::ToolbarDrop { key, side, index });
                true
            });
            let fb = flow.clone();
            drop.connect_enter(move |_, x, y| {
                // A full side takes nothing: no gap, no drop.
                if !zone_has_room(&fb, side) {
                    return gtk::gdk::DragAction::empty();
                }
                fb.add_css_class("drop-active");
                fb.set_gap(Some(fb.insertion_index(x, y)));
                gtk::gdk::DragAction::MOVE
            });
            let fb = flow.clone();
            let el = empty_label.clone();
            drop.connect_motion(move |_, x, y| {
                if !zone_has_room(&fb, side) {
                    return gtk::gdk::DragAction::empty();
                }
                el.set_visible(false);
                fb.set_gap(Some(fb.insertion_index(x, y)));
                gtk::gdk::DragAction::MOVE
            });
            let fb = flow.clone();
            drop.connect_leave(move |_| {
                fb.remove_css_class("drop-active");
                fb.set_gap(None);
                // The "Empty" caption comes back with the next rebuild if
                // the zone is still empty (see rebuild_toolbar_chips).
                empty_label.set_visible(fb.first_child().is_none());
            });
            flow.add_controller(drop);

            zones.push(ToolbarZone { side, flow, empty });
        }
        ToolbarEditor { zones, drag_size }
    }

    /// One draggable chip: the button's icon over its name.
    fn chip(item: ToolbarItem, drag_size: &Rc<std::cell::Cell<(i32, i32)>>) -> gtk::Box {
        let chip = gtk::Box::new(gtk::Orientation::Vertical, 4);
        chip.add_css_class("toolbar-chip");
        chip.set_widget_name(item.key());
        chip.set_tooltip_text(Some(&i18n("Drag to move")));
        let icon = gtk::Image::from_icon_name(item.icon());
        icon.set_pixel_size(16);
        chip.append(&icon);
        let label = gtk::Label::new(Some(&i18n(item.label())));
        label.add_css_class("caption");
        chip.append(&label);

        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        let key = item.key();
        // The chip's likeness is taken while it is still on screen: it is
        // hidden once the drag is under way, and a hidden widget paints
        // nothing.
        let likeness: Rc<std::cell::RefCell<Option<gtk::gdk::Paintable>>> = Rc::new(std::cell::RefCell::new(None));
        let c = chip.clone();
        let l = likeness.clone();
        let ds = drag_size.clone();
        drag.connect_prepare(move |_, _, _| {
            *l.borrow_mut() = Some(gtk::WidgetPaintable::new(Some(&c)).current_image());
            // The zones open their gap at this chip's own size.
            ds.set((c.width(), c.height()));
            Some(gtk::gdk::ContentProvider::for_value(&key.to_value()))
        });
        let c = chip.clone();
        let l = likeness.clone();
        drag.connect_drag_begin(move |source, _| {
            if let Some(p) = l.borrow().as_ref() {
                source.set_icon(Some(p), c.width() / 2, c.height() / 2);
            }
            // Lifted out of its row; the neighbours close the hole.
            c.set_visible(false);
        });
        let c = chip.clone();
        let ds = drag_size.clone();
        drag.connect_drag_end(move |_, _, _| {
            ds.set((0, 0));
            // Dropped nowhere (or somewhere that rebuilt the zones, in
            // which case this widget is already gone): back in place.
            c.set_visible(true);
        });
        chip.add_controller(drag);
        chip
    }
}

/// Set the content width of a preferences page: the clamp it lays its
/// groups out in is not exposed, so it is found among the descendants.
fn widen_page(page: &gtk::Widget, max: i32) {
    fn walk(w: &gtk::Widget, max: i32) -> bool {
        if let Some(clamp) = w.downcast_ref::<adw::Clamp>() {
            clamp.set_maximum_size(max);
            return true;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if walk(&c, max) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    if !walk(page, max) {
        tracing::warn!("preferences: no clamp found on the page to widen");
    }
}

/// Whether a zone can take the chip being dragged: the hidden zone always,
/// a side while fewer than `TOOLBAR_SIDE_MAX` chips are on it. The chip
/// being dragged is hidden in its own zone, so it is not counted there and
/// moving within a full side still works.
fn zone_has_room(flow: &ChipFlow, side: Option<ToolbarSide>) -> bool {
    if side.is_none() {
        return true;
    }
    let mut shown = 0;
    let mut child = flow.first_child();
    while let Some(c) = child {
        if c.is_visible() {
            shown += 1;
        }
        child = c.next_sibling();
    }
    shown < crate::config::TOOLBAR_SIDE_MAX
}

impl Preferences {
    /// Refill the editor's zones from the layout being edited.
    fn rebuild_toolbar_chips(&self) {
        let Some(editor) = &self.toolbar_editor else {
            return;
        };
        for zone in &editor.zones {
            zone.flow.remove_all();
            let items: Vec<ToolbarItem> = match zone.side {
                Some(ToolbarSide::Left) => self.toolbar.left.clone(),
                Some(ToolbarSide::Right) => self.toolbar.right.clone(),
                None => self.toolbar.hidden(),
            };
            zone.empty.set_visible(items.is_empty());
            for item in items {
                zone.flow.append(&ToolbarEditor::chip(item, &editor.drag_size));
            }
        }
    }
}

/// One sidebar entry (#141): the stack child it shows, and whether that
/// child lives in the accounts component (whose own stack then switches).
struct SidePage {
    id: &'static str,
    title: &'static str,
    icon: &'static str,
    accounts: bool,
}

/// The sidebar, section by section.
const SIDE_PAGES: &[(&str, &[SidePage])] = &[
    (
        i18n_noop("Accounts"),
        &[
            SidePage { id: "accounts", title: i18n_noop("Mail Accounts"), icon: "co.hyprlab.Hylki-avatar-default-symbolic", accounts: true },
            SidePage { id: "tags", title: i18n_noop("Tags"), icon: "co.hyprlab.Hylki-tag-outline-symbolic", accounts: true },
            SidePage { id: "filters", title: i18n_noop("Filters"), icon: "co.hyprlab.Hylki-filter-folder-symbolic", accounts: true },
            SidePage { id: "senders", title: i18n_noop("Senders"), icon: "co.hyprlab.Hylki-contact-new-symbolic", accounts: true },
            SidePage { id: "openpgp", title: i18n_noop("OpenPGP"), icon: "co.hyprlab.Hylki-channel-secure-symbolic", accounts: false },
            SidePage { id: "cloud", title: i18n_noop("Cloud Storage"), icon: "co.hyprlab.Hylki-cloud-symbolic", accounts: false },
        ],
    ),
    (
        i18n_noop("Settings"),
        &[
            SidePage { id: "general", title: i18n_noop("General"), icon: "co.hyprlab.Hylki-puzzle-piece-symbolic", accounts: false },
            SidePage { id: "appearance", title: i18n_noop("Appearance"), icon: "co.hyprlab.Hylki-preferences-desktop-appearance-symbolic", accounts: false },
            SidePage { id: "sidebar", title: i18n_noop("Sidebar"), icon: "co.hyprlab.Hylki-sidebar-show-symbolic", accounts: false },
            SidePage { id: "list", title: i18n_noop("Message List"), icon: "co.hyprlab.Hylki-view-list-bullet-symbolic", accounts: false },
            SidePage { id: "conversations", title: i18n_noop("Conversations"), icon: "co.hyprlab.Hylki-chat-bubbles-text-symbolic", accounts: false },
            SidePage { id: "reading", title: i18n_noop("Reading"), icon: "co.hyprlab.Hylki-mail-read-symbolic", accounts: false },
            SidePage { id: "composing", title: i18n_noop("Composing"), icon: "co.hyprlab.Hylki-document-edit-symbolic", accounts: false },
            SidePage { id: "privacy", title: i18n_noop("Privacy"), icon: "co.hyprlab.Hylki-security-high-symbolic", accounts: false },
            SidePage { id: "datetime", title: i18n_noop("Date and Time"), icon: "co.hyprlab.Hylki-x-office-calendar-symbolic", accounts: false },
            SidePage { id: "system", title: i18n_noop("System"), icon: "co.hyprlab.Hylki-applications-system-symbolic", accounts: false },
            SidePage { id: "backup", title: i18n_noop("Backup"), icon: "co.hyprlab.Hylki-document-save-symbolic", accounts: false },
        ],
    ),
];

/// The Files extension row's subtitle: where it stands, and where it lives.
/// The GNOME Files combo rows' positions for each preference value.
fn files_action_index(a: crate::config::FilesAction) -> u32 {
    use crate::config::FilesAction as A;
    match a {
        A::Ask => 0,
        A::New => 1,
        A::Draft => 2,
        A::Reply => 3,
    }
}

/// The links combo's position for a stored choice: 0 the desktop's default,
/// 1 the chooser, otherwise the browser's place in the list. A browser that
/// is no longer installed (or cannot be seen from inside the sandbox) reads
/// as the default, which is what its links will do.
fn link_browser_index(browsers: &[crate::ui::launch::Browser], choice: &str) -> u32 {
    if choice.is_empty() {
        return 0;
    }
    if choice == crate::ui::launch::ASK {
        return 1;
    }
    browsers.iter().position(|b| b.id == choice).map(|i| i as u32 + 2).unwrap_or(0)
}

/// What the Links group says above the combo — which depends on whether
/// there are any browsers to name.
fn link_group_description(browsers: &[crate::ui::launch::Browser]) -> String {
    if browsers.is_empty() {
        i18n(
            "Which browser a link in a message opens in. Inside its Flatpak sandbox Hylki \
             cannot see the applications installed on the system, so links leave through \
             the desktop portal: choose \"Ask each time\" to pick the browser as each link \
             is opened — the desktop's chooser can remember the choice.",
        )
    } else {
        i18n(
            "Which browser a link in a message opens in. \"System default\" follows the \
             desktop's own choice; \"Ask each time\" lets the desktop ask before each one.",
        )
    }
}

fn files_large_index(l: crate::config::FilesLarge) -> u32 {
    use crate::config::FilesLarge as L;
    match l {
        L::Ask => 0,
        L::Attach => 1,
        L::Cloud => 2,
    }
}

fn nautilus_status_text(state: &crate::nautilus_ext::State) -> String {
    use crate::nautilus_ext::Status;
    let state_text = match state.status {
        Status::NotInstalled => i18n("Not installed"),
        Status::Installed if state.loaded => i18n("Installed and loaded by Files"),
        Status::Installed if state.loader == Some(false) => {
            i18n("Installed, but the nautilus-python package is missing, so Files cannot load it")
        }
        Status::Installed => i18n(
            "Installed. Files has not loaded it yet: restart Files, and make sure the \
             nautilus-python package is installed",
        ),
        Status::Outdated => i18n("Installed, but not this version's copy"),
    };
    let state = state_text;
    match crate::nautilus_ext::path() {
        Some(p) => {
            let shown = p.display().to_string();
            let shown = match std::env::var("HOME") {
                Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
                    format!("~{}", &shown[home.len()..])
                }
                _ => shown,
            };
            format!("{state} — {shown}")
        }
        None => state,
    }
}

/// Hide the "editable" pencil an `adw::EntryRow` draws whatever its
/// `editable` says (the icon carries the `edit-icon` style class).
fn hide_edit_icon(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(c) = child {
        if c.has_css_class("edit-icon") {
            c.set_visible(false);
        } else {
            hide_edit_icon(&c);
        }
        child = c.next_sibling();
    }
}

/// A plain error alert over the settings window.
fn report(parent: &adw::Window, heading: &str, body: &str) {
    let dialog = adw::MessageDialog::new(Some(parent), Some(heading), Some(body));
    dialog.add_response("ok", &i18n("OK"));
    dialog.set_close_response("ok");
    dialog.present();
}

fn side_page(id: &str) -> Option<&'static SidePage> {
    SIDE_PAGES.iter().flat_map(|(_, pages)| pages.iter()).find(|p| p.id == id)
}

#[derive(Debug)]
pub enum PrefInput {
    ToggleShowRemoteBanner(bool),
    ToggleAutoRemoteContent(bool),
    ToggleGravatar(bool),
    ToggleAvatars(bool),
    ToggleOwnMailboxFace(bool),
    ToggleSenderLogos(bool),
    ChangeDateStyle(u32),
    ChangeClockStyle(u32),
    ChangeLanguage(u32),
    /// The GNOME Files extension (#188): install this build's copy, remove
    /// the installed one, ask Files to quit so it reloads.
    NautilusInstall,
    NautilusRemove,
    NautilusRestartFiles,
    /// The GNOME Files hand-off rows: destination, over-the-limit action,
    /// and the limit in MB.
    ChangeFilesAction(u32),
    ChangeFilesLarge(u32),
    ChangeFilesLimit(u32),
    /// Settings → System → Links: the browser row moved (#232).
    ChangeLinkBrowser(u32),
    /// The app changed the Files preferences (a dialog's "always do this").
    SetFilesPrefs(crate::config::FilesPrefs),
    /// Re-read the extension's state (the System page came into view; Files
    /// may have loaded the extension since).
    NautilusRefresh,
    ToggleThreading(bool),
    ToggleThreadsExpanded(bool),
    ToggleThreadNewestFirst(bool),
    ToggleAlwaysShowRecipients(bool),
    ToggleSingleMessageCard(bool),
    ToggleReaderSwitch(bool),
    ChangeReaderDefault(u32),
    ToggleCardAttachments(bool),
    ToggleAttachmentDrawer(bool),
    ToggleThreadExpansion(bool),
    ToggleConfirmThreadDelete(bool),
    ChangeCardActionsMode(u32),
    ToggleListPalette(bool),
    ToggleListPaletteHover(bool),
    ToggleCardPaletteMenu(bool),
    ToggleSwipeEnabled(bool),
    ToggleSwipeReversed(bool),
    ChangeSwipeSensitivity(f64),
    ToggleComposeInline(bool),
    ToggleReplyFields(bool),
    /// The "Send new messages from" combo: 0 = the open folder's account,
    /// then `identities` in order.
    ChangeComposeDefaultFrom(u32),
    TogglePastePlain(bool),
    ToggleSpellcheck(bool),
    SpellLangsEdited(String),
    ChangeFetchInterval(u32),
    TogglePush(bool),
    ToggleNotifications(bool),
    ToggleNotificationContent(bool),
    ToggleAttachmentsRow(bool),
    ToggleContactsRow(bool),
    ToggleShowUnified(bool),
    ToggleUnifiedChipAllInboxes(bool),
    ToggleUnifiedChipStarred(bool),
    ToggleUnifiedChipDrafts(bool),
    ToggleUnifiedChipArchive(bool),
    ToggleUnifiedChipFiltered(bool),
    ToggleUnifiedFiltered(bool),
    ToggleUnifiedStarred(bool),
    ToggleUnifiedSent(bool),
    ToggleUnifiedDrafts(bool),
    ToggleUnifiedArchive(bool),
    ToggleUnifiedTags(bool),
    ToggleShowAccounts(bool),
    /// One of Focus Mode's switches.
    ToggleFocus(crate::config::FocusPart, bool),
    /// The app's Focus Mode changed elsewhere (menu, shortcut): follow.
    SetFocusMode(crate::config::FocusMode),
    /// The main menu flipped "Show Accounts": the switch follows.
    SetShowAccounts(bool),
    ChangeChevronSide(u32),
    ChangeFilteredPlacement(u32),
    ChangeTagsPlacement(u32),
    ToggleSidebarHoverExpand(bool),
    ToggleRememberSidebar(bool),
    ToggleRememberRail(bool),
    ToggleRailDots(bool),
    /// A reader toolbar chip was dropped: `key` names the button, `side`
    /// the zone (None = not shown), `index` its place in that zone.
    ToolbarDrop { key: String, side: Option<ToolbarSide>, index: usize },
    ToolbarRestore,
    /// The showcase's stand-in for a drag hovering a zone: open the gap at
    /// `index` of the zone (0 left, 1 right, 2 not shown).
    ToolbarGapPreview { zone: usize, index: usize },
    ToggleRailFoldEnabled(bool),
    ToggleRailFoldAccounts(bool),
    ToggleRailFoldAllInboxes(bool),
    ToggleRailFoldStarred(bool),
    ToggleRailFoldSent(bool),
    ToggleRailFoldDrafts(bool),
    ToggleRailFoldArchive(bool),
    ToggleRailFoldFiltered(bool),
    ToggleRailFoldTags(bool),
    ChangePreviewLines(u32),
    ToggleSingleKey(bool),
    ToggleConsoleMode(bool),
    ChangeReadMark(u32),
    ExportSettings,
    ExportLog,
    ImportSettings,
    ToggleRunInBackground(bool),
    ToggleAutostart(bool),
    ToggleTray(bool),
    ChangeTrayIcon(u32),
    ToggleTrayMail(bool),
    ChangeAppIcon(String),
    ChangePaletteCollapse(u64),
    ChangeCardPaletteCollapse(u64),
    ChangeMessageTheme(u32),
    ToggleOverrideFonts(bool),
    ChangeReaderFont(String),
    ToggleOverrideColors(bool),
    TogglePlainMonospace(bool),
    ChangePlainFont(String),
    ChangeComposeFormat(u32),
    ChangeReplyPosition(u32),
    ChangeAppTheme(u32),
    ChangeTheme(String),
    ChangeSettingsOpen(u32),
    /// Switch the window to the Accounts panel (true) or Preferences (false).
    ShowAccounts(bool),
    /// Put the deferred pages back into the stack (scheduled after the
    /// first paint).
    MountPages,
    /// A fresh accounts panel for this open (the window is kept between
    /// opens; the panel is rebuilt over the current accounts).
    SetAccountsPanel { panel: gtk::Widget, sender: relm4::Sender<crate::ui::accounts::AccountsInput> },
    /// A sidebar category was chosen (#141).
    SelectPage(String),
    /// The open editor says it holds unsaved changes: ask before leaving it
    /// for this page.
    LeaveEditorPrompt(String),
    /// Leave the editor for this page — it is saved, discarded, or was never
    /// touched.
    LeaveEditorTo(String),
    /// Select a category by id from outside (the app's showcase hook).
    ShowPageById(String),
    /// An accounts-panel editor subpage (account, filter or tag) opened on
    /// the named settings page, or closed — hide/show the shared header so
    /// the editor's own header takes over the window.
    EditorOpen(Option<&'static str>),
    /// The Cloud Storage page's account editor is up (or gone).
    CloudEditorOpen(bool),
}

#[derive(Debug)]
pub enum PrefOutput {
    /// A category was shown, so the app can reopen the window on it.
    PageShown(String),
    SetAutoRemoteContent(bool),
    SetShowRemoteBanner(bool),
    SetGravatar(bool),
    SetAvatars(bool),
    SetOwnMailboxFace(bool),
    SetSenderLogos(bool),
    SetDateStyle(DateStyle),
    SetClockStyle(ClockStyle),
    /// The interface language code chosen, "" for the system's (#179).
    SetLanguage(String),
    SetThreading(bool),
    SetThreadsExpanded(bool),
    SetThreadNewestFirst(bool),
    SetAlwaysShowRecipients(bool),
    SetSingleMessageCard(bool),
    SetReaderSwitch(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    SetCardAttachments(bool),
    SetAttachmentDrawer(bool),
    SetThreadExpansion(bool),
    SetConfirmThreadDelete(bool),
    SetCardActionsMode { hover_toggle: bool, hover_auto: bool },
    SetListPalette(bool),
    SetListPaletteHover(bool),
    SetCardPaletteMenu(bool),
    SetSwipeEnabled(bool),
    SetSwipeReversed(bool),
    SetSwipeSensitivity(f64),
    SetComposeInline(bool),
    SetReplyFields(bool),
    SetFilesPrefs(crate::config::FilesPrefs),
    /// The browser links open in (#232): "" = the desktop's default,
    /// "ask" = its app chooser, otherwise a desktop entry id.
    SetLinkBrowser(String),
    SetComposeDefaultFrom(String),
    SetPastePlain(bool),
    SetSpellcheck(bool),
    SetSpellcheckLangs(String),
    SetFetchInterval(u64),
    SetPush(bool),
    SetNotifications(bool),
    SetNotificationContent(bool),
    SetAttachmentsRow(bool),
    SetContactsRow(bool),
    SetShowUnified(bool),
    SetUnifiedChips(crate::config::UnifiedChips),
    SetUnifiedFiltered(bool),
    SetUnifiedKinds(crate::config::UnifiedKinds),
    SetUnifiedTags(bool),
    SetShowAccounts(bool),
    SetChevronsLeft(bool),
    SetFilteredPlacement(crate::config::SectionPlacement),
    SetTagsPlacement(crate::config::SectionPlacement),
    SetConsoleMode(bool),
    SetReadMark(crate::config::ReadMark),
    ExportSettings,
    ExportLog,
    ImportSettings,
    SetSidebarHoverExpand(bool),
    SetRememberSidebar(bool),
    SetRememberRail(bool),
    SetRailDots(bool),
    SetReaderToolbar(ReaderToolbar),
    SetFocusMode(crate::config::FocusMode),
    SetRailFold(crate::config::RailFold),
    SetAppTheme(AppTheme),
    /// A theme was picked in the gallery (its id, or "system").
    SetTheme(String),
    /// The "this window opens to" choice changed (true = Accounts).
    SetSettingsOpenAccounts(bool),
    SetPreviewLines(u32),
    SetSingleKey(bool),
    SetRunInBackground(bool),
    SetAutostart(bool),
    SetTray(bool),
    SetTrayIcon(TrayIcon),
    SetTrayMail(bool),
    SetAppIcon(String),
    SetPaletteCollapse(u64),
    SetCardPaletteCollapse(u64),
    SetMessageTheme(MessageTheme),
    SetOverrideFonts(bool),
    SetReaderFont(String),
    SetOverrideColors(bool),
    SetPlainMonospace(bool),
    SetPlainFont(String),
    SetComposeFormat(crate::config::ComposeFormat),
    SetReplyPosition(crate::config::ReplyPosition),
    Closed,
}

impl Preferences {
    /// Select a sidebar row by page id; the selection handler shows it.
    fn select_row(&self, id: &str) {
        let Some(list) = &self.side_list else { return };
        let name = format!("page:{id}");
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if row.widget_name() == name {
                list.select_row(Some(&row));
                return;
            }
            i += 1;
        }
    }

    /// The sidebar moved away from an open account editor: save, discard,
    /// or stay. Staying puts the selection back on Mail Accounts.
    fn ask_to_leave_editor(&self, id: &str, sender: &ComponentSender<Self>) {
        let parent = relm4::main_application().active_window();
        let (heading, body) = match self.editor_page {
            "filters" => (
                i18n("Save the filter?"),
                i18n("The filter editor is open. Save what you changed, or discard it, before moving on."),
            ),
            "tags" => (
                i18n("Save the tag?"),
                i18n("The tag editor is open. Save what you changed, or discard it, before moving on."),
            ),
            _ => (
                i18n("Save the account?"),
                i18n("The account editor is open. Save what you changed, or discard it, before moving on."),
            ),
        };
        let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&heading), Some(&body));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("discard", &i18n("Discard"));
        dialog.add_response("save", &i18n("Save"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let s = sender.clone();
        let accounts = self.accounts_sender.clone();
        let cloud = self.cloud.as_ref().map(|c| c.sender().clone());
        let editor_page = self.editor_page;
        let id = id.to_string();
        dialog.connect_response(None, move |_, resp| {
            let cloud_editor = editor_page == "cloud";
            match resp {
                "save" => {
                    if cloud_editor {
                        if let Some(c) = &cloud {
                            let _ = c.send(crate::ui::cloud_accounts::CloudAccountsInput::SaveClicked);
                        }
                    } else {
                        let _ = accounts.send(crate::ui::accounts::AccountsInput::SaveOpenPage);
                    }
                    s.input(PrefInput::LeaveEditorTo(id.clone()));
                }
                "discard" => {
                    if cloud_editor {
                        if let Some(c) = &cloud {
                            let _ = c.send(crate::ui::cloud_accounts::CloudAccountsInput::CloseEditor);
                        }
                    } else {
                        let _ = accounts.send(crate::ui::accounts::AccountsInput::CloseEditor);
                    }
                    s.input(PrefInput::LeaveEditorTo(id.clone()));
                }
                // Staying: put the selection back on the editor's own page.
                _ => s.input(PrefInput::ShowPageById(editor_page.into())),
            }
        });
        dialog.present();
    }

    /// Return the deferred pages to the stack (see `deferred_pages`).
    fn mount_pages(&self) {
        let pages = std::mem::take(&mut *self.deferred_pages.borrow_mut());
        if let Some(stack) = &self.panels_stack {
            for (name, child) in pages {
                stack.add_named(&child, Some(&name));
            }
        }
    }

    /// Show a category: the accounts component's page, or one of ours.
    fn show_page(&self, id: &str) {
        let Some(page) = side_page(id) else { return };
        self.mount_pages();
        if let Some(stack) = &self.panels_stack {
            if page.accounts {
                stack.set_visible_child_name("accounts");
                let _ = self
                    .accounts_sender
                    .send(crate::ui::accounts::AccountsInput::ShowPage(id.to_string()));
            } else {
                stack.set_visible_child_name(id);
            }
        }
        if let Some(cp) = &self.content_page {
            cp.set_title(&i18n(page.title));
        }
        if let Some(split) = &self.split {
            if split.is_collapsed() {
                split.set_show_content(true);
            }
        }
    }
}

#[relm4::component(pub)]
impl Component for Preferences {
    type Init = PrefInit;
    type Input = PrefInput;
    type Output = PrefOutput;
    type CommandOutput = ();

    view! {
        adw::Window {
            set_modal: false,
            set_default_width: 920,
            // The same size every time: the two-pane layout (#141) fits its
            // sidebar at this height, and nothing is remembered from a resize.
            set_default_height: 810,
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
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            set_show_end_title_buttons: false,
                        },

                        #[wrap(Some)]
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

                            add_named[Some("general")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("General"),

                                    #[name = "fetch_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Check for new mail"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFetchInterval(row.selected()));
                                        },
                                    },

                                    #[name = "push_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Instant new mail (IMAP push)"),
                                        set_subtitle: &i18n("Uses IMAP IDLE to receive messages the moment they arrive."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePush(row.is_active()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Notifications"),

                                    #[name = "notifications_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Desktop notifications"),
                                        set_subtitle: &i18n("Show system notifications for new mail and error alerts when Hylki isn't focused."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotifications(row.is_active()));
                                        },
                                    },

                                    #[name = "notification_content_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.notifications,
                                        set_title: &i18n("Show sender and subject"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationContent(row.is_active()));
                                        },
                                    },
                                },
                            },

                            #[name = "appearance_page"]
                            add_named[Some("appearance")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    // Rendered as Pango markup — a bare "&" breaks it.
                                    set_title: &i18n("Appearance"),

                                    #[name = "app_theme_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Style"),
                                        set_subtitle: &i18n("The app itself. Message content has its own \
                                                       setting under Reading."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeAppTheme(row.selected()));
                                        },
                                    },

                                    // The theme gallery; its content is built
                                    // in init (a grid the view! macro can't
                                    // declare), like the app-icon strip below.
                                    #[name = "theme_row"]
                                    adw::PreferencesRow {
                                        set_title: &i18n("Theme"),
                                        set_activatable: false,
                                        set_focusable: false,
                                    },

                                    // The app-icon gallery; its content is built in init
                                    // (a grid of textures the view! macro can't declare).
                                    #[name = "app_icon_row"]
                                    adw::PreferencesRow {
                                        set_title: &i18n("App icon"),
                                        set_activatable: false,
                                        set_focusable: false,
                                    },

                                    #[name = "settings_open_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("This window opens to"),
                                        set_subtitle: &i18n("The view shown first when Settings \
                                                       is opened from the menu."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSettingsOpen(row.selected()));
                                        },
                                    },

                                },

                                // The reader toolbar's buttons: three drop zones
                                // (left group, right group, not shown) of
                                // draggable chips, filled in init from the saved
                                // layout. Every drop is applied and saved at once.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Toolbar"),
                                    set_description: Some(
                                        &i18n("The buttons above the reading pane. Drag them between the \
                                               groups and into the order you want. The left group always stays on the toolbar; \
                                               the right group folds into a ⋯ menu when the reading \
                                               pane is narrow. Changes apply at once."),
                                    ),
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Button {
                                        set_label: &i18n("Restore Defaults"),
                                        set_valign: gtk::Align::Center,
                                        connect_clicked => PrefInput::ToolbarRestore,
                                    },

                                    #[name = "toolbar_editor_box"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,
                                    },
                                },

                                // Focus Mode: the master switch and its parts.
                                // Every row applies at once, animated.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Focus Mode"),
                                    set_description: Some(
                                        &i18n("A quieter layout for reading. The parts ticked below slide away \
                                               when Focus Mode is on and come back when it is off. Also in the \
                                               main menu, and Ctrl+Shift+F."),
                                    ),

                                    #[name = "focus_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.enabled,
                                        set_title: &i18n("Focus Mode"),
                                        set_subtitle: &i18n("Strip the parts below away for reading."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::Enabled, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_start_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.start_focused,
                                        set_title: &i18n("Start in Focus Mode"),
                                        set_subtitle: &i18n("Every launch opens in the mode. Off, the app opens as normal, \
                                                       whichever way the last session was left."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::StartFocused, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_toolbar_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.reader_toolbar,
                                        set_title: &i18n("Fold the reading pane's toolbar"),
                                        set_subtitle: &i18n("Every button goes into its ⋯ menu."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderToolbar, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_list_header_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.list_header,
                                        set_title: &i18n("Fold the message list's header"),
                                        set_subtitle: &i18n("Search, the unread and starred filters, the count and the sort \
                                                       order go into a ⋯ menu. Only the sidebar toggle stays."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ListHeader, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_accounts_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_accounts,
                                        set_title: &i18n("Hide the accounts in the sidebar"),
                                        set_subtitle: &i18n("Only the unified section stays."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAccounts, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_unified_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.fold_unified,
                                        set_title: &i18n("Fold up the unified rows"),
                                        set_subtitle: &i18n("Inboxes, Starred and the rest show folded. A click still opens \
                                                       one for the while; the full layout comes back as you left it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::FoldUnified, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_avatars_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_avatars,
                                        set_title: &i18n("Hide sender avatars"),
                                        set_subtitle: &i18n("The message list's circles go, and the room they took comes back."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAvatars, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_hide_preview_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_preview,
                                        set_title: &i18n("Hide the preview text"),
                                        set_subtitle: &i18n("Rows show the sender, the subject and the date alone. The preview \
                                                       is only hidden: it is back the moment Focus Mode ends."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HidePreview, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_preview_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.one_preview_line,
                                        // Nothing left to cap once the preview
                                        // text is hidden altogether.
                                        #[watch]
                                        set_sensitive: !model.focus.hide_preview,
                                        set_title: &i18n("One line of preview text"),
                                        set_subtitle: &i18n("Rows show at most one line of a message's text."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::OnePreviewLine, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_subject_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_subject,
                                        set_title: &i18n("Hide the subject"),
                                        set_subtitle: &i18n("Rows show who wrote and when, and nothing else. Tag chips ride \
                                                       on the subject line, so they go with it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideSubject, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_rail_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.rail_sidebar,
                                        set_title: &i18n("Fold the sidebar to the icon rail"),
                                        set_subtitle: &i18n("The sidebar shows as icons, with a dot for unread mail in place \
                                                       of the counts. Hovering it still slides the full sidebar out."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::RailSidebar, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_reader_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.reader_view,
                                        set_title: &i18n("Reader View"),
                                        set_subtitle: &i18n("Every message opens in Reader View. The header's switch still \
                                                       changes the one on screen."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderView, row.is_active()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("sidebar")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Sidebar"),

                                    #[name = "show_accounts_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.show_accounts,
                                        set_title: &i18n("Accounts in the sidebar"),
                                        set_subtitle: &i18n("Each account's own section: its folders, filtered folders and tags. Off leaves the \
                                                       unified section alone. Also in the main menu, and \
                                                       Ctrl+Shift+A."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleShowAccounts(row.is_active()));
                                        },
                                    },

                                    #[name = "chevron_side_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Chevron placement"),
                                        set_subtitle: &i18n("Which side of Inboxes and the account rows \
                                                       their expand/collapse chevrons sit on."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeChevronSide(row.selected()));
                                        },
                                    },

                                    #[name = "show_attachments_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Attachments in the sidebar"),
                                        set_subtitle: &i18n("A shortcut for browsing every account's attachments, \
                                                       pinned at the bottom of the sidebar."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAttachmentsRow(row.is_active()));
                                        },
                                    },

                                    #[name = "show_contacts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Contacts in the sidebar"),
                                        set_subtitle: &i18n("A shortcut that opens your contacts, pinned at the \
                                                       bottom of the sidebar."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleContactsRow(row.is_active()));
                                        },
                                    },

                                    #[name = "sidebar_hover_expand_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Expand the sidebar on hover"),
                                        set_subtitle: &i18n("Whenever the sidebar is collapsed to its icon rail, \
                                                       hovering it floats the full sidebar out over the \
                                                       panes; it stays out until you click outside it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSidebarHoverExpand(row.is_active()));
                                        },
                                    },

                                    #[name = "remember_sidebar_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Remember the sidebar layout"),
                                        set_subtitle: &i18n("Reopen with the accounts, folders and sections as \
                                                       you left them. Off starts every launch with \
                                                       everything folded up."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRememberSidebar(row.is_active()));
                                        },
                                    },

                                    #[name = "remember_rail_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Remember icon rail state"),
                                        set_subtitle: &i18n("Reopen with icon rail in the last used state. Off \
                                                       starts every launch with the full sidebar."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRememberRail(row.is_active()));
                                        },
                                    },
                                },


                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Unified"),
                                    set_description: Some(
                                        &i18n("The section at the top of the sidebar that combines every \
                                               account. Only shown with more than one account."),
                                    ),

                                    #[name = "show_unified_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Inboxes"),
                                        set_subtitle: &i18n("A unified inbox combining every account, at the top \
                                                       of the sidebar. Only shown with more than one \
                                                       account."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleShowUnified(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_starred_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Starred"),
                                        set_subtitle: &i18n("Every account's starred folder as one list, opening to each account's own."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedStarred(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_sent_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Sent"),
                                        set_subtitle: &i18n("Every account's sent mail as one list, opening to each account's own."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedSent(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_drafts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Drafts"),
                                        set_subtitle: &i18n("Every account's drafts as one list, opening to each account's own."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedDrafts(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_archive_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Archive"),
                                        set_subtitle: &i18n("Every account's archive as one list, opening to each account's own."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedArchive(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_filtered_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Filters"),
                                        set_subtitle: &i18n("List every folder your filter rules file into in a \
                                                       collapsible section of the unified view."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedFiltered(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_tags_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Tags"),
                                        set_subtitle: &i18n("The tags, each showing every account's mail with it. Each account keeps its own Tags \
                                                       section either way."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedTags(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_chips_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Unread counts"),
                                        set_subtitle: &i18n("Which unified rows show their combined unread \
                                                       chip while folded up. Expanded, the rows beneath \
                                                       carry the counts. Sent never shows one."),
                                        set_expanded: true,

                                        #[name = "unified_chip_all_inboxes_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Inboxes"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipAllInboxes(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_starred_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Starred"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipStarred(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_drafts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Drafts"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipDrafts(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_archive_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Archive"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipArchive(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_filtered_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Filters"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipFiltered(row.is_active()));
                                            },
                                        },
                                    },

                                    #[name = "filtered_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Filters placement"),
                                        set_subtitle: &i18n("Choose between appearing in a unified section, \
                                                       or a list above or below the accounts."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilteredPlacement(row.selected()));
                                        },
                                    },

                                    #[name = "tags_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Tags placement"),
                                        set_subtitle: &i18n("Choose between appearing in a unified section, \
                                                       or a list above or below the accounts."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeTagsPlacement(row.selected()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Icon rail"),
                                    set_description: Some(
                                        &i18n("The sidebar collapsed to icons, by its button or in a \
                                               narrow window."),
                                    ),

                                    #[name = "rail_dots_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Unread dots instead of counts"),
                                        set_subtitle: &i18n("Mark folders and accounts that have unread mail with \
                                                       a dot in the accent colour rather than the number \
                                                       of messages. The count stays in the tooltip."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRailDots(row.is_active()));
                                        },
                                    },

                                    #[name = "rail_fold_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Fold up expanded items"),
                                        set_subtitle: &i18n("When the sidebar collapses to the icon rail, the items \
                                                       switched on below start folded up. A long-press in the \
                                                       rail still expands or collapses any of them, and the \
                                                       full sidebar comes back exactly as you left it."),
                                        set_show_enable_switch: true,
                                        set_expanded: true,
                                        connect_enable_expansion_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRailFoldEnabled(row.enables_expansion()));
                                        },

                                        #[name = "rail_fold_accounts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("All accounts"),
                                            set_subtitle: &i18n("Every account's folder list."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldAccounts(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_all_inboxes_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Inboxes"),
                                            set_subtitle: &i18n("The account list under the unified Inboxes row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldAllInboxes(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_starred_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Starred"),
                                            set_subtitle: &i18n("The account list under the unified Starred row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldStarred(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_sent_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Sent"),
                                            set_subtitle: &i18n("The account list under the unified Sent row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldSent(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_drafts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Drafts"),
                                            set_subtitle: &i18n("The account list under the unified Drafts row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldDrafts(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_archive_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Archive"),
                                            set_subtitle: &i18n("The account list under the unified Archive row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldArchive(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_filtered_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Filters"),
                                            set_subtitle: &i18n("The folders under the Filters row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldFiltered(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_tags_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Tags"),
                                            set_subtitle: &i18n("The tags under the Tags row."),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldTags(row.is_active()));
                                            },
                                        },
                                    },
                                },
                            },

                            add_named[Some("list")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Message List"),

                                    #[name = "avatars_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Sender avatars"),
                                        set_subtitle: &i18n("The sender's avatar beside each message, in the list \
                                                       and above the message. Turning it off gives the \
                                                       sender and subject more room."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAvatars(row.is_active()));
                                        },
                                    },

                                    // Your own mail (#189): its mailbox's
                                    // face, or the circle any sender gets.
                                    #[name = "own_mailbox_face_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.avatars,
                                        set_title: &i18n("Sent mail uses your account circle"),
                                        set_subtitle: &i18n("Messages you sent wear the account's Gravatar, \
                                                       picture or emoji, as its circle in the sidebar \
                                                       does."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleOwnMailboxFace(row.is_active()));
                                        },
                                    },

                                    #[name = "preview_lines_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Preview lines"),
                                        set_subtitle: &i18n("How much of each message to show under its subject. \
                                                       Off also stops previews being downloaded."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangePreviewLines(row.selected()));
                                        },
                                    },

                                    #[name = "list_palette_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Actions palette in the message list"),
                                        set_subtitle: &i18n("The \u{22ef} action row under each message summary. \
                                                       Turning it off returns its space to the row; \
                                                       messages are still acted on from their cards and \
                                                       the right-click menu."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleListPalette(row.is_active()));
                                        },
                                    },

                                    #[name = "list_palette_hover_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.list_palette,
                                        set_title: &i18n("Open the actions palette on hover"),
                                        set_subtitle: &i18n("The message list's \u{22ef} actions palette slides \
                                                       open by itself while the pointer rests on a row."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleListPaletteHover(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_enabled_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Swipe actions"),
                                        set_subtitle: &i18n("Drag a message sideways with the mouse, or swipe it \
                                                       with two fingers on a trackpad, to archive or delete \
                                                       it. Default is left to delete, right to archive."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSwipeEnabled(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_reversed_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.swipe_enabled,
                                        set_title: &i18n("Reverse swipe directions"),
                                        set_subtitle: &i18n("Turning this on swaps the swipe actions to: \
                                                       left archives, right deletes."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSwipeReversed(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_sensitivity_row"]
                                    adw::SpinRow {
                                        #[watch]
                                        set_sensitive: model.swipe_enabled,
                                        set_title: &i18n("Trackpad swipe sensitivity"),
                                        set_subtitle: &i18n("How readily a two-finger trackpad swipe moves \
                                                       a message. Raise it if a swipe never gets far \
                                                       enough to archive or delete, lower it if messages \
                                                       slide when you did not mean them to. Mouse drags \
                                                       are unaffected."),
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSwipeSensitivity(row.value()));
                                        },
                                    },

                                    #[name = "palette_collapse_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Actions palette timeout"),
                                        set_subtitle: &i18n("Seconds the message list's actions palette stays \
                                                       open after the cursor leaves it."),
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangePaletteCollapse(row.value() as u64));
                                        },
                                    },
                                },
                            },

                            add_named[Some("conversations")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Conversations"),

                                    #[name = "threading_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Group messages by conversation"),
                                        set_subtitle: &i18n("Collapse replies into a single threaded conversation."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreading(row.is_active()));
                                        },
                                    },

                                    #[name = "thread_expansion_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.threading,
                                        set_title: &i18n("Expandable conversations"),
                                        set_subtitle: &i18n("Allow a conversation to expand/collapse its messages \
                                                       in the list. When off, the row keeps its count chip \
                                                       but the messages are displayed only as cards in the \
                                                       reading pane."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreadExpansion(row.is_active()));
                                        },
                                    },

                                    #[name = "threads_expanded_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.threading && model.thread_expansion,
                                        set_title: &i18n("Expand conversations by default"),
                                        set_subtitle: &i18n("Show every message of a conversation in the list. \
                                                       When off, conversations start collapsed to their \
                                                       newest message."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreadsExpanded(row.is_active()));
                                        },
                                    },

                                    #[name = "thread_newest_first_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Newest message first"),
                                        set_subtitle: &i18n("Show a conversation's latest message at the top of \
                                                       the reading pane. Off reads oldest to newest, \
                                                       downward."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreadNewestFirst(row.is_active()));
                                        },
                                    },

                                    #[name = "reply_position_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Reply editor"),
                                        set_subtitle: &i18n("Where a reply or forward opens in the reading \
                                                       pane. Following the reading order puts it above the \
                                                       messages with newest first, and below them otherwise, \
                                                       so it continues the conversation where it ends."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReplyPosition(row.selected()));
                                        },
                                    },

                                    #[name = "card_attachments_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Attachments on each message"),
                                        set_subtitle: &i18n("List a message's attachments beneath it in a \
                                                       conversation, so which file came with which message \
                                                       is clear. Click one to open it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleCardAttachments(row.is_active()));
                                        },
                                    },

                                    #[name = "attachment_drawer_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Attachment drawer"),
                                        set_subtitle: &i18n("Gather every attachment in the open conversation \
                                                       in a drawer beneath the messages."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAttachmentDrawer(row.is_active()));
                                        },
                                    },

                                    #[name = "confirm_thread_delete_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Confirm conversation deletion"),
                                        set_subtitle: &i18n("Warn before deleting when a whole conversation is \
                                                       selected, since every message in the thread goes \
                                                       with it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleConfirmThreadDelete(row.is_active()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("reading")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Reading"),

                                    #[name = "read_mark_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Mark as read"),
                                        set_subtitle: &i18n("When an opened message counts as read. \
                                                       Conversations mark each message as it \
                                                       comes into view."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReadMark(row.selected()));
                                        },
                                    },

                                    #[name = "message_theme_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Message appearance"),
                                        set_subtitle: &i18n("Theme for email content only, not the app itself."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeMessageTheme(row.selected()));
                                        },
                                    },

                                    #[name = "override_fonts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use my own font"),
                                        set_subtitle: &i18n("Show every message in the font and size chosen \
                                                       below instead of the sender's. Headings keep \
                                                       their relative size; code stays monospaced."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleOverrideFonts(row.is_active()));
                                        },
                                    },

                                    #[name = "reader_font_row"]
                                    adw::ActionRow {
                                        set_title: &i18n("Message font"),
                                        set_subtitle: &i18n("The interface font unless another is chosen."),
                                        #[name = "reader_font_button"]
                                        add_suffix = &gtk::FontDialogButton {
                                            set_valign: gtk::Align::Center,
                                            set_dialog: &gtk::FontDialog::new(),
                                            set_level: gtk::FontLevel::Font,
                                            set_use_font: true,
                                            connect_font_desc_notify[sender] => move |button| {
                                                let font = button
                                                    .font_desc()
                                                    .map(|d| d.to_string())
                                                    .unwrap_or_default();
                                                sender.input(PrefInput::ChangeReaderFont(font));
                                            },
                                        },
                                    },

                                    #[name = "override_colors_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use my own colours"),
                                        set_subtitle: &i18n("Ignore the text and background colours senders \
                                                       set, so every message reads in the same black or \
                                                       white on the reader's ground. Pictures are kept; \
                                                       links take the accent colour."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleOverrideColors(row.is_active()));
                                        },
                                    },

                                    #[name = "plain_mono_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Plain-text messages in monospace"),
                                        set_subtitle: &i18n("Show messages sent as plain text in a fixed-width \
                                                       font, so columns and code line up. Formatted \
                                                       messages are not affected."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePlainMonospace(row.is_active()));
                                        },
                                    },

                                    #[name = "plain_font_row"]
                                    adw::ActionRow {
                                        set_title: &i18n("Monospace font"),
                                        set_subtitle: &i18n("The system's monospace font unless another is chosen."),
                                        #[name = "plain_font_button"]
                                        add_suffix = &gtk::FontDialogButton {
                                            set_valign: gtk::Align::Center,
                                            set_dialog: &gtk::FontDialog::new(),
                                            set_level: gtk::FontLevel::Font,
                                            set_use_font: true,
                                            connect_font_desc_notify[sender] => move |button| {
                                                let font = button
                                                    .font_desc()
                                                    .map(|d| d.to_string())
                                                    .unwrap_or_default();
                                                sender.input(PrefInput::ChangePlainFont(font));
                                            },
                                        },
                                    },

                                    #[name = "card_actions_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Message card actions palette"),
                                        set_subtitle: &i18n("How each message's action icons show in the \
                                                       reader, single or threaded."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeCardActionsMode(row.selected()));
                                        },
                                    },

                                    #[name = "card_palette_collapse_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Message card actions palette timeout"),
                                        set_subtitle: &i18n("Seconds a card's actions palette stays open \
                                                       after the cursor leaves it."),
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeCardPaletteCollapse(row.value() as u64));
                                        },
                                    },

                                    // Only the hidden-behind-a-toggle mode has
                                    // a ⋯ to press, so the choice is only live
                                    // there.
                                    #[name = "card_palette_menu_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.card_actions_hover,
                                        set_title: &i18n("Message card actions palette as a menu"),
                                        set_subtitle: &i18n("The \u{22ef} opens the message's menu, the same \
                                                       one a right-click shows, instead of sliding the \
                                                       actions palette out."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleCardPaletteMenu(row.is_active()));
                                        },
                                    },

                                    #[name = "single_message_card_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Single messages as cards"),
                                        set_subtitle: &i18n("Show a lone message as an inset card with the same \
                                                       border as a conversation's messages. Off fills the \
                                                       pane edge to edge."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSingleMessageCard(row.is_active()));
                                        },
                                    },

                                    #[name = "always_show_recipients_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Always show recipients"),
                                        set_subtitle: &i18n("Show who each message went to under its sender, \
                                                       without clicking the recipients chip. With one \
                                                       recipient the chip is dropped entirely."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAlwaysShowRecipients(row.is_active()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Reader View"),
                                    set_description: Some(&i18n("Reader View shows a message as its text alone, in one plain format, without the sender's layout, colours and fonts.")),

                                    #[name = "reader_switch_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Reader View switch"),
                                        set_subtitle: &i18n("Show the switch in the message header, beside the account name."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleReaderSwitch(row.is_active()));
                                        },
                                    },

                                    #[name = "reader_default_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("When a message is opened"),
                                        set_subtitle: &i18n("Keep the switch where it was last set, or start every message in Reader View or as sent. The switch still changes the message on screen."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReaderDefault(row.selected()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("composing")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Composing"),

                                    #[name = "compose_inline_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Compose in the main window"),
                                        set_subtitle: &i18n("New message slides down over the reading pane, \
                                                       like a reply — pop it out to a window from its \
                                                       header. Off = open a separate window directly."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleComposeInline(row.is_active()));
                                        },
                                    },

                                    #[name = "reply_fields_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show From, To and Subject in the reply panel"),
                                        set_subtitle: &i18n("The inline reply opens with its address and subject rows \
                                                       showing. Off, they stay folded away behind a button in \
                                                       the panel's header."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleReplyFields(row.is_active()));
                                        },
                                    },

                                    #[name = "default_from_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Send new messages from"),
                                        set_subtitle: &i18n("Replies still answer from the address the \
                                                       original was sent to."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeComposeDefaultFrom(row.selected()));
                                        },
                                    },

                                    #[name = "paste_plain_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Paste as plain text"),
                                        set_subtitle: &i18n("Pasting into a message strips the \
                                                       clipboard's formatting. Off, a paste \
                                                       keeps its formatting. Right-clicking \
                                                       the editor always offers both."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePastePlain(row.is_active()));
                                        },
                                    },

                                    #[name = "compose_format_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Write messages in"),
                                        set_subtitle: &i18n("What new messages, replies and forwards start \
                                                       as. Markdown and HTML are written as source and \
                                                       sent as formatted mail; plain text is sent without \
                                                       formatting. The composer's format button switches \
                                                       any one message."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeComposeFormat(row.selected()));
                                        },
                                    },
                                },

                                #[name = "spelling_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Spelling"),
                                    // The description is filled in at init with the
                                    // dictionaries the app can actually see — checking a
                                    // language without one silently checks nothing, so
                                    // honesty about what is installed beats silence.

                                    #[name = "spellcheck_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Check spelling as you type"),
                                        set_subtitle: &i18n("Misspelled words in the message body \
                                                       are underlined; right-click a word \
                                                       for corrections."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSpellcheck(row.is_active()));
                                        },
                                    },

                                    #[name = "spell_lang_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Language"),
                                        set_subtitle: &i18n("Dictionaries the app can see"),
                                    },

                                    // Words the user taught the checker ("Learn Spelling"
                                    // in the composer, or added right here). Populated
                                    // and maintained imperatively in init.
                                    #[name = "spell_words_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Added words"),
                                        set_subtitle: &i18n("Words the spell checker was taught. \
                                                       The message body applies changes \
                                                       after a restart."),
                                    },
                                },
                            },

                            add_named[Some("privacy")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Privacy"),
                                    set_description: Some(
                                        i18n("Hylki collects no telemetry and sends no analytics. Remote \
                                         content (images, trackers) is blocked by default. Allow it per \
                                         message, trust a sender to always load it, or turn on \"Always \
                                         load remote content\" below.").as_str()
                                    ),

                                    #[name = "auto_remote_content_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Always load remote content"),
                                        set_subtitle: &i18n("Show images and other remote content in every new \
                                                       message without asking. Off by default, since \
                                                       remote content can be used to track when and where \
                                                       you read a message."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAutoRemoteContent(row.is_active()));
                                        },
                                    },

                                    #[name = "show_remote_banner_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Warn when remote content is blocked"),
                                        set_subtitle: &i18n("Shows the banner offering to load it. Turning this \
                                                       off only hides the notice — remote content is still \
                                                       blocked just the same."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleShowRemoteBanner(row.is_active()));
                                        },
                                    },

                                    #[name = "gravatar_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use Gravatar when a contact has no photo"),
                                        set_subtitle: &i18n("Local GNOME Contacts photos are always preferred. \
                                                       Gravatar sends a hash of the sender's email."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleGravatar(row.is_active()));
                                        },
                                    },

                                    #[name = "sender_logos_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show sender logos"),
                                        set_subtitle: &i18n("Fills the sender's avatar with the brand's logo: the \
                                                       one the sender publishes for mail (BIMI), one bundled \
                                                       with Hylki, or the site's own icon. All but the bundled \
                                                       ones are fetched from the sender's domain, which then \
                                                       learns your IP address, as blocking remote content \
                                                       otherwise avoids."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSenderLogos(row.is_active()));
                                        },
                                    },

                                },
                            },

                            add_named[Some("datetime")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Date and Time"),
                                    set_description: Some(
                                        i18n("By default dates follow the system's own arrangement — its \
                                         field order, month names and clock. Choose a format here to \
                                         override what your system is set to.").as_str()
                                    ),

                                    #[name = "date_style_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Date format"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeDateStyle(row.selected()));
                                        },
                                    },

                                    #[name = "clock_style_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Clock"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeClockStyle(row.selected()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("system")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("System"),

                                    #[name = "language_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Language"),
                                        set_subtitle: &i18n("The language Hylki is shown in. \"System\" follows \
                                                       the desktop, with English where no translation exists. \
                                                       A change applies the next time Hylki starts."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeLanguage(row.selected()));
                                        },
                                    },

                                    #[name = "background_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Keep running in the background"),
                                        set_subtitle: &i18n("Closing the window hides it instead of quitting, so new \
                                                       mail still arrives. Hylki then appears under Background \
                                                       Apps in the system menu, where it can be quit."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRunInBackground(row.is_active()));
                                        },
                                    },

                                    #[name = "autostart_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Start at login"),
                                        set_subtitle: &i18n("Start checking for mail when you log in. Hylki starts \
                                                       without a window and waits in the system menu."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAutostart(row.is_active()));
                                        },
                                    },

                                    #[name = "tray_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show a tray icon"),
                                        set_subtitle: &i18n("An icon in the system tray, with a red dot while any \
                                                       inbox has unread mail. Click it to open Hylki. GNOME \
                                                       needs the AppIndicator extension; other desktops show \
                                                       it as they are."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleTray(row.is_active()));
                                        },
                                    },

                                    #[name = "tray_icon_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Tray icon"),
                                        set_subtitle: &i18n("The Hylki icon, or a plain envelope in white or black \
                                                       to match the panel."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeTrayIcon(row.selected()));
                                        },
                                    },

                                    #[name = "tray_mail_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Unread mail in the tray menu"),
                                        set_subtitle: &i18n("List the newest unread inbox messages in the icon's \
                                                       menu; click one to open it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleTrayMail(row.is_active()));
                                        },
                                    },

                                    #[name = "single_key_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Single-key shortcuts"),
                                        set_subtitle: &i18n("Act on mail with one key and no modifier — j/k to move, \
                                                       r to reply, a to archive. Press Ctrl+? for the full list."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSingleKey(row.is_active()));
                                        },
                                    },

                                    #[name = "console_mode_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Console mode"),
                                        set_subtitle: &i18n("A verbose live console in the status bar showing \
                                                       everything Hylki is doing under the hood."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleConsoleMode(row.is_active()));
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Export log"),
                                        set_subtitle: &i18n("Save everything the console has recorded since Hylki \
                                                       started, to attach to a bug report. Email addresses \
                                                       are shortened to their domain."),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ExportLog,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("co.hyprlab.Hylki-go-next-symbolic"),
                                        },
                                    },
                                },

                                // The Files right-click extension (#188).
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("GNOME Files Integration"),
                                    set_description: Some(
                                        i18n("Add \"Send with Hylki\" to the right-click menu in Files (Nautilus): \
                                              the selected files open in a new message, attached. This installs \
                                              a small extension in your home folder. It also needs the \
                                              nautilus-python package (python3-nautilus on Debian and Ubuntu), \
                                              and Files has to be restarted before the entry appears.").as_str()
                                    ),

                                    adw::ActionRow {
                                        set_title: &i18n("Right-click menu entry"),
                                        #[watch]
                                        set_subtitle: &nautilus_status_text(&model.nautilus),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Remove"),
                                            set_valign: gtk::Align::Center,
                                            #[watch]
                                            set_visible: model.nautilus.status != crate::nautilus_ext::Status::NotInstalled,
                                            connect_clicked => PrefInput::NautilusRemove,
                                        },
                                        add_suffix = &gtk::Button {
                                            #[watch]
                                            set_label: &if model.nautilus.status == crate::nautilus_ext::Status::Outdated {
                                                i18n("Update")
                                            } else {
                                                i18n("Install")
                                            },
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: model.nautilus.status != crate::nautilus_ext::Status::Installed,
                                            connect_clicked => PrefInput::NautilusInstall,
                                        },
                                    },

                                    // The loader's install command, ready to paste, while
                                    // there is no sign Files can load the extension.
                                    #[name = "nautilus_cmd_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Install the nautilus-python package first: paste this in a terminal"),
                                        set_text: model.nautilus_cmd.unwrap_or_default(),
                                        set_editable: false,
                                        add_css_class: "monospace",
                                        #[watch]
                                        set_visible: model.nautilus_cmd.is_some()
                                            && !model.nautilus.loaded
                                            && model.nautilus.loader != Some(true),
                                        add_suffix = &gtk::Button {
                                            set_icon_name: "co.hyprlab.Hylki-edit-copy-symbolic",
                                            set_valign: gtk::Align::Center,
                                            set_tooltip_text: Some(i18n("Copy").as_str()),
                                            add_css_class: "flat",
                                            connect_clicked[cmd = model.nautilus_cmd.unwrap_or_default().to_string()] => move |b| {
                                                b.clipboard().set_text(&cmd);
                                                b.set_icon_name("co.hyprlab.Hylki-verified-checkmark-symbolic");
                                                let b = b.clone();
                                                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                                                    b.set_icon_name("co.hyprlab.Hylki-edit-copy-symbolic");
                                                });
                                            },
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Restart Files"),
                                        set_subtitle: &i18n("Closes every Files window, the same as \"nautilus -q\". \
                                                       The next one opens with the entry, or without it once \
                                                       removed."),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Restart"),
                                            set_valign: gtk::Align::Center,
                                            connect_clicked => PrefInput::NautilusRestartFiles,
                                        },
                                    },

                                    // What the handed-in files open into, and
                                    // what happens when they are big.
                                    #[name = "files_action_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Default Send with Hylki behavior"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesAction(row.selected()));
                                        },
                                    },

                                    #[name = "files_large_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Default large attachments behavior"),
                                        set_subtitle: &i18n("Upload to cloud storage needs a cloud storage \
                                                       account (Settings → Cloud Storage) otherwise the \
                                                       files are attached by default."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesLarge(row.selected()));
                                        },
                                    },

                                    #[name = "files_limit_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Size limit"),
                                        set_subtitle: &i18n("In MB, for all the files together."),
                                        set_adjustment: Some(&gtk::Adjustment::new(20.0, 1.0, 5000.0, 1.0, 10.0, 0.0)),
                                        set_numeric: true,
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesLimit(row.value().round() as u32));
                                        },
                                    },
                                },

                                // Where a link in a message goes (#232).
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Links"),
                                    set_description: Some(link_group_description(&model.browsers).as_str()),

                                    // Its handler is connected after the list
                                    // is filled below, not here: giving a
                                    // ComboRow its model moves the selection
                                    // to the first item, and that notification
                                    // would read as the user choosing
                                    // "System default".
                                    #[name = "link_browser_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Open links in"),
                                    },
                                },
                            },

                            add_named[Some("backup")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Backup"),
                                    set_description: Some(
                                        i18n("Everything Hylki keeps, as one file: mail and cloud storage \
                                         accounts, preferences, filters, tags, the sidebar and window \
                                         layout, the app icon choice and the words taught to the spell \
                                         checker. Passwords and sign-ins stay in the system keyring and \
                                         are never exported; OpenPGP keys stay in GnuPG.").as_str()
                                    ),

                                    adw::ActionRow {
                                        set_title: &i18n("Export settings"),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ExportSettings,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("co.hyprlab.Hylki-go-next-symbolic"),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Import settings"),
                                        set_subtitle: &i18n("Replaces the current accounts and preferences in \
                                                       place. Don't remove accounts first: removal also \
                                                       deletes their keyring passwords, which no backup \
                                                       carries."),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ImportSettings,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("co.hyprlab.Hylki-go-next-symbolic"),
                                        },
                                    },
                                },
                            },
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
            nautilus: crate::nautilus_ext::State::read(),
            nautilus_cmd: crate::platform::nautilus_python_install_command(),
            files: init.files,
            files_rows: None,
            browsers: crate::ui::launch::browsers(),
            notifications: init.notifications,
            toolbar: init.reader_toolbar.clone(),
            focus: init.focus,
            toolbar_editor: None,
            show_unified: init.show_unified,
            unified_kinds: init.unified_kinds,
            unified_chips: init.unified_chips,
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
            content_page: None,
            split: None,
            accounts_sender: init.accounts_sender.clone(),
            host_header: None,
            pgp_keys: None,
            identities: init.identities.clone(),
            cloud: None,
            editor_open: false,
            editor_page: "accounts",
        };

        let widgets = view_output!();
        tracing::debug!("settings window: preferences view built in {:?}", t_init.elapsed());

        // Settings never truncates. AdwComboRow's DEFAULT item factory builds
        // the selected-value display with an ellipsizing label — and rebuilds
        // it on every selection change, so fixing the widget after the fact
        // doesn't stick ("Follow system" → "Follow s…"). Give every combo a
        // plain factory whose labels never ellipsize; the short row titles
        // yield the space instead.
        fn no_truncate(row: &adw::ComboRow) {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, item| {
                if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                    let label = gtk::Label::new(None);
                    label.set_xalign(0.0);
                    item.set_child(Some(&label));
                }
            });
            factory.connect_bind(|_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
                if let (Some(label), Some(s)) = (
                    item.child().and_downcast::<gtk::Label>(),
                    item.item().and_downcast::<gtk::StringObject>(),
                ) {
                    label.set_label(&s.string());
                }
            });
            row.set_factory(Some(&factory));
        }
        for row in [
            &widgets.fetch_row,
            &widgets.preview_lines_row,
            &widgets.message_theme_row,
            &widgets.card_actions_row,
            &widgets.app_theme_row,
            &widgets.settings_open_row,
            &widgets.tray_icon_row,
            &widgets.date_style_row,
            &widgets.clock_style_row,
            &widgets.language_row,
            &widgets.chevron_side_row,
        ] {
            no_truncate(row);
        }

        // The install-command field is read-only: the row's pencil, which
        // says "type here", would be a lie.
        hide_edit_icon(widgets.nautilus_cmd_row.upcast_ref());

        // Language combo: the system's, then every catalogue shipped.
        let choices = language_choices();
        let lang_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        widgets.language_row.set_model(Some(&gtk::StringList::new(&lang_labels)));
        widgets.language_row.set_selected(
            choices.iter().position(|(_, c)| *c == init.language).unwrap_or(0) as u32,
        );

        widgets.auto_remote_content_row.set_active(init.auto_remote_content);
        widgets.show_remote_banner_row.set_active(init.show_remote_banner);
        widgets.gravatar_row.set_active(init.gravatar);
        widgets.avatars_row.set_active(init.avatars);
        widgets.own_mailbox_face_row.set_active(init.own_mailbox_face);
        widgets.sender_logos_row.set_active(init.sender_logos);

        // Mail-check interval combo.
        let labels_owned: Vec<String> = FETCH_INTERVALS.iter().map(|(l, _)| i18n(l)).collect();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets.fetch_row.set_model(Some(&gtk::StringList::new(&labels)));
        let selected = FETCH_INTERVALS
            .iter()
            .position(|(_, secs)| *secs == init.fetch_interval_secs)
            .unwrap_or(0);
        widgets.fetch_row.set_selected(selected as u32);
        widgets.push_row.set_active(init.push);
        widgets.notifications_row.set_active(init.notifications);
        widgets.notification_content_row.set_active(init.notification_content);
        widgets.show_attachments_row.set_active(init.show_attachments);
        widgets.show_contacts_row.set_active(init.show_contacts);
        widgets.show_unified_row.set_active(init.show_unified);
        widgets.unified_chip_all_inboxes_row.set_active(init.unified_chips.all_inboxes);
        widgets.unified_chip_starred_row.set_active(init.unified_chips.starred);
        widgets.unified_chip_drafts_row.set_active(init.unified_chips.drafts);
        widgets.unified_chip_archive_row.set_active(init.unified_chips.archive);
        widgets.unified_chip_filtered_row.set_active(init.unified_chips.filtered);
        widgets.unified_filtered_row.set_active(init.unified_filtered);
        widgets.unified_starred_row.set_active(init.unified_kinds.starred);
        widgets.unified_sent_row.set_active(init.unified_kinds.sent);
        widgets.unified_drafts_row.set_active(init.unified_kinds.drafts);
        widgets.unified_archive_row.set_active(init.unified_kinds.archive);
        widgets.unified_tags_row.set_active(init.unified_tags);
        for (row, placement) in [
            (&widgets.filtered_placement_row, init.filtered_placement),
            (&widgets.tags_placement_row, init.tags_placement),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                i18n("In the unified section").as_str(),
                i18n("Above the accounts").as_str(),
                i18n("Below the accounts").as_str(),
            ])));
            row.set_selected(placement_index(placement));
        }
        widgets.chevron_side_row.set_model(Some(&gtk::StringList::new(&[i18n("Left").as_str(), i18n("Right").as_str()])));
        widgets.chevron_side_row.set_selected(if init.chevrons_left { 0 } else { 1 });
        widgets.sidebar_hover_expand_row.set_active(init.sidebar_hover_expand);
        widgets.remember_sidebar_row.set_active(init.remember_sidebar);
        widgets.remember_rail_row.set_active(init.remember_rail);
        widgets.rail_dots_row.set_active(init.rail_dots);
        widgets.rail_fold_row.set_enable_expansion(init.rail_fold.enabled);
        widgets.rail_fold_accounts_row.set_active(init.rail_fold.accounts);
        widgets.rail_fold_all_inboxes_row.set_active(init.rail_fold.all_inboxes);
        widgets.rail_fold_starred_row.set_active(init.rail_fold.starred);
        widgets.rail_fold_sent_row.set_active(init.rail_fold.sent);
        widgets.rail_fold_drafts_row.set_active(init.rail_fold.drafts);
        widgets.rail_fold_archive_row.set_active(init.rail_fold.archive);
        widgets.rail_fold_filtered_row.set_active(init.rail_fold.filtered);
        widgets.rail_fold_tags_row.set_active(init.rail_fold.tags);
        let preview_labels_owned = [i18n("Off"), i18n("1 line"), i18n("2 lines"), i18n("3 lines")];
        let preview_labels: Vec<&str> = preview_labels_owned.iter().map(String::as_str).collect();
        widgets
            .preview_lines_row
            .set_model(Some(&gtk::StringList::new(&preview_labels)));
        widgets.preview_lines_row.set_selected(init.preview_lines.min(3));

        widgets.background_row.set_active(init.run_in_background);
        widgets.autostart_row.set_active(init.autostart);
        // Starting at login only means anything if Hylki stays running.
        widgets.autostart_row.set_sensitive(init.run_in_background);
        {
            let autostart_row = widgets.autostart_row.clone();
            widgets.background_row.connect_active_notify(move |row| {
                autostart_row.set_sensitive(row.is_active());
            });
        }
        // Theme: title in the row's own voice, the gallery beneath — the
        // same shape as the app-icon row under it. A pick goes straight
        // out and is painted at once.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(12);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("Theme").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            let s = sender.clone();
            let gallery = crate::ui::theme_picker::gallery(
                &init.theme,
                std::rc::Rc::new(move |id: &str| s.input(PrefInput::ChangeTheme(id.to_string()))),
            );
            gallery.set_margin_top(10);
            body.append(&gallery);
            widgets.theme_row.set_child(Some(&body));
        }

        // App icon: title in the row's own voice, the gallery beneath.
        // Picks go straight out; the app applies and offers the restart.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(8);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("App icon").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            widgets.app_icon_row.set_child(Some(&body));
            // The icon gallery decodes the whole catalogue; it fills in a
            // moment after the window is up rather than holding it back.
            let s = sender.clone();
            let app_icon = init.app_icon.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let s = s.clone();
                let strip = crate::ui::icon_picker::strip(
                    &app_icon,
                    56,
                    std::rc::Rc::new(move |id: &str| s.input(PrefInput::ChangeAppIcon(id.to_string()))),
                );
                strip.set_margin_top(6);
                body.append(&strip);
                gtk::glib::ControlFlow::Break
            });
        }
        tracing::debug!("settings window: preferences icon strip done at {:?}", t_init.elapsed());

        widgets.tray_row.set_active(init.tray);
        let tray_icon_labels_owned: Vec<String> = TRAY_ICONS.iter().map(|(l, _)| i18n(l)).collect();
        let tray_icon_labels: Vec<&str> = tray_icon_labels_owned.iter().map(String::as_str).collect();
        widgets
            .tray_icon_row
            .set_model(Some(&gtk::StringList::new(&tray_icon_labels)));
        let tray_icon_sel = TRAY_ICONS
            .iter()
            .position(|(_, t)| *t == init.tray_icon)
            .unwrap_or(0);
        widgets.tray_icon_row.set_selected(tray_icon_sel as u32);
        widgets.tray_mail_row.set_active(init.tray_mail);
        // The icon choice and the menu's mail list only mean anything with
        // a tray icon shown.
        widgets.tray_icon_row.set_sensitive(init.tray);
        widgets.tray_mail_row.set_sensitive(init.tray);
        {
            let tray_icon_row = widgets.tray_icon_row.clone();
            let tray_mail_row = widgets.tray_mail_row.clone();
            widgets.tray_row.connect_active_notify(move |row| {
                tray_icon_row.set_sensitive(row.is_active());
                tray_mail_row.set_sensitive(row.is_active());
            });
        }
        tracing::debug!("settings window: prefs tail A (tray) at {:?}", t_init.elapsed());
        widgets.single_key_row.set_active(init.single_key_shortcuts);
        widgets.console_mode_row.set_active(init.console_mode);
        widgets.read_mark_row.set_model(Some(&gtk::StringList::new(&[
            i18n("When displayed").as_str(),
            i18n("After two seconds").as_str(),
            i18n("Manually").as_str(),
        ])));
        no_truncate(&widgets.read_mark_row);
        widgets.read_mark_row.set_selected(match init.read_mark {
            crate::config::ReadMark::Shown => 0,
            crate::config::ReadMark::Delay => 1,
            crate::config::ReadMark::Manual => 2,
        });
        widgets.threading_row.set_active(init.threading);
        widgets.threads_expanded_row.set_active(init.threads_expanded);
        widgets.thread_newest_first_row.set_active(init.thread_newest_first);
        widgets.always_show_recipients_row.set_active(init.always_show_recipients);
        widgets.single_message_card_row.set_active(init.single_message_card);
        widgets.reader_switch_row.set_active(init.reader_switch);
        widgets.reader_default_row.set_model(Some(&gtk::StringList::new(&[
            &i18n("Remember the last choice"),
            &i18n("Reader View on"),
            &i18n("Reader View off"),
        ])));
        no_truncate(&widgets.reader_default_row);
        widgets.reader_default_row.set_selected(match init.reader_default {
            crate::config::ReaderDefault::Remember => 0,
            crate::config::ReaderDefault::On => 1,
            crate::config::ReaderDefault::Off => 2,
        });
        widgets.card_attachments_row.set_active(init.card_attachments);
        widgets.attachment_drawer_row.set_active(init.attachment_drawer);
        widgets.thread_expansion_row.set_active(init.thread_expansion);
        widgets.confirm_thread_delete_row.set_active(init.confirm_thread_delete);
        widgets.card_actions_row.set_model(Some(&gtk::StringList::new(&[
            i18n("Hidden behind a toggle").as_str(),
            i18n("Shown while hovering").as_str(),
            i18n("Always visible").as_str(),
        ])));
        widgets.card_actions_row.set_selected(if init.card_actions_hover {
            0
        } else if init.card_actions_auto {
            1
        } else {
            2
        });
        widgets.list_palette_row.set_active(init.list_palette);
        widgets.list_palette_hover_row.set_active(init.list_palette_hover);
        widgets.card_palette_menu_row.set_active(init.card_palette_menu);
        widgets.swipe_enabled_row.set_active(init.swipe_enabled);
        widgets.swipe_reversed_row.set_active(init.swipe_reversed);
        widgets.compose_inline_row.set_active(init.compose_inline);
        widgets.reply_fields_row.set_active(init.reply_fields);
        widgets.files_action_row.set_model(Some(&gtk::StringList::new(&[
            i18n("Ask each time").as_str(),
            i18n("A new message").as_str(),
            i18n("A draft you pick").as_str(),
            i18n("A reply to a message you pick").as_str(),
        ])));
        no_truncate(&widgets.files_action_row);
        widgets.files_action_row.set_selected(files_action_index(init.files.action));
        widgets.files_large_row.set_model(Some(&gtk::StringList::new(&[
            i18n("Ask each time").as_str(),
            i18n("Attach them anyway").as_str(),
            i18n("Upload to cloud storage and link them").as_str(),
        ])));
        no_truncate(&widgets.files_large_row);
        widgets.files_large_row.set_selected(files_large_index(init.files.large));
        widgets.files_limit_row.set_value(init.files.limit_mb as f64);
        model.files_rows = Some((
            widgets.files_action_row.clone(),
            widgets.files_large_row.clone(),
            widgets.files_limit_row.clone(),
        ));
        // The links combo: the desktop's own default, the chooser, then every
        // browser this process can launch.
        {
            let mut labels: Vec<String> =
                vec![i18n("System default"), i18n("Ask each time")];
            labels.extend(model.browsers.iter().map(|b| b.name.clone()));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets.link_browser_row.set_model(Some(&gtk::StringList::new(&refs)));
            no_truncate(&widgets.link_browser_row);
            widgets
                .link_browser_row
                .set_selected(link_browser_index(&model.browsers, &init.link_browser));
            widen_combo_value(&widgets.link_browser_row, 50);
            let s = sender.clone();
            widgets.link_browser_row.connect_selected_notify(move |row| {
                s.input(PrefInput::ChangeLinkBrowser(row.selected()));
            });
        }
        // "Send new messages from": the open folder's account, then every
        // enabled account and alias, labelled as the composer's From row
        // labels them. Only meaningful with more than one identity.
        {
            let mut labels: Vec<String> = vec![i18n("Account of the current folder")];
            labels.extend(model.identities.iter().map(|(name, addr)| {
                if name.trim().is_empty() {
                    addr.clone()
                } else {
                    format!("{name} <{addr}>")
                }
            }));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets.default_from_row.set_model(Some(&gtk::StringList::new(&refs)));
            let want = init.compose_default_from.trim();
            let sel = model
                .identities
                .iter()
                .position(|(_, addr)| addr.eq_ignore_ascii_case(want))
                .map(|i| i + 1)
                .unwrap_or(0);
            widgets.default_from_row.set_selected(sel as u32);
            widgets.default_from_row.set_visible(model.identities.len() > 1);
            widen_combo_value(&widgets.default_from_row, 50);
        }
        widgets.paste_plain_row.set_active(init.paste_plain);
        widgets.compose_format_row.set_model(Some(&gtk::StringList::new(&[
            &i18n("Rich text"),
            &i18n("Markdown"),
            &i18n("HTML"),
            &i18n("Plain text"),
        ])));
        no_truncate(&widgets.compose_format_row);
        widgets.compose_format_row.set_selected(match init.compose_format {
            crate::config::ComposeFormat::Rich => 0,
            crate::config::ComposeFormat::Markdown => 1,
            crate::config::ComposeFormat::Html => 2,
            crate::config::ComposeFormat::Plain => 3,
        });
        widgets.reply_position_row.set_model(Some(&gtk::StringList::new(&[
            &i18n("Above the messages"),
            &i18n("Below the messages"),
            &i18n("Follows the reading order"),
        ])));
        no_truncate(&widgets.reply_position_row);
        widgets.reply_position_row.set_selected(match init.reply_position {
            crate::config::ReplyPosition::Top => 0,
            crate::config::ReplyPosition::Bottom => 1,
            crate::config::ReplyPosition::Follow => 2,
        });
        widgets.spellcheck_row.set_active(init.spellcheck);
        // The language dropdown offers exactly what checking can use: the
        // installed dictionaries, behind a "System language" default. Typed
        // codes are gone — a language nobody has a dictionary for silently
        // checks nothing, so only real options are offered (#114).
        {
            // Listing the installed dictionaries reads several directories;
            // done a moment after the window is up, not before it.
            let spell_lang_row = widgets.spell_lang_row.clone();
            let spelling_group = widgets.spelling_group.clone();
            let spellcheck_langs = init.spellcheck_langs.clone();
            let sender = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
            let dicts = crate::ui::rich_editor::installed_dictionaries();
            let list = gtk::StringList::new(&[]);
            list.append(&i18n_f(
                "System language — {language}",
                &[("language", &crate::spell::language_display_name(
                    &crate::ui::rich_editor::resolved_spell_language()
                ))]
            ));
            for d in &dicts {
                list.append(&crate::spell::language_display_name(d));
            }
            spell_lang_row.set_model(Some(&list));
            let selected = dicts
                .iter()
                .position(|d| *d == spellcheck_langs)
                .map(|i| i as u32 + 1)
                .unwrap_or(0);
            spell_lang_row.set_selected(selected);
            if dicts.is_empty() {
                spell_lang_row.set_sensitive(false);
                spelling_group.set_description(Some(
                    "No dictionaries are visible to the app. On Flatpak, add your \
                     language with: flatpak config --set extra-languages <code>",
                ));
            }
            let s = sender.clone();
            // Connected after the initial set_selected, so restoring the
            // saved choice doesn't immediately re-save it.
            spell_lang_row.connect_selected_notify(move |row| {
                let i = row.selected() as usize;
                let code =
                    if i == 0 { String::new() } else { dicts.get(i - 1).cloned().unwrap_or_default() };
                s.input(PrefInput::SpellLangsEdited(code));
            });
                gtk::glib::ControlFlow::Break
            });
        }
        {
            // The learned-words list manages itself entirely in widget-land:
            // the rows call the spell module directly and rebuild in place.
            let exp = widgets.spell_words_row.clone();
            let add = adw::EntryRow::builder().title(&i18n("Add a word")).build();
            add.set_show_apply_button(true);
            {
                let exp = exp.clone();
                add.connect_apply(move |row| {
                    crate::spell::add_personal_word(&row.text());
                    row.set_text("");
                    rebuild_personal_words(&exp);
                });
            }
            exp.add_row(&add);
            rebuild_personal_words(&exp);
        }
        tracing::debug!("settings window: prefs tail B (words) at {:?}", t_init.elapsed());

        // Date and clock combos.
        let date_labels_owned: Vec<String> = DATE_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let date_labels: Vec<&str> = date_labels_owned.iter().map(String::as_str).collect();
        widgets
            .date_style_row
            .set_model(Some(&gtk::StringList::new(&date_labels)));
        widgets.date_style_row.set_selected(
            DATE_STYLES
                .iter()
                .position(|(_, s)| *s == init.date_style)
                .unwrap_or(0) as u32,
        );
        let clock_labels_owned: Vec<String> = CLOCK_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let clock_labels: Vec<&str> = clock_labels_owned.iter().map(String::as_str).collect();
        widgets
            .clock_style_row
            .set_model(Some(&gtk::StringList::new(&clock_labels)));
        widgets.clock_style_row.set_selected(
            CLOCK_STYLES
                .iter()
                .position(|(_, s)| *s == init.clock_style)
                .unwrap_or(0) as u32,
        );

        // Message-content appearance combo.
        let app_theme_labels_owned: Vec<String> = APP_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let app_theme_labels: Vec<&str> = app_theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .app_theme_row
            .set_model(Some(&gtk::StringList::new(&app_theme_labels)));
        let app_theme_sel = APP_THEMES
            .iter()
            .position(|(_, t)| *t == init.app_theme)
            .unwrap_or(0);
        widgets.app_theme_row.set_selected(app_theme_sel as u32);

        let theme_labels_owned: Vec<String> = MESSAGE_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let theme_labels: Vec<&str> = theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .message_theme_row
            .set_model(Some(&gtk::StringList::new(&theme_labels)));
        let theme_sel = MESSAGE_THEMES
            .iter()
            .position(|(_, t)| *t == init.message_theme)
            .unwrap_or(0);
        widgets.message_theme_row.set_selected(theme_sel as u32);
        tracing::debug!("settings window: prefs tail C (themes) at {:?}", t_init.elapsed());

        // The reader's font override (#56): the button shows the chosen font,
        // or the interface font when none was chosen yet (so what it shows is
        // what the reader will use); the row only responds while the switch
        // is on.
        {
            let desc = if init.reader_font.trim().is_empty() {
                gtk::Settings::default()
                    .and_then(|s| s.gtk_font_name())
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "Cantarell 11".to_string())
            } else {
                init.reader_font.clone()
            };
            widgets
                .reader_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.reader_font_row.set_sensitive(init.override_fonts);
            let font_row = widgets.reader_font_row.clone();
            widgets.override_fonts_row.connect_active_notify(move |row| {
                font_row.set_sensitive(row.is_active());
            });
        }
        widgets.override_fonts_row.set_active(init.override_fonts);
        widgets.override_colors_row.set_active(init.override_colors);
        // Plain-text messages in monospace (#181): the font button shows
        // the desktop's monospace font until another is chosen.
        {
            let desc = if init.plain_font.trim().is_empty() {
                crate::desktop::monospace_font()
            } else {
                init.plain_font.clone()
            };
            widgets
                .plain_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.plain_font_row.set_sensitive(init.plain_monospace);
            let font_row = widgets.plain_font_row.clone();
            widgets.plain_mono_row.connect_active_notify(move |row| {
                font_row.set_sensitive(row.is_active());
            });
        }
        widgets.plain_mono_row.set_active(init.plain_monospace);

        // Trackpad swipe sensitivity: 1 (libadwaita's own, far too long a
        // swipe for most trackpads) to 10, in half steps.
        let adj = gtk::Adjustment::new(
            init.swipe_sensitivity,
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
            0.5,
            1.0,
            0.0,
        );
        widgets.swipe_sensitivity_row.set_digits(1);
        widgets.swipe_sensitivity_row.set_adjustment(Some(&adj));

        // Hover-palette delay spinner (0–3000ms, step 50).
        // Actions palette timeout: 1–30 seconds.
        let adj = gtk::Adjustment::new(init.palette_collapse_secs as f64, 1.0, 30.0, 1.0, 5.0, 0.0);
        widgets.palette_collapse_row.set_adjustment(Some(&adj));
        let adj =
            gtk::Adjustment::new(init.card_palette_collapse_secs as f64, 1.0, 30.0, 1.0, 5.0, 0.0);
        widgets.card_palette_collapse_row.set_adjustment(Some(&adj));

        widgets
            .settings_open_row
            .set_model(Some(&gtk::StringList::new(&["Settings", "Accounts"])));
        widgets
            .settings_open_row
            .set_selected(if init.settings_open_accounts { 1 } else { 0 });

        widgets.accounts_slot.set_child(Some(&init.accounts_panel));
        let pgp = crate::ui::pgp_keys::PgpKeys::builder()
            .launch(crate::ui::pgp_keys::PgpKeysInit { identities: init.identities.clone() })
            .detach();
        widgets.pgp_slot.set_child(Some(pgp.widget()));
        model.pgp_keys = Some(pgp);
        let cloud = crate::ui::cloud_accounts::CloudAccounts::builder()
            .launch(())
            .forward(sender.input_sender(), |o| match o {
                crate::ui::cloud_accounts::CloudAccountsOutput::EditorOpen(open) => PrefInput::CloudEditorOpen(open),
            });
        widgets.cloud_slot.set_child(Some(cloud.widget()));
        model.cloud = Some(cloud);
        // The sidebar (#141): a heading per section, a row per category.
        tracing::debug!("settings window: prefs tail D (before sidebar rows) at {:?}", t_init.elapsed());
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
        model.panels_stack = Some(widgets.panels_stack.clone());
        model.accounts_slot = Some(widgets.accounts_slot.clone());
        model.toolbar_editor = Some(ToolbarEditor::build(&widgets.toolbar_editor_box, &sender));
        // The toolbar editor's zones want a row of six chips: the page's
        // column is 40px wider than the stock preferences clamp allows.
        widen_page(widgets.appearance_page.upcast_ref(), 640);
        model.rebuild_toolbar_chips();
        tracing::debug!("settings window: prefs tail E (sidebar rows built) at {:?}", t_init.elapsed());
        model.side_list = Some(widgets.side_list.clone());
        model.content_page = Some(widgets.content_page.clone());
        model.split = Some(widgets.split.clone());
        let first = init
            .start_page
            .as_deref()
            .filter(|id| side_page(id).is_some())
            .unwrap_or(if init.start_on_accounts { "accounts" } else { "general" });
        model.select_row(first);
        // Every page but the one shown leaves the stack until after the
        // first paint: laid out and styled together, the pages' hundreds of
        // rows were most of the wait for the window to appear.
        {
            let stack = &widgets.panels_stack;
            let shown = stack.visible_child_name().map(|n| n.to_string());
            let pages = stack.pages();
            let mut deferred = Vec::new();
            for i in 0..pages.n_items() {
                let Some(page) = pages.item(i).and_downcast::<gtk::StackPage>() else { continue };
                let name = page.name().map(|n| n.to_string()).unwrap_or_default();
                if Some(&name) != shown.as_ref() {
                    deferred.push((name, page.child()));
                }
            }
            for (_, child) in &deferred {
                stack.remove(child);
            }
            *model.deferred_pages.borrow_mut() = deferred;
            let s = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                s.input(PrefInput::MountPages);
                gtk::glib::ControlFlow::Break
            });
        }
        model.host_header = Some(widgets.host_header.clone());

        tracing::debug!("settings window: preferences init {:?}", t_init.elapsed());
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            PrefInput::ToggleOwnMailboxFace(on) => {
                let _ = sender.output(PrefOutput::SetOwnMailboxFace(on));
            }
            PrefInput::ToggleSenderLogos(on) => {
                let _ = sender.output(PrefOutput::SetSenderLogos(on));
            }
            PrefInput::ChangeDateStyle(i) => {
                if let Some((_, style)) = DATE_STYLES.get(i as usize) {
                    let _ = sender.output(PrefOutput::SetDateStyle(*style));
                }
            }
            PrefInput::ChangeClockStyle(i) => {
                if let Some((_, style)) = CLOCK_STYLES.get(i as usize) {
                    let _ = sender.output(PrefOutput::SetClockStyle(*style));
                }
            }
            PrefInput::ChangeLanguage(i) => {
                if let Some((_, code)) = language_choices().get(i as usize) {
                    let _ = sender.output(PrefOutput::SetLanguage(code.clone()));
                }
            }
            PrefInput::NautilusInstall => {
                if let Err(e) = crate::nautilus_ext::install() {
                    report(root, &i18n("Could not install the extension"), &e);
                }
                self.nautilus = crate::nautilus_ext::State::read();
            }
            PrefInput::NautilusRemove => {
                if let Err(e) = crate::nautilus_ext::remove() {
                    report(root, &i18n("Could not remove the extension"), &e);
                }
                self.nautilus = crate::nautilus_ext::State::read();
            }
            PrefInput::ChangeFilesAction(idx) => {
                use crate::config::FilesAction as A;
                let action = match idx {
                    1 => A::New,
                    2 => A::Draft,
                    3 => A::Reply,
                    _ => A::Ask,
                };
                if self.files.action != action {
                    self.files.action = action;
                    let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
                }
            }
            PrefInput::ChangeFilesLarge(idx) => {
                use crate::config::FilesLarge as L;
                let large = match idx {
                    1 => L::Attach,
                    2 => L::Cloud,
                    _ => L::Ask,
                };
                if self.files.large != large {
                    self.files.large = large;
                    let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
                }
            }
            PrefInput::ChangeLinkBrowser(idx) => {
                let choice = match idx {
                    0 => String::new(),
                    1 => crate::ui::launch::ASK.to_string(),
                    n => self
                        .browsers
                        .get(n as usize - 2)
                        .map(|b| b.id.clone())
                        .unwrap_or_default(),
                };
                let _ = sender.output(PrefOutput::SetLinkBrowser(choice));
            }
            PrefInput::ChangeFilesLimit(mb) => {
                let mb = mb.max(1);
                if self.files.limit_mb != mb {
                    self.files.limit_mb = mb;
                    let _ = sender.output(PrefOutput::SetFilesPrefs(self.files));
                }
            }
            PrefInput::SetFilesPrefs(p) => {
                self.files = p;
                if let Some((action, large, limit)) = self.files_rows.as_ref() {
                    if action.selected() != files_action_index(p.action) {
                        action.set_selected(files_action_index(p.action));
                    }
                    if large.selected() != files_large_index(p.large) {
                        large.set_selected(files_large_index(p.large));
                    }
                    if limit.value().round() as u32 != p.limit_mb {
                        limit.set_value(p.limit_mb as f64);
                    }
                }
            }
            PrefInput::NautilusRestartFiles => {
                if let Err(e) = crate::nautilus_ext::quit_files() {
                    report(root, &i18n("Could not restart Files"), &e);
                }
                self.nautilus = crate::nautilus_ext::State::read();
            }
            PrefInput::NautilusRefresh => {
                self.nautilus = crate::nautilus_ext::State::read();
            }
            PrefInput::ToggleAvatars(on) => {
                self.avatars = on;
                let _ = sender.output(PrefOutput::SetAvatars(on));
            }
            PrefInput::ToggleGravatar(on) => {
                let _ = sender.output(PrefOutput::SetGravatar(on));
            }
            PrefInput::ToggleAutoRemoteContent(on) => {
                let _ = sender.output(PrefOutput::SetAutoRemoteContent(on));
            }
            PrefInput::ToggleThreading(on) => {
                self.threading = on;
                let _ = sender.output(PrefOutput::SetThreading(on));
            }
            PrefInput::ToggleThreadExpansion(on) => {
                self.thread_expansion = on;
                let _ = sender.output(PrefOutput::SetThreadExpansion(on));
            }
            PrefInput::ToggleConfirmThreadDelete(on) => {
                let _ = sender.output(PrefOutput::SetConfirmThreadDelete(on));
            }
            PrefInput::ToggleThreadsExpanded(on) => {
                let _ = sender.output(PrefOutput::SetThreadsExpanded(on));
            }
            PrefInput::ToggleThreadNewestFirst(on) => {
                let _ = sender.output(PrefOutput::SetThreadNewestFirst(on));
            }
            PrefInput::ToggleAlwaysShowRecipients(on) => {
                let _ = sender.output(PrefOutput::SetAlwaysShowRecipients(on));
            }
            PrefInput::ToggleSingleMessageCard(on) => {
                let _ = sender.output(PrefOutput::SetSingleMessageCard(on));
            }
            PrefInput::ToggleReaderSwitch(on) => {
                let _ = sender.output(PrefOutput::SetReaderSwitch(on));
            }
            PrefInput::ChangeReaderDefault(idx) => {
                let policy = match idx {
                    1 => crate::config::ReaderDefault::On,
                    2 => crate::config::ReaderDefault::Off,
                    _ => crate::config::ReaderDefault::Remember,
                };
                let _ = sender.output(PrefOutput::SetReaderDefault(policy));
            }
            PrefInput::ToggleCardAttachments(on) => {
                let _ = sender.output(PrefOutput::SetCardAttachments(on));
            }
            PrefInput::ToggleAttachmentDrawer(on) => {
                let _ = sender.output(PrefOutput::SetAttachmentDrawer(on));
            }
            PrefInput::ChangeCardActionsMode(index) => {
                self.card_actions_hover = index == 0;
                let (hover_toggle, hover_auto) = match index {
                    0 => (true, false),
                    1 => (false, true),
                    _ => (false, false),
                };
                let _ = sender.output(PrefOutput::SetCardActionsMode {
                    hover_toggle,
                    hover_auto,
                });
            }
            PrefInput::ToggleListPalette(on) => {
                self.list_palette = on;
                let _ = sender.output(PrefOutput::SetListPalette(on));
            }
            PrefInput::ToggleListPaletteHover(on) => {
                let _ = sender.output(PrefOutput::SetListPaletteHover(on));
            }
            PrefInput::ToggleCardPaletteMenu(on) => {
                let _ = sender.output(PrefOutput::SetCardPaletteMenu(on));
            }
            PrefInput::ToggleSwipeEnabled(on) => {
                self.swipe_enabled = on;
                let _ = sender.output(PrefOutput::SetSwipeEnabled(on));
            }
            PrefInput::ToggleSwipeReversed(on) => {
                let _ = sender.output(PrefOutput::SetSwipeReversed(on));
            }
            PrefInput::ChangeSwipeSensitivity(factor) => {
                let _ = sender.output(PrefOutput::SetSwipeSensitivity(factor));
            }
            PrefInput::ToggleReplyFields(on) => {
                let _ = sender.output(PrefOutput::SetReplyFields(on));
            }
            PrefInput::ChangeComposeDefaultFrom(index) => {
                let addr = (index > 0)
                    .then(|| self.identities.get(index as usize - 1))
                    .flatten()
                    .map(|(_, addr)| addr.clone())
                    .unwrap_or_default();
                let _ = sender.output(PrefOutput::SetComposeDefaultFrom(addr));
            }
            PrefInput::ToggleComposeInline(on) => {
                let _ = sender.output(PrefOutput::SetComposeInline(on));
            }
            PrefInput::TogglePastePlain(on) => {
                let _ = sender.output(PrefOutput::SetPastePlain(on));
            }
            PrefInput::ToggleSpellcheck(on) => {
                let _ = sender.output(PrefOutput::SetSpellcheck(on));
            }
            PrefInput::SpellLangsEdited(langs) => {
                let _ = sender.output(PrefOutput::SetSpellcheckLangs(langs));
            }
            PrefInput::ChangeFetchInterval(index) => {
                let secs = FETCH_INTERVALS
                    .get(index as usize)
                    .map(|(_, s)| *s)
                    .unwrap_or(0);
                let _ = sender.output(PrefOutput::SetFetchInterval(secs));
            }
            PrefInput::TogglePush(on) => {
                let _ = sender.output(PrefOutput::SetPush(on));
            }
            PrefInput::ToggleNotifications(on) => {
                self.notifications = on;
                let _ = sender.output(PrefOutput::SetNotifications(on));
            }
            PrefInput::ToggleNotificationContent(on) => {
                let _ = sender.output(PrefOutput::SetNotificationContent(on));
            }
            PrefInput::ToggleAttachmentsRow(on) => {
                let _ = sender.output(PrefOutput::SetAttachmentsRow(on));
            }
            PrefInput::ToggleShowUnified(on) => {
                self.show_unified = on;
                let _ = sender.output(PrefOutput::SetShowUnified(on));
            }
            PrefInput::ToggleUnifiedChipAllInboxes(on) => {
                self.unified_chips.all_inboxes = on;
                let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
            }
            PrefInput::ToggleUnifiedChipStarred(on) => {
                self.unified_chips.starred = on;
                let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
            }
            PrefInput::ToggleUnifiedChipDrafts(on) => {
                self.unified_chips.drafts = on;
                let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
            }
            PrefInput::ToggleUnifiedChipArchive(on) => {
                self.unified_chips.archive = on;
                let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
            }
            PrefInput::ToggleUnifiedChipFiltered(on) => {
                self.unified_chips.filtered = on;
                let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
            }
            PrefInput::ToggleUnifiedFiltered(on) => {
                let _ = sender.output(PrefOutput::SetUnifiedFiltered(on));
            }
            PrefInput::ToggleUnifiedStarred(on) => {
                self.unified_kinds.starred = on;
                let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
            }
            PrefInput::ToggleUnifiedSent(on) => {
                self.unified_kinds.sent = on;
                let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
            }
            PrefInput::ToggleUnifiedDrafts(on) => {
                self.unified_kinds.drafts = on;
                let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
            }
            PrefInput::ToggleUnifiedArchive(on) => {
                self.unified_kinds.archive = on;
                let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
            }
            PrefInput::ToggleUnifiedTags(on) => {
                let _ = sender.output(PrefOutput::SetUnifiedTags(on));
            }
            PrefInput::MountPages => self.mount_pages(),
            PrefInput::SetAccountsPanel { panel, sender: accounts } => {
                self.accounts_sender = accounts;
                if let Some(slot) = &self.accounts_slot {
                    slot.set_child(Some(&panel));
                }
            }
            PrefInput::ToggleShowAccounts(on) => {
                if self.show_accounts != on {
                    self.show_accounts = on;
                    let _ = sender.output(PrefOutput::SetShowAccounts(on));
                }
            }
            PrefInput::ToggleFocus(part, on) => {
                if self.focus.get(part) != on {
                    self.focus.set(part, on);
                    let _ = sender.output(PrefOutput::SetFocusMode(self.focus));
                }
            }
            PrefInput::SetFocusMode(focus) => {
                self.focus = focus;
            }
            PrefInput::SetShowAccounts(on) => {
                self.show_accounts = on;
            }
            PrefInput::ChangeFilteredPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetFilteredPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeTagsPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetTagsPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeChevronSide(idx) => {
                let _ = sender.output(PrefOutput::SetChevronsLeft(idx == 0));
            }
            PrefInput::ToggleContactsRow(on) => {
                let _ = sender.output(PrefOutput::SetContactsRow(on));
            }
            PrefInput::ToggleSidebarHoverExpand(on) => {
                let _ = sender.output(PrefOutput::SetSidebarHoverExpand(on));
            }
            PrefInput::ToggleRememberSidebar(on) => {
                let _ = sender.output(PrefOutput::SetRememberSidebar(on));
            }
            PrefInput::ToggleRememberRail(on) => {
                let _ = sender.output(PrefOutput::SetRememberRail(on));
            }
            PrefInput::ToolbarDrop { key, side, index } => {
                if let Some(item) = ToolbarItem::from_key(&key) {
                    self.toolbar.place(item, side, index);
                    self.rebuild_toolbar_chips();
                    let _ = sender.output(PrefOutput::SetReaderToolbar(self.toolbar.clone()));
                }
            }
            PrefInput::ToolbarGapPreview { zone, index } => {
                if let Some(editor) = &self.toolbar_editor {
                    // A gap the size of the first chip found, as a drag of
                    // it would open.
                    if let Some(chip) = editor.zones.iter().find_map(|z| z.flow.first_child()) {
                        editor.drag_size.set((chip.width(), chip.height()));
                    }
                    if let Some(z) = editor.zones.get(zone) {
                        z.flow.add_css_class("drop-active");
                        z.flow.set_gap(Some(index));
                    }
                }
            }
            PrefInput::ToolbarRestore => {
                if self.toolbar != ReaderToolbar::default() {
                    self.toolbar = ReaderToolbar::default();
                    self.rebuild_toolbar_chips();
                    let _ = sender.output(PrefOutput::SetReaderToolbar(self.toolbar.clone()));
                }
            }
            PrefInput::ToggleRailDots(on) => {
                let _ = sender.output(PrefOutput::SetRailDots(on));
            }
            PrefInput::ToggleRailFoldEnabled(on) => {
                self.rail_fold.enabled = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldAccounts(on) => {
                self.rail_fold.accounts = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldAllInboxes(on) => {
                self.rail_fold.all_inboxes = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldStarred(on) => {
                self.rail_fold.starred = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldSent(on) => {
                self.rail_fold.sent = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldDrafts(on) => {
                self.rail_fold.drafts = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldArchive(on) => {
                self.rail_fold.archive = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldFiltered(on) => {
                self.rail_fold.filtered = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleRailFoldTags(on) => {
                self.rail_fold.tags = on;
                let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
            }
            PrefInput::ToggleSingleKey(on) => {
                let _ = sender.output(PrefOutput::SetSingleKey(on));
            }
            PrefInput::ToggleConsoleMode(on) => {
                let _ = sender.output(PrefOutput::SetConsoleMode(on));
            }
            PrefInput::ChangeReadMark(idx) => {
                let policy = match idx {
                    1 => crate::config::ReadMark::Delay,
                    2 => crate::config::ReadMark::Manual,
                    _ => crate::config::ReadMark::Shown,
                };
                let _ = sender.output(PrefOutput::SetReadMark(policy));
            }
            PrefInput::ExportLog => {
                let _ = sender.output(PrefOutput::ExportLog);
            }
            PrefInput::ExportSettings => {
                let _ = sender.output(PrefOutput::ExportSettings);
            }
            PrefInput::ImportSettings => {
                let _ = sender.output(PrefOutput::ImportSettings);
            }
            PrefInput::ToggleRunInBackground(on) => {
                let _ = sender.output(PrefOutput::SetRunInBackground(on));
            }
            PrefInput::ToggleAutostart(on) => {
                let _ = sender.output(PrefOutput::SetAutostart(on));
            }
            PrefInput::ToggleTray(on) => {
                let _ = sender.output(PrefOutput::SetTray(on));
            }
            PrefInput::ToggleTrayMail(on) => {
                let _ = sender.output(PrefOutput::SetTrayMail(on));
            }
            PrefInput::ChangeAppIcon(id) => {
                let _ = sender.output(PrefOutput::SetAppIcon(id));
            }
            PrefInput::ChangeTrayIcon(index) => {
                let icon = TRAY_ICONS
                    .get(index as usize)
                    .map(|(_, t)| *t)
                    .unwrap_or_default();
                let _ = sender.output(PrefOutput::SetTrayIcon(icon));
            }
            PrefInput::ChangePreviewLines(index) => {
                // The combo lists Off, then 1, 2 and 3 lines — so the row index is
                // the number of lines.
                let _ = sender.output(PrefOutput::SetPreviewLines(index));
            }
            PrefInput::ChangePaletteCollapse(secs) => {
                let _ = sender.output(PrefOutput::SetPaletteCollapse(secs));
            }
            PrefInput::ChangeCardPaletteCollapse(secs) => {
                let _ = sender.output(PrefOutput::SetCardPaletteCollapse(secs));
            }
            PrefInput::ChangeAppTheme(index) => {
                let theme = APP_THEMES
                    .get(index as usize)
                    .map(|(_, t)| *t)
                    .unwrap_or_default();
                let _ = sender.output(PrefOutput::SetAppTheme(theme));
            }
            PrefInput::ChangeTheme(id) => {
                let _ = sender.output(PrefOutput::SetTheme(id));
            }
            PrefInput::ChangeSettingsOpen(index) => {
                let _ = sender.output(PrefOutput::SetSettingsOpenAccounts(index == 1));
            }
            PrefInput::ShowAccounts(accounts) => {
                self.select_row(if accounts { "accounts" } else { "general" });
            }
            PrefInput::SelectPage(id) => {
                if self.editor_open && id != self.editor_page {
                    // The accounts panel's editors can tell whether anything
                    // was actually changed, and answer with either
                    // LeaveEditorTo or LeaveEditorPrompt. The cloud editor
                    // keeps no such record, so it is always asked about.
                    if self.editor_page == "cloud" {
                        self.ask_to_leave_editor(&id, &sender);
                    } else {
                        let _ = self
                            .accounts_sender
                            .send(crate::ui::accounts::AccountsInput::LeaveRequest(id));
                    }
                } else {
                    self.show_page(&id);
                    if id == "system" {
                        sender.input(PrefInput::NautilusRefresh);
                    }
                    let _ = sender.output(PrefOutput::PageShown(id));
                }
            }
            PrefInput::ShowPageById(id) => self.select_row(&id),
            PrefInput::LeaveEditorPrompt(id) => self.ask_to_leave_editor(&id, &sender),
            PrefInput::LeaveEditorTo(id) => {
                // The row the user clicked is already the selected one — the
                // click selected it before any of this — so re-selecting it
                // emits nothing and the page has to be shown outright.
                self.select_row(&id);
                self.show_page(&id);
                if id == "system" {
                    sender.input(PrefInput::NautilusRefresh);
                }
                let _ = sender.output(PrefOutput::PageShown(id));
            }
            PrefInput::EditorOpen(page) => {
                self.editor_open = page.is_some();
                self.editor_page = page.unwrap_or("accounts");
                if let Some(header) = &self.host_header {
                    header.set_visible(page.is_none());
                }
            }
            PrefInput::CloudEditorOpen(open) => {
                self.editor_open = open;
                self.editor_page = "cloud";
                if let Some(header) = &self.host_header {
                    header.set_visible(!open);
                }
            }
            PrefInput::ChangeMessageTheme(index) => {
                let theme = MESSAGE_THEMES
                    .get(index as usize)
                    .map(|(_, t)| *t)
                    .unwrap_or_default();
                let _ = sender.output(PrefOutput::SetMessageTheme(theme));
            }
            PrefInput::ToggleShowRemoteBanner(on) => {
                let _ = sender.output(PrefOutput::SetShowRemoteBanner(on));
            }
            PrefInput::ToggleOverrideFonts(on) => {
                let _ = sender.output(PrefOutput::SetOverrideFonts(on));
            }
            PrefInput::ChangeReaderFont(font) => {
                let _ = sender.output(PrefOutput::SetReaderFont(font));
            }
            PrefInput::ToggleOverrideColors(on) => {
                let _ = sender.output(PrefOutput::SetOverrideColors(on));
            }
            PrefInput::TogglePlainMonospace(on) => {
                let _ = sender.output(PrefOutput::SetPlainMonospace(on));
            }
            PrefInput::ChangePlainFont(font) => {
                let _ = sender.output(PrefOutput::SetPlainFont(font));
            }
            PrefInput::ChangeComposeFormat(idx) => {
                let format = match idx {
                    1 => crate::config::ComposeFormat::Markdown,
                    2 => crate::config::ComposeFormat::Html,
                    3 => crate::config::ComposeFormat::Plain,
                    _ => crate::config::ComposeFormat::Rich,
                };
                let _ = sender.output(PrefOutput::SetComposeFormat(format));
            }
            PrefInput::ChangeReplyPosition(idx) => {
                let position = match idx {
                    1 => crate::config::ReplyPosition::Bottom,
                    2 => crate::config::ReplyPosition::Follow,
                    _ => crate::config::ReplyPosition::Top,
                };
                let _ = sender.output(PrefOutput::SetReplyPosition(position));
            }
        }
    }
}


/// Rebuild the "Added words" expander's word rows from the personal word
/// list. The permanent "Add a word" entry row is left alone; word rows are
/// tagged with a widget name so only they are cleared. Each row's trash
/// button forgets its word and rebuilds.
fn rebuild_personal_words(exp: &adw::ExpanderRow) {
    // Collect the old word rows first: ExpanderRow has no child iterator, so
    // rows remember themselves via the widget name.
    let mut stale: Vec<gtk::Widget> = Vec::new();
    let mut child = exp.first_child();
    while let Some(c) = child {
        collect_named(&c, "vireo-spell-word", &mut stale);
        child = c.next_sibling();
    }
    for row in &stale {
        // The named widget is the ActionRow itself; ExpanderRow::remove
        // wants the row it was handed in add_row.
        exp.remove(row);
    }
    let words = crate::spell::personal_words();
    exp.set_enable_expansion(true);
    for w in words {
        let row = adw::ActionRow::builder().title(&w).build();
        row.set_widget_name("vireo-spell-word");
        let del = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk::Align::Center);
        del.set_tooltip_text(Some(i18n("Forget this word").as_str()));
        {
            let word = w.clone();
            let exp = exp.clone();
            del.connect_clicked(move |_| {
                crate::spell::remove_personal_word(&word);
                rebuild_personal_words(&exp);
            });
        }
        row.add_suffix(&del);
        exp.add_row(&row);
    }
}

/// Depth-first search for widgets with the given name under `root`.
fn collect_named(root: &gtk::Widget, name: &str, out: &mut Vec<gtk::Widget>) {
    if root.widget_name() == name {
        out.push(root.clone());
        return;
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        collect_named(&c, name, out);
        child = c.next_sibling();
    }
}

/// The placement combos' rows, in the order [`SectionPlacement`] lists them.
fn placement_index(p: crate::config::SectionPlacement) -> u32 {
    use crate::config::SectionPlacement::*;
    match p {
        AllInboxes => 0,
        AboveAccounts => 1,
        BelowAccounts => 2,
    }
}

fn placement_from_index(idx: u32) -> crate::config::SectionPlacement {
    use crate::config::SectionPlacement::*;
    match idx {
        1 => AboveAccounts,
        2 => BelowAccounts,
        _ => AllInboxes,
    }
}

/// Give a combo row's selected-value label `extra` more pixels than the
/// ellipsized width libadwaita allows it, so a sentence-length choice
/// ("Account of the current folder") reads whole instead of trailing off.
/// The value label is the row's only descendant label showing the
/// selected string; nothing else in the row is touched.
fn widen_combo_value(row: &adw::ComboRow, extra: i32) {
    let Some(want) = row
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string())
    else {
        return;
    };
    fn find(w: &gtk::Widget, want: &str) -> Option<gtk::Label> {
        if let Some(label) = w.downcast_ref::<gtk::Label>() {
            if label.label() == want {
                return Some(label.clone());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(hit) = find(&c, want) {
                return Some(hit);
            }
            child = c.next_sibling();
        }
        None
    }
    if let Some(label) = find(row.upcast_ref::<gtk::Widget>(), &want) {
        let (_, natural, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
        label.set_width_request(natural + extra);
    }
}
