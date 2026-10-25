//! Building the list: its first state, and the scroll and click handlers
//! `init` hangs on the widgets the view made.

use super::rows::LIST_WINDOW;
use super::{
    MessageList, MessageListInput, MessageListOutput, QuickFilters, SearchScope, SortOrder,
};
use crate::i18n::i18n;
use crate::ui::message_row::RowShared;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageList {
    /// The list before any folder is shown.
    pub(super) fn initial(
        list_view: gtk::ListView,
        shared: std::rc::Rc<RowShared>,
        input: relm4::Sender<MessageListInput>,
        color_provider: crate::ui::DisplayCss,
    ) -> Self {
        MessageList {
            list_view,
            shared,
            input,
            rebuild_queued: None,
            view_key: None,
            view_fp: 0,
            views: std::collections::VecDeque::new(),
            pending_restore: None,
            prebuild: None,
            prebuild_gen: 0,
            switched: false,
            look_stale: Default::default(),
            pending_select: None,
            late_select: None,
            all: Vec::new(),
            search_pool: Vec::new(),
            pool_folders: Default::default(),
            search_text: Default::default(),
            search_gen: Default::default(),
            search_hits: None,
            scope: SearchScope::AllFolders,
            search_entry: None,
            scope_picker: None,
            search_open: false,
            search_closed_at: None,
            search_pinned: false,
            search_bottom: false,
            bulk_bottom: false,
            bars: None,
            shown: Vec::new(),
            total_matches: 0,
            index_complete: true,
            loaded: false,
            at_bottom: false,
            window: LIST_WINDOW,
            tail: Vec::new(),
            threads_gen: std::cell::Cell::new(0),
            threads_job: std::cell::Cell::new(None),
            collapsing_threads: std::collections::HashMap::new(),
            query: String::new(),
            gravatar: false,
            avatars: true,
            sender_logos: false,
            preview_lines: 1,
            show_subject: true,
            colorize: false,
            account_colors: std::collections::HashMap::new(),
            color_provider,
            face_gen: 0,
            tags_gen: 0,
            thread_links: Vec::new(),
            source_threads: std::cell::RefCell::new(None),
            thread_summaries: std::collections::HashMap::new(),
            thread_row_newest: false,
            own_addresses: Default::default(),
            gmail_keys: crate::config::load_privacy().gmail_keys,
            groups: std::collections::HashMap::new(),
            listed_folders: std::collections::HashSet::new(),
            asked_threads: std::collections::HashSet::new(),
            recheck_accounts: std::collections::HashSet::new(),
            selected_id: None,
            selected_ids: Vec::new(),
            emitted_thread: Vec::new(),
            selection_count: 0,
            checking: false,
            nav_direction: 1,
            from_reader: 0,
            reader_keys: Vec::new(),
            expanded_threads: std::collections::HashSet::new(),
            show_recipient: false,
            restorable: false,
            snooze_offer: None,
            in_junk: false,
            in_drafts: false,
            in_templates: false,
            default_expanded: false,
            msg_thread: std::collections::HashMap::new(),
            thread_members: std::collections::HashMap::new(),
            nested: std::collections::HashMap::new(),
            scroller: None,
            sort: SortOrder::DateNewest,
            quick: QuickFilters::default(),
            contact_addresses: None,
            last_count: String::new(),
            threading: true,
            thread_expansion: true,
            list_palette: true,
            single_line: false,
            columns: crate::config::ListColumn::DEFAULT.to_vec(),
            graph_accounts: std::collections::HashSet::new(),
            graph_in_view: false,
            sort_reversed: false,
            group_by_date: crate::config::load_privacy().list_group_by_date,
            day_headings: false,
            pills_below: crate::config::load_privacy().list_pills_below,
            headings: false,
            headings_bar: gtk::Box::new(gtk::Orientation::Horizontal, 8),
            widths: std::collections::HashMap::new(),
            column_scroll: false,
            pane_width: 0,
            heading_bins: Default::default(),
            bulk_tag_btn: gtk::Button::new(),
        }
    }
}

/// Keep the headings and the rows scrolled sideways together.
pub(super) fn link_heading_scroll(
    rows_scroller: &gtk::ScrolledWindow,
    headings: &gtk::ScrolledWindow,
) {
    // The headings follow the rows sideways, and a scroll over the
    // headings moves the rows. Setting the value it already has
    // emits nothing, so the two do not chase each other.
    let rows = rows_scroller.hadjustment();
    let heads = headings.hadjustment();
    let h = heads.clone();
    rows.connect_value_changed(move |a| h.set_value(a.value()));
    let r = rows.clone();
    heads.connect_value_changed(move |a| r.set_value(a.value()));
    // The headings take their full width a frame after the rows
    // may have scrolled: put them back in step then.
    let r = rows.clone();
    heads.connect_changed(move |a| a.set_value(r.value()));
}

/// The headings' right-click menu.
pub(super) fn watch_heading_menu(area: &gtk::Widget, sender: &ComponentSender<MessageList>) {
    // A right-click on the headings offers the way to the columns'
    // settings (#443).
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let anchor = area.clone();
    let s = sender.clone();
    click.connect_pressed(move |g, _, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        let s = s.clone();
        let entry =
            crate::ui::context_menu::MenuEntry::new(i18n("Customize Columns…"), move || {
                let _ = s.output(MessageListOutput::CustomizeColumns);
            })
            .icon("view-list-bullet-symbolic");
        crate::ui::context_menu::show_context_menu(&anchor, x, y, vec![vec![entry]]);
    });
    area.add_controller(click);
}

/// Report the rows' width as the view is allocated.
pub(super) fn watch_pane_width(
    scroller: &gtk::ScrolledWindow,
    sender: &ComponentSender<MessageList>,
) {
    // The rows' width, which fitted columns fit into: the view's
    // page across, set as it is allocated.
    let input = sender.input_sender().clone();
    scroller
        .hadjustment()
        .connect_notify_local(Some("page-size"), move |adj, _| {
            let _ = input.send(MessageListInput::PaneWidth(adj.page_size() as i32));
        });
}

/// Report reaching the bottom, and nearing the end, of the list.
pub(super) fn watch_bottom(scroller: &gtk::ScrolledWindow, sender: &ComponentSender<MessageList>) {
    // Whether the list sits at its bottom, for the spinner that says
    // more of the folder is on its way.
    let adj = scroller.vadjustment();
    let input = sender.input_sender().clone();
    let at_bottom = std::rc::Rc::new(std::cell::Cell::new(false));
    let near = std::rc::Rc::new(std::cell::Cell::new(false));
    let check = move |adj: &gtk::Adjustment| {
        let bottom =
            adj.upper() > adj.page_size() && adj.value() + adj.page_size() >= adj.upper() - 1.0;
        if at_bottom.replace(bottom) != bottom {
            let _ = input.send(MessageListInput::AtBottom(bottom));
        }
        // Two screens from the end, the next rows are added, so
        // they are there before the scroll reaches them.
        let close = adj.upper() > adj.page_size()
            && adj.value() + adj.page_size() >= adj.upper() - 2.0 * adj.page_size();
        if near.replace(close) != close && close {
            let _ = input.send(MessageListInput::NearEnd);
        }
    };
    let c = check.clone();
    adj.connect_value_changed(move |a| c(a));
    adj.connect_changed(move |a| check(a));
}
