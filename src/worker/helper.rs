//! JSON over stdin/stdout with a bundled provider helper: a separate
//! program that signs in to a provider Hylki cannot reach over IMAP, SMTP or
//! JMAP (Tuta's `hylki-tuta`, Proton's `hylki-proton`), because only that
//! provider's own client library can decrypt its mail.
//!
//! One request per run: one UTF-8 JSON object goes to the helper on stdin,
//! one comes back on stdout, and stderr is discarded. Secrets travel on
//! stdin only, never in argv or the environment, and nothing the helper
//! prints reaches the log or the UI: every failure is a code from a closed
//! list, turned into text by [`HelperError::message`].
//!
//! The one exception is a streaming run ([`Watch`]): its request goes in the
//! same way, but stdin stays open for as long as the caller wants the run to
//! last, and the helper writes one JSON object per line until stdin closes.
//!
//! Nothing here is specific to one provider, so a second helper only needs
//! a [`HelperSpec`] of its own.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::config::{LiveSession, Protocol};
use crate::i18n::i18n;

/// The version every request carries and every reply must echo.
pub(super) const PROTOCOL_VERSION: u64 = 1;

/// How long one helper run may take, process start included. The helper
/// gives up on its own after 90 seconds; the key derivation at sign-in
/// alone takes a while.
pub(super) const HELPER_TIMEOUT: Duration = Duration::from_secs(100);

/// A JSON object, the only shape a request or a reply has.
pub(super) type Object = Map<String, Value>;

/// Sends one request and returns the helper's reply object: the seam the
/// tests replace. Production code uses [`process_transport`].
pub(super) type Transport = Arc<dyn Fn(&Value) -> Result<Object, HelperError> + Send + Sync>;

/// Where a helper lives and what it may answer.
#[derive(Debug, Clone, Copy)]
pub(super) struct HelperSpec {
    /// The executable's file name.
    pub(super) name: &'static str,
    /// The codes passed through to the caller; anything else reads as
    /// `request_failed`, so a helper can never put words in the UI.
    pub(super) codes: &'static [&'static str],
    /// A reply larger than this is refused before it is parsed.
    pub(super) max_response_bytes: usize,
    /// How long one run may take before it is killed.
    pub(super) timeout: Duration,
    /// Whether the helper hands back a new session with its replies
    /// (Proton's refresh tokens are spent on use): such an account's
    /// session lives in [`crate::config::live_session`], where every call
    /// waits its turn and the newest copy is kept.
    pub(super) rotates_sessions: bool,
}

/// A human-verification challenge (a CAPTCHA) a sign-in was refused with:
/// opaque values the helper copied from the server, solved by the user in
/// a web view and handed back on the retry.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HvChallenge {
    pub(crate) token: String,
    /// The verification methods offered (`captcha`, `email`, ...).
    pub(crate) methods: Vec<String>,
}

impl std::fmt::Debug for HvChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HvChallenge")
            .field("methods", &self.methods)
            .finish_non_exhaustive()
    }
}

/// A helper request that failed, as a fixed code, and for a sign-in the
/// server wants a person behind, its challenge. Its Debug output never
/// holds a secret or server text.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HelperError {
    pub(crate) code: String,
    hv: Option<HvChallenge>,
}

impl std::fmt::Debug for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("HelperError");
        out.field("code", &self.code);
        if let Some(hv) = &self.hv {
            out.field("hv", hv);
        }
        out.finish()
    }
}

impl HelperError {
    pub(crate) fn new(code: &str) -> HelperError {
        HelperError {
            code: code.to_string(),
            hv: None,
        }
    }

    /// A `human_verification_required` refusal with `challenge`, as the
    /// helper's reply would make it.
    #[cfg(test)]
    pub(crate) fn with_challenge(challenge: HvChallenge) -> HelperError {
        HelperError {
            code: "human_verification_required".to_string(),
            hv: Some(challenge),
        }
    }

    /// The sign-in needs the code from the account's authenticator app.
    pub(crate) fn needs_second_factor(&self) -> bool {
        self.code == "second_factor_required"
    }

    /// The sign-in needs the account's separate mailbox password (a Proton
    /// account in two-password mode).
    pub(crate) fn needs_mailbox_password(&self) -> bool {
        self.code == "mailbox_password_required"
    }

    /// The server wants a CAPTCHA solved before it lets the sign-in go on;
    /// [`HelperError::hv_challenge`] has it when the helper could read it.
    /// A `human_verification_failed` refusal carries none: signing in again
    /// without the spent token brings a fresh one.
    pub(crate) fn needs_human_verification(&self) -> bool {
        self.code == "human_verification_required"
    }

    /// The challenge of a `human_verification_required` refusal.
    pub(crate) fn hv_challenge(&self) -> Option<HvChallenge> {
        self.hv.clone()
    }

    /// What the user is told, fixed per code: the helper's own words never
    /// reach the screen.
    pub(crate) fn message(&self) -> String {
        match self.code.as_str() {
            "helper_missing" => i18n(
                "The helper program for this account is not installed. Reinstall Hylki.",
            ),
            "timeout" => i18n("The server did not answer in time."),
            "transport_failed" => i18n("Could not reach the server."),
            "login_failed" => i18n("Sign-in failed. Check the address and the password."),
            "second_factor_required" => {
                i18n("Enter the six-digit code from your authenticator app.")
            }
            "invalid_totp" => i18n("The two-step verification code must be six digits."),
            "second_factor_failed" => i18n("The two-step verification code was not accepted."),
            "mailbox_password_required" => i18n("Enter the account's mailbox password."),
            "mailbox_password_failed" => i18n("The mailbox password was not accepted."),
            "human_verification_required" => {
                i18n("The server wants to confirm that a person is signing in.")
            }
            "human_verification_failed" => {
                i18n("The verification was not accepted. Sign in again to get a new one.")
            }
            "unsupported_u2f" | "unsupported_webauthn" => i18n(
                "This account signs in with a security key, which Hylki does not support yet. Add an authenticator app to the account.",
            ),
            "unsupported_second_factor" => i18n(
                "This account's two-step verification is not supported yet. Add an authenticator app to the account.",
            ),
            "not_signed_in" => i18n(
                "This account is not signed in. Sign in again in the account's settings.",
            ),
            "session_revoked" => i18n(
                "The server ended this sign-in. Sign in again in the account's settings.",
            ),
            "session_invalid" => i18n(
                "The stored sign-in is damaged. Sign in again in the account's settings.",
            ),
            "client_outdated" => i18n("The server no longer accepts this version. Update Hylki."),
            "revoke_failed" => i18n("Could not end the sign-in on the server."),
            "folder_not_found" => i18n("The folder is no longer on the server."),
            "folder_failed" => i18n("The server did not make the change to the folder."),
            "folder_exists" => i18n("There is already a folder by that name there."),
            "invalid_parent" => i18n("A folder can't be moved into itself."),
            "not_custom_folder" => i18n("Tuta's own folders can't be changed."),
            "limit_reached" => i18n("The account has reached Tuta's limit for that."),
            "label_exists" => i18n("There is already a tag by that name."),
            "not_a_draft" => i18n("The message is not a draft."),
            "invalid_name" => i18n("That name can't be used for a folder or a tag."),
            "invalid_color" => i18n("That color can't be used for a tag."),
            "label_not_found" => i18n("The tag is no longer on the server."),
            "label_failed" => i18n("The server did not make the change to the tag."),
            "draft_not_found" => i18n("The draft is no longer on the server."),
            "draft_failed" => i18n("The server did not store the draft."),
            "not_trash_or_spam" => {
                i18n("Mail can only be deleted for good from the Trash or Spam.")
            }
            "delete_failed" => i18n("The server did not delete the mail."),
            "operation_still_active" => {
                i18n("The server is still busy with an earlier change. Try again in a moment.")
            }
            "import_invalid" => i18n("The message could not be read, so it was not stored."),
            "import_too_large" => i18n("The message is too large for Tuta to take in."),
            "import_refused" => i18n(
                "Tuta does not let this account import mail. Importing needs a Legend or Unlimited plan.",
            ),
            "import_failed" => i18n("The server did not store the message."),
            "invalid_destination" => i18n("Mail can't be moved into that folder."),
            "message_gone" => i18n("The message is no longer on the server."),
            "decryption_failed" => i18n("The message could not be decrypted."),
            "unsupported_legacy_body" => {
                i18n("This message is stored in an older format Hylki cannot read yet.")
            }
            "attachment_failed" => i18n("An attachment could not be read or stored."),
            "invalid_recipient" => i18n("A recipient's address is not valid."),
            "sender_unknown" => i18n("The From address does not belong to this account."),
            "body_too_large" => i18n("The message text is too long to send."),
            "recipient_not_found" => i18n(
                "A recipient's Tuta address was not found, so the message could not be encrypted for it. Check the address.",
            ),
            "secure_external_unsupported" => i18n(
                "End-to-end encrypted delivery reaches Tuta addresses only, and a recipient is outside Tuta.",
            ),
            "ordinary_delivery_requires_consent" => i18n(
                "Mail to an address outside Tuta is not end-to-end encrypted, and sending it that way was not confirmed.",
            ),
            "message_too_large" => i18n("The message is too large to send."),
            "confidential_send_unsupported" => i18n(
                "A recipient has no key for end-to-end encryption, so the message can't be sent encrypted.",
            ),
            "send_failed" => i18n("The server refused the message."),
            "send_uncertain" => {
                i18n("The message may have been sent. Check Sent before trying again.")
            }
            "unsupported_operation" => i18n("This account can't do that yet."),
            _ => i18n("The helper program for this account failed. Try again later."),
        }
    }
}

/// The executable `name` resolves to: beside Hylki's own (a package or an
/// AppImage ships it there), then on `PATH` (the Flatpak's `/app/bin`).
fn locate(name: &str) -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|candidate| is_executable(candidate))
        .or_else(|| find_in(name, std::env::var_os("PATH").as_deref()))
}

fn find_in(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The helper `spec` names, found by [`locate`].
pub(super) fn locate_helper(spec: HelperSpec) -> Result<PathBuf, HelperError> {
    locate(spec.name).ok_or_else(|| HelperError::new("helper_missing"))
}

/// Runs the helper `spec` names, found by [`locate`], once per request.
pub(super) fn process_transport(spec: HelperSpec) -> Transport {
    Arc::new(move |request| run_helper(&locate_helper(spec)?, request, spec))
}

/// One helper run: fixed argv (no shell), the request on stdin, one JSON
/// object back on stdout. Blocking; call it from a blocking task.
pub(super) fn run_helper(
    executable: &Path,
    request: &Value,
    spec: HelperSpec,
) -> Result<Object, HelperError> {
    let fail = HelperError::new;
    // The request may hold a password or a session. Hylki has no zeroizing
    // buffer type (its passwords are plain Strings too), so the bytes are
    // only kept as long as the write to the pipe takes.
    let input = serde_json::to_vec(request).map_err(|_| fail("invalid_request"))?;
    let mut child = Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| fail("helper_missing"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| fail("helper_failed"))?;
    let writer = std::thread::spawn(move || {
        // A helper that exits without reading is caught by its exit status.
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().ok_or_else(|| fail("helper_failed"))?;
    let cap = spec.max_response_bytes;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        // One byte over the cap is enough to know it is too large; the pipe
        // is then closed, which ends a helper still writing.
        (&mut stdout)
            .take(cap as u64 + 1)
            .read_to_end(&mut output)
            .map(|_| output)
    });
    let deadline = Instant::now() + spec.timeout;
    let mut reader = Some(reader);
    let mut output: Option<Vec<u8>> = None;
    let status = loop {
        // The reply read in full while the helper still runs: one over the
        // cap stops it now rather than at the deadline.
        if reader.as_ref().is_some_and(|r| r.is_finished()) {
            let read = reader
                .take()
                .map(|r| r.join())
                .ok_or_else(|| fail("helper_failed"))?
                .map_err(|_| fail("helper_failed"))?
                .map_err(|_| fail("helper_failed"))?;
            if read.len() > cap {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail("response_too_large"));
            }
            output = Some(read);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                tracing::warn!("helper {} timed out and was stopped", spec.name);
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail("timeout"));
            }
        }
    };
    let _ = writer.join();
    let output = match (output, reader) {
        (Some(output), _) => output,
        (None, Some(reader)) => reader
            .join()
            .map_err(|_| fail("helper_failed"))?
            .map_err(|_| fail("helper_failed"))?,
        (None, None) => return Err(fail("helper_failed")),
    };
    // Before the exit status: a helper cut off mid-reply exits uncleanly,
    // and the size is the reason.
    if output.len() > cap {
        return Err(fail("response_too_large"));
    }
    if !status.success() {
        tracing::warn!("helper {} exited uncleanly", spec.name);
        return Err(fail("helper_failed"));
    }
    match serde_json::from_slice(&output) {
        Ok(Value::Object(reply)) => Ok(reply),
        _ => Err(fail("invalid_response")),
    }
}

/// A request's envelope: `parameters` beside `version` and `op`.
fn envelope(operation: &str, parameters: Object) -> Value {
    let mut envelope = Object::new();
    envelope.insert("version".into(), PROTOCOL_VERSION.into());
    envelope.insert("op".into(), operation.into());
    envelope.extend(parameters);
    Value::Object(envelope)
}

/// A reply's version, and its error, as a code from `spec`'s list, with
/// the challenge of a human-verification refusal.
fn check_reply(reply: &Object, spec: HelperSpec) -> Result<(), HelperError> {
    if reply.get("version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err(HelperError::new("unsupported_version"));
    }
    if let Some(detail) = reply.get("error") {
        let code = match detail.get("code") {
            Some(Value::String(code)) if spec.codes.contains(&code.as_str()) => code.as_str(),
            _ => "request_failed",
        };
        let mut error = HelperError::new(code);
        if code == "human_verification_required" {
            error.hv = hv_challenge(detail.get("hv"));
        }
        return Err(error);
    }
    Ok(())
}

/// One versioned request: `parameters` go beside `version` and `op`, and
/// the answer is the reply's `result` object or its error code.
pub(super) fn request(
    transport: &Transport,
    spec: HelperSpec,
    operation: &str,
    parameters: Object,
) -> Result<Object, HelperError> {
    exchange(transport, spec, operation, parameters).0
}

/// [`request`], with the session the reply handed back beside the answer:
/// `result.session` or, for a failure after the helper resumed the
/// session, `error.session`. A helper whose sessions rotate needs it kept
/// whatever the answer.
fn exchange(
    transport: &Transport,
    spec: HelperSpec,
    operation: &str,
    parameters: Object,
) -> (Result<Object, HelperError>, Option<Value>) {
    let mut reply = match transport(&envelope(operation, parameters)) {
        Ok(reply) => reply,
        Err(e) => return (Err(e), None),
    };
    if let Err(error) = check_reply(&reply, spec) {
        // A reply of another protocol version hands back nothing to trust.
        let session = match error.code.as_str() {
            "unsupported_version" => None,
            _ => reply.get("error").and_then(session_of),
        };
        return (Err(error), session);
    }
    match reply.remove("result") {
        Some(Value::Object(result)) => {
            let session = result.get("session").filter(|s| s.is_object()).cloned();
            (Ok(result), session)
        }
        _ => (Err(HelperError::new("invalid_response")), None),
    }
}

/// The longest line a streaming run may print. Its lines are small events;
/// a longer one is a helper gone wrong, and is not read into memory.
const MAX_WATCH_LINE: usize = 64 * 1024;

/// How long a streaming helper is given to leave once its stdin closes,
/// before it is killed.
const WATCH_EXIT_GRACE: Duration = Duration::from_secs(2);

/// A streaming helper run, such as Tuta's `watch`: the request goes in on
/// stdin as for any other, but stdin is held open, which is what keeps the
/// helper going, and every line it prints is one JSON object carrying the
/// version, checked like a reply. Dropping it closes stdin, gives the helper
/// [`WATCH_EXIT_GRACE`] to leave and kills it if it has not.
pub(super) struct Watch {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: std::sync::mpsc::Receiver<Result<Object, HelperError>>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

impl Watch {
    /// Start `executable` with `request` on its stdin. Blocking only for
    /// as long as the request takes to write.
    pub(super) fn start(
        executable: &Path,
        request: &Value,
        spec: HelperSpec,
    ) -> Result<Watch, HelperError> {
        let fail = HelperError::new;
        let mut input = serde_json::to_vec(request).map_err(|_| fail("invalid_request"))?;
        input.push(b'\n');
        let mut child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| fail("helper_missing"))?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail("helper_failed"));
        };
        let (tx, lines) = std::sync::mpsc::channel();
        let reader = std::thread::Builder::new()
            .name(format!("{}-watch", spec.name))
            .spawn(move || read_watch_lines(stdout, spec, &tx));
        // A helper that exits without reading shows as the end of its
        // output, which `next` reports.
        let written = stdin.write_all(&input).and_then(|()| stdin.flush());
        if reader.is_err() || written.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail("helper_failed"));
        }
        Ok(Watch {
            child,
            stdin: Some(stdin),
            lines,
        })
    }

    /// The helper's next line, waiting at most `wait`: `None` when it said
    /// nothing meanwhile. An error when it has ended, printed something
    /// other than a line of the protocol, or reported a failure, whose code
    /// it then is; nothing more is read after one.
    pub(super) fn next(&mut self, wait: Duration) -> Result<Option<Object>, HelperError> {
        use std::sync::mpsc::RecvTimeoutError;
        match self.lines.recv_timeout(wait) {
            Ok(line) => line.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(HelperError::new("helper_failed")),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + WATCH_EXIT_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read a streaming helper's lines into `tx` until it ends, a line is not
/// one of the protocol, or the receiver has gone.
fn read_watch_lines(
    stdout: std::process::ChildStdout,
    spec: HelperSpec,
    tx: &std::sync::mpsc::Sender<Result<Object, HelperError>>,
) {
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        let read = (&mut reader)
            .take(MAX_WATCH_LINE as u64 + 1)
            .read_until(b'\n', &mut line);
        let parsed = match read {
            Ok(0) => return,
            Err(_) => Err(HelperError::new("helper_failed")),
            Ok(_) if line.len() > MAX_WATCH_LINE => Err(HelperError::new("response_too_large")),
            Ok(_) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(_) => match serde_json::from_slice(&line) {
                Ok(Value::Object(object)) => check_reply(&object, spec).map(|()| object),
                _ => Err(HelperError::new("invalid_response")),
            },
        };
        let last = parsed.is_err();
        if tx.send(parsed).is_err() || last {
            return;
        }
    }
}

/// The session an error reply handed back, when it carries one.
fn session_of(detail: &Value) -> Option<Value> {
    detail.get("session").filter(|s| s.is_object()).cloned()
}

/// A challenge as the helper relays it, or `None` when it is not one: a
/// token, and at least one method to offer.
fn hv_challenge(hv: Option<&Value>) -> Option<HvChallenge> {
    let hv = hv?;
    let token = hv.get("token")?.as_str().filter(|t| !t.is_empty())?;
    let methods: Vec<String> = hv
        .get("methods")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect();
    if methods.is_empty() {
        return None;
    }
    Some(HvChallenge {
        token: token.to_string(),
        methods,
    })
}

/// Whether a session object names `email` as its login. A session for
/// another address is never used: the keyring entry would be crossed.
pub(super) fn session_login_matches(session: &Value, email: &str) -> bool {
    match session.get("login") {
        Some(Value::String(login)) => login.to_lowercase() == email.to_lowercase(),
        _ => false,
    }
}

/// What a sign-in sends: the password and, when the server asked for
/// them, the authenticator code, a two-password account's mailbox
/// password, and a solved human-verification challenge. A field left
/// `None` is not sent, so a helper that knows nothing of it (Tuta's) never
/// sees it. Its Debug output names no value.
#[derive(Clone, Default)]
pub(crate) struct HelperLogin {
    pub(crate) email: String,
    pub(crate) password: String,
    pub(crate) totp: Option<String>,
    pub(crate) mailbox_password: Option<String>,
    pub(crate) hv_token: Option<String>,
    pub(crate) hv_type: Option<String>,
}

impl std::fmt::Debug for HelperLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperLogin")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Sign in and return the resumable session, as the JSON to keep in the
/// keyring. The password goes to the helper once and is never kept.
pub(super) fn login(
    transport: &Transport,
    spec: HelperSpec,
    sign_in: &HelperLogin,
) -> Result<String, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("email".into(), sign_in.email.as_str().into());
    parameters.insert("password".into(), sign_in.password.as_str().into());
    parameters.insert(
        "totp".into(),
        sign_in.totp.as_deref().map_or(Value::Null, Value::from),
    );
    for (key, value) in [
        ("mailbox_password", &sign_in.mailbox_password),
        ("hv_token", &sign_in.hv_token),
        ("hv_type", &sign_in.hv_type),
    ] {
        if let Some(value) = value {
            parameters.insert(key.into(), value.as_str().into());
        }
    }
    let result = request(transport, spec, "login", parameters)?;
    match result.get("session") {
        Some(session) if session_login_matches(session, &sign_in.email) => Ok(session.to_string()),
        _ => Err(HelperError::new("invalid_response")),
    }
}

/// A signed-in account's way to its helper: every request resumes the
/// account's session, so a session the server ended reads as
/// `session_revoked`, apart from an outage.
#[derive(Clone)]
pub(super) struct HelperClient {
    spec: HelperSpec,
    transport: Transport,
    email: String,
    /// The session JSON, for as long as the worker runs, the way the other
    /// accounts' passwords are held: Hylki has no secret-holding type. Each
    /// request parses it into a value dropped with the request, and a
    /// session the reply hands back replaces it here.
    live: Arc<LiveSession>,
}

impl std::fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperClient")
            .field("helper", &self.spec.name)
            .finish_non_exhaustive()
    }
}

/// A session JSON that may be used for `email`: a damaged or crossed
/// keyring entry means signing in again.
fn check_session(session: &str, email: &str) -> Result<(), HelperError> {
    if session.is_empty() {
        return Err(HelperError::new("not_signed_in"));
    }
    let parsed: Value =
        serde_json::from_str(session).map_err(|_| HelperError::new("session_invalid"))?;
    if !session_login_matches(&parsed, email) {
        return Err(HelperError::new("session_invalid"));
    }
    Ok(())
}

impl HelperClient {
    /// A client for `email`'s stored `session`, checked before any request.
    /// The session is the client's own: for a helper whose sessions do not
    /// change.
    pub(super) fn new(
        spec: HelperSpec,
        transport: Transport,
        email: &str,
        session: &str,
    ) -> Result<HelperClient, HelperError> {
        check_session(session, email)?;
        let live = LiveSession::default();
        live.set_latest(session);
        Ok(HelperClient {
            spec,
            transport,
            email: email.to_string(),
            live: Arc::new(live),
        })
    }

    /// A client on `live`, the session every user of the account shares.
    /// `stored` (the keyring's copy) seeds it when this run has none yet;
    /// otherwise the newer live one is used, and checked like a stored one.
    pub(super) fn shared(
        spec: HelperSpec,
        transport: Transport,
        email: &str,
        live: Arc<LiveSession>,
        stored: &str,
    ) -> Result<HelperClient, HelperError> {
        // No turn is taken: a call another thread is running must not hold
        // up a worker that only wants a client.
        if live.latest().is_empty() {
            check_session(stored, email)?;
        }
        check_session(&live.seed(stored), email)?;
        Ok(HelperClient {
            spec,
            transport,
            email: email.to_string(),
            live,
        })
    }

    /// The session as it now stands, rotated by every reply that handed one
    /// back.
    pub(super) fn session(&self) -> String {
        self.live.latest()
    }

    /// One operation on the session. The call holds the account's turn
    /// from reading the session to keeping the one the reply handed back,
    /// failure or not, so no other call ever presents a refresh token this
    /// one spent.
    pub(super) fn request(
        &self,
        operation: &str,
        mut parameters: Object,
    ) -> Result<Object, HelperError> {
        let _turn = self.live.turn();
        let session: Value = serde_json::from_str(&self.live.latest())
            .map_err(|_| HelperError::new("session_invalid"))?;
        parameters.insert("session".into(), session.clone());
        let (answer, handed_back) = exchange(&self.transport, self.spec, operation, parameters);
        if let Some(rotated) = handed_back {
            // A session for another address would cross the keyring entry.
            if rotated != session && session_login_matches(&rotated, &self.email) {
                self.live.set_latest(&rotated.to_string());
            }
        }
        answer
    }

    /// Start a streaming `operation` on the session as it stands, with the
    /// helper at `executable`. Its lines hand no session back, so it is for
    /// a helper whose sessions do not rotate.
    pub(super) fn watch(
        &self,
        executable: &Path,
        operation: &str,
    ) -> Result<Watch, HelperError> {
        let session: Value = serde_json::from_str(&self.live.latest())
            .map_err(|_| HelperError::new("session_invalid"))?;
        let mut parameters = Object::new();
        parameters.insert("session".into(), session);
        Watch::start(executable, &envelope(operation, parameters), self.spec)
    }
}

/// The helper behind a protocol, for the protocols that have one.
fn spec_for(protocol: Protocol) -> Option<HelperSpec> {
    match protocol {
        Protocol::Tuta => Some(super::tuta::SPEC),
        Protocol::Proton => Some(super::proton::SPEC),
        _ => None,
    }
}

/// Sign in to a helper-backed account from the account editor or the
/// wizard: the session JSON to store, or why not. Blocking (the helper's
/// key derivation takes seconds); call it from a blocking task.
///
/// A rotating session becomes the account's live one at once, after any
/// call still running on the old one: from then on it is the one every
/// call and every keyring write uses.
pub(crate) fn helper_login_blocking(
    protocol: Protocol,
    sign_in: &HelperLogin,
) -> Result<String, HelperError> {
    let spec = spec_for(protocol).ok_or_else(|| HelperError::new("unsupported_operation"))?;
    let session = login(&process_transport(spec), spec, sign_in)?;
    if spec.rotates_sessions {
        let live = crate::config::live_session(&sign_in.email);
        let _turn = live.turn();
        live.set_latest(&session);
    }
    Ok(session)
}

/// End a removed account's session on the server. Blocking, and best
/// effort: the keyring entry goes either way, and a session left open
/// expires on its own. A rotating session is ended as this run last saw
/// it, and then forgotten, so no later write puts it back in the keyring.
pub(crate) fn helper_revoke_blocking(protocol: Protocol, email: &str, session: &str) {
    let Some(spec) = spec_for(protocol) else {
        return;
    };
    let transport = process_transport(spec);
    let client = if spec.rotates_sessions {
        HelperClient::shared(
            spec,
            transport,
            email,
            crate::config::live_session(email),
            session,
        )
    } else {
        HelperClient::new(spec, transport, email, session)
    };
    let revoked = client.and_then(|client| client.request("revoke", Object::new()));
    if spec.rotates_sessions {
        crate::config::forget_live_session(email);
    }
    if let Err(e) = revoked {
        tracing::warn!(
            "could not end the session of the removed account {email}: {}",
            e.code
        );
    }
}

#[cfg(test)]
pub(super) mod test_support {
    //! Fake helpers: shell scripts standing in for a helper binary.

    use std::path::PathBuf;

    use serde_json::Value;

    /// A script in a directory of its own, removed when dropped.
    pub(in crate::worker) struct FakeHelper {
        dir: PathBuf,
    }

    impl FakeHelper {
        pub(in crate::worker) fn path(&self) -> PathBuf {
            self.dir.join("helper")
        }

        /// What the helper read on stdin.
        pub(in crate::worker) fn stdin(&self) -> Value {
            let text = std::fs::read(self.dir.join("stdin")).expect("stdin recorded");
            serde_json::from_slice(&text).expect("stdin is json")
        }

        /// How many command-line arguments the helper got.
        pub(in crate::worker) fn argc(&self) -> usize {
            let text = std::fs::read_to_string(self.dir.join("argc")).expect("argc recorded");
            text.trim().parse().expect("argc is a number")
        }

        /// Whether the helper saw its stdin close and left on its own.
        pub(in crate::worker) fn ended(&self) -> bool {
            self.dir.join("ended").exists()
        }
    }

    impl Drop for FakeHelper {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn script(body: &str, reply: &str) -> FakeHelper {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hylki-helper-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("helper dir");
        std::fs::write(dir.join("reply"), reply).expect("reply");
        let path = dir.join("helper");
        std::fs::write(&path, format!("#!/bin/sh\ndir=$(dirname \"$0\")\n{body}")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        FakeHelper { dir }
    }

    /// Records its argc and stdin, prints `stdout`, exits with `status`.
    pub(in crate::worker) fn fake_helper(stdout: &str, status: i32) -> FakeHelper {
        script(
            &format!(
                "echo $# > \"$dir/argc\"\ncat > \"$dir/stdin\"\ncat \"$dir/reply\"\nexit {status}\n"
            ),
            stdout,
        )
    }

    /// A streaming helper: records its argc and the request line, prints
    /// `stdout`, then, when `stays`, keeps going until its stdin closes and
    /// notes that it did; otherwise it exits at once.
    pub(in crate::worker) fn streaming_helper(stdout: &str, stays: bool) -> FakeHelper {
        let rest = if stays {
            "cat > /dev/null\necho ended > \"$dir/ended\"\n"
        } else {
            ""
        };
        script(
            &format!(
                "echo $# > \"$dir/argc\"\nhead -n 1 > \"$dir/stdin\"\ncat \"$dir/reply\"\n{rest}"
            ),
            stdout,
        )
    }

    /// Reads its request, then pays no attention to its stdin closing.
    pub(in crate::worker) fn stubborn_helper() -> FakeHelper {
        script("head -n 1 > /dev/null\nexec sleep 30\n", "")
    }

    /// Prints a secret to stderr and never answers.
    pub(in crate::worker) fn hanging_helper() -> FakeHelper {
        script("cat > /dev/null\necho secret-token >&2\nexec sleep 5\n", "")
    }

    /// Writes `bytes` bytes of reply and waits to be stopped.
    pub(in crate::worker) fn flooding_helper(bytes: usize) -> FakeHelper {
        script(
            &format!("cat > /dev/null\nhead -c {bytes} /dev/zero | tr '\\0' 'a'\nexec sleep 5\n"),
            "",
        )
    }
}

pub(super) mod send;

#[cfg(test)]
mod tests;
