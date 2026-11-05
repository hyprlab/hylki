//! The helper transport against shell scripts standing in for a helper.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::test_support::{
    fake_helper, flooding_helper, hanging_helper, streaming_helper, stubborn_helper, FakeHelper,
};
use super::*;

const SPEC: HelperSpec = HelperSpec {
    name: "hylki-test-helper",
    codes: &["login_failed", "session_revoked", "second_factor_required"],
    max_response_bytes: 1000,
    timeout: Duration::from_secs(20),
    rotates_sessions: false,
};

fn transport_for(helper: &FakeHelper, spec: HelperSpec) -> Transport {
    let path = helper.path();
    Arc::new(move |request| run_helper(&path, request, spec))
}

fn session() -> String {
    json!({"login": "Me@Tuta.com", "user_id": "u", "access_token": "TOKEN-SECRET"}).to_string()
}

#[test]
fn a_helper_is_found_only_where_it_can_run() {
    use std::os::unix::fs::PermissionsExt;
    let helper = fake_helper("", 0);
    let dir = helper.path().parent().unwrap().to_path_buf();
    assert_eq!(
        find_in("helper", Some(dir.as_os_str())),
        Some(helper.path())
    );
    std::fs::set_permissions(helper.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(find_in("helper", Some(dir.as_os_str())), None);
    assert_eq!(find_in("helper", None), None);
}

#[test]
fn a_missing_helper_says_so() {
    let transport = process_transport(HelperSpec {
        name: "hylki-no-such-helper-anywhere",
        ..SPEC
    });
    let err = request(&transport, SPEC, "sync", Object::new()).unwrap_err();
    assert_eq!(err.code, "helper_missing");
    let gone = std::env::temp_dir().join("hylki-helper-that-is-not-there");
    let err = run_helper(&gone, &json!({}), SPEC).unwrap_err();
    assert_eq!(err.code, "helper_missing");
}

#[test]
fn the_request_goes_on_stdin_with_its_version_and_nothing_in_argv() {
    let helper = fake_helper(r#"{"version":1,"result":{"ok":true}}"#, 0);
    let transport = transport_for(&helper, SPEC);
    let mut parameters = Object::new();
    parameters.insert("folder".into(), "a/b".into());
    let result = request(&transport, SPEC, "sync", parameters).unwrap();
    assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(helper.argc(), 0);
    assert_eq!(
        helper.stdin(),
        json!({"version": 1, "op": "sync", "folder": "a/b"})
    );
}

#[test]
fn a_helper_that_never_answers_is_stopped() {
    let helper = hanging_helper();
    let spec = HelperSpec {
        timeout: Duration::from_millis(300),
        ..SPEC
    };
    let started = std::time::Instant::now();
    let err = run_helper(&helper.path(), &json!({}), spec).unwrap_err();
    assert_eq!(err.code, "timeout");
    assert!(started.elapsed() < Duration::from_secs(4));
    // What it printed to stderr went nowhere.
    assert!(!format!("{err:?}").contains("secret-token"));
}

#[test]
fn a_reply_that_is_not_a_versioned_object_is_refused() {
    for (stdout, code) in [
        ("not json", "invalid_response"),
        ("[1,2]", "invalid_response"),
        (r#"{"version":2,"result":{}}"#, "unsupported_version"),
        (r#"{"result":{}}"#, "unsupported_version"),
        (r#"{"version":1,"result":"text"}"#, "invalid_response"),
        (r#"{"version":1}"#, "invalid_response"),
    ] {
        let helper = fake_helper(stdout, 0);
        let err = request(&transport_for(&helper, SPEC), SPEC, "sync", Object::new()).unwrap_err();
        assert_eq!(err.code, code, "{stdout}");
    }
}

#[test]
fn an_unclean_exit_fails_whatever_it_printed() {
    let helper = fake_helper(r#"{"version":1,"result":{}}"#, 3);
    let err = request(&transport_for(&helper, SPEC), SPEC, "sync", Object::new()).unwrap_err();
    assert_eq!(err.code, "helper_failed");
}

#[test]
fn known_codes_pass_through_and_the_rest_are_generic() {
    let helper = fake_helper(r#"{"version":1,"error":{"code":"session_revoked"}}"#, 0);
    let err = request(&transport_for(&helper, SPEC), SPEC, "sync", Object::new()).unwrap_err();
    assert_eq!(err.code, "session_revoked");
    let helper = fake_helper(
        r#"{"version":1,"error":{"code":"Server said: <b>nope</b>"}}"#,
        0,
    );
    let err = request(&transport_for(&helper, SPEC), SPEC, "sync", Object::new()).unwrap_err();
    assert_eq!(err.code, "request_failed");
    assert!(!err.message().contains("nope"));
}

#[test]
fn an_oversized_reply_is_refused_before_it_is_parsed() {
    let helper = flooding_helper(5000);
    let started = std::time::Instant::now();
    let err = run_helper(&helper.path(), &json!({}), SPEC).unwrap_err();
    assert_eq!(err.code, "response_too_large");
    // Stopped once the cap was passed, not at the deadline.
    assert!(started.elapsed() < Duration::from_secs(4));
}

fn sign_in(email: &str, totp: Option<&str>) -> HelperLogin {
    HelperLogin {
        email: email.into(),
        password: "pw".into(),
        totp: totp.map(str::to_string),
        ..HelperLogin::default()
    }
}

#[test]
fn login_returns_the_session_for_that_address_only() {
    let reply = json!({"version": 1, "result": {"session": serde_json::from_str::<Value>(&session()).unwrap()}});
    let helper = fake_helper(&reply.to_string(), 0);
    let transport = transport_for(&helper, SPEC);
    let stored = login(&transport, SPEC, &sign_in("me@tuta.com", Some("123456"))).unwrap();
    assert!(session_login_matches(
        &serde_json::from_str(&stored).unwrap(),
        "me@tuta.com"
    ));
    // The fields a sign-in did not need are not sent at all: Tuta's helper
    // refuses a field it does not know.
    assert_eq!(
        helper.stdin(),
        json!({"version": 1, "op": "login", "email": "me@tuta.com", "password": "pw", "totp": "123456"})
    );
    let err = login(&transport, SPEC, &sign_in("someone@else.com", None)).unwrap_err();
    assert_eq!(err.code, "invalid_response");
}

#[test]
fn a_full_sign_in_sends_the_mailbox_password_and_the_solved_challenge() {
    let reply = json!({"version": 1, "result": {"session": serde_json::from_str::<Value>(&session()).unwrap()}});
    let helper = fake_helper(&reply.to_string(), 0);
    let full = HelperLogin {
        mailbox_password: Some("mailbox".into()),
        hv_token: Some("solved".into()),
        hv_type: Some("captcha".into()),
        ..sign_in("me@tuta.com", Some("123456"))
    };
    login(&transport_for(&helper, SPEC), SPEC, &full).unwrap();
    assert_eq!(
        helper.stdin(),
        json!({"version": 1, "op": "login", "email": "me@tuta.com", "password": "pw",
            "totp": "123456", "mailbox_password": "mailbox", "hv_token": "solved",
            "hv_type": "captcha"})
    );
    let shown = format!("{full:?}");
    for secret in ["pw", "mailbox", "solved", "123456"] {
        assert!(!shown.contains(&format!("\"{secret}\"")), "{shown}");
    }
}

#[test]
fn a_second_factor_is_asked_for_by_code() {
    let helper = fake_helper(
        r#"{"version":1,"error":{"code":"second_factor_required"}}"#,
        0,
    );
    let err = login(
        &transport_for(&helper, SPEC),
        SPEC,
        &sign_in("me@tuta.com", None),
    )
    .unwrap_err();
    assert!(err.needs_second_factor());
    assert!(!HelperError::new("login_failed").needs_second_factor());
    // Every code reads as some sentence, an unknown one too.
    for code in ["helper_failed", "anything at all", "send_uncertain"] {
        assert!(!HelperError::new(code).message().is_empty());
    }
}

const PROTON_SPEC: HelperSpec = HelperSpec {
    codes: &[
        "mailbox_password_required",
        "human_verification_required",
        "human_verification_failed",
        "session_revoked",
    ],
    rotates_sessions: true,
    ..SPEC
};

fn answering(reply: Value) -> Transport {
    Arc::new(move |_| Ok(reply.as_object().unwrap().clone()))
}

#[test]
fn a_verification_challenge_comes_with_its_refusal() {
    let refusal = json!({"version": 1, "error": {"code": "human_verification_required",
        "hv": {"token": "challenge", "methods": ["captcha", "email", 7, ""]}}});
    let err = login(
        &answering(refusal),
        PROTON_SPEC,
        &sign_in("me@tuta.com", None),
    )
    .unwrap_err();
    assert!(err.needs_human_verification());
    let hv = err.hv_challenge().expect("the challenge is kept");
    assert_eq!(hv.token, "challenge");
    assert_eq!(hv.methods, ["captcha", "email"]);
    // The token is the server's, and stays out of the log.
    assert!(!format!("{err:?}").contains("challenge\""), "{err:?}");

    // A challenge with nothing to solve it by is no challenge.
    let bare = json!({"version": 1, "error": {"code": "human_verification_required",
        "hv": {"token": "challenge", "methods": []}}});
    let err = login(&answering(bare), PROTON_SPEC, &sign_in("me@tuta.com", None)).unwrap_err();
    assert!(err.needs_human_verification() && err.hv_challenge().is_none());

    // A spent token asks for a new challenge, and carries none.
    let spent = json!({"version": 1, "error": {"code": "human_verification_failed",
        "hv": {"token": "x", "methods": ["captcha"]}}});
    let err = login(
        &answering(spent),
        PROTON_SPEC,
        &sign_in("me@tuta.com", None),
    )
    .unwrap_err();
    assert!(!err.needs_human_verification() && err.hv_challenge().is_none());
    assert_eq!(err.code, "human_verification_failed");

    // Only the codes the spec lists pass, and only a listed code's
    // challenge is read.
    let other = json!({"version": 1, "error": {"code": "human_verification_required",
        "hv": {"token": "challenge", "methods": ["captcha"]}}});
    let err = login(&answering(other), SPEC, &sign_in("me@tuta.com", None)).unwrap_err();
    assert_eq!(err.code, "request_failed");
    assert!(err.hv_challenge().is_none());
}

#[test]
fn a_two_password_account_asks_for_its_mailbox_password() {
    let refusal = json!({"version": 1, "error": {"code": "mailbox_password_required"}});
    let err = login(
        &answering(refusal),
        PROTON_SPEC,
        &sign_in("me@tuta.com", None),
    )
    .unwrap_err();
    assert!(err.needs_mailbox_password());
    assert!(!err.needs_second_factor() && !err.needs_human_verification());
    assert!(!HelperError::new("mailbox_password_failed").needs_mailbox_password());
}

fn rotated(token: &str) -> Value {
    json!({"login": "Me@Tuta.com", "user_id": "u", "access_token": token})
}

#[test]
fn a_rotated_session_is_kept_on_success_and_on_failure() {
    let presented: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
    let seen = presented.clone();
    let replies = std::sync::Mutex::new(vec![
        // Last first: popped from the end.
        json!({"version": 1, "result": {}}),
        json!({"version": 1, "error": {"code": "session_revoked", "session": rotated("third")}}),
        json!({"version": 1, "result": {"session": rotated("second")}}),
        // A session for another address is never taken.
        json!({"version": 1, "result": {"session": {"login": "other@tuta.com"}}}),
    ]);
    let transport: Transport = Arc::new(move |request: &Value| {
        seen.lock().unwrap().push(request["session"].clone());
        let reply = replies.lock().unwrap().pop().unwrap();
        Ok(reply.as_object().unwrap().clone())
    });
    let live = Arc::new(LiveSession::default());
    let client = HelperClient::shared(
        PROTON_SPEC,
        transport,
        "me@tuta.com",
        live.clone(),
        &session(),
    )
    .unwrap();
    client.request("sync", Object::new()).unwrap();
    assert_eq!(client.session(), session());
    client.request("sync", Object::new()).unwrap();
    assert_eq!(client.session(), rotated("second").to_string());
    let err = client.request("sync", Object::new()).unwrap_err();
    assert_eq!(err.code, "session_revoked");
    // The failure's session is kept too: its refresh token is the live one.
    assert_eq!(live.latest(), rotated("third").to_string());
    client.request("sync", Object::new()).unwrap();
    let tokens: Vec<Value> = presented
        .lock()
        .unwrap()
        .iter()
        .map(|s| s["access_token"].clone())
        .collect();
    assert_eq!(tokens, ["TOKEN-SECRET", "TOKEN-SECRET", "second", "third"]);

    // A second client on the same account picks up the newest session,
    // not the stale stored copy it was handed.
    let transport: Transport = Arc::new(|_| Err(HelperError::new("helper_failed")));
    let later =
        HelperClient::shared(PROTON_SPEC, transport, "me@tuta.com", live, &session()).unwrap();
    assert_eq!(later.session(), rotated("third").to_string());
}

#[test]
fn calls_on_one_session_take_turns() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let running = Arc::new(AtomicUsize::new(0));
    let overlapped = Arc::new(AtomicUsize::new(0));
    let (r, o) = (running.clone(), overlapped.clone());
    let transport: Transport = Arc::new(move |_| {
        if r.fetch_add(1, Ordering::SeqCst) > 0 {
            o.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(Duration::from_millis(20));
        r.fetch_sub(1, Ordering::SeqCst);
        Ok(json!({"version": 1, "result": {}})
            .as_object()
            .unwrap()
            .clone())
    });
    let live = Arc::new(LiveSession::default());
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let client = HelperClient::shared(
                PROTON_SPEC,
                transport.clone(),
                "me@tuta.com",
                live.clone(),
                &session(),
            )
            .unwrap();
            std::thread::spawn(move || client.request("sync", Object::new()).unwrap())
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(overlapped.load(Ordering::SeqCst), 0);
}

#[test]
fn a_client_checks_its_session_and_sends_it_with_each_request() {
    let transport: Transport = Arc::new(|_| Err(HelperError::new("helper_failed")));
    let err = HelperClient::new(SPEC, transport.clone(), "me@tuta.com", "").unwrap_err();
    assert_eq!(err.code, "not_signed_in");
    let err = HelperClient::new(SPEC, transport.clone(), "me@tuta.com", "{").unwrap_err();
    assert_eq!(err.code, "session_invalid");
    let err = HelperClient::new(SPEC, transport, "other@tuta.com", &session()).unwrap_err();
    assert_eq!(err.code, "session_invalid");

    let helper = fake_helper(r#"{"version":1,"result":{}}"#, 0);
    let client = HelperClient::new(
        SPEC,
        transport_for(&helper, SPEC),
        "me@tuta.com",
        &session(),
    )
    .unwrap();
    client.request("sync", Object::new()).unwrap();
    assert_eq!(helper.stdin()["session"]["access_token"], "TOKEN-SECRET");
}

#[test]
fn secrets_stay_out_of_debug_output() {
    let transport: Transport = Arc::new(|_| Err(HelperError::new("helper_failed")));
    let client = HelperClient::new(SPEC, transport, "me@tuta.com", &session()).unwrap();
    let shown = format!("{client:?}");
    assert!(!shown.contains("TOKEN-SECRET"), "{shown}");
    assert!(!shown.contains("Me@Tuta.com"), "{shown}");
    let err = HelperError::new("login_failed");
    assert_eq!(
        format!("{err:?}"),
        r#"HelperError { code: "login_failed" }"#
    );
}

/// A streaming run's lines until it says nothing for `quiet`.
fn lines_of(watch: &mut Watch, quiet: Duration) -> Vec<Result<Value, String>> {
    let mut out = Vec::new();
    loop {
        match watch.next(quiet) {
            Ok(Some(line)) => out.push(Ok(Value::Object(line))),
            Ok(None) => return out,
            Err(e) => {
                out.push(Err(e.code));
                return out;
            }
        }
    }
}

fn client_of(helper: &FakeHelper) -> HelperClient {
    HelperClient::new(
        SPEC,
        transport_for(helper, SPEC),
        "me@tuta.com",
        &session(),
    )
    .unwrap()
}

#[test]
fn a_streaming_run_reads_line_after_line_until_its_stdin_closes() {
    let helper = streaming_helper(
        "{\"version\":1,\"event\":\"ready\"}\n\n{\"version\":1,\"event\":\"mail_changed\",\"folder\":\"a/b\"}\n",
        true,
    );
    let mut watch = client_of(&helper).watch(&helper.path(), "watch").unwrap();
    let lines = lines_of(&mut watch, Duration::from_millis(500));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0].as_ref().unwrap()["event"], "ready");
    assert_eq!(lines[1].as_ref().unwrap()["folder"], "a/b");
    // The request went on stdin, the session in it, nothing in argv.
    assert_eq!(helper.argc(), 0);
    let request = helper.stdin();
    assert_eq!(request["op"], "watch");
    assert_eq!(request["version"], 1);
    assert_eq!(request["session"]["access_token"], "TOKEN-SECRET");
    assert!(!helper.ended());
    // Closing stdin is the stop signal, and the helper leaves on its own.
    drop(watch);
    assert!(helper.ended());
    assert!(!format!("{:?}", client_of(&helper)).contains("TOKEN-SECRET"));
}

#[test]
fn a_streaming_run_stops_at_the_first_line_it_cannot_take() {
    for (stdout, code) in [
        (
            r#"{"version":1,"error":{"code":"session_revoked"}}"#,
            "session_revoked",
        ),
        (
            r#"{"version":1,"error":{"code":"Server said: no"}}"#,
            "request_failed",
        ),
        (r#"{"version":2,"event":"ready"}"#, "unsupported_version"),
        ("not json", "invalid_response"),
        ("[1]", "invalid_response"),
    ] {
        let helper = streaming_helper(
            &format!("{stdout}\n{{\"version\":1,\"event\":\"ready\"}}\n"),
            true,
        );
        let mut watch = client_of(&helper).watch(&helper.path(), "watch").unwrap();
        let lines = lines_of(&mut watch, Duration::from_millis(500));
        assert_eq!(lines, [Err(code.to_string())], "{stdout}");
    }
}

#[test]
fn a_streaming_run_that_ends_or_floods_says_so() {
    let helper = streaming_helper("{\"version\":1,\"event\":\"ready\"}\n", false);
    let mut watch = client_of(&helper).watch(&helper.path(), "watch").unwrap();
    let lines = lines_of(&mut watch, Duration::from_secs(5));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1], Err("helper_failed".to_string()));

    let long = format!(
        "{{\"version\":1,\"event\":\"{}\"}}\n",
        "a".repeat(MAX_WATCH_LINE)
    );
    let helper = streaming_helper(&long, true);
    let mut watch = client_of(&helper).watch(&helper.path(), "watch").unwrap();
    let lines = lines_of(&mut watch, Duration::from_secs(5));
    assert_eq!(lines, [Err("response_too_large".to_string())]);

    let gone = std::env::temp_dir().join("hylki-helper-that-is-not-there");
    let err = client_of(&helper).watch(&gone, "watch").unwrap_err();
    assert_eq!(err.code, "helper_missing");
}

#[test]
fn a_streaming_helper_that_will_not_leave_is_stopped() {
    let helper = stubborn_helper();
    let watch = client_of(&helper).watch(&helper.path(), "watch").unwrap();
    let started = Instant::now();
    drop(watch);
    let took = started.elapsed();
    assert!(took >= WATCH_EXIT_GRACE, "{took:?}");
    assert!(took < WATCH_EXIT_GRACE + Duration::from_secs(5), "{took:?}");
}
