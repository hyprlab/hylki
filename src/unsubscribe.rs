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
//! with the sender check into the cache (see [`crate::verify`]); nothing here
//! touches the network until the user asks.

use crate::models::{ReplyRoute, Unsubscribe};

/// The `List-Unsubscribe-Post` value RFC 8058 requires, as it is compared
/// (lowercased, no whitespace or quotes).
const ONE_CLICK: &str = "list-unsubscribe=one-click";

/// Read everything a raw message offers, headers and body.
pub fn detect_raw(raw: &[u8]) -> Option<Unsubscribe> {
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    detect(&parsed)
}

/// Read everything a parsed message offers, headers and body.
pub fn detect(parsed: &mail_parser::Message) -> Option<Unsubscribe> {
    let headers = header_handles(parsed);
    let post = raw_header_values(parsed, "List-Unsubscribe-Post").into_iter().next();
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
    if parsed.in_reply_to().as_text_list().is_some_and(|l| !l.is_empty()) {
        return true;
    }
    let subject = parsed.subject().unwrap_or_default().trim_start().to_ascii_lowercase();
    ["re:", "fw:", "fwd:", "aw:", "sv:", "vs:", "tr:", "rv:", "wg:", "antw:", "r:", "i:"]
        .iter()
        .any(|p| subject.starts_with(p))
}

fn first_address(a: Option<&mail_parser::Address>) -> Option<String> {
    a.and_then(|a| a.first()).and_then(|x| x.address()).map(|s| s.to_string())
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
    parsed.parts.iter().skip(1).flat_map(|p| of(&p.headers)).collect()
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

/// A folded header value as one line.
fn unfold(value: &str) -> String {
    value
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the handles from header values: every `List-Unsubscribe`-like
/// value, the `List-Unsubscribe-Post` value and the `List-Id`.
pub fn from_headers(
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
    // RFC 8058 asks for https; a sender who promised one-click on an http
    // handle still gets the POST — the token is already in a cleartext mail,
    // and the alternative is a browser.
    let one_click = match post {
        Some(p) if squash_ascii(p) == ONE_CLICK => https.or_else(|| web.clone()),
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
        let handle: String = after[..end].chars().filter(|c| !c.is_whitespace()).collect();
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
    if lower.starts_with("mailto:") || lower.starts_with("http://") || lower.starts_with("https://") {
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
    let Some((local, domain)) = s.split_once('@') else { return false };
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

// ---------------------------------------------------------------------------
// The body
// ---------------------------------------------------------------------------

/// What the body offered: the best web link with its score, a `mailto:`,
/// and a reply instruction.
#[derive(Debug, Default)]
struct BodyFindings {
    web: Option<(String, u8)>,
    mailto: Option<String>,
    reply: Option<ReplyRoute>,
}

impl BodyFindings {
    fn is_empty(&self) -> bool {
        self.web.is_none() && self.mailto.is_none() && self.reply.is_none()
    }

    fn merge(&mut self, other: BodyFindings) {
        if let Some((url, score)) = other.web {
            self.offer_web(url, score);
        }
        if self.mailto.is_none() {
            self.mailto = other.mailto;
        }
        if self.reply.is_none() {
            self.reply = other.reply;
        }
    }

    fn offer_web(&mut self, url: String, score: u8) {
        if score > 0 && self.web.as_ref().is_none_or(|(_, s)| score > *s) {
            self.web = Some((url, score));
        }
    }
}

/// How sure a link is: the text says it outright, the URL says it, the
/// sentence around a "click here" says it, or only a preferences page.
const STRONG_TEXT: u8 = 4;
const STRONG_URL: u8 = 3;
const CONTEXT: u8 = 3;
const MEDIUM: u8 = 2;

/// Words that mean "unsubscribe", squashed (see [`squash`]) so they match
/// whatever the spacing, hyphens or case. English first, then the languages
/// bulk mail is commonly sent in.
const STRONG_WORDS: &[&str] = &[
    "unsubscribe", "unsubcribe", "unsuscribe", "unsubscribing", "optout", "signoff",
    "stopreceiving", "nolongerwishtoreceive", "nolongerwanttoreceive", "nolongerreceive",
    "removeme", "removeyourself", "removefromthislist", "removefromlist", "removefromourlist",
    "leavethislist", "leavelist", "leavethegroup", "cancelsubscription", "cancelyoursubscription",
    "cancelmysubscription", "endsubscription", "stopemails", "stoptheseemails",
    "stopthesemessages", "turnoffthesenotifications", "turnoffnotifications",
    // German
    "abmelden", "abbestellen", "austragen", "newsletterabmelden", "keineweiteren",
    // French
    "désabonner", "desabonner", "désinscrire", "desinscrire", "désinscription", "desinscription",
    "désabonnement", "desabonnement", "neplusrecevoir",
    // Spanish
    "darsedebaja", "darmedebaja", "dardebaja", "bajadelalista", "cancelarsuscripción",
    "cancelarsuscripcion", "anularsuscripción", "anularsuscripcion", "desuscribir", "dejarderecibir",
    // Portuguese
    "cancelarsubscrição", "cancelarsubscricao", "cancelarinscrição", "cancelarinscricao",
    "descadastrar", "desinscrever", "sairdalista", "deixarderecebir",
    // Italian
    "disiscriviti", "disiscrizione", "annullaiscrizione", "cancellaiscrizione", "cancellati",
    "rimuovimi", "nonriceverepiù",
    // Dutch
    "afmelden", "uitschrijven", "uitschrijving", "afmelding",
    // Swedish, Danish, Norwegian, Finnish
    "avregistrera", "avslutaprenumeration", "avprenumerera", "afmeld", "frameld", "opsigabonnement",
    "avmeld", "avsluttabonnement", "meldavabonnement", "peruutatilaus", "poistatilaus", "perutilaus",
    "lopetatilaus",
    // Polish, Czech, Slovak, Hungarian, Romanian
    "wypiszsię", "wypiszsie", "rezygnacja", "anulujsubskrypcję", "anulujsubskrypcje", "zrezygnuj",
    "odhlásit", "odhlasit", "odhlášení", "odhlaseni", "zrušitodběr", "zrusitodber", "odhlásiť",
    "leiratkozás", "leiratkozas", "leiratkozom", "dezabonare", "dezabonează", "dezaboneaza",
    // Russian, Ukrainian, Greek, Turkish
    "отписаться", "отписка", "отменитьподписку", "отказатьсяотрассылки", "відписатися", "відписка",
    "απεγγραφή", "κατάργησηεγγραφής", "διαγραφήαπότηλίστα",
    "aboneliktençık", "aboneliktencik", "abonelikiptal", "aboneliğiiptal", "listedençık",
    // Japanese, Chinese, Korean
    "配信停止", "購読解除", "登録解除", "配信解除", "購読をやめる", "受信拒否", "配信を停止",
    "取消订阅", "退订", "取消訂閱", "退訂", "구독취소", "수신거부", "구독해지",
    // Arabic, Hebrew, Indonesian/Malay, Vietnamese, Thai, Hindi
    "إلغاءالاشتراك", "الغاءالاشتراك", "הסרהמרשימתהתפוצה", "להסרהמרשימת", "הסרמרשימת",
    "berhentiberlangganan", "berhentilangganan", "hủyđăngký", "huydangky", "ยกเลิกการสมัคร",
    "ยกเลิกรับข่าวสาร", "सदस्यतारद्द", "सदस्यतासमाप्त",
];

/// Words that mean a preferences page, where unsubscribing is one choice.
const MEDIUM_WORDS: &[&str] = &[
    "emailpreference", "communicationpreference", "notificationpreference", "subscriptionpreference",
    "managepreference", "manageyourpreference", "updatepreference", "updateyourpreference",
    "changeyourpreference", "preferencecenter", "preferencecentre", "managesubscription",
    "manageyoursubscription", "managenotification", "notificationsetting", "emailsetting",
    "subscriptionsetting", "manageemail", "managealert", "manageyouremail", "mailingpreference",
];

/// Link texts that say nothing by themselves; the sentence around them does.
const GENERIC_TEXTS: &[&str] = &[
    "here", "clickhere", "click", "thislink", "link", "thispage", "page", "this", "tap", "taphere",
    "clic", "cliquezici", "ici", "hier", "klickhier", "klickensiehier", "aquí", "aqui", "hagaclicaquí",
    "cliqueaqui", "qui", "clicca", "cliccaqui", "klikhier", "kliktutaj", "tutaj", "здесь", "тут",
    "こちら", "这里", "這裡", "여기",
];

/// What a URL says, lowercased and looked for as a substring.
const STRONG_URL_WORDS: &[&str] = &[
    "unsubscribe", "unsub", "optout", "opt-out", "opt_out", "/oo/", "signoff", "sign-off",
    "desabonn", "desinscri", "abmeld", "abbestell", "afmeld", "uitschrijv", "darsedebaja",
    "darse-de-baja", "descadastr", "disiscri", "wypisz", "otpis", "remove-me", "removeme",
    "remove_me", "/remove", "leave-list", "/leave",
];
const MEDIUM_URL_WORDS: &[&str] = &[
    "preference", "subscription", "/manage", "email-settings", "emailsettings", "notification",
];

/// Text as the word lists are matched: lowercased, letters and digits only.
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn has_strong(squashed: &str) -> bool {
    STRONG_WORDS.iter().any(|w| squashed.contains(w))
}

fn has_medium(squashed: &str) -> bool {
    MEDIUM_WORDS.iter().any(|w| squashed.contains(w))
}

fn is_generic(squashed: &str) -> bool {
    GENERIC_TEXTS.contains(&squashed)
}

fn url_score(url: &str) -> u8 {
    let l = url.to_ascii_lowercase();
    if STRONG_URL_WORDS.iter().any(|w| l.contains(w)) {
        STRONG_URL
    } else if MEDIUM_URL_WORDS.iter().any(|w| l.contains(w)) {
        MEDIUM
    } else {
        0
    }
}

/// Score a link by its text, its URL and the sentence it sits in.
fn link_score(text: &str, url: &str, context: &str) -> u8 {
    let text = squash(text);
    let ctx = squash(context);
    let mut score = 0u8;
    if has_strong(&text) {
        score = STRONG_TEXT;
    } else if is_generic(&text) && has_strong(&ctx) {
        score = CONTEXT;
    } else if has_medium(&text) {
        score = MEDIUM;
    } else if is_generic(&text) && has_medium(&ctx) {
        score = 1;
    }
    score.max(url_score(url))
}

/// Every anchor in an HTML body, scored.
fn scan_html(html: &str) -> BodyFindings {
    let mut out = BodyFindings::default();
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<a") {
        let start = pos + rel;
        let after = start + 2;
        // `<a ` or `<a>`, not `<abbr>`/`<address>`.
        if !lower[after..].starts_with(|c: char| c.is_whitespace() || c == '>') {
            pos = after;
            continue;
        }
        let Some(tag_end_rel) = lower[after..].find('>') else { break };
        let tag_end = after + tag_end_rel;
        let tag = &html[start..tag_end];
        let close = lower[tag_end + 1..].find("</a").map(|r| tag_end + 1 + r);
        let inner_end = close.unwrap_or_else(|| (tag_end + 1 + 400).min(html.len()));
        let inner_end = floor_char(html, inner_end);
        let inner = &html[tag_end + 1..inner_end];
        pos = inner_end;

        let Some(href) = attr(tag, "href").map(|h| decode_entities(&h)) else { continue };
        let href = href.trim().to_string();
        let mut text = tag_text(inner);
        for a in ["title", "aria-label"] {
            if let Some(v) = attr(tag, a) {
                text.push(' ');
                text.push_str(&decode_entities(&v));
            }
        }
        // The sentence around the link, for a "click here". Both sides
        // matter: "to unsubscribe, click here" and "click here to
        // unsubscribe" are equally common, and only reading backwards
        // missed every message written the second way. The trailing side
        // stops at the next link so one anchor never borrows the next
        // one's words.
        let ctx_start = floor_char(html, start.saturating_sub(600));
        let before = tag_text(&html[ctx_start..start]);
        let before: String = before.chars().rev().take(160).collect::<Vec<_>>().into_iter().rev().collect();
        let tail_from = match close {
            Some(c) => lower[c..].find('>').map(|r| c + r + 1).unwrap_or(inner_end),
            None => inner_end,
        };
        let tail_from = floor_char(html, tail_from);
        let tail_to = floor_char(html, (tail_from + 600).min(html.len()));
        let tail = &html[tail_from..tail_to];
        let tail = match lower[tail_from..tail_to].find("<a") {
            Some(next) => &tail[..next],
            None => tail,
        };
        let after: String = tag_text(tail).chars().take(160).collect();
        let context = format!("{before} {after}");

        let lower_href = href.to_ascii_lowercase();
        if lower_href.starts_with("mailto:") {
            let sq = squash(&format!("{text} {href}"));
            if (has_strong(&sq) || has_strong(&squash(&context)))
                && out.mailto.is_none()
                && parse_mailto(&href).is_some()
            {
                out.mailto = Some(href);
            }
        } else if lower_href.starts_with("http://") || lower_href.starts_with("https://") {
            let score = link_score(&text, &href, &context);
            out.offer_web(href, score);
        }
    }
    out
}

/// Back `i` off to a char boundary.
fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// An attribute's raw value out of a tag's text (`<a href="…" title='…'>`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let preceded = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        if !preceded {
            continue;
        }
        let rest = lower[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let value_at = tag.len() - rest.len();
        let rest_orig = tag[value_at..].trim_start();
        let value_at = tag.len() - rest_orig.len();
        let value = match rest_orig.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &tag[value_at + 1..];
                match body.find(q) {
                    Some(e) => &body[..e],
                    None => body,
                }
            }
            _ => {
                let e = rest_orig.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(rest_orig.len());
                &rest_orig[..e]
            }
        };
        return Some(value.to_string());
    }
    None
}

/// The text of a run of HTML: tags dropped (an image's alt text kept),
/// entities decoded, whitespace collapsed.
fn tag_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        out.push(' ');
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = &after[..gt];
        let lower = tag.trim_start().to_ascii_lowercase();
        if lower.starts_with("img") {
            if let Some(alt) = attr(tag, "alt").or_else(|| attr(tag, "title")) {
                out.push_str(&alt);
                out.push(' ');
            }
        }
        // A style or script block's content is not text.
        if lower.starts_with("style") || lower.starts_with("script") {
            let close = after[gt..].to_ascii_lowercase().find("</");
            rest = match close {
                Some(c) => &after[gt + c..],
                None => "",
            };
            continue;
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);
    let decoded = decode_entities(&out);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The handful of entities mail bodies use, decoded.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let name = &after[..semi];
        let replacement = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix('#')
                .and_then(|n| {
                    if let Some(h) = n.strip_prefix(['x', 'X']) {
                        u32::from_str_radix(h, 16).ok()
                    } else {
                        n.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Words in an instruction that mean "write to": "send an email to X".
const SEND_WORDS: &[&str] = &[
    "send", "email", "e-mail", "mail", "write", "message", "envía", "envia", "envoyer", "envoyez",
    "senden", "schreib", "invia", "stuur", "skicka", "wyślij", "отправ", "напиш",
];
/// Words in an instruction that mean "reply": "reply with STOP".
const REPLY_WORDS: &[&str] = &[
    "reply", "replying", "respond", "responding", "antworten", "antwort", "répondre", "répondez",
    "repondre", "responder", "responda", "rispondi", "rispondere", "antwoord", "svara", "odpowiedz",
    "ответ",
];
/// The words a reply is asked to carry, uppercased as a subject.
const REPLY_COMMANDS: &[&str] = &["unsubscribe", "stop", "remove", "signoff", "leave", "quit", "end"];

const TOKEN_BREAK: &[char] = &['<', '>', '"', '\'', '(', ')', '[', ']'];

/// A line that tells the reader to write back to leave, in words that never
/// say "unsubscribe": "Reply to this email with STOP in the subject line."
/// It needs a reply-or-send word and a command word the list named. The
/// ambiguous commands count only in capitals, as lists write them, so
/// "we stop sending at 5pm" and "reply to leave a comment" are not
/// instructions. Gives back the command, uppercased.
fn instruction_command(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let asks = REPLY_WORDS.iter().any(|w| lower.contains(w))
        || SEND_WORDS.iter().any(|w| lower.contains(w));
    if !asks {
        return None;
    }
    line.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .find(|token| {
            let l = token.to_lowercase();
            REPLY_COMMANDS.contains(&l.as_str())
                && (matches!(l.as_str(), "unsubscribe" | "signoff")
                    || token.chars().all(|c| !c.is_lowercase()))
        })
        .map(|t| t.to_uppercase())
}

/// A plain-text body: a URL on the line of (or the two lines after) an
/// unsubscribe word, "send an email to X", and "reply with UNSUBSCRIBE".
fn scan_text(text: &str, reply_addr: Option<&str>) -> BodyFindings {
    let mut out = BodyFindings::default();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let sq = squash(line);
        let asked = instruction_command(line);
        let strong = has_strong(&sq) || asked.is_some();
        let medium = has_medium(&sq);
        if !strong && !medium {
            continue;
        }
        let score = if strong { STRONG_TEXT } else { MEDIUM };
        let window = &lines[i.saturating_sub(1)..(i + 3).min(lines.len())];
        let joined = window.join(" ");
        let lower = joined.to_lowercase();
        // A web address near the word.
        for tok in joined.split(|c: char| c.is_whitespace() || TOKEN_BREAK.contains(&c)) {
            let tok = tok.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            let l = tok.to_ascii_lowercase();
            if l.starts_with("http://") || l.starts_with("https://") {
                out.offer_web(tok.to_string(), score.max(url_score(tok)));
            } else if l.starts_with("www.") {
                out.offer_web(format!("https://{tok}"), score.max(url_score(tok)));
            }
        }
        if !strong {
            continue;
        }
        // "reply with UNSUBSCRIBE": the reply route, with the word asked for.
        if out.reply.is_none() && REPLY_WORDS.iter().any(|w| lower.contains(w)) {
            if let Some(to) = reply_addr {
                let subject = asked.clone().unwrap_or_else(|| command_in(&lower));
                out.reply = Some(ReplyRoute { to: to.to_string(), subject });
            }
        }
        // "send an email to leave@list.example": the mailto route.
        if out.mailto.is_none() && SEND_WORDS.iter().any(|w| lower.contains(w)) {
            for tok in joined.split(|c: char| c.is_whitespace() || c == ',' || TOKEN_BREAK.contains(&c)) {
                let tok = tok.trim_end_matches(['.', ';', ':', '!', '?']).trim_start_matches("mailto:");
                if is_email(tok) {
                    let subject = asked.clone().unwrap_or_else(|| command_in(&lower));
                    out.mailto = Some(format!("mailto:{tok}?subject={subject}"));
                    break;
                }
            }
        }
    }
    out
}

/// The command word an instruction asks for, uppercased; UNSUBSCRIBE when
/// it names none.
fn command_in(lower: &str) -> String {
    REPLY_COMMANDS
        .iter()
        .find(|w| lower.contains(*w))
        .map(|w| w.to_uppercase())
        .unwrap_or_else(|| "UNSUBSCRIBE".to_string())
}

// ---------------------------------------------------------------------------
// mailto: and the one-click POST
// ---------------------------------------------------------------------------

/// Where a `mailto:` handle sends its unsubscribe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailtoTarget {
    /// Comma-separated addresses, as the composer takes them.
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Take a `mailto:` URI apart (RFC 6068): the addresses before `?`, then
/// `subject=`, `body=`, `to=` and `cc=` in the query, percent-decoded.
pub fn parse_mailto(uri: &str) -> Option<MailtoTarget> {
    let rest = strip_scheme(uri, "mailto:")?;
    let (addr_part, query) = match rest.split_once('?') {
        Some((a, q)) => (a, q),
        None => (rest, ""),
    };
    let mut to: Vec<String> = addr_part
        .split(',')
        .map(percent_decode)
        .map(|a| a.trim().to_string())
        .filter(|a| a.contains('@'))
        .collect();
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = v,
            "body" => body = v,
            "to" | "cc" => {
                to.extend(v.split(',').map(|a| a.trim().to_string()).filter(|a| a.contains('@')))
            }
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
    Some(MailtoTarget { to: to.join(", "), subject, body })
}

fn strip_scheme<'a>(uri: &'a str, scheme: &str) -> Option<&'a str> {
    let uri = uri.trim();
    (uri.len() >= scheme.len() && uri[..scheme.len()].eq_ignore_ascii_case(scheme))
        .then(|| &uri[scheme.len()..])
}

/// `%20` and `+` back to characters, for a mailto query.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16)
            {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Send the RFC 8058 one-click request: a POST of `List-Unsubscribe=One-Click`
/// to the list's handle. Blocking; run it off the UI thread. Any 2xx
/// answer means the list took the request.
pub fn one_click_post(url: &str) -> Result<(), String> {
    let l = url.to_ascii_lowercase();
    if !l.starts_with("https://") && !l.starts_with("http://") {
        return Err("the list's unsubscribe handle is not a web address".to_string());
    }
    match ureq::post(url)
        .set("User-Agent", crate::logo::USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .send_form(&[("List-Unsubscribe", "One-Click")])
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(format!("the list's server answered {code}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example?subject=unsubscribe"));
        assert_eq!(u.web.as_deref(), Some("https://list.example/u/1"));
        assert_eq!(u.one_click, None, "no List-Unsubscribe-Post, no one-click promise");
        assert!(u.list_id.is_empty());
        assert!(u.in_headers);
    }

    #[test]
    fn one_click_needs_the_post_header() {
        let hdr = s(&["<https://list.example/u/1>, <mailto:leave@list.example>"]);
        let u = from_headers(&hdr, Some("List-Unsubscribe=One-Click"), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/1"));
        let u = from_headers(&hdr, Some(" \"list-unsubscribe = one-click\" "), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/1"), "case, space and quotes are forgiven");
        let u = from_headers(&hdr, Some("something-else"), None).unwrap();
        assert_eq!(u.one_click, None);
        let u = from_headers(&s(&["<http://list.example/u/1>"]), Some("List-Unsubscribe=One-Click"), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("http://list.example/u/1"), "http gets the POST too");
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
        assert_eq!(unfold("<mailto:a@b.c>,\r\n <https://x.y/z>"), "<mailto:a@b.c>, <https://x.y/z>");
    }

    #[test]
    fn bare_handles_without_brackets_or_schemes_are_taken_too() {
        // ArtStation, through Amazon SES: the URL alone, no brackets.
        let hdr = s(&["https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish"]);
        let u = from_headers(&hdr, None, None).unwrap();
        assert_eq!(u.web.as_deref(), Some("https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish"));
        assert!(!u.direct(), "a page only: the button opens the browser");
        let u = from_headers(&s(&["MAILTO:leave@x.example, https://x.example/u"]), None, None).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("MAILTO:leave@x.example"));
        assert_eq!(u.web.as_deref(), Some("https://x.example/u"));
        // No scheme at all: an address, or a www. host.
        let u = from_headers(&s(&["<leave@x.example>, <www.x.example/unsub>"]), None, None).unwrap();
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
        assert_eq!(from_headers(&s(&["<ftp://odd.example/x>"]), None, None), None);
        let plain = message("", "text/plain", "Hi, lunch on Saturday? I unsubscribed from that gym, by the way.");
        assert_eq!(detect_raw(&plain), None, "the word alone, with no link, is not a route");
    }

    #[test]
    fn mailto_targets_are_taken_apart() {
        let t = parse_mailto("mailto:leave@list.example?subject=unsubscribe%20me&body=please+go").unwrap();
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
        assert_eq!(parse_mailto("mailto:?subject=x"), None, "no address, no target");
        assert_eq!(parse_mailto("https://x.example"), None);
    }

    #[test]
    fn vendor_aliases_and_part_headers_count() {
        let raw = message("X-Unsubscribe-Web: https://x.example/leave/1\r\n", "text/plain", "hi");
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/leave/1"));
        let raw = b"From: a@b.c\r\nContent-Type: multipart/alternative; boundary=B\r\n\r\n--B\r\n\
Content-Type: text/plain\r\nList-Unsubscribe: <mailto:leave@x.example>\r\n\r\nhi\r\n--B--\r\n";
        assert_eq!(detect_raw(raw).unwrap().mailto.as_deref(), Some("mailto:leave@x.example"));
    }

    #[test]
    fn the_demo_newsletter_offers_one_click_and_mail() {
        let raw = crate::backend::demo_raw(10).expect("the demo newsletter has headers");
        let u = detect_raw(raw.as_bytes()).expect("handles");
        assert!(u.one_click.is_some() && u.mailto.is_some() && u.direct());
        assert_eq!(u.list_id, "digest.this-week-in-rust.org");
        assert_eq!(crate::verify::check_sender(raw.as_bytes()).trust, crate::models::SenderTrust::Pass);
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
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example?subject=unsubscribe"));
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
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://esp.example/list-manage/unsubscribe?u=1&id=2"));
        let raw = message("", "text/html", "<a href=\"https://esp.example/optout/9\">Ok</a>");
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://esp.example/optout/9"));
    }

    #[test]
    fn click_here_takes_its_sentence_into_account() {
        let raw = message(
            "",
            "text/html",
            "<td>To stop receiving these emails, please <a href=\"https://x.example/r/1\">click here</a>.</td>",
        );
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/r/1"));
        // The same sentence with a generic link elsewhere: not this link.
        let raw = message("", "text/html", "<p>Read the story <a href=\"https://x.example/story\">here</a>.</p>");
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
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/u/d/abc"));
        // A generic link must not borrow the next link's words.
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/story\">here</a> · <a href=\"https://x.example/bye\">Unsubscribe</a>",
        );
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/bye"));
    }

    #[test]
    fn the_strongest_link_wins_over_a_preferences_page() {
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/prefs\">Manage your preferences</a> · \
             <a href=\"https://x.example/bye\">Unsubscribe</a>",
        );
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/bye"));
        let raw = message("", "text/html", "<a href=\"https://x.example/prefs\">Email preferences</a>");
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/prefs"), "a preferences page still counts when it is all there is");
    }

    #[test]
    fn image_links_and_mailto_links_in_the_body_count() {
        let raw = message(
            "",
            "text/html",
            "<a href=\"https://x.example/u/7\"><img src=\"cid:btn\" alt=\"Unsubscribe\"></a>",
        );
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/u/7"));
        let raw = message(
            "",
            "text/html",
            "<a href=\"mailto:leave@x.example?subject=Unsubscribe\">Unsubscribe</a>",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@x.example?subject=Unsubscribe"));
        assert!(u.direct());
    }

    #[test]
    fn other_languages_are_understood() {
        for text in ["Abmelden", "Se désabonner", "Darse de baja", "Cancelar subscrição", "Disiscriviti", "Uitschrijven", "Avregistrera", "Wypisz się", "Отписаться", "配信停止", "取消订阅", "구독 취소", "Abonelikten çık"] {
            let raw = message("", "text/html; charset=utf-8", &format!("<a href=\"https://x.example/l\">{text}</a>"));
            assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/l"), "{text}");
        }
    }

    #[test]
    fn plain_text_instructions_are_read() {
        let raw = message("", "text/plain", "Thanks for reading.\n\nTo unsubscribe, visit:\nhttps://x.example/unsub/abc\n");
        assert_eq!(detect_raw(&raw).unwrap().web.as_deref(), Some("https://x.example/unsub/abc"));

        let raw = message("", "text/plain", "To unsubscribe, send an email to leave@x.example with the subject UNSUBSCRIBE.");
        let u = detect_raw(&raw).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@x.example?subject=UNSUBSCRIBE"));

        let raw = message(
            "Reply-To: bounce@x.example\r\n",
            "text/plain",
            "Don't want these? Reply to this email with STOP in the subject line.",
        );
        let u = detect_raw(&raw).unwrap();
        assert_eq!(u.reply, Some(ReplyRoute { to: "bounce@x.example".into(), subject: "STOP".into() }));
        assert!(u.direct());

        let raw = message("", "text/plain", "Just reply with the word unsubscribe and we'll take you off.");
        assert_eq!(detect_raw(&raw).unwrap().reply.unwrap().to, "digest@list.example", "From when there is no Reply-To");
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
        let raw = message("Subject: This week\r\n", "text/plain", "To stop these, reply with UNSUBSCRIBE.");
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
        let raw = message("", "text/plain", "Reply with STOP and we will take you off.");
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
        assert_eq!(u.web.as_deref(), Some("https://x.example/page"), "the header's page is kept");
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@x.example"), "the body's mailto is added");
        assert!(u.in_headers && u.direct());
    }

    #[test]
    fn entities_and_attributes_are_read() {
        assert_eq!(decode_entities("a &amp; b &#39;c&#x27; &nbsp;d &unknown; e"), "a & b 'c'  d &unknown; e");
        assert_eq!(attr("<a class=x href='https://a/b' title=\"T\">", "href").as_deref(), Some("https://a/b"));
        assert_eq!(attr("<a data-href=\"no\" href=https://a/c>", "href").as_deref(), Some("https://a/c"));
        assert_eq!(tag_text("Un<b>sub</b>scribe <img alt=\"now\"> <style>p{}</style>x"), "Un sub scribe now x");
    }

    /// Survey a directory of saved messages: `HYLKI_UNSUB_SURVEY=<dir>
    /// cargo test --bin hylki unsubscribe::tests::survey_dir -- --ignored
    /// --nocapture` reports what was found for each, and names every
    /// message that says "unsubscribe" somewhere but gave up no route —
    /// the ones that slipped through.
    #[test]
    #[ignore]
    fn survey_dir() {
        let Ok(dir) = std::env::var("HYLKI_UNSUB_SURVEY") else { return };
        let (mut total, mut hdr, mut body_only, mut none, mut missed) = (0, 0, 0, 0, 0);
        let mut entries: Vec<_> = std::fs::read_dir(&dir).expect("dir").filter_map(|e| e.ok()).collect();
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
                    eprintln!("{:<9} {:<10} {from}", route, if u.in_headers { "header" } else { "body" });
                }
                None => {
                    none += 1;
                    // Did we miss one? Only counts if the word is really there.
                    let text: String = (0..parsed.as_ref().map_or(0, |p| p.text_body_count()))
                        .filter_map(|i| parsed.as_ref().and_then(|p| p.body_text(i)).map(|t| t.to_string()))
                        .chain(
                            (0..parsed.as_ref().map_or(0, |p| p.html_body_count()))
                                .filter_map(|i| parsed.as_ref().and_then(|p| p.body_html(i)).map(|t| t.to_string())),
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
        let Ok(path) = std::env::var("HYLKI_UNSUB_PROBE") else { return };
        let raw = std::fs::read(&path).expect("readable file");
        let check = crate::verify::check_sender(&raw);
        eprintln!("{path}: trust {:?}, unsubscribe {:#?}", check.trust, detect_raw(&raw));
    }
}
