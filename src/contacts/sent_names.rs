//! The names your own mail gave the people it went to (#724). Every backend
//! keeps a message's recipients as bare addresses, so a card of sent mail
//! has no name for them unless the address books have one. The composer
//! records each recipient with the name it went out under, and that is the
//! name shown.
//!
//! Only your own sends count: a name a received message gives its sender
//! is whatever that sender claimed, and stays out of here.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

static SENT_NAMES: RwLock<Option<Arc<HashMap<String, String>>>> = RwLock::new(None);

/// Read the recorded names on another thread. Until they are in, sent mail
/// shows the addresses alone.
pub(crate) fn load_sent_names() {
    std::thread::spawn(|| {
        let Ok(cache) = crate::cache::Cache::open() else {
            return;
        };
        let names = cache.recorded_names();
        if let Ok(mut slot) = SENT_NAMES.write() {
            // A send while this read ran has its names in already.
            let mut all = names;
            if let Some(sent) = slot.as_ref() {
                all.extend(sent.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            *slot = Some(Arc::new(all));
        }
    });
}

/// Names a send just recorded, so its card names them straight away.
pub(crate) fn note_sent_names(recipients: &[(String, String)]) {
    let Ok(mut slot) = SENT_NAMES.write() else {
        return;
    };
    let mut names = slot.as_deref().cloned().unwrap_or_default();
    for (name, email) in recipients {
        let name = name.trim();
        if !name.is_empty() && !name.eq_ignore_ascii_case(email.trim()) {
            names.insert(email.trim().to_lowercase(), name.to_string());
        }
    }
    *slot = Some(Arc::new(names));
}

/// The name `addr` was last written to under, if any.
fn sent_name(addr: &str) -> Option<String> {
    let slot = SENT_NAMES.read().ok()?;
    slot.as_ref()?.get(&addr.trim().to_lowercase()).cloned()
}

/// What to call recipient `name` <`addr`>: the saved contact's name, the
/// name the mail gives, else the name you last wrote to them under. Empty
/// when none of them names anyone.
pub(crate) fn recipient_name(name: &str, addr: &str) -> String {
    let shown = super::shown_name(name, addr);
    let shown = shown.trim();
    if !shown.is_empty() && !shown.eq_ignore_ascii_case(addr.trim()) {
        return shown.to_string();
    }
    sent_name(addr).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_names_its_recipients() {
        note_sent_names(&[
            ("Public".into(), "Public@DCCMA.example".into()),
            (String::new(), "bare@x.example".into()),
            ("same@x.example".into(), "same@x.example".into()),
        ]);
        assert_eq!(recipient_name("", "public@dccma.example"), "Public");
        assert_eq!(recipient_name("Given", "public@dccma.example"), "Given");
        assert_eq!(recipient_name("", "bare@x.example"), "");
        assert_eq!(recipient_name("", "same@x.example"), "");
    }
}
