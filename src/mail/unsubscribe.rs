//! One-click unsubscribe: finding every way a message offers to be left,
//! and taking the best one.
//!
//! **Headers.** A mailing list that wants to be left announces how in its
//! headers (RFC 2369): `List-Unsubscribe` carries one or more `<uri>`
//! handles, a `mailto:` to write to and/or an `https:` page to visit.
//! RFC 8058 adds `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, which
//! promises that a bare POST of that form body to the https handle
//! unsubscribes the recipient with no page, no login and no confirmation in
//! between — the mechanism the big providers' Unsubscribe buttons run on.
//! Real mail bends the rules in every direction: handles without the angle
//! brackets (ArtStation through Amazon SES), bare addresses without a
//! scheme, `www.` without a scheme, uppercase schemes, whitespace inside the
//! brackets, the header on a MIME part instead of the top, and vendor
//! aliases like `X-Unsubscribe`. All of those are read here.
//!
//! **The body.** Plenty of bulk mail carries no header at all and offers
//! only a link in its footer. So the body is read too: an anchor whose text
//! says unsubscribe (in any of the languages below), whose URL says it, or
//! whose surrounding sentence says it when the link text is only "here"; an
//! image link with alt text; a `mailto:` link; and in plain text, a URL on
//! the line of (or after) the word, "send an email to X", and "reply with
//! UNSUBSCRIBE" — the last two give a route that needs no browser either.
//!
//! **Precedence.** The app takes the routes that need no browser first: the
//! one-click POST, then a mail to a `mailto:` handle, then a reply as
//! instructed. Only a message that offers nothing but a web page sends the
//! user to the browser, and the button says so.
//!
//! The handles are read once, when the message's body is fetched, and ride
//! with the sender check into the cache (see [`crate::mail::verify`]); nothing here
//! touches the network until the user asks.

use crate::mail::mail_header::unfold;
use crate::models::Unsubscribe;

mod action;
mod body;
mod html;
mod text;

pub(crate) use action::{one_click_post, parse_mailto, MailtoTarget};
use body::BodyFindings;
use html::scan_html;
use text::scan_text;

/// The `List-Unsubscribe-Post` value RFC 8058 requires, as it is compared
/// (lowercased, no whitespace or quotes).
const ONE_CLICK: &str = "list-unsubscribe=one-click";

/// Read everything a raw message offers, headers and body.
#[cfg(test)]
fn detect_raw(raw: &[u8]) -> Option<Unsubscribe> {
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    detect(&parsed)
}

/// Read everything a parsed message offers, headers and body.
pub(crate) fn detect(parsed: &mail_parser::Message) -> Option<Unsubscribe> {
    let headers = header_handles(parsed);
    let post = raw_header_values(parsed, "List-Unsubscribe-Post")
        .into_iter()
        .next();
    let list_id = raw_header_values(parsed, "List-Id").into_iter().next();
    let mut found = from_headers(&headers, post.as_deref(), list_id.as_deref());

    // The body, for what the headers did not say. A reply target is needed
    // for "reply with UNSUBSCRIBE": Reply-To first, else From.
    let reply_addr = first_address(parsed.reply_to()).or_else(|| first_address(parsed.from()));
    let mut body = BodyFindings::default();
    for i in 0..parsed.html_body_count() {
        if let Some(html) = parsed.body_html(i) {
            body.merge(scan_html(&html));
        }
    }
    for i in 0..parsed.text_body_count() {
        if let Some(text) = parsed.body_text(i) {
            body.merge(scan_text(&text, reply_addr.as_deref()));
        }
    }
    // A "reply with UNSUBSCRIBE" read out of a message that is itself a
    // reply or a forward would answer the wrong person: the colleague who
    // forwarded you a newsletter, not the list that sent it. The link and
    // the address in such a body still stand; only the reply is dropped.
    if body.reply.is_some() && is_reply_or_forward(parsed) {
        body.reply = None;
    }
    if !body.is_empty() {
        let u = found.get_or_insert_with(|| Unsubscribe {
            list_id: list_id.as_deref().map(list_id_of).unwrap_or_default(),
            ..Default::default()
        });
        if u.mailto.is_none() {
            u.mailto = body.mailto;
        }
        if u.reply.is_none() {
            u.reply = body.reply;
        }
        if u.web.is_none() {
            u.web = body.web.map(|(url, _)| url);
        }
    }
    found
}

/// Whether this message is itself an answer to, or a forward of, another:
/// it references one, or its subject wears one of the prefixes mail
/// programs add.
fn is_reply_or_forward(parsed: &mail_parser::Message) -> bool {
    if parsed
        .in_reply_to()
        .as_text_list()
        .is_some_and(|l| !l.is_empty())
    {
        return true;
    }
    crate::models::is_reply_or_forward_subject(parsed.subject().unwrap_or_default())
}

fn first_address(a: Option<&mail_parser::Address>) -> Option<String> {
    a.and_then(|a| a.first())
        .and_then(|x| x.address())
        .map(|s| s.to_string())
}

/// Every header value that names an unsubscribe handle: `List-Unsubscribe`
/// and any vendor alias whose name says "unsubscribe" (`X-Unsubscribe`,
/// `X-List-Unsubscribe`, `X-Unsubscribe-Web`, …), except the RFC 8058
/// `List-Unsubscribe-Post` marker. The top-level headers first; the parts'
/// own headers only when the top carries none (a broken sender that put
/// the header on its text part).
fn header_handles(parsed: &mail_parser::Message) -> Vec<String> {
    let is_handle_header = |name: &str| {
        let l = name.to_ascii_lowercase();
        l.contains("unsubscribe") && l != "list-unsubscribe-post"
    };
    let raw = parsed.raw_message();
    let of = |headers: &[mail_parser::Header]| -> Vec<String> {
        headers
            .iter()
            .filter(|h| is_handle_header(h.name()))
            .filter_map(|h| raw.get(h.offset_start()..h.offset_end()))
            .map(|b| unfold(&String::from_utf8_lossy(b)))
            .filter(|v| !v.is_empty())
            .collect()
    };
    let top = of(parsed.headers());
    if !top.is_empty() {
        return top;
    }
    parsed
        .parts
        .iter()
        .skip(1)
        .flat_map(|p| of(&p.headers))
        .collect()
}

/// The raw, unfolded value of every top-level header of that name.
/// `mail_parser` parses the `List-*` family as addresses, which mangles a
/// `mailto:` with a query string — the bytes it was given are read back
/// instead.
fn raw_header_values(parsed: &mail_parser::Message, name: &str) -> Vec<String> {
    let raw = parsed.raw_message();
    parsed
        .headers()
        .iter()
        .filter(|h| h.name().eq_ignore_ascii_case(name))
        .filter_map(|h| raw.get(h.offset_start()..h.offset_end()))
        .map(|b| unfold(&String::from_utf8_lossy(b)))
        .filter(|v| !v.is_empty())
        .collect()
}

/// Build the handles from header values: every `List-Unsubscribe`-like
/// value, the `List-Unsubscribe-Post` value and the `List-Id`.
fn from_headers(
    list_unsubscribe: &[String],
    post: Option<&str>,
    list_id: Option<&str>,
) -> Option<Unsubscribe> {
    let mut mailto = None;
    let mut https = None;
    let mut web = None;
    for value in list_unsubscribe {
        for handle in handles_of(value) {
            let lower = handle.to_ascii_lowercase();
            if lower.starts_with("mailto:") {
                if mailto.is_none() && parse_mailto(&handle).is_some() {
                    mailto = Some(handle);
                }
            } else if lower.starts_with("https://") {
                if https.is_none() {
                    https = Some(handle.clone());
                }
                if web.is_none() {
                    web = Some(handle);
                }
            } else if lower.starts_with("http://") && web.is_none() {
                web = Some(handle);
            }
        }
    }
    // RFC 8058 allows the POST to an https handle only: over http the
    // request, and the token that unsubscribes, can be read and replayed
    // on the way, or answered by anyone in between. An http handle is still
    // the list's page, for the browser.
    let one_click = match post {
        Some(p) if squash_ascii(p) == ONE_CLICK => https,
        _ => None,
    };
    if one_click.is_none() && mailto.is_none() && web.is_none() {
        return None;
    }
    Some(Unsubscribe {
        one_click,
        mailto,
        reply: None,
        web,
        list_id: list_id.map(list_id_of).unwrap_or_default(),
        in_headers: true,
    })
}

/// The handles of a `List-Unsubscribe` value, in order. RFC 2369 wants each
/// in `<…>`, with comments (`(…)`) and anything else outside the brackets
/// skipped — but plenty of real mail writes bare handles with no brackets
/// at all, so a value without any is read as bare handles separated by
/// commas or whitespace. A handle with no scheme is given one: an address
/// becomes `mailto:`, a `www.` host becomes `https://`.
fn handles_of(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else { break };
        let handle: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if !handle.is_empty() {
            out.push(handle);
        }
        rest = &after[end + 1..];
    }
    if out.is_empty() && !value.contains('<') {
        out.extend(
            value
                .split(|c: char| c == ',' || c.is_whitespace())
                .map(|t| t.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')'))
                .filter(|t| !t.is_empty())
                .map(str::to_string),
        );
    }
    out.into_iter().filter_map(with_scheme).collect()
}

/// A handle with its scheme supplied when the sender left it off; `None`
/// for something that is neither an address nor a web address.
fn with_scheme(handle: String) -> Option<String> {
    let lower = handle.to_ascii_lowercase();
    if lower.starts_with("mailto:") || lower.starts_with("http://") || lower.starts_with("https://")
    {
        return Some(handle);
    }
    if lower.starts_with("www.") {
        return Some(format!("https://{handle}"));
    }
    if is_email(&handle) {
        return Some(format!("mailto:{handle}"));
    }
    None
}

/// A bare address: one `@`, a dot after it, nothing that a URL would carry.
fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.ends_with('.')
        && !s.contains(['/', ':', '?', ' ', '<', '>', ',', '"'])
        && s.matches('@').count() == 1
}

/// Lowercased, with whitespace and quotes gone: how header markers compare.
fn squash_ascii(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The identifier out of a `List-Id` value (`Weekly digest <weekly.example.com>`
/// gives `weekly.example.com`), lowercased: the key a list is remembered by.
fn list_id_of(value: &str) -> String {
    let id = match (value.find('<'), value.rfind('>')) {
        (Some(s), Some(e)) if e > s => &value[s + 1..e],
        _ => value,
    };
    id.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::body::{has_medium, has_strong, squash};
    use super::*;
    use crate::models::ReplyRoute;

    /// Header unfolding (`crate::mail::mail_header::unfold`), on the corpus both
    /// header readers were pinned with before their copies were merged.
    #[test]
    fn folded_header_values_become_one_line() {
        let corpus = [
            ("a\r\n b", "a b"),
            ("a\r\n\tb\r\n  c", "a b c"),
            ("  padded  ", "padded"),
            ("a\n\n\nb", "a b"),
            ("a\rb", "a b"),
            ("<x>,\r\n <y>", "<x>, <y>"),
            ("", ""),
            ("\r\n", ""),
        ];
        for (raw, one) in corpus {
            assert_eq!(unfold(raw), one, "{raw:?}");
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn message(headers: &str, content_type: &str, body: &str) -> Vec<u8> {
        format!("From: Digest <digest@list.example>\r\n{headers}Content-Type: {content_type}\r\n\r\n{body}\r\n")
            .into_bytes()
    }

    #[test]
    fn mailto_and_web_handles_are_told_apart() {
        let u = from_headers(
            &s(&["<mailto:leave@list.example?subject=unsubscribe>, <https://list.example/u/1>"]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            u.mailto.as_deref(),
            Some("mailto:leave@list.example?subject=unsubscribe")
        );
        assert_eq!(u.web.as_deref(), Some("https://list.example/u/1"));
        assert_eq!(
            u.one_click, None,
            "no List-Unsubscribe-Post, no one-click promise"
        );
        assert!(u.list_id.is_empty());
        assert!(u.in_headers);
    }

    #[test]
    fn one_click_needs_the_post_header() {
        let hdr = s(&["<https://list.example/u/1>, <mailto:leave@list.example>"]);
        let u = from_headers(&hdr, Some("List-Unsubscribe=One-Click"), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/1"));
        let u = from_headers(&hdr, Some(" \"list-unsubscribe = one-click\" "), None).unwrap();
        assert_eq!(
            u.one_click.as_deref(),
            Some("https://list.example/u/1"),
            "case, space and quotes are forgiven"
        );
        let u = from_headers(&hdr, Some("something-else"), None).unwrap();
        assert_eq!(u.one_click, None);
        let u = from_headers(
            &s(&["<http://list.example/u/1>"]),
            Some("List-Unsubscribe=One-Click"),
            None,
        )
        .unwrap();
        assert_eq!(u.one_click, None, "RFC 8058: no POST over http");
        assert_eq!(
            u.web.as_deref(),
            Some("http://list.example/u/1"),
            "the page still opens in the browser"
        );
    }

    #[test]
    fn comments_folding_and_whitespace_are_forgiven() {
        let hdr = s(&[
            "(Use this command to get off the list) <mailto:leave@list.example>,  < https://list.example/leave >",
        ]);
        let u = from_headers(&hdr, None, Some("Weekly digest <weekly.list.example>")).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example"));
        assert_eq!(u.web.as_deref(), Some("https://list.example/leave"));
        assert_eq!(u.list_id, "weekly.list.example");
        assert_eq!(
            unfold("<mailto:a@b.c>,\r\n <https://x.y/z>"),
            "<mailto:a@b.c>, <https://x.y/z>"
        );
    }

    #[test]
    fn bare_handles_without_brackets_or_schemes_are_taken_too() {
        // ArtStation, through Amazon SES: the URL alone, no brackets.
        let hdr = s(&[
            "https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish",
        ]);
        let u = from_headers(&hdr, None, None).unwrap();
        assert_eq!(u.web.as_deref(), Some("https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish"));
        assert!(!u.direct(), "a page only: the button opens the browser");
        let u = from_headers(
            &s(&["MAILTO:leave@x.example, https://x.example/u"]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(u.mailto.as_deref(), Some("MAILTO:leave@x.example"));
        assert_eq!(u.web.as_deref(), Some("https://x.example/u"));
        // No scheme at all: an address, or a www. host.
        let u = from_headers(
            &s(&["<leave@x.example>, <www.x.example/unsub>"]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@x.example"));
        assert_eq!(u.web.as_deref(), Some("https://www.x.example/unsub"));
        // A bracketed value keeps the strict reading: text outside is not a handle.
        let u = from_headers(&s(&["https://ignored.example <mailto:a@b.c>"]), None, None).unwrap();
        assert_eq!(u.web, None);
    }

    #[test]
    fn a_message_with_no_handles_offers_nothing() {
        assert_eq!(from_headers(&[], None, None), None);
        assert_eq!(from_headers(&s(&["nothing in brackets"]), None, None), None);
        assert_eq!(
            from_headers(&s(&["<ftp://odd.example/x>"]), None, None),
            None
        );
        let plain = message(
            "",
            "text/plain",
            "Hi, lunch on Saturday? I unsubscribed from that gym, by the way.",
        );
        assert_eq!(
            detect_raw(&plain),
            None,
            "the word alone, with no link, is not a route"
        );
    }

    #[test]
    fn vendor_aliases_and_part_headers_count() {
        let raw = message(
            "X-Unsubscribe-Web: https://x.example/leave/1\r\n",
            "text/plain",
            "hi",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/leave/1")
        );
        let raw = b"From: a@b.c\r\nContent-Type: multipart/alternative; boundary=B\r\n\r\n--B\r\n\
Content-Type: text/plain\r\nList-Unsubscribe: <mailto:leave@x.example>\r\n\r\nhi\r\n--B--\r\n";
        assert_eq!(
            detect_raw(raw).unwrap().mailto.as_deref(),
            Some("mailto:leave@x.example")
        );
    }

    #[test]
    fn the_demo_newsletter_offers_one_click_and_mail() {
        let raw = crate::backend::demo_raw(10).expect("the demo newsletter has headers");
        let u = detect_raw(raw.as_bytes()).expect("handles");
        assert!(u.one_click.is_some() && u.mailto.is_some() && u.direct());
        assert_eq!(u.list_id, "digest.this-week-in-rust.org");
        assert_eq!(
            crate::mail::verify::check_sender(raw.as_bytes()).trust,
            crate::models::SenderTrust::Pass
        );
    }

    #[test]
    fn detect_reads_the_raw_headers_of_a_message() {
        let raw = b"From: Digest <digest@list.example>\r\n\
List-Id: The digest <digest.list.example>\r\n\
List-Unsubscribe: <mailto:leave@list.example?subject=unsubscribe>,\r\n <https://list.example/u/abc>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Subject: Hello\r\n\r\nBody\r\n";
        let u = detect_raw(raw.as_slice()).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/abc"));
        assert_eq!(
            u.mailto.as_deref(),
            Some("mailto:leave@list.example?subject=unsubscribe")
        );
        assert_eq!(u.list_id, "digest.list.example");
        let plain = b"From: a@b.c\r\nSubject: x\r\n\r\nhi\r\n";
        assert_eq!(detect_raw(plain.as_slice()), None);
    }

    #[test]
    fn a_footer_link_that_says_unsubscribe_is_found() {
        let raw = message(
            "",
            "text/html",
            "<p>News.</p><p style=\"font-size:11px\">You get this because you signed up. \
             <a href=\"https://track.example/c?u=1\">View online</a> | \
             <a href=\"https://track.example/c?u=2\"><span>Unsub</span>scribe</a></p>",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(u.web.as_deref(), Some("https://track.example/c?u=2"));
        assert!(!u.in_headers);
        assert!(!u.direct());
    }

    #[test]
    fn a_link_whose_url_says_it_is_found_whatever_its_text() {
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://esp.example/list-manage/unsubscribe?u=1&amp;id=2\">Click</a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://esp.example/list-manage/unsubscribe?u=1&id=2")
        );
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://esp.example/optout/9\">Ok</a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://esp.example/optout/9")
        );
    }

    #[test]
    fn click_here_takes_its_sentence_into_account() {
        let raw = message(
            "",
            "text/html",
            "<td>To stop receiving these emails, please <a href=\"https://x.example/r/1\">click here</a>.</td>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/r/1")
        );
        // The same sentence with a generic link elsewhere: not this link.
        let raw = message(
            "",
            "text/html",
            "<p>Read the story <a href=\"https://x.example/story\">here</a>.</p>",
        );
        assert_eq!(detect_raw(&raw), None);
    }

    #[test]
    fn the_word_after_a_link_counts_as_much_as_before_it() {
        // loanDepot, Substack and others write it this way round.
        let raw = message(
            "",
            "text/html",
            "<p>If you do not wish to receive future messages, \
             <a href=\"https://x.example/u/d/abc\">click here</a> to unsubscribe.<br>Irvine, CA</p>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/u/d/abc")
        );
        // A generic link must not borrow the next link's words.
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/story\">here</a> · <a href=\"https://x.example/bye\">Unsubscribe</a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/bye")
        );
    }

    #[test]
    fn the_strongest_link_wins_over_a_preferences_page() {
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/prefs\">Manage your preferences</a> · \
             <a href=\"https://x.example/bye\">Unsubscribe</a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/bye")
        );
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/prefs\">Email preferences</a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/prefs"),
            "a preferences page still counts when it is all there is"
        );
    }

    #[test]
    fn image_links_and_mailto_links_in_the_body_count() {
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/u/7\"><img src=\"cid:btn\" alt=\"Unsubscribe\"></a>",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/u/7")
        );
        let raw = message(
            "",
            "text/html",
            "<a href=\"mailto:leave@x.example?subject=Unsubscribe\">Unsubscribe</a>",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(
            u.mailto.as_deref(),
            Some("mailto:leave@x.example?subject=Unsubscribe")
        );
        assert!(u.direct());
    }

    #[test]
    fn other_languages_are_understood() {
        for text in [
            "Abmelden",
            "Se désabonner",
            "Darse de baja",
            "Cancelar subscrição",
            "Disiscriviti",
            "Uitschrijven",
            "Avregistrera",
            "Wypisz się",
            "Отписаться",
            "配信停止",
            "取消订阅",
            "구독 취소",
            "Abonelikten çık",
        ] {
            let raw = message(
                "",
                "text/html; charset=utf-8",
                &format!("<a href=\"https://x.example/l\">{text}</a>"),
            );
            assert_eq!(
                detect_raw(&raw).unwrap().web.as_deref(),
                Some("https://x.example/l"),
                "{text}"
            );
        }
    }

    #[test]
    fn plain_text_instructions_are_read() {
        let raw = message(
            "",
            "text/plain",
            "Thanks for reading.\n\nTo unsubscribe, visit:\nhttps://x.example/unsub/abc\n",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().web.as_deref(),
            Some("https://x.example/unsub/abc")
        );

        let raw = message(
            "",
            "text/plain",
            "To unsubscribe, send an email to leave@x.example with the subject UNSUBSCRIBE.",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(
            u.mailto.as_deref(),
            Some("mailto:leave@x.example?subject=UNSUBSCRIBE")
        );

        let raw = message(
            "Reply-To: bounce@x.example\r\n",
            "text/plain",
            "Don't want these? Reply to this email with STOP in the subject line.",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(
            u.reply,
            Some(ReplyRoute {
                to: "bounce@x.example".into(),
                subject: "STOP".into()
            })
        );
        assert!(u.direct());

        let raw = message(
            "",
            "text/plain",
            "Just reply with the word unsubscribe and we'll take you off.",
        );
        assert_eq!(
            detect_raw(&raw).unwrap().reply.unwrap().to,
            "digest@list.example",
            "From when there is no Reply-To"
        );
    }

    #[test]
    fn a_forwarded_newsletter_never_gets_a_reply_route() {
        // A relative forwards you a newsletter: its footer's link is still
        // offered, but answering *them* with UNSUBSCRIBE would be wrong.
        let raw = message(
            "Subject: Fw: Arsenic in Rice\r\nIn-Reply-To: <abc@news.example>\r\n",
            "text/plain",
            "Thought you'd want this.\n\nTo stop these, reply with UNSUBSCRIBE or visit https://news.example/u/9",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(u.reply, None, "no reply route out of a forward");
        assert_eq!(u.web.as_deref(), Some("https://news.example/u/9"));
        // The same words in a message of its own keep the reply route.
        let raw = message(
            "Subject: This week\r\n",
            "text/plain",
            "To stop these, reply with UNSUBSCRIBE.",
        );
        assert!(detect_raw(&raw).unwrap().reply.is_some());
    }

    #[test]
    fn ordinary_mail_is_not_mistaken_for_an_instruction() {
        for line in [
            "Reply to this when you get a chance and we'll stop by the office.",
            "Send me the list and I'll remove the duplicates.",
            "Feel free to reply if you want to leave early.",
            "The build did not end cleanly — reply with the log.",
        ] {
            let raw = message("", "text/plain", line);
            assert_eq!(detect_raw(&raw), None, "{line}");
        }
        // The same sentence with the command in capitals is an instruction.
        let raw = message(
            "",
            "text/plain",
            "Reply with STOP and we will take you off.",
        );
        assert_eq!(detect_raw(&raw).unwrap().reply.unwrap().subject, "STOP");
    }

    #[test]
    fn body_findings_fill_in_what_headers_left_out() {
        let raw = message(
            "List-Unsubscribe: <https://x.example/page>\r\n",
            "text/html",
            "<a href=\"mailto:leave@x.example\">Unsubscribe by email</a>",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(
            u.web.as_deref(),
            Some("https://x.example/page"),
            "the header's page is kept"
        );
        assert_eq!(
            u.mailto.as_deref(),
            Some("mailto:leave@x.example"),
            "the body's mailto is added"
        );
        assert!(u.in_headers && u.direct());
    }

    /// Survey a directory of saved messages: `HYLKI_UNSUB_SURVEY=<dir>
    /// cargo test --bin hylki unsubscribe::tests::survey_dir -- --ignored
    /// --nocapture` reports what was found for each, and names every
    /// message that says "unsubscribe" somewhere but gave up no route —
    /// the ones that slipped through.
    #[test]
    #[ignore]
    fn survey_dir() {
        let Ok(dir) = std::env::var("HYLKI_UNSUB_SURVEY") else {
            return;
        };
        let (mut total, mut hdr, mut body_only, mut none, mut missed) = (0, 0, 0, 0, 0);
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let raw = match std::fs::read(entry.path()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            total += 1;
            let name = entry.file_name().to_string_lossy().to_string();
            let parsed = mail_parser::MessageParser::default().parse(&raw);
            let from = parsed
                .as_ref()
                .and_then(|p| p.from().and_then(|a| a.first()).and_then(|x| x.address()))
                .unwrap_or("?")
                .to_string();
            match detect_raw(&raw) {
                Some(u) => {
                    if u.in_headers {
                        hdr += 1;
                    } else {
                        body_only += 1;
                    }
                    let route = if u.one_click.is_some() {
                        "one-click"
                    } else if u.mailto.is_some() {
                        "mailto"
                    } else if u.reply.is_some() {
                        "reply"
                    } else {
                        "web"
                    };
                    eprintln!(
                        "{:<9} {:<10} {from}",
                        route,
                        if u.in_headers { "header" } else { "body" }
                    );
                }
                None => {
                    none += 1;
                    // Did we miss one? Only counts if the word is really there.
                    let text: String = (0..parsed.as_ref().map_or(0, |p| p.text_body_count()))
                        .filter_map(|i| {
                            parsed
                                .as_ref()
                                .and_then(|p| p.body_text(i))
                                .map(|t| t.to_string())
                        })
                        .chain(
                            (0..parsed.as_ref().map_or(0, |p| p.html_body_count())).filter_map(
                                |i| {
                                    parsed
                                        .as_ref()
                                        .and_then(|p| p.body_html(i))
                                        .map(|t| t.to_string())
                                },
                            ),
                        )
                        .collect();
                    let sq = squash(&text);
                    if has_strong(&sq) || has_medium(&sq) {
                        missed += 1;
                        eprintln!("MISSED    {name} {from}");
                    }
                }
            }
        }
        eprintln!("\n== {total} messages: {hdr} from headers, {body_only} from the body, {none} with nothing ({missed} of those say the word) ==");
    }

    /// Probe a saved message (or header block): `HYLKI_UNSUB_PROBE=<file>
    /// cargo test --bin hylki unsubscribe::tests::probe_file -- --ignored
    /// --nocapture` prints what is read out of it.
    #[test]
    #[ignore]
    fn probe_file() {
        let Ok(path) = std::env::var("HYLKI_UNSUB_PROBE") else {
            return;
        };
        let raw = std::fs::read(&path).expect("readable file");
        let check = crate::mail::verify::check_sender(&raw);
        eprintln!(
            "{path}: trust {:?}, unsubscribe {:#?}",
            check.trust,
            detect_raw(&raw)
        );
    }
}
