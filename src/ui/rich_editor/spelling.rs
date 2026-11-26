//! Spell-check languages for the editor's WebKit context, and the Spelling
//! submenu of its context menu.

use super::SpellLangsCallback;
use crate::i18n::{i18n, i18n_f};

/// Push the spell-checking preference onto the shared web context (#114).
/// Checking only happens in editable views, so the reader — which shares the
/// context — is unaffected.
///
/// The languages are ALWAYS set when checking is on: enchant is given no
/// language at all until told, so "enabled" without this call underlines
/// nothing. The configured languages win; blank follows the session locale;
/// and each is swapped for the closest installed dictionary when its own
/// is missing, since a language without a dictionary silently checks
/// nothing. WebKit accepts a word any of the languages knows (#365).
pub(crate) fn apply_spellcheck() {
    let ctx = crate::ui::message_view::shared_web_context();
    let on = crate::config::load_privacy().spellcheck;
    ctx.set_spell_checking_enabled(on);
    if !on {
        return;
    }
    let langs = crate::desktop::spell::resolved_spell_languages();
    let langs: Vec<&str> = langs.iter().map(String::as_str).collect();
    ctx.set_spell_checking_languages(&langs);
}

/// The right-click menu's Spelling Language submenu (#365): the system
/// language, then a check item per installed dictionary, ticked as the
/// setting stands. Choosing one switches it as the matching switch in
/// Settings does, and `cb` takes the setting that results.
pub(super) fn append_spelling_menu(menu: &webkit6::ContextMenu, cb: &SpellLangsCallback) {
    use gtk::prelude::ToVariant;
    if !crate::config::load_privacy().spellcheck {
        return;
    }
    let dicts = crate::desktop::spell::installed_dictionaries();
    if dicts.is_empty() {
        return;
    }
    let setting = crate::config::load_privacy().spellcheck_langs;
    let chosen = crate::desktop::spell::parse_languages(&setting);
    let system = crate::desktop::spell::system_spell_languages();
    let system_label = i18n_f(
        "System language ({language})",
        &[(
            "language",
            &crate::desktop::spell::spell_language_names(&system),
        )],
    );
    let entries = std::iter::once((String::new(), system_label, chosen.is_empty())).chain(
        dicts.iter().map(|d| {
            (
                d.clone(),
                crate::desktop::spell::language_display_name(d),
                chosen.contains(d),
            )
        }),
    );
    let sub = webkit6::ContextMenu::new();
    for (i, (code, label, on)) in entries.enumerate() {
        // A boolean state is what makes WebKit draw a check item.
        let action = gtk::gio::SimpleAction::new_stateful(
            &format!("hylki-spell-lang-{i}"),
            None,
            &on.to_variant(),
        );
        let (cb, setting, system) = (cb.clone(), setting.clone(), system.clone());
        action.connect_activate(move |_, _| {
            let next = crate::desktop::spell::toggle_language(&setting, &code, !on, &system);
            if let Some(f) = cb.borrow().as_ref() {
                f(next);
            }
        });
        sub.append(&webkit6::ContextMenuItem::from_gaction(
            &action, &label, None,
        ));
        if i == 0 {
            sub.append(&webkit6::ContextMenuItem::new_separator());
        }
    }
    menu.append(&webkit6::ContextMenuItem::new_separator());
    menu.append(&webkit6::ContextMenuItem::with_submenu(
        &i18n("Spelling Language"),
        &sub,
    ));
}
