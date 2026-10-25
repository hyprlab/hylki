//! Filling a bound row from its data: text, columns, flags, tags and the
//! classes its host widget wears. Runs on every bind; kept cheap.

use super::faces::recipient_names;
use super::headings::{due_label, pill_label};
use super::row::Row;
use super::shared::{RowLook, RowShared};
use super::{column_width, RowData, LINE_ICON_PX};
use crate::config::ListColumn;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Importance, Message};
use crate::ui::column_bin::ColumnBin;
use adw::prelude::*;
use relm4::prelude::*;

impl Row {
    /// Fill the widgets from the bound message and the list's look.
    pub(crate) fn refresh(&self) {
        let Some(shared) = self.shared() else { return };
        let Some(data) = self.data() else { return };
        let look = shared.look.borrow().clone();
        let msg = &data.msg;
        let meta = &data.meta;
        self.arrange(look.single_line.then_some(&look.columns[..]));
        self.sync_host_classes();
        self.refresh_frame(&data, &look);
        self.refresh_avatar(&data, &look);
        let unread = msg.unread || meta.unread;
        self.refresh_marks(&data, &look, unread);
        if look.single_line {
            self.fill_columns(&data, &look, &shared, unread);
        }
        self.refresh_date(&data, &look, &shared);
        self.refresh_subject(&data, &look, &shared, unread);
        self.refresh_preview(&data, &look);
        self.refresh_swipe_state(&shared, &look);
        self.sync_palette_buttons();
        self.sync_swipe_strip();
    }

    /// The reveal, the thread rail and node, the actions line and the
    /// content box's classes.
    fn refresh_frame(&self, data: &RowData, look: &RowLook) {
        let meta = &data.meta;
        let w = &self.w;
        let single = look.single_line;
        {
            let st = self.st.borrow();
            w.revealer
                .set_reveal_child(meta.revealed && !st.swipe_committing && !meta.appear);
        }

        // The thread rail and node.
        w.rail_stub.set_visible(meta.is_last);
        w.node.set_visible(meta.is_child);

        // The actions line: centred under the avatar with circles on (circle
        // centre minus half the button, plus a reply card's 10px indent),
        // hugging the pill's edge without.
        w.actions_line.set_visible(look.show_palette);
        w.actions_line
            .set_margin_start(match (look.avatars, meta.is_child) {
                (true, true) => 32,
                (true, false) => 22,
                (false, true) => 20,
                (false, false) => 10,
            });
        w.actions_line
            .set_margin_bottom(if look.avatars { 3 } else { 4 });
        self.sync_palette_classes();

        // The content box.
        let mut content = vec!["message-row"];
        if !look.avatars {
            content.push("no-avatar");
        }
        if !look.show_palette {
            content.push("no-palette");
        }
        if single {
            content.push("single-line");
        } else if look.show_palette && look.avatars && look.preview_lines == 0 {
            content.push("palette-room");
        }
        w.content.set_css_classes(&content);
        w.content
            .set_size_request(if single { look.min_width } else { -1 }, -1);
    }

    /// The circle: its visibility, the account ring, and the face.
    fn refresh_avatar(&self, data: &RowData, look: &RowLook) {
        let msg = &data.msg;
        let w = &self.w;
        // The circle.
        w.avatar_revealer.set_visible(look.avatars);
        let shown = self.st.borrow().avatar_shown;
        w.avatar_revealer.set_reveal_child(shown);
        w.avatar_revealer.set_css_classes(if shown {
            &["focus-fade"]
        } else {
            &["focus-fade", "away"]
        });
        let ring = format!("hylki-acct-ring-{}", msg.account_id);
        if look.ringed.contains(&msg.account_id) {
            w.avatar.set_css_classes(&[ring.as_str()]);
        } else {
            w.avatar.set_css_classes(&[]);
        }
        if look.avatars {
            let email = self.face_email(data, look);
            let want = Some((email.clone(), look.face_gen));
            if self.st.borrow().face_for != want {
                self.st.borrow_mut().face_for = want;
                self.st.borrow_mut().avatar_texture = None;
                self.load_face(&email, look);
            }
        }
        // Only what changed: an avatar given its text again restyles and
        // lays out its initials, and a list switch fills two hundred rows.
        let name = self.face_name(data, look);
        if w.avatar.text().as_deref() != Some(name.as_str()) {
            w.avatar.set_text(Some(&name));
        }
        let image = self.avatar_image(data, look);
        if w.avatar.custom_image() != image {
            w.avatar.set_custom_image(image.as_ref());
        }
    }

    /// The unread dot, the sender, and the attachment, star and replied
    /// marks.
    fn refresh_marks(&self, data: &RowData, look: &RowLook, unread: bool) {
        let msg = &data.msg;
        let meta = &data.meta;
        let w = &self.w;
        let single = look.single_line;
        // Whether a widget's column is on: a card shows what it always has.
        let col = |c: ListColumn| !single || look.columns.contains(&c);
        // Faded rather than hidden: the slot is always reserved and only
        // the dot's ink changes, so text never jitters as mail is read.
        w.dot.set_valign(if look.avatars || single {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        });
        w.dot.set_opacity(if unread { 1.0 } else { 0.0 });
        // Only a dot for the message's own unread state takes a click: on a
        // thread head lit by an unread reply, it would mark the head unread.
        let clickable = msg.unread && !look.in_drafts;
        w.dot.set_can_target(clickable);
        w.dot
            .set_tooltip_text(clickable.then(|| i18n("Mark as Read")).as_deref());
        w.text.set_valign(if look.avatars {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        });

        w.name_col.set_visible(col(ListColumn::Sender));
        w.name.set_label(&self.name_line(data, look));
        w.name.set_css_classes(if unread {
            &["message-sender", "unread"]
        } else {
            &["message-sender"]
        });
        // On one line an icon keeps its column whether lit or not, so the
        // columns after it line up.
        w.clip.set_visible(if single {
            col(ListColumn::Attachment)
        } else {
            msg.has_attachment
        });
        w.clip
            .set_opacity(if msg.has_attachment { 1.0 } else { 0.0 });
        let starred = msg.starred || meta.starred;
        w.star.set_visible(if single {
            col(ListColumn::Star)
        } else {
            starred
        });
        w.star.set_opacity(if starred { 1.0 } else { 0.0 });
        let (answered, forwarded) = if meta.key.is_some() {
            (meta.answered, meta.forwarded)
        } else {
            (msg.answered, msg.forwarded)
        };
        w.answer.set_visible(if single {
            col(ListColumn::Replied)
        } else {
            answered || forwarded
        });
        for (arrow, lit) in [(&w.replied, answered), (&w.forwarded, forwarded)] {
            arrow.set_visible(single || lit);
            arrow.set_opacity(if lit { 1.0 } else { 0.0 });
        }
        w.replied
            .set_tooltip_text(answered.then(|| i18n("Replied")).as_deref());
        w.forwarded
            .set_tooltip_text(forwarded.then(|| i18n("Forwarded")).as_deref());
    }

    /// The date column and its label, the column widths on one line, and
    /// the conversation's count chip.
    fn refresh_date(&self, data: &RowData, look: &RowLook, shared: &RowShared) {
        let msg = &data.msg;
        let meta = &data.meta;
        let w = &self.w;
        let single = look.single_line;
        // Whether a widget's column is on: a card shows what it always has.
        let col = |c: ListColumn| !single || look.columns.contains(&c);
        w.date.set_visible(col(ListColumn::Date));
        w.date_col.set_visible(col(ListColumn::Date));
        if single {
            for (c, bin) in [
                (ListColumn::Sender, &w.name_col),
                (ListColumn::Recipients, &w.recipients_col),
                (ListColumn::Correspondents, &w.people_col),
                (ListColumn::Account, &w.account_col),
                (ListColumn::Tags, &w.tags_col),
                (ListColumn::Due, &w.due_col),
                (ListColumn::Date, &w.date_col),
            ] {
                bin.set_width(column_width(look, c));
            }
        }
        match (&meta.latest, &meta.latest_at) {
            (_, Some((ts, date))) if single => {
                w.date.set_label(&crate::models::date_short(*ts, date))
            }
            (Some(latest), _) if !single => w.date.set_label(latest),
            _ if single => w
                .date
                .set_label(&crate::models::date_short(msg.timestamp, &msg.date)),
            _ => w.date.set_label(&msg.datetime_list()),
        }
        // Snoozed mail says when it comes back instead (#527).
        let until = shared.snoozed.borrow().until(msg);
        match until {
            Some(t) => {
                w.date.set_label(&if single {
                    crate::models::date_short(t, "")
                } else {
                    crate::models::datetime_list_at(t, "")
                });
                w.date.add_css_class("snoozed");
                w.date.set_tooltip_text(Some(&i18n_f(
                    "Snoozed until {when}",
                    &[("when", &crate::datefmt::date_time(t))],
                )));
            }
            None => {
                w.date.remove_css_class("snoozed");
                w.date.set_tooltip_text(None);
            }
        }
        w.chip.set_visible(meta.count > 1);
        w.chip_count.set_label(&meta.count.to_string());
        w.chip_caret
            .set_visible(look.thread_expansion && meta.expandable);
        w.chip_caret.set_css_classes(if meta.expanded {
            &["thread-toggle-icon", "open"]
        } else {
            &["thread-toggle-icon"]
        });
    }

    /// The subject (with its text on one line) and where the tag pills sit.
    fn refresh_subject(&self, data: &RowData, look: &RowLook, shared: &RowShared, unread: bool) {
        let msg = &data.msg;
        let meta = &data.meta;
        let w = &self.w;
        let single = look.single_line;
        // Whether a widget's column is on: a card shows what it always has.
        let col = |c: ListColumn| !single || look.columns.contains(&c);
        w.subject_line.set_visible(look.show_subject);
        // One line: the subject, then its text dimmed after it, in one label,
        // so the subject is cut only once the text has gone (#334).
        if single {
            let esc = |t: &str| gtk::glib::markup_escape_text(t).to_string();
            let text =
                crate::models::preview_display(meta.preview.as_deref().unwrap_or(&msg.preview));
            let markup = if look.preview_lines > 0 && !text.is_empty() {
                format!(
                    "{} <span weight=\"normal\" alpha=\"55%\">— {}</span>",
                    esc(&msg.subject),
                    esc(&text)
                )
            } else {
                esc(&msg.subject)
            };
            w.subject.set_markup(&markup);
        } else {
            w.subject.set_use_markup(false);
            w.subject.set_label(&msg.subject);
        }
        w.subject.set_css_classes(if unread {
            &["message-subject", "unread"]
        } else {
            &["message-subject"]
        });
        w.tags_box.set_visible(col(ListColumn::Tags));
        w.tags_col.set_visible(single && col(ListColumn::Tags));
        self.render_tags(msg, shared, look);
        if !single {
            let below = look.pills_below;
            let home = if below {
                &w.pills_line
            } else {
                &w.subject_line
            };
            if w.tags_box.parent().as_ref() != Some(home.upcast_ref()) {
                if let Some(from) = w.tags_box.parent().and_downcast::<gtk::Box>() {
                    from.remove(&w.tags_box);
                }
                home.append(&w.tags_box);
            }
            w.pills_line
                .set_visible(below && look.show_subject && w.tags_box.first_child().is_some());
        }
    }

    /// The preview line and its lock.
    fn refresh_preview(&self, data: &RowData, look: &RowLook) {
        let msg = &data.msg;
        let meta = &data.meta;
        let w = &self.w;
        let single = look.single_line;
        let preview = meta.preview.as_deref().unwrap_or(&msg.preview);
        w.preview_line.set_visible(look.preview_lines > 0);
        w.lock
            .set_visible(crate::models::preview_is_encrypted(preview));
        let shown = crate::models::preview_display(preview);
        if single {
            // Carried by the subject's label on one line.
            w.preview.set_visible(false);
        } else {
            w.preview.set_visible(true);
            w.preview.set_label(&shown);
            w.preview.set_wrap(look.preview_lines > 1);
            w.preview.set_lines(look.preview_lines.max(1) as i32);
        }
    }

    /// Whether the row takes swipes now.
    fn refresh_swipe_state(&self, shared: &RowShared, look: &RowLook) {
        let w = &self.w;
        w.surface.set_sensitivity(shared.swipe_sensitivity.get());
        if let Some(t) = self.tracker.borrow().as_ref() {
            // A committing row takes no new gestures — it is on its way out.
            // A single line swipes not at all: sideways is how a list wider
            // than its pane scrolls to its other columns (#443).
            t.set_enabled(
                shared.swipe_enabled.get()
                    && !look.single_line
                    && !self.st.borrow().swipe_committing,
            );
        }
    }

    /// The columns only a single line has (#334): each shown or not as the
    /// setting says, and filled from the message when it is.
    fn fill_columns(&self, data: &RowData, look: &RowLook, shared: &RowShared, unread: bool) {
        let w = &self.w;
        let msg = &data.msg;
        let on = |c: ListColumn| look.columns.contains(&c);
        let weight: &[&str] = if unread {
            &["message-sender", "unread"]
        } else {
            &["message-sender"]
        };

        w.recipients_col.set_visible(on(ListColumn::Recipients));
        if on(ListColumn::Recipients) {
            w.recipients.set_label(&recipient_names(&msg.to));
            w.recipients.set_css_classes(weight);
        }

        w.people_col.set_visible(on(ListColumn::Correspondents));
        if on(ListColumn::Correspondents) {
            let people = data
                .meta
                .people
                .clone()
                .unwrap_or_else(|| msg.from_name.clone());
            w.people.set_tooltip_text(Some(&people));
            w.people.set_label(&people);
            w.people.set_css_classes(weight);
        }

        w.importance.set_visible(on(ListColumn::Importance));
        let (icon, class, tip) = match msg.importance {
            Importance::High => (
                "emblem-important-symbolic",
                "importance-high",
                Some(i18n("High importance")),
            ),
            Importance::Low => (
                "hylki-importance-low-symbolic",
                "importance-low",
                Some(i18n("Low importance")),
            ),
            Importance::Normal => ("emblem-important-symbolic", "importance-high", None),
        };
        w.importance.set_icon_name(Some(icon));
        w.importance.set_css_classes(&[class]);
        w.importance
            .set_opacity(if tip.is_some() { 1.0 } else { 0.0 });
        w.importance.set_tooltip_text(tip.as_deref());

        w.account_col.set_visible(on(ListColumn::Account));
        if on(ListColumn::Account) {
            let name = shared
                .account_names
                .borrow()
                .get(&msg.account_id)
                .cloned()
                .unwrap_or_default();
            w.account.set_tooltip_text(Some(&name));
            w.account.set_label(&name);
        }

        w.due_col.set_visible(on(ListColumn::Due));
        if on(ListColumn::Due) {
            w.due.set_label(&due_label(msg.due));
            let overdue = msg.due > 0
                && crate::datefmt::day_key(msg.due)
                    < crate::datefmt::day_key(crate::datefmt::now());
            w.due.set_css_classes(if overdue {
                &["message-date", "overdue"]
            } else {
                &["message-date"]
            });
        }
    }

    /// The row's own classes, on the item widget the view wraps it in.
    pub(super) fn sync_host_classes(&self) {
        let Some(data) = self.data() else { return };
        // The first tag (in tag order) the message carries tints its row.
        let cursor = self
            .shared()
            .is_some_and(|shared| shared.cursor.get() == Some((data.msg.account_id, data.msg.id)));
        let tint = self.shared().and_then(|shared| {
            let tags = shared.tags.borrow();
            let tag = data.msg.pills(&tags).into_iter().find_map(|p| match p {
                crate::models::Pill::Tag(t) => Some(t.css_class()),
                crate::models::Pill::Label(_) => None,
            })?;
            Some(format!("rowtint-{tag}"))
        });
        let st = self.st.borrow();
        let mut v = vec!["message-item"];
        if let Some(class) = tint.as_deref() {
            v.push(class);
        }
        if data.msg.unread {
            v.push("message-unread");
        }
        if data.meta.unread {
            v.push("thread-unread");
        }
        if data.meta.is_child {
            v.push("thread-child");
        }
        if data.meta.is_last {
            v.push("thread-last");
        }
        if st.swipe_active {
            v.push("swiping");
        }
        if st.dragging {
            v.push("dragging");
        }
        if cursor {
            v.push("kbd-cursor");
        }
        drop(st);
        self.w.host.set_css_classes(&v);
    }

    /// Rebuild the row's chips when the keywords, the labels or the tags
    /// changed: one pill per tag the message carries (#71), then one per
    /// Gmail label no tag stands for (#374).
    fn render_tags(&self, msg: &Message, shared: &RowShared, look: &RowLook) {
        let folder = look
            .folder_pills
            .get(&(msg.account_id, msg.folder_id))
            .cloned();
        let want = Some((
            msg.keywords.clone(),
            msg.labels.clone(),
            look.tags_gen,
            folder.clone(),
        ));
        if self.st.borrow().tags_for == want {
            return;
        }
        self.st.borrow_mut().tags_for = want;
        let tags_box = &self.w.tags_box;
        while let Some(child) = tags_box.first_child() {
            tags_box.remove(&child);
        }
        // A result of a search across folders says where it is (#374).
        if let Some(folder) = folder {
            let chip = gtk::Label::new(Some(&folder));
            chip.add_css_class("tag-chip");
            chip.add_css_class("label-chip");
            chip.add_css_class("folder-chip");
            chip.set_tooltip_text(Some(&i18n_f("In {folder}", &[("folder", &folder)])));
            chip.set_valign(gtk::Align::Center);
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.set_max_width_chars(14);
            tags_box.append(&chip);
        }
        for pill in msg.pills(&shared.tags.borrow()) {
            let chip = pill_label(&pill);
            chip.set_valign(gtk::Align::Center);
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.set_max_width_chars(14);
            tags_box.append(&chip);
        }
    }

    /// Lay the row's widgets out on one line or as the three-line card
    /// (#334). They are moved, not copied, and only when the layout
    /// changes: a row is recycled across messages but rarely across layouts.
    fn arrange(&self, columns: Option<&[ListColumn]>) {
        if self.st.borrow().columns.as_deref() == columns {
            return;
        }
        self.st.borrow_mut().columns = columns.map(<[ListColumn]>::to_vec);
        let w = &self.w;
        fn detach(widget: &gtk::Widget) {
            if let Some(from) = widget.parent().and_downcast::<gtk::Box>() {
                from.remove(widget);
            } else if let Some(bin) = widget.parent().and_downcast::<ColumnBin>() {
                bin.set_child(None::<&gtk::Widget>);
            }
        }
        fn into(to: &gtk::Box, widgets: &[&gtk::Widget]) {
            for widget in widgets {
                detach(widget);
                to.append(*widget);
            }
        }
        let name: &gtk::Widget = w.name.upcast_ref();
        let subject: &gtk::Widget = w.subject.upcast_ref();
        let star: &gtk::Widget = w.star.upcast_ref();
        let answer: &gtk::Widget = w.answer.upcast_ref();
        let clip: &gtk::Widget = w.clip.upcast_ref();
        let date: &gtk::Widget = w.date.upcast_ref();
        let chip: &gtk::Widget = w.chip.upcast_ref();
        let tags: &gtk::Widget = w.tags_box.upcast_ref();
        let lock: &gtk::Widget = w.lock.upcast_ref();
        let preview: &gtk::Widget = w.preview.upcast_ref();
        if let Some(columns) = columns {
            // In the setting's order; a column that is off keeps its widget
            // wherever it was, hidden. The conversation chip rides at the
            // subject's end, where the subject gives way to it.
            for c in columns {
                match c {
                    ListColumn::Star => into(&w.line, &[star]),
                    ListColumn::Replied => into(&w.line, &[answer]),
                    ListColumn::Sender => {
                        detach(name);
                        w.name_col.set_child(Some(name));
                        into(&w.line, &[w.name_col.upcast_ref()]);
                    }
                    ListColumn::Recipients => into(&w.line, &[w.recipients_col.upcast_ref()]),
                    ListColumn::Correspondents => into(&w.line, &[w.people_col.upcast_ref()]),
                    ListColumn::Subject => {
                        detach(subject);
                        w.subject_col.set_child(Some(subject));
                        into(&w.line, &[lock, w.subject_col.upcast_ref(), chip]);
                    }
                    ListColumn::Tags => {
                        detach(tags);
                        w.tags_col.set_child(Some(tags));
                        into(&w.line, &[w.tags_col.upcast_ref()]);
                    }
                    ListColumn::Attachment => into(&w.line, &[clip]),
                    ListColumn::Importance => into(&w.line, &[w.importance.upcast_ref()]),
                    ListColumn::Account => into(&w.line, &[w.account_col.upcast_ref()]),
                    ListColumn::Due => into(&w.line, &[w.due_col.upcast_ref()]),
                    ListColumn::Date => {
                        detach(date);
                        w.date_col.set_child(Some(date));
                        into(&w.line, &[w.date_col.upcast_ref()]);
                    }
                }
            }
            // Its bin sets the width (see `ColumnBin`); expanding, the
            // name would take the subject's room.
            w.name.set_hexpand(false);
            w.name.set_xalign(0.0);
            w.date.set_width_chars(9);
            w.date.set_xalign(1.0);
            w.avatar.set_size(16);
            // Smaller marks on one line, level with its smaller text.
            w.star.set_pixel_size(LINE_ICON_PX);
            w.clip.set_pixel_size(LINE_ICON_PX);
            w.replied.set_pixel_size(LINE_ICON_PX);
            w.forwarded.set_pixel_size(LINE_ICON_PX);
        } else {
            into(&w.top, &[name, answer, clip, star, date, chip]);
            into(&w.subject_line, &[subject, tags]);
            into(&w.preview_line, &[lock, preview]);
            w.name.set_hexpand(true);
            w.date.set_width_chars(-1);
            w.date.set_xalign(0.5);
            w.avatar.set_size(38);
            w.star.set_pixel_size(-1);
            w.clip.set_pixel_size(-1);
            w.replied.set_pixel_size(-1);
            w.forwarded.set_pixel_size(-1);
        }
        w.text.set_visible(columns.is_none());
        w.line.set_visible(columns.is_some());
    }

    /// Show this many preview lines in place (Focus Mode).
    pub(crate) fn set_preview_lines(&self, lines: u32) {
        let lines = lines.clamp(1, 3);
        self.w.preview.set_wrap(lines > 1);
        self.w.preview.set_lines(lines as i32);
    }

    pub(super) fn set_hover(&self, over: bool) {
        self.st.borrow_mut().hovered = over;
        self.sync_palette_classes();
        // Hover mode: the palette slides open by itself on the row, and arms
        // the usual collapse timeout on leave.
        let Some(shared) = self.shared() else { return };
        if !shared.palette_hover.get() || self.data().is_none() {
            return;
        }
        if over {
            if !self.st.borrow().palette_open {
                self.open_palette(true);
            }
            self.cancel_collapse();
        } else if self.st.borrow().palette_open {
            self.arm_collapse();
        }
    }
}
