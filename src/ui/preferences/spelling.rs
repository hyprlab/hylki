//! Composing → Spelling: the dictionary switches (#365) and the personal
//! word list.

use super::rows::collect_named;
use super::*;

/// The spelling language switches (#365): "System language", then one per
/// installed dictionary, built a moment after the window is up. Moving
/// them to match the setting fires their notify handlers like a click
/// does; `syncing` tells the two apart.
#[derive(Clone)]
pub(super) struct SpellLangRows {
    pub(super) expander: adw::ExpanderRow,
    /// The setting the switches show: "" or "en_US, de_DE".
    pub(super) setting: Rc<std::cell::RefCell<String>>,
    pub(super) system: Rc<std::cell::RefCell<Option<adw::SwitchRow>>>,
    pub(super) dicts: Rc<std::cell::RefCell<Vec<(String, adw::SwitchRow)>>>,
    syncing: Rc<std::cell::Cell<bool>>,
}

impl SpellLangRows {
    pub(super) fn new(expander: adw::ExpanderRow, setting: String) -> Self {
        SpellLangRows {
            expander,
            setting: Rc::new(std::cell::RefCell::new(setting)),
            system: Default::default(),
            dicts: Default::default(),
            syncing: Default::default(),
        }
    }

    /// Show `setting` on the switches and in the row's subtitle.
    pub(super) fn show(&self, setting: &str) {
        *self.setting.borrow_mut() = setting.to_string();
        let chosen = crate::desktop::spell::parse_languages(setting);
        self.syncing.set(true);
        if let Some(row) = self.system.borrow().as_ref() {
            row.set_active(chosen.is_empty());
        }
        for (code, row) in self.dicts.borrow().iter() {
            row.set_active(chosen.contains(code));
        }
        self.syncing.set(false);
        let subtitle = if chosen.is_empty() {
            i18n_f(
                "System language ({language})",
                &[(
                    "language",
                    &crate::desktop::spell::spell_language_names(
                        &crate::desktop::spell::system_spell_languages(),
                    ),
                )],
            )
        } else {
            crate::desktop::spell::spell_language_names(&chosen)
        };
        self.expander
            .set_subtitle(&gtk::glib::markup_escape_text(&subtitle));
    }

    /// A switch was flipped by hand: the setting that results, after the
    /// switches are moved to show it. `None` while they are being moved.
    pub(super) fn toggled(&self, code: &str, on: bool) -> Option<String> {
        if self.syncing.get() {
            return None;
        }
        let current = self.setting.borrow().clone();
        let next = crate::desktop::spell::toggle_language(
            &current,
            code,
            on,
            &crate::desktop::spell::system_spell_languages(),
        );
        self.show(&next);
        (next != current).then_some(next)
    }
}

/// Rebuild the "Added words" expander's word rows from the personal word
/// list. The permanent "Add a word" entry row is left alone; word rows are
/// tagged with a widget name so only they are cleared. Each row's trash
/// button forgets its word and rebuilds.
pub(super) fn rebuild_personal_words(exp: &adw::ExpanderRow) {
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
    let words = crate::desktop::spell::personal_words();
    exp.set_enable_expansion(true);
    for w in words {
        let row = adw::ActionRow::builder().title(&w).build();
        row.set_widget_name("vireo-spell-word");
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk::Align::Center);
        del.set_tooltip_text(Some(i18n("Forget this word").as_str()));
        {
            let word = w.clone();
            let exp = exp.clone();
            del.connect_clicked(move |_| {
                crate::desktop::spell::remove_personal_word(&word);
                rebuild_personal_words(&exp);
            });
        }
        row.add_suffix(&del);
        exp.add_row(&row);
    }
}

impl Preferences {
    pub(super) fn on_set_spellcheck_langs(&mut self, langs: String) {
        if let Some(rows) = self.spell_langs.as_ref() {
            if *rows.setting.borrow() != langs {
                rows.show(&langs);
            }
        }
    }
}
