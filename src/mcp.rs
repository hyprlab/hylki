//! The assistant connection: a Model Context Protocol server inside the
//! running app, through which an AI assistant reads the mail, looks up
//! contacts and prepares messages for the user to send.
//!
//! The server is on only while Settings says so ([`Server`]), and only as
//! long as the app runs: it listens on a socket in the user's runtime
//! directory ([`socket_path`]), which no other user can open, and the
//! connecting process is checked to be the user's own as well. It reads
//! with the app's own code (the cache, the contact lookup), drafts through
//! the app's own composer and changes mail through the app's own actions, so
//! there is never a second program reading the database a different version
//! of Hylki laid out.
//!
//! What an assistant may reach is [`crate::config::AssistantPolicy`], checked
//! at every call, and nothing until the user allows it. No setting allows
//! sending, erasing mail for good, or encrypted mail: there is no tool for
//! the first two, and the tools get mail in a way that cannot decrypt.
//!
//! An assistant's client starts `hylki --mcp` ([`run_relay`]), an ordinary
//! stdio server to it, which only carries bytes between the client and that
//! socket; with nothing listening, it says so to the assistant.

use std::path::PathBuf;

mod activity;
mod content;
mod draft;
mod organize;
mod pdf;
mod relay;
mod server;
mod tools;

pub(crate) use activity::Activity;
pub(crate) use draft::{text_html, Draft, DraftKind, Original};
pub(crate) use organize::{Change, Organized, Target};
pub(crate) use relay::run as run_relay;
pub(crate) use server::{remove_socket, AppCall, Server, SharedPolicy};

/// The command-line flag that runs the relay instead of the app.
pub(crate) const MCP_FLAG: &str = "--mcp";

/// The name the assistant's client knows the server by.
const SERVER_NAME: &str = if cfg!(feature = "beta") {
    "hylki-beta"
} else {
    "hylki"
};

/// The beta can run beside the stable app, and must not take its socket.
const SOCKET_NAME: &str = if cfg!(feature = "beta") {
    "mcp-beta.sock"
} else {
    "mcp.sock"
};

/// Where the running app listens: `$XDG_RUNTIME_DIR/hylki/mcp.sock`, which
/// only the user can reach. Inside the Flatpak sandbox the runtime
/// directory is made anew for each instance, and the relay is another
/// instance, so there it is the app's own directory under it, which every
/// instance of the app shares. `None` without a runtime directory.
pub(crate) fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)?;
    let dir = match std::env::var("FLATPAK_ID") {
        Ok(id) if !id.is_empty() => runtime.join("app").join(id),
        _ => runtime.join("hylki"),
    };
    Some(dir.join(SOCKET_NAME))
}

/// The command an assistant's client starts as a local (stdio) MCP server,
/// the way this copy of Hylki runs: through `flatpak run`, from the AppImage,
/// or as the installed program.
pub(crate) fn connect_command() -> String {
    format!("{} {MCP_FLAG}", relay_program())
}

fn relay_program() -> String {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        if !id.is_empty() {
            return format!("flatpak run {}", shell_word(&id));
        }
    }
    // An AppImage's program is unpacked somewhere new at each start.
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        return shell_word(&image.to_string_lossy());
    }
    let exe = std::env::current_exe().ok();
    let on_path = exe.as_ref().and_then(|exe| {
        let name = exe.file_name()?.to_str()?.to_string();
        let found = std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(&name))
            .find(|p| p.is_file())?;
        let same = std::fs::canonicalize(&found).ok()? == std::fs::canonicalize(exe).ok()?;
        same.then_some(name)
    });
    match (on_path, exe) {
        (Some(name), _) => shell_word(&name),
        (None, Some(exe)) => shell_word(&exe.to_string_lossy()),
        (None, None) => "hylki".to_string(),
    }
}

/// `word` as one word to a POSIX shell: as it is when nothing in it is
/// special, otherwise in single quotes.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::shell_word;

    #[test]
    fn a_path_is_quoted_only_when_a_shell_would_split_it() {
        assert_eq!(shell_word("/usr/bin/hylki"), "/usr/bin/hylki");
        assert_eq!(
            shell_word("/home/a/My Apps/Hylki.AppImage"),
            "'/home/a/My Apps/Hylki.AppImage'"
        );
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word(""), "''");
    }
}
