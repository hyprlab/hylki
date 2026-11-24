//! DuckDuckGo Email Protection relay addresses (#708). Mail between a
//! Duck Address and anyone else goes through an address that spells the
//! other side out: `brian_at_gmail.com_jane@duck.com` stands for
//! brian@gmail.com, writing to or from jane@duck.com. Settings → Contacts
//! can show the address it stands for; the relay address is what replies,
//! copies and new messages keep using, so mail still goes through duck.com.

use std::borrow::Cow;
use std::cell::Cell;

thread_local! {
    /// Settings → Contacts → Show the address behind a Duck Address.
    static DUCK_ADDRESSES: Cell<bool> = const { Cell::new(false) };
}

/// Show the address behind a relay address from now on, or the relay
/// address itself. True when that changed.
pub(super) fn set_duck_addresses(on: bool) -> bool {
    DUCK_ADDRESSES.with(|c| c.replace(on)) != on
}

/// The address the relay address `addr` stands for, whatever Settings
/// says: `None` for anything that isn't one, a plain Duck Address included.
pub(crate) fn duck_original(addr: &str) -> Option<String> {
    let (local, domain) = addr.trim().rsplit_once('@')?;
    if !domain.eq_ignore_ascii_case("duck.com") {
        return None;
    }
    // The Duck Address's own name follows the last underscore; its names
    // are letters, digits and hyphens.
    let (encoded, alias) = local.rsplit_once('_')?;
    if alias.is_empty() {
        return None;
    }
    // The last `_at_`, since the other side's own name may hold one; a
    // domain never does.
    let at = encoded.to_ascii_lowercase().rfind("_at_")?;
    let (user, host) = (&encoded[..at], &encoded[at + 4..]);
    let host_ok = host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-');
    (!user.is_empty() && host_ok).then(|| format!("{user}@{host}"))
}

/// The address behind the relay address `addr`, when Settings asks for it.
pub(crate) fn shown_duck(addr: &str) -> Option<String> {
    if !DUCK_ADDRESSES.with(Cell::get) {
        return None;
    }
    duck_original(addr)
}

/// `addr` as shown: the address behind a relay address when Settings asks
/// for it, else `addr` itself.
pub(crate) fn shown_addr(addr: &str) -> Cow<'_, str> {
    match shown_duck(addr) {
        Some(original) => Cow::Owned(original),
        None => Cow::Borrowed(addr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_address_names_the_address_behind_it() {
        let cases = [
            ("brian_at_gmail.com_jane@duck.com", Some("brian@gmail.com")),
            (" Brian_AT_Gmail.com_jane@Duck.com ", Some("Brian@Gmail.com")),
            ("j_doe_at_mail.example.org_quiet-fox@duck.com", Some("j_doe@mail.example.org")),
            ("a_at_b_at_x.com_jane@duck.com", Some("a_at_b@x.com")),
            ("jane@duck.com", None),
            ("brian_at_gmail.com_@duck.com", None),
            ("_at_gmail.com_jane@duck.com", None),
            ("brian_at_gmail_jane@duck.com", None),
            ("brian_at_gmail.com_jane@example.com", None),
            ("brian@gmail.com", None),
            ("", None),
        ];
        for (addr, want) in cases {
            assert_eq!(duck_original(addr).as_deref(), want, "{addr}");
        }
    }

    #[test]
    fn the_switch_decides_what_shows() {
        set_duck_addresses(false);
        assert_eq!(shown_addr("brian_at_gmail.com_jane@duck.com"), "brian_at_gmail.com_jane@duck.com");
        assert!(set_duck_addresses(true));
        assert!(!set_duck_addresses(true));
        assert_eq!(shown_addr("brian_at_gmail.com_jane@duck.com"), "brian@gmail.com");
        assert_eq!(shown_addr("jane@duck.com"), "jane@duck.com");
        set_duck_addresses(false);
    }
}
