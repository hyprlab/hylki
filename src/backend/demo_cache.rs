//! Seeding a cache with the demo mailbox, attachments included, for the
//! showcase and the screenshots.

use crate::backend::{MailBackend, MockBackend};
use crate::models::Message;

/// Seed a cache with the demo's sample mail, so the offline demo exercises the
/// very queries the real gallery runs — its paging, search, sort and folder
/// scope are all SQL, and a demo that answered from a Vec would prove none of
/// them work. Only ever called with the in-memory cache demo mode opens, so
/// none of this can reach a real `cache.db`.
pub(crate) fn seed_demo_cache(cache: &crate::cache::Cache) {
    let mock = MockBackend::new();
    seed_sample_mail(cache, &mock);
    seed_sample_attachments(cache, &mock);
    // HYLKI_DEMO_ATTACHMENTS=N adds N more attachments per account, spread back
    // over twenty years — the shape of a real archive, for exercising the
    // gallery's paging and its scroll.
    if let Some(n) = std::env::var("HYLKI_DEMO_ATTACHMENTS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        seed_archive_attachments(cache, &mock, n);
    }
}

/// Every sample folder and its messages.
fn seed_sample_mail(cache: &crate::cache::Cache, mock: &MockBackend) {
    for account in mock.accounts() {
        let folders = MailBackend::folders(mock, account.id);
        cache.save_folders(account.id, &folders);
        for folder in &folders {
            let messages: Vec<Message> = mock
                .messages(folder.id)
                .into_iter()
                .map(|mut m| {
                    m.folder_id = folder.id;
                    m
                })
                .collect();
            if !messages.is_empty() {
                cache.save_messages(account.id, &folder.path, &messages);
            }
        }
    }
}

/// The sample attachments hang off messages of their own, invented here so
/// the gallery has rows going back further than the sample inbox does.
fn seed_sample_attachments(cache: &crate::cache::Cache, mock: &MockBackend) {
    for account in mock.accounts() {
        for &(uid, folder, name, from, subject, kb, ago) in demo_attachments(account.id) {
            let ts = crate::datefmt::now() - ago * 86_400;
            let message = attachment_message(
                account.id,
                uid,
                from.to_string(),
                format!("{}@example.com", from.to_lowercase().replace(' ', ".")),
                subject.to_string(),
                ts,
            );
            seed_attachment(cache, folder, &message, name.to_string(), kb);
        }
    }
}

/// `n` more attachments per account, one every 430 seconds back from now
/// over three folders and five kinds of file.
fn seed_archive_attachments(cache: &crate::cache::Cache, mock: &MockBackend, n: u32) {
    let kinds = [
        ("invoice", "pdf", 90u64),
        ("photo", "jpg", 2200),
        ("notes", "txt", 8),
        ("sheet", "xlsx", 140),
        ("archive", "zip", 5400),
    ];
    let folders = ["Inbox", "Archive", "Newsletters"];
    for account in mock.accounts() {
        for i in 0..n {
            let uid = 20_000 + i;
            let (stem, ext, kb) = kinds[(i as usize) % kinds.len()];
            // Oldest at the far end, so the newest page is the first one.
            let ts = crate::datefmt::now() - (i as i64 + 1) * 430;
            let folder = folders[(i as usize) % folders.len()];
            let message = attachment_message(
                account.id,
                uid,
                format!("Sender {}", i % 40),
                format!("sender{}@example.com", i % 40),
                format!("Message {i}"),
                ts,
            );
            seed_attachment(cache, folder, &message, format!("{stem}-{i}.{ext}"), kb);
        }
    }
}

/// A read message dated `ts` that carries one attachment and nothing else.
fn attachment_message(
    account_id: u32,
    uid: u32,
    from_name: String,
    from_addr: String,
    subject: String,
    ts: i64,
) -> Message {
    Message {
        id: uid,
        account_id,
        folder_id: 0,
        uid,
        from_name,
        from_addr,
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject,
        preview: String::new(),
        body: String::new(),
        date: crate::datefmt::day_month_year(ts),
        timestamp: ts,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: true,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}

/// `message` into `folder`, described as holding one file of `kb` KB.
fn seed_attachment(
    cache: &crate::cache::Cache,
    folder: &str,
    message: &Message,
    name: String,
    kb: u64,
) {
    // Insert-or-replace, not save: `save_messages` clears the folder
    // first, so seeding one message at a time would leave only the last.
    cache.upsert_messages(message.account_id, folder, std::slice::from_ref(message));
    cache.save_attachment_meta(
        message.account_id,
        folder,
        message.uid,
        &[crate::models::AttachmentMeta {
            idx: 0,
            name,
            mime: String::new(),
            size: kb * 1024,
            section: String::new(),
        }],
    );
}

/// (uid, folder, filename, sender, subject, KB, days ago) for the demo gallery,
/// spread over the folder kinds the gallery's scope acts on.
fn demo_attachments(
    account_id: u32,
) -> &'static [(
    u32,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    u64,
    i64,
)] {
    match account_id {
        1 => &[
            (
                9001,
                "Inbox",
                "Q3-review.pdf",
                "Dana Whitfield",
                "Quarter review pack",
                842,
                1,
            ),
            (
                9002,
                "Inbox",
                "roadmap.png",
                "Priya Raman",
                "Roadmap sketch",
                310,
                2,
            ),
            (
                9003,
                "Inbox",
                "notes.txt",
                "Tomas Weber",
                "Handover notes",
                6,
                3,
            ),
            (
                9004,
                "Inbox",
                "budget-2026.xlsx",
                "Dana Whitfield",
                "Budget draft",
                96,
                5,
            ),
            (
                9005,
                "Archive",
                "contract-signed.pdf",
                "Legal",
                "Countersigned",
                1204,
                40,
            ),
            (
                9006,
                "Archive",
                "offsite-photos.zip",
                "Priya Raman",
                "Offsite photos",
                18400,
                62,
            ),
            (
                9007,
                "Archive",
                "invoice-2019.pdf",
                "Northwind Ltd",
                "Invoice",
                74,
                2200,
            ),
            (
                9008,
                "Starred",
                "keys.asc",
                "Tomas Weber",
                "My public key",
                3,
                9,
            ),
            (
                9009,
                "Newsletters",
                "issue-42.pdf",
                "The Weekly",
                "Issue 42",
                520,
                4,
            ),
            (
                9010,
                "Newsletters",
                "banner.jpg",
                "The Weekly",
                "Issue 41",
                244,
                11,
            ),
            (
                9011,
                "Sent",
                "proposal-v3.docx",
                "Jason M.",
                "Re: Proposal",
                180,
                2,
            ),
            (
                9012,
                "Sent",
                "screenshot.png",
                "Jason M.",
                "Re: That bug",
                420,
                6,
            ),
        ],
        2 => &[
            (
                9101,
                "Inbox",
                "invoice-1180.pdf",
                "Northwind Ltd",
                "Invoice 1180",
                88,
                1,
            ),
            (
                9102,
                "Inbox",
                "logo-pack.zip",
                "Studio Kern",
                "Brand assets",
                9600,
                3,
            ),
            (9103, "Inbox", "meeting.ics", "Calendar", "Standup", 2, 1),
            (
                9104,
                "Archive",
                "invoice-1104.pdf",
                "Northwind Ltd",
                "Invoice 1104",
                86,
                95,
            ),
            (
                9105,
                "Invoices",
                "invoice-1172.pdf",
                "Northwind Ltd",
                "Invoice 1172",
                87,
                21,
            ),
            (
                9106,
                "Invoices",
                "invoice-1165.pdf",
                "Acme Supply",
                "Invoice 1165",
                91,
                33,
            ),
            (
                9107,
                "Sent",
                "remittance.pdf",
                "Hyprlab",
                "Payment sent",
                64,
                7,
            ),
        ],
        _ => &[
            (
                9201,
                "Inbox",
                "boarding-pass.pdf",
                "Skyline Air",
                "Your trip",
                140,
                2,
            ),
            (9202, "Inbox", "recipe.jpg", "Mum", "That cake", 880, 8),
            (
                9203,
                "Archive",
                "insurance-2025.pdf",
                "Cover Direct",
                "Renewal",
                320,
                210,
            ),
            (
                9204,
                "Orders",
                "receipt-8841.pdf",
                "Bookshop",
                "Your order",
                48,
                14,
            ),
            (
                9205,
                "Sent",
                "holiday-plan.odt",
                "Jason",
                "Re: August",
                22,
                12,
            ),
        ],
    }
}
