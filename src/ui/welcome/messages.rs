//! The wizard's messages: its pages' input, what it hands the app, and the
//! results of its connection tests and sign-ins.

use super::*;

/// The settings chosen on the privacy + personalize pages, applied by the app
/// through its normal Set* handlers when the wizard finishes.
#[derive(Debug, Clone)]
pub(crate) struct WelcomePrefs {
    pub(crate) block_remote: bool,
    pub(crate) gravatar: bool,
    pub(crate) sender_logos: bool,
    pub(crate) notification_content: bool,
    pub(crate) preview_lines: u32,
    pub(crate) avatars: bool,
    pub(crate) threading: bool,
    /// The app icon picked from the gallery (an `app_icon::catalog` id), or
    /// empty when nothing was picked over an icon set outside Hylki.
    pub(crate) app_icon: String,
}

#[derive(Debug)]
pub(crate) enum WelcomeInput {
    Next,
    Back,
    ProviderChanged,
    TestAndAdd,
    /// Open Proton's human verification for the challenge the last
    /// sign-in came back with.
    VerifyHuman,
    /// The challenge was solved: sign in again with its token.
    HumanVerified(proton_verify::Verified),
    ImportGoa(usize),
    RescanGoa,
    OpenOnlineAccounts,
    /// The page or the manual form changed: relabel the footer button.
    RefreshFooter,
    Finish,
    /// The hero page's language drop-down moved to choice `n`.
    LanguageChanged(u32),
}

#[derive(Debug)]
pub(crate) enum WelcomeOutput {
    /// A manual account passed its connection test, or a custom OAuth one
    /// signed in: save + connect it.
    AddAccount(Box<AccountConfig>),
    /// A GNOME Online Accounts import was chosen.
    ImportGoa(Box<AccountConfig>),
    /// The chosen privacy/personalize settings (sent once, on finish).
    Prefs(WelcomePrefs),
    /// The wizard is done (window already closing).
    Done,
    /// A language was picked on the first page: its locale code, "" for
    /// the system's. The app saves it and comes back in that language.
    Language(String),
}

#[derive(Debug)]
pub(crate) enum WelcomeCmd {
    Tested {
        account: Box<AccountConfig>,
        result: ConnTest,
        seq: u32,
    },
    /// A Tuta or Proton sign-in through the helper ended: the session it
    /// opened, which the account keeps in place of the password, or why
    /// not.
    HelperSignedIn {
        account: Box<AccountConfig>,
        result: Result<String, worker::HelperError>,
        seq: u32,
    },
    /// The browser sign-in for a custom OAuth account ended: its refresh
    /// token, or why not.
    SignedIn {
        account: Box<AccountConfig>,
        /// The refresh token, and who signed in when the provider says.
        result: Result<(String, Option<(String, String)>), String>,
        seq: u32,
    },
}
