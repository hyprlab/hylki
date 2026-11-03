//! Grammar and style checking in the editor (#452): the document's script
//! hands over the text, a LanguageTool server is asked off the main thread,
//! and the problems go back to be marked. A right-click on one puts the
//! server's message and suggestions at the top of the menu.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::ObjectExt;
use serde::Deserialize;
use webkit6::prelude::WebViewExt;

use super::exec;
use crate::i18n::{i18n, i18n_f};
use crate::services::grammar;

/// One editor's side of the checking.
#[derive(Default)]
pub(super) struct Grammar {
    /// The host wants the text checked: only the composer does.
    on: Cell<bool>,
    /// Nothing is sent for now: the message is to go out encrypted.
    paused: Cell<bool>,
    /// A request is with the server; the next waits in `pending`, and only
    /// the newest of those is worth sending.
    busy: Cell<bool>,
    pending: RefCell<Option<Request>>,
    /// The problem a right-click landed on, for the menu that follows it,
    /// and when the document said so.
    hit: RefCell<Option<(Instant, Hit)>>,
    /// Why the last check failed, shown in the menu until one succeeds.
    error: RefCell<Option<String>>,
}

pub(super) type GrammarState = Rc<Grammar>;

#[derive(Deserialize)]
struct Request {
    seq: u64,
    text: String,
}

#[derive(Deserialize)]
pub(super) struct Hit {
    id: u64,
    message: String,
    replacements: Vec<String>,
    rule: String,
    text: String,
}

/// Listen for the document's requests and right-clicks, and switch the
/// script on in every document the editor loads once the host has asked.
pub(super) fn connect(
    webview: &webkit6::WebView,
    ucm: &webkit6::UserContentManager,
    state: &GrammarState,
) {
    ucm.register_script_message_handler("hylkiGrammar", None);
    ucm.register_script_message_handler("hylkiGrammarHit", None);
    {
        let (weak, state) = (webview.downgrade(), state.clone());
        ucm.connect_script_message_received(Some("hylkiGrammar"), move |_, value| {
            let Ok(req) = serde_json::from_str::<Request>(&value.to_str()) else {
                return;
            };
            request(&weak, &state, req);
        });
    }
    {
        let state = state.clone();
        ucm.connect_script_message_received(Some("hylkiGrammarHit"), move |_, value| {
            *state.hit.borrow_mut() = serde_json::from_str::<Option<Hit>>(&value.to_str())
                .ok()
                .flatten()
                .map(|hit| (Instant::now(), hit));
        });
    }
    let state = state.clone();
    webview.connect_load_changed(move |v, ev| {
        if ev == webkit6::LoadEvent::Finished && state.on.get() {
            exec(v, SWITCH_ON);
        }
    });
}

const SWITCH_ON: &str = "window.__hylkiGrammarOn && window.__hylkiGrammarOn()";

/// Start checking the text of this editor.
pub(super) fn switch_on(webview: &webkit6::WebView, state: &GrammarState) {
    if !state.on.replace(true) {
        exec(webview, SWITCH_ON);
        // LanguageTool on this computer takes seconds to start: it starts
        // as the composer opens, not when the first check is due.
        let settings = grammar::load();
        if settings.active() && settings.engine() == grammar::Engine::Local {
            grammar::local::warm();
        }
    }
}

/// Hold the text back while `on`, marks and all, and check it again when
/// let go.
pub(super) fn pause(webview: &webkit6::WebView, state: &GrammarState, on: bool) {
    if state.paused.replace(on) == on {
        return;
    }
    exec(
        webview,
        "window.__hylkiGrammarClear && window.__hylkiGrammarClear()",
    );
    if !on && state.on.get() {
        exec(webview, SWITCH_ON);
    }
}

fn request(weak: &gtk::glib::WeakRef<webkit6::WebView>, state: &GrammarState, req: Request) {
    if state.busy.get() {
        *state.pending.borrow_mut() = Some(req);
        return;
    }
    let settings = grammar::load();
    if !settings.active() || state.paused.get() {
        state.error.replace(None);
        show(weak, req.seq, "[]");
        return;
    }
    // The key is read from the keyring once, the first time a check
    // needs it, and on the keyring's own thread.
    let premium = !settings.username.trim().is_empty();
    let key = if premium {
        match grammar::known_key() {
            Some(k) => k,
            None => {
                state.busy.set(true);
                let (weak, state) = (weak.clone(), state.clone());
                crate::config::keyring_then(grammar::load_key, move |_| {
                    state.busy.set(false);
                    request(&weak, &state, req);
                });
                return;
            }
        }
    } else {
        String::new()
    };
    state.busy.set(true);
    // The languages chosen for spelling, as chosen: one swapped for the
    // nearest installed dictionary is still checked as itself here, since
    // the server has its own.
    let privacy = crate::config::load_privacy();
    let chosen = crate::desktop::spell::parse_languages(&privacy.spellcheck_langs);
    let languages = if chosen.is_empty() {
        crate::desktop::spell::system_spell_languages()
    } else {
        chosen
    };
    let spelling = privacy.spellcheck;
    let (weak, state) = (weak.clone(), state.clone());
    gtk::glib::spawn_future_local(async move {
        let Request { seq, text } = req;
        let found = gtk::gio::spawn_blocking(move || {
            grammar::check(&settings, &key, &text, &languages, spelling)
        })
        .await
        .unwrap_or_else(|_| Err(i18n("The check stopped unexpectedly.")));
        state.busy.set(false);
        match found {
            Ok(issues) => {
                state.error.replace(None);
                show(
                    &weak,
                    seq,
                    &serde_json::to_string(&issues).unwrap_or_else(|_| "[]".into()),
                );
            }
            Err(why) => {
                if state.error.borrow().as_deref() != Some(why.as_str()) {
                    tracing::warn!("grammar check failed: {why}");
                }
                state.error.replace(Some(why));
                show(&weak, seq, "[]");
            }
        }
        let next = state.pending.borrow_mut().take();
        if let Some(next) = next {
            request(&weak, &state, next);
        }
    });
}

fn show(weak: &gtk::glib::WeakRef<webkit6::WebView>, seq: u64, issues: &str) {
    if let Some(v) = weak.upgrade() {
        exec(
            &v,
            &format!("window.__hylkiGrammarShow && window.__hylkiGrammarShow({seq},{issues})"),
        );
    }
}

/// How long a right-click's place stays the one the menu is for.
const HIT_FRESH: Duration = Duration::from_secs(1);

/// The longest a problem's message is in the menu: some run to a
/// paragraph, and a menu that wide is hard to read.
const MESSAGE_CHARS: usize = 90;

/// Lead the menu with the problem under the pointer: what is wrong, the
/// suggestions, and Ignore. With none under it, why checking is not
/// working, if it is not. Returns how many items went in.
pub(super) fn lead_menu(
    menu: &webkit6::ContextMenu,
    view: &webkit6::WebView,
    state: &GrammarState,
) -> i32 {
    if !state.on.get() {
        return 0;
    }
    let mut at = 0;
    let mut put = |item: webkit6::ContextMenuItem| {
        menu.insert(&item, at);
        at += 1;
    };
    // The document says where the right-click was just before WebKit asks
    // for the menu. One said longer ago belongs to another click, and must
    // not offer a fix for a mark the pointer is not on.
    let hit = state
        .hit
        .borrow_mut()
        .take()
        .filter(|(when, _)| when.elapsed() < HIT_FRESH)
        .map(|(_, hit)| hit);
    let Some(hit) = hit else {
        if let Some(why) = state.error.borrow().as_ref() {
            if grammar::load().active() {
                put(note(
                    "hylki-grammar-error",
                    &i18n_f("Grammar check failed: {error}", &[("error", why)]),
                ));
                put(webkit6::ContextMenuItem::new_separator());
            }
        }
        return at;
    };
    put(note("hylki-grammar-note", &shorten(&hit.message)));
    if hit.replacements.is_empty() {
        put(note("hylki-grammar-none", &i18n("No suggestions")));
    }
    for (i, rep) in hit.replacements.iter().enumerate() {
        let action = gtk::gio::SimpleAction::new(&format!("hylki-grammar-fix-{i}"), None);
        let v = view.downgrade();
        let js = format!(
            "window.__hylkiGrammarFix({},{})",
            hit.id,
            serde_json::to_string(rep).unwrap_or_else(|_| "''".into())
        );
        action.connect_activate(move |_, _| {
            if let Some(v) = v.upgrade() {
                exec(&v, &js);
            }
        });
        let label = if rep.trim().is_empty() {
            i18n("Remove")
        } else {
            rep.clone()
        };
        put(webkit6::ContextMenuItem::from_gaction(
            &action, &label, None,
        ));
    }
    let action = gtk::gio::SimpleAction::new("hylki-grammar-ignore", None);
    let v = view.downgrade();
    let (rule, text) = (hit.rule.clone(), hit.text.clone());
    action.connect_activate(move |_, _| {
        grammar::ignore(&rule, &text);
        if let Some(v) = v.upgrade() {
            let (r, t) = (
                serde_json::to_string(&rule).unwrap_or_default(),
                serde_json::to_string(&text).unwrap_or_default(),
            );
            exec(&v, &format!("window.__hylkiGrammarIgnore({r},{t})"));
        }
    });
    put(webkit6::ContextMenuItem::from_gaction(
        &action,
        &i18n("Ignore"),
        None,
    ));
    put(webkit6::ContextMenuItem::new_separator());
    at
}

/// A line of the menu that only says something.
fn note(name: &str, text: &str) -> webkit6::ContextMenuItem {
    let action = gtk::gio::SimpleAction::new(name, None);
    action.set_enabled(false);
    webkit6::ContextMenuItem::from_gaction(&action, text, None)
}

fn shorten(message: &str) -> String {
    let message = message.trim();
    if message.chars().count() <= MESSAGE_CHARS {
        return message.to_string();
    }
    let cut: String = message.chars().take(MESSAGE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_are_cut_for_the_menu() {
        assert_eq!(shorten(" Use “à”. "), "Use “à”.");
        let long = "word ".repeat(40);
        let s = shorten(&long);
        assert_eq!(s.chars().count(), MESSAGE_CHARS);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn a_right_click_off_any_problem_reads_as_none() {
        assert!(serde_json::from_str::<Option<Hit>>("null")
            .unwrap()
            .is_none());
        let hit: Option<Hit> = serde_json::from_str(
            r#"{"id":3,"message":"m","replacements":["à"],"rule":"A","text":"a"}"#,
        )
        .unwrap();
        assert_eq!(hit.unwrap().replacements, ["à"]);
    }
}
