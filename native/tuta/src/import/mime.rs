//! A MIME message as the data of Tuta's mail import: a port of the official
//! desktop importer's conversion (tutanota `src/app-kit/mimimi`,
//! `importer/importable_mail.rs`, `extend_mail_parser.rs` and
//! `plain_text_to_html_converter.rs`) at the pinned release, over the same
//! `mail-parser` version. Keeping its rules means a mail imported here reads
//! the same as one the official client imported: the body is one HTML string
//! (plain parts converted, `multipart/alternative` reduced to one choice),
//! every other part is an attachment, and the raw header block is kept.
//!
//! Where upstream would panic on hostile input (`todo!()` for a group in
//! `Resent-From`, an `assert!` in the alternative choice, `unwrap` on
//! encryption) this port degrades instead: the input is untrusted mail.
use crypto_primitives::{
    aes::InitializationVector, key::GenericAesKey, randomizer_facade::RandomizerFacade,
    versioned::VersionedAesKey,
};
use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{
    Address, ContentType, HeaderName, MessagePart, MessagePartId, MimeHeaders, PartType,
};
use std::borrow::Cow;
use std::collections::HashSet;
use tutasdk::date::DateTime;
use tutasdk::entities::generated::sys::BlobReferenceTokenWrapper;
use tutasdk::entities::generated::tutanota::{
    EncryptedMailAddress, ImportAttachment, ImportMailData, ImportMailDataMailReference,
    MailAddress, NewImportAttachment, Recipients,
};

/// Upstream's `FIXED_CUSTOM_ID`: aggregate ids inside an import only have to
/// be the right length, not unique.
const FIXED_CUSTOM_ID: &str = "____";

/// TS `MailState`; an imported mail is filed as received.
#[derive(Default)]
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) enum MailState {
    #[default]
    Received = 2,
}

/// TS `MailMethod`; upstream never sets anything but `Nothing`.
#[derive(Default)]
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) enum ICalType {
    #[default]
    Nothing = 0,
}

/// TS `ReplyType`.
#[derive(Default)]
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) enum ReplyType {
    #[default]
    Nothing = 0,
    Reply = 1,
    Forward = 2,
    ReplyForward = 3,
}

#[cfg_attr(test, derive(PartialEq, Debug, Clone))]
pub(crate) struct ImportableMailAttachment {
    pub meta_data: ImportableMailAttachmentMetaData,
    pub content: Vec<u8>,
}

impl ImportableMailAttachment {
    /// The attachment's `ImportAttachment`: its file key wrapped with the mail
    /// group key, its name, type and cid encrypted with the file key.
    pub(crate) fn make_import_attachment_data(
        &self,
        mail_group_key: &VersionedAesKey,
        randomizer: &RandomizerFacade,
        file_session_key: &GenericAesKey,
        reference_tokens: Vec<BlobReferenceTokenWrapper>,
    ) -> Option<ImportAttachment> {
        let owner_enc_file_session_key = mail_group_key
            .encrypt_key(file_session_key, InitializationVector::generate(randomizer));
        let encrypt = |text: &[u8]| {
            file_session_key
                .encrypt_data(text, InitializationVector::generate(randomizer))
                .ok()
        };
        let enc_cid = match &self.meta_data.content_id {
            Some(cid) => Some(encrypt(cid.as_bytes())?),
            None => None,
        };
        Some(ImportAttachment {
            _id: None,
            ownerEncFileSessionKey: owner_enc_file_session_key.object,
            ownerFileKeyVersion: i64::try_from(owner_enc_file_session_key.version).ok()?,
            existingAttachmentFile: None,
            newAttachment: Some(NewImportAttachment {
                _id: None,
                encCid: enc_cid,
                encFileHash: None,
                encFileName: encrypt(self.meta_data.filename.as_bytes())?,
                encMimeType: encrypt(self.meta_data.content_type.as_bytes())?,
                ownerEncFileHashSessionKey: None,
                ownerKeyVersion: None,
                referenceTokens: reference_tokens,
            }),
        })
    }
}

#[cfg_attr(test, derive(PartialEq, Debug, Clone))]
pub(crate) struct ImportableMailAttachmentMetaData {
    pub filename: String,
    pub content_id: Option<String>,
    pub content_type: String,
}

#[derive(Default, PartialEq)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct MailContact {
    pub mail_address: String,
    pub name: String,
}

impl From<&mail_parser::Addr<'_>> for MailContact {
    fn from(addr: &mail_parser::Addr) -> Self {
        Self {
            name: addr.name().unwrap_or_default().to_string(),
            mail_address: addr.address().unwrap_or_default().to_string(),
        }
    }
}

impl From<MailContact> for MailAddress {
    fn from(value: MailContact) -> Self {
        Self {
            _id: None,
            address: value.mail_address,
            name: value.name,
            contact: None,
            _errors: Default::default(),
        }
    }
}

/// What `ImportMailData` is made of, before keys.
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) struct ImportableMail {
    pub(crate) headers_string: String,
    pub(crate) subject: String,
    pub(crate) html_body_text: String,
    pub(crate) attachments: Vec<ImportableMailAttachment>,

    pub(crate) date: Option<DateTime>,

    pub(crate) different_envelope_sender: Option<String>,
    pub(crate) from_addresses: Vec<MailContact>,
    pub(crate) to_addresses: Vec<MailContact>,
    pub(crate) cc_addresses: Vec<MailContact>,
    pub(crate) bcc_addresses: Vec<MailContact>,
    pub(crate) reply_to_addresses: Vec<MailContact>,

    pub(crate) ical_type: ICalType,
    pub(crate) reply_type: ReplyType,

    pub(crate) mail_state: MailState,
    pub(crate) is_phishing: bool,
    pub(crate) unread: bool,

    pub(crate) message_id: Option<String>,
    pub(crate) in_reply_to: Option<String>,
    pub(crate) references: Vec<String>,
}

enum ContentTransferEncoding {
    Base64,
    QuotedPrintable,
    Other,
}

impl ImportableMail {
    fn handle_plain_text(email_body_as_html: &mut String, plain_text: &str) {
        email_body_as_html.push_str(&plain_text_to_html(plain_text));
    }

    fn handle_html_text(email_body_as_html: &mut String, html_text: &str) {
        email_body_as_html.push_str(html_text);
    }

    /// Of a `multipart/alternative`, keep one alternative. Upstream means to
    /// prefer HTML over plain text, but compares the subtype against the
    /// main type (`c_type == "plain" && subtype == "text"`), so no part ever
    /// matches and its rule reduces to: ignore every alternative not already
    /// ignored, then keep the last one -- which is what RFC 2046 says the
    /// sender prefers. That effective rule is what is ported, so an import
    /// here reads like the official client's; upstream's `assert!` (which
    /// fires on a nested alternative whose last part an outer one already
    /// ignored) is left out.
    fn handle_multipart(
        multipart_ignored_alternative: &mut HashSet<MessagePartId>,
        part: &MessagePart,
        multi_part_ids: &[MessagePartId],
    ) {
        let is_multipart_alternative = part.content_type().is_some_and(|content_type| {
            content_type.c_type == "multipart" && content_type.subtype() == Some("alternative")
        });
        // Every other multipart is treated as `mixed`: its parts are shown in
        // order (RFC 2046 5.1.3).
        if !is_multipart_alternative {
            return;
        }
        for multipart_id in multi_part_ids {
            multipart_ignored_alternative.insert(*multipart_id);
        }
        if let Some(last_choice) = multi_part_ids.last() {
            multipart_ignored_alternative.remove(last_choice);
        }
    }

    fn handle_binary(
        part: &MessagePart,
        attachments: &mut Vec<ImportableMailAttachment>,
        content: Vec<u8>,
    ) {
        let content_id = part.content_id().map(ToString::to_string);
        let filename = Self::get_filename(part, "unknown");
        let content_type = part
            .content_type()
            .map(MakeString::make_string)
            .map(Cow::into_owned)
            .unwrap_or_else(|| Self::default_content_type().make_string().into_owned());
        attachments.push(ImportableMailAttachment {
            meta_data: ImportableMailAttachmentMetaData {
                filename,
                content_id,
                content_type,
            },
            content,
        });
    }

    fn handle_message(
        attachments: &mut Vec<ImportableMailAttachment>,
        parent_part: &MessagePart,
        message: &mail_parser::Message,
    ) {
        let content_type = parent_part
            .content_type()
            .map(MakeString::make_string)
            .map(Cow::into_owned)
            .unwrap_or_default();
        let content_id = parent_part.content_id().map(ToString::to_string);
        // The attached message whole, headers included, as it was sent.
        let content = message
            .parts
            .first()
            .and_then(|nested| {
                message
                    .raw_message
                    .get(nested.raw_header_offset()..nested.raw_end_offset())
            })
            .unwrap_or_default()
            .to_vec();
        let filename = Self::get_filename(parent_part, message.subject().unwrap_or("unknown"));
        attachments.push(ImportableMailAttachment {
            meta_data: ImportableMailAttachmentMetaData {
                filename,
                content_id,
                content_type,
            },
            content,
        });
    }

    /// The body as one HTML string, and every part that is not body as an
    /// attachment.
    fn process_all_parts(
        parsed_message: &mail_parser::Message,
    ) -> (String, Vec<ImportableMailAttachment>) {
        let mut email_body_as_html = String::new();
        let mut attachments = Vec::with_capacity(parsed_message.attachments.len());
        // The alternatives of a multipart/alternative not chosen.
        let mut multipart_ignored_alternative = HashSet::new();

        for (part_id, part) in parsed_message.parts.iter().enumerate() {
            if multipart_ignored_alternative.contains(&part_id) {
                continue;
            }
            match &part.body {
                // Text is body unless it is marked as an attachment, and only
                // when it is text/plain (or untyped): any other text/* is a
                // file.
                PartType::Text(text)
                    if !Self::is_attachment(&email_body_as_html, part)
                        && Self::is_plain_text(part) =>
                {
                    Self::handle_plain_text(&mut email_body_as_html, text.as_ref());
                }
                // A part is only ever Html when marked text/html, so no
                // second type check is needed.
                PartType::Html(html_text) if !Self::is_attachment(&email_body_as_html, part) => {
                    Self::handle_html_text(&mut email_body_as_html, html_text.as_ref());
                }
                // Text kept as a file: from its raw bytes, since the decoded
                // text may already have lost an unlabelled charset (upstream
                // `2002_06_12_doublebound.msg`).
                PartType::Html(_) | PartType::Text(_) => {
                    let raw_content = parsed_message
                        .raw_message
                        .get(part.offset_body..part.offset_end)
                        .unwrap_or_default()
                        .to_vec();
                    let raw_content = match Self::get_content_transfer_type(part) {
                        ContentTransferEncoding::Base64 => {
                            base64_decode(raw_content.as_slice()).unwrap_or(raw_content)
                        }
                        ContentTransferEncoding::QuotedPrintable => {
                            quoted_printable_decode(raw_content.as_slice()).unwrap_or(raw_content)
                        }
                        ContentTransferEncoding::Other => raw_content,
                    };
                    Self::handle_binary(part, &mut attachments, raw_content);
                }
                PartType::Binary(binary_content) | PartType::InlineBinary(binary_content) => {
                    Self::handle_binary(part, &mut attachments, binary_content.to_vec());
                }
                PartType::Message(attached_message) => {
                    Self::handle_message(&mut attachments, part, attached_message);
                }
                PartType::Multipart(multi_part_ids) => {
                    Self::handle_multipart(
                        &mut multipart_ignored_alternative,
                        part,
                        multi_part_ids,
                    );
                }
            }
        }
        (email_body_as_html, attachments)
    }

    fn is_plain_text(part: &MessagePart) -> bool {
        // Untyped text is taken as text/plain; message/rfc822 too, since in a
        // multipart/digest it is the default part type (RFC 2046 5.1.5).
        part.content_type().is_none_or(|content_type| {
            let subtype = content_type.subtype();
            let is_text_plain =
                content_type.c_type == "text" && (subtype == Some("plain") || subtype.is_none());
            let is_message_rfc822 = content_type.c_type == "message" && subtype == Some("rfc822");
            is_text_plain || is_message_rfc822
        })
    }

    fn is_attachment(email_body_as_html: &str, part: &MessagePart) -> bool {
        part.content_disposition()
            .is_some_and(|content_disposition| content_disposition.c_type == "attachment")
            || (!email_body_as_html.is_empty() && part.content_id().is_some())
    }

    fn get_filename(part: &MessagePart, fallback_name: &str) -> String {
        let content_disposition_filename = part
            .content_disposition()
            .and_then(|c| c.attribute("filename").map(ToString::to_string));
        let content_type_filename = part
            .content_type()
            .and_then(|c| c.attribute("name").map(ToString::to_string));
        let file_name = content_disposition_filename.unwrap_or_else(|| {
            content_type_filename.unwrap_or_else(|| {
                let filename_suffix = part
                    .content_type()
                    .map(Self::get_suffix_from_content_type)
                    .unwrap_or_default();
                fallback_name.to_string() + filename_suffix
            })
        });
        escape_filename(&file_name)
    }

    fn get_suffix_from_content_type(content_type: &ContentType) -> &'static str {
        if content_type.c_type == "message" {
            if content_type.subtype() == Some("rfc822") {
                ".eml"
            } else {
                ".txt"
            }
        } else if content_type.c_type == "text" {
            if content_type.subtype() == Some("calendar") {
                ".ics"
            } else {
                ".txt"
            }
        } else {
            ""
        }
    }

    fn get_content_transfer_type(parent_part: &MessagePart) -> ContentTransferEncoding {
        match parent_part.content_transfer_encoding() {
            Some(cte) if cte.eq_ignore_ascii_case("QUOTED-PRINTABLE") => {
                ContentTransferEncoding::QuotedPrintable
            }
            Some(cte) if cte.eq_ignore_ascii_case("BASE64") => ContentTransferEncoding::Base64,
            _ => ContentTransferEncoding::Other,
        }
    }

    fn default_content_type() -> ContentType<'static> {
        ContentType {
            c_type: Cow::Borrowed("text"),
            c_subtype: Some(Cow::Borrowed("plain")),
            attributes: Some(vec![(Cow::Borrowed("charset"), Cow::Borrowed("us-ascii"))]),
        }
    }

    fn map_to_tuta_mail_address(mail_parser_addresses: &Address) -> Vec<MailContact> {
        match mail_parser_addresses {
            Address::List(address_list) => address_list.iter().map(MailContact::from).collect(),
            Address::Group(group_senders) => group_senders
                .iter()
                .flat_map(|group| group.addresses.as_slice())
                .map(MailContact::from)
                .collect(),
        }
    }

    pub(crate) fn take_out_attachments(&mut self) -> Vec<ImportableMailAttachment> {
        std::mem::take(&mut self.attachments)
    }

    pub(crate) fn make_import_mail_data(
        self,
        owner_enc_sk: Vec<u8>,
        owner_enc_sk_version: i64,
    ) -> ImportMailData {
        let ImportableMail {
            headers_string,
            subject,
            html_body_text,
            different_envelope_sender,
            from_addresses,
            cc_addresses,
            bcc_addresses,
            to_addresses,
            date,
            reply_to_addresses,
            ical_type,
            reply_type,
            mail_state,
            is_phishing,
            unread,
            message_id,
            in_reply_to,
            references,
            attachments: _,
        } = self;

        let reply_tos = reply_to_addresses
            .into_iter()
            .map(|reply_to| EncryptedMailAddress {
                _id: Some(tutasdk::CustomId::from_custom_string(FIXED_CUSTOM_ID)),
                name: reply_to.name,
                address: reply_to.mail_address,
                _errors: Default::default(),
            })
            .collect();
        let from_addresses: Vec<MailAddress> = from_addresses.into_iter().map(Into::into).collect();
        let references = references
            .into_iter()
            .map(|reference| ImportMailDataMailReference {
                _id: Some(tutasdk::CustomId::from_custom_string(FIXED_CUSTOM_ID)),
                reference,
            })
            .collect();
        // Without a usable Date the epoch is filed, so the wrong date is
        // obvious rather than plausible.
        let date = date.unwrap_or_else(|| DateTime::from_millis(0));

        ImportMailData {
            _format: 0,
            ownerEncSessionKey: owner_enc_sk,
            ownerKeyVersion: owner_enc_sk_version,
            compressedHeaders: headers_string,
            subject,
            compressedBodyText: html_body_text,
            differentEnvelopeSender: different_envelope_sender,
            sender: from_addresses
                .first()
                .cloned()
                .unwrap_or(MailContact::default().into()),
            recipients: Recipients {
                _id: Some(tutasdk::CustomId::from_custom_string(FIXED_CUSTOM_ID)),
                bccRecipients: bcc_addresses.into_iter().map(Into::into).collect(),
                ccRecipients: cc_addresses.into_iter().map(Into::into).collect(),
                toRecipients: to_addresses.into_iter().map(Into::into).collect(),
            },
            replyTos: reply_tos,
            unread,
            confidential: false,
            method: ical_type as i64,
            phishingStatus: i64::from(is_phishing),
            replyType: reply_type as i64,
            date,
            state: mail_state as i64,
            messageId: message_id,
            inReplyTo: in_reply_to,
            references,
            importedAttachments: vec![],
            _errors: Default::default(),
            imapModSeq: None,
            imapUid: None,
            labels: vec![],
        }
    }

    pub(crate) fn from_parsed_message(parsed_message: &mail_parser::Message) -> Self {
        let subject = parsed_message.subject().unwrap_or_default().to_string();
        let date = get_importable_date_time(parsed_message);

        let name_as_address_if_empty_address = |mut address: MailContact| -> MailContact {
            if address.mail_address.is_empty() && !address.name.is_empty() {
                address.mail_address = address.name;
                address.name = String::new();
            }
            address
        };
        let from_addresses = parsed_message
            .from()
            .or_else(|| parsed_message.sender())
            .map(Self::map_to_tuta_mail_address)
            .unwrap_or_default()
            .into_iter()
            .map(name_as_address_if_empty_address)
            .collect::<Vec<_>>();

        let different_envelope_sender = parsed_message
            .sender()
            .map(Self::map_to_tuta_mail_address)
            .unwrap_or_default()
            // There is at most one envelope sender.
            .pop()
            .map(name_as_address_if_empty_address)
            .filter(|diff_sender| {
                from_addresses
                    .iter()
                    .any(|from| from.mail_address != diff_sender.mail_address)
            })
            .map(|mail_address| mail_address.mail_address);

        let addresses = |header: Option<&Address>| -> Vec<MailContact> {
            header
                .map(Self::map_to_tuta_mail_address)
                .unwrap_or_default()
                .into_iter()
                .filter(|address| !address.mail_address.trim().is_empty())
                .collect()
        };
        let to_addresses = addresses(parsed_message.to());
        let cc_addresses = addresses(parsed_message.cc());
        let bcc_addresses = addresses(parsed_message.bcc());
        let reply_to_addresses = addresses(parsed_message.reply_to());

        let headers_string = parsed_message
            .headers_raw()
            .map(|(name, value)| name.to_string() + ":" + value)
            .collect::<Vec<_>>()
            .join("");

        let reply_type = get_reply_type_from_headers(parsed_message.headers());
        let message_id = parsed_message.message_id().map(String::from);
        let in_reply_to = parsed_message.in_reply_to().as_text().map(String::from);
        let references = match parsed_message.references() {
            mail_parser::HeaderValue::Text(reference) => Vec::from([reference.to_string()]),
            mail_parser::HeaderValue::TextList(references) => {
                references.iter().map(Cow::to_string).collect()
            }
            _ => Vec::new(),
        };

        let (html_body_text, attachments) = ImportableMail::process_all_parts(parsed_message);

        Self {
            headers_string,
            html_body_text,
            subject,
            different_envelope_sender,
            from_addresses,
            to_addresses,
            cc_addresses,
            bcc_addresses,
            reply_to_addresses,
            date,
            reply_type,
            message_id,
            in_reply_to,
            references,
            attachments,

            ical_type: Default::default(),
            unread: false,
            mail_state: Default::default(),
            is_phishing: false,
        }
    }
}

/// The Date header as an import date: `None` when there is none or it does
/// not fit (a date before the epoch is the epoch), so the import files the
/// epoch.
fn get_importable_date_time(message: &mail_parser::Message) -> Option<DateTime> {
    message
        .date()
        .map(mail_parser::DateTime::to_timestamp)
        .map(|timestamp| timestamp.max(0))
        .and_then(|seconds| (seconds as u64).checked_mul(1000))
        .map(DateTime::from_millis)
}

/// A filename valid on Linux and Windows: upstream's pattern
/// `[\/:*?<>"|]`, each replaced by `_` (a backslash is not in it).
fn escape_filename(file_name: &str) -> String {
    file_name
        .chars()
        .map(|c| match c {
            '/' | ':' | '*' | '?' | '<' | '>' | '"' | '|' => '_',
            c => c,
        })
        .collect()
}

// ------------------------------------------------- extend_mail_parser.rs

/// Upstream's reply type from the headers, as it is: an *empty*
/// `Resent-From` marks a forward and an *empty* `References` a reply.
fn get_reply_type_from_headers(headers: &[mail_parser::Header<'_>]) -> ReplyType {
    let mut is_forward = false;
    let mut is_reply = false;
    for header in headers {
        if header.name == HeaderName::ResentFrom {
            if header.value().make_string().trim().is_empty() {
                is_forward = true;
            }
        } else if header.name == HeaderName::References
            && header.value().make_string().trim().is_empty()
        {
            is_reply = true;
        }
        if is_reply && is_forward {
            break;
        }
    }
    match (is_forward, is_reply) {
        (true, true) => ReplyType::ReplyForward,
        (true, false) => ReplyType::Forward,
        (false, true) => ReplyType::Reply,
        (false, false) => ReplyType::default(),
    }
}

/// Types of the parser as the strings upstream imports.
trait MakeString {
    fn make_string(&self) -> Cow<'_, str>;
}

impl MakeString for mail_parser::HeaderValue<'_> {
    fn make_string(&self) -> Cow<'_, str> {
        match self {
            mail_parser::HeaderValue::ContentType(content_t) => content_t.make_string(),
            // Upstream's placeholder, kept: nothing reads a Received here.
            mail_parser::HeaderValue::Received(_) => Cow::Borrowed("todo"),
            mail_parser::HeaderValue::Address(address) => address.make_string(),
            mail_parser::HeaderValue::DateTime(date_time) => date_time.make_string(),
            mail_parser::HeaderValue::TextList(text_list) => Cow::Owned(text_list.join(",")),
            mail_parser::HeaderValue::Text(text) => Cow::Borrowed(text.as_ref()),
            mail_parser::HeaderValue::Empty => Cow::Borrowed(""),
        }
    }
}

impl MakeString for mail_parser::DateTime {
    fn make_string(&self) -> Cow<'_, str> {
        const DAY_OF_WEEK: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTH_OF_YEAR: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sept", "Oct", "Nov", "Dec",
        ];
        let weekday = DAY_OF_WEEK
            .get(usize::from(self.day_of_week()))
            .unwrap_or(&"");
        let month = MONTH_OF_YEAR
            .get(usize::from(self.month).wrapping_sub(1))
            .unwrap_or(&"");
        let Self {
            year,
            day,
            hour: hh,
            minute: mm,
            second: ss,
            tz_hour: tz_hh,
            tz_minute: tz_mm,
            ..
        } = self;
        Cow::Owned(format!(
            "{weekday}, {day:02} {month:02} {year} {hh:02}:{mm:02}:{ss:02} +{tz_hh:02}{tz_mm:02}"
        ))
    }
}

impl MakeString for mail_parser::Address<'_> {
    fn make_string(&self) -> Cow<'_, str> {
        let list = |addresses: &[mail_parser::Addr]| {
            addresses
                .iter()
                .map(|addr| make_mail_address(addr.name(), addr.address()))
                .collect::<Vec<_>>()
        };
        match self {
            mail_parser::Address::List(address_list) => list(address_list).join(",").into(),
            // Upstream leaves this `todo!()`; its members are the addresses.
            mail_parser::Address::Group(groups) => groups
                .iter()
                .flat_map(|group| list(&group.addresses))
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        }
    }
}

impl MakeString for mail_parser::ContentType<'_> {
    fn make_string(&self) -> Cow<'_, str> {
        let attribute_str = self.attributes.as_ref().map(|attributes| {
            attributes
                .iter()
                .map(|(name, value)| {
                    if value.is_empty() {
                        name.to_string()
                    } else {
                        name.to_string() + "=\"" + value.as_ref() + "\""
                    }
                })
                .collect::<Vec<_>>()
                .join(";")
        });
        let mut content_type = self.c_type.as_ref().to_string();
        if let Some(subtype) = self.c_subtype.as_ref() {
            content_type.push('/');
            content_type.push_str(subtype);
        }
        if let Some(attribute_str) = attribute_str {
            if !content_type.is_empty() {
                content_type.push(';');
            }
            content_type.push_str(attribute_str.as_str());
        }
        Cow::Owned(content_type)
    }
}

fn make_mail_address(name: Option<&str>, address: Option<&str>) -> String {
    let name = name.unwrap_or_default();
    // A name is always quoted (tutadb #417).
    let mut res = if name.is_empty() || name.starts_with('"') {
        name.to_string()
    } else {
        String::from('"') + name + "\""
    };
    if let Some(address) = address {
        res.push('<');
        res.push_str(address);
        res.push('>');
    }
    res
}

// --------------------------------------- plain_text_to_html_converter.rs

/// Plain text as HTML, ported from tutadb's `PlainTextToHtmlConverter`:
/// line breaks become `<br>`, `>`-quoted blocks become nested
/// `<blockquote>`s, and `&`, `<` and `>` are escaped.
pub(crate) fn plain_text_to_html(plain_text: &str) -> String {
    let mut result = String::new();
    let mut previous_quote_level: usize = 0;
    // Upstream splits on `\r?\n`.
    for (i, line) in plain_text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line_quote_level = get_line_quote_level(line);
        // An explicit break only when the quote level stays: a blockquote
        // edge already breaks the line.
        if i > 0 && previous_quote_level == line_quote_level {
            result.push_str("<br>");
        }
        result.push_str(
            &"</blockquote>".repeat(previous_quote_level.saturating_sub(line_quote_level)),
        );
        result.push_str(
            &"<blockquote>".repeat(line_quote_level.saturating_sub(previous_quote_level)),
        );
        if line_quote_level > 0 {
            // Past the '>'s and the one space after them; both are ASCII, so
            // this is a char boundary.
            if line.len() > line_quote_level {
                result.push_str(&escape_plain_text_line(&line[line_quote_level + 1..]));
            }
        } else {
            result.push_str(&escape_plain_text_line(line));
        }
        previous_quote_level = line_quote_level;
    }
    result.push_str(&"</blockquote>".repeat(previous_quote_level));
    result
}

fn escape_plain_text_line(line: &str) -> String {
    line.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// How many `>` open the line: only a run ended by a space or the end of
/// the line counts.
fn get_line_quote_level(line: &str) -> usize {
    let mut line_open_blockquotes = 0;
    for char in line.chars() {
        if char == '>' {
            line_open_blockquotes += 1;
        } else if char == ' ' {
            break;
        } else {
            line_open_blockquotes = 0;
            break;
        }
    }
    line_open_blockquotes
}

#[cfg(test)]
mod mime_string_to_importable_mail_test;
#[cfg(test)]
mod msg_file_compatibility_test;
#[cfg(test)]
mod plain_text_to_html_test;
