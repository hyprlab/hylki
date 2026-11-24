//! A card's face for a sender who is not one of your own mailboxes (#560):
//! their contact photo, then their Gravatar and their domain's icon while
//! those Privacy switches are on. The chain and the caches are the message
//! list's, so a face the list has found is already here. One known when the
//! document is built goes in with it; one that has to be looked up is patched
//! into the cards when it arrives, rather than loading the page again.

use super::types::MessageViewInput;
use super::MessageView;
use crate::services::avatar::{self, CacheLookup, FetchMode, FetchOutcome};
use crate::services::logo;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

/// A card circle's size in CSS pixels, as `wrapper_css` draws it.
const SIZE: i32 = 26;

thread_local! {
    /// The Gravatar and sender-logo switches. Held here rather than by each
    /// reader, so the pop-out window's follows them too.
    static TIERS: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    /// Faces already scaled for a card, by address, with the texture each
    /// was made from: a photo changed in Contacts is scaled afresh.
    static URIS: RefCell<HashMap<String, (gtk::gdk::Texture, String)>> =
        RefCell::new(HashMap::new());
}

/// Hand the readers the Privacy switches for the tiers that ask the network.
pub(crate) fn set_face_tiers(gravatar: bool, logos: bool) {
    TIERS.with(|t| t.set((gravatar, logos)));
}

/// The Gravatar and sender-logo switches, in that order.
pub(super) fn tiers() -> (bool, bool) {
    TIERS.with(Cell::get)
}

fn key(email: &str) -> String {
    email.trim().to_lowercase()
}

/// The face already known for `email`, by the switches on now: its contact
/// photo, else its Gravatar, else its domain's icon. `None` when there is
/// none, and when it has not been looked up yet. No I/O.
pub(crate) fn known_texture(email: &str) -> Option<gtk::gdk::Texture> {
    if email.trim().is_empty() {
        return None;
    }
    let (gravatar, logos) = tiers();
    match avatar::lookup(email, gravatar) {
        CacheLookup::Texture(texture) => Some(texture),
        CacheLookup::Missing if logos => logo::cached(email),
        CacheLookup::Missing | CacheLookup::Fetch { .. } => None,
    }
}

/// The face already known for `email`, as a `data:` PNG for a card. `None`
/// when there is none, and when it has not been looked up yet.
pub(super) fn known_face(email: &str) -> Option<String> {
    let texture = known_texture(email)?;
    let key = key(email);
    let hit = URIS.with(|u| {
        u.borrow()
            .get(&key)
            .filter(|(made_from, _)| *made_from == texture)
            .map(|(_, uri)| uri.clone())
    });
    if hit.is_some() {
        return hit;
    }
    let uri = crate::ui::initials::texture_data_uri(&texture, SIZE)?;
    URIS.with(|u| u.borrow_mut().insert(key, (texture, uri.clone())));
    Some(uri)
}

/// The tiers still to ask about one address's face.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FaceAsk {
    /// The contact and Gravatar tiers, with the EDS generation and mode.
    avatar: Option<(u64, FetchMode)>,
    /// The domain icon.
    logo: bool,
}

impl FaceAsk {
    /// What was asked about `email`, so the same question is not put twice.
    fn tag(&self, email: &str) -> String {
        match self.avatar {
            Some((generation, mode)) => format!("{email}\u{1}{generation}\u{1}{mode:?}"),
            None => format!("{email}\u{1}logo"),
        }
    }
}

/// What is left to ask about `email`'s face, or `None` when its face is
/// known or nothing more can be learned.
pub(crate) fn still_to_ask(email: &str) -> Option<FaceAsk> {
    let (gravatar, logos) = tiers();
    match avatar::lookup(email, gravatar) {
        CacheLookup::Texture(_) => None,
        CacheLookup::Fetch { generation, mode } => Some(FaceAsk {
            avatar: Some((generation, mode)),
            logo: logos && !logo::known_missing(email),
        }),
        CacheLookup::Missing => {
            let wanted = logos
                && match logo::cached(email) {
                    // A week-old icon still shows; this is the cue to look
                    // for a fresh one, as the list does.
                    Some(_) => logo::wants_refresh(email),
                    None => !logo::known_missing(email),
                };
            wanted.then_some(FaceAsk {
                avatar: None,
                logo: true,
            })
        }
    }
}

/// Ask the tiers in `what` about `email`. Blocking: call off the main thread.
pub(crate) fn ask(email: String, what: FaceAsk) -> FaceAnswer {
    let avatar = what
        .avatar
        .map(|(generation, mode)| (generation, mode, avatar::fetch(&email, mode)));
    let personal = matches!(avatar, Some((_, _, FetchOutcome::Found(_))));
    let logo = (what.logo && !personal).then(|| logo::fetch(&email));
    FaceAnswer {
        email,
        avatar,
        logo,
    }
}

/// Put an answer in the caches the list and the readers share, and give back
/// the address it was for. Main thread only.
pub(crate) fn keep(answer: FaceAnswer) -> String {
    let FaceAnswer {
        email,
        avatar,
        logo,
    } = answer;
    if let Some((generation, mode, outcome)) = avatar {
        avatar::cache_result(&email, generation, mode, outcome);
    }
    match logo {
        Some(Some(bytes)) => {
            logo::decode_and_cache(&email, &bytes);
        }
        Some(None) => logo::remember_missing(&email),
        None => {}
    }
    email
}

/// What a face lookup off the main thread brought back.
#[derive(Debug)]
pub(crate) struct FaceAnswer {
    email: String,
    /// The contact and Gravatar tiers' answer, when they were asked, with
    /// the EDS generation and mode they were asked under.
    avatar: Option<(u64, FetchMode, FetchOutcome)>,
    /// The domain icon's answer, when it was asked: its bytes, or a miss.
    logo: Option<Option<Vec<u8>>>,
}

/// A reader's face lookups.
#[derive(Default)]
pub(super) struct Faces {
    /// Where an answer is sent, once the reader is built.
    pub(super) input: Option<relm4::Sender<MessageViewInput>>,
    /// The lookups already started, by address and what was asked, so each
    /// render does not start them again and a failure is not retried on
    /// every one.
    asked: HashSet<String>,
    /// Addresses whose face came in after the document was built. Patched
    /// at once when a page is up, and again into the next one to load, which
    /// may have been built before the answer.
    owed: HashSet<String>,
}

impl MessageView {
    /// Start looking up the faces of the senders on screen not known yet.
    pub(super) fn look_up_faces(&mut self) {
        let Some(input) = self.faces.input.clone() else {
            return;
        };
        let mut seen = HashSet::new();
        for m in &self.thread {
            let email = key(&m.face_addr());
            if email.is_empty() || !seen.insert(email.clone()) {
                continue;
            }
            // Your own mailbox wears the face its account chose (#189).
            if avatar::own_face(&email).is_some() {
                continue;
            }
            let Some(what) = still_to_ask(&email) else {
                continue;
            };
            if !self.faces.asked.insert(what.tag(&email)) {
                continue;
            }
            let input = input.clone();
            relm4::spawn(async move {
                let found = tokio::task::spawn_blocking(move || ask(email, what)).await;
                if let Ok(answer) = found {
                    let _ = input.send(MessageViewInput::FaceFound(Box::new(answer)));
                }
            });
        }
    }

    pub(super) fn on_face_found(&mut self, answer: FaceAnswer) {
        // Kept whatever is on screen now: the caches are the list's too.
        let email = keep(answer);
        // A tier left to ask (Gravatar once the contacts are read), or an
        // answer the address books changed under.
        self.look_up_faces();
        if known_face(&email).is_none() {
            return;
        }
        self.faces.owed.insert(email.clone());
        if self.webview_ready {
            self.patch_face(&email);
        }
    }

    /// Faces that arrived while the document was loading.
    pub(super) fn patch_owed_faces(&mut self) {
        for email in std::mem::take(&mut self.faces.owed) {
            self.patch_face(&email);
        }
    }

    /// Put `email`'s face on the circles of the cards it wrote.
    fn patch_face(&self, email: &str) {
        let Some(uri) = known_face(email) else {
            return;
        };
        let keys: Vec<String> = self
            .thread
            .iter()
            .filter(|m| key(&m.face_addr()) == email && avatar::own_face(email).is_none())
            .map(|m| format!("{}:{}", m.account_id, m.id))
            .collect();
        if keys.is_empty() {
            return;
        }
        // Replaced rather than given a new src: before a window existed the
        // circle was markup, not an image.
        self.run_js(&format!(
            "(function(u,ks){{ks.forEach(function(k){{\
             document.querySelectorAll('.hylki-ava[data-key=\"'+k+'\"]').forEach(function(e){{\
             var n=document.createElement('img');n.className='hylki-ava';n.alt='';\
             n.dataset.key=k;n.src=u;e.replaceWith(n);}});}});}})({uri:?},{keys:?})"
        ));
    }
}
