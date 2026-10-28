//! Compose window: write a new message, reply, or forward.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{ComposeFormat, SignaturePosition};
use crate::i18n::{i18n, i18n_f};
use crate::ui::recipient_row::RecipientRow;
use crate::ui::rich_editor::{RichEditor, SourceKind};

mod attachments;
mod cloud;
mod format;
mod history;
mod host;
mod keys;
mod messages;
mod model;
mod send;
mod setup;
mod showcase;
mod signature;
mod subject;
mod suggest;
#[cfg(test)]
mod tests;
mod translate;
mod types;

use crate::ui::later::{next_monday, preset_time};
use cloud::not_signed_in;
pub(crate) use format::forget_zoom;
use format::{markdown_body, session_zoom, zoom_chip, SHOWCASE_PLAIN_LOG};
use host::{fold_on_first_map, pgp_tooltip, set_toggle_icon, size_for_host, COMPOSE_MIN_WIDTH};
pub(crate) use keys::composer_key;
pub(crate) use messages::{ComposeInput, ComposeOutput};
pub(crate) use model::Compose;
use send::{pick_send_time, start_autosave};
use setup::{add_toolbar_end_buttons, connect_editor};
use showcase::showcase_hooks;
use signature::sig_html;
use subject::wire_subject;
use suggest::{load_suggestions, wire_recipient_rows};
pub(crate) use types::{ComposeAccount, ComposeInit, ComposePrefill};

#[relm4::component(pub(crate))]
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

            // The overlay carries the drop surfaces shown while files are
            // dragged over the composer (drop_zones.rs).
            #[wrap(Some)]
            #[name = "drop_overlay"]
            set_child = &gtk::Overlay {
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
                        // Save as Template (#360) beside it, as Send Later
                        // is beside Send; a template being edited has the
                        // one way to be saved.
                        pack_start = &gtk::Box {
                            add_css_class: "linked",
                            gtk::Button {
                                set_label: &if model.template { i18n("Save Template") } else { i18n("Save Draft") },
                                set_tooltip_text: Some(
                                    if model.template { i18n("Save to Templates") } else { i18n("Save to Drafts") }.as_str()
                                ),
                                connect_clicked => ComposeInput::SaveDraft,
                            },
                            gtk::MenuButton {
                                set_icon_name: "pan-down-symbolic",
                                set_tooltip_text: Some(i18n("More ways to save").as_str()),
                                set_can_focus: false,
                                set_visible: !model.template,
                                #[wrap(Some)]
                                set_popover = &gtk::Popover {
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        add_css_class: "context-menu-list",
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            set_halign: gtk::Align::Fill,
                                            set_tooltip_text: Some(i18n("Keep a copy in Templates to start new messages from").as_str()),
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Save as Template"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SaveAsTemplate);
                                            },
                                        },
                                        // Only while editing an existing draft: the
                                        // message is moved to Trash, not saved, and
                                        // the editor closes.
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            set_halign: gtk::Align::Fill,
                                            set_tooltip_text: Some(i18n("Move this draft to Trash").as_str()),
                                            #[watch]
                                            set_visible: model.draft_origin.is_some(),
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Delete Draft"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::DeleteDraft);
                                            },
                                        },
                                    },
                                },
                            },
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
                                set_icon_name: "pan-down-symbolic",
                                add_css_class: "suggested-action",
                                set_tooltip_text: Some(i18n("Send options").as_str()),
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
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::ClearSendAt);
                                                sender.input_sender().emit(ComposeInput::Send);
                                            },
                                        },
                                        gtk::Separator {},
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Tomorrow morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 8)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Tomorrow afternoon ({time})", &[("time", &crate::datefmt::clock_label(13))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 13)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Monday morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(next_monday(8)));
                                            },
                                        },
                                        gtk::Separator {},
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Pick a date and time…"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::PickSendTime);
                                            },
                                        },
                                        gtk::Separator {},
                                        gtk::CheckButton {
                                            add_css_class: "context-menu-item",
                                            set_label: Some(&i18n("Request a read receipt")),
                                            set_tooltip_text: Some(&i18n("Not every mail app sends one.")),
                                            set_active: model.request_receipt,
                                            connect_toggled[sender] => move |b| {
                                                sender.input_sender().emit(ComposeInput::ToggleReceipt(b.is_active()));
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
                            set_icon_name: "view-more-horizontal-symbolic",
                            set_tooltip_text: Some(i18n("Actions").as_str()),
                            #[watch]
                            set_visible: model.narrow,
                            connect_clicked => ComposeInput::OverflowMenu,
                        },
                        // OpenPGP (#133): only offered where a gpg exists.
                        #[name = "encrypt_btn"]
                        pack_end = &gtk::ToggleButton {
                            set_icon_name: "channel-secure-symbolic",
                            add_css_class: "pgp-toggle",
                            #[watch]
                            set_tooltip_text: Some(&pgp_tooltip(&i18n("Encrypt with OpenPGP to every recipient's key"), model.encrypt)),
                            #[watch]
                            set_visible: crate::mail::pgp::available() && !model.narrow,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ToggleEncrypt(b.is_active()));
                            },
                        },
                        #[name = "sign_btn"]
                        pack_end = &gtk::ToggleButton {
                            set_icon_name: "security-high-symbolic",
                            add_css_class: "pgp-toggle",
                            #[watch]
                            set_tooltip_text: Some(&pgp_tooltip(&i18n("Sign with your OpenPGP key"), model.sign)),
                            #[watch]
                            set_visible: crate::mail::pgp::available() && !model.narrow,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ToggleSign(b.is_active()));
                            },
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "mail-attachment-symbolic",
                            set_tooltip_text: Some(i18n("Attach files").as_str()),
                            #[watch]
                            set_visible: !model.narrow,
                            connect_clicked => ComposeInput::AttachFiles,
                        },
                        // Cloud attachments (#144): only with an account set up.
                        pack_end = &gtk::Button {
                            set_icon_name: "cloud-symbolic",
                            set_tooltip_text: Some(i18n("Upload to cloud storage and share a link").as_str()),
                            #[watch]
                            set_visible: !model.cloud_accounts.is_empty() && !model.narrow,
                            #[watch]
                            set_sensitive: model.cloud_busy == 0,
                            connect_clicked => ComposeInput::CloudAttach,
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "x-office-address-book-symbolic",
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
                            set_icon_name: "pan-down-symbolic",
                            add_css_class: "fields-chevron",
                            set_tooltip_text: Some(i18n("Show From, To and Subject").as_str()),
                            set_can_focus: false,
                            #[watch]
                            set_visible: model.compact && !model.windowed,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ShowFields(b.is_active()));
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
                        gtk::Image { set_icon_name: Some("alarm-symbolic") },
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
                        gtk::Image { set_icon_name: Some("dialog-password-symbolic") },
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
                            // The From as text (#347): any name and address,
                            // sent through the account chosen above.
                            #[name = "custom_from_row"]
                            adw::EntryRow {
                                set_title: &i18n("From"),
                                set_input_purpose: gtk::InputPurpose::Email,
                                set_visible: false,
                            },
                            #[name = "to_row"]
                            RecipientRow {
                                set_title: &i18n("To"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "cc_row"]
                            RecipientRow {
                                set_title: &i18n("Cc"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "bcc_row"]
                            RecipientRow {
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

                        // Attachments in up to three equal columns (#299),
                        // filling left to right and wrapping.
                        #[name = "attach_box"]
                        gtk::FlowBox {
                            set_selection_mode: gtk::SelectionMode::None,
                            set_column_spacing: 6,
                            set_row_spacing: 6,
                            set_homogeneous: true,
                            set_max_children_per_line: 3,
                            add_css_class: "attach-flow",
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
            own_addresses,
            windowed,
            can_toggle,
            compact,
            decorations,
            format,
            signature_position,
        } = init;
        let in_reply_to = prefill.in_reply_to.clone();
        let references = prefill.references.clone();
        let forwards = prefill.forwards.clone();
        let draft_origin = prefill.draft_origin.clone();
        let template = prefill.template && draft_origin.is_some();
        let outbox_origin = prefill.outbox_origin;
        let prefill_attachments = prefill.attachments.clone();
        let handed_files =
            !prefill_attachments.is_empty() && draft_origin.is_none() && outbox_origin.is_none();
        let prefill_encrypt = prefill.encrypt;
        let request_receipt = prefill.receipt.unwrap_or(false);
        let send_at = prefill.send_at;
        let current_sig = accounts
            .get(selected)
            .map(|a| a.signature.clone())
            .unwrap_or_default();
        let sign_default = accounts.get(selected).is_some_and(|a| a.sign_default);

        let completion = gtk::Popover::new();
        completion.set_autohide(false); // don't steal focus from the entry
        completion.set_can_focus(false);
        completion.set_position(gtk::PositionType::Bottom);
        completion.add_css_class("menu");

        // Initial editor content: a blank line to type on, then the
        // signature and the quoted reply/forward (if any), in the order
        // the setting says (#237). A draft or a queued message already has
        // both and gets neither again: each reopening of a draft used to add
        // another blank line (#349). With Return set to start paragraphs the
        // line is a paragraph too, so the first Return splits it into two.
        let resumed = prefill.resumed || draft_origin.is_some() || outbox_origin.is_some();
        let mut content = String::from(
            match (resumed, crate::config::load_privacy().return_paragraph) {
                (true, _) => "",
                (false, true) => "<p><br></p>",
                (false, false) => "<div><br></div>",
            },
        );
        let sig_dashes = crate::config::load_privacy().signature_dashes;
        let sig = if !resumed && !prefill.no_signature && !current_sig.is_empty() {
            sig_html(&current_sig, sig_dashes)
        } else {
            String::new()
        };
        match signature_position {
            SignaturePosition::AboveQuote => {
                content.push_str(&sig);
                content.push_str(&prefill.body_html);
            }
            SignaturePosition::BelowQuote => {
                content.push_str(&prefill.body_html);
                content.push_str(&sig);
            }
        }
        // Blocking the quote's pictures must not blank the user's own
        // signature, whichever identity it ends up sent from.
        let remote_images = prefill.block_remote_images.then(|| {
            accounts
                .iter()
                .flat_map(|a| a.signatures.iter())
                .flat_map(|sig| super::rich_editor::remote_image_urls(&sig.html))
                .collect::<Vec<_>>()
        });
        let editor = RichEditor::new(
            &content,
            remote_images.as_deref(),
            format == ComposeFormat::Plain,
        );
        editor.set_formatting_visible(format == ComposeFormat::Rich);
        editor.set_zoom(session_zoom());
        let zoom_chip = zoom_chip(&sender);
        editor.toolbar_end().prepend(&zoom_chip);
        {
            let s = sender.input_sender().clone();
            editor.widget.add_controller(super::ctrl_scroll_zoom(
                || true,
                move |step| {
                    let _ = s.send(ComposeInput::Zoom(step));
                },
            ));
        }
        // A source format starts from the same content, written out as
        // source: a reply's quoted original becomes `> ` lines in Markdown,
        // or the HTML it already was.
        match format {
            ComposeFormat::Markdown => {
                editor.set_source(SourceKind::Markdown, &markdown_body(&content))
            }
            ComposeFormat::Html => editor.set_source(
                SourceKind::Html,
                &crate::mail::markdown::pretty_html(&content),
            ),
            ComposeFormat::Rich | ComposeFormat::Plain => {}
        }
        // HYLKI_SHOWCASE_PLAIN_LOG also logs the body a composer opens with
        // (demo only), to check a reply started in plain text (#436).
        if std::env::var_os("HYLKI_SHOWCASE_PLAIN_LOG").is_some()
            && std::env::var_os("HYLKI_DEMO").is_some()
        {
            let ed = editor.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(2500), move || {
                ed.eval(SHOWCASE_PLAIN_LOG, move |v| {
                    tracing::info!("showcase plain: opened {format:?} {v}")
                });
            });
        }
        let (format_btn, preview_btn, preview_content, translate_btn, signature_btn) =
            add_toolbar_end_buttons(
                &editor,
                format,
                accounts
                    .get(selected)
                    .is_some_and(|a| !a.signatures.is_empty()),
                &sender,
            );

        connect_editor(&editor, &sender);

        let mut model = Compose {
            accounts,
            editor,
            current_sig,
            sig_dashes,
            signature_position,
            attachments: prefill_attachments,
            attach_thumbs: Default::default(),
            suggestions: Vec::new(),
            graph_directories: Default::default(),
            own_mail: Default::default(),
            from_account: 0,
            directory_matches: Vec::new(),
            directory_generation: 0,
            directory_errors_shown: std::collections::HashSet::new(),
            completion,
            completion_field: None,
            completion_list: None,
            completion_selected: 0,
            completion_count: 0,
            completion_open: std::rc::Rc::new(std::cell::Cell::new(false)),
            in_reply_to,
            references,
            forwards,
            draft_origin,
            template,
            outbox_origin,
            compose_id,
            windowed,
            can_toggle,
            // A compact (fields-hidden) pane only makes sense once it is
            // addressed: replies arrive with To filled, forwards do not.
            compact: compact && !prefill.to.trim().is_empty(),
            decorations,
            fields_shown: crate::config::load_privacy().reply_fields,
            custom_from: false,
            narrow: false,
            // Files handed in from outside are something to lose: closing
            // asks first, as it would once the user had attached them.
            fields_dirty: handed_files,
            asking_discard: false,
            sign: false,
            sign_touched: false,
            sign_expected: None,
            format,
            zoom_chip,
            format_btn,
            translate_btn,
            signature_btn,
            translating: false,
            autosaving: false,
            autosaved: None,
            autosave_pending: None,
            autosave_made: false,
            // A template is saved when its editor says so: an automatic save
            // would put the half-edited one in its place.
            autosave_off: template,
            tr_subject: None,
            preview_btn,
            preview_content,
            encrypt: false,
            request_receipt,
            pgp_checking: false,
            pgp_cleared: None,
            send_at,
            cloud_accounts: crate::services::cloud::load_enabled_accounts(),
            drop_zones: None,
            cloud_links: Vec::new(),
            cloud_busy: 0,
            cloud_passwords: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        };
        model.from_account = model
            .accounts
            .get(selected)
            .map(|a| a.id)
            .unwrap_or_default();
        let widgets = view_output!();
        if prefill_encrypt && crate::mail::pgp::available() {
            // Through the buttons, so the toggles and the model agree.
            widgets.encrypt_btn.set_active(true);
        } else if sign_default && crate::mail::pgp::available() {
            // A draft does not record whether it was signed, so a reopened
            // one starts as its account says, like any new message.
            model.sign_expected = Some(true);
            widgets.sign_btn.set_active(true);
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

        fold_on_first_map(&root, &widgets, &sender);

        model.show_initial_rows(&widgets, &prefill);

        model.fill_from_row(&widgets, &prefill, selected, &sender);

        widgets.to_row.set_text(&prefill.to);
        widgets.cc_row.set_text(&prefill.cc);
        widgets.bcc_row.set_text(&prefill.bcc);
        widgets.subject_row.set_text(&prefill.subject);
        if !model.attachments.is_empty() {
            model.rebuild_attachments(&widgets.attach_box, &sender);
        }

        model.install_drop_zones(&root, &widgets, &sender);

        if !prefill.inline_files.is_empty() {
            model.editor.insert_files(&prefill.inline_files);
        }
        // Files handed in over the size limit (Settings → System → GNOME
        // Files): straight into the upload dialog, once the composer has a
        // window for it to be transient for.
        if !prefill.cloud_uploads.is_empty() {
            let s = sender.clone();
            let paths = prefill.cloud_uploads.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                s.input_sender().emit(ComposeInput::CloudPicked(paths));
            });
        }

        showcase_hooks(&root, &model.editor, &sender);

        load_suggestions(own_addresses, &sender);

        wire_recipient_rows(&widgets, &sender);

        wire_subject(&widgets, &sender);

        model.install_keys(&root, &sender);

        start_autosave(&sender);

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        // Each arm handles its message in full, or hands it to an `on_*`
        // method that returns early where it has to; every message ends at
        // `update_view` below.
        {
            match message {
                ComposeInput::Cancel => self.on_cancel(&sender),

                ComposeInput::ConfirmDiscard => self.on_confirm_discard(&sender, root),

                ComposeInput::KeepEditing => self.asking_discard = false,

                ComposeInput::Discard => self.on_discard(&sender),

                ComposeInput::SendAt(at) => {
                    self.send_at = Some(at);
                    sender.input_sender().emit(ComposeInput::Send);
                }

                ComposeInput::ClearSendAt => {
                    self.send_at = None;
                }

                ComposeInput::FocusField(field) => self.on_focus_field(field, widgets),

                ComposeInput::ShowFields(on) => {
                    self.fields_shown = on;
                    widgets
                        .fields_list
                        .set_visible(!(self.compact && !self.windowed) || on);
                }

                ComposeInput::SetNarrow(narrow) => self.on_set_narrow(narrow, widgets, root),

                ComposeInput::OverflowMenu => self.on_overflow_menu(widgets, &sender),

                ComposeInput::CloudAttach => self.on_cloud_attach(&sender, root),

                ComposeInput::DroppedFiles(choice, paths) => {
                    self.on_dropped_files(choice, paths, &sender)
                }

                ComposeInput::CloudPicked(paths) => self.on_cloud_picked(paths, &sender, root),

                ComposeInput::CloudUpload {
                    paths,
                    account,
                    link_password,
                } => self.on_cloud_upload(paths, account, link_password, &sender),

                ComposeInput::CloudUploadSignedIn {
                    paths,
                    account,
                    link_password,
                    password,
                } => match password {
                    Some(password) => self.on_cloud_upload_signed_in(
                        paths,
                        account,
                        link_password,
                        password,
                        widgets,
                        &sender,
                    ),
                    None => not_signed_in(root, &account.name),
                },

                ComposeInput::CloudUploaded { name, result } => {
                    self.on_cloud_uploaded(name, result, widgets, &sender, root)
                }

                ComposeInput::RemoveCloudLink(i) => self.on_remove_cloud_link(i, widgets, &sender),

                ComposeInput::CopyCloudPasswords => self.on_copy_cloud_passwords(),

                ComposeInput::PickSendTime => {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    pick_send_time(parent.as_ref(), self.send_at, sender.input_sender().clone());
                }

                ComposeInput::DeleteDraft => self.on_delete_draft(&sender),

                ComposeInput::ToggleWindowed => {
                    let _ = sender.output(ComposeOutput::ToggleWindow(self.compose_id));
                }

                ComposeInput::SetDecorations(on) => self.on_set_decorations(on, widgets),

                ComposeInput::SetWindowed(windowed) => self.on_set_windowed(windowed, widgets),

                ComposeInput::FocusInitial => self.focus_initial(widgets),

                ComposeInput::MarkFieldsDirty => self.fields_dirty = true,

                ComposeInput::SaveDraftIfDirty => self.on_save_draft_if_dirty(&sender),

                ComposeInput::OpenContacts => self.on_open_contacts(widgets, &sender, root),

                ComposeInput::AttachFiles => self.on_attach_files(&sender, root),

                ComposeInput::AddAttachments(paths) => {
                    self.on_add_attachments(paths, widgets, &sender)
                }

                ComposeInput::RemoveAttachment(i) => self.on_remove_attachment(i, widgets, &sender),

                ComposeInput::Zoom(step) => self.on_zoom(step),

                ComposeInput::History { redo } => {
                    self.history_step(redo, widgets, &sender);
                }

                ComposeInput::ShowcaseHistory(step) => {
                    self.showcase_history(step, widgets, &sender);
                }

                ComposeInput::ShowcaseFrom(n) => {
                    widgets.from_row.set_selected(n);
                }

                ComposeInput::BodyHistory(can_undo) => self.on_body_history(can_undo, &sender),

                ComposeInput::CustomFrom(on) => self.on_custom_from(on, widgets),

                ComposeInput::AccountChanged => self.on_account_changed(widgets),

                ComposeInput::InsertSignature(place) => self.on_insert_signature(place, widgets),

                ComposeInput::SignatureClicked => self.on_signature_clicked(widgets, &sender),

                ComposeInput::UseSignature(choice) => self.on_use_signature(choice, widgets),

                ComposeInput::SignatureMenu => self.on_signature_menu(&sender),

                ComposeInput::Suggest(field) => self.on_suggest(field, widgets, &sender),

                ComposeInput::DirectoriesKnown { graph, own_mail } => {
                    self.graph_directories = std::sync::Arc::new(graph);
                    self.own_mail = own_mail;
                }

                ComposeInput::SuggestionsLoaded(loaded) => {
                    self.on_suggestions_loaded(loaded, widgets)
                }

                ComposeInput::DirectoryLookup {
                    field,
                    token,
                    generation,
                } => self.on_directory_lookup(field, token, generation, &sender),

                ComposeInput::DirectoryResults {
                    field,
                    token,
                    generation,
                    matches,
                    failed,
                } => {
                    self.report_directory_failures(failed, &sender);
                    self.on_directory_results(field, token, generation, matches, widgets);
                }

                ComposeInput::AddSuggestions { account_id, new } => {
                    self.on_add_suggestions(account_id, new)
                }

                ComposeInput::CompletionMove(delta) => self.on_completion_move(delta),

                ComposeInput::CompletionAccept => self.on_completion_accept(widgets),

                ComposeInput::CompletionClose => {
                    self.completion_open.set(false);
                    self.completion.popdown();
                }

                ComposeInput::ToggleSign(on) => self.on_toggle_sign(on),

                ComposeInput::FormatMenu => self.on_format_menu(&sender),

                ComposeInput::TranslateMenu => self.on_translate_menu(&sender),

                ComposeInput::ShowTranslateMenu(found) => {
                    self.on_show_translate_menu(found, &sender)
                }

                ComposeInput::TranslateTo(target) => self.on_translate_to(target, &sender, root),

                ComposeInput::TranslateBegun { found, target } => {
                    self.on_translate_begun(found, target, widgets, &sender, root)
                }

                ComposeInput::TranslateToggle { original } => {
                    self.on_translate_toggle(original, widgets, &sender)
                }

                ComposeInput::Translated {
                    result,
                    how,
                    subject,
                } => self.on_translated(result, how, subject, widgets, root),

                ComposeInput::SetFormat(to) => self.on_set_format(to, &sender),

                ComposeInput::AskPlain => self.on_ask_plain(&sender, root),

                ComposeInput::ApplyFormat(to) => self.on_apply_format(to, &sender),

                ComposeInput::LoadAs { from, to, body } => self.on_load_as(from, to, body),

                ComposeInput::TogglePreview(on) => self.on_toggle_preview(on, &sender),

                ComposeInput::ShowPreview(body) => {
                    let html = match self.format {
                        ComposeFormat::Markdown => {
                            crate::mail::direction::stamp(&crate::mail::markdown::to_html(&body))
                        }
                        ComposeFormat::Html => crate::mail::direction::stamp(&body),
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
                ComposeInput::ToggleReceipt(on) => {
                    self.request_receipt = on;
                    self.fields_dirty = true;
                }
                ComposeInput::PgpChecked { checked, result } => {
                    self.on_pgp_checked(checked, result, widgets, &sender)
                }
                ComposeInput::Send => self.on_send(widgets, &sender),

                ComposeInput::SendBody(fields) => self.on_send_body(fields, &sender),

                ComposeInput::SaveDraft => self.on_save_draft(false, widgets, &sender),
                ComposeInput::SaveAsTemplate => self.on_save_draft(true, widgets, &sender),

                ComposeInput::SaveDraftBody {
                    fields,
                    as_template,
                } => self.on_save_draft_body(fields, as_template, &sender),

                ComposeInput::AutoSave => self.on_auto_save(&sender),

                ComposeInput::AutoSaveRead => self.on_auto_save_read(widgets, &sender),

                ComposeInput::AutoSaveBody(fields) => self.on_auto_save_body(fields, &sender),

                ComposeInput::Autosaved { saved, origin } => self.on_autosaved(saved, origin),
            }
        }
        // Overriding `update_with_view` takes over relm4's default, which
        // ran `update_view` after every message; without this the `#[watch]`
        // bindings above only ever hold their `init` values.
        self.update_view(widgets, sender);
    }
}

impl Compose {}
