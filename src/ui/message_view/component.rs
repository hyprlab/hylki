//! The reading pane as a relm4 component: its widget tree (view!), building
//! it, and `update`, which routes each input to its `on_*` method.

use super::init::link_preview_plaque;
use super::types::{MessageViewInput, MessageViewOutput};
use super::webview::new_webview;
use super::MessageView;
use crate::i18n::{i18n, i18n_f};
use adw::prelude::*;
use relm4::prelude::*;

#[relm4::component(pub(crate))]
impl Component for MessageView {
    type Init = ();
    type Input = MessageViewInput;
    type Output = MessageViewOutput;
    type CommandOutput = ();

    view! {
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            add_named[Some("empty")] = &adw::StatusPage {
                // Drafts open in the editor, so the empty pane says so there.
                #[watch]
                set_icon_name: Some(if model.checked.0 > 0 {
                    "object-select-symbolic"
                } else if model.drafts_view {
                    "document-edit-symbolic"
                } else {
                    "hylki-mail-read-symbolic"
                }),
                #[watch]
                set_title: &model.empty_title(),
                #[watch]
                set_description: Some(if model.checked.0 > 0 {
                    i18n("The actions above the list apply to all of them.")
                } else if model.drafts_view {
                    i18n("Choose a draft from the list to edit it here.")
                } else {
                    i18n("Choose a message from the list to read it here.")
                }.as_str()),
            },

            add_named[Some("message")] = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "reader-pane",

                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.spoof_alert_shown(),

                    gtk::Box {
                        add_css_class: "spoof-alert",
                        set_spacing: 8,

                        gtk::Image { set_icon_name: Some("dialog-warning-symbolic") },
                        gtk::Label {
                            #[watch]
                            set_label: model
                                .sender_check
                                .as_ref()
                                .map(|c| c.summary.as_str())
                                .unwrap_or_default(),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                    },
                },

                #[name = "remote_alert"]
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.blocked && model.show_banner,

                    gtk::Box {
                        add_css_class: "remote-alert",
                        set_spacing: 8,

                        gtk::Image { set_icon_name: Some("security-high-symbolic") },
                        gtk::Label {
                            set_label: &i18n("Remote content (images, trackers) is blocked to protect your privacy."),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            // Ask for the full single-line width and wrap only
                            // when the pane really is too narrow — without this
                            // a wrapping label requests minimal width and folds
                            // to two lines even with room to spare.
                            set_natural_wrap_mode: gtk::NaturalWrapMode::None,
                            set_xalign: 0.0,
                        },
                        gtk::Button {
                            set_label: &i18n("Load"),
                            set_valign: gtk::Align::Center,
                            connect_clicked => MessageViewInput::LoadRemoteOnce,
                        },
                        gtk::Button {
                            set_label: &i18n("Always allow sender"),
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_tooltip_text: model.current.as_ref().map(|m| m.from_addr.as_str()),
                            connect_clicked => MessageViewInput::AllowSenderAlways,
                        },
                    },
                },

                // In-message find (#103): WebKit's FindController does the
                // matching and highlighting; this bar just drives it.
                #[name = "find_bar"]
                gtk::Revealer {
                    #[watch]
                    set_transition_type: if model.find_bottom {
                        gtk::RevealerTransitionType::SlideUp
                    } else {
                        gtk::RevealerTransitionType::SlideDown
                    },
                    #[watch]
                    set_reveal_child: model.find_open,

                    gtk::Box {
                        add_css_class: "reader-find",
                        #[watch]
                        set_class_active: ("bottom", model.find_bottom),
                        set_spacing: 6,

                        #[name = "find_entry"]
                        gtk::SearchEntry {
                            set_hexpand: true,
                            set_placeholder_text: Some(i18n("Find in message").as_str()),
                            connect_search_changed[sender] => move |entry| {
                                sender.input_sender().emit(MessageViewInput::FindChanged(entry.text().to_string()));
                            },
                            connect_activate[sender] => move |_| {
                                sender.input_sender().emit(MessageViewInput::FindNext);
                            },
                            connect_stop_search[sender] => move |_| {
                                sender.input_sender().emit(MessageViewInput::CloseFind);
                            },
                            add_controller = gtk::EventControllerFocus {
                                connect_leave[sender] => move |ctl| {
                                    let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                    let sender = sender.clone();
                                    crate::ui::close_if_focus_left_row(&entry, move || sender.input_sender().emit(MessageViewInput::CloseFind));
                                },
                            },
                        },

                        gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_label: &match model.find_matches {
                                Some((_, 0)) => i18n("No matches"),
                                Some((cur, total)) => i18n_f("{cur} of {total}", &[("cur", &cur.to_string()), ("total", &total.to_string())]),
                                None => String::new(),
                            },
                        },

                        gtk::Button {
                            set_icon_name: "pan-up-symbolic",
                            set_tooltip_text: Some(i18n("Previous match").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::FindPrev,
                        },
                        gtk::Button {
                            set_icon_name: "pan-down-symbolic",
                            set_tooltip_text: Some(i18n("Next match").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::FindNext,
                        },
                        gtk::Button {
                            set_icon_name: "window-close-symbolic",
                            set_tooltip_text: Some(i18n("Close find").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::CloseFind,
                        },
                    },
                },

                gtk::Box {
                    add_css_class: "reader-header",
                    // The header's top padding is cut to the 4px that puts the
                    // account chip level with the message list's first row.
                    // That only reads as alignment when the header is against
                    // the toolbar; under a bar it reads as cramped, so give it
                    // its air back whenever one is showing.
                    #[watch]
                    set_class_active: (
                        "under-bar",
                        model.spoof_alert_shown()
                            || (model.blocked && model.show_banner)
                            || (model.find_open && !model.find_bottom),
                    ),
                    // Under the split reply there is no list row to line up
                    // with either, only the composer's grab strip: 14px more
                    // air than the aligned case.
                    #[watch]
                    set_class_active: ("under-split", model.under_split),
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 12,
                        #[watch]
                        set_visible: model.account_line_shown(),

                        // Stays when the chip is hidden in Settings: it is
                        // what pushes the switches to the line's right end.
                        gtk::Box {
                            set_halign: gtk::Align::Start,
                            set_hexpand: true,
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_visible: model.account_name.is_some(),
                            #[name = "account_chip"]
                            gtk::Label {
                                #[watch]
                                set_visible: model.account_chip_shown,
                                #[watch]
                                set_label: model.account_name.as_deref().unwrap_or_default(),
                                add_css_class: "account-chip",
                                add_css_class: "hylki-account-chip",
                            },
                        },

                        // Message zoom (Ctrl+ / Ctrl-): the percentage, shown
                        // only while it is away from the default Settings
                        // starts at, so a chosen size says nothing about
                        // itself. A click goes back to that default.
                        gtk::Label {
                            #[watch]
                            set_label: &format!("{}%", model.zoom),
                            #[watch]
                            set_visible: model.current.is_some() && model.zoom != model.zoom_default,
                            set_valign: gtk::Align::Center,
                            add_css_class: "caption",
                            add_css_class: "hylki-zoom-chip",
                            set_cursor_from_name: Some("pointer"),
                            set_tooltip_text: Some(i18n("Message zoom. Ctrl+ and Ctrl-, or Ctrl and the scroll wheel, change it; a click here, or Ctrl+0, puts it back to the default from Settings.").as_str()),
                            add_controller = gtk::GestureClick {
                                connect_released[sender] => move |_, _, _, _| {
                                    let _ = sender.output(MessageViewOutput::Zoom(0));
                                },
                            },
                        },

                        // Light / Dark Mode: the message on screen in the
                        // other scheme, for that message alone. Sits just
                        // before the Reader View switch, the highlights icon
                        // for a label.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 6,
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            add_css_class: "reader-toggle",
                            set_tooltip_text: Some(i18n("Show this message in light or dark mode. Other messages are not affected.").as_str()),
                            #[watch]
                            set_visible: model.current.is_some() && model.theme_switch_shown,

                            gtk::Image {
                                set_icon_name: Some("highlights-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Switch {
                                set_valign: gtk::Align::Center,
                                update_property: &[gtk::accessible::Property::Label(&i18n("Light / Dark Mode"))],
                                #[watch]
                                set_active: model.message_flipped(),
                                // Setting it to what is already shown (a new
                                // message, a theme change) does nothing.
                                connect_active_notify[sender] => move |sw| {
                                    sender.input_sender().emit(MessageViewInput::SetMessageFlipped(sw.is_active()));
                                },
                            },
                        },

                        // Reader View: the message(s) as content alone. A
                        // switch behind a pair of reading glasses on the
                        // account chip's line, at its right end — part of
                        // the message header, not the action row.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 6,
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            add_css_class: "reader-toggle",
                            set_tooltip_text: Some(i18n("Reader View shows a message as its text alone, in one plain format, without the sender's layout, colors and fonts.").as_str()),
                            #[watch]
                            set_visible: model.current.is_some() && model.reader_switch_shown,

                            gtk::Image {
                                set_icon_name: Some("glasses-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Switch {
                                set_valign: gtk::Align::Center,
                                update_property: &[gtk::accessible::Property::Label(&i18n("Reader View"))],
                                #[watch]
                                set_active: model.reader_mode,
                                // The owner decides what a flip does, so it
                                // never renders on its own: the main window
                                // saves the choice and hands it back to every
                                // reader, a message's own window applies it
                                // to itself alone.
                                connect_active_notify[sender] => move |sw| {
                                    let _ = sender.output(MessageViewOutput::ReaderMode(sw.is_active()));
                                },
                            },
                        },
                    },

                    gtk::Label {
                        #[watch]
                        set_label: &model.current.as_ref().map(|m| model.shown_subject(m)).unwrap_or_default(),
                        set_halign: gtk::Align::Start,
                        set_wrap: true,
                        // Break mid-word for unbreakable tokens (e.g. an
                        // undecodable subject or a long URL) so an extreme
                        // subject can never force the pane — and with it the
                        // window controls — wider than the screen.
                        set_wrap_mode: gtk::pango::WrapMode::WordChar,
                        set_xalign: 0.0,
                        set_selectable: true,
                        add_css_class: "reader-subject",
                    },

                    // Tag chips (#71) for a lone full-bleed message, whose
                    // header is these widgets; cards draw theirs in the
                    // document. Filled by post_view.
                    #[local_ref]
                    header_tags -> gtk::Box {
                        set_spacing: 4,
                        set_halign: gtk::Align::Start,
                        #[watch]
                        set_visible: model.header_tags_shown(),
                    },
                },


                #[name = "body_stack"]
                gtk::Stack {
                    // The message's ground, painted on the stack itself so every
                    // page sits on it — the spinner box is centred and paints
                    // only its own few square inches.
                    add_css_class: "reader-cover",
                    // Both pages sit on that same ground, so a short dissolve
                    // between them reads as the message arriving rather than as
                    // the hard cut a stack does by default.
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    set_transition_duration: 120,
                    set_vexpand: true,
                    #[watch]
                    set_visible_child_name: model.body_page(),

                    #[name = "loading_page"]
                    add_named[Some("loading")] = &gtk::Box {
                        add_css_class: "reader-loading",
                        set_focusable: true,
                        set_orientation: gtk::Orientation::Vertical,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_spacing: 14,

                        gtk::Spinner {
                            set_spinning: true,
                            set_width_request: 36,
                            set_height_request: 36,
                        },
                        gtk::Label {
                            // Dimming comes from the cover's own foreground, which
                            // is picked for the message theme — `dim-label` would
                            // fade it against the app's instead.
                            set_label: &i18n("Loading…"),
                        },
                    },
                },
            },

            #[watch]
            set_visible_child_name: if model.current.is_some() { "message" } else { "empty" },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let webview = new_webview();
        let link_preview = link_preview_plaque(&webview);
        let mut model = MessageView::initial(webview, &link_preview);
        model.watch_loads(&sender);
        model.watch_context_menu(&sender);
        model.watch_bridge(&sender);
        model.watch_theme(&sender);

        let header_tags = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let widgets = view_output!();
        model.find_entry = Some(widgets.find_entry.clone());
        model.find_bar = Some((
            widgets.find_bar.clone(),
            widgets.remote_alert.clone().upcast(),
        ));
        model.loading_page = Some(widgets.loading_page.clone());
        model.account_chip = Some(widgets.account_chip.clone());
        model.cover = Some(widgets.body_stack.clone());
        model.apply_webview_bg(model.effective_dark());
        let body_overlay = gtk::Overlay::new();
        body_overlay.set_child(Some(&model.webview));
        body_overlay.add_overlay(&model.page_fade.cover);
        body_overlay.add_overlay(&link_preview);
        let zoom_sender = sender.clone();
        body_overlay.add_controller(crate::ui::ctrl_scroll_zoom(
            || true,
            move |step| {
                let _ = zoom_sender.output(MessageViewOutput::Zoom(step));
            },
        ));
        widgets.body_stack.add_named(&body_overlay, Some("body"));
        widgets.body_stack.set_visible_child_name("body");
        ComponentParts { model, widgets }
    }

    fn post_view() {
        self.sync_header_tags(&widgets.header_tags);
        self.hold_focus_while_loading();
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            MessageViewInput::Show {
                thread,
                allow_remote,
                account_name,
                account_color,
                loading,
                primary,
                folder_labels,
                instant,
                sent,
            } => self.on_show(
                thread,
                allow_remote,
                account_name,
                account_color,
                loading,
                primary,
                folder_labels,
                instant,
                sent,
            ),
            MessageViewInput::LoadRemoteOnce => self.on_load_remote_once(&sender),
            MessageViewInput::ToggleRemote { account_id, id } => {
                self.on_toggle_remote(account_id, id, &sender)
            }
            MessageViewInput::AllowSenderAlways => self.on_allow_sender_always(&sender),
            MessageViewInput::Print => self.on_print(),
            MessageViewInput::PrintPreview => self.on_print_preview(),
            MessageViewInput::FacesChanged => self.render(),

            MessageViewInput::ThemeChanged => self.on_theme_changed(),
            MessageViewInput::SetAlwaysShowRecipients(on) => {
                self.always_show_recipients = on;
            }
            MessageViewInput::SetFoldMessages(fold) => self.on_set_fold_messages(fold),
            MessageViewInput::SetFoldLook { preview_lines } => self.on_set_fold_look(preview_lines),
            MessageViewInput::Folded {
                account_id,
                id,
                folded,
            } => self.on_folded(account_id, id, folded),
            MessageViewInput::Mapped => self.on_mapped(),
            MessageViewInput::SetChecked {
                count,
                conversations,
            } => self.checked = (count, conversations),
            MessageViewInput::FoldAll(folded) => self.on_fold_all(folded),
            MessageViewInput::SetSingleMessageCard(on) => {
                self.single_message_card = on;
            }
            MessageViewInput::SetPgpLabels(on) => self.on_set_pgp_labels(on),
            MessageViewInput::SetCardAttachments(map) => self.on_set_card_attachments(map),
            MessageViewInput::SetCardAttachmentsShown(on) => self.on_set_card_attachments_shown(on),
            MessageViewInput::SetAttachmentDrawer(on) => self.on_set_attachment_drawer(on),
            MessageViewInput::ScrollToAttachments { account_id, id } => {
                self.on_scroll_to_attachments(account_id, id)
            }
            MessageViewInput::AttachmentAction {
                account_id,
                id,
                index,
                save,
            } => self.on_attachment_action(account_id, id, index, save, &sender),
            MessageViewInput::SetBannerShown(show) => {
                self.show_banner = show;
            }
            MessageViewInput::SetSpoofBannerShown(show) => {
                self.show_spoof_banner = show;
            }
            MessageViewInput::SetUnderSplit(under) => {
                self.under_split = under;
            }
            MessageViewInput::SetContentTheme(o) => self.on_set_content_theme(o),
            MessageViewInput::SetReaderStyle(style) => self.on_set_reader_style(style),
            MessageViewInput::SetReaderMode(on) => {
                self.set_reader_mode(on);
            }
            MessageViewInput::SetMessageFlipped(on) => self.on_set_message_flipped(on),
            MessageViewInput::SetZoomDefault(percent) => {
                self.zoom_default = percent;
            }
            MessageViewInput::SetZoom(percent) => self.on_set_zoom(percent),
            MessageViewInput::SetReaderSwitchShown(on) => {
                self.reader_switch_shown = on;
            }
            MessageViewInput::SetThemeSwitchShown(on) => {
                self.theme_switch_shown = on;
            }
            MessageViewInput::SetAccountChipShown(on) => {
                self.account_chip_shown = on;
            }
            MessageViewInput::SetReaderDefault(policy) => self.on_set_reader_default(policy),
            MessageViewInput::ToggleSenderStyle { account_id, id } => {
                self.on_toggle_sender_style(account_id, id)
            }
            MessageViewInput::SetSenderCheck(check) => {
                self.sender_check = Some(*check);
            }

            MessageViewInput::SenderCheckFor {
                account_id,
                id,
                check,
            } => self.on_sender_check_for(account_id, id, check),
            MessageViewInput::SetUnsubscribed(lists) => self.on_set_unsubscribed(lists),
            MessageViewInput::SetReceiptMarks(marks) => self.on_set_receipt_marks(marks),
            MessageViewInput::Translate { account_id, id } => {
                self.on_translate(account_id, id, &sender)
            }
            MessageViewInput::Translated {
                account_id,
                id,
                result,
            } => self.on_translated(account_id, id, result),
            MessageViewInput::Unsubscribe { account_id, id } => {
                self.on_unsubscribe(account_id, id, &sender)
            }
            MessageViewInput::ReceiptAnswer {
                account_id,
                id,
                send,
            } => self.on_receipt_answer(account_id, id, send, &sender),
            MessageViewInput::UnsubscribeState {
                account_id,
                id,
                state,
            } => self.on_unsubscribe_state(account_id, id, state),
            MessageViewInput::SetIdentities(identities) => self.on_set_identities(identities),
            MessageViewInput::SetInviteAnswers(answers) => self.on_set_invite_answers(answers),
            MessageViewInput::InviteAction {
                account_id,
                id,
                action,
            } => self.on_invite_action(account_id, id, action, &sender),
            MessageViewInput::SenderInfoAt {
                account_id,
                id,
                rect,
                page_width,
            } => self.on_sender_info_at(account_id, id, rect, page_width, &sender),
            MessageViewInput::PgpFetchKey { account_id, id } => {
                self.on_pgp_fetch_key(account_id, id, &sender)
            }
            MessageViewInput::PgpTrustKey { account_id, id } => {
                self.on_pgp_trust_key(account_id, id, &sender)
            }
            MessageViewInput::PgpTrustConfirmed {
                account_id,
                id,
                fingerprint,
            } => self.on_pgp_trust_confirmed(account_id, id, fingerprint, &sender),
            MessageViewInput::PgpDone {
                account_id,
                id,
                result,
            } => self.on_pgp_done(account_id, id, result, &sender),
            MessageViewInput::Rendered => self.on_rendered(),
            MessageViewInput::SuppressAutoRead { account_id, id } => {
                self.on_suppress_auto_read(account_id, id)
            }
            MessageViewInput::ClearCards => self.on_clear_cards(&sender),
            MessageViewInput::CardClicked {
                account_id,
                id,
                mode,
            } => self.on_card_clicked(account_id, id, mode, &sender),
            MessageViewInput::CopySelection => self.on_copy_selection(),
            MessageViewInput::SelectAllCards => self.on_select_all_cards(&sender),
            MessageViewInput::StepCard(delta) => self.on_step_card(delta, &sender),
            MessageViewInput::FocusBody => self.on_focus_body(),
            MessageViewInput::ComposeTo(addr) => {
                let _ = sender.output(MessageViewOutput::ComposeTo(addr));
            }
            MessageViewInput::AddContact(addr) => {
                let _ = sender.output(MessageViewOutput::AddContactAddr(addr));
            }
            MessageViewInput::SetSelectedCards(keys) => self.on_set_selected_cards(keys),
            MessageViewInput::FrameSized {
                account_id,
                id,
                height,
            } => self.on_frame_sized(account_id, id, height),
            MessageViewInput::MarkSeen { account_id, id } => {
                self.on_mark_seen(account_id, id, &sender)
            }
            MessageViewInput::CardAction {
                action,
                account_id,
                id,
            } => self.on_card_action(action, account_id, id, &sender),
            MessageViewInput::CardMenuAt { key, x, y, hit } => {
                self.on_card_menu_at(key, x, y, hit, &sender)
            }
            MessageViewInput::CardMenuFrom {
                account_id,
                id,
                x,
                y,
                page_width,
            } => self.on_card_menu_from(account_id, id, x, y, page_width, &sender),
            MessageViewInput::CardMoveAt {
                account_id,
                id,
                x,
                y,
                page_width,
            } => self.on_card_move_at(account_id, id, x, y, page_width, &sender),
            MessageViewInput::SetCardActionsMode {
                hover_toggle,
                hover_auto,
            } => self.on_set_card_actions_mode(hover_toggle, hover_auto),
            MessageViewInput::SetPaletteCollapse(secs) => self.on_set_palette_collapse(secs),
            MessageViewInput::SetCardPaletteMenu(on) => self.on_set_card_palette_menu(on),
            MessageViewInput::ClearDot { account_id, id } => self.on_clear_dot(account_id, id),
            MessageViewInput::SetReadMark(policy) => self.on_set_read_mark(policy),
            MessageViewInput::OpenFind => self.on_open_find(&sender),
            MessageViewInput::CloseFind => self.on_close_find(),
            MessageViewInput::SetFindBottom(on) => self.on_set_find_bottom(on),
            MessageViewInput::DragCard(key) => self.on_drag_card(key),
            MessageViewInput::FindChanged(text) => self.on_find_changed(text),
            MessageViewInput::FindNext => self.eval_find("hylkiFindStep(1)"),
            MessageViewInput::FindPrev => self.eval_find("hylkiFindStep(-1)"),
            MessageViewInput::FindCounted { current, total } => {
                self.find_matches = Some((current, total));
            }
            MessageViewInput::SetTags(tags) => self.on_set_tags(tags),
            MessageViewInput::SetCardKeywords {
                account_id,
                id,
                keywords,
                labels,
            } => self.on_set_card_keywords(account_id, id, keywords, labels),
            MessageViewInput::SetCardStar {
                account_id,
                id,
                starred,
            } => self.on_set_card_star(account_id, id, starred),
            MessageViewInput::FocusCard { account_id, id } => self.on_focus_card(account_id, id),
            MessageViewInput::BlurCard => self.on_blur_card(),
            MessageViewInput::ScrollAnchor {
                account_id,
                id,
                offset,
            } => self.on_scroll_anchor(account_id, id, offset),
            MessageViewInput::PinArrival { account_id, id } => self.on_pin_arrival(account_id, id),
            MessageViewInput::SetDraftsView(on) => {
                self.drafts_view = on;
            }
            MessageViewInput::SetTemplateFolders(folders) => self.on_set_template_folders(folders),
            MessageViewInput::TemplateAction {
                account_id,
                id,
                edit,
            } => self.on_template_action(account_id, id, edit, &sender),
            MessageViewInput::HoldPlace { account_id, id } => self.on_hold_place(account_id, id),
            MessageViewInput::CardContact { account_id, id } => {
                self.on_card_contact(account_id, id, &sender)
            }
            MessageViewInput::OpenHeader { account_id, id } => {
                self.on_open_header(account_id, id, &sender)
            }
        }
    }
}
