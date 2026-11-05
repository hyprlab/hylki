//! Acting on an offer: a `mailto:` handle taken apart for the composer,
//! and the RFC 8058 one-click POST.

/// Where a `mailto:` handle sends its unsubscribe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MailtoTarget {
    /// Comma-separated addresses, as the composer takes them.
    pub(crate) to: String,
    pub(crate) subject: String,
    pub(crate) body: String,
}

/// Take a `mailto:` URI apart (RFC 6068): the addresses before `?`, then
/// `subject=`, `body=`, `to=` and `cc=` in the query, percent-decoded.
pub(crate) fn parse_mailto(uri: &str) -> Option<MailtoTarget> {
    let rest = strip_scheme(uri, "mailto:")?;
    let (addr_part, query) = match rest.split_once('?') {
        Some((a, q)) => (a, q),
        None => (rest, ""),
    };
    let mut to: Vec<String> = addr_part
        .split(',')
        .map(|a| crate::encoding::percent::decode(a, false))
        .map(|a| a.trim().to_string())
        .filter(|a| a.contains('@'))
        .collect();
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = k.to_ascii_lowercase();
        // An address keeps its `+` (plus-addressing, `list+token@…`); only
        // the text fields read it as a space, as form-encoded links write it.
        let v = crate::encoding::percent::decode(v, !matches!(key.as_str(), "to" | "cc"));
        match key.as_str() {
            "subject" => subject = v,
            "body" => body = v,
            "to" | "cc" => to.extend(
                v.split(',')
                    .map(|a| a.trim().to_string())
                    .filter(|a| a.contains('@')),
            ),
            _ => {}
        }
    }
    if to.is_empty() {
        return None;
    }
    // A list that names no subject still gets a request it can recognise;
    // most mailto handles carry `subject=unsubscribe` anyway.
    if subject.trim().is_empty() {
        subject = "Unsubscribe".to_string();
    }
    if body.trim().is_empty() {
        body = subject.clone();
    }
    Some(MailtoTarget {
        to: to.join(", "),
        subject,
        body,
    })
}

fn strip_scheme<'a>(uri: &'a str, scheme: &str) -> Option<&'a str> {
    let uri = uri.trim();
    (uri.len() >= scheme.len() && uri[..scheme.len()].eq_ignore_ascii_case(scheme))
        .then(|| &uri[scheme.len()..])
}

/// Send the RFC 8058 one-click request: a POST of `List-Unsubscribe=One-Click`
/// to the list's handle. Blocking; run it off the UI thread. Only a 2xx
/// answer to the POST itself means the list took the request. Only an
/// https handle is posted to (RFC 8058 §3.1), whatever a cached verdict
/// from before that rule says.
pub(crate) fn one_click_post(url: &str) -> Result<(), String> {
    if !url
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("https://")
    {
        return Err("the list's one-click handle is not an https address".to_string());
    }
    post_one_click(url)
}

fn post_one_click(url: &str) -> Result<(), String> {
    // Redirects are not followed: ureq turns a redirected POST into a GET,
    // and the page it lands on (a login, a "confirm" form) answers 200
    // without anything having been unsubscribed (#284).
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    match agent
        .post(url)
        .set("User-Agent", crate::services::logo::USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .send_form(&[("List-Unsubscribe", "One-Click")])
    {
        Ok(resp) => post_status(resp.status()),
        Err(ureq::Error::Status(code, _)) => post_status(code),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether the one-click answer's status says the list took the request.
fn post_status(code: u16) -> Result<(), String> {
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(format!("the list's server answered {code}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailto_targets_are_taken_apart() {
        let t = parse_mailto("mailto:leave@list.example?subject=unsubscribe%20me&body=please+go")
            .unwrap();
        assert_eq!(t.to, "leave@list.example");
        assert_eq!(t.subject, "unsubscribe me");
        assert_eq!(t.body, "please go");
        let t = parse_mailto("MAILTO:a@x.example,b@x.example").unwrap();
        assert_eq!(t.to, "a@x.example, b@x.example");
        assert_eq!(t.subject, "Unsubscribe", "a subject is always sent");
        assert_eq!(t.body, "Unsubscribe");
        // LISTSERV: the command in the body, no subject.
        let t = parse_mailto("mailto:LISTSERV@lists.example?body=SIGNOFF%20MYLIST").unwrap();
        assert_eq!(t.body, "SIGNOFF MYLIST");
        // Plus-addressed handles keep their `+`: it is part of the address.
        let t =
            parse_mailto("mailto:list+abc123@x.example?cc=b+1@x.example&subject=go+away").unwrap();
        assert_eq!(t.to, "list+abc123@x.example, b+1@x.example");
        assert_eq!(t.subject, "go away");
        assert_eq!(
            parse_mailto("mailto:?subject=x"),
            None,
            "no address, no target"
        );
        assert_eq!(parse_mailto("https://x.example"), None);
    }

    /// A local server that answers the first request with `status` and a
    /// second one, should a redirect be followed, with 200.
    fn answer_once(status: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reply = format!(
            "HTTP/1.1 {status}\r\nLocation: http://127.0.0.1:{port}/done\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        std::thread::spawn(move || {
            let ok =
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
            for answer in [reply, ok] {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(answer.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/unsub")
    }

    #[test]
    fn one_click_takes_only_a_direct_success() {
        // The local server speaks plain http, so it is reached past the
        // https rule, which is checked on its own below.
        assert!(post_one_click(&answer_once("200 OK")).is_ok());
        assert!(post_one_click(&answer_once("202 Accepted")).is_ok());
        // A redirect is not followed: the page behind it would answer 200
        // to a GET and pass for a list that took the request (#284).
        assert!(post_one_click(&answer_once("302 Found")).is_err());
        assert!(post_one_click(&answer_once("303 See Other")).is_err());
        assert!(post_one_click(&answer_once("404 Not Found")).is_err());
    }

    #[test]
    fn one_click_posts_to_https_only() {
        // Refused before any connection is tried.
        assert!(one_click_post("http://127.0.0.1:9/u").is_err());
        assert!(one_click_post("HTTP://list.example/u").is_err());
        assert!(one_click_post("mailto:leave@list.example").is_err());
    }
}
