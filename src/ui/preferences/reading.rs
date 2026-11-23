//! Settings → Message List, Conversations and Reading, and Printing.

use super::choices::{
    fold_messages_at, list_layout_at, read_mark_at, reader_default_at, search_start_index,
    search_starts,
};
use super::*;

impl Preferences {
    pub(super) fn on_toggle_avatars(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.avatars = on;
        let _ = sender.output(PrefOutput::SetAvatars(on));
    }

    pub(super) fn on_toggle_threading(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.threading = on;
        let _ = sender.output(PrefOutput::SetThreading(on));
    }

    pub(super) fn on_toggle_thread_expansion(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.thread_expansion = on;
        let _ = sender.output(PrefOutput::SetThreadExpansion(on));
    }

    pub(super) fn on_change_reader_default(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let policy = reader_default_at(idx);
        let _ = sender.output(PrefOutput::SetReaderDefault(policy));
    }

    pub(super) fn on_change_reader_zoom(&mut self, idx: u32, sender: ComponentSender<Self>) {
        if let Some(&z) = crate::config::READER_ZOOM_STEPS.get(idx as usize) {
            let _ = sender.output(PrefOutput::SetReaderZoom(z));
        }
    }

    pub(super) fn on_toggle_swipe_enabled(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.swipe_enabled = on;
        let _ = sender.output(PrefOutput::SetSwipeEnabled(on));
    }

    pub(super) fn on_change_read_mark(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let policy = read_mark_at(idx);
        let _ = sender.output(PrefOutput::SetReadMark(policy));
    }

    pub(super) fn on_change_list_layout(&mut self, index: u32, sender: ComponentSender<Self>) {
        let layout = list_layout_at(index);
        self.list_single_possible = layout != crate::config::ListLayout::Cards;
        let _ = sender.output(PrefOutput::SetListLayout(layout));
    }

    /// The "Search starts in" row's choices and selection, as the number of
    /// accounts has them (#699).
    pub(super) fn fill_search_start_row(&self) {
        let Some(row) = &self.search_start_row else {
            return;
        };
        let labels: Vec<String> = search_starts(self.lone_account)
            .iter()
            .map(|(l, _)| i18n(l))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&labels)));
        row.set_selected(search_start_index(self.search_start, self.lone_account));
    }

    /// A choice made on the row. Filling the row moves its selection too,
    /// which is not a choice: an index no longer selected is stale, and one
    /// that shows the setting as it is changes nothing. So "Everywhere"
    /// stays stored while it shows as "This account".
    pub(super) fn on_change_search_start(&mut self, index: u32, sender: ComponentSender<Self>) {
        let Some(row) = &self.search_start_row else {
            return;
        };
        if row.selected() != index
            || index == search_start_index(self.search_start, self.lone_account)
        {
            return;
        }
        if let Some((_, start)) = search_starts(self.lone_account).get(index as usize) {
            self.search_start = *start;
            let _ = sender.output(PrefOutput::SetListSearchStart(*start));
        }
    }

    pub(super) fn on_change_preview_lines(&mut self, index: u32, sender: ComponentSender<Self>) {
        // The combo lists Off, then 1, 2 and 3 lines — so the row index is
        // the number of lines.
        let _ = sender.output(PrefOutput::SetPreviewLines(index));
    }

    pub(super) fn on_change_fold_messages(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let fold = fold_messages_at(idx);
        let _ = sender.output(PrefOutput::SetFoldMessages(fold));
    }
}

impl Preferences {
    /// Flip one of the Printing switches and hand the app the whole set.
    pub(super) fn set_print_option(
        &mut self,
        switch: fn(&mut crate::config::PrintOptions) -> &mut bool,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        *switch(&mut self.print_options) = on;
        let _ = sender.output(PrefOutput::SetPrintOptions(self.print_options));
    }
}
