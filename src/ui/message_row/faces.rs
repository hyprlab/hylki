//! A row's face: the sender's avatar or logo, looked up off the main thread,
//! and the names the row shows.

use super::row::Row;
use super::shared::RowLook;
use super::RowData;
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;
use std::rc::Rc;

/// A background face lookup's answer, correlated by sender address (a recycled
/// row compares before using it). The tiers are personal-first: the contact's
/// own photo, their Gravatar, then the icon their domain publishes (#30), with
/// the UI's colored initials as the implicit last resort.
#[derive(Debug)]
enum FaceCmd {
    /// The avatar tiers (contact photo, Gravatar) answered. `logo` carries the
    /// logo tier's answer when it was consulted in the same trip: found bytes,
    /// or a definitive miss to remember.
    Avatar {
        email: String,
        generation: u64,
        mode: crate::services::avatar::FetchMode,
        outcome: crate::services::avatar::FetchOutcome,
        logo: Option<Option<Vec<u8>>>,
    },
    /// A logo-only lookup — the avatar tiers had already answered from cache.
    Logo {
        email: String,
        bytes: Option<Vec<u8>>,
    },
}

/// Run the avatar tiers off the main thread, falling through to the domain icon
/// when they come up empty and `want_logo` says the switch is on. `generation`
/// and `mode` come from [`crate::services::avatar::lookup`] and ride along so the result
/// can be cached against the EDS state that was actually queried.
async fn find_face(
    email: String,
    generation: u64,
    mode: crate::services::avatar::FetchMode,
    want_logo: bool,
) -> FaceCmd {
    let lookup_email = email.clone();
    let result = tokio::task::spawn_blocking(move || {
        let outcome = crate::services::avatar::fetch(&lookup_email, mode);
        let logo = (want_logo
            && !matches!(outcome, crate::services::avatar::FetchOutcome::Found(_)))
        .then(|| crate::services::logo::fetch(&lookup_email));
        (outcome, logo)
    })
    .await;
    let (outcome, logo) = result.unwrap_or((crate::services::avatar::FetchOutcome::Retry, None));
    FaceCmd::Avatar {
        email,
        generation,
        mode,
        outcome,
        logo,
    }
}

/// Fetch just the domain icon, off the main thread.
async fn find_logo(email: String) -> FaceCmd {
    let lookup_email = email.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::services::logo::fetch(&lookup_email))
        .await
        .ok()
        .flatten();
    FaceCmd::Logo { email, bytes }
}

/// Display names from a raw To header: "Ann <a@x>, b@y" -> "Ann, b@y".
/// A saved contact goes by its saved name when Settings asks (#541).
pub(crate) fn recipient_names(to: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    for part in to.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let addr = match part.split_once('<') {
            Some((_, rest)) => rest.trim_end_matches('>').trim(),
            None => part,
        };
        let name = match part.split_once('<') {
            Some((n, _)) if !n.trim().trim_matches('"').is_empty() => n.trim().trim_matches('"'),
            _ => addr,
        };
        let name = crate::contacts::shown_name(name, addr);
        if !name.is_empty() {
            names.push(name.into_owned());
        }
    }
    names.join(", ")
}

/// The first recipient's bare address from a raw To header, if any.
pub(super) fn first_recipient_addr(to: &str) -> Option<String> {
    let first = to.split(',').map(str::trim).find(|p| !p.is_empty())?;
    let addr = match first.split_once('<') {
        Some((_, rest)) => rest.trim_end_matches('>').trim(),
        None => first,
    };
    (!addr.is_empty()).then(|| addr.to_string())
}

impl Row {
    /// The row's name line: the sender — or, in a Sent folder, who the
    /// message went to, since every sender there is you (#27).
    pub(super) fn name_line(&self, data: &RowData, look: &RowLook) -> String {
        if !look.show_recipient {
            // A thread head surfaces its NEWEST member's sender.
            if let Some((name, addr)) = &data.meta.from {
                return crate::contacts::shown_name(name, addr).into_owned();
            }
            return data.msg.shown_from_name().into_owned();
        }
        let names = recipient_names(&data.msg.to);
        if names.is_empty() {
            data.msg.shown_from_name().into_owned()
        } else {
            format!("To: {names}")
        }
    }

    /// What the avatar's initials (and face lookups) key on: the first
    /// recipient in a Sent folder, the sender everywhere else.
    pub(super) fn face_name(&self, data: &RowData, look: &RowLook) -> String {
        if look.show_recipient {
            let names = recipient_names(&data.msg.to);
            if let Some(first) = names.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
        if let Some((name, addr)) = &data.meta.from {
            return crate::contacts::shown_name(name, addr).into_owned();
        }
        data.msg.shown_from_name().into_owned()
    }

    /// The address face lookups run against — the first recipient's in a
    /// Sent folder, so the circle shows who the mail went to.
    pub(super) fn face_email(&self, data: &RowData, look: &RowLook) -> String {
        if look.show_recipient {
            if let Some(addr) = first_recipient_addr(&data.msg.to) {
                return addr;
            }
        }
        if let Some((_, addr)) = &data.meta.from {
            return addr.clone();
        }
        data.msg.from_addr.clone()
    }

    /// What the avatar circle shows: the sender's picture when one is known,
    /// else their initials drawn ink-centred (see `ui::initials`). The same
    /// paintable is returned for the same name, so the avatar sees no change
    /// between refreshes.
    pub(super) fn avatar_image(
        &self,
        data: &RowData,
        look: &RowLook,
    ) -> Option<gtk::gdk::Paintable> {
        let mut st = self.st.borrow_mut();
        if let Some(tex) = &st.avatar_texture {
            return Some(tex.clone().upcast());
        }
        // A message from one of your own mailboxes wears that mailbox's emoji
        // on its color (#189), the same face the sidebar circle shows. It
        // shares the slot with the initials, keyed by what it draws.
        if let Some((emoji, color)) =
            crate::services::avatar::own_face(&self.face_email(data, look))
                .and_then(|face| face.emoji.map(|emoji| (emoji, face.color)))
        {
            let key = format!("{emoji}\u{1}{color}");
            if st.initials.as_ref().is_none_or(|(n, _)| *n != key) {
                let bg = gtk::gdk::RGBA::parse(&color).unwrap_or(gtk::gdk::RGBA::BLACK);
                let fg = gtk::gdk::RGBA::parse(crate::color::readable_text(&color))
                    .unwrap_or(gtk::gdk::RGBA::WHITE);
                let face = crate::ui::initials::InitialsPaintable::solid(&emoji, bg, fg, 0.55);
                st.initials = Some((key, face));
            }
            return st.initials.as_ref().map(|(_, p)| p.clone().upcast());
        }
        let name = self.face_name(data, look);
        if st.initials.as_ref().is_none_or(|(n, _)| *n != name) {
            st.initials =
                crate::ui::initials::InitialsPaintable::for_name(&name).map(|p| (name.clone(), p));
        }
        st.initials.as_ref().map(|(_, p)| p.clone().upcast())
    }

    /// Fill the circle: a cached face if one is known, otherwise go and
    /// look. The chain is your own mailbox's picture (#189) → contact photo
    /// → Gravatar → domain icon → initials, each tier consulted only while
    /// its switch is on.
    pub(super) fn load_face(&self, email: &str, look: &RowLook) {
        if email.is_empty() {
            return;
        }
        if let Some(face) = crate::services::avatar::own_face(email) {
            let tex = face
                .gravatar
                .then(|| crate::services::avatar::own_gravatar(email))
                .flatten()
                .or_else(|| {
                    face.picture
                        .as_deref()
                        .and_then(crate::ui::initials::avatar_texture)
                });
            self.st.borrow_mut().avatar_texture = tex;
            return;
        }
        match crate::services::avatar::lookup(email, look.gravatar) {
            crate::services::avatar::CacheLookup::Texture(texture) => {
                self.st.borrow_mut().avatar_texture = Some(texture);
            }
            // Contact and Gravatar are definitively absent — the logo tier is
            // all that's left before initials.
            crate::services::avatar::CacheLookup::Missing => self.load_logo(email, look),
            crate::services::avatar::CacheLookup::Fetch { generation, mode } => {
                let want_logo = look.sender_logos && !crate::services::logo::known_missing(email);
                self.look_up(find_face(email.to_string(), generation, mode, want_logo));
            }
        }
    }

    /// The logo tier: only consulted when enabled, so switching "sender
    /// logos" off hides cached logos at once. A domain already asked about
    /// is not asked again — one request a session, not one a row.
    fn load_logo(&self, email: &str, look: &RowLook) {
        if !look.sender_logos {
            self.st.borrow_mut().avatar_texture = None;
            return;
        }
        if let Some(tex) = crate::services::logo::cached(email) {
            self.st.borrow_mut().avatar_texture = Some(tex);
            // A week-old stored icon still shows, but this new message from
            // the sender is the cue to look for a fresh one.
            if crate::services::logo::wants_refresh(email) {
                self.look_up(find_logo(email.to_string()));
            }
            return;
        }
        self.st.borrow_mut().avatar_texture = None;
        if !crate::services::logo::known_missing(email) {
            self.look_up(find_logo(email.to_string()));
        }
    }

    /// Run a face lookup off the main thread and bring its answer back to
    /// this row, which by then may show someone else.
    fn look_up(&self, fut: impl std::future::Future<Output = FaceCmd> + Send + 'static) {
        let weak = self.me.clone();
        let handle = relm4::spawn(fut);
        glib::MainContext::default().spawn_local(async move {
            if let Ok(cmd) = handle.await {
                if let Some(row) = weak.upgrade() {
                    row.face_answer(cmd);
                }
            }
        });
    }

    fn face_answer(&self, cmd: FaceCmd) {
        let Some(shared) = self.shared() else { return };
        let look = shared.look.borrow().clone();
        let current = self.data().map(|d| self.face_email(&d, &look));
        match cmd {
            FaceCmd::Avatar {
                email,
                generation,
                mode,
                outcome,
                logo,
            } => {
                // Record what came back before deciding what to draw — the
                // caches are shared, so the sender's other rows benefit even
                // when this row now shows a different message.
                let retry_stale =
                    crate::services::avatar::cache_result(&email, generation, mode, outcome);
                match logo {
                    Some(Some(bytes)) => {
                        crate::services::logo::decode_and_cache(&email, &bytes);
                    }
                    Some(None) => crate::services::logo::remember_missing(&email),
                    None => {}
                }
                if !current.is_some_and(|c| c.eq_ignore_ascii_case(&email)) {
                    return;
                }
                match crate::services::avatar::lookup(&email, look.gravatar) {
                    crate::services::avatar::CacheLookup::Texture(texture) => {
                        self.st.borrow_mut().avatar_texture = Some(texture);
                    }
                    crate::services::avatar::CacheLookup::Missing => self.load_logo(&email, &look),
                    crate::services::avatar::CacheLookup::Fetch { generation, mode } => {
                        self.st.borrow_mut().avatar_texture = None;
                        // Only chase a result the EDS generation invalidated;
                        // a transient Gravatar failure waits for a later bind.
                        if retry_stale {
                            let want_logo =
                                look.sender_logos && !crate::services::logo::known_missing(&email);
                            self.look_up(find_face(email, generation, mode, want_logo));
                        }
                    }
                }
            }
            FaceCmd::Logo { email, bytes } => {
                let texture = match bytes {
                    Some(bytes) => crate::services::logo::decode_and_cache(&email, &bytes),
                    None => {
                        // Remember the miss, so the sender's other rows and
                        // the next sync don't ask the same domain again.
                        crate::services::logo::remember_missing(&email);
                        None
                    }
                };
                if !(current.is_some_and(|c| c.eq_ignore_ascii_case(&email)) && look.sender_logos) {
                    return;
                }
                self.st.borrow_mut().avatar_texture = texture;
            }
        }
        if let Some(data) = self.data() {
            self.w
                .avatar
                .set_custom_image(self.avatar_image(&data, &look).as_ref());
        }
    }

    /// Slide the circle away (Focus Mode), before the list drops it.
    pub(crate) fn slide_avatar_away(&self) {
        self.st.borrow_mut().avatar_shown = false;
        self.w.avatar_revealer.set_reveal_child(false);
        self.w
            .avatar_revealer
            .set_css_classes(&["focus-fade", "away"]);
    }

    /// Fold the circle and slide it in a moment later (Focus Mode has given
    /// the avatars back), once the row is on screen and the revealer mapped.
    pub(crate) fn slide_avatar_in(self: &Rc<Self>) {
        self.st.borrow_mut().avatar_shown = false;
        self.w.avatar_revealer.set_transition_duration(0);
        self.w.avatar_revealer.set_reveal_child(false);
        self.w
            .avatar_revealer
            .set_transition_duration(crate::ui::FOCUS_ANIM_MS);
        let weak = Rc::downgrade(self);
        let gen = self.st.borrow().gen;
        glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            let Some(row) = weak.upgrade() else { return };
            if row.st.borrow().gen != gen {
                return;
            }
            row.st.borrow_mut().avatar_shown = true;
            row.w.avatar_revealer.set_reveal_child(true);
            row.w.avatar_revealer.set_css_classes(&["focus-fade"]);
        });
    }
}
