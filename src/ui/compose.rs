//! Compose window: write a new message, reply, or forward.

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::Suggestion;
use crate::models::DraftOrigin;
use crate::config::ComposeFormat;
use crate::ui::rich_editor::{self, RichEditor, SourceKind, js_escape};
use crate::worker::OutgoingMessage;
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::ui::context_menu::{show_context_menu, MenuEntry};

/// Which recipient field a suggestion is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

/// A signature block as HTML (`-- ` delimiter). The stored signature is HTML
/// (legacy plain-text signatures are converted).
fn sig_html(sig: &str) -> String {
    let body = rich_editor::signature_to_html(sig);
    format!("<div class=\"vireo-sig\"><br>-- <br>{body}</div>")
}

/// The signature as it reads in a source-mode body: Markdown under
/// its `-- ` line, or the same HTML block the rich editor holds.
fn sig_source(kind: SourceKind, sig: &str) -> String {
    if sig.is_empty() {
        return String::new();
    }
    match kind {
        SourceKind::Html => format!("\n{}\n", sig_html(sig)),
        SourceKind::Markdown => format!(
            "\n\n-- \n{}\n",
            crate::markdown::from_html(&rich_editor::signature_to_html(sig))
        ),
    }
}

/// The icon a composing format carries, in the header and in its menu.
fn format_icon(format: ComposeFormat) -> &'static str {
    match format {
        ComposeFormat::Rich => "co.hyprlab.Hylki-format-text-rich-symbolic",
        ComposeFormat::Markdown => "co.hyprlab.Hylki-markdown-symbolic",
        ComposeFormat::Html => "co.hyprlab.Hylki-code-symbolic",
        ComposeFormat::Plain => "co.hyprlab.Hylki-text-x-generic-symbolic",
    }
}

/// What a format is called where the user meets it.
fn format_label(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Rich text"),
        ComposeFormat::Markdown => i18n("Markdown"),
        ComposeFormat::Html => i18n("HTML"),
        ComposeFormat::Plain => i18n("Plain text"),
    }
}

/// The one-line explanation under each format in the menu.
fn format_hint(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Format as you type, with the toolbar"),
        ComposeFormat::Markdown => i18n("Write Markdown; it is sent as formatted mail"),
        ComposeFormat::Html => i18n("Write the message's HTML by hand"),
        ComposeFormat::Plain => i18n("No formatting at all"),
    }
}

/// The narrowest the compose pane goes (its `adw::BreakpointBin` floor).
const COMPOSE_MIN_WIDTH: i32 = 360;
/// Fallback fold threshold when the toolbar could not be measured at init.
const COMPOSE_ACTIONS_BREAKPOINT: f64 = 620.0;

/// The width a header bar's rows need side by side: the natural widths of
/// its centre box's children (start row, title, end row) summed — what the
/// bar itself reports doubles the wider side to keep the title centred.
/// `None` when the bar's insides are not the expected shape.
fn header_rows_width(header: &gtk::Widget) -> Option<i32> {
    fn find_center_box(w: &gtk::Widget, depth: u32) -> Option<gtk::Widget> {
        if w.type_().name() == "GtkCenterBox" {
            return Some(w.clone());
        }
        if depth == 0 {
            return None;
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = find_center_box(&child, depth - 1) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    let cb = find_center_box(header, 6)?;
    let mut total = 0;
    let mut c = cb.first_child();
    while let Some(w) = c {
        total += w.measure(gtk::Orientation::Horizontal, -1).1;
        c = w.next_sibling();
    }
    Some(total)
}

/// Size the pane for its host. Both hosts impose a definite height now — the
/// reader-covering overlay inline (it fills the whole pane), the window itself
/// popped out — so the editor always expands to fill whatever it is given.
/// Inline, the compose header also stands in for the reader's (which it
/// covers, or tops), so it takes over the GNOME window decorations — unless
/// `decorations` is off: a split reply beneath the reader leaves the reader's
/// own header in place, and a second set of controls mid-pane would be wrong.
fn size_for_host(
    root: &adw::ToolbarView,
    header: &adw::HeaderBar,
    editor_holder: &gtk::Box,
    windowed: bool,
    decorations: bool,
) {
    root.set_vexpand(true);
    editor_holder.set_vexpand(true);
    editor_holder.set_height_request(-1);
    header.set_show_end_title_buttons(!windowed && decorations);
}

/// Set the inline/window toggle button's icon + tooltip for the current host.
fn set_toggle_icon(btn: &gtk::Button, windowed: bool) {
    if windowed {
        btn.set_icon_name("co.hyprlab.Hylki-view-restore-symbolic");
        btn.set_tooltip_text(Some(i18n("Collapse into reader").as_str()));
    } else {
        btn.set_icon_name("co.hyprlab.Hylki-view-fullscreen-symbolic");
        btn.set_tooltip_text(Some(i18n("Open in window").as_str()));
    }
}

/// Replace the recipient currently being typed (after the last comma) with the
/// chosen suggestion, leaving a trailing ", " ready for the next recipient.
fn complete_field(row: &adw::EntryRow, sug: &Suggestion) {
    let text = row.text().to_string();
    let prefix_len = text.rfind(',').map(|i| i + 1).unwrap_or(0);
    let prefix = &text[..prefix_len];
    row.set_text(&format!("{}{}, ", prefix, sug.display()));
    row.set_position(-1);
}

/// One selectable "from" account in the compose window.
#[derive(Debug, Clone)]
pub struct ComposeAccount {
    pub id: u32,
    pub label: String,
    /// Signature text appended to the body when this account is selected.
    pub signature: String,
    /// The identity's sending address (the account's own, or an alias's).
    pub email: String,
    /// The account's chosen OpenPGP key (fingerprint), if any (#133).
    pub pgp_key: Option<String>,
    /// Set for a send-as alias (#34): the full From to put on the wire
    /// ("Name <alias@host>"). `None` sends as the account itself.
    pub alias_from: Option<String>,
}

/// Initial field contents (empty for a new message; populated for reply/forward).
#[derive(Debug, Default)]
pub struct ComposePrefill {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// HTML prefill placed into the rich editor (e.g. a quoted reply/forward).
    pub body_html: String,
    /// Files to attach on open, already on disk (a queued message's attachments
    /// are written out before its composer opens).
    pub attachments: Vec<std::path::PathBuf>,
    /// Threading headers for a reply: the parent's Message-ID, and the thread's
    /// id chain. Both stored bare (no angle brackets).
    pub in_reply_to: String,
    pub references: String,
    /// When editing an existing draft, its origin (so saving/sending replaces it).
    pub draft_origin: Option<DraftOrigin>,
    /// Start with Encrypt (and so Sign) on: a reply to an encrypted message (#133).
    pub encrypt: bool,
    /// When editing a queued Outbox message, the row this replaces.
    pub outbox_origin: Option<u32>,
    /// For a reply: the original's To+Cc, so the composer can answer from the
    /// alias the mail was addressed to (#34). Empty otherwise.
    pub reply_addressed_to: String,
    /// For a new message: the address to send from when the user has chosen
    /// a default identity in Settings (#157). Empty = the account's own
    /// address. Ignored when `reply_addressed_to` names an identity.
    pub from_address: String,
    /// Send Later (#145): a queued message's scheduled time, kept while it is
    /// edited so Send re-queues it for the same moment.
    pub send_at: Option<i64>,
    /// Files handed in from GNOME Files that were too big to attach: the
    /// composer opens its cloud upload dialog on them as soon as it is up,
    /// so the links land in the body instead.
    pub cloud_uploads: Vec<std::path::PathBuf>,
}

/// Everything the compose pane needs to open.
#[derive(Debug)]
pub struct ComposeInit {
    /// Stable id so the app can track this composer across inline/window moves.
    pub compose_id: u32,
    pub prefill: ComposePrefill,
    pub accounts: Vec<ComposeAccount>,
    /// Index into `accounts` of the account to send from by default.
    pub selected: usize,
    /// Recipient autocomplete suggestions (Contacts + mail history).
    pub suggestions: Vec<Suggestion>,
    /// Whether the pane starts hosted in a standalone window (vs. inline).
    pub windowed: bool,
    /// Whether the inline/window toggle button is offered (reply/forward only).
    pub can_toggle: bool,
    /// Compact reply (#86 follow-up): the split reply hides every address/
    /// subject row and shows just the editor — popping out to a window brings
    /// the full fields back.
    pub compact: bool,
    /// Whether the inline header carries the window decorations. Off for a
    /// split reply placed beneath the reader (#212), whose header stays.
    pub decorations: bool,
    /// What the message is written in: rich text, Markdown, HTML
    /// source, or plain text.
    pub format: ComposeFormat,
}

pub struct Compose {
    accounts: Vec<ComposeAccount>,
    /// The rich-text (HTML) body editor.
    editor: RichEditor,
    /// Signature currently appended to the body (so it can be swapped out).
    current_sig: String,
    /// Files to attach.
    attachments: Vec<std::path::PathBuf>,
    /// When editing a queued Outbox message, the row this replaces once sent.
    outbox_origin: Option<u32>,
    /// Recipient suggestions, filtered as the user types.
    suggestions: Vec<Suggestion>,
    /// Shared autocomplete popover and which field it's currently attached to.
    completion: gtk::Popover,
    completion_field: Option<Field>,
    /// The list inside the popover + the keyboard-highlighted row, for arrow-key nav.
    completion_list: Option<gtk::ListBox>,
    completion_selected: usize,
    completion_count: usize,
    /// Whether the popover is showing (read synchronously by the key handler).
    completion_open: std::rc::Rc<std::cell::Cell<bool>>,
    /// Threading headers carried from the message being replied to.
    in_reply_to: String,
    references: String,
    /// When editing an existing draft, its origin (replaced on save/send).
    draft_origin: Option<DraftOrigin>,
    /// Stable id the app uses to track this composer across host moves.
    compose_id: u32,
    /// Currently shown as a standalone window (drives the toggle-button icon).
    windowed: bool,
    /// Whether this composer offers the inline/window toggle at all.
    can_toggle: bool,
    compact: bool,
    /// Whether the inline header carries the window decorations (see
    /// `ComposeInit::decorations`).
    decorations: bool,
    /// A compact reply's field rows, revealed by the header button (#154)
    /// or from the start by the preference.
    fields_shown: bool,
    /// The pane is too narrow for the full toolbar: everything but Cancel,
    /// Send and the fields chevron folds into the ⋯ menu (like the reader's
    /// header), so the window controls at the end never leave the canvas.
    narrow: bool,
    /// A recipient/subject field was edited since open (body edits are tracked
    /// separately by the editor itself). Used for save-if-dirty.
    fields_dirty: bool,
    /// OpenPGP (#133): sign the message; encrypt it to every recipient.
    sign: bool,
    encrypt: bool,
    /// What this message is written in. Plain text hides the
    /// formatting toolbar and sends no HTML part (#180); Markdown and HTML
    /// are written as source and converted on the way out.
    format: ComposeFormat,
    /// The format chooser and preview toggle, which live at the end of the
    /// editor's formatting row rather than in the header.
    format_btn: gtk::Button,
    preview_btn: gtk::ToggleButton,
    /// The preview toggle's icon-and-label insides, kept only so the label
    /// can be dropped in a pane too narrow to carry it.
    preview_content: adw::ButtonContent,
    /// Send Later (#145): when set, Send queues the message for this time.
    send_at: Option<i64>,
    /// Cloud attachments (#144): the accounts files can be uploaded to, the
    /// links already placed in the body, and how many uploads are running.
    cloud_accounts: Vec<crate::cloud::CloudAccount>,
    cloud_links: Vec<CloudLink>,
    cloud_busy: u32,
    /// Download passwords made for this message's links, to pass on
    /// separately: (file name, password).
    cloud_passwords: Vec<(String, String)>,
    /// The composer's own undo history (#200): the body's typing and
    /// formatting, and the attachments alongside it. Ctrl+Z takes the top
    /// one, applies it, and puts what reverses it onto `redo_stack` — the
    /// same shape as the message list's history. See
    /// [`Compose::push_history`] for the one place the two kinds of change
    /// are ordered against each other.
    undo_stack: Vec<ComposeUndoEntry>,
    redo_stack: Vec<ComposeUndoEntry>,
}

/// One entry of the composer's history: a thing to *do*, and what the user
/// called the change it takes back, so the menu can say "Undo Typing".
#[derive(Debug)]
struct ComposeUndoEntry {
    step: ComposeStep,
    what: String,
}

/// A reversible change inside the composer.
#[derive(Debug)]
enum ComposeStep {
    /// The body's own text history. WebKit owns the text — every edit,
    /// every formatting command, every paste and every dropped image is in
    /// *its* stack, which cannot be read, counted or spliced. This marker
    /// stands in for all of it: while the marker is on top and the editor
    /// still has something to take back, each step asks the editor for one
    /// more; once the editor has run out the marker crosses to the other
    /// stack and the change underneath comes up.
    ///
    /// There is never more than one on a stack, and it is kept on top.
    Body,
    /// Put these files back among the attachments at `at`.
    Insert { at: usize, paths: Vec<std::path::PathBuf> },
    /// Take `count` attachments out again, starting at `at`.
    Remove { at: usize, count: usize },
}

/// A share link placed in the body (#144), by the id of its paragraph.
#[derive(Clone, Debug)]
struct CloudLink {
    id: String,
    name: String,
    url: String,
}

#[derive(Debug)]
pub enum ComposeInput {
    Send,
    /// Send Later (#145): queue for this unix time, then send as usual.
    SendAt(i64),
    /// Open the date-and-time picker.
    PickSendTime,
    /// Forget the scheduled time: Send goes out at once again.
    ClearSendAt,
    /// A compact reply shows or hides its From/To/Subject rows (#154).
    ShowFields(bool),
    /// The pane crossed its width breakpoint: fold the toolbar into the ⋯
    /// menu, or spread it out again.
    SetNarrow(bool),
    /// The folded toolbar's ⋯ button: the actions as a menu.
    OverflowMenu,
    /// Cloud attachments (#144): pick files to upload and share.
    CloudAttach,
    /// Files picked; ask which account and how, then upload.
    CloudPicked(Vec<std::path::PathBuf>),
    /// Upload these files to `account`, whose `expire_days` and `password`
    /// carry the user's choices for this upload; `link_password` is a
    /// password of their own for every link, else one is generated per
    /// file.
    CloudUpload { paths: Vec<std::path::PathBuf>, account: crate::cloud::CloudAccount, link_password: Option<String> },
    /// One upload finished (in a thread): the link, or why not.
    CloudUploaded { name: String, result: Result<crate::cloud::ShareResult, String> },
    /// Take a link back out of the body.
    RemoveCloudLink(usize),
    CopyCloudPasswords,
    /// Move the draft being edited to Trash and close without saving.
    DeleteDraft,
    /// The OpenPGP Sign toggle (#133).
    ToggleSign(bool),
    /// Pick what this message is written in.
    SetFormat(ComposeFormat),
    /// The header's format button: the four formats as a menu.
    FormatMenu,
    /// The body came back for a format change; put it in the new one.
    LoadAs { from: ComposeFormat, to: ComposeFormat, body: String },
    /// Show or hide the rendered preview of a source message.
    TogglePreview(bool),
    /// The source came back for the preview; render it.
    ShowPreview(String),
    /// The OpenPGP Encrypt toggle; encrypting turns signing on too.
    ToggleEncrypt(bool),
    /// The editor's HTML + plain text came back asynchronously — finish sending.
    SendBody { html: String, text: String, to: String, cc: String, bcc: String, reply_to: String, subject: String, from_account_id: u32, from_alias: Option<String> },
    /// Save the current message to Drafts.
    SaveDraft,
    /// The editor content came back — finish saving the draft.
    SaveDraftBody { html: String, text: String, to: String, cc: String, bcc: String, reply_to: String, subject: String, from_account_id: u32, from_alias: Option<String> },
    Cancel,
    /// The user clicked the inline/window toggle button.
    ToggleWindowed,
    /// The app moved this pane between inline and window; sync the button icon.
    SetWindowed(bool),
    /// Whether the inline header should carry the window decorations: off
    /// while the pane sits beneath the reader, whose header stays (#212).
    SetDecorations(bool),
    /// Re-grab keyboard focus into the editor (after a host move).
    FocusEditor,
    /// A recipient/subject field changed — mark dirty.
    MarkFieldsDirty,
    /// Save to Drafts only if edited, then close (used when superseded / on nav).
    SaveDraftIfDirty,
    AccountChanged,
    AttachFiles,
    AddAttachments(Vec<std::path::PathBuf>),
    RemoveAttachment(usize),
    OpenContacts,
    /// The given recipient field changed — refresh autocomplete.
    Suggest(Field),
    /// Addresses just sent to from another composer: into this one's
    /// suggestions at once, without waiting for a reopen.
    AddSuggestions(Vec<Suggestion>),
    /// Arrow-key move of the autocomplete highlight (+1 down, -1 up).
    CompletionMove(i32),
    /// Accept the highlighted suggestion into the active field.
    CompletionAccept,
    /// Dismiss the autocomplete popover.
    CompletionClose,
    /// Ctrl+Z / Ctrl+Shift+Z (#200): step the composer's history back or
    /// forward — the body's text and the attachments, in one order.
    History { redo: bool },
    /// The body's text history changed: WebKit gained or lost a step. The
    /// flag is whether it now has anything to take back.
    BodyHistory(bool),
    /// Showcase only (HYLKI_SHOWCASE_COMPOSE_UNDO): one step of the scripted
    /// history check. See [`Compose::showcase_history`].
    ShowcaseHistory(u8),
}

#[derive(Debug)]
pub enum ComposeOutput {
    Send(Box<OutgoingMessage>),
    /// Save the message to the Drafts folder (no send).
    SaveDraft(Box<OutgoingMessage>),
    /// Delete the draft this composer was opened from, and close it. The app
    /// moves the draft to Trash (undoable, like deleting it from the list).
    DeleteDraft { id: u32, origin: DraftOrigin },
    /// Ask the app to promote/demote this pane (inline ↔ window). Carries the id.
    ToggleWindow(u32),
    /// What this composer's history can do now, so the window that hosts it
    /// inline can label and enable its own Undo and Redo entries from the
    /// composer while the composer has focus (#200). `None` means that
    /// direction is empty.
    History { id: u32, undo: Option<String>, redo: Option<String> },
    /// This pane is done (cancelled / sent / draft-saved / superseded). Carries
    /// the id so the app tears down the right host.
    Close(u32),
}

#[relm4::component(pub)]
impl Component for Compose {
    type Init = ComposeInit;
    type Input = ComposeInput;
    type Output = ComposeOutput;
    type CommandOutput = ();

    view! {
        // Host-agnostic root: the same pane is shown inline (in a reader Revealer)
        // or set as the content of an app-owned window. Hosting/close is the app's
        // job (see ComposeOutput::ToggleWindow / Close).
        adw::BreakpointBin {
            // The floor the pane can shrink to (inline in a narrow reader,
            // or a small window): below the measured full-toolbar width the
            // breakpoint set in `init` folds the actions into the ⋯ menu.
            set_size_request: (COMPOSE_MIN_WIDTH, 200),

            #[wrap(Some)]
            #[name = "toolbar_root"]
            set_child = &adw::ToolbarView {
                #[name = "header"]
                add_top_bar = &adw::HeaderBar {
                    add_css_class: "compose-toolbar",
                    set_show_start_title_buttons: false,
                    set_show_end_title_buttons: false,
                    // No "Hylki" branding on the compose bar.
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        set_label: "",
                    },

                    pack_start = &gtk::Button {
                        set_label: &i18n("Cancel"),
                        connect_clicked => ComposeInput::Cancel,
                    },
                    // Save Draft stays, label and all, however narrow the
                    // pane: it is the one action worth a click in a hurry.
                    pack_start = &gtk::Button {
                        set_label: &i18n("Save Draft"),
                        set_tooltip_text: Some(i18n("Save to Drafts").as_str()),
                        connect_clicked => ComposeInput::SaveDraft,
                    },
                    // Only while editing an existing draft: the message is
                    // moved to Trash, not saved, and the editor closes.
                    pack_start = &gtk::Button {
                        set_label: &i18n("Delete Draft"),
                        set_tooltip_text: Some(i18n("Move this draft to Trash").as_str()),
                        #[watch]
                        set_visible: model.draft_origin.is_some() && !model.narrow,
                        connect_clicked => ComposeInput::DeleteDraft,
                    },
                    // Send, with Send Later beside it (#145): presets, or a
                    // date and time of your own.
                    pack_end = &gtk::Box {
                        add_css_class: "linked",
                        add_css_class: "send-split",
                        gtk::Button {
                            #[watch]
                            set_label: &if model.send_at.is_some() { i18n("Schedule") } else { i18n("Send") },
                            add_css_class: "suggested-action",
                            connect_clicked => ComposeInput::Send,
                        },
                        // A floating divider, not a seam: the box paints the
                        // accent behind it so the two read as one control.
                        gtk::Separator {
                            set_orientation: gtk::Orientation::Vertical,
                        },
                        gtk::MenuButton {
                            set_icon_name: "co.hyprlab.Hylki-pan-down-symbolic",
                            add_css_class: "suggested-action",
                            set_tooltip_text: Some(i18n("Send later").as_str()),
                            set_can_focus: false,
                            #[wrap(Some)]
                            set_popover = &gtk::Popover {
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 2,
                                    // Menu rows, not buttons: regular weight
                                    // like every other popover menu (#167).
                                    add_css_class: "context-menu-list",
                                    gtk::Button {
                                        add_css_class: "flat",
                                        add_css_class: "context-menu-item",
                                        set_halign: gtk::Align::Fill,
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Send now"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::ClearSendAt);
                                            sender.input(ComposeInput::Send);
                                        },
                                    },
                                    gtk::Separator {},
                                    gtk::Button {
                                        add_css_class: "flat",
                                        add_css_class: "context-menu-item",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n_f("Tomorrow morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(preset_time(1, 8)));
                                        },
                                    },
                                    gtk::Button {
                                        add_css_class: "flat",
                                        add_css_class: "context-menu-item",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n_f("Tomorrow afternoon ({time})", &[("time", &crate::datefmt::clock_label(13))]), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(preset_time(1, 13)));
                                        },
                                    },
                                    gtk::Button {
                                        add_css_class: "flat",
                                        add_css_class: "context-menu-item",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n_f("Monday morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(next_monday(8)));
                                        },
                                    },
                                    gtk::Separator {},
                                    gtk::Button {
                                        add_css_class: "flat",
                                        add_css_class: "context-menu-item",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Pick a date and time…"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::PickSendTime);
                                        },
                                    },
                                },
                            },
                        },
                    },
                    // The folded toolbar (narrow pane): every action that is
                    // not Cancel or Send lives in this menu.
                    #[name = "overflow_btn"]
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-view-more-horizontal-symbolic",
                        set_tooltip_text: Some(i18n("Actions").as_str()),
                        #[watch]
                        set_visible: model.narrow,
                        connect_clicked => ComposeInput::OverflowMenu,
                    },
                    // OpenPGP (#133): only offered where a gpg exists.
                    #[name = "encrypt_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Hylki-channel-secure-symbolic",
                        set_tooltip_text: Some(i18n("Encrypt with OpenPGP to every recipient's key").as_str()),
                        #[watch]
                        set_visible: crate::pgp::available() && !model.narrow,
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ToggleEncrypt(b.is_active()));
                        },
                    },
                    #[name = "sign_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Hylki-security-high-symbolic",
                        set_tooltip_text: Some(i18n("Sign with your OpenPGP key").as_str()),
                        #[watch]
                        set_visible: crate::pgp::available() && !model.narrow,
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ToggleSign(b.is_active()));
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-attachment-symbolic",
                        set_tooltip_text: Some(i18n("Attach files").as_str()),
                        #[watch]
                        set_visible: !model.narrow,
                        connect_clicked => ComposeInput::AttachFiles,
                    },
                    // Cloud attachments (#144): only with an account set up.
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-cloud-symbolic",
                        set_tooltip_text: Some(i18n("Upload to cloud storage and share a link").as_str()),
                        #[watch]
                        set_visible: !model.cloud_accounts.is_empty() && !model.narrow,
                        #[watch]
                        set_sensitive: model.cloud_busy == 0,
                        connect_clicked => ComposeInput::CloudAttach,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-x-office-address-book-symbolic",
                        set_tooltip_text: Some(i18n("Open Contacts").as_str()),
                        #[watch]
                        set_visible: !model.narrow,
                        connect_clicked => ComposeInput::OpenContacts,
                    },
                    // Promote inline reply → window, or collapse window → inline.
                    // Icon set in `init` and on SetWindowed.
                    #[name = "toggle_btn"]
                    pack_end = &gtk::Button {
                        set_tooltip_text: Some(i18n("Open in window").as_str()),
                        #[watch]
                        set_visible: model.can_toggle && !model.narrow,
                        connect_clicked => ComposeInput::ToggleWindowed,
                    },
                    // The compact reply's From/To/Subject rows (#154): folded
                    // away by default, one press brings them back.
                    #[name = "fields_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Hylki-pan-down-symbolic",
                        add_css_class: "fields-chevron",
                        set_tooltip_text: Some(i18n("Show From, To and Subject").as_str()),
                        set_can_focus: false,
                        #[watch]
                        set_visible: model.compact && !model.windowed,
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ShowFields(b.is_active()));
                        },
                    },
                },
                // Send Later (#145): says when a scheduled message goes, with a
                // way back to sending at once.
                add_top_bar = &gtk::Box {
                    add_css_class: "schedule-bar",
                    set_spacing: 8,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_top: 4,
                    set_margin_bottom: 4,
                    #[watch]
                    set_visible: model.send_at.is_some(),
                    gtk::Image { set_icon_name: Some("co.hyprlab.Hylki-alarm-symbolic") },
                    gtk::Label {
                        set_hexpand: true,
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        #[watch]
                        set_label: &model.send_at.map(|t| i18n_f("Scheduled for {when}", &[("when", &crate::datefmt::date_time(t))])).unwrap_or_default(),
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        set_label: &i18n("Send now instead"),
                        connect_clicked => ComposeInput::ClearSendAt,
                    },
                },
                // Cloud attachments (#144): the download passwords, which
                // stay out of the message and go to the recipient some other way.
                add_top_bar = &gtk::Box {
                    set_spacing: 8,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_top: 4,
                    set_margin_bottom: 4,
                    #[watch]
                    set_visible: !model.cloud_passwords.is_empty(),
                    gtk::Image { set_icon_name: Some("co.hyprlab.Hylki-dialog-password-symbolic") },
                    gtk::Label {
                        set_hexpand: true,
                        set_halign: gtk::Align::Start,
                        set_wrap: true,
                        set_selectable: true,
                        #[watch]
                        set_label: &model.cloud_passwords.iter().map(|(n, p)| i18n_f("Download password for {name}: {password}", &[("name", n), ("password", p)])).collect::<Vec<_>>().join("\n"),
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        set_label: &i18n("Copy"),
                        connect_clicked => ComposeInput::CopyCloudPasswords,
                    },
                },

                #[wrap(Some)]
                set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,
                    add_css_class: "compose-pane",

                    // From and To are always offered, inline included — a
                    // forward is unaddressable without To (#25, #52). Cc, Bcc,
                    // and (for replies/forwards) the prefilled Subject wait
                    // behind the To row's "More" button; per-row visibility is
                    // set in `init`.
                    #[name = "fields_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        add_css_class: "compose-fields",
                        set_selection_mode: gtk::SelectionMode::None,

                        #[name = "from_row"]
                        adw::ComboRow {
                            set_title: &i18n("From"),
                            connect_selected_notify => ComposeInput::AccountChanged,
                        },
                        #[name = "to_row"]
                        adw::EntryRow {
                            set_title: &i18n("To"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "cc_row"]
                        adw::EntryRow {
                            set_title: &i18n("Cc"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "bcc_row"]
                        adw::EntryRow {
                            set_title: &i18n("Bcc"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "reply_to_row"]
                        adw::EntryRow {
                            set_title: &i18n("Reply-To"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "subject_row"]
                        adw::EntryRow {
                            set_title: &i18n("Subject"),
                        },
                    },

                    #[name = "attach_box"]
                    gtk::FlowBox {
                        set_selection_mode: gtk::SelectionMode::None,
                        set_column_spacing: 6,
                        set_row_spacing: 6,
                        set_max_children_per_line: 4,
                        set_visible: false,
                    },

                    // Holder for the shared rich-text editor (toolbar + body),
                    // appended in `init`.
                    #[name = "editor_holder"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_vexpand: true,
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
        let ComposeInit {
            compose_id,
            prefill,
            accounts,
            selected,
            suggestions,
            windowed,
            can_toggle,
            compact,
            decorations,
            format,
        } = init;
        let in_reply_to = prefill.in_reply_to.clone();
        let references = prefill.references.clone();
        let draft_origin = prefill.draft_origin.clone();
        let outbox_origin = prefill.outbox_origin;
        let prefill_attachments = prefill.attachments.clone();
        let prefill_encrypt = prefill.encrypt;
        let send_at = prefill.send_at;
        let current_sig = accounts.get(selected).map(|a| a.signature.clone()).unwrap_or_default();

        let completion = gtk::Popover::new();
        completion.set_autohide(false); // don't steal focus from the entry
        completion.set_can_focus(false);
        completion.set_position(gtk::PositionType::Bottom);
        completion.add_css_class("menu");

        // Initial editor content: a blank line to type on, the quoted
        // reply/forward (if any), then the signature.
        let mut content = String::from("<div><br></div>");
        if !prefill.body_html.is_empty() {
            content.push_str(&prefill.body_html);
        }
        // A draft already contains its signature; don't add another.
        if draft_origin.is_none() && !current_sig.is_empty() {
            content.push_str(&sig_html(&current_sig));
        }
        let editor = RichEditor::new(&content);
        editor.set_formatting_visible(format == ComposeFormat::Rich);
        // A source format starts from the same content, written out as
        // source: a reply's quoted original becomes `> ` lines in Markdown,
        // or the HTML it already was.
        match format {
            ComposeFormat::Markdown => {
                editor.set_source(SourceKind::Markdown, &crate::markdown::from_html(&content))
            }
            ComposeFormat::Html => {
                editor.set_source(SourceKind::Html, &crate::markdown::pretty_html(&content))
            }
            ComposeFormat::Rich | ComposeFormat::Plain => {}
        }
        // The format chooser and the preview toggle belong with the other
        // formatting controls, at the far end of the same row: a header
        // button for them folded away exactly when the pane was narrow, and
        // the format is a property of the body, not of the window.
        // Icon alone: it sits at the end of a row of icons, and the label
        // it wore for a while read as a second toolbar rather than as one
        // more control on the same one. The tooltip still names the format.
        let format_btn = gtk::Button::from_icon_name(format_icon(format));
        format_btn.set_tooltip_text(Some(
            i18n_f("Writing in {format}", &[("format", &format_label(format))]).as_str(),
        ));
        format_btn.add_css_class("flat");
        format_btn.set_can_focus(false);
        // The preview keeps its word. It is a state rather than an action,
        // and an eye on its own leaves which state to guesswork.
        let preview_content = adw::ButtonContent::builder()
            .icon_name("co.hyprlab.Hylki-eye-open-negative-filled-symbolic")
            .label(i18n("Preview"))
            .build();
        let preview_btn = gtk::ToggleButton::builder().child(&preview_content).build();
        preview_btn.set_tooltip_text(Some(i18n("Show the message as it will be sent").as_str()));
        preview_btn.add_css_class("flat");
        preview_btn.set_can_focus(false);
        preview_btn.set_can_shrink(true);
        preview_btn.set_visible(format.is_source());
        {
            let s = sender.input_sender().clone();
            format_btn.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::FormatMenu);
            });
            let s = sender.input_sender().clone();
            preview_btn.connect_toggled(move |b| {
                let _ = s.send(ComposeInput::TogglePreview(b.is_active()));
            });
        }
        // The chooser goes last, which in a right-aligned group is the far
        // end of the row: it is there in every format, so it is the one
        // that must not move. Preview comes and goes beside it, to its
        // left, where an appearing button pushes nothing around.
        editor.toolbar_end().append(&preview_btn);
        editor.toolbar_end().append(&format_btn);

        // "Send as Attachment Instead" on an inline image: the editor lifts
        // it to a temp file and it joins the attachment chips here.
        {
            let s = sender.input_sender().clone();
            editor.connect_send_as_attachment(move |path| {
                let _ = s.send(ComposeInput::AddAttachments(vec![path]));
            });
        }

        // The body's text history joins the composer's order (#200): every
        // edit in there is one step, wherever the attachments' steps fall.
        {
            let s = sender.input_sender().clone();
            editor.connect_history_changed(move |can_undo, _| {
                let _ = s.send(ComposeInput::BodyHistory(can_undo));
            });
        }

        let model = Compose {
            accounts,
            editor,
            current_sig,
            attachments: prefill_attachments,
            suggestions,
            completion,
            completion_field: None,
            completion_list: None,
            completion_selected: 0,
            completion_count: 0,
            completion_open: std::rc::Rc::new(std::cell::Cell::new(false)),
            in_reply_to,
            references,
            draft_origin,
            outbox_origin,
            compose_id,
            windowed,
            can_toggle,
            // A compact (fields-hidden) pane only makes sense once it is
            // addressed: replies arrive with To filled, forwards do not.
            compact: compact && !prefill.to.trim().is_empty(),
            decorations,
            fields_shown: crate::config::load_reply_fields(),
            narrow: false,
            fields_dirty: false,
            sign: false,
            format,
            format_btn,
            preview_btn,
            preview_content,
            encrypt: false,
            send_at,
            cloud_accounts: crate::cloud::load_enabled_accounts(),
            cloud_links: Vec::new(),
            cloud_busy: 0,
            cloud_passwords: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        };
        let widgets = view_output!();
        if prefill_encrypt && crate::pgp::available() {
            // Through the buttons, so the toggles and the model agree.
            widgets.encrypt_btn.set_active(true);
        }
        widgets.editor_holder.append(&model.editor.widget);

        // The inline/window toggle: only reply/forward panes can toggle. Its icon
        // reflects the current host (fullscreen = "expand to window", restore =
        // "collapse back inline").
        set_toggle_icon(&widgets.toggle_btn, model.windowed);
        size_for_host(
            &widgets.toolbar_root,
            &widgets.header,
            &widgets.editor_holder,
            model.windowed,
            model.decorations,
        );

        // Fold the toolbar once the pane is narrower than the full row (as
        // the reader's header does). Measured on the first map — before
        // anything is hidden, and once the header has a window, since the
        // window controls it carries inline only exist (and measure) with
        // one. A later host move changes that by a few dozen pixels; the
        // first host's threshold is kept, which only ever folds early.
        {
            let s = sender.clone();
            let header: gtk::Widget = widgets.header.clone().upcast();
            let measured = std::rc::Rc::new(std::cell::Cell::new(false));
            root.connect_map(move |root| {
                if measured.replace(true) {
                    return;
                }
                // Not the bar's own natural width: a header bar keeps its
                // title centred, so it asks for twice its wider side, and
                // this bar's end row is much wider than its start — every
                // button added there counted double and the fold came far
                // too soon. The centre box's rows, summed, are what has to
                // fit.
                let full = header.measure(gtk::Orientation::Horizontal, -1).1;
                let rows = header_rows_width(&header);
                tracing::debug!("compose fold: header natural {full}, rows {rows:?}");
                let need = rows.unwrap_or(full);
                let threshold = if need <= 0 { COMPOSE_ACTIONS_BREAKPOINT } else { need as f64 + 24.0 };
                let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    threshold,
                    adw::LengthUnit::Px,
                ));
                let s1 = s.clone();
                bp.connect_apply(move |_| s1.input(ComposeInput::SetNarrow(true)));
                let s2 = s.clone();
                bp.connect_unapply(move |_| s2.input(ComposeInput::SetNarrow(false)));
                root.add_breakpoint(bp);
            });
        }

        // Per-row visibility (#25): To always; Cc/Bcc only when prefilled (a
        // reply-all carries Cc). The Subject is always shown — replies and
        // forwards arrive with it prefilled, but it stays the user's to see
        // and change (2026-08-31).
        let cc_shown = !prefill.cc.trim().is_empty();
        let bcc_shown = !prefill.bcc.trim().is_empty();
        widgets.cc_row.set_visible(cc_shown);
        widgets.bcc_row.set_visible(bcc_shown);
        // Reply-To (#58) is rare enough to always start hidden behind "More".
        widgets.reply_to_row.set_visible(false);
        widgets.subject_row.set_visible(true);
        // Compact split reply: only the editor shows; the full field rows
        // return when the composer pops out to a window. Never for a pane
        // that arrives unaddressed — a forward — which needs its To row
        // (#139).
        widgets.fields_list.set_visible(!model.compact || model.fields_shown);
        widgets.fields_btn.set_active(model.fields_shown);
        {
            let more = gtk::Button::with_label(&i18n("More"));
            more.add_css_class("flat");
            more.set_valign(gtk::Align::Center);
            more.set_tooltip_text(Some(i18n("Show Cc, Bcc and Reply-To").as_str()));
            let cc = widgets.cc_row.clone();
            let bcc = widgets.bcc_row.clone();
            let reply_to = widgets.reply_to_row.clone();
            let btn = more.clone();
            more.connect_clicked(move |_| {
                cc.set_visible(true);
                bcc.set_visible(true);
                reply_to.set_visible(true);
                btn.set_visible(false);
            });
            widgets.to_row.add_suffix(&more);
        }

        // Populate the From dropdown.
        let labels: Vec<&str> = model.accounts.iter().map(|a| a.label.as_str()).collect();
        let strings = gtk::StringList::new(&labels);
        widgets.from_row.set_model(Some(&strings));
        // Custom factory so the selected account isn't needlessly ellipsized.
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::None);
                item.set_child(Some(&label));
            }
        });
        factory.connect_bind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let text = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|s| s.string().to_string())
                    .unwrap_or_default();
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    label.set_label(&text);
                }
            }
        });
        widgets.from_row.set_factory(Some(&factory));
        widgets.from_row.set_selected(selected as u32);
        widgets.from_row.set_visible(model.accounts.len() > 1);

        widgets.to_row.set_text(&prefill.to);
        widgets.cc_row.set_text(&prefill.cc);
        widgets.bcc_row.set_text(&prefill.bcc);
        widgets.subject_row.set_text(&prefill.subject);
        if !model.attachments.is_empty() {
            model.rebuild_attachments(&widgets.attach_box, &sender);
        }

        // Files handed in over the size limit (Settings → System → GNOME
        // Files): straight into the upload dialog, once the composer has a
        // window for it to be transient for.
        if !prefill.cloud_uploads.is_empty() {
            let s = sender.clone();
            let paths = prefill.cloud_uploads.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                s.input(ComposeInput::CloudPicked(paths));
            });
        }

        // HYLKI_SHOWCASE_CLOUD_DIALOG opens the cloud upload dialog on a
        // stand-in file two seconds after the composer is up (demo only),
        // for a capture of this upload's terms.
        if std::env::var_os("HYLKI_SHOWCASE_CLOUD_DIALOG").is_some() && std::env::var_os("HYLKI_DEMO").is_some() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                s.input(ComposeInput::CloudPicked(vec![std::path::PathBuf::from("/tmp/Q3 report.pdf")]));
            });
        }

        // Wire autocomplete *after* prefilling, so the initial text doesn't pop it.
        for (row, field) in [
            (&widgets.to_row, Field::To),
            (&widgets.cc_row, Field::Cc),
            (&widgets.bcc_row, Field::Bcc),
        ] {
            let s = sender.clone();
            row.connect_changed(move |_| {
                s.input(ComposeInput::Suggest(field));
                s.input(ComposeInput::MarkFieldsDirty);
            });

            // Close the popover when the field loses focus.
            let focus = gtk::EventControllerFocus::new();
            let s = sender.clone();
            focus.connect_leave(move |_| s.input(ComposeInput::CompletionClose));
            row.add_controller(focus);
        }
        // Subject edits also count as dirtying the draft.
        let s = sender.clone();
        widgets
            .subject_row
            .connect_changed(move |_| s.input(ComposeInput::MarkFieldsDirty));
        // The subject gets the body's red underlines too (#114): every edit
        // re-checks the line through enchant directly and paints error
        // underlines onto the row's inner GtkText — the row itself exposes
        // no Pango attributes. The word the cursor sits in is exempt while
        // typing and joins the check after a 400ms pause, so mistakes show
        // before the space without half-words flashing red mid-keystroke.
        // Addresses stay uncheckable on purpose: only the subject is prose.
        if let Some(text) = inner_text(widgets.subject_row.upcast_ref()) {
            let t = text.clone();
            let pending: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            widgets.subject_row.connect_changed(move |row| {
                if let Some(prev) = pending.borrow_mut().take() {
                    prev.remove();
                }
                let content = row.text().to_string();
                // Editable positions count characters; attribute ranges
                // count bytes.
                let cursor = content
                    .char_indices()
                    .nth(row.position().max(0) as usize)
                    .map(|(b, _)| b)
                    .unwrap_or(content.len());
                t.set_attributes(crate::spell::error_attrs(&content, Some(cursor)).as_ref());
                let t = t.clone();
                let row = row.clone();
                let slot = pending.clone();
                let id = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(400),
                    move || {
                        slot.borrow_mut().take();
                        t.set_attributes(crate::spell::error_attrs(&row.text(), None).as_ref());
                    },
                );
                *pending.borrow_mut() = Some(id);
            });
            // Prefilled subjects (replies, drafts) get checked on open too.
            text.set_attributes(
                crate::spell::error_attrs(&widgets.subject_row.text(), None).as_ref(),
            );
        }

        // Drive the suggestion list from a single capture-phase key handler on
        // the window — the toplevel sees every key first, regardless of focus.
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = sender.clone();
        let open = model.completion_open.clone();
        let editor = model.editor.clone();
        let key_root = root.clone();
        key.connect_key_pressed(move |_, keyval, _, state| {
            use gtk::glib::Propagation;
            // Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y run the composer's own history
            // (#200): the body's typing and formatting with the attachments
            // in the same order. Caught here, in the capture phase above the
            // editor, so the editor's own handler for the bare body never
            // sees them and the attachments stay in the order.
            //
            // The address and subject rows are the exception: GTK gives every
            // GtkText an undo history of its own, and for a single line that
            // is the one the user means.
            if let Some(redo) = rich_editor::history_key(keyval, state) {
                if !focus_is_entry(&key_root) {
                    s.input(ComposeInput::History { redo });
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Ctrl+V pastes per the "Paste as plain text" preference, read
            // here so a settings change applies to composers already open.
            // Only over the body: the address and subject entries are plain
            // text by nature, and the focus guard leaves their Ctrl+V alone.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && keyval == gtk::gdk::Key::v
                && editor.has_focus()
            {
                editor.paste(!crate::config::load_paste_plain());
                return Propagation::Stop;
            }
            if !open.get() {
                // Escape backs out of the whole composer — the same as Cancel,
                // so an accidental reply is one key away from being undone. Only
                // once the suggestion list is closed, which Escape dismisses
                // first (below), so one press never does both.
                if keyval == gtk::gdk::Key::Escape {
                    s.input(ComposeInput::Cancel);
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Compare by value (the const-as-pattern match wasn't matching).
            if keyval == gtk::gdk::Key::Down {
                s.input(ComposeInput::CompletionMove(1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Up {
                s.input(ComposeInput::CompletionMove(-1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter {
                s.input(ComposeInput::CompletionAccept);
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Escape {
                s.input(ComposeInput::CompletionClose);
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        root.add_controller(key);

        if prefill.to.is_empty() {
            widgets.to_row.grab_focus();
        } else {
            model.editor.grab_focus();
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        'handle: {
        match message {
            ComposeInput::Cancel => {
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }

            ComposeInput::SendAt(at) => {
                self.send_at = Some(at);
                sender.input(ComposeInput::Send);
            }

            ComposeInput::ClearSendAt => {
                self.send_at = None;
            }

            ComposeInput::ShowFields(on) => {
                self.fields_shown = on;
                widgets.fields_list.set_visible(!(self.compact && !self.windowed) || on);
            }

            ComposeInput::SetNarrow(narrow) => {
                self.narrow = narrow;
                self.dress_format_buttons();
            }

            ComposeInput::OverflowMenu => {
                let entry = |label: String, icon: &str, msg: fn() -> ComposeInput| {
                    let s = sender.clone();
                    MenuEntry::new(&label, move || s.input(msg()))
                        .icon(&format!("co.hyprlab.Hylki-{icon}-symbolic"))
                };
                let mut drafts = vec![entry(i18n("Save Draft"), "document-save", || ComposeInput::SaveDraft)];
                if self.draft_origin.is_some() {
                    drafts.push(entry(i18n("Delete Draft"), "user-trash", || ComposeInput::DeleteDraft));
                }
                let mut attach = vec![entry(i18n("Attach files"), "mail-attachment", || ComposeInput::AttachFiles)];
                if !self.cloud_accounts.is_empty() {
                    attach.push(
                        entry(i18n("Upload to cloud storage and share a link"), "cloud", || ComposeInput::CloudAttach)
                            .enabled(self.cloud_busy == 0),
                    );
                }
                attach.push(entry(i18n("Open Contacts"), "x-office-address-book", || ComposeInput::OpenContacts));
                // The OpenPGP toggles go through their buttons so the
                // toggled handlers keep the model and the buttons agreeing.
                let mut pgp = Vec::new();
                if crate::pgp::available() {
                    let check = |on: bool, icon: &str| if on { "verified-checkmark" } else { icon }.to_string();
                    let sign = widgets.sign_btn.clone();
                    pgp.push(
                        MenuEntry::new(&i18n("Sign with your OpenPGP key"), move || sign.set_active(!sign.is_active()))
                            .icon(&format!("co.hyprlab.Hylki-{}-symbolic", check(self.sign, "security-high"))),
                    );
                    let encrypt = widgets.encrypt_btn.clone();
                    pgp.push(
                        MenuEntry::new(
                            &i18n("Encrypt with OpenPGP to every recipient's key"),
                            move || encrypt.set_active(!encrypt.is_active()),
                        )
                        .icon(&format!("co.hyprlab.Hylki-{}-symbolic", check(self.encrypt, "channel-secure"))),
                    );
                }
                // The format chooser and the preview toggle are not here:
                // they sit at the end of the formatting row, which a narrow
                // pane never folds away.
                let mut host = Vec::new();
                if self.can_toggle {
                    host.push(if self.windowed {
                        entry(i18n("Collapse into reader"), "view-restore", || ComposeInput::ToggleWindowed)
                    } else {
                        entry(i18n("Open in window"), "view-fullscreen", || ComposeInput::ToggleWindowed)
                    });
                }
                let btn = &widgets.overflow_btn;
                show_context_menu(btn, (btn.width() / 2) as f64, btn.height() as f64, vec![drafts, attach, pgp, host]);
            }

            ComposeInput::CloudAttach => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&i18n("Upload to Cloud Storage"));
                let parent = root.root().and_downcast::<gtk::Window>();
                let s = sender.input_sender().clone();
                dialog.open_multiple(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Ok(model) = res {
                        let paths: Vec<_> = (0..model.n_items())
                            .filter_map(|i| model.item(i).and_downcast::<gtk::gio::File>()?.path())
                            .collect();
                        if !paths.is_empty() {
                            let _ = s.send(ComposeInput::CloudPicked(paths));
                        }
                    }
                });
            }

            ComposeInput::CloudPicked(paths) => {
                let parent = root.root().and_downcast::<gtk::Window>();
                cloud_upload_dialog(parent.as_ref(), &self.cloud_accounts, paths, sender.input_sender().clone());
            }

            ComposeInput::CloudUpload { paths, account, link_password } => {
                let secret = if account.has_secret() {
                    crate::config::load_cloud_password(&account.key())
                } else {
                    Some(String::new())
                };
                let Some(password) = secret else {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    let d = adw::MessageDialog::new(
                        parent.as_ref(),
                        Some(i18n("Not signed in").as_str()),
                        Some(i18n_f("The sign-in for {name} is not in the keyring. Open Settings, Cloud Storage, and enter it again.", &[("name", &account.name)]).as_str()),
                    );
                    d.add_response("ok", &i18n("OK"));
                    d.present();
                    break 'handle;
                };
                for path in paths {
                    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    self.cloud_busy += 1;
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                    let s = sender.input_sender().clone();
                    let (a, pw, lp) = (account.clone(), password.clone(), link_password.clone());
                    std::thread::spawn(move || {
                        let result = crate::cloud::upload_and_share(&a, &pw, &path, lp.as_deref());
                        let _ = s.send(ComposeInput::CloudUploaded { name, result });
                    });
                }
            }

            ComposeInput::CloudUploaded { name, result } => {
                self.cloud_busy = self.cloud_busy.saturating_sub(1);
                match result {
                    Ok(share) => {
                        let id = format!("vireo-cloud-{}", crate::rng::token(8).unwrap_or_else(|_| share.size.to_string()));
                        let mut caption = crate::cloud::human_size(share.size);
                        if let Some(d) = &share.expires {
                            caption.push_str(&format!(", {}", i18n_f("link expires {date}", &[("date", d)])));
                        }
                        if share.password.is_some() {
                            caption.push_str(&format!(", {}", i18n("password-protected")));
                        }
                        let html = format!(
                            "<p id=\"{id}\" data-vireo-cloud=\"1\">\u{1F4CE} <a href=\"{url}\">{name}</a> ({caption})</p>",
                            url = html_escape(&share.url),
                            name = html_escape(&share.name),
                            caption = html_escape(&caption),
                        );
                        // Into the body where the user's own text ends: above
                        // the signature, and above a quoted original in a
                        // reply, so the link reads as part of the message.
                        if let Some(kind) = self.editor.source_kind() {
                            let line = match kind {
                                SourceKind::Markdown => format!(
                                    "\u{1F4CE} [{name}]({url}) ({caption})",
                                    name = share.name,
                                    url = share.url,
                                    caption = caption,
                                ),
                                SourceKind::Html => html.clone(),
                            };
                            // The same place, said in text: above the `-- `
                            // signature line if there is one, else at the end.
                            self.editor.run_js(&format!(
                                "(function(){{var t=document.getElementById('src');if(!t)return;\
                                 var l='{}';var v=t.value;var i=v.lastIndexOf('\\n-- \\n');\
                                 if(i>=0){{v=v.slice(0,i)+'\\n'+l+'\\n'+v.slice(i);}}\
                                 else{{v=v.replace(/\\s*$/,'')+'\\n\\n'+l+'\\n';}}\
                                 t.value=v;window.__hylkiDirty=true;}})()",
                                js_escape(&line)
                            ));
                            if let Some(p) = share.password.clone() {
                                self.cloud_passwords.push((share.name.clone(), p));
                            }
                            self.cloud_links.push(CloudLink { id, name: share.name, url: share.url });
                            self.rebuild_attachments(&widgets.attach_box, &sender);
                            break 'handle;
                        }
                        self.editor.run_js(&format!(
                            "(function(){{var d=document.createElement('div');d.innerHTML='{}';\
                             var p=d.firstChild;var b=document.body;\
                             var first=null;var cands=b.querySelectorAll('.vireo-sig,.vireo-quote-attr,blockquote');\
                             for(var i=0;i<cands.length;i++){{var t=cands[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;\
                             if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}}\
                             if(first)b.insertBefore(p,first);else b.appendChild(p);\
                             document.dispatchEvent(new Event('input'));}})()",
                            js_escape(&html)
                        ));
                        if let Some(p) = share.password.clone() {
                            self.cloud_passwords.push((share.name.clone(), p));
                        }
                        self.cloud_links.push(CloudLink { id, name: share.name, url: share.url });
                    }
                    Err(e) => {
                        let parent = root.root().and_downcast::<gtk::Window>();
                        let d = adw::MessageDialog::new(
                            parent.as_ref(),
                            Some(i18n_f("Could not upload {name}", &[("name", &name)]).as_str()),
                            Some(&e),
                        );
                        d.add_response("ok", &i18n("OK"));
                        d.present();
                    }
                }
                self.rebuild_attachments(&widgets.attach_box, &sender);
            }

            ComposeInput::RemoveCloudLink(i) => {
                if i < self.cloud_links.len() {
                    let link = self.cloud_links.remove(i);
                    self.cloud_passwords.retain(|(n, _)| *n != link.name);
                    // Source mode has no element to remove: the line that
                    // names the link is what goes.
                    if self.editor.source_kind().is_some() {
                        self.editor.run_js(&format!(
                            "(function(){{var t=document.getElementById('src');if(!t)return;\
                             var u='{}';\
                             t.value=t.value.split('\\n').filter(function(l){{return l.indexOf(u)<0;}})\
                               .join('\\n');window.__hylkiDirty=true;}})()",
                            js_escape(&link.url)
                        ));
                        self.rebuild_attachments(&widgets.attach_box, &sender);
                        break 'handle;
                    }
                    self.editor.run_js(&format!(
                        "(function(){{var p=document.getElementById('{}');if(p)p.remove();document.dispatchEvent(new Event('input'));}})()",
                        js_escape(&link.id)
                    ));
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                }
            }

            ComposeInput::CopyCloudPasswords => {
                let text = self
                    .cloud_passwords
                    .iter()
                    .map(|(n, p)| format!("{n}: {p}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&text);
                }
            }

            ComposeInput::PickSendTime => {
                let parent = root.root().and_downcast::<gtk::Window>();
                pick_send_time(parent.as_ref(), self.send_at, sender.input_sender().clone());
            }

            ComposeInput::DeleteDraft => {
                if let Some(origin) = self.draft_origin.clone() {
                    let _ = sender
                        .output(ComposeOutput::DeleteDraft { id: self.compose_id, origin });
                }
            }

            ComposeInput::ToggleWindowed => {
                let _ = sender.output(ComposeOutput::ToggleWindow(self.compose_id));
            }

            ComposeInput::SetDecorations(on) => {
                self.decorations = on;
                size_for_host(
                    &widgets.toolbar_root,
                    &widgets.header,
                    &widgets.editor_holder,
                    self.windowed,
                    on,
                );
            }

            ComposeInput::SetWindowed(windowed) => {
                self.windowed = windowed;
                set_toggle_icon(&widgets.toggle_btn, windowed);
                size_for_host(
                    &widgets.toolbar_root,
                    &widgets.header,
                    &widgets.editor_holder,
                    windowed,
                    self.decorations,
                );
                // A compact reply grows its field rows back in a window (and
                // sheds them again if it returns inline).
                widgets.fields_list.set_visible(!(self.compact && !windowed) || self.fields_shown);
            }

            ComposeInput::FocusEditor => self.editor.grab_focus(),

            ComposeInput::MarkFieldsDirty => self.fields_dirty = true,

            ComposeInput::SaveDraftIfDirty => {
                // Save only if the user actually edited something, so navigating
                // away from a pristine quote-only reply doesn't litter Drafts.
                if self.fields_dirty {
                    sender.input(ComposeInput::SaveDraft);
                } else {
                    let s = sender.clone();
                    let id = self.compose_id;
                    self.editor.is_dirty(move |body_dirty| {
                        if body_dirty {
                            s.input(ComposeInput::SaveDraft);
                        } else {
                            let _ = s.output(ComposeOutput::Close(id));
                        }
                    });
                }
            }

            ComposeInput::OpenContacts => {
                // Browse contacts; the chosen one is appended to the To field.
                let Some(win) = root.root().and_downcast::<gtk::Window>() else {
                    break 'handle;
                };
                let to_row = widgets.to_row.clone();
                crate::ui::contacts_browser::present(&win, move |contact| {
                    let display = if contact.name.trim().is_empty()
                        || contact.name == contact.email
                    {
                        contact.email.clone()
                    } else {
                        format!("{} <{}>", contact.name, contact.email)
                    };
                    let cur = to_row.text().to_string();
                    let trimmed = cur.trim_end();
                    let sep = if trimmed.is_empty() {
                        ""
                    } else if trimmed.ends_with(',') {
                        " "
                    } else {
                        ", "
                    };
                    to_row.set_text(&format!("{cur}{sep}{display}, "));
                    to_row.set_position(-1);
                });
            }

            ComposeInput::AttachFiles => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&i18n("Attach Files"));
                let parent = root.root().and_downcast::<gtk::Window>();
                let s = sender.input_sender().clone();
                dialog.open_multiple(
                    parent.as_ref(),
                    gtk::gio::Cancellable::NONE,
                    move |res| {
                        if let Ok(model) = res {
                            let mut paths = Vec::new();
                            for i in 0..model.n_items() {
                                if let Some(file) =
                                    model.item(i).and_downcast::<gtk::gio::File>()
                                {
                                    if let Some(p) = file.path() {
                                        paths.push(p);
                                    }
                                }
                            }
                            if !paths.is_empty() {
                                let _ = s.send(ComposeInput::AddAttachments(paths));
                            }
                        }
                    },
                );
            }

            ComposeInput::AddAttachments(paths) => {
                if paths.is_empty() {
                    break 'handle;
                }
                let at = self.attachments.len();
                let what = attachment_label(i18n_noop("Attach {name}"), &paths[0], paths.len());
                self.attachments.extend(paths.iter().cloned());
                self.push_history(what, ComposeStep::Remove { at, count: paths.len() });
                self.rebuild_attachments(&widgets.attach_box, &sender);
                self.report_history(&sender);
            }

            ComposeInput::RemoveAttachment(i) => {
                if i < self.attachments.len() {
                    let path = self.attachments.remove(i);
                    let what = attachment_label(i18n_noop("Remove {name}"), &path, 1);
                    self.push_history(what, ComposeStep::Insert { at: i, paths: vec![path] });
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                    self.report_history(&sender);
                }
            }

            ComposeInput::History { redo } => {
                self.history_step(redo, widgets, &sender);
            }

            ComposeInput::ShowcaseHistory(step) => {
                self.showcase_history(step, widgets, &sender);
            }

            ComposeInput::BodyHistory(can_undo) => {
                // A fresh edit takes its place in the order — one marker
                // covers a whole run of typing, since the editor coalesces
                // its own steps and this only has to say where they sit.
                if can_undo
                    && !matches!(self.undo_stack.last().map(|e| &e.step), Some(ComposeStep::Body))
                {
                    self.push_history(i18n("Typing"), ComposeStep::Body);
                }
                self.report_history(&sender);
            }

            ComposeInput::AccountChanged => {
                // Swap the editor's signature block for the new account's.
                let idx = widgets.from_row.selected() as usize;
                let new_sig = self.accounts.get(idx).map(|a| a.signature.clone()).unwrap_or_default();
                if new_sig == self.current_sig {
                    break 'handle;
                }
                // Source mode holds the signature as text, so the swap is a
                // text replacement in the field rather than a DOM one.
                if let Some(kind) = self.editor.source_kind() {
                    let old = sig_source(kind, &self.current_sig);
                    let new = sig_source(kind, &new_sig);
                    self.editor.run_js(&format!(
                        "(function(){{var t=document.getElementById('src');if(!t)return;\
                         var o='{}',n='{}';var v=t.value;\
                         var i=o?v.lastIndexOf(o):-1;\
                         if(i>=0){{v=v.slice(0,i)+n+v.slice(i+o.length);}}else{{v=v+n;}}\
                         t.value=v;window.__hylkiDirty=true;}})()",
                        js_escape(&old),
                        js_escape(&new)
                    ));
                    self.current_sig = new_sig;
                    break 'handle;
                }
                {
                    let replacement = if new_sig.is_empty() {
                        String::new()
                    } else {
                        sig_html(&new_sig)
                    };
                    let js = format!(
                        "(function(){{var s=document.querySelector('.vireo-sig');\
                         var h='{}';\
                         if(s){{if(h){{s.outerHTML=h;}}else{{s.remove();}}}}\
                         else if(h){{document.body.insertAdjacentHTML('beforeend',h);}}}})()",
                        rich_editor::js_escape(&replacement)
                    );
                    self.editor.run_js(&js);
                    self.current_sig = new_sig;
                }
            }

            ComposeInput::Suggest(field) => {
                let row = match field {
                    Field::To => &widgets.to_row,
                    Field::Cc => &widgets.cc_row,
                    Field::Bcc => &widgets.bcc_row,
                };
                self.show_completion(field, row);
            }

            ComposeInput::AddSuggestions(new) => {
                for n in new {
                    let key = n.email.to_lowercase();
                    match self.suggestions.iter_mut().find(|s| s.email.to_lowercase() == key) {
                        Some(s) => {
                            s.score += 1;
                            if s.name.trim().is_empty() || s.name == s.email {
                                s.name = n.name;
                            }
                        }
                        None => self.suggestions.push(n),
                    }
                }
            }

            ComposeInput::CompletionMove(delta) => {
                if self.completion_count == 0 {
                    break 'handle;
                }
                let max = self.completion_count as i32 - 1;
                let new = (self.completion_selected as i32 + delta).clamp(0, max) as usize;
                self.completion_selected = new;
                if let Some(list) = &self.completion_list {
                    if let Some(row) = list.row_at_index(new as i32) {
                        list.select_row(Some(&row));
                    }
                }
            }

            ComposeInput::CompletionAccept => {
                let row = match self.completion_field {
                    Some(Field::To) => &widgets.to_row,
                    Some(Field::Cc) => &widgets.cc_row,
                    Some(Field::Bcc) => &widgets.bcc_row,
                    None => break 'handle,
                };
                let text = row.text().to_string();
                let token = text.rsplit(',').next().unwrap_or("").trim().to_string();
                if !token.is_empty() {
                    let chosen = self.ranked_matches(&token).into_iter().nth(self.completion_selected);
                    if let Some(sug) = chosen {
                        complete_field(row, &sug);
                    }
                }
                self.completion_open.set(false);
                self.completion.popdown();
            }

            ComposeInput::CompletionClose => {
                self.completion_open.set(false);
                self.completion.popdown();
            }

            ComposeInput::ToggleSign(on) => self.sign = on,

            ComposeInput::FormatMenu => {
                let btn = &self.format_btn;
                show_context_menu(
                    btn,
                    (btn.width() / 2) as f64,
                    btn.height() as f64,
                    vec![self.format_entries(&sender)],
                );
            }

            ComposeInput::SetFormat(to) => {
                let from = self.format;
                if to == from {
                    break 'handle;
                }
                self.format = to;
                self.editor.set_formatting_visible(to == ComposeFormat::Rich);
                self.dress_format_buttons();
                self.preview_btn.set_visible(to.is_source());
                // A preview of the old format's render would be a lie about
                // the new one.
                self.preview_btn.set_active(false);
                // Rich and plain text share one document — plain text only
                // decides what is *sent* — so switching between them must
                // not reload it and throw away the undo history and caret.
                if from.is_source() || to.is_source() {
                    let s = sender.clone();
                    self.read_body_as(from, false, move |body, _| {
                        s.input(ComposeInput::LoadAs { from, to, body });
                    });
                }
            }

            ComposeInput::LoadAs { from, to, body } => {
                // Every conversion goes through HTML: it is the one form
                // all four formats can be written to and read back from.
                let html = match from {
                    ComposeFormat::Markdown => crate::markdown::to_html(&body),
                    _ => body,
                };
                match to {
                    ComposeFormat::Markdown => self
                        .editor
                        .set_source(SourceKind::Markdown, &crate::markdown::from_html(&html)),
                    ComposeFormat::Html => self
                        .editor
                        .set_source(SourceKind::Html, &crate::markdown::pretty_html(&html)),
                    ComposeFormat::Rich | ComposeFormat::Plain => self.editor.set_html(&html),
                }
                self.editor.grab_focus();
            }

            ComposeInput::TogglePreview(on) => {
                // Keep the button with the state, for the times the message
                // arrives from somewhere other than a click on it.
                if self.preview_btn.is_active() != on {
                    self.preview_btn.set_active(on);
                }
                if !on {
                    self.editor.show_preview(None);
                    break 'handle;
                }
                let s = sender.clone();
                self.read_body_as(self.format, false, move |body, _| {
                    s.input(ComposeInput::ShowPreview(body));
                });
            }

            ComposeInput::ShowPreview(body) => {
                let html = match self.format {
                    ComposeFormat::Markdown => crate::markdown::to_html(&body),
                    _ => body,
                };
                self.editor.show_preview(Some(&html));
            }
            ComposeInput::ToggleEncrypt(on) => {
                self.encrypt = on;
                if on && !self.sign {
                    self.sign = true;
                    widgets.sign_btn.set_active(true);
                }
            }
            ComposeInput::Send => {
                let to = widgets.to_row.text().trim().to_string();
                if to.is_empty() {
                    widgets.to_row.add_css_class("error");
                    break 'handle;
                }
                let cc = widgets.cc_row.text().trim().to_string();
                let bcc = widgets.bcc_row.text().trim().to_string();
                let reply_to = widgets.reply_to_row.text().trim().to_string();
                let subject = widgets.subject_row.text().to_string();
                let idx = widgets.from_row.selected() as usize;
                // OpenPGP (#133): say what is missing before anything leaves.
                if self.sign || self.encrypt {
                    let from = self.accounts.get(idx);
                    let problem = pgp_send_check(
                        from.map(|a| a.email.as_str()).unwrap_or(""),
                        from.and_then(|a| a.pgp_key.as_deref()),
                        &[to.as_str(), cc.as_str(), bcc.as_str()],
                        self.encrypt,
                    );
                    if let Err(problem) = problem {
                        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                        let dialog = adw::MessageDialog::new(
                            parent.as_ref(),
                            Some(&i18n("Cannot send with OpenPGP")),
                            Some(&problem),
                        );
                        dialog.add_response("ok", &i18n("OK"));
                        dialog.present();
                        break 'handle;
                    }
                }
                let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
                let from_alias = self.accounts.get(idx).and_then(|a| a.alias_from.clone());

                // Pull the body out of the editor (async), then finish
                // sending via SendBody. The send-time reader also recuts any
                // picture armed for it; a draft save below does not. In a
                // source format `html` carries the source itself, which
                // `outgoing_body` converts.
                let s = sender.clone();
                self.read_body_as(self.format, true, move |html, text| {
                    s.input(ComposeInput::SendBody {
                        html,
                        text,
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        reply_to: reply_to.clone(),
                        subject: subject.clone(),
                        from_account_id,
                        from_alias: from_alias.clone(),
                    });
                });
            }

            ComposeInput::SendBody { html, text, to, cc, bcc, reply_to, subject, from_account_id, from_alias } => {
                let (html, text) = self.outgoing_body(html, text);
                let out = self
                    .build_outgoing(from_account_id, from_alias, to, cc, bcc, reply_to, subject, text, html);
                let _ = sender.output(ComposeOutput::Send(Box::new(out)));
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }

            ComposeInput::SaveDraft => {
                // A draft can be saved without recipients; just capture the fields.
                let to = widgets.to_row.text().trim().to_string();
                let cc = widgets.cc_row.text().trim().to_string();
                let bcc = widgets.bcc_row.text().trim().to_string();
                let reply_to = widgets.reply_to_row.text().trim().to_string();
                let subject = widgets.subject_row.text().to_string();
                let idx = widgets.from_row.selected() as usize;
                let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
                let from_alias = self.accounts.get(idx).and_then(|a| a.alias_from.clone());
                let s = sender.clone();
                self.read_body_as(self.format, false, move |html, text| {
                    s.input(ComposeInput::SaveDraftBody {
                        html,
                        text,
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        reply_to: reply_to.clone(),
                        subject: subject.clone(),
                        from_account_id,
                        from_alias: from_alias.clone(),
                    });
                });
            }

            ComposeInput::SaveDraftBody { html, text, to, cc, bcc, reply_to, subject, from_account_id, from_alias } => {
                let (html, text) = self.outgoing_body(html, text);
                let out = self
                    .build_outgoing(from_account_id, from_alias, to, cc, bcc, reply_to, subject, text, html);
                let _ = sender.output(ComposeOutput::SaveDraft(Box::new(out)));
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }
        }
        }
        // Overriding `update_with_view` takes over relm4's default, which
        // ran `update_view` after every message; without this the `#[watch]`
        // bindings above only ever hold their `init` values.
        self.update_view(widgets, sender);
    }
}

impl Compose {
    /// The format chooser's icon and tooltip for the format it is currently
    /// set to, and whether the preview toggle can afford its label.
    fn dress_format_buttons(&self) {
        self.format_btn.set_icon_name(format_icon(self.format));
        self.format_btn.set_tooltip_text(Some(
            i18n_f("Writing in {format}", &[("format", &format_label(self.format))]).as_str(),
        ));
        // The one label on the row goes when there is no width for it, the
        // same trade the header makes when it folds.
        let preview = i18n("Preview");
        self.preview_content.set_label(if self.narrow { "" } else { preview.as_str() });
    }

    /// The four formats as menu entries, the current one wearing a tick in
    /// place of its icon (as the OpenPGP toggles do).
    fn format_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        [
            ComposeFormat::Rich,
            ComposeFormat::Markdown,
            ComposeFormat::Html,
            ComposeFormat::Plain,
        ]
        .into_iter()
        .map(|format| {
            let s = sender.clone();
            let label = format!("{} — {}", format_label(format), format_hint(format));
            MenuEntry::new(&label, move || s.input(ComposeInput::SetFormat(format)))
                .icon(format_icon(format))
                .selected(format == self.format)
        })
        .collect()
    }

    /// Read the body out of the editor in `format`'s own terms: the source
    /// text for a source format (with no plain-text rendering, since the
    /// source *is* text), the document's HTML and text otherwise.
    ///
    /// `for_send` picks the reader that recuts pictures armed for it; a
    /// draft save must never cost quality, so it passes false.
    fn read_body_as(
        &self,
        format: ComposeFormat,
        for_send: bool,
        cb: impl FnOnce(String, String) + 'static,
    ) {
        if format.is_source() {
            self.editor.extract_source(move |src| cb(src, String::new()));
        } else if for_send {
            self.editor.extract_for_send(cb);
        } else {
            self.editor.extract(cb);
        }
    }

    /// The `(html, text)` parts of the outgoing message, from what the
    /// editor handed back. Markdown is rendered and keeps its source as the
    /// text part — Markdown being, deliberately, plain text that reads as
    /// itself. Hand-written HTML is sanitized and gets a Markdown rendering
    /// as its text part. Plain text sends no HTML part at all (#180).
    fn outgoing_body(&self, body: String, text: String) -> (String, String) {
        match self.format {
            ComposeFormat::Rich => (body, text),
            ComposeFormat::Plain => (String::new(), text),
            ComposeFormat::Markdown => {
                let html = crate::markdown::sanitize_outgoing(&crate::markdown::to_html(&body));
                (html, body)
            }
            ComposeFormat::Html => {
                let text = crate::markdown::html_to_text(&body);
                (crate::markdown::sanitize_outgoing(&body), text)
            }
        }
    }

    /// Assemble an [`OutgoingMessage`] from the composed fields + attachments,
    /// carrying the draft origin so a saved/sent draft replaces its predecessor.
    #[allow(clippy::too_many_arguments)]
    fn build_outgoing(
        &self,
        from_account_id: u32,
        from_alias: Option<String>,
        to: String,
        cc: String,
        bcc: String,
        reply_to: String,
        subject: String,
        text: String,
        html: String,
    ) -> OutgoingMessage {
        OutgoingMessage {
            from_account_id,
            from_alias,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            body: text,
            html,
            attachments: self
                .attachments
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            draft_origin: self.draft_origin.clone(),
            outbox_origin: self.outbox_origin,
            sign: self.sign,
            encrypt: self.encrypt,
            send_at: self.send_at,
        }
    }

    /// Suggestions matching `token`, ranked best-first (prefix match, then most
    /// frequently used), capped to a handful.
    fn ranked_matches(&self, token: &str) -> Vec<Suggestion> {
        let q = token.to_lowercase();
        let mut matches: Vec<Suggestion> =
            self.suggestions.iter().filter(|s| s.matches(token)).cloned().collect();
        matches.sort_by(|a, b| {
            let pa = a.email.to_lowercase().starts_with(&q) || a.name.to_lowercase().starts_with(&q);
            let pb = b.email.to_lowercase().starts_with(&q) || b.name.to_lowercase().starts_with(&q);
            // Own addresses come after everyone else's, prefix match or not.
            a.own.cmp(&b.own)
                .then(pb.cmp(&pa))
                .then(b.score.cmp(&a.score))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        matches.truncate(8);
        matches
    }

    /// Filter suggestions by the recipient fragment being typed and show them in
    /// a popover under the field; clicking one completes that recipient.
    fn show_completion(&mut self, field: Field, row: &adw::EntryRow) {
        let text = row.text().to_string();
        // The recipient currently being typed is the part after the last comma.
        let token = text.rsplit(',').next().unwrap_or("").trim().to_string();
        if token.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }
        let matches = self.ranked_matches(&token);
        if matches.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }

        // Attach the popover to the active field (only re-parent when it moves).
        if self.completion_field != Some(field) {
            if self.completion.parent().is_some() {
                self.completion.unparent();
            }
            self.completion.set_parent(row);
            self.completion_field = Some(field);
        }

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        // Keep focus in the entry so its key controller drives navigation.
        list.set_can_focus(false);
        list.add_css_class("autocomplete");
        let count = matches.len();
        for sug in matches {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            item.set_margin_start(6);
            item.set_margin_end(6);
            item.set_margin_top(3);
            item.set_margin_bottom(3);
            // Mark where the suggestion came from: address book vs. mail history.
            let icon = gtk::Image::from_icon_name(if sug.from_contacts {
                "co.hyprlab.Hylki-avatar-default-symbolic"
            } else {
                "co.hyprlab.Hylki-document-open-recent-symbolic"
            });
            icon.set_valign(gtk::Align::Center);
            icon.add_css_class("dim-label");
            item.append(&icon);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let title = gtk::Label::new(Some(&sug.name));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            text.append(&title);
            if sug.name != sug.email {
                let sub = gtk::Label::new(Some(&sug.email));
                sub.set_halign(gtk::Align::Start);
                sub.set_xalign(0.0);
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
                text.append(&sub);
            }
            item.append(&text);
            let lbr = gtk::ListBoxRow::new();
            lbr.set_can_focus(false);
            lbr.set_child(Some(&item));
            list.append(&lbr);

            // Complete this recipient on click.
            let row2 = row.clone();
            let pop = self.completion.downgrade();
            let sug = sug.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| {
                complete_field(&row2, &sug);
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            });
            lbr.add_controller(gesture);
        }

        // Highlight the first suggestion so Enter accepts it immediately.
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }
        self.completion_selected = 0;
        self.completion_count = count;
        self.completion_list = Some(list.clone());

        self.completion.set_child(Some(&list));
        self.completion.popup();
        self.completion_open.set(true);
    }

    /// Record a change the composer can take back (#200), ending whatever
    /// redo branch was open — as every undo history does.
    ///
    /// A change of the composer's own is filed *under* the body's marker, so
    /// the marker always sits on top: what someone was just writing comes
    /// back first, and their attachments after it, newest first. The order
    /// has to be decided here because it cannot be honoured any finer —
    /// WebKit's text history can be stepped but not counted, so a marker has
    /// no way to say that two of its steps belong above an attachment and
    /// the rest below it. Writing wins, which is what a composer is for.
    fn push_history(&mut self, what: String, step: ComposeStep) {
        let at = match self.undo_stack.last().map(|e| &e.step) {
            Some(ComposeStep::Body) => self.undo_stack.len() - 1,
            _ => self.undo_stack.len(),
        };
        self.undo_stack.insert(at, ComposeUndoEntry { step, what });
        self.redo_stack.clear();
    }

    /// Step the composer's history one change back, or forward with `redo`.
    ///
    /// A [`ComposeStep::Body`] marker is not popped when it is used: the
    /// editor holds an unknown number of text steps behind it, so the marker
    /// stays where it is and a matching one is put on the other side, until
    /// the editor says it has run out. Only then does it cross over and the
    /// change under it come up. That reading of the editor is a beat behind
    /// the command that changed it, which costs at worst one keypress that
    /// does nothing — never a step out of order.
    fn history_step(
        &mut self,
        redo: bool,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let (mut from, mut to) = if redo {
            (std::mem::take(&mut self.redo_stack), std::mem::take(&mut self.undo_stack))
        } else {
            (std::mem::take(&mut self.undo_stack), std::mem::take(&mut self.redo_stack))
        };
        let mut changed_attachments = false;
        while let Some(entry) = from.last() {
            if matches!(entry.step, ComposeStep::Body) {
                if if redo { self.editor.can_redo() } else { self.editor.can_undo() } {
                    if redo {
                        self.editor.redo();
                    } else {
                        self.editor.undo();
                    }
                    // Whatever the editor just took back is now available
                    // in the other direction, at this point in the order.
                    if !matches!(to.last().map(|e| &e.step), Some(ComposeStep::Body)) {
                        to.push(body_entry());
                    }
                    break;
                }
                // Run out (or a format change reloaded the document and
                // dropped its history): hand the marker over — dropping it
                // if that side already ends in one — and go on to the
                // change underneath.
                from.pop();
                if !matches!(to.last().map(|e| &e.step), Some(ComposeStep::Body)) {
                    to.push(body_entry());
                }
                continue;
            }
            let entry = from.pop().expect("checked by the loop condition");
            let back = self.apply_step(entry.step);
            to.push(ComposeUndoEntry { step: back, what: entry.what });
            changed_attachments = true;
            break;
        }
        if redo {
            self.redo_stack = from;
            self.undo_stack = to;
        } else {
            self.undo_stack = from;
            self.redo_stack = to;
        }
        if changed_attachments {
            self.rebuild_attachments(&widgets.attach_box, sender);
        }
        self.report_history(sender);
    }

    /// Carry out one step, returning the step that puts it back.
    fn apply_step(&mut self, step: ComposeStep) -> ComposeStep {
        match step {
            // Never reached: the caller keeps body markers on their stack.
            ComposeStep::Body => ComposeStep::Body,
            ComposeStep::Insert { at, paths } => {
                let at = at.min(self.attachments.len());
                let count = paths.len();
                self.attachments.splice(at..at, paths);
                ComposeStep::Remove { at, count }
            }
            ComposeStep::Remove { at, count } => {
                let at = at.min(self.attachments.len());
                let end = (at + count).min(self.attachments.len());
                let paths: Vec<_> = self.attachments.drain(at..end).collect();
                ComposeStep::Insert { at, paths }
            }
        }
    }

    /// Tell the host what the history can do now, for its own Undo and Redo
    /// entries. The body's side is read from the editor rather than from the
    /// stack: a marker that has run out is still sitting there until the
    /// next step walks past it.
    fn report_history(&self, sender: &ComponentSender<Self>) {
        let reachable = |stack: &[ComposeUndoEntry], can_text: bool| {
            stack
                .iter()
                .rev()
                .find(|e| !matches!(e.step, ComposeStep::Body) || can_text)
                .map(|e| e.what.clone())
        };
        let _ = sender.output(ComposeOutput::History {
            id: self.compose_id,
            undo: reachable(&self.undo_stack, self.editor.can_undo()),
            redo: reachable(&self.redo_stack, self.editor.can_redo()),
        });
    }

    /// Showcase only: walk the history through a round that mixes typing
    /// with attachments, logging the body and the attachment names after
    /// each step on `hylki::compose::undo`. The keys cannot be injected on
    /// a Wayland desktop, so this is how the interleaving is checked.
    ///
    /// Each step schedules the next a beat later, because both sides of it
    /// are asynchronous: the document's edits go to the web process, and
    /// the editor state that reports them comes back the same way.
    fn showcase_history(
        &mut self,
        step: u8,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let stack = |s: &[ComposeUndoEntry]| {
            s.iter().map(|e| e.what.as_str()).collect::<Vec<_>>().join(" | ")
        };
        let names = |p: &[std::path::PathBuf]| {
            p.iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        // Source mode is a textarea rather than the rich document, and takes
        // the same route through WebKit's history — worth walking too, since
        // three of the four composing formats are written in it (#180).
        let focus_js = if self.editor.source_kind().is_some() {
            "var t=document.getElementById('src');t.focus();\
             t.selectionStart=t.selectionEnd=t.value.length;"
        } else {
            "document.body.focus();"
        };
        // HYLKI_SHOWCASE_COMPOSE_UNDO=backspace trims the body with the
        // Backspace key's own editing command instead of typing into it,
        // which is how a reply is usually cut down to size.
        let mode = std::env::var("HYLKI_SHOWCASE_COMPOSE_UNDO").unwrap_or_default();
        let deleting = mode == "backspace";
        // `pause` leaves more than the script's PAUSE_MS between two runs of
        // typing, so each should come back on a press of its own.
        let pausing = mode == "pause";
        let type_in = |editor: &RichEditor, text: &str| {
            editor.run_js(&format!("{focus_js}document.execCommand('insertText',false,'{text}')"));
        };
        let attach = |sender: &ComponentSender<Self>| {
            let path = std::env::temp_dir().join("hylki-undo-probe.txt");
            let _ = std::fs::write(&path, b"probe");
            sender.input(ComposeInput::AddAttachments(vec![path]));
        };
        if step == 0 {
            // The widget has to hold focus, not just the document, or the
            // window's own Undo entry never changes hands.
            self.editor.grab_focus();
            self.editor.run_js(focus_js);
        }
        if deleting {
            // Something to delete first — a caret at the top of a fresh
            // reply has nothing behind it, and Backspace there is a no-op
            // that rightly leaves no step to take back.
            match step {
                0 => type_in(&self.editor, "ONETWO"),
                1..=3 => self.editor.editing_command("DeleteBackward"),
                4 => attach(&sender),
                5..=9 => sender.input(ComposeInput::History { redo: false }),
                10..=12 => sender.input(ComposeInput::History { redo: true }),
                _ => {}
            }
        } else if pausing {
            match step {
                0 => type_in(&self.editor, "ONE"),
                1 => {}                       // the long wait below
                2 => type_in(&self.editor, "TWO"),
                3..=6 => sender.input(ComposeInput::History { redo: false }),
                7..=9 => sender.input(ComposeInput::History { redo: true }),
                _ => {}
            }
        } else {
            match step {
                0 => type_in(&self.editor, "ONE"),
                1 => attach(&sender),
                2 => type_in(&self.editor, "TWO"),
                3..=8 => sender.input(ComposeInput::History { redo: false }),
                9..=11 => sender.input(ComposeInput::History { redo: true }),
                _ => {}
            }
        }
        if step > 13 {
            return;
        }
        let s = sender.clone();
        let editor = self.editor.clone();
        let gap = if pausing && step == 1 { 7000 } else { 700 };
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(gap), move || {
            let report = move |body: String| {
                tracing::info!(target: "hylki::compose::undo", "step {step}: body {body:?}");
                s.input(ComposeInput::ShowcaseHistory(step + 1));
            };
            if editor.source_kind().is_some() {
                editor.extract_source(report);
            } else {
                editor.extract_html(report);
            }
        });
        tracing::info!(
            target: "hylki::compose::undo",
            "step {step}: undo [{}] redo [{}] text({},{}) attachments [{}]",
            stack(&self.undo_stack),
            stack(&self.redo_stack),
            self.editor.can_undo(),
            self.editor.can_redo(),
            names(&self.attachments),
        );
        let _ = widgets;
    }

    fn rebuild_attachments(&self, flow: &gtk::FlowBox, sender: &ComponentSender<Self>) {
        while let Some(child) = flow.first_child() {
            flow.remove(&child);
        }
        for (i, path) in self.attachments.iter().enumerate() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());

            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chip.add_css_class("attach-chip");
            // FlowBoxChild defaults to halign: Fill, which would otherwise
            // stretch this box the full width of its cell — leaving the pill's
            // background trailing well past the remove button. Hug the content.
            chip.set_halign(gtk::Align::Start);
            chip.append(&gtk::Image::from_icon_name("co.hyprlab.Hylki-mail-attachment-symbolic"));
            let lbl = gtk::Label::new(Some(&name));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            lbl.set_max_width_chars(22);
            chip.append(&lbl);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Hylki-window-close-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::RemoveAttachment(i));
            });
            chip.append(&rm);

            flow.append(&chip);
            // GtkFlowBox auto-wraps `chip` in a FlowBoxChild that, unlike
            // `chip` itself, has no halign we can set beforehand — it still
            // fills (and hover-highlights) the full cell. Shrink it to the
            // pill's own size and drop its own row interactivity, since the
            // remove button inside is the only real click target.
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
                cell.set_focusable(false);
            }
        }
        // Cloud links (#144) sit with the attachments but read as links: a
        // cloud icon, the name, and a remove that also takes the paragraph
        // out of the body. Uploads in flight show a spinner chip.
        for (i, link) in self.cloud_links.iter().enumerate() {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chip.add_css_class("attach-chip");
            chip.add_css_class("cloud-chip");
            chip.set_halign(gtk::Align::Start);
            chip.set_tooltip_text(Some(&link.url));
            chip.append(&gtk::Image::from_icon_name("co.hyprlab.Hylki-cloud-symbolic"));
            let lbl = gtk::Label::new(Some(&i18n_f("{name} (link)", &[("name", &link.name)])));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            lbl.set_max_width_chars(26);
            chip.append(&lbl);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Hylki-window-close-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::RemoveCloudLink(i));
            });
            chip.append(&rm);
            flow.append(&chip);
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
                cell.set_focusable(false);
            }
        }
        for _ in 0..self.cloud_busy {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            chip.add_css_class("attach-chip");
            chip.set_halign(gtk::Align::Start);
            let spin = gtk::Spinner::new();
            spin.start();
            chip.append(&spin);
            chip.append(&gtk::Label::new(Some(&i18n("Uploading…"))));
            flow.append(&chip);
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
            }
        }
        flow.set_visible(!self.attachments.is_empty() || !self.cloud_links.is_empty() || self.cloud_busy > 0);
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Which account to upload to, and how the links are made this time:
/// the expiry and the password, seeded from the account's settings and
/// changeable for this upload alone.
fn cloud_upload_dialog(
    parent: Option<&gtk::Window>,
    accounts: &[crate::cloud::CloudAccount],
    paths: Vec<std::path::PathBuf>,
    sender: relm4::Sender<ComposeInput>,
) {
    if accounts.is_empty() {
        return;
    }
    let n = paths.len();
    let dialog = adw::MessageDialog::new(
        parent,
        Some(i18n("Upload to Cloud Storage").as_str()),
        Some(crate::i18n::ni18n_f("Upload {n} file and put its share link in the message.", "Upload {n} files and put their share links in the message.", n as u32, &[("n", &n.to_string())]).as_str()),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("upload", &i18n("Upload"));
    dialog.set_default_response(Some("upload"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("upload", adw::ResponseAppearance::Suggested);

    let names: Vec<String> = accounts.iter().map(|a| if a.name.trim().is_empty() { a.where_shown() } else { a.name.clone() }).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let combo = adw::ComboRow::new();
    combo.set_title(&i18n("Account"));
    combo.set_model(Some(&gtk::StringList::new(&name_refs)));
    combo.set_visible(accounts.len() > 1);

    // This upload's terms, starting from the account's own.
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely"));
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect with a password"));
    let password = adw::EntryRow::new();
    password.set_title(&i18n("Download password (empty: generated)"));
    let apply_defaults = {
        let (expire, protect, password) = (expire.clone(), protect.clone(), password.clone());
        let accounts = accounts.to_vec();
        move |i: usize| {
            if let Some(a) = accounts.get(i) {
                // What the service allows for this account: a row it
                // does not is greyed out, with the reason.
                expire.set_sensitive(a.expiry_allowed());
                expire.set_subtitle(&if a.expiry_allowed() { i18n("Days; 0 keeps the link indefinitely") } else { a.link_note.clone() });
                protect.set_sensitive(a.password_allowed());
                protect.set_subtitle(if a.password_allowed() { "" } else { a.link_note.as_str() });
                expire.set_value(if a.expiry_allowed() { a.expire_days as f64 } else { 0.0 });
                protect.set_active(a.password_allowed() && a.password);
                password.set_text("");
            }
        }
    };
    apply_defaults(0);
    password.set_visible(protect.is_active());
    {
        let password = password.clone();
        protect.connect_active_notify(move |p| password.set_visible(p.is_active()));
    }
    {
        let apply_defaults = apply_defaults.clone();
        combo.connect_selected_notify(move |c| apply_defaults(c.selected() as usize));
    }

    let group = adw::PreferencesGroup::new();
    group.add(&combo);
    group.add(&expire);
    group.add(&protect);
    group.add(&password);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(400);
    bx.append(&group);
    dialog.set_extra_child(Some(&bx));

    let accounts = accounts.to_vec();
    let paths = std::cell::RefCell::new(Some(paths));
    dialog.connect_response(None, move |_, resp| {
        if resp != "upload" {
            return;
        }
        let (Some(paths), Some(account)) = (paths.borrow_mut().take(), accounts.get(combo.selected() as usize)) else {
            return;
        };
        let mut account = account.clone();
        account.expire_days = expire.value() as u32;
        account.password = protect.is_active();
        let typed = password.text().trim().to_string();
        let link_password = (account.password && !typed.is_empty()).then_some(typed);
        let _ = sender.send(ComposeInput::CloudUpload { paths, account, link_password });
    });
    dialog.present();
}

/// The GtkText embedded somewhere inside a composite row — where Pango
/// attributes (the spell-check underlines) actually live.
fn inner_text(widget: &gtk::Widget) -> Option<gtk::Text> {
    if let Some(t) = widget.downcast_ref::<gtk::Text>() {
        return Some(t.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(t) = inner_text(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}

/// Whether an OpenPGP send can go ahead (#133): a key of the user's own
/// for the From address (or the account's chosen key), and, to encrypt, a
/// public key for every recipient. The message names the first gap.
fn pgp_send_check(from: &str, chosen_key: Option<&str>, fields: &[&str], encrypt: bool) -> Result<(), String> {
    let gpg = crate::pgp::Gpg::system();
    let own = chosen_key
        .and_then(|f| crate::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::pgp::secret_key_for(&gpg, from));
    if own.is_none() {
        return Err(crate::i18n::i18n_f(
            "There is no OpenPGP key of your own for {addr}. Generate one under Settings, OpenPGP, \
             or choose a key in the account's settings.",
            &[("addr", from)],
        ));
    }
    if encrypt {
        for field in fields {
            for part in field.split(',') {
                let (_, addr) = crate::config::split_identity(part.trim());
                let addr = addr.trim();
                if addr.is_empty() {
                    continue;
                }
                if crate::pgp::public_key_for(&gpg, addr).is_none() {
                    return Err(crate::i18n::i18n_f(
                        "There is no OpenPGP key for {addr}. Ask them for their public key, or fetch it \
                         under Settings, OpenPGP.",
                        &[("addr", addr)],
                    ));
                }
            }
        }
    }
    Ok(())
}


/// Send Later presets (#145): `days` from today at `hour`:00, local time.
fn preset_time(days: i64, hour: u32) -> i64 {
    use chrono::{Duration, Local, TimeZone};
    let day = (Local::now() + Duration::days(days)).date_naive();
    let ndt = day.and_hms_opt(hour, 0, 0).unwrap_or_default();
    Local.from_local_datetime(&ndt).single().map(|t| t.timestamp()).unwrap_or_else(crate::datefmt::now)
}

/// The coming Monday at `hour`:00 local time (a Monday today means next week's).
fn next_monday(hour: u32) -> i64 {
    use chrono::Datelike;
    let today = chrono::Local::now().weekday().num_days_from_monday() as i64;
    let ahead = (7 - today) % 7;
    preset_time(if ahead == 0 { 7 } else { ahead }, hour)
}

/// The Send Later picker (#145): a calendar and an hour/minute pair, starting
/// from the scheduled time if there is one, else the next full hour. A time
/// already past is refused rather than queued to go at once by surprise.
fn pick_send_time(parent: Option<&gtk::Window>, current: Option<i64>, sender: relm4::Sender<ComposeInput>) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let start = match current {
        Some(t) => Local.timestamp_opt(t, 0).single().unwrap_or_else(Local::now),
        None => {
            let n = Local::now() + chrono::Duration::hours(1);
            n.with_minute(0).and_then(|n| n.with_second(0)).unwrap_or(n)
        }
    };
    let dialog = adw::MessageDialog::new(parent, Some(i18n("Send later").as_str()), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", &i18n("Schedule"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let calendar = gtk::Calendar::new();
    calendar.set_year(start.year());
    calendar.set_month(start.month0() as i32);
    calendar.set_day(start.day() as i32);
    bx.append(&calendar);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour.set_value(start.hour() as f64);
    hour.set_orientation(gtk::Orientation::Vertical);
    hour.set_wrap(true);
    let minute = gtk::SpinButton::with_range(0.0, 55.0, 5.0);
    minute.set_value((start.minute() / 5 * 5) as f64);
    minute.set_orientation(gtk::Orientation::Vertical);
    minute.set_wrap(true);
    row.append(&hour);
    row.append(&gtk::Label::new(Some(":")));
    row.append(&minute);
    bx.append(&row);
    dialog.set_extra_child(Some(&bx));
    let chosen = move || -> Option<i64> {
        let d = calendar.date();
        let ndt = chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
            .and_hms_opt(hour.value_as_int() as u32, minute.value_as_int() as u32, 0)?;
        Local.from_local_datetime(&ndt).single().map(|t| t.timestamp())
    };
    dialog.connect_response(None, move |dlg, resp| {
        if resp != "ok" {
            return;
        }
        match chosen() {
            Some(t) if t > crate::datefmt::now() => {
                let _ = sender.send(ComposeInput::SendAt(t));
            }
            _ => {
                // An explicit time in the past is a slip, not a request to
                // send at once.
                let d = adw::MessageDialog::new(
                    dlg.transient_for().as_ref(),
                    Some(i18n("That time has passed").as_str()),
                    Some(i18n("Choose a time later than now.").as_str()),
                );
                d.add_response("ok", &i18n("OK"));
                d.present();
            }
        }
    });
    dialog.present();
}

/// Whether keyboard focus is in a one-line text field of the composer — an
/// address row or the subject. Those keep their own native undo; everything
/// else in the composer answers to its history. The body is a WebView, not a
/// `GtkEditable`, so it never matches here.
fn focus_is_entry(root: &impl IsA<gtk::Widget>) -> bool {
    let mut node = root
        .as_ref()
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
    while let Some(widget) = node {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return true;
        }
        node = widget.parent();
    }
    false
}

/// A marker for the body's own text history, named the way the user would
/// name what it takes back.
fn body_entry() -> ComposeUndoEntry {
    ComposeUndoEntry { step: ComposeStep::Body, what: i18n("Typing") }
}

/// What to call an attachment change in the Undo menu: the file's own name,
/// or a count once there is more than one. `template` carries the `{name}`
/// placeholder and is translated here.
fn attachment_label(template: &str, path: &std::path::Path, count: usize) -> String {
    let name = if count > 1 {
        crate::i18n::ni18n_f(
            "{n} attachment",
            "{n} attachments",
            count as u32,
            &[("n", &count.to_string())],
        )
    } else {
        path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    };
    i18n_f(template, &[("name", &name)])
}
