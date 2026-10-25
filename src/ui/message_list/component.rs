//! The message list as a relm4 component: its widget tree (view!), building
//! it, and `update`, which routes each input to its `on_*` method.

use super::init::{link_heading_scroll, watch_bottom, watch_heading_menu, watch_pane_width};

use super::layout::{schedule_midnight_refresh, BarSlots, LIST_MIN_WIDTH};
use super::rows::LIST_WINDOW;
use super::search::ScopePicker;
use super::threading::thread_slot;
use super::types::{BulkAction, MessageListInput, MessageListOutput};
use super::MessageList;
use crate::i18n::i18n;
use crate::ui::message_row::RowShared;
use adw::prelude::*;
use relm4::prelude::*;

#[relm4::component(pub(crate))]
impl SimpleComponent for MessageList {
    type Init = ();
    type Input = MessageListInput;
    type Output = MessageListOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "message-list-pane",

            // The bars set to open above the rows (#496): `place_bars`
            // moves the search field and the bulk bar between this and
            // the foot.
            #[name = "bars_top"]
            gtk::Box {
                add_css_class: "list-toolbar",
                set_orientation: gtk::Orientation::Vertical,
            },

            // The single line's column headings (#334), filled by
            // `sync_headings`; a click on one sorts by its column.
            #[name = "headings_area"]
            gtk::Box {
                add_css_class: "list-headings-bar",
                #[watch]
                set_visible: model.single_line && model.headings,
                // Scrolled sideways with the rows, never by itself: no bar
                // of its own, its place set from the list's (#443).
                #[name = "headings_scroller"]
                gtk::ScrolledWindow {
                    set_hexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::External,
                    set_vscrollbar_policy: gtk::PolicyType::Never,
                    set_propagate_natural_height: true,
                    #[local_ref]
                    headings_bar -> gtk::Box {
                        set_hexpand: true,
                    },
                },
            },

            gtk::Overlay {
                #[wrap(Some)]
                #[name = "scroller"]
                set_child = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    // External, not Never: with Never the widest row's minimum (the
                    // actions palette reservation plus the avatar column) propagates
                    // all the way up and becomes part of the window's minimum width,
                    // which pushed it past half of a 1920px screen — at which point
                    // GNOME refuses to tile the window to the left/right edge. Rows
                    // ellipsize, so a narrow pane clips gracefully instead.
                    // Automatic does not propagate it either: a single line
                    // too wide for the pane scrolls to its columns past the
                    // edge (#443), with a bar that says so.
                    #[watch]
                    set_hscrollbar_policy: if model.single_line && model.column_scroll { gtk::PolicyType::Automatic } else { gtk::PolicyType::External },
                    // The pane's own floor, now that rows no longer set one: room
                    // for a row's full actions palette (avatar + dot + the reserved
                    // actions line), so opening the palette never needs to clip —
                    // the narrow-window breakpoint rails the sidebar in time to
                    // afford this even in a half-screen tile. (Grows by the thread
                    // indent while a conversation is expanded — see the rebuild.)
                    set_size_request: (LIST_MIN_WIDTH, -1),

                    // The view must be the scroller's own child to build only
                    // the rows on screen.
                    #[local_ref]
                    list_view -> gtk::ListView {},
                },

                // At the bottom while the rest of the folder streams in.
                add_overlay = &gtk::Box {
                    add_css_class: "list-loading",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::End,
                    set_spacing: 8,
                    set_margin_bottom: 14,
                    set_can_target: false,
                    #[watch]
                    set_visible: model.is_loading_more(),

                    // Spun only while shown: a spinning spinner redraws every
                    // frame for as long as it is mapped (#275).
                    gtk::Spinner {
                        set_width_request: 18,
                        set_height_request: 18,
                        #[watch]
                        set_spinning: model.is_loading_more(),
                    },
                    gtk::Label {
                        set_label: &i18n("Loading more…"),
                        add_css_class: "dim-label",
                    },
                },

                // Placeholder when the folder has loaded and holds nothing.
                // Same full-size AdwStatusPage styling as the reader's
                // "No message selected", so the two placeholders match.
                add_overlay = &adw::StatusPage {
                    set_icon_name: Some("mail-inbox-symbolic"),
                    #[watch]
                    set_title: &(if model.quick.is_empty() { i18n("No Messages") } else { i18n("No Matching Messages") }),
                    #[watch]
                    set_description: Some(if model.quick.is_empty() {
                        i18n("There's nothing here right now.")
                    } else {
                        i18n("No message here passes the filters.")
                    }.as_str()),
                    #[watch]
                    set_visible: model.is_empty_state(),
                },
            },

            // The bars set to open below the rows (#496): one coming up
            // shrinks the list from its foot and never moves the rows being
            // read or clicked, as the status bar does (#433). The view
            // builds both bars here; `place_bars` puts them where the
            // settings say.
            #[name = "bars_bottom"]
            gtk::Box {
                add_css_class: "list-footer",
                set_orientation: gtk::Orientation::Vertical,

                // Bulk-action bar, revealed while more than one message is selected.
                #[name = "bulk_revealer"]
                gtk::Revealer {
                    #[watch]
                    set_reveal_child: model.selection_count > 1 || (model.checking && model.selection_count > 0),

                    // A collapsed revealer still reserves its child's width,
                    // so this bar of seven buttons was setting the whole
                    // pane's minimum width — 340px — however narrow the rows
                    // became (#29). Scrolled, its minimum is nothing and its
                    // natural size is unchanged, so it only clips when the
                    // list is genuinely too narrow to hold it.
                    gtk::ScrolledWindow {
                        set_vscrollbar_policy: gtk::PolicyType::Never,
                        set_hscrollbar_policy: gtk::PolicyType::External,
                        set_propagate_natural_width: true,
                        set_propagate_natural_height: true,

                    gtk::Box {
                        add_css_class: "bulk-bar",
                        set_spacing: 2,
                        // The buttons never take focus from the list: the
                        // selection keeps its focused highlight and the
                        // single-key shortcuts keep working after a click.

                        gtk::Label {
                            #[watch]
                            set_label: &format!("{} selected", model.selection_count),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            add_css_class: "bulk-count",
                        },
                        // One toggle, like the star: it shows what a click will
                        // do. Drafts are neither read nor unread: it goes when
                        // the list shows them.
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.selection_any_unread() {
                                "hylki-mail-read-symbolic"
                            } else {
                                "mail-unread-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.selection_any_unread() { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            #[watch]
                            set_visible: !model.in_drafts,
                            connect_clicked => MessageListInput::BulkToggleRead,
                        },
                        // Mapped to Unflag by the Bulk handler once every
                        // selected message is starred (#313).
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.selection_all_starred() {
                                "hylki-non-starred-symbolic"
                            } else {
                                "starred-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.selection_all_starred() { i18n("Remove Star") } else { i18n("Star") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Flag),
                        },
                        // Only once a tag exists, like the row menu's Tags.
                        #[local_ref]
                        bulk_tag_btn -> gtk::Button {
                            set_icon_name: "tag-outline-symbolic",
                            set_tooltip_text: Some(i18n("Tags").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            #[watch]
                            set_visible: !model.shared.tags.borrow().is_empty(),
                            connect_clicked => MessageListInput::BulkTagMenu,
                        },
                        gtk::Button {
                            set_icon_name: "mail-archive-symbolic",
                            set_tooltip_text: Some(i18n("Archive").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Archive),
                        },
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.in_junk {
                                "mail-mark-notjunk-symbolic"
                            } else {
                                "mail-mark-junk-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.in_junk { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            // Mapped to NotSpam in Junk by the Bulk handler.
                            connect_clicked => MessageListInput::Bulk(BulkAction::Spam),
                        },
                        gtk::Button {
                            set_icon_name: "user-trash-symbolic",
                            set_tooltip_text: Some(i18n("Delete").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Delete),
                        },
                        gtk::Separator {
                            set_orientation: gtk::Orientation::Vertical,
                        },
                        gtk::Button {
                            set_icon_name: "edit-clear-symbolic",
                            set_tooltip_text: Some(i18n("Clear selection").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::ClearSelection,
                        },
                    },
                    },
                },

                // Search stays hidden until asked for (#102).
                #[name = "search_revealer"]
                gtk::Revealer {
                    #[watch]
                    set_reveal_child: model.search_open || model.search_pinned,

                gtk::Box {
                    set_spacing: 6,
                    add_css_class: "list-search-row",

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        // Its own default minimum is wider than the rows need.
                        set_width_chars: 3,
                        #[watch]
                        set_placeholder_text: Some(model.search_placeholder().as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input(MessageListInput::Search(entry.text().to_string()));
                        },
                        // Esc closes (SearchEntry's own stop signal).
                        connect_stop_search[sender] => move |_| {
                            sender.input(MessageListInput::CloseSearch);
                        },
                        // Leaving an empty entry closes too.
                        add_controller = gtk::EventControllerFocus {
                            connect_leave[sender] => move |ctl| {
                                let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                let sender = sender.clone();
                                crate::ui::close_if_focus_left_row(&entry, move || sender.input(MessageListInput::CloseSearch));
                            },
                        },
                    },

                    // The scope picker; its popover is built in `init`
                    // (`ScopePicker`).
                    #[name = "scope_button"]
                    gtk::MenuButton {
                        set_valign: gtk::Align::Center,
                        set_tooltip_text: Some(i18n("Choose which folders to search").as_str()),
                    },
                },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let shared = RowShared::new(sender.input_sender().clone());
        let list_view = gtk::ListView::new(
            Some(shared.selection.clone()),
            Some(crate::ui::message_row::factory(&shared)),
        );
        shared.view.set(Some(&list_view));
        Self::wire_list(&list_view, &shared, sender.input_sender());
        crate::ui::message_row::make_spare_rows(&shared);

        let color_provider = crate::ui::DisplayCss::new();

        let mut model = MessageList::initial(
            list_view.clone(),
            shared,
            sender.input_sender().clone(),
            color_provider,
        );

        let bulk_tag_btn = model.bulk_tag_btn.clone();
        let headings_bar = model.headings_bar.clone();
        let widgets = view_output!();
        model.scroller = Some(widgets.scroller.clone());
        link_heading_scroll(&widgets.scroller, &widgets.headings_scroller);
        watch_heading_menu(widgets.headings_area.upcast_ref(), &sender);
        watch_pane_width(&widgets.scroller, &sender);
        watch_bottom(&widgets.scroller, &sender);
        model.sync_look();
        model.search_entry = Some(widgets.search_entry.clone());
        model.bars = Some(BarSlots {
            top: widgets.bars_top.clone(),
            bottom: widgets.bars_bottom.clone(),
            search: widgets.search_revealer.clone(),
            bulk: widgets.bulk_revealer.clone(),
        });

        model.scope_picker = Some(ScopePicker::new(
            &widgets.scope_button,
            sender.input_sender(),
        ));
        model.place_bars();

        schedule_midnight_refresh(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            MessageListInput::SetView { key, fp, restore } => {
                self.on_set_view(key, fp, restore, &sender)
            }
            MessageListInput::SetViewFp(fp) => self.view_fp = fp,
            MessageListInput::Prebuild { key, fp, messages } => self.on_prebuild(key, fp, messages),
            MessageListInput::PrebuildThreads { gen, threads } => {
                self.on_prebuild_threads(gen, *threads)
            }
            MessageListInput::SetMessages { messages } => self.on_set_messages(messages),
            MessageListInput::AppendMessages { messages } => self.on_append_messages(messages),
            MessageListInput::SetLoading => self.on_set_loading(&sender),
            MessageListInput::ResetPaging => self.on_reset_paging(&sender),
            MessageListInput::SetIndexComplete(complete) => {
                self.index_complete = complete;
            }
            MessageListInput::AtBottom(bottom) => self.at_bottom = bottom,
            MessageListInput::NearEnd => self.grow_window(LIST_WINDOW),
            MessageListInput::ThreadsReady { gen, threads } => self.on_threads_ready(gen, *threads),
            MessageListInput::AskThreads => self.on_ask_threads(&sender),
            MessageListInput::RunQueuedRebuild => self.on_run_queued_rebuild(&sender),
            MessageListInput::SetThreadLinks(links) => self.on_set_thread_links(links),
            MessageListInput::SetThreadSummaries(summaries) => {
                self.on_set_thread_summaries(summaries)
            }
            MessageListInput::RecheckThreadSummaries(account_id) => {
                self.on_recheck_thread_summaries(account_id)
            }
            MessageListInput::ConversationSize {
                account_id,
                id,
                size,
            } => self.on_conversation_size(account_id, id, size),
            MessageListInput::SetThreadRowNewest(on) => self.on_set_thread_row_newest(on),
            MessageListInput::SetOwnAddresses(addresses) => self.on_set_own_addresses(addresses),
            MessageListInput::SetThreading(on) => self.on_set_threading(on),
            MessageListInput::SetThreadExpansion(on) => self.on_set_thread_expansion(on),
            MessageListInput::SetListPalette(on) => self.on_set_list_palette(on),
            MessageListInput::ResolveDelete => self.on_resolve_delete(&sender),
            MessageListInput::SetThreadsExpanded(on) => self.on_set_threads_expanded(on),
            // Every row's date, and a conversation's latest, is worked out
            // by the rebuild.
            MessageListInput::RefreshDates => self.rebuild(),
            MessageListInput::SetSenderLogos(on) => self.on_set_sender_logos(on),
            // The rows on screen are filled again in place: a rebuild
            // here undid the selection a snooze's removal had just moved on.
            MessageListInput::SetSnoozed(snoozed) => {
                if *self.shared.snoozed.borrow() != snoozed {
                    *self.shared.snoozed.borrow_mut() = snoozed;
                    self.shared.refresh_all();
                }
            }
            MessageListInput::SetSingleLine(on) => self.on_set_single_line(on),
            MessageListInput::SetColumns(columns) => self.on_set_columns(columns),
            MessageListInput::SetAccountNames { names, graph } => {
                self.on_set_account_names(names, graph)
            }
            MessageListInput::SetLook {
                avatars,
                preview_lines,
                subject,
                animate,
            } => self.on_set_look(avatars, preview_lines, subject, animate, &sender),
            MessageListInput::LookSettled => {
                self.avatars = false;
                self.sync_look();
            }
            MessageListInput::SetAvatars(on) => self.on_set_avatars(on),
            MessageListInput::SetGravatar(on) => self.on_set_gravatar(on),
            MessageListInput::SetShowRecipient(on) => self.on_set_show_recipient(on),
            MessageListInput::SetRestorable(on) => self.restorable = on,
            MessageListInput::SetSnoozeOffer(offer) => self.snooze_offer = offer,
            MessageListInput::SetInJunk(on) => self.on_set_in_junk(on),
            MessageListInput::SetInDrafts(on) => self.on_set_in_drafts(on),
            MessageListInput::SetInTemplates(on) => self.in_templates = on,
            MessageListInput::ContactPhotosChanged => self.on_contact_photos_changed(),
            MessageListInput::SetColorize(on) => self.on_set_colorize(on),
            MessageListInput::DayChanged => self.on_day_changed(&sender),
            MessageListInput::SetAccountColors(colors) => self.on_set_account_colors(colors),
            MessageListInput::Search(q) => self.on_search(q, &sender),
            MessageListInput::SearchMatched {
                gen,
                query,
                text,
                hits,
            } => self.on_search_matched(gen, query, text, hits),
            MessageListInput::SetScope(scope) => self.on_set_scope(scope, &sender),
            MessageListInput::SetSearchPool(pool, folders, text) => {
                self.on_set_search_pool(pool, folders, text)
            }
            MessageListInput::SelectFromReader { keys, conversation } => {
                self.on_select_from_reader(keys, conversation, &sender)
            }
            MessageListInput::SelectionChanged => self.on_selection_changed(&sender),
            MessageListInput::Bulk(action) => self.on_bulk(action, &sender),
            MessageListInput::MoveSelection(delta) => self.on_move_selection(delta, &sender),
            MessageListInput::SetGmailKeys(on) => self.on_set_gmail_keys(on, &sender),
            MessageListInput::MoveCursor(delta) => self.on_move_cursor(delta),
            MessageListInput::OpenCursor => self.on_open_cursor(&sender),
            MessageListInput::PointerPressed { extend } => self.on_pointer_pressed(extend, &sender),
            MessageListInput::MoveSelectedTo { account_id, dest } => {
                self.on_move_selected_to(account_id, dest, &sender)
            }
            MessageListInput::CloseOpen => self.on_close_open(&sender),
            MessageListInput::StepConversation(delta) => self.on_step_conversation(delta),
            MessageListInput::SelectAllShown => self.on_select_all_shown(&sender),
            MessageListInput::SelectWhere(filter) => self.on_select_where(filter, &sender),
            MessageListInput::SetNavDirection(delta) => self.on_set_nav_direction(delta),
            MessageListInput::ToggleSelection => self.on_toggle_selection(&sender),
            MessageListInput::SetQuickFilters { filters, contacts } => {
                self.on_set_quick_filters(filters, contacts)
            }
            MessageListInput::FocusList => self.on_focus_list(),
            MessageListInput::ReclaimFocus => self.on_reclaim_focus(),
            MessageListInput::ShowcaseSearch(text, scope) => {
                self.on_showcase_search(text, scope, &sender)
            }
            MessageListInput::FocusSearch => self.on_focus_search(&sender),
            MessageListInput::CloseSearch => self.on_close_search(&sender),
            MessageListInput::BulkToggleRead => self.on_bulk_toggle_read(&sender),
            MessageListInput::BulkTagMenu => self.on_bulk_tag_menu(&sender),
            MessageListInput::ClearSelection => self.on_clear_selection(),
            MessageListInput::SetSort(order) => self.on_set_sort(order),
            MessageListInput::ResizeColumn {
                column,
                width,
                done,
            } => self.on_resize_column(column, width, done, &sender),
            MessageListInput::SetColumnWidths(widths) => self.on_set_column_widths(widths),
            MessageListInput::RefreshHeadings => self.sync_headings(),
            MessageListInput::SetBarsPlacement {
                search_bottom,
                bulk_bottom,
            } => self.on_set_bars_placement(search_bottom, bulk_bottom),
            MessageListInput::SetSearchPinned(on) => self.on_set_search_pinned(on),
            MessageListInput::SetColumnScroll(on) => self.on_set_column_scroll(on),
            MessageListInput::PaneWidth(width) => self.on_pane_width(width),
            MessageListInput::SetHeadings(on) => {
                self.headings = on;
                self.sync_headings();
            }
            MessageListInput::SetGroupByDate(on) => self.on_set_group_by_date(on),
            MessageListInput::SetPillsBelow(on) => self.on_set_pills_below(on),
            MessageListInput::SortByColumn(column) => self.on_sort_by_column(column, &sender),
            MessageListInput::ToggleThread(key) => self.on_toggle_thread(key, &sender),
            MessageListInput::FinishCollapseThread(key) => self.on_finish_collapse_thread(key),
            MessageListInput::RowActivated(index) => self.on_row_activated(index, &sender),
            MessageListInput::MarkRead(slot) => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = false);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetRead { slot, read } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = !read);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetStarred { slot, starred } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.starred = starred);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetAnswered {
                slot,
                forwarded,
                on,
            } => self.on_set_answered(slot, forwarded, on),
            MessageListInput::SetKeywords {
                slot,
                keywords,
                labels,
            } => self.on_set_keywords(slot, keywords, labels),
            MessageListInput::SetTags(tags) => self.on_set_tags(tags),
            MessageListInput::SetTagFor {
                message,
                keyword,
                add,
            } => self.on_set_tag_for(message, keyword, add, &sender),
            MessageListInput::SetHasAttachment {
                account_id,
                folder_id,
                uid,
                has,
            } => self.on_set_has_attachment(account_id, folder_id, uid, has),
            MessageListInput::Remove(id) => self.on_remove(id, &sender),
            MessageListInput::LeaveConversation { gone, head } => {
                self.on_leave_conversation(gone, head)
            }
            MessageListInput::RemoveMany(ids) => self.on_remove_many(ids, &sender),
            MessageListInput::RowAction { action, message } => {
                self.on_row_action(action, message, &sender)
            }
            MessageListInput::SetPaletteCollapse(secs) => {
                self.shared.palette_collapse_secs.set(secs)
            }
            MessageListInput::SetPaletteHover(on) => self.shared.palette_hover.set(on),
            MessageListInput::SetSwipeReversed(on) => self.shared.swipe_reversed.set(on),
            MessageListInput::SetSwipeEnabled(on) => self.on_set_swipe_enabled(on),
            MessageListInput::SetSwipeSensitivity(factor) => {
                self.shared.swipe_sensitivity.set(factor);
                self.shared.refresh_all();
            }
            MessageListInput::SetSelected(id) => self.on_set_selected(id),
            MessageListInput::DebugOpenPalette(idx) => self.on_debug_open_palette(idx),
            MessageListInput::DebugSwipe { index, left } => self.on_debug_swipe(index, left),
            MessageListInput::DebugRowMenu => self.on_debug_row_menu(&sender),
            MessageListInput::SelectAndLoad(key) => self.on_select_and_load(key, &sender),
            MessageListInput::RowMoveTo { message, x, y } => {
                self.on_row_move_to(message, x, y, &sender)
            }
            MessageListInput::ContextMenu { x, y, key } => self.on_context_menu(x, y, key, &sender),
        }
        // Whatever just happened, restate the header count if it moved — every
        // path that changes the visible rows funnels through here.
        let count = self.count_label();
        if count != self.last_count {
            self.last_count = count.clone();
            let _ = sender.output(MessageListOutput::CountChanged(count));
        }
    }
}
