//! The demo mailbox: the sample messages, their raw headers, and the
//! message that arrives while the demo runs.

use crate::backend::MockBackend;
use crate::models::Message;

/// Raw header blocks for the demo messages that come from a mailing list:
/// run through the real sender check when the body is served, so the
/// newsletter's card wears a verdict and the Unsubscribe banner exactly as
/// a fetched message would. Keyed by message id.
const DEMO_HEADERS: &[(u32, &str, &str)] = &[
    // No List-Unsubscribe at all: the route is the link in the footer,
    // which is how a great deal of real bulk mail is written.
    (
        7,
        "From: GNOME Foundation <news@gnome.org>\r\n\
         Authentication-Results: mx.hylki.local; dkim=pass header.d=gnome.org; \
         spf=pass smtp.mailfrom=gnome.org; dmarc=pass header.from=gnome.org\r\n\
         Subject: GNOME 49 release candidate is here\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "",
    ),
    (
    10,
    "From: Rust Weekly <digest@this-week-in-rust.org>\r\n\
     Authentication-Results: mx.hylki.local; dkim=pass header.d=this-week-in-rust.org; \
     spf=pass smtp.mailfrom=this-week-in-rust.org; dmarc=pass header.from=this-week-in-rust.org\r\n\
     List-Id: This Week in Rust <digest.this-week-in-rust.org>\r\n\
     List-Unsubscribe: <mailto:leave@this-week-in-rust.org?subject=unsubscribe>, \
     <https://this-week-in-rust.org/unsubscribe/612/abc>\r\n\
     List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
     Subject: This Week in Rust #612\r\n\r\n",
        "",
    ),
    // A sensitivity label (#411), as Outlook writes one for a tenant that
    // uses Microsoft Purview: the reader shows it as a pill.
    (
        23,
        "From: Sophie Turner <sophie@studio.dev>\r\n\
         Subject: Q3 roadmap review\r\n\
         msip_labels: MSIP_Label_9c2a4f1e-5b3d-4c7a-8e21-0d6f3b9a1c42_Enabled=true;\r\n \
         MSIP_Label_9c2a4f1e-5b3d-4c7a-8e21-0d6f3b9a1c42_Method=Standard;\r\n \
         MSIP_Label_9c2a4f1e-5b3d-4c7a-8e21-0d6f3b9a1c42_Name=Confidential\\Internal Use;\r\n \
         MSIP_Label_9c2a4f1e-5b3d-4c7a-8e21-0d6f3b9a1c42_SiteId=2b7c5e90-1f4d-4a6b-9c3e-8d1a0f2e6b57\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "",
    ),
    // A meeting request, as a calendar sends one: the readable half that
    // every client shows, and the `text/calendar` part that is the meeting
    // (#223). The event floats — no TZID — so it reads 2:00 PM wherever the
    // demo runs.
    (
        13,
        "From: Calendar <calendar@hylki.hyprlab.co>\r\n\
         Subject: Invitation: Architecture sync @ Thu 2:00 PM\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"hylki-demo-invite\"\r\n\r\n\
         --hylki-demo-invite\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "\r\n--hylki-demo-invite\r\n\
         Content-Type: text/calendar; charset=utf-8; method=REQUEST; name=invite.ics\r\n\r\n\
         BEGIN:VCALENDAR\r\n\
         PRODID:-//Studio//Calendar//EN\r\n\
         VERSION:2.0\r\n\
         METHOD:REQUEST\r\n\
         BEGIN:VEVENT\r\n\
         UID:demo-architecture-sync@hylki.hyprlab.co\r\n\
         SEQUENCE:0\r\n\
         SUMMARY:Architecture sync\r\n\
         LOCATION:Conference Room B / video link\r\n\
         DTSTART:20261105T140000\r\n\
         DTEND:20261105T150000\r\n\
         RRULE:FREQ=WEEKLY\r\n\
         ORGANIZER;CN=Priya Sharma:mailto:priya@studio.dev\r\n\
         ATTENDEE;CN=Jason M.;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:jason@hylki.hyprlab.co\r\n\
         ATTENDEE;CN=Marcus Chen;PARTSTAT=ACCEPTED:mailto:marcus@studio.dev\r\n\
         ATTENDEE;CN=Sophie Turner;PARTSTAT=NEEDS-ACTION:mailto:sophie@studio.dev\r\n\
         ATTENDEE;CUTYPE=ROOM;CN=Conference Room B:mailto:room-b@studio.dev\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n\
         --hylki-demo-invite--\r\n",
    ),
];

/// A demo message as raw RFC 822: its header block (see [`DEMO_HEADERS`])
/// followed by its body, so the real sender check and the real unsubscribe
/// scan can be run over it exactly as they are over fetched mail.
pub(crate) fn demo_raw(id: u32) -> Option<String> {
    let (headers, tail) = DEMO_HEADERS
        .iter()
        .find(|(i, _, _)| *i == id)
        .map(|(_, h, t)| (*h, *t))?;
    let body = MockBackend::new()
        .message(id)
        .map(|m| m.body)
        .unwrap_or_default();
    Some(format!("{headers}{body}{tail}"))
}

/// Compact spec for a sample message; expanded into a [`Message`] by [`build`].
struct Spec {
    id: u32,
    account_id: u32,
    folder_id: u32,
    from_name: &'static str,
    from_addr: &'static str,
    to: &'static str,
    subject: &'static str,
    preview: &'static str,
    body: &'static str,
    date: &'static str,
    unread: bool,
    starred: bool,
    /// Tag keywords the demo message carries (see the showcase's tags.toml).
    keywords: &'static [&'static str],
    has_attachment: bool,
    /// A parent message id when this is a reply (drives conversation threading).
    in_reply_to: Option<u32>,
}

fn build(s: &Spec) -> Message {
    Message {
        id: s.id,
        account_id: s.account_id,
        folder_id: s.folder_id,
        uid: s.id,
        // Newer id → more recent. Spaced an hour apart from a fixed base so the
        // demo is deterministic (no wall-clock dependency).
        timestamp: 1_760_000_000 - (s.id as i64) * 3600,
        from_name: s.from_name.into(),
        from_addr: s.from_addr.into(),
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: s.to.into(),
        cc: String::new(),
        bcc: String::new(),
        subject: s.subject.into(),
        preview: s.preview.into(),
        body: s.body.into(),
        date: s.date.into(),
        unread: s.unread,
        starred: s.starred,
        answered: false,
        forwarded: false,
        keywords: s.keywords.iter().map(|k| k.to_string()).collect(),
        has_attachment: s.has_attachment,
        message_id: format!("<demo-{}@hylki.local>", s.id),
        references: s
            .in_reply_to
            .map(|p| format!("<demo-{p}@hylki.local>"))
            .unwrap_or_default(),
        importance: Default::default(),
        due: 0,
        labels: demo_labels(s.id),
    }
}

/// Gmail labels on some of the demo's mail, for captures of their pills
/// (#374). Only with HYLKI_DEMO_LABELS set: the demo the README shows has
/// none.
fn demo_labels(id: u32) -> Vec<String> {
    if std::env::var_os("HYLKI_DEMO_LABELS").is_none() {
        return Vec::new();
    }
    match id % 4 {
        0 => vec!["Clients/Acme".into(), "Travel".into()],
        1 => vec!["Receipts".into()],
        2 => vec!["Newsletters".into(), "Family".into()],
        _ => Vec::new(),
    }
}

/// A late reply into the demo's deep conversation (account 1's Inbox):
/// what a sync brings in while that thread is open. `HYLKI_DEMO_ARRIVAL=<s>`
/// has the mock worker deliver it after that many seconds.
pub(crate) fn demo_arrival() -> Message {
    let mut m = build(&Spec {
        id: 90,
        account_id: 1,
        folder_id: 1,
        from_name: "Priya Sharma",
        from_addr: "priya@studio.dev",
        to: "jason@hylki.hyprlab.co",
        subject: "Re: Reader redesign: final review",
        preview: "One more thing: the dark-mode cards need a hair more contrast on the sender line…",
        body: "One more thing: the dark-mode cards need a hair more contrast on the sender line. I've pushed a tweak to the branch.\n\nPriya",
        date: "Just now",
        unread: true,
        starred: false,
        keywords: &[],
        has_attachment: false,
        in_reply_to: Some(6),
    });
    // Newer than anything the demo ships with.
    m.timestamp = 1_760_000_000 + 3600;
    m
}

pub(super) fn sample_messages() -> Vec<Message> {
    const ME: &str = "jason@hylki.hyprlab.co";
    const LAB: &str = "hello@hyprlab.dev";
    const PERSONAL: &str = "jason.m@fastmail.com";
    let specs = [
        // ---- Account 1 · Inbox · a deep conversation (ids 1–6, oldest = 6) ----
        // Timestamps derive from the id (smaller id = newer), so the thread owns
        // the smallest ids to sit newest at the top of the inbox.
        Spec { id: 6, account_id: 1, folder_id: 1, from_name: "Sophie Turner", from_addr: "sophie@studio.dev", to: ME,
            subject: "Reader redesign: final review",
            preview: "Before we tag the release I'd like one last pass over the reading pane. I've collected everything into…",
            body: "<p>Hi all,</p><p>Before we tag the release I'd like one last pass over the reading pane. I've collected everything into <a href=\"https://example.com/review-board\">the review board</a>:</p><ul><li>Full-width conversation layout</li><li>Card action palettes</li><li>The new accent selection in the list</li></ul><p>Comments by Thursday, please!</p><p>Sophie</p>",
            date: "8:12 AM", unread: false, starred: false, keywords: &["Work"], has_attachment: false, in_reply_to: None },
        Spec { id: 5, account_id: 1, folder_id: 1, from_name: "Marcus Chen", from_addr: "marcus@studio.dev", to: ME,
            subject: "Re: Reader redesign: final review",
            preview: "Went through the board this morning. The full-width layout is a clear win — one note on plain-text mail…",
            body: "Went through the board this morning. The full-width layout is a clear win — one note on plain-text mail: the header should sit on the same ground as the body so it reads as one surface.\n\nMarcus",
            date: "8:31 AM", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(6) },
        Spec { id: 4, account_id: 1, folder_id: 1, from_name: "Jason M.", from_addr: ME, to: "sophie@studio.dev",
            subject: "Re: Reader redesign: final review",
            preview: "Agreed on the continuous surface for plain mail — that's in. I also merged the thread chip work so the…",
            body: "Agreed on the continuous surface for plain mail — that's in.\n\nI also merged the thread chip work so the count and caret are one quiet pill now, and selection uses the system accent with white text in both schemes.\n\nJason",
            date: "8:56 AM", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(5) },
        Spec { id: 3, account_id: 1, folder_id: 1, from_name: "Priya Sharma", from_addr: "priya@studio.dev", to: ME,
            subject: "Re: Reader redesign: final review",
            preview: "Tested on a 13\" laptop — the narrower message list is great. Attaching two screens of the hover palette…",
            body: "Tested on a 13\" laptop — the narrower message list is great and nothing overflows.\n\nAttaching two screens of the hover palette so we can compare the reserved-space fade against the old layout shift.\n\nPriya",
            date: "9:20 AM", unread: false, starred: false, keywords: &["Work", "To_Do"], has_attachment: true, in_reply_to: Some(4) },
        Spec { id: 2, account_id: 1, folder_id: 1, from_name: "Sophie Turner", from_addr: "sophie@studio.dev", to: ME,
            subject: "Re: Reader redesign: final review",
            preview: "Love it. Last open question from me: should the card actions default to hover-reveal or always visible…",
            body: "Love it. Last open question from me: should the card actions default to hover-reveal or always visible? Preferences has all three modes, we just need a default for new installs.\n\nSophie",
            date: "9:41 AM", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(3) },
        Spec { id: 1, account_id: 1, folder_id: 1, from_name: "Marcus Chen", from_addr: "marcus@studio.dev", to: ME,
            subject: "Re: Reader redesign: final review",
            preview: "Hover-reveal gets my vote — it keeps the cards calm and the fade is lovely. Ship it 🚀",
            body: "Hover-reveal gets my vote — it keeps the cards calm and the fade is lovely.\n\nShip it 🚀\n\nMarcus",
            date: "10:24 AM", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(2) },
        // ---- Account 1 · Inbox ----
        Spec { id: 7, account_id: 1, folder_id: 1, from_name: "GNOME Foundation", from_addr: "news@gnome.org", to: ME,
            subject: "GNOME 49 release candidate is here",
            preview: "The release candidate for GNOME 49 is now available for testing. This cycle brings major performance work…",
            body: "Hi Jason,\n\nThe release candidate for GNOME 49 is now available for testing. This cycle brings major performance work across the shell and a refreshed libadwaita with new adaptive widgets.\n\nHighlights:\n  • Faster startup and lower memory use\n  • New AdwMultiLayoutView for responsive layouts\n  • Improved Wayland fractional scaling\n\nPlease help us test and file issues before the final release.\n\n— The GNOME Release Team\n\n---\nYou are receiving this because you signed up for GNOME news.\nTo unsubscribe, visit https://mail.gnome.org/list/news/unsubscribe/j7Kq2",
            date: "9:42 AM", unread: true, starred: true, keywords: &["Personal"], has_attachment: false, in_reply_to: None },
        // ---- Account 1 · Inbox · the Q3 roadmap thread (ids 23 → 8, oldest = 23).
        // Sophie's original is the oldest (id 23, yesterday); the newest reply
        // keeps id 8 so the thread sits where it always did in the inbox. ----
        Spec { id: 8, account_id: 1, folder_id: 1, from_name: "Sophie Turner", from_addr: "sophie@studio.dev", to: ME,
            subject: "Re: Q3 roadmap review",
            preview: "Perfect, updated the doc with the parallel track and Priya's staging step. Thursday's session is now a…",
            body: "Perfect, updated the doc with the parallel track and Priya's staging step. Thursday's session is now a sign-off rather than a review, so I've cut it to 30 minutes.\n\nLast call for comments before then!\n\nSophie",
            date: "9:53 AM", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(9) },
        Spec { id: 9, account_id: 1, folder_id: 1, from_name: "Priya Sharma", from_addr: "priya@studio.dev", to: ME,
            subject: "Re: Q3 roadmap review",
            preview: "One thing from QA: if steps one and two run in parallel we need the staging environment a week earlier than…",
            body: "One thing from QA: if steps one and two run in parallel we need the staging environment a week earlier than the doc says. I've asked Marcus whether ops can do it.\n\nOtherwise the sequencing looks right to me.\n\nPriya",
            date: "8:53 AM", unread: false, starred: false, keywords: &["To_Do"], has_attachment: false, in_reply_to: Some(21) },
        Spec { id: 21, account_id: 1, folder_id: 1, from_name: "Marcus Chen", from_addr: "marcus@studio.dev", to: ME,
            subject: "Re: Q3 roadmap review",
            preview: "+1 on parallelising. I mocked up the timeline both ways (attached). The compressed version lands the migration…",
            body: "+1 on parallelising. I mocked up the timeline both ways (attached). The compressed version lands the migration on the 14th with a week of slack before the release freeze.\n\nMarcus",
            date: "Yesterday", unread: false, starred: false, keywords: &[], has_attachment: true, in_reply_to: Some(22) },
        Spec { id: 22, account_id: 1, folder_id: 1, from_name: "Jason M.", from_addr: ME, to: "sophie@studio.dev",
            subject: "Re: Q3 roadmap review",
            preview: "Looks great overall. I left comments on the migration phase — I think we can parallelize the first two steps…",
            body: "Looks great overall. I left comments on the migration phase — I think we can parallelize the first two steps and pull the whole thing in by a week. Happy to walk through it tomorrow at 10.\n\nJason",
            date: "Yesterday", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: Some(23) },
        Spec { id: 23, account_id: 1, folder_id: 1, from_name: "Sophie Turner", from_addr: "sophie@studio.dev", to: ME,
            subject: "Q3 roadmap review",
            preview: "Sharing the draft roadmap ahead of Thursday. The migration phase is the big open question — see the timeline…",
            body: "Hi Jason,\n\nSharing the draft roadmap ahead of Thursday. The migration phase is the big open question — see the timeline in the doc and let me know if the sequencing works.\n\nThanks,\nSophie",
            date: "Yesterday", unread: false, starred: false, keywords: &["Work"], has_attachment: true, in_reply_to: None },
        Spec { id: 10, account_id: 1, folder_id: 1, from_name: "Rust Weekly", from_addr: "digest@this-week-in-rust.org", to: ME,
            subject: "This Week in Rust #612",
            preview: "Crate of the week, RFCs, and community updates. This issue: async closures stabilize, and a deep dive into…",
            // A designed newsletter, as they come: a preheader hidden from
            // the message, a layout table with its own colors and fonts, a
            // button, a tracking pixel. The Reader View demo strips it to
            // the words.
            body: "<html><head><style>body{margin:0;background:#f4f1ea;font-family:Georgia,serif}.btn{background:#c0392b;color:#fff!important;padding:10px 18px;border-radius:4px;text-decoration:none;display:inline-block}</style></head>\
                   <body><div style=\"display:none;max-height:0;overflow:hidden;font-size:1px;color:#f4f1ea\">Async closures land on stable, zero-copy parsing, 14 crates \u{200b}\u{200b}\u{200b}</div>\
                   <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"#f4f1ea\"><tr><td align=\"center\" style=\"padding:24px 12px\">\
                   <table width=\"560\" cellpadding=\"0\" cellspacing=\"0\" bgcolor=\"#ffffff\" style=\"border:1px solid #e0d9c8\">\
                   <tr><td bgcolor=\"#2c3e50\" style=\"padding:22px 28px;color:#ecf0f1;font-size:26px;font-weight:bold;letter-spacing:1px\">This Week in Rust <span style=\"color:#f39c12\">#612</span></td></tr>\
                   <tr><td style=\"padding:24px 28px 8px;font-size:15px;line-height:1.5;color:#333\"><p style=\"margin:0 0 14px\">Welcome to another issue of <b>This Week in Rust</b>!</p>\
                   <p style=\"margin:0 0 14px\">This week: async closures stabilize on stable, a deep dive into zero-copy parsing, and 14 new crates worth your attention.</p></td></tr>\
                   <tr><td style=\"padding:0 28px\"><table cellpadding=\"0\" cellspacing=\"0\" width=\"100%\"><tr>\
                   <td width=\"50%\" valign=\"top\" style=\"padding:12px;background:#fdf6e3;font-size:14px;color:#333\"><h3 style=\"margin:0 0 6px;color:#c0392b;font-size:16px\">Crate of the week</h3>zerocopy-parse: parse without copying a byte. <a href=\"https://example.com/crate\" style=\"color:#c0392b\">Read more</a></td>\
                   <td width=\"50%\" valign=\"top\" style=\"padding:12px;background:#eef5fb;font-size:14px;color:#333\"><h3 style=\"margin:0 0 6px;color:#2980b9;font-size:16px\">RFCs</h3>Three RFCs entered final comment period this week, including the one on <i>async closures</i>.</td>\
                   </tr></table></td></tr>\
                   <tr><td style=\"padding:20px 28px;font-size:15px;color:#333\"><table cellpadding=\"0\" cellspacing=\"0\"><tr><th align=\"left\" style=\"padding:4px 12px 4px 0;border-bottom:2px solid #2c3e50\">Crate</th><th align=\"left\" style=\"padding:4px 0;border-bottom:2px solid #2c3e50\">Downloads</th></tr>\
                   <tr><td style=\"padding:4px 12px 4px 0\">zerocopy-parse</td><td style=\"padding:4px 0\">41,200</td></tr><tr><td style=\"padding:4px 12px 4px 0\">tokio-tap</td><td style=\"padding:4px 0\">12,844</td></tr></table></td></tr>\
                   <tr><td align=\"center\" style=\"padding:8px 28px 28px\"><a class=\"btn\" href=\"https://example.com/digest/612\">Read the full digest</a></td></tr>\
                   <tr><td bgcolor=\"#ecf0f1\" style=\"padding:14px 28px;font-size:12px;color:#7f8c8d\">You are receiving this because you subscribed. <a href=\"https://example.com/unsubscribe\" style=\"color:#7f8c8d\">Unsubscribe</a></td></tr>\
                   </table></td></tr></table><img src=\"https://example.com/open.gif\" width=\"1\" height=\"1\" alt=\"\"></body></html>",
            date: "Yesterday", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 11, account_id: 1, folder_id: 1, from_name: "Apple", from_addr: "no-reply@apple.com", to: ME,
            subject: "Your receipt from Apple",
            preview: "Thank you for your purchase. Your receipt is attached. Order ID W1234567890…",
            body: "Thank you for your purchase.\n\nOrder ID: W1234567890\niCloud+ 2TB — $9.99\n\nYour receipt is attached.",
            date: "Yesterday", unread: false, starred: false, keywords: &[], has_attachment: true, in_reply_to: None },
        Spec { id: 12, account_id: 1, folder_id: 1, from_name: "Marcus Chen", from_addr: "marcus@studio.dev", to: ME,
            subject: "Design tokens are merged 🎨",
            preview: "The design token pipeline is finally merged into main. Dark mode now derives entirely from the token set…",
            body: "Hey,\n\nThe design token pipeline is finally merged into main. Dark mode now derives entirely from the token set, so we no longer maintain two stylesheets. Pull main when you get a chance.\n\nMarcus",
            date: "Wed", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        // The readable half of a meeting request; the calendar part that
        // goes with it is in DEMO_HEADERS (#223).
        Spec { id: 13, account_id: 1, folder_id: 1, from_name: "Calendar", from_addr: "calendar@hylki.hyprlab.co", to: ME,
            subject: "Invitation: Architecture sync @ Thu 2:00 PM",
            preview: "You have been invited to Architecture sync. Thursday 2:00 PM – 3:00 PM. Conference Room B / video link…",
            body: "You have been invited to: Architecture sync\n\nWhen: Thursday 2:00 PM – 3:00 PM\nWhere: Conference Room B / video link\nOrganiser: Priya Sharma\n\nAccept · Decline · Maybe",
            date: "Wed", unread: false, starred: false, keywords: &[], has_attachment: true, in_reply_to: None },
        Spec { id: 14, account_id: 1, folder_id: 1, from_name: "Emma Wright", from_addr: "emma@example.com", to: ME,
            subject: "Lunch this weekend?",
            preview: "It's been ages! Are you free Saturday for lunch at that new place downtown? Let me know what works…",
            body: "Hey stranger!\n\nIt's been ages! Are you free Saturday for lunch at that new place downtown? Let me know what works.\n\nxo Emma",
            date: "Tue", unread: false, starred: true, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 15, account_id: 1, folder_id: 1, from_name: "Linear", from_addr: "notifications@linear.app", to: ME,
            subject: "3 issues assigned to you this sprint",
            preview: "VIREO-142 Reader dark mode, VIREO-148 Infinite scroll spinner, VIREO-151 OAuth for Microsoft…",
            body: "You have 3 issues in the current sprint:\n\n  • VIREO-142  Reader dark mode\n  • VIREO-148  Infinite scroll spinner\n  • VIREO-151  OAuth for Microsoft\n\nOpen in Linear to update status.",
            date: "Tue", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 16, account_id: 1, folder_id: 1, from_name: "Framer", from_addr: "team@framer.com", to: ME,
            subject: "Your weekly site analytics",
            preview: "hylki.hyprlab.co had 4,218 visitors this week, up 32%. Top page: /download. See the full breakdown…",
            body: "hylki.hyprlab.co — weekly summary\n\nVisitors: 4,218 (+32%)\nTop page: /download\nAvg. time on page: 1m 47s\n\nView the full report online.",
            date: "Mon", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        // ---- Account 1 · Drafts ----
        Spec { id: 20, account_id: 1, folder_id: 4, from_name: "Jason M.", from_addr: ME, to: "team@hylki.hyprlab.co",
            subject: "Release notes for 0.2",
            preview: "Draft — Highlights for the next build: actions palette, message-content theme, infinite scroll…",
            body: "Draft.\n\nHighlights for 0.2:\n  • actions palette with slide-in animation\n  • Per-message light/dark content theme\n  • Infinite scroll for large folders\n\nTODO: add screenshots.",
            date: "Mon", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        // ---- Account 2 · Inbox ----
        Spec { id: 30, account_id: 2, folder_id: 11, from_name: "Proton", from_addr: "security@proton.me", to: LAB,
            subject: "New sign-in to your account",
            preview: "We noticed a new sign-in from Fedora Linux · Firefox. If this was you, no action is needed…",
            body: "We noticed a new sign-in to your Proton account.\n\nDevice: Fedora Linux · Firefox\nLocation: —\n\nIf this was you, no action is needed.",
            date: "10:11 AM", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 31, account_id: 2, folder_id: 11, from_name: "Buy Me a Coffee", from_addr: "no-reply@buymeacoffee.com", to: LAB,
            subject: "You have a new supporter ☕",
            preview: "Alex bought you a coffee and left a note: “Love Hylki — the GNOME-native mail client I've wanted for years!”",
            body: "Good news!\n\nAlex bought you a coffee and left a note:\n\n  “Love Hylki — the GNOME-native mail client I've wanted for years!”\n\nSay thanks from your dashboard.",
            date: "Yesterday", unread: true, starred: true, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 32, account_id: 2, folder_id: 11, from_name: "GitHub", from_addr: "notifications@github.com", to: LAB,
            subject: "[hyprlab/hylki] Star milestone: 1,000 ⭐",
            preview: "Your repository hyprlab/hylki just reached 1,000 stars. Nice work! See who starred recently…",
            body: "hyprlab/hylki just reached 1,000 stars 🎉\n\nRecent stargazers: @ada, @torvalds-fan, @rustacean…\n\nView on GitHub.",
            date: "Yesterday", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 33, account_id: 2, folder_id: 11, from_name: "This Week in GNOME", from_addr: "hello@thisweek.gnome.org", to: LAB,
            subject: "This Week in GNOME #180",
            preview: "Highlights from across the GNOME project: new adaptive widgets, shell performance work, and a wave of app releases…",
            body: "This Week in GNOME\n\nHighlights this week:\n  • New adaptive widgets in libadwaita\n  • Shell performance improvements\n  • A fresh wave of app releases\n\nRead the full issue online.",
            date: "Fri", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        // ---- Account 3 · Inbox ----
        Spec { id: 40, account_id: 3, folder_id: 21, from_name: "Elena Ruiz", from_addr: "elena.ruiz@gmail.com", to: PERSONAL,
            subject: "Cabin weekend — final headcount?",
            preview: "Locking the booking tonight! Are you and Sam still in for the 19th? We're at nine people if so, which…",
            body: "Hey!\n\nLocking the booking tonight! Are you and Sam still in for the 19th? We're at nine people if so, which means the bigger cabin.\n\nBring the good coffee ☕\n\nElena",
            date: "11:02 AM", unread: true, starred: true, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 41, account_id: 3, folder_id: 21, from_name: "Fastmail", from_addr: "support@fastmail.com", to: PERSONAL,
            subject: "Your storage is 80% full",
            preview: "Your mailbox is approaching its storage limit. Consider archiving old attachments or upgrading your plan…",
            body: "Hello,\n\nYour mailbox is approaching its storage limit (80% of 30 GB used).\n\nConsider archiving old attachments or upgrading your plan.\n\n— The Fastmail team",
            date: "9:47 AM", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 42, account_id: 3, folder_id: 21, from_name: "City Library", from_addr: "noreply@citylibrary.org", to: PERSONAL,
            subject: "Hold ready for pickup: \"The Pragmatic Programmer\"",
            preview: "Good news — an item you placed on hold is ready for pickup at the Central branch. Please collect it within…",
            body: "Good news — an item you placed on hold is ready for pickup at the Central branch:\n\n  The Pragmatic Programmer (20th Anniversary Edition)\n\nPlease collect it within 7 days.\n\nCity Library",
            date: "Yesterday", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 43, account_id: 3, folder_id: 21, from_name: "Ridgeline Cycles", from_addr: "service@ridgelinecycles.com", to: PERSONAL,
            subject: "Your tune-up is complete",
            preview: "Your bike is ready! We replaced the chain, trued the rear wheel, and bled the rear brake. Total comes to…",
            body: "Your bike is ready!\n\nWork done:\n  • New chain\n  • Trued rear wheel\n  • Rear brake bleed\n\nTotal: $86.40 — payable at pickup.\n\nRidgeline Cycles",
            date: "Thu", unread: false, starred: false, keywords: &[], has_attachment: true, in_reply_to: None },

        // ---- Filtered folders: mail the demo's filter rules filed away. Each
        // rule matches only what sits here (nothing in an inbox), since the
        // mock backend never moves anything. Unread counts match the folders. ----
        Spec { id: 17, account_id: 1, folder_id: 8, from_name: "Design Systems Weekly", from_addr: "hello@designsystems.substack.com", to: ME,
            subject: "Issue 84: tokens that survive a rebrand",
            preview: "This week: how three teams versioned their color tokens through a rebrand without touching a component, plus…",
            body: "This week:\n\n  • Tokens that survive a rebrand: three teams, three approaches\n  • A11y contrast audits you can automate\n  • Reader question: when is a component too small to ship?\n\nRead the full issue online.\n\n— Design Systems Weekly",
            date: "7:40 AM", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 18, account_id: 1, folder_id: 8, from_name: "The Pragmatic Engineer", from_addr: "pragmaticengineer@substack.com", to: ME,
            subject: "The Pulse: what changed in developer tooling this quarter",
            preview: "A roundup of the quarter's tooling shifts: the editors gaining ground, the CI providers losing it, and why…",
            body: "The Pulse\n\nA roundup of the quarter's tooling shifts: the editors gaining ground, the CI providers losing it, and why build times are back on everyone's roadmap.\n\nAlso this week: two engineering-culture pieces worth your time.\n\n— Gergely",
            date: "Yesterday", unread: true, starred: false, keywords: &["To_Do"], has_attachment: false, in_reply_to: None },
        Spec { id: 19, account_id: 1, folder_id: 8, from_name: "Frontend Focus", from_addr: "frontendfocus@substack.com", to: ME,
            subject: "#412: container queries, everywhere at last",
            preview: "Container queries now ship in every major engine. Here is what that unlocks, with a few layouts that were…",
            body: "Container queries now ship in every major engine. Here is what that unlocks, with a few layouts that were awkward or impossible before.\n\nPlus: a CSS reset worth revisiting, and a small library for view transitions.\n\n— Frontend Focus",
            date: "Mon", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 34, account_id: 2, folder_id: 18, from_name: "Hetzner", from_addr: "billing@hetzner.com", to: LAB,
            subject: "Invoice R0034215 for September",
            preview: "Your invoice for the current billing period is attached. The amount will be debited from your account in…",
            body: "Dear customer,\n\nYour invoice for the current billing period is attached.\n\n  Invoice: R0034215\n  Amount: €23.80\n\nThe amount will be debited from your account in 7 days.\n\nHetzner Online",
            date: "6:15 AM", unread: true, starred: false, keywords: &[], has_attachment: true, in_reply_to: None },
        Spec { id: 35, account_id: 2, folder_id: 18, from_name: "Fastly", from_addr: "billing@fastly.com", to: LAB,
            subject: "Your Fastly invoice is ready",
            preview: "The invoice for last month's usage is now available in your account. Total: $12.04. No action is needed if…",
            body: "The invoice for last month's usage is now available in your account.\n\n  Total: $12.04\n\nNo action is needed if you pay by card on file.\n\nFastly Billing",
            date: "Fri", unread: false, starred: false, keywords: &["Work"], has_attachment: true, in_reply_to: None },
        Spec { id: 44, account_id: 3, folder_id: 28, from_name: "Backcountry Outfitters", from_addr: "orders@backcountryoutfitters.com", to: PERSONAL,
            subject: "Order #48213 has shipped",
            preview: "Good news, your order is on its way. Track your package: it should arrive by Thursday. Items in this shipment…",
            body: "Good news, your order is on its way.\n\n  Order #48213\n  Arrives: Thursday\n\nItems in this shipment:\n  • Merino base layer (M)\n  • Trail gaiters\n\nBackcountry Outfitters",
            date: "8:52 AM", unread: true, starred: false, keywords: &["Personal"], has_attachment: false, in_reply_to: None },
        Spec { id: 45, account_id: 3, folder_id: 28, from_name: "Bookshop.org", from_addr: "orders@bookshop.org", to: PERSONAL,
            subject: "Your order is confirmed",
            preview: "Thanks for supporting Central Books! Your order of 2 items is confirmed and will ship within 2 business days…",
            body: "Thanks for supporting Central Books!\n\nYour order of 2 items is confirmed and will ship within 2 business days.\n\n  • The Design of Everyday Things\n  • A Philosophy of Software Design\n\nBookshop.org",
            date: "Tue", unread: false, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        // Junk (account 1): what the filter caught, so the folder's "Not
        // Spam" path (#168) has something to act on.
        Spec { id: 46, account_id: 1, folder_id: 6, from_name: "Prize Department", from_addr: "winner@lucky-draw-notify.biz", to: ME,
            subject: "You have been selected!!!",
            preview: "Congratulations, your address was chosen in our monthly draw. Confirm your details within 24 hours to claim…",
            body: "Congratulations, your address was chosen in our monthly draw.\n\nConfirm your details within 24 hours to claim your prize.",
            date: "Mon", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
        Spec { id: 47, account_id: 1, folder_id: 6, from_name: "Nordic Sauna Club", from_addr: "news@nordicsaunaclub.example", to: ME,
            subject: "Your membership offer expires tonight",
            preview: "Last chance: 40% off a year of unlimited sessions. Offer ends at midnight…",
            body: "Last chance: 40% off a year of unlimited sessions.\n\nOffer ends at midnight.",
            date: "Sun", unread: true, starred: false, keywords: &[], has_attachment: false, in_reply_to: None },
    ];
    let mut out: Vec<Message> = specs.iter().map(build).collect();
    // What the demo's own mail answers wears the replied arrow (#454), and a
    // receipt passed on to the accountant the forwarded one.
    let answered: std::collections::HashSet<String> = out
        .iter()
        .filter(|m| [ME, LAB, PERSONAL].contains(&m.from_addr.as_str()) && !m.references.is_empty())
        .map(|m| m.references.clone())
        .collect();
    for m in out.iter_mut() {
        m.answered = answered.contains(&m.message_id);
        m.forwarded = m.id == 11;
    }
    // HYLKI_DEMO_BULK=N pads every account's Inbox and Sent with N more
    // messages, to exercise the list at a real mailbox's size.
    if let Some(n) = std::env::var("HYLKI_DEMO_BULK")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        let base = out
            .iter()
            .map(|m| m.timestamp)
            .max()
            .unwrap_or(1_700_000_000);
        let mut id = 100_000u32;
        for (account_id, folders) in [(1u32, [1u32, 3u32]), (2, [11, 13]), (3, [21, 23])] {
            for folder_id in folders {
                for i in 0..n {
                    id += 1;
                    let sent = folder_id % 10 == 3;
                    out.push(Message {
                        id,
                        account_id,
                        folder_id,
                        uid: id,
                        from_name: if sent { "Jason M.".into() } else { format!("Sender {}", i % 97) },
                        from_addr: if sent { ME.into() } else { format!("sender{}@example.com", i % 97) },
                        reply_to: String::new(),
                        reply_to_name: String::new(),
                        to: if sent { format!("person{}@example.com", i % 53) } else { ME.into() },
                        cc: String::new(),
                        bcc: String::new(),
                        subject: format!("Bulk message {i} in folder {folder_id}"),
                        preview: "A generated message that stands in for real mail, so a list of thousands can be measured.".into(),
                        body: String::new(),
                        date: "Mon".into(),
                        timestamp: base - 3600 * (i as i64 + 1) - account_id as i64,
                        unread: !sent && i % 7 == 0,
                        starred: i % 41 == 0,
                        answered: false,
                        forwarded: false,
                        keywords: Vec::new(),
                        has_attachment: false,
                        message_id: format!("<bulk-{id}@hylki.local>"),
                        references: String::new(),
                        importance: Default::default(),
                        due: 0,
                        labels: Vec::new(),
                    });
                }
            }
        }
    }
    out
}
