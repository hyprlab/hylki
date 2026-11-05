//! Proton's human verification (CAPTCHA). Proton can refuse a sign-in from
//! a new client or network until a person solves a challenge on
//! verify.proton.me. That page runs here in a view of its own, the way
//! Proton's own apps embed it, and the token it posts back on success is
//! what the retried sign-in sends (`native/proton/README.md`, `login`).
//!
//! The view is the only one in Hylki that runs a site's JavaScript: the
//! challenge is Proton's page, not mail, and does not work without it. It
//! has its own settings and an ephemeral session, so the reader's views
//! are untouched and nothing the page sets outlives the dialog.

use adw::prelude::*;
use serde_json::Value;
use webkit6::prelude::*;

use crate::i18n::i18n;
use crate::worker::{HelperError, HvChallenge};

/// The page's origin: the only one whose messages are read.
const VERIFY_ORIGIN: &str = "https://verify.proton.me";

/// The script message handler the injected script posts to.
const HANDLER: &str = "protonVerify";

/// The domain whose subdomains may load into the view. The page's own
/// `Content-Security-Policy` lets it frame only itself, `verify-api` and
/// `account` (`frame-src`), but the CAPTCHA page those frames serve allows
/// any https frame, so its pieces are let in from any of Proton's hosts
/// rather than from a list that would break when Proton moves one. Proton's
/// bare `proton.me` is its website, where links lead, and opens in the
/// browser.
const ALLOWED_DOMAIN: &str = ".proton.me";

/// Forwards the page's own messages. It posts them with
/// `window.parent.postMessage(…, "*")`, and a page shown at the top has
/// itself as its parent, so they land on this window. The origin goes with
/// each one so the native side checks it again rather than trust this.
const FORWARD_SCRIPT: &str = r#"
window.addEventListener('message', function (event) {
    if (event.origin !== 'https://verify.proton.me') {
        return;
    }
    try {
        window.webkit.messageHandlers.protonVerify.postMessage(
            JSON.stringify({ origin: event.origin, data: event.data }));
    } catch (e) {}
});
"#;

/// A solved challenge: what the retried sign-in sends as `hv_token` and
/// `hv_type`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Verified {
    pub(crate) token: String,
    pub(crate) kind: String,
}

/// Without the token, which stands in for the solved challenge until the
/// sign-in spends it, so it stays out of logs.
impl std::fmt::Debug for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verified")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Where a Proton sign-in stands with human verification. The form that
/// signs in (the editor, the wizard) keeps one.
#[derive(Default)]
pub(crate) struct HumanCheck {
    /// The challenge the last sign-in came back with: what the Verify
    /// button opens again after the dialog was closed.
    challenge: Option<HvChallenge>,
    /// The solved token, sent with each sign-in until one gets past it or
    /// it is refused. A sign-in it let through can still stop for a code or
    /// a mailbox password, and the retry with those carries it too.
    solved: Option<Verified>,
}

/// What the form does about a sign-in that stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HumanStep {
    /// Nothing to verify.
    Nothing,
    /// Show the challenge (`HumanCheck::challenge`).
    Show,
    /// The solved token was refused and no new challenge came with the
    /// refusal: sign in again without it, which brings a fresh one.
    Retry,
}

impl HumanCheck {
    pub(crate) fn challenge(&self) -> Option<&HvChallenge> {
        self.challenge.as_ref()
    }

    /// The solved token and its type, for the sign-in's `hv_token` and
    /// `hv_type`.
    pub(crate) fn solved(&self) -> (Option<String>, Option<String>) {
        match &self.solved {
            Some(v) => (Some(v.token.clone()), Some(v.kind.clone())),
            None => (None, None),
        }
    }

    /// Take the dialog's answer, and whether to sign in again with it: not
    /// when the form moved on (another provider, a sign-in that finished)
    /// while the dialog was open, which forgets the challenge.
    pub(crate) fn solve(&mut self, verified: Verified) -> bool {
        if self.challenge.is_none() {
            return false;
        }
        self.solved = Some(verified);
        true
    }

    /// Forget the challenge and its token: the sign-in finished, or the
    /// form was reset.
    pub(crate) fn clear(&mut self) {
        *self = HumanCheck::default();
    }

    /// Take in why a sign-in stopped. A demanded or refused verification
    /// spends the token sent with it; a new challenge replaces the old one.
    pub(crate) fn stopped(&mut self, error: &HelperError) -> HumanStep {
        let refused = error.code == "human_verification_failed";
        if !refused && !error.needs_human_verification() {
            return HumanStep::Nothing;
        }
        let had_token = self.solved.take().is_some();
        match error.hv_challenge() {
            Some(challenge) => {
                self.challenge = Some(challenge);
                HumanStep::Show
            }
            // Only after a token was sent, so the retry, which sends none,
            // cannot come back here.
            None if refused && had_token => HumanStep::Retry,
            None => {
                self.challenge = None;
                HumanStep::Nothing
            }
        }
    }
}

/// The page's messages that matter here. It sends others (its size, that
/// it loaded, its own errors, which it shows itself) that change nothing.
#[derive(Debug, PartialEq, Eq)]
enum PageMessage {
    Verified(Verified),
    /// The person dismissed the challenge in the page.
    Close,
}

/// The challenge page for `challenge`. `embed=true` makes the page post the
/// solved token rather than spend it itself, which is the token the sign-in
/// must present.
pub(crate) fn verify_url(challenge: &HvChallenge) -> String {
    let methods: Vec<String> = challenge
        .methods
        .iter()
        .map(|m| crate::encoding::percent::encode(m))
        .collect();
    format!(
        "{VERIFY_ORIGIN}/?methods={}&token={}&embed=true",
        methods.join(","),
        crate::encoding::percent::encode(&challenge.token)
    )
}

/// Read what the injected script forwarded: `{"origin", "data"}`, where
/// `data` is the page's JSON string. Anything from another origin, of
/// another shape, or that is not a success or a close is nothing.
fn parse_message(forwarded: &str) -> Option<PageMessage> {
    let envelope: Value = serde_json::from_str(forwarded).ok()?;
    if envelope.get("origin")?.as_str()? != VERIFY_ORIGIN {
        return None;
    }
    // The page posts a JSON string; an object would read the same.
    let message = match envelope.get("data")? {
        Value::String(text) => serde_json::from_str(text).ok()?,
        other => other.clone(),
    };
    match message.get("type")?.as_str()? {
        "HUMAN_VERIFICATION_SUCCESS" => {
            let payload = message.get("payload")?;
            let token = payload.get("token")?.as_str()?;
            let kind = payload.get("type")?.as_str()?;
            // What the helper takes: printable ASCII without spaces.
            let fits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic());
            (fits(token) && fits(kind)).then(|| {
                PageMessage::Verified(Verified {
                    token: token.to_string(),
                    kind: kind.to_string(),
                })
            })
        }
        "CLOSE" => Some(PageMessage::Close),
        _ => None,
    }
}

/// Whether `uri` may load in the view: https on one of Proton's subdomains,
/// the blank documents frames start from, and those hosts' blobs.
fn navigation_allowed(uri: &str) -> bool {
    if matches!(uri, "about:blank" | "about:srcdoc") {
        return true;
    }
    let target = uri.strip_prefix("blob:").unwrap_or(uri);
    let Ok(parsed) = gtk::glib::Uri::parse(target, gtk::glib::UriFlags::NONE) else {
        return false;
    };
    parsed.scheme().eq_ignore_ascii_case("https")
        && matches!(parsed.port(), -1 | 443)
        && parsed.userinfo().is_none()
        && parsed
            .host()
            .is_some_and(|host| host.to_ascii_lowercase().ends_with(ALLOWED_DOMAIN))
}

/// Show the challenge over `parent`, and hand `done` the solved token, or
/// none when the dialog was closed first. The dialog is returned for tests.
pub(crate) fn present(
    parent: &impl IsA<gtk::Widget>,
    challenge: &HvChallenge,
    done: impl FnOnce(Option<Verified>) + 'static,
) -> adw::Dialog {
    let ucm = webkit6::UserContentManager::new();
    ucm.register_script_message_handler(HANDLER, None);
    ucm.add_script(&webkit6::UserScript::new(
        FORWARD_SCRIPT,
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserScriptInjectionTime::Start,
        &["https://verify.proton.me/*"],
        &[],
    ));
    // Its own settings object: the reader's keep JavaScript to Hylki's own
    // wrapper, and a shared object would carry this change to them.
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_enable_developer_extras(false);
    let session = webkit6::NetworkSession::new_ephemeral();
    let view = webkit6::WebView::builder()
        .network_session(&session)
        .user_content_manager(&ucm)
        .settings(&settings)
        .build();
    view.set_vexpand(true);

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&view));
    let dialog = adw::Dialog::builder()
        .title(i18n("Human Verification"))
        .content_width(480)
        .content_height(640)
        .child(&toolbar)
        .build();

    // `done` runs once: on success, or when the dialog closes first.
    type DoneSlot = std::rc::Rc<std::cell::RefCell<Option<Box<dyn FnOnce(Option<Verified>)>>>>;
    let slot: DoneSlot = std::rc::Rc::new(std::cell::RefCell::new(Some(Box::new(done))));
    {
        let slot = slot.clone();
        let dialog = dialog.downgrade();
        ucm.connect_script_message_received(Some(HANDLER), move |_, value| {
            let Some(message) = parse_message(&value.to_str()) else {
                return;
            };
            if let PageMessage::Verified(verified) = message {
                if let Some(done) = slot.borrow_mut().take() {
                    done(Some(verified));
                }
            }
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }
    {
        let slot = slot.clone();
        dialog.connect_closed(move |_| {
            if let Some(done) = slot.borrow_mut().take() {
                done(None);
            }
        });
    }
    {
        // The page closes itself when it is done outside the embedded flow.
        let dialog = dialog.downgrade();
        view.connect_close(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }
    view.connect_decide_policy(|view, decision, kind| {
        let new_window = kind == webkit6::PolicyDecisionType::NewWindowAction;
        if kind != webkit6::PolicyDecisionType::NavigationAction && !new_window {
            return false;
        }
        let Some(mut action) = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|nav| nav.navigation_action())
        else {
            return false;
        };
        let uri = action
            .request()
            .and_then(|r| r.uri())
            .map(|u| u.to_string())
            .unwrap_or_default();
        if !new_window && navigation_allowed(&uri) {
            return false;
        }
        decision.ignore();
        // A link the person followed (Proton's terms, say) opens in the
        // browser; anything the page tries on its own is just dropped.
        let web = uri.starts_with("https://") || uri.starts_with("http://");
        if web && action.is_user_gesture() {
            let window = view.root().and_downcast::<gtk::Window>();
            crate::ui::launch::open_link(&uri, window.as_ref());
        }
        true
    });

    dialog.present(Some(parent));
    view.load_uri(&verify_url(challenge));
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarded(origin: &str, data: Value) -> String {
        serde_json::json!({ "origin": origin, "data": data }).to_string()
    }

    fn success(token: &str, kind: &str) -> String {
        serde_json::json!({
            "type": "HUMAN_VERIFICATION_SUCCESS",
            "payload": { "token": token, "type": kind },
        })
        .to_string()
    }

    #[test]
    fn the_posted_json_string_yields_the_token() {
        let posted = forwarded(VERIFY_ORIGIN, Value::String(success("abc-123", "captcha")));
        assert_eq!(
            parse_message(&posted),
            Some(PageMessage::Verified(Verified {
                token: "abc-123".into(),
                kind: "captcha".into(),
            }))
        );
        // The page's own dismissal.
        let close = forwarded(VERIFY_ORIGIN, Value::String(r#"{"type":"CLOSE"}"#.into()));
        assert_eq!(parse_message(&close), Some(PageMessage::Close));
    }

    #[test]
    fn other_message_types_are_nothing() {
        for data in [
            r#"{"type":"LOADED"}"#,
            r#"{"type":"RESIZE","payload":{"height":420}}"#,
            r#"{"type":"ERROR","payload":{"message":"x"}}"#,
            r#"{"type":"NOTIFICATION","payload":{"type":"error","text":"x"}}"#,
            r#"{"type":"human_verification_success","payload":{"token":"t","type":"captcha"}}"#,
        ] {
            let posted = forwarded(VERIFY_ORIGIN, Value::String(data.into()));
            assert_eq!(parse_message(&posted), None, "{data}");
        }
    }

    #[test]
    fn messages_from_another_origin_are_nothing() {
        let data = Value::String(success("abc", "captcha"));
        for origin in [
            "https://verify.proton.me.evil.example",
            "http://verify.proton.me",
            "https://verify-api.proton.me",
            "https://evil.example",
            "null",
            "",
        ] {
            assert_eq!(
                parse_message(&forwarded(origin, data.clone())),
                None,
                "{origin}"
            );
        }
        // No origin at all.
        let bare = serde_json::json!({ "data": success("abc", "captcha") }).to_string();
        assert_eq!(parse_message(&bare), None);
    }

    #[test]
    fn garbage_is_nothing() {
        for raw in [
            "",
            "not json",
            "[object Object]",
            "null",
            "42",
            r#""a string""#,
            r#"{"origin":"https://verify.proton.me"}"#,
            r#"{"origin":"https://verify.proton.me","data":"not json"}"#,
            r#"{"origin":"https://verify.proton.me","data":"{\"type\":7}"}"#,
        ] {
            assert_eq!(parse_message(raw), None, "{raw}");
        }
        // A success without a usable token or type.
        for (token, kind) in [
            ("", "captcha"),
            ("abc", ""),
            ("a b", "captcha"),
            ("abc", "cap\ncha"),
        ] {
            let posted = forwarded(VERIFY_ORIGIN, Value::String(success(token, kind)));
            assert_eq!(parse_message(&posted), None, "{token:?} {kind:?}");
        }
        let no_payload = forwarded(
            VERIFY_ORIGIN,
            Value::String(r#"{"type":"HUMAN_VERIFICATION_SUCCESS"}"#.into()),
        );
        assert_eq!(parse_message(&no_payload), None);
    }

    #[test]
    fn the_url_escapes_the_token_and_methods() {
        let challenge = HvChallenge {
            token: "a+b/c=d&e f".into(),
            methods: vec!["captcha".into(), "email".into()],
        };
        assert_eq!(
            verify_url(&challenge),
            "https://verify.proton.me/?methods=captcha,email&token=a%2Bb%2Fc%3Dd%26e%20f&embed=true"
        );
        // A method can't break out of its parameter either.
        let odd = HvChallenge {
            token: "t".into(),
            methods: vec!["sms&embed=false".into()],
        };
        assert_eq!(
            verify_url(&odd),
            "https://verify.proton.me/?methods=sms%26embed%3Dfalse&token=t&embed=true"
        );
    }

    #[test]
    fn only_the_challenge_hosts_load_in_the_view() {
        for uri in [
            "https://verify.proton.me/?methods=captcha&token=t&embed=true",
            "https://verify-api.proton.me/core/v4/captcha?Token=t",
            "https://account.proton.me/",
            // Further pieces of the CAPTCHA, wherever Proton serves them.
            "https://mail-api.proton.me/core/v4/captcha?Token=t",
            "https://VERIFY.proton.me/",
            "https://verify.proton.me:443/",
            "about:blank",
            "blob:https://verify.proton.me/0c5e6a1e",
        ] {
            assert!(navigation_allowed(uri), "{uri}");
        }
        for uri in [
            "http://verify.proton.me/",
            "https://verify.proton.me:8443/",
            // Proton's website, where its links lead: the browser's.
            "https://proton.me/legal/terms",
            "https://evilproton.me/",
            "https://verify.proton.me./",
            "https://verify.proton.me.evil.example/",
            "https://user@verify.proton.me/",
            "https://evil.example/?https://verify.proton.me/",
            "blob:https://evil.example/1",
            "data:text/html,hi",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!navigation_allowed(uri), "{uri}");
        }
    }

    fn challenge(token: &str) -> HvChallenge {
        HvChallenge {
            token: token.into(),
            methods: vec!["captcha".into()],
        }
    }

    #[test]
    fn a_challenge_is_shown_solved_and_spent() {
        let mut check = HumanCheck::default();
        assert_eq!(check.solved(), (None, None));
        // An answer with no challenge pending is dropped.
        assert!(!check.solve(Verified {
            token: "stale".into(),
            kind: "captcha".into(),
        }));
        assert_eq!(check.solved(), (None, None));
        // Not about verification: nothing changes.
        assert_eq!(
            check.stopped(&HelperError::new("login_failed")),
            HumanStep::Nothing
        );

        assert_eq!(
            check.stopped(&HelperError::with_challenge(challenge("c1"))),
            HumanStep::Show
        );
        assert_eq!(check.challenge().map(|c| c.token.as_str()), Some("c1"));
        check.solve(Verified {
            token: "solved".into(),
            kind: "captcha".into(),
        });
        assert_eq!(
            check.solved(),
            (Some("solved".into()), Some("captcha".into()))
        );
        // Let through, then stopped for a code: the token goes with the
        // retry that sends the code.
        assert_eq!(
            check.stopped(&HelperError::new("second_factor_required")),
            HumanStep::Nothing
        );
        assert_eq!(check.solved().0.as_deref(), Some("solved"));

        // Refused: sign in again without it, once.
        assert_eq!(
            check.stopped(&HelperError::new("human_verification_failed")),
            HumanStep::Retry
        );
        assert_eq!(check.solved(), (None, None));
        assert_eq!(
            check.stopped(&HelperError::new("human_verification_failed")),
            HumanStep::Nothing
        );

        // A fresh challenge replaces the old one and spends the token.
        check.solve(Verified {
            token: "solved".into(),
            kind: "captcha".into(),
        });
        assert_eq!(
            check.stopped(&HelperError::with_challenge(challenge("c2"))),
            HumanStep::Show
        );
        assert_eq!(check.challenge().map(|c| c.token.as_str()), Some("c2"));
        assert_eq!(check.solved(), (None, None));

        // A demand without a challenge cannot be shown.
        assert_eq!(
            check.stopped(&HelperError::new("human_verification_required")),
            HumanStep::Nothing
        );
        assert!(check.challenge().is_none());

        check.stopped(&HelperError::with_challenge(challenge("c3")));
        check.clear();
        assert!(check.challenge().is_none());
        assert_eq!(check.solved(), (None, None));
        assert!(!check.solve(Verified {
            token: "late".into(),
            kind: "captcha".into(),
        }));
    }

    #[test]
    fn the_token_stays_out_of_debug_output() {
        let verified = Verified {
            token: "very-secret-token".into(),
            kind: "captcha".into(),
        };
        let shown = format!("{verified:?}");
        assert!(shown.contains("captcha"));
        assert!(!shown.contains("very-secret-token"));
    }

    /// The dialog itself: it opens on the challenge's page, and a success
    /// the page posts from verify.proton.me reaches `done` and closes it.
    /// The page is stood in for by local HTML under Proton's origin, so no
    /// network is needed. Ignored by default: it needs a display. Run it on
    /// its own (GTK is initialised once per process, from one thread):
    ///
    /// ```text
    /// cargo test -- --ignored the_dialog_loads_the_challenge_and_hands_back_the_token
    /// ```
    #[test]
    #[ignore = "needs a display"]
    fn the_dialog_loads_the_challenge_and_hands_back_the_token() {
        use std::cell::RefCell;
        use std::rc::Rc;
        use std::time::{Duration, Instant};

        gtk::init().expect("a display");
        adw::init().expect("libadwaita");
        // The web and network processes can take a minute or more to start in
        // a sandbox without a session bus. Yield when there is nothing to
        // dispatch rather than spin, which starves them on a busy machine.
        let run_until = |what: &str, done: &dyn Fn() -> bool| {
            let start = Instant::now();
            let ctx = gtk::glib::MainContext::default();
            while !done() {
                assert!(start.elapsed() < Duration::from_secs(240), "{what}");
                if !ctx.iteration(false) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        let window = adw::Window::new();
        window.set_default_size(600, 800);
        window.present();

        let challenge = HvChallenge {
            token: "a+b".into(),
            methods: vec!["captcha".into(), "email".into()],
        };
        let answer: Rc<RefCell<Option<Option<Verified>>>> = Rc::default();
        let dialog = {
            let answer = answer.clone();
            present(&window, &challenge, move |v| *answer.borrow_mut() = Some(v))
        };
        let view = dialog
            .child()
            .and_downcast::<adw::ToolbarView>()
            .and_then(|t| t.content())
            .and_downcast::<webkit6::WebView>()
            .expect("the web view");
        assert_eq!(
            view.uri().as_deref(),
            Some("https://verify.proton.me/?methods=captcha,email&token=a%2Bb&embed=true")
        );
        // Scripts run in this view only, from its own settings object.
        let settings = WebViewExt::settings(&view).expect("settings");
        assert!(settings.enables_javascript());
        assert!(!settings.is_javascript_can_open_windows_automatically());

        // The page, as far as the script and the handler can tell.
        view.stop_loading();
        let finished = Rc::new(std::cell::Cell::new(false));
        {
            let finished = finished.clone();
            view.connect_load_changed(move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    finished.set(true);
                }
            });
        }
        view.load_html("<html><body>stand-in</body></html>", Some(VERIFY_ORIGIN));
        run_until("the stand-in page loads", &|| finished.get());

        // Its other messages change nothing.
        view.evaluate_javascript(
            "window.postMessage(JSON.stringify({type: 'LOADED'}), '*'); 0",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
        let settle = Instant::now();
        while settle.elapsed() < Duration::from_millis(500) {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(answer.borrow().is_none());

        view.evaluate_javascript(
            "window.postMessage(JSON.stringify({type: 'HUMAN_VERIFICATION_SUCCESS', \
             payload: {token: 'solved-token', type: 'captcha'}}), '*'); 0",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
        run_until("the token comes back", &|| answer.borrow().is_some());
        assert_eq!(
            answer.borrow_mut().take(),
            Some(Some(Verified {
                token: "solved-token".into(),
                kind: "captcha".into(),
            }))
        );
        run_until("the dialog closes", &|| dialog.parent().is_none());
    }

    /// Closing the dialog first is a cancel: `done` hears none, once.
    /// Ignored by default: it needs a display. Run it on its own.
    #[test]
    #[ignore = "needs a display"]
    fn closing_the_dialog_cancels() {
        use std::cell::RefCell;
        use std::rc::Rc;

        gtk::init().expect("a display");
        adw::init().expect("libadwaita");
        let window = adw::Window::new();
        window.present();
        let answers: Rc<RefCell<Vec<Option<Verified>>>> = Rc::default();
        let dialog = {
            let answers = answers.clone();
            present(&window, &challenge("t"), move |v| {
                answers.borrow_mut().push(v)
            })
        };
        dialog.close();
        let start = std::time::Instant::now();
        while answers.borrow().is_empty() && start.elapsed() < std::time::Duration::from_secs(10) {
            gtk::glib::MainContext::default().iteration(true);
        }
        assert_eq!(*answers.borrow(), vec![None]);
    }
}
