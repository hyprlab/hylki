//! Read receipts (#520) against a real server, ignored by default.

use std::time::Duration;

use crate::worker::imap::bodies::load_raw;
use crate::worker::imap::connect::connect;
use crate::worker::imap::send::delete_draft;
use crate::worker::imap::sync::load_messages;
use crate::worker::smtp::send_smtp;
use crate::worker::test_support::{sample_account, sample_outgoing};
use crate::worker::{extract_body, OutgoingMessage};

/// With `IMAP_LIVE=host,port,user,password` and `SMTP_LIVE=host,port`
/// (implicit TLS): a message that asks for a receipt arrives asking, and
/// the receipt sent back arrives as a report the reader shows as words.
#[test]
#[ignore]
fn live_receipt_is_asked_for_and_answered() {
    let (Ok(spec), Ok(smtp)) = (std::env::var("IMAP_LIVE"), std::env::var("SMTP_LIVE")) else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let (smtp_host, smtp_port) = smtp.split_once(',').expect("host,port");
    let account = crate::config::AccountConfig {
        email: p[2].into(),
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        smtp_host: smtp_host.into(),
        smtp_port: smtp_port.parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            smtp_starttls: false,
            imap_accept_invalid_certs: true,
            smtp_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let pid = std::process::id();
    let asking = OutgoingMessage {
        to: account.email.clone(),
        subject: format!("Receipt please {pid}"),
        body: "Did this arrive?".into(),
        request_receipt: true,
        ..sample_outgoing()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let account = &account;
        let mut sess = connect(account).await.expect("connect");
        send_smtp(account, &asking).await.expect("sent");
        let arrived = |subject: String| async move {
            let mut sess = connect(account).await.expect("connect");
            for _ in 0..20 {
                let inbox = load_messages(1, &mut sess, 1, "INBOX", true, None)
                    .await
                    .expect("load");
                if let Some(m) = inbox.into_iter().find(|m| m.subject == subject) {
                    let raw = load_raw(&mut sess, "INBOX", m.uid).await.expect("raw");
                    return (m, raw);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            panic!("{subject} never arrived");
        };
        let (m, raw) = arrived(asking.subject.clone()).await;
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let request = crate::mail::receipt::detect(&parsed).expect("it asks");
        assert_eq!(request.to, account.email);
        println!("request: {request:?}");

        let receipt = OutgoingMessage {
            to: request.to.clone(),
            subject: format!("Read: {}", m.subject),
            body: "This is a read receipt.".into(),
            in_reply_to: m.message_id.clone(),
            receipt_report: Some(crate::mail::receipt::report_fields(
                &account.email,
                &request,
                &m.message_id,
                false,
            )),
            ..sample_outgoing()
        };
        send_smtp(account, &receipt).await.expect("receipt sent");
        let (r, rraw) = arrived(receipt.subject.clone()).await;
        let text = String::from_utf8_lossy(&rraw);
        assert!(
            text.contains("report-type=disposition-notification"),
            "{text}"
        );
        assert!(
            text.contains(&format!("Original-Message-ID: <{}>", m.message_id)),
            "{text}"
        );
        assert!(!r.has_attachment, "the report is no attachment");
        assert!(extract_body(&rraw).contains("This is a read receipt."));
        let rparsed = mail_parser::MessageParser::default().parse(&rraw).unwrap();
        assert!(
            crate::mail::receipt::detect(&rparsed).is_none(),
            "a receipt asks nothing"
        );

        for uid in [m.uid, r.uid] {
            delete_draft(&mut sess, "INBOX", uid)
                .await
                .expect("clean up");
        }
        let _ = sess.close().await;
    });
}
