//! Tags (#71): the list, the tag editor and the tag finder.

use super::*;

/// The tag dialog's "any color" disc (#147): a hue wheel while no custom
/// color is chosen, the chosen color as a filled disc once one is.
fn custom_swatch_widget(color: Option<&str>) -> gtk::DrawingArea {
    if let Some(c) = color {
        return crate::ui::context_menu::swatch_widget(c, true);
    }
    let area = gtk::DrawingArea::new();
    area.set_content_width(16);
    area.set_content_height(16);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(|_, cr, w, h| {
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let steps = 12;
        for i in 0..steps {
            let a0 = i as f64 / steps as f64 * std::f64::consts::TAU;
            let a1 = (i + 1) as f64 / steps as f64 * std::f64::consts::TAU;
            let (r, g, b) = hue_rgb(i as f64 / steps as f64);
            cr.set_source_rgb(r, g, b);
            cr.move_to(cx, cy);
            cr.arc(cx, cy, 6.0, a0 - 0.02, a1 + 0.02);
            cr.close_path();
            let _ = cr.fill();
        }
    });
    area
}

/// A fully saturated color at hue `h` (0..1) as RGB in 0..1.
fn hue_rgb(h: f64) -> (f64, f64, f64) {
    let h6 = h * 6.0;
    let x = 1.0 - ((h6 % 2.0) - 1.0).abs();
    match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    }
}

impl AccountsWindow {
    /// The tag finder's button while a scan runs: a turning spinner beside
    /// a label saying so. A press meanwhile starts nothing.
    pub(super) fn set_tag_scanning(&mut self, on: bool) {
        self.tag_scanning = on;
        if let Some(spinner) = &self.find_tags_spinner {
            spinner.set_visible(on);
            spinner.set_spinning(on);
        }
        if let Some(label) = &self.find_tags_label {
            label.set_label(&if on {
                i18n("Searching…")
            } else {
                i18n("Find Tags…")
            });
        }
    }

    /// The tag finder's report: every keyword found that is not a tag here
    /// yet, each with a proposed name and color, ticked to be imported.
    /// "Import All" takes the lot; "Import Selected" only the ticked ones.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn show_tag_findings(
        &self,
        parent: Option<&gtk::Window>,
        found: Vec<TagProposal>,
        sender: &ComponentSender<Self>,
    ) {
        if found.is_empty() {
            let dialog = crate::ui::dialog::message_dialog(
                parent,
                Some(i18n("No New Tags").as_str()),
                Some(i18n("Every tag in your mailboxes is already set up here.").as_str()),
            );
            dialog.add_response("ok", &i18n("OK"));
            dialog.set_default_response(Some("ok"));
            dialog.set_close_response("ok");
            dialog.present();
            return;
        }

        let n = found.len();
        let dialog = crate::ui::dialog::message_dialog(
            parent,
            Some(i18n("Tags Found").as_str()),
            Some(
                i18n_f(
                    "{n} tags are in use in your mailboxes but not set up here.",
                    &[("n", &n.to_string())],
                )
                .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("selected", &i18n("Import Selected"));
        dialog.add_response("all", &i18n("Import All"));
        dialog.set_response_appearance("all", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("all"));
        dialog.set_close_response("cancel");

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let mut checks: Vec<(gtk::CheckButton, crate::config::Tag)> = Vec::new();
        for p in &found {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&p.tag.name));
            let mut parts = vec![p.tag.keyword.clone()];
            if p.count > 0 {
                parts.push(i18n_f("{n} messages", &[("n", &p.count.to_string())]));
            }
            parts.extend(p.places.iter().cloned());
            row.set_subtitle(&gtk::glib::markup_escape_text(&parts.join(" · ")));
            row.add_prefix(&crate::ui::context_menu::swatch_widget(&p.tag.color, true));
            let check = gtk::CheckButton::new();
            check.set_active(true);
            check.set_valign(gtk::Align::Center);
            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));
            checks.push((check, p.tag.clone()));
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_propagate_natural_height(true);
        scroller.set_max_content_height(360);
        // Wider than the dialog's default column, so a row's keyword, count
        // and folders read on a line or two.
        scroller.set_size_request(520, -1);
        scroller.set_child(Some(&list));
        dialog.set_extra_child(Some(&scroller));

        // The report is the showcase's to capture (HYLKI_SHOWCASE_FIND_TAGS).
        if std::env::var("HYLKI_SHOWCASE_FIND_TAGS").is_ok() {
            if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                let d = dialog.clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    crate::ui::showcase::showcase_capture(d.upcast_ref(), &path);
                });
            }
        }

        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            let tags: Vec<crate::config::Tag> = match resp {
                "all" => checks.iter().map(|(_, t)| t.clone()).collect(),
                "selected" => checks
                    .iter()
                    .filter(|(c, _)| c.is_active())
                    .map(|(_, t)| t.clone())
                    .collect(),
                _ => return,
            };
            if !tags.is_empty() {
                s.input_sender().emit(AccountsInput::ImportTags(tags));
            }
        });
        dialog.present();
    }

    /// The Tags list (#71): a row per tag — its color as a disc, its name,
    /// its keyword — activating to edit, with a remove button.
    pub(super) fn rebuild_tag_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.tags_list else { return };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.tags.is_empty());
        for (i, t) in self.tags.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| s.input_sender().emit(AccountsInput::EditTag(i)));
            row.set_title(&gtk::glib::markup_escape_text(&t.name));
            row.set_subtitle(&gtk::glib::markup_escape_text(&t.keyword));
            let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            handle.add_css_class("dim-label");
            row.add_prefix(&handle);
            row.add_prefix(&crate::ui::context_menu::swatch_widget(&t.color, true));
            // The number that toggles it from the keyboard (#157), for the
            // first nine.
            if i < 9 {
                let key = gtk::Label::new(Some(&(i + 1).to_string()));
                key.add_css_class("shortcut-key");
                key.set_valign(gtk::Align::Center);
                key.set_tooltip_text(Some(
                    i18n("Toggles the tag on a message (with single-key shortcuts on)").as_str(),
                ));
                row.add_suffix(&key);
            }
            // Drag to reorder, as the accounts list does.
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::MOVE);
            let from = i as u32;
            drag.connect_prepare(move |_, _, _| {
                Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
            });
            row.add_controller(drag);
            let drop = gtk::DropTarget::new(gtk::glib::Type::U32, gtk::gdk::DragAction::MOVE);
            let to = i;
            let input = sender.input_sender().clone();
            drop.connect_drop(move |_, value, _, _| {
                if let Ok(from) = value.get::<u32>() {
                    let _ = input.send(AccountsInput::MoveTag {
                        from: from as usize,
                        to,
                    });
                    true
                } else {
                    false
                }
            });
            row.add_controller(drop);
            let rm = gtk::Button::from_icon_name("user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove tag").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input_sender().emit(AccountsInput::RemoveTag(i)));
            row.add_suffix(&rm);
            // A chevron says the row opens the tag's editor, as the
            // account and cloud rows do.
            let next = gtk::Image::from_icon_name("go-next-symbolic");
            next.add_css_class("dim-label");
            row.add_suffix(&next);
            list.append(&row);
        }
    }

    /// The tag dialog (#71): name, color, keyword. The keyword follows the
    /// name until the user edits it; editing an existing tag keeps its
    /// keyword unless changed, since the messages carry the keyword, not
    /// the name.
    pub(super) fn open_tag_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::Tag)>,
    ) {
        use crate::config::{Tag, TAG_COLORS};
        let parent = relm4::main_application().active_window();
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Tag"), i18n("Save"))
        } else {
            (i18n("Add Tag"), i18n("Add Tag"))
        };

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);

        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));

        let keyword_row = adw::EntryRow::new();
        keyword_row.set_title(&i18n("Keyword"));
        keyword_row.set_tooltip_text(Some(
            i18n("How the server stores the tag, without spaces. Thunderbird's built-in tags are $label1 to $label5.")
                .as_str(),
        ));

        // The keyword follows the name until it is typed into by hand. The
        // follow is done under a guard: set_text empties the field before
        // refilling it, and each step fires `changed`, so without the guard
        // the empty step read as hand-editing after the second letter.
        let keyword_touched = std::rc::Rc::new(std::cell::Cell::new(edit.is_some()));
        let syncing = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let keyword_row = keyword_row.clone();
            let touched = keyword_touched.clone();
            let syncing = syncing.clone();
            name_row.connect_changed(move |row| {
                if !touched.get() {
                    syncing.set(true);
                    keyword_row.set_text(&Tag::keyword_for(&row.text()));
                    syncing.set(false);
                }
            });
        }
        {
            let touched = keyword_touched.clone();
            let syncing = syncing.clone();
            keyword_row.connect_changed(move |_| {
                if !syncing.get() {
                    touched.set(true);
                }
            });
        }

        // The color: one disc per palette entry on a line of its own under
        // a caption (beside a title, eight discs left the title wrapping one
        // letter per line in the dialog's width). Toggle buttons in one
        // group behave as radios; the pressed one is the choice.
        let color_row = gtk::ListBoxRow::new();
        color_row.set_activatable(false);
        color_row.set_selectable(false);
        let color_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        color_box.set_margin_top(10);
        color_box.set_margin_bottom(10);
        color_box.set_margin_start(12);
        color_box.set_margin_end(12);
        let color_label = gtk::Label::new(Some(i18n("Color").as_str()));
        color_label.set_halign(gtk::Align::Start);
        color_label.add_css_class("caption");
        color_label.add_css_class("dim-label");
        color_box.append(&color_label);
        let swatches = gtk::FlowBox::new();
        swatches.set_selection_mode(gtk::SelectionMode::None);
        swatches.set_homogeneous(true);
        // Nine discs (the palette and the custom one) as two even rows.
        swatches.set_min_children_per_line(5);
        swatches.set_max_children_per_line(5);
        swatches.set_column_spacing(4);
        swatches.set_row_spacing(4);
        swatches.set_halign(gtk::Align::Start);
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(
            edit.as_ref()
                .map(|(_, t)| t.color.clone())
                .unwrap_or_else(|| TAG_COLORS[0].to_string()),
        ));
        let mut first: Option<gtk::ToggleButton> = None;
        for color in TAG_COLORS {
            let toggle = gtk::ToggleButton::new();
            toggle.add_css_class("flat");
            toggle.add_css_class("circular");
            toggle.set_child(Some(&crate::ui::context_menu::swatch_widget(color, true)));
            toggle.set_tooltip_text(Some(color));
            if let Some(f) = &first {
                toggle.set_group(Some(f));
            } else {
                first = Some(toggle.clone());
            }
            toggle.set_active(chosen.borrow().eq_ignore_ascii_case(color));
            let chosen = chosen.clone();
            let color = color.to_string();
            toggle.connect_toggled(move |t| {
                if t.is_active() {
                    *chosen.borrow_mut() = color.clone();
                }
            });
            swatches.insert(&toggle, -1);
        }
        // A ninth disc for any color (#147): a hue wheel until one is picked,
        // then the picked color. Pressing it opens the GTK color chooser;
        // it stays a member of the toggle group so a palette pick clears it.
        // A tag edited with a color outside the palette opens on this disc.
        {
            let custom: Option<String> = edit
                .as_ref()
                .map(|(_, t)| t.color.clone())
                .filter(|c| !TAG_COLORS.iter().any(|p| p.eq_ignore_ascii_case(c)));
            let toggle = gtk::ToggleButton::new();
            toggle.add_css_class("flat");
            toggle.add_css_class("circular");
            toggle.set_child(Some(&custom_swatch_widget(custom.as_deref())));
            toggle.set_tooltip_text(Some(i18n("Custom color…").as_str()));
            if let Some(f) = &first {
                toggle.set_group(Some(f));
            }
            toggle.set_active(custom.is_some());
            let custom = std::rc::Rc::new(std::cell::RefCell::new(custom));
            let chosen = chosen.clone();
            let parent = parent.clone();
            toggle.connect_clicked(move |t| {
                let picker = gtk::ColorDialog::new();
                picker.set_with_alpha(false);
                picker.set_title(&i18n("Tag Color"));
                let initial = gtk::gdk::RGBA::parse(chosen.borrow().as_str())
                    .unwrap_or(gtk::gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
                let t = t.clone();
                let chosen = chosen.clone();
                let custom = custom.clone();
                picker.choose_rgba(
                    parent.as_ref(),
                    Some(&initial),
                    None::<&gtk::gio::Cancellable>,
                    move |res| match res {
                        Ok(rgba) => {
                            let hex = crate::color::to_hex(&rgba);
                            t.set_child(Some(&custom_swatch_widget(Some(&hex))));
                            *custom.borrow_mut() = Some(hex.clone());
                            *chosen.borrow_mut() = hex;
                            t.set_active(true);
                        }
                        Err(_) => {
                            // Cancelled: back to whatever the choice was. A
                            // palette color lives in its own disc; a custom
                            // one picked earlier stays on this disc.
                            if custom.borrow().is_none() {
                                let back = chosen.borrow().clone();
                                let mut child = t
                                    .parent()
                                    .and_then(|p| p.parent())
                                    .and_then(|b| b.first_child());
                                while let Some(c) = child {
                                    if let Some(tb) =
                                        c.first_child().and_downcast::<gtk::ToggleButton>()
                                    {
                                        if tb
                                            .tooltip_text()
                                            .is_some_and(|tip| tip.eq_ignore_ascii_case(&back))
                                        {
                                            tb.set_active(true);
                                            break;
                                        }
                                    }
                                    child = c.next_sibling();
                                }
                            }
                        }
                    },
                );
            });
            swatches.insert(&toggle, -1);
        }
        color_box.append(&swatches);
        color_row.set_child(Some(&color_box));

        let edit_index = edit.as_ref().map(|(i, _)| *i);
        if let Some((_, tag)) = &edit {
            name_row.set_text(&tag.name);
            keyword_row.set_text(&tag.keyword);
        }

        form.append(&name_row);
        form.append(&color_row);
        form.append(&keyword_row);

        let s = sender.clone();
        self.push_form_page(nav, "tag", &title, &verb, &form, move || {
            let name = name_row.text().trim().to_string();
            if name.is_empty() {
                return false;
            }
            // A keyword the server would refuse falls back to the derived one.
            let typed = keyword_row.text().trim().to_string();
            let mut keyword = if Tag::valid_keyword(&typed) {
                typed
            } else {
                Tag::keyword_for(&name)
            };
            if keyword.is_empty() {
                keyword = "Tag".to_string();
            }
            let tag = Tag {
                name,
                keyword,
                color: chosen.borrow().clone(),
            };
            s.input_sender().emit(match edit_index {
                Some(i) => AccountsInput::TagEdited(i, tag),
                None => AccountsInput::TagAdded(tag),
            });
            true
        });
    }
}

impl AccountsWindow {
    pub(super) fn on_edit_tag(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(tag) = self.tags.get(i).cloned() {
            self.open_tag_page(&widgets.nav, &sender, Some((i, tag)));
        }
    }

    pub(super) fn on_remove_tag(&mut self, i: usize, sender: ComponentSender<Self>) {
        if i < self.tags.len() {
            self.tags.remove(i);
            self.rebuild_tag_rows(&sender);
            // The rules name tags by keyword; a rule whose tag is gone
            // shows the bare keyword until it is edited.
            self.rebuild_filter_rows(&sender);
            let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
        }
    }

    pub(super) fn on_move_tag(&mut self, from: usize, to: usize, sender: ComponentSender<Self>) {
        if from < self.tags.len() && from != to {
            let tag = self.tags.remove(from);
            let to = to.min(self.tags.len());
            self.tags.insert(to, tag);
            self.rebuild_tag_rows(&sender);
            let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
        }
    }

    pub(super) fn on_tag_added(&mut self, tag: crate::config::Tag, sender: ComponentSender<Self>) {
        self.tags.push(tag);
        self.rebuild_tag_rows(&sender);
        let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
    }

    pub(super) fn on_tag_edited(
        &mut self,
        i: usize,
        tag: crate::config::Tag,
        sender: ComponentSender<Self>,
    ) {
        if let Some(slot) = self.tags.get_mut(i) {
            if *slot != tag {
                *slot = tag;
                self.rebuild_tag_rows(&sender);
                self.rebuild_filter_rows(&sender);
                let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
            }
        }
    }

    pub(super) fn on_find_tags(&mut self, sender: ComponentSender<Self>) {
        if !self.tag_scanning {
            self.set_tag_scanning(true);
            let _ = sender.output(AccountsOutput::FindTags);
        }
    }

    pub(super) fn on_tag_findings(
        &mut self,
        found: Vec<TagProposal>,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        self.set_tag_scanning(false);
        let parent = root.root().and_downcast::<gtk::Window>();
        self.show_tag_findings(parent.as_ref(), found, &sender);
    }

    pub(super) fn on_import_tags(
        &mut self,
        tags: Vec<crate::config::Tag>,
        sender: ComponentSender<Self>,
    ) {
        let mut added = 0;
        for t in tags {
            if !self
                .tags
                .iter()
                .any(|x| x.keyword.eq_ignore_ascii_case(&t.keyword))
            {
                self.tags.push(t);
                added += 1;
            }
        }
        if added > 0 {
            self.rebuild_tag_rows(&sender);
            self.rebuild_filter_rows(&sender);
            let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_rgb_walks_the_colour_wheel() {
        assert_eq!(hue_rgb(0.0), (1.0, 0.0, 0.0));
        assert_eq!(hue_rgb(0.25), (0.5, 1.0, 0.0));
        assert_eq!(hue_rgb(0.5), (0.0, 1.0, 1.0));
        assert_eq!(hue_rgb(1.0), (1.0, 0.0, 0.0));
    }
}
