//! What BODYSTRUCTURE says about attachments, without downloading anything.

use crate::worker::decode_header;
use crate::worker::render::attachments::{mime_extension, INLINE_ATTACHMENT_MIN};

/// Whether an IMAP BODYSTRUCTURE contains a part marked as an attachment.
///
/// `Content-Disposition: attachment` is the obvious case, but Apple Mail sends
/// iPhone photos as *inline* parts of a multipart/mixed, so disposition alone
/// misses them. Any non-text part therefore counts, except a small one carrying a
/// Content-ID — see [`INLINE_ATTACHMENT_MIN`].
pub(in crate::worker) fn structure_has_attachment(
    bs: &async_imap::imap_proto::types::BodyStructure,
) -> bool {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    let is_attachment = |common: &async_imap::imap_proto::types::BodyContentCommon| {
        common
            .disposition
            .as_ref()
            .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
    };
    // BODYSTRUCTURE reports the *encoded* size; base64 inflates by 4/3.
    let decoded_size = |other: &async_imap::imap_proto::types::BodyContentSinglePart| {
        let octets = other.octets as usize;
        match other.transfer_encoding {
            ContentEncoding::Base64 => octets / 4 * 3,
            _ => octets,
        }
    };

    match bs {
        Bs::Multipart { bodies, .. } => bodies.iter().any(structure_has_attachment),
        // A text part is an attachment only when it says so — except a
        // calendar part, which a meeting request sends as a plain
        // alternative with no disposition at all (#223). That part is the
        // meeting: it has to be listed, and the reader has to know the
        // message carries files before it will ask for them.
        Bs::Text { common, .. } => is_attachment(common) || is_calendar(common),
        Bs::Basic { common, other, .. } | Bs::Message { common, other, .. } => {
            is_attachment(common)
                || (!is_receipt_report(common)
                    && (other.id.is_none() || decoded_size(other) >= INLINE_ATTACHMENT_MIN))
        }
    }
}

/// Whether a BODYSTRUCTURE part is a read receipt's report (#520), which
/// the reader keeps out of the attachments.
fn is_receipt_report(common: &async_imap::imap_proto::types::BodyContentCommon) -> bool {
    common.ty.ty.eq_ignore_ascii_case("message")
        && common
            .ty
            .subtype
            .eq_ignore_ascii_case("disposition-notification")
}

/// Whether a BODYSTRUCTURE part is a `text/calendar` one (#223).
fn is_calendar(common: &async_imap::imap_proto::types::BodyContentCommon) -> bool {
    common.ty.ty.eq_ignore_ascii_case("text") && common.ty.subtype.eq_ignore_ascii_case("calendar")
}

/// Every attachment in a BODYSTRUCTURE, with what it is called, how big it is
/// and which IMAP section holds it — all without downloading a byte of it. This
/// is what lets the gallery reach back through an archive: the server describes
/// the files, and only opening one actually fetches it.
///
/// What counts as an attachment is [`structure_has_attachment`]'s rule, kept
/// deliberately identical so a message the list gives a paperclip to is a
/// message the gallery lists parts for.
pub(super) fn structure_attachments(
    bs: &async_imap::imap_proto::types::BodyStructure,
) -> Vec<crate::models::AttachmentMeta> {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    /// A parameter's value, matched case-insensitively as RFC 2045 requires.
    fn param(params: &async_imap::imap_proto::types::BodyParams, key: &str) -> Option<String> {
        params.as_ref()?.iter().find_map(|(k, v)| {
            k.eq_ignore_ascii_case(key)
                .then(|| decode_header(v.as_bytes()))
        })
    }

    fn walk(bs: &Bs, section: &str, out: &mut Vec<crate::models::AttachmentMeta>) {
        match bs {
            Bs::Multipart { bodies, .. } => {
                for (i, child) in bodies.iter().enumerate() {
                    let child_section = if section.is_empty() {
                        format!("{}", i + 1)
                    } else {
                        format!("{section}.{}", i + 1)
                    };
                    walk(child, &child_section, out);
                }
            }
            Bs::Text { common, other, .. }
            | Bs::Basic { common, other, .. }
            | Bs::Message { common, other, .. } => {
                let declared = common
                    .disposition
                    .as_ref()
                    .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
                // BODYSTRUCTURE reports the encoded size; base64 inflates by 4/3.
                let size = match other.transfer_encoding {
                    ContentEncoding::Base64 => other.octets as u64 / 4 * 3,
                    _ => other.octets as u64,
                };
                let is_text = matches!(bs, Bs::Text { .. });
                // A text part is only ever an attachment when it says so; for
                // the rest, an inline part with a Content-ID is decoration
                // unless it is big enough to be content (see the constant).
                let counts = if is_text {
                    declared || is_calendar(common)
                } else {
                    declared
                        || (!is_receipt_report(common)
                            && (other.id.is_none() || size as usize >= INLINE_ATTACHMENT_MIN))
                };
                if !counts {
                    return;
                }
                let name = common
                    .disposition
                    .as_ref()
                    .and_then(|d| param(&d.params, "filename"))
                    .or_else(|| param(&common.ty.params, "name"))
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| {
                        // Unnamed, as Apple Mail's inline photos arrive: give it
                        // the extension its type implies so the gallery can sort
                        // and filter it like any other file.
                        let ext = mime_extension(&common.ty.ty, &common.ty.subtype);
                        format!("attachment-{}{ext}", out.len() + 1)
                    });
                out.push(crate::models::AttachmentMeta {
                    idx: out.len() as u32,
                    name,
                    mime: format!(
                        "{}/{}",
                        common.ty.ty.to_lowercase(),
                        common.ty.subtype.to_lowercase()
                    ),
                    size,
                    section: section.to_string(),
                });
            }
        }
    }

    let mut out = Vec::new();
    // A message that is not multipart is one part, section 1.
    walk(
        bs,
        if matches!(bs, Bs::Multipart { .. }) {
            ""
        } else {
            "1"
        },
        &mut out,
    );
    out
}

#[cfg(test)]
mod attachment_scan_tests {
    use std::borrow::Cow;

    use async_imap::imap_proto::types::{
        BodyContentCommon, BodyContentSinglePart, BodyStructure, ContentDisposition,
        ContentEncoding, ContentType,
    };

    use super::{mime_extension, structure_attachments, INLINE_ATTACHMENT_MIN};
    use crate::worker::imap::structure::structure_has_attachment;

    fn ctype(
        ty: &'static str,
        subtype: &'static str,
        name: Option<&'static str>,
    ) -> ContentType<'static> {
        ContentType {
            ty: Cow::Borrowed(ty),
            subtype: Cow::Borrowed(subtype),
            params: name.map(|n| vec![(Cow::Borrowed("name"), Cow::Borrowed(n))]),
        }
    }

    fn disposition(
        ty: &'static str,
        filename: Option<&'static str>,
    ) -> ContentDisposition<'static> {
        ContentDisposition {
            ty: Cow::Borrowed(ty),
            params: filename.map(|f| vec![(Cow::Borrowed("filename"), Cow::Borrowed(f))]),
        }
    }

    fn single(
        ty: &'static str,
        subtype: &'static str,
        octets: u32,
        encoding: ContentEncoding<'static>,
        disp: Option<ContentDisposition<'static>>,
        content_id: Option<&'static str>,
        name: Option<&'static str>,
    ) -> BodyStructure<'static> {
        BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ctype(ty, subtype, name),
                disposition: disp,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: content_id.map(Cow::Borrowed),
                md5: None,
                description: None,
                transfer_encoding: encoding,
                octets,
            },
            extension: None,
        }
    }

    fn text(subtype: &'static str, octets: u32) -> BodyStructure<'static> {
        BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", subtype, None),
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets,
            },
            lines: 10,
            extension: None,
        }
    }

    fn multipart(bodies: Vec<BodyStructure<'static>>) -> BodyStructure<'static> {
        BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ctype("multipart", "mixed", None),
                disposition: None,
                language: None,
                location: None,
            },
            bodies,
            extension: None,
        }
    }

    /// The ordinary case: a message with a body and one attached file. The body
    /// is not an attachment; the file is, named from its disposition, and its
    /// section is what a FETCH would ask for.
    #[test]
    fn a_body_and_one_attachment_yields_just_the_attachment() {
        let bs = multipart(vec![
            text("plain", 400),
            single(
                "application",
                "pdf",
                4000,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("report.pdf"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "report.pdf");
        assert_eq!(found[0].mime, "application/pdf");
        assert_eq!(found[0].section, "2", "the second part of the multipart");
        assert_eq!(
            found[0].size, 3000,
            "base64 inflates by 4/3, so decode it back"
        );
    }

    /// A newsletter's logo: inline, small, with a Content-ID. Decoration, not a
    /// file anyone wants in their gallery.
    #[test]
    fn a_small_inline_image_with_a_content_id_is_decoration() {
        let bs = multipart(vec![
            text("html", 900),
            single(
                "image",
                "png",
                2000,
                ContentEncoding::Base64,
                None,
                Some("logo@x"),
                None,
            ),
        ]);
        assert!(structure_attachments(&bs).is_empty());
    }

    /// The same shape, but big enough to be a photograph someone sent: Apple
    /// Mail attaches pictures exactly this way, and they have to be saveable.
    #[test]
    fn a_large_inline_image_is_content_however_it_is_labelled() {
        let octets = (INLINE_ATTACHMENT_MIN as u32 + 1) * 4 / 3 + 8;
        let bs = multipart(vec![
            text("plain", 40),
            single(
                "image",
                "jpeg",
                octets,
                ContentEncoding::Base64,
                None,
                Some("img@x"),
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].name, "attachment-1.jpg",
            "unnamed, so named by its type"
        );
    }

    /// Nested multiparts number their sections through: this is what an
    /// alternative body beside an attachment looks like.
    #[test]
    fn nested_parts_are_numbered_through() {
        let bs = multipart(vec![
            multipart(vec![text("plain", 100), text("html", 200)]),
            single(
                "application",
                "zip",
                900,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("bundle.zip"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "2");
    }

    /// A text part is only an attachment when it says so — otherwise every
    /// message's own body would turn up in the gallery.
    #[test]
    fn a_text_part_counts_only_when_it_declares_itself() {
        let plain = multipart(vec![text("plain", 500)]);
        assert!(structure_attachments(&plain).is_empty());

        let attached = multipart(vec![BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", "csv", None),
                disposition: Some(disposition("attachment", Some("rows.csv"))),
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets: 700,
            },
            lines: 9,
            extension: None,
        }]);
        let found = structure_attachments(&attached);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "rows.csv");
        assert_eq!(found[0].size, 700, "not base64, so the octets stand");
    }

    /// The filename falls back to the content type's own `name` parameter,
    /// which is where older clients put it.
    #[test]
    fn a_name_parameter_is_used_when_there_is_no_filename() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "msword",
                600,
                ContentEncoding::SevenBit,
                Some(disposition("attachment", None)),
                None,
                Some("minutes.doc"),
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "minutes.doc");
    }

    /// An RFC 2047 filename is decoded, not shown as the encoded word.
    #[test]
    fn an_encoded_filename_is_decoded() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "pdf",
                600,
                ContentEncoding::SevenBit,
                Some(disposition(
                    "attachment",
                    Some("=?utf-8?Q?rapport=20final?="),
                )),
                None,
                None,
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "rapport final");
    }

    /// A message that is not multipart at all is one part, section 1.
    #[test]
    fn a_single_part_message_is_section_one() {
        let bs = single(
            "application",
            "pdf",
            800,
            ContentEncoding::SevenBit,
            Some(disposition("attachment", Some("lone.pdf"))),
            None,
            None,
        );
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "1");
    }

    #[test]
    fn only_image_types_get_a_guessed_extension() {
        assert_eq!(mime_extension("image", "jpeg"), ".jpg");
        assert_eq!(mime_extension("IMAGE", "PNG"), ".png");
        assert_eq!(mime_extension("application", "pdf"), "");
        assert_eq!(mime_extension("image", "svg+xml"), "");
    }

    #[test]
    fn large_cid_image_counts_toward_the_paperclip() {
        // BODYSTRUCTURE reports the base64 size: 4 characters per 3 bytes, padded.
        let octets = INLINE_ATTACHMENT_MIN.div_ceil(3) * 4;
        let raw = format!(
            concat!(
                "* 1 FETCH (BODYSTRUCTURE (",
                "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
                "(\"IMAGE\" \"JPEG\" (\"NAME\" \"pic.jpg\") \"<pic>\" NIL \"BASE64\" {} NIL ",
                "(\"INLINE\" (\"FILENAME\" \"pic.jpg\")) NIL NIL)",
                " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
            ),
            octets
        );
        assert!(structure_has_attachment(&bodystructure(&raw)));
    }

    /// Parse a real server BODYSTRUCTURE response into the structure our IMAP
    /// summary path sees.
    fn bodystructure(raw: &str) -> async_imap::imap_proto::types::BodyStructure<'_> {
        use async_imap::imap_proto::{parser::parse_response, AttributeValue, Response};
        let (_, resp) = parse_response(raw.as_bytes()).expect("parses");
        match resp {
            Response::Fetch(_, attrs) => attrs
                .into_iter()
                .find_map(|a| match a {
                    AttributeValue::BodyStructure(bs) => Some(bs),
                    _ => None,
                })
                .expect("has BODYSTRUCTURE"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn inline_iphone_photo_counts_as_an_attachment() {
        // Apple marks the JPEG `INLINE` with no Content-ID; requiring a disposition
        // of `attachment` missed it, so no paperclip and no download.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 4 2 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"JPEG\" (\"NAME\" \"a.jpeg\") NIL NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"a.jpeg\")) NIL NIL)",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 40 4 NIL NIL NIL NIL)",
            " \"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL NIL))\r\n",
        ));
        assert!(structure_has_attachment(&bs));
    }

    #[test]
    fn cid_logo_does_not_count_as_an_attachment() {
        // Same inline disposition, but a Content-ID: it's rendered in the body.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"PNG\" (\"NAME\" \"logo.png\") \"<logo>\" NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"logo.png\")) NIL NIL)",
            " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    #[test]
    fn plain_alternative_has_no_attachment() {
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            " \"ALTERNATIVE\" (\"BOUNDARY\" \"a\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    /// A meeting request's calendar part is an attachment, however the
    /// sender labelled it (#223): it arrives as a plain alternative with no
    /// disposition at all, which is the one text part that still counts.
    #[test]
    fn a_calendar_part_counts_as_an_attachment() {
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"CALENDAR\" (\"CHARSET\" \"utf-8\" \"METHOD\" \"REQUEST\") NIL NIL ",
            "\"7BIT\" 900 20 NIL NIL NIL NIL)",
            " \"ALTERNATIVE\" (\"BOUNDARY\" \"a\") NIL NIL NIL))\r\n",
        ));
        assert!(structure_has_attachment(&bs));
        let metas = structure_attachments(&bs);
        assert_eq!(metas.len(), 1, "only the calendar part: {metas:?}");
        assert_eq!(metas[0].mime, "text/calendar");
        assert_eq!(metas[0].section, "3");
        // Unnamed by the sender, as Outlook sends it: named for its type, so
        // something will open it.
        assert!(metas[0].name.ends_with(".ics"), "{}", metas[0].name);
    }
}
