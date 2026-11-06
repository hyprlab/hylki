//! The account editor's signatures (#445): the lazily made rich editor,
//! and the list of named signatures it edits one at a time.

use super::*;

/// What signature `i` is called in the chooser and the composer: its name,
/// or its place in the list when it has none.
fn signature_label(signatures: &[NamedSignature], i: usize) -> String {
    match signatures.get(i).map(|s| s.name.trim()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => i18n_f("Signature {n}", &[("n", &(i + 1).to_string())]),
    }
}

/// Whether the editor's HTML is effectively empty (no visible content).
fn signature_is_empty(html: &str) -> bool {
    let stripped: String = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    stripped.replace("&nbsp;", " ").trim().is_empty()
}

impl AccountsWindow {
    /// The signature editor, created (and mounted in its holder) on first
    /// use: a WebKit view is the slowest thing the panel would otherwise
    /// build, for a field most visits never reach.
    pub(super) fn sig_editor(&mut self, widgets: &AccountsWindowWidgets) -> &RichEditor {
        self.sig_editor.get_or_insert_with(|| {
            let editor = RichEditor::new("", None, false, &[]);
            // The text keeps its height when the toolbar wraps or is
            // expanded to a second row; the panel grows instead.
            editor.set_text_height(130);
            editor.toolbar_end().append(&self.sig_source_btn);
            widgets.form.sig_holder.append(&editor.widget);
            editor
        })
    }

    /// Set "Edit HTML" and the formatting tools to the mode the signature
    /// editor is in: as source the tools would go nowhere, and the button
    /// goes back to the rendered signature.
    fn sync_source_button(&mut self, widgets: &AccountsWindowWidgets) {
        let source = self.sig_editor(widgets).source_kind().is_some();
        self.sig_editor(widgets).set_formatting_visible(!source);
        self.sig_source_btn.set_label(&if source {
            i18n("Preview")
        } else {
            i18n("Edit HTML")
        });
    }

    /// The shown signature as HTML, from the rich editor or, while it is
    /// shown as source, from that source, cleaned as an imported one is.
    pub(super) fn read_signature(
        &mut self,
        widgets: &AccountsWindowWidgets,
        cb: impl FnOnce(String) + 'static,
    ) {
        let editor = self.sig_editor(widgets);
        if editor.source_kind().is_some() {
            editor.extract_source(move |text| cb(rich_editor::signature_from_source(&text, None)));
        } else {
            editor.extract_html(cb);
        }
    }

    /// Put an account's signatures in the editor (#445), the first one
    /// shown. An account without any gets one empty signature to type in,
    /// which new messages start with once it has text, as before there
    /// could be several.
    pub(super) fn load_signatures(
        &mut self,
        widgets: &AccountsWindowWidgets,
        mut signatures: Vec<NamedSignature>,
        mut default: Option<usize>,
    ) {
        if signatures.is_empty() {
            signatures.push(NamedSignature {
                name: i18n("Signature"),
                sender_name: String::new(),
                html: String::new(),
            });
            default = Some(0);
        }
        self.sig_edits = signatures;
        self.sig_default = default;
        self.sig_touched = false;
        self.sig_reading = false;
        self.show_signature(widgets, 0);
    }

    /// Show signature `i` in the editor and set the rows above it to it.
    /// Their notifies arrive after this and find nothing changed.
    fn show_signature(&mut self, widgets: &AccountsWindowWidgets, i: usize) {
        self.sig_shown = i;
        let html = self
            .sig_edits
            .get(i)
            .map(|s| rich_editor::signature_to_html(&s.html))
            .unwrap_or_default();
        self.sig_editor(widgets).set_html(&html);
        self.sync_source_button(widgets);
        let labels: Vec<String> = (0..self.sig_edits.len())
            .map(|n| signature_label(&self.sig_edits, n))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        widgets
            .form
            .sig_choice_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        widgets.form.sig_choice_row.set_selected(i as u32);
        widgets.form.sig_name_row.set_text(
            self.sig_edits
                .get(i)
                .map(|s| s.name.as_str())
                .unwrap_or_default(),
        );
        widgets.form.sig_sender_row.set_text(
            self.sig_edits
                .get(i)
                .map(|s| s.sender_name.as_str())
                .unwrap_or_default(),
        );
        widgets
            .form
            .sig_default_row
            .set_active(self.sig_default == Some(i));
        let several = self.sig_edits.len() > 1;
        widgets.form.sig_choice_row.set_visible(several);
        widgets.form.sig_name_row.set_visible(several);
    }

    /// Read the shown signature's text back out of the editor before
    /// another is shown (`add` makes a new one); the answer is
    /// [`AccountsInput::SigStored`].
    fn read_back_signature(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
        add: bool,
    ) {
        self.sig_reading = true;
        let s = sender.clone();
        self.sig_editor(widgets)
            .is_dirty(move |dirty| s.input_sender().emit(AccountsInput::SigTextTouched(dirty)));
        let s = sender.clone();
        self.read_signature(widgets, move |html| {
            s.input_sender()
                .emit(AccountsInput::SigStored { html, add })
        });
    }

    /// The signatures Save writes: the empty ones left out, a missing name
    /// filled in, and the default followed to where it lands.
    pub(super) fn saved_signatures(&self) -> (Vec<NamedSignature>, Option<usize>) {
        let mut kept = Vec::new();
        let mut default = None;
        for (i, sig) in self.sig_edits.iter().enumerate() {
            if signature_is_empty(sig.html.trim()) {
                continue;
            }
            if self.sig_default == Some(i) {
                default = Some(kept.len());
            }
            kept.push(NamedSignature {
                name: signature_label(&self.sig_edits, i),
                sender_name: sig.sender_name.trim().to_string(),
                html: sig.html.clone(),
            });
        }
        (kept, default)
    }
}

impl AccountsWindow {
    pub(super) fn on_release_signature_editor(&mut self, widgets: &AccountsWindowWidgets) {
        let editing = widgets
            .nav
            .visible_page()
            .and_then(|p| p.tag())
            .is_some_and(|t| t == "editor");
        if !editing {
            if let Some(editor) = self.sig_editor.take() {
                // Kept for the next editor, which takes it in.
                editor.toolbar_end().remove(&self.sig_source_btn);
                widgets.form.sig_holder.remove(&editor.widget);
            }
        }
    }

    pub(super) fn on_signature_source_toggle(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if self.sig_reading {
            return;
        }
        // The view loads a new document either way, which forgets
        // that the old one was edited.
        let s = sender.clone();
        self.sig_editor(widgets)
            .is_dirty(move |dirty| s.input_sender().emit(AccountsInput::SigTextTouched(dirty)));
        let s = sender.clone();
        if self.sig_editor(widgets).source_kind().is_some() {
            self.sig_editor(widgets).extract_source(move |text| {
                s.input_sender().emit(AccountsInput::SignatureToRich(text))
            });
        } else {
            self.sig_editor(widgets).extract_html(move |html| {
                s.input_sender()
                    .emit(AccountsInput::SignatureToSource(html))
            });
        }
    }

    pub(super) fn on_signature_to_source(&mut self, html: String, widgets: &AccountsWindowWidgets) {
        self.sig_editor(widgets)
            .set_source(rich_editor::SourceKind::Html, &html);
        self.sync_source_button(widgets);
    }

    pub(super) fn on_signature_to_rich(&mut self, text: String, widgets: &AccountsWindowWidgets) {
        self.sig_editor(widgets)
            .set_html(&rich_editor::signature_from_source(&text, None));
        self.sync_source_button(widgets);
    }

    pub(super) fn on_signature_import(&mut self, sender: ComponentSender<Self>, root: &adw::Bin) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title(&i18n("Import Signature"));
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("HTML and text files")));
        filter.add_mime_type("text/html");
        filter.add_mime_type("text/plain");
        filter.add_suffix("html");
        filter.add_suffix("htm");
        filter.add_suffix("txt");
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        let parent = root.root().and_downcast::<gtk::Window>();
        let s = sender.input_sender().clone();
        dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                let _ = s.send(AccountsInput::SignatureImportFile(path));
            }
        });
    }

    pub(super) fn on_signature_import_file(
        &mut self,
        path: std::path::PathBuf,
        widgets: &AccountsWindowWidgets,
        root: &adw::Bin,
    ) {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                self.sig_editor(widgets)
                    .set_html(&rich_editor::signature_from_source(&text, path.parent()));
                self.sync_source_button(widgets);
            }
            Err(e) => {
                let host = root.root().and_downcast::<gtk::Window>();
                crate::ui::dialog::alert(
                    host.as_ref(),
                    i18n("Could Not Read the File").as_str(),
                    &format!("{}\n{e}", path.display()),
                );
            }
        }
    }

    pub(super) fn on_sig_show(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let pick = widgets.form.sig_choice_row.selected() as usize;
        if self.sig_reading || pick == self.sig_shown || pick >= self.sig_edits.len() {
            return;
        }
        self.read_back_signature(widgets, &sender, false);
    }

    pub(super) fn on_sig_add(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if !self.sig_reading {
            self.read_back_signature(widgets, &sender, true);
        }
    }

    pub(super) fn on_sig_delete(&mut self, widgets: &AccountsWindowWidgets) {
        if self.sig_reading || self.sig_edits.len() < 2 {
            return;
        }
        let gone = self.sig_shown;
        self.sig_edits.remove(gone);
        self.sig_default = match self.sig_default {
            Some(d) if d == gone => None,
            Some(d) if d > gone => Some(d - 1),
            d => d,
        };
        self.sig_touched = true;
        self.show_signature(widgets, gone.saturating_sub(1));
    }

    pub(super) fn on_sig_rename(&mut self, widgets: &AccountsWindowWidgets) {
        let name = widgets.form.sig_name_row.text().to_string();
        let shown = self.sig_shown;
        let Some(sig) = self.sig_edits.get_mut(shown) else {
            return;
        };
        if sig.name == name {
            return;
        }
        sig.name = name;
        self.sig_touched = true;
        // Setting the model's entry moves the chooser off it; its
        // notify finds it back on the shown one and does nothing.
        let label = signature_label(&self.sig_edits, shown);
        if let Some(list) = widgets
            .form
            .sig_choice_row
            .model()
            .and_downcast::<gtk::StringList>()
        {
            list.splice(shown as u32, 1, &[label.as_str()]);
            widgets.form.sig_choice_row.set_selected(shown as u32);
        }
    }

    pub(super) fn on_sig_sender_rename(&mut self, widgets: &AccountsWindowWidgets) {
        let sender_name = widgets.form.sig_sender_row.text().trim().to_string();
        let shown = self.sig_shown;
        let Some(sig) = self.sig_edits.get_mut(shown) else {
            return;
        };
        if sig.sender_name == sender_name {
            return;
        }
        sig.sender_name = sender_name;
        self.sig_touched = true;
    }

    pub(super) fn on_sig_default(&mut self, widgets: &AccountsWindowWidgets) {
        let on = widgets.form.sig_default_row.is_active();
        let shown = Some(self.sig_shown);
        let default = match (on, self.sig_default == shown) {
            (true, false) => shown,
            (false, true) => None,
            _ => return,
        };
        self.sig_default = default;
        self.sig_touched = true;
    }

    pub(super) fn on_sig_stored(
        &mut self,
        html: String,
        add: bool,
        widgets: &AccountsWindowWidgets,
    ) {
        self.sig_reading = false;
        if let Some(shown) = self.sig_edits.get_mut(self.sig_shown) {
            shown.html = html;
        }
        let next = if add {
            let n = self.sig_edits.len() + 1;
            self.sig_edits.push(NamedSignature {
                name: i18n_f("Signature {n}", &[("n", &n.to_string())]),
                sender_name: String::new(),
                html: String::new(),
            });
            self.sig_touched = true;
            self.sig_edits.len() - 1
        } else {
            widgets.form.sig_choice_row.selected() as usize
        };
        self.show_signature(widgets, next.min(self.sig_edits.len().saturating_sub(1)));
        if add {
            widgets.form.sig_name_row.grab_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_emptiness_ignores_tags_and_spaces_but_also_images() {
        assert!(signature_is_empty(""));
        assert!(signature_is_empty("<p></p>"));
        assert!(signature_is_empty("<p> &nbsp; </p><br>"));
        // An image-only signature counts as empty (see Suspected bugs).
        assert!(signature_is_empty("<img src=\"cid:logo\">"));
        assert!(!signature_is_empty("<b>Jo</b>"));
        assert!(!signature_is_empty("a < b"));
    }

    #[test]
    fn a_signature_without_a_name_is_called_by_its_place() {
        let sigs = vec![
            NamedSignature {
                name: " Work ".into(),
                sender_name: "Ada".into(),
                html: String::new(),
            },
            NamedSignature {
                name: "  ".into(),
                sender_name: String::new(),
                html: String::new(),
            },
        ];
        assert_eq!(signature_label(&sigs, 0), "Work");
        assert_eq!(signature_label(&sigs, 1), "Signature 2");
        assert_eq!(signature_label(&sigs, 4), "Signature 5");
    }
}
