use super::inbound::{
    armored_block, autocrypt_key, detect, legacy_display_part, looks_like_mime,
    protected_header_block, protected_subject, signed_bytes, text_entity, unwrap_message, Shape,
};
use super::keys::{
    delete_key, export_public, generate_key, import_keys, import_summary, key_by_fingerprint,
    key_display, list_keys, parse_colons, public_key_for, secret_key_by_fingerprint,
    secret_key_for, trust_key, KeyValidity,
};
use super::outbound::{encrypt, sign_detached};
use super::status::{parse_status, signature_verdict, Outcome};
use super::{available, run, Gpg, Run};
use crate::models::{PgpSignature, PgpStatus, PgpTrust};

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn status_lines_distil_to_an_outcome() {
    let o = parse_status(&lines(&[
        "ENC_TO 1234567890ABCDEF 1 0",
        "DECRYPTION_OKAY",
        "GOODSIG 1234567890ABCDEF Ada Lovelace <ada@example.org>",
        "VALIDSIG 0123456789ABCDEF0123456789ABCDEF01234567 2026-09-07 1 0 4 0 1 8 00 0123456789ABCDEF0123456789ABCDEF01234567",
        "TRUST_ULTIMATE 0 pgp",
    ]));
    assert!(o.decryption_ok);
    assert_eq!(o.sig.as_ref().unwrap().0, "GOODSIG");
    assert_eq!(o.sig.as_ref().unwrap().2, "Ada Lovelace <ada@example.org>");
    assert_eq!(o.trust, Some(PgpTrust::Full));
    assert_eq!(
        o.fingerprint.as_deref(),
        Some("0123456789ABCDEF0123456789ABCDEF01234567")
    );

    let o = parse_status(&lines(&[
        "ERRSIG 1234567890ABCDEF 1 8 00 1757000000 9 -",
        "NO_PUBKEY 1234567890ABCDEF",
    ]));
    assert_eq!(o.missing_key.as_deref(), Some("1234567890ABCDEF"));
    assert!(o.sig.is_none());

    let o = parse_status(&lines(&[
        "ENC_TO AAAA 1 0",
        "NO_SECKEY AAAA",
        "DECRYPTION_FAILED",
    ]));
    assert!(!o.decryption_ok);
    assert_eq!(o.no_seckey, vec!["AAAA".to_string()]);
}

#[test]
fn verdicts_follow_the_status() {
    let mut notes = Vec::new();
    let o = parse_status(&lines(&["GOODSIG AA Ada <a@b.c>", "TRUST_UNDEFINED 0 pgp"]));
    match signature_verdict(&o, &mut notes) {
        PgpSignature::Good { signer, trust, .. } => {
            assert_eq!(signer, "Ada <a@b.c>");
            assert_eq!(trust, PgpTrust::Unknown);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(notes.len(), 1);
    let o = parse_status(&lines(&["BADSIG AA Ada <a@b.c>"]));
    assert!(matches!(
        signature_verdict(&o, &mut notes),
        PgpSignature::Bad { .. }
    ));
    let o = parse_status(&lines(&["EXPKEYSIG AA Ada <a@b.c>"]));
    assert!(matches!(
        signature_verdict(&o, &mut notes),
        PgpSignature::ExpiredKey { .. }
    ));
    let o = parse_status(&lines(&["NO_PUBKEY AA"]));
    assert!(matches!(
        signature_verdict(&o, &mut notes),
        PgpSignature::NoKey { .. }
    ));
    assert!(matches!(
        signature_verdict(&Outcome::default(), &mut notes),
        PgpSignature::None
    ));
}

#[test]
fn key_ids_read_in_groups_of_four() {
    assert_eq!(
        key_display("0123456789ABCDEF0123456789abcdef01234567"),
        "89AB CDEF 0123 4567"
    );
    assert_eq!(key_display("1234567890ABCDEF"), "1234 5678 90AB CDEF");
}

const SIGNED: &str = "From: a@b.c\r\nTo: d@e.f\r\nSubject: hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"bnd\"\r\n\r\n\
--bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line\r\n\
--bnd\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\n\r\n\
-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n--bnd--\r\n";

/// The signed bytes run from the part's first header to just before the
/// CRLF that precedes the boundary — exactly what the signer hashed.
#[test]
fn detects_pgp_mime_signed_and_takes_exact_bytes() {
    match detect(SIGNED.as_bytes()) {
        Some(Shape::Signed { data, signature }) => {
            assert_eq!(
                String::from_utf8(data).unwrap(),
                "Content-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line"
            );
            assert!(String::from_utf8(signature)
                .unwrap()
                .starts_with("-----BEGIN PGP SIGNATURE-----"));
        }
        other => panic!("{other:?}"),
    }
}

const ENCRYPTED: &str = "From: a@b.c\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
--enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
--enc\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n--enc--\r\n";

#[test]
fn detects_pgp_mime_encrypted_and_inline_forms() {
    match detect(ENCRYPTED.as_bytes()) {
        Some(Shape::Encrypted { armored }) => {
            let a = String::from_utf8(armored).unwrap();
            assert!(a.starts_with("-----BEGIN PGP MESSAGE-----"), "{a}");
        }
        other => panic!("{other:?}"),
    }
    assert!(detect(ENCRYPTED.as_bytes()).is_some_and(|s| s.is_encrypted()));
    assert!(!detect(SIGNED.as_bytes()).is_some_and(|s| s.is_encrypted()));

    let inline = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\nSee below\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n";
    match detect(inline.as_bytes()) {
        Some(Shape::InlineEncrypted { armored }) => {
            assert_eq!(
                String::from_utf8(armored).unwrap(),
                "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----"
            );
        }
        other => panic!("{other:?}"),
    }
    let clear = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n\r\nhi\r\n-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
    assert!(matches!(
        detect(clear.as_bytes()),
        Some(Shape::InlineSigned { .. })
    ));
    assert_eq!(
        detect(b"From: a@b.c\r\nContent-Type: text/plain\r\n\r\nplain mail\r\n"),
        None
    );
    // A plain message that merely quotes the markers is not a signed one.
    let html_only = "From: a@b.c\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n";
    assert_eq!(detect(html_only.as_bytes()), None);
}

#[test]
fn mime_versus_bare_text_is_told_by_the_first_line() {
    assert!(looks_like_mime(b"Content-Type: text/plain\r\n\r\nx"));
    assert!(looks_like_mime(
        b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n"
    ));
    assert!(!looks_like_mime(b"Hello there\r\n"));
    let e = text_entity(b"hi");
    assert!(e.starts_with(b"Content-Type: text/plain"));
    // Protected headers may come first; bare text that merely starts
    // like a header is still text.
    assert!(looks_like_mime(
        b"Subject: Plans\r\nFrom: a@b.c\r\nContent-Type: text/plain\r\n\r\nx"
    ));
    assert!(!looks_like_mime(b"Subject: Plans\r\n\r\nSee you at noon."));
    assert!(!looks_like_mime(
        b"Note: Content-Type: text/plain is fine\r\n"
    ));
}

/// The inner entity as Thunderbird sends it: protected headers on a
/// flagged multipart/mixed, then the legacy display part.
const PROTECTED: &str =
    "Content-Type: multipart/mixed; boundary=\"pp\";\r\n protected-headers=\"v1\"\r\n\
From: Ann <ann@example.com>\r\nTo: bob@example.com\r\n\
Subject: =?UTF-8?Q?Caf=C3=A9_at_noon?=\r\n\r\n\
--pp\r\nContent-Type: text/plain; charset=UTF-8; protected-headers=\"v1\"\r\n\
Content-Disposition: inline\r\n\r\nSubject: Caf\u{e9} at noon\r\n\r\n\
--pp\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nSee you there.\r\n--pp--\r\n";

#[test]
fn protected_subject_comes_from_the_inner_entity() {
    assert_eq!(
        protected_subject(PROTECTED.as_bytes()).as_deref(),
        Some("Caf\u{e9} at noon")
    );
    assert_eq!(
        protected_subject(b"Content-Type: text/plain\r\n\r\nhi"),
        None
    );
    assert_eq!(
        protected_subject(b"Subject:  \r\nContent-Type: text/plain\r\n\r\nhi"),
        None
    );
}

#[test]
fn legacy_display_part_is_found_only_under_protected_headers() {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default()
        .parse(PROTECTED.as_bytes())
        .unwrap();
    let first = parsed.root_part().sub_parts().unwrap()[0];
    assert_eq!(legacy_display_part(&parsed), Some(first));
    // Enigmail's form.
    let enigmail = PROTECTED.replace(
        "text/plain; charset=UTF-8; protected-headers",
        "text/rfc822-headers; protected-headers",
    );
    let parsed = MessageParser::default().parse(enigmail.as_bytes()).unwrap();
    assert_eq!(legacy_display_part(&parsed), Some(first));
    // RFC 9788's form.
    let rfc = PROTECTED
        .replace(
            "protected-headers=\"v1\"\r\n\r\nSubject",
            "hp-legacy-display=\"1\"\r\n\r\nSubject",
        )
        .replace("\r\n protected-headers=\"v1\"", "\r\n hp=\"cipher\"");
    let parsed = MessageParser::default().parse(rfc.as_bytes()).unwrap();
    assert_eq!(legacy_display_part(&parsed), Some(first));
    // Without the flag on the root, nothing is hidden.
    let plain = PROTECTED.replace(";\r\n protected-headers=\"v1\"", "");
    let parsed = MessageParser::default().parse(plain.as_bytes()).unwrap();
    assert_eq!(legacy_display_part(&parsed), None);
}

#[test]
fn protected_subject_is_never_serialized() {
    let status = PgpStatus {
        encrypted: true,
        decrypted: true,
        subject: Some("secret".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(!json.contains("secret"), "{json}");
    let back: PgpStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back.subject, None);
}

#[test]
fn colon_listing_becomes_keys() {
    let text = "\
tru::1:1757000000:0:3:1:5\n\
pub:u:255:22:7949BE459D0D7AF2:1750000000:0::u:::scESC::::::23::0:\n\
fpr:::::::::79277AB4A01A00F9574FD84E7949BE459D0D7AF2:\n\
uid:u::::1750000000::ABCDEF::jason@hyprlab.co <jason@hyprlab.co>::::::::::0:\n\
sub:u:255:18:94D25AE427F6BE67:1750000000:0:::::e::::::23:\n\
fpr:::::::::1111111111111111111111111194D25AE427F6BE67:\n\
pub:-:4096:1:51300FD844417A2E:1757000000:1820000000::-:::scSC::::::23::0:\n\
fpr:::::::::51300FD844417A2EF1828831DC8357B877CD8F05:\n\
uid:-::::1757000000::ABCDEF::Jason Martin <jasonjmartin@me.com>::::::::::0:\n\
uid:r::::1757000000::ABCDEF::Old Name <old@example.org>::::::::::0:\n";
    let keys = parse_colons(text, false);
    assert_eq!(keys.len(), 2);
    let k = &keys[0];
    assert_eq!(k.fingerprint, "79277AB4A01A00F9574FD84E7949BE459D0D7AF2");
    assert_eq!(k.key_id, "7949BE459D0D7AF2");
    assert!(k.can_encrypt && k.can_sign);
    assert_eq!(k.validity, KeyValidity::Ultimate);
    assert!(k.matches_address("Jason@Hyprlab.co"));
    assert_eq!(
        k.fingerprint_display(),
        "7927 7AB4 A01A 00F9 574F D84E 7949 BE45 9D0D 7AF2"
    );
    let k = &keys[1];
    assert!(!k.can_encrypt, "no encryption subkey");
    assert_eq!(k.expires, Some(1820000000));
    assert_eq!(k.validity, KeyValidity::Unknown);
    assert_eq!(
        k.uids,
        vec!["Jason Martin <jasonjmartin@me.com>"],
        "revoked uid dropped"
    );
    assert_eq!(k.emails(), vec!["jasonjmartin@me.com"]);
}

#[test]
fn keys_are_searched_by_uid_key_id_and_fingerprint() {
    let text = "\
pub:-:4096:1:51300FD844417A2E:1757000000:1820000000::-:::scSC::::::23::0:\n\
fpr:::::::::51300FD844417A2EF1828831DC8357B877CD8F05:\n\
uid:-::::1757000000::ABCDEF::Jason Martin <jasonjmartin@me.com>::::::::::0:\n\
uid:-::::1757000000::ABCDEF::Hyprlab <hyprlab@proton.me>::::::::::0:\n";
    let k = &parse_colons(text, false)[0];
    assert!(k.matches_search(""), "an empty search shows everything");
    assert!(k.matches_search("jason"));
    assert!(k.matches_search("MARTIN me.com"), "every word, any case");
    assert!(k.matches_search("proton"), "not only the primary uid");
    assert!(k.matches_search("0x51300fd844417a2e"));
    assert!(k.matches_search("DC83 57B8 77CD 8F05"));
    assert!(!k.matches_search("jason gmail"));
    assert!(!k.matches_search("0x1234"));
}

#[test]
fn import_status_is_summarised() {
    let out = Run {
        status_lines: lines(&[
            "IMPORT_OK 1 AAAA",
            "IMPORT_OK 0 BBBB",
            "IMPORT_RES 2 0 1 0 0 0 0 0 0 0 0 0 0 0 0",
        ]),
        ..Default::default()
    };
    let s = import_summary(&out);
    assert_eq!(s.imported, 1);
    assert_eq!(s.unchanged, 1);
    assert_eq!(s.fingerprints, vec!["AAAA", "BBBB"]);
}

#[test]
fn autocrypt_header_yields_the_key() {
    let key = b"\x98\x01\x02binarykey";
    let b64 = crate::encoding::base64_encode(key);
    let raw = format!(
        "From: a@b.c\r\nAutocrypt: addr=a@b.c; prefer-encrypt=mutual;\r\n keydata={}\r\nContent-Type: text/plain\r\n\r\nhi\r\n",
        b64
    );
    assert_eq!(autocrypt_key(raw.as_bytes()).as_deref(), Some(&key[..]));
    assert_eq!(autocrypt_key(b"From: a@b.c\r\n\r\nhi"), None);
}

/// The end-to-end path against a real gpg with a throwaway keyring:
/// encrypt-and-sign a MIME entity, then watch it come back decrypted with
/// the signature verified. Skipped where gpg is missing.
#[test]
fn round_trip_through_a_throwaway_keyring() {
    if !available() {
        eprintln!("gpg not installed; skipping");
        return;
    }
    let home = std::env::temp_dir().join(format!("hylki-gpg-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let gpg = Gpg {
        home: Some(home.clone()),
    };
    let status = |args: &[&str], input: &[u8]| -> Run { run(&gpg, args, input, None) };
    let gen = status(
        &[
            "--passphrase",
            "",
            "--pinentry-mode",
            "loopback",
            "--quick-gen-key",
            "Test Hylki <test@hylki.invalid>",
            "default",
            "default",
            "never",
        ],
        b"",
    );
    assert!(
        gen.status_lines
            .iter()
            .any(|l| l.starts_with("KEY_CREATED")),
        "{gen:?}"
    );
    let entity = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsecret hello\r\n";
    let enc = status(
        &[
            "--armor",
            "--trust-model",
            "always",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--recipient",
            "test@hylki.invalid",
            "--sign",
            "--encrypt",
        ],
        entity,
    );
    let armored = String::from_utf8(enc.stdout.clone()).unwrap();
    assert!(
        armored.contains("-----BEGIN PGP MESSAGE-----"),
        "{:?}",
        enc.status_lines
    );
    let mail = format!(
        "From: test@hylki.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
         --enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
         --enc\r\nContent-Type: application/octet-stream\r\n\r\n{armored}\r\n--enc--\r\n"
    );
    let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
    assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
    let inner = String::from_utf8(u.inner.clone().unwrap()).unwrap();
    assert!(inner.contains("secret hello"), "{inner}");
    assert!(
        matches!(
            u.status.signature,
            PgpSignature::Good {
                trust: PgpTrust::Full,
                ..
            }
        ),
        "{:?}",
        u.status
    );
    assert_eq!(u.status.subject, None);

    // Protected headers (#493): the subject inside replaces the "..." outside.
    let enc = status(
        &[
            "--armor",
            "--trust-model",
            "always",
            "--recipient",
            "test@hylki.invalid",
            "--encrypt",
        ],
        PROTECTED.as_bytes(),
    );
    let armored = String::from_utf8(enc.stdout.clone()).unwrap();
    let mail = format!(
        "From: test@hylki.invalid\r\nSubject: ...\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
         --enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
         --enc\r\nContent-Type: application/octet-stream\r\n\r\n{armored}\r\n--enc--\r\n"
    );
    let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
    assert!(u.status.decrypted, "{:?}", u.status);
    assert_eq!(u.status.subject.as_deref(), Some("Caf\u{e9} at noon"));

    // Detached signature over a MIME part, verified the RFC 3156 way.
    let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsigned hello";
    let sig = status(
        &[
            "--armor",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--detach-sign",
        ],
        part,
    );
    let sig = String::from_utf8(sig.stdout).unwrap();
    let mail = format!(
        "From: test@hylki.invalid\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n\
         --b\r\n{}\r\n--b\r\nContent-Type: application/pgp-signature\r\n\r\n{sig}\r\n--b--\r\n",
        String::from_utf8_lossy(part)
    );
    let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
    assert!(!u.status.encrypted);
    assert!(
        matches!(u.status.signature, PgpSignature::Good { .. }),
        "{:?}",
        u.status
    );
    assert_eq!(u.inner.unwrap(), part.to_vec());

    // A tampered signed part fails.
    let mail = mail.replace("signed hello", "signed hellO");
    let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
    assert!(
        matches!(u.status.signature, PgpSignature::Bad { .. }),
        "{:?}",
        u.status
    );

    // Encrypted to a key we don't have: reported, nothing rendered.
    let other = "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----";
    let mail = format!("From: x@y.z\r\nContent-Type: text/plain\r\n\r\n{other}\r\n");
    let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
    assert!(u.status.encrypted && !u.status.decrypted);
    assert!(u.inner.is_none());

    // Key management on the same keyring.
    let mine = list_keys(&gpg, true);
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert!(mine[0].can_encrypt && mine[0].secret);
    assert!(secret_key_for(&gpg, "TEST@hylki.invalid").is_some());
    assert!(public_key_for(&gpg, "test@hylki.invalid").is_some());
    assert!(public_key_for(&gpg, "nobody@hylki.invalid").is_none());
    let fpr =
        generate_key(&gpg, "Second Key", "second@hylki.invalid", "1y", "pw").expect("generated");
    let second = key_by_fingerprint(&gpg, &fpr).expect("listed");
    assert!(
        second.can_encrypt,
        "default default gives an encryption subkey: {second:?}"
    );
    assert!(second.expires.is_some());
    let armored = export_public(&gpg, &fpr).expect("exported");
    assert!(String::from_utf8_lossy(&armored).contains("BEGIN PGP PUBLIC KEY BLOCK"));
    // Into a second, empty keyring: imported, untrusted, then vouched for.
    let home2 = std::env::temp_dir().join(format!("hylki-gpg-test2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home2);
    std::fs::create_dir_all(&home2).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home2, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let gpg2 = Gpg {
        home: Some(home2.clone()),
    };
    let s = import_keys(&gpg2, &armored).expect("imported");
    assert_eq!(s.imported, 1);
    assert_eq!(s.fingerprints, vec![fpr.clone()]);
    assert_eq!(import_keys(&gpg2, &armored).unwrap().unchanged, 1);
    assert!(import_keys(&gpg2, b"not a key").is_err());
    assert_eq!(
        key_by_fingerprint(&gpg2, &fpr).unwrap().validity,
        KeyValidity::Unknown
    );
    assert!(
        trust_key(&gpg2, &fpr, None).is_err(),
        "no key of one's own to sign with"
    );
    let own = generate_key(&gpg2, "", "me@hylki.invalid", "never", "").expect("own key");
    trust_key(&gpg2, &fpr, Some(&own)).expect("signed");
    assert!(key_by_fingerprint(&gpg2, &fpr).unwrap().validity.trusted());
    // Encrypt to the imported key from the second keyring; the first opens it.
    let ct = encrypt(
        &gpg2,
        b"Content-Type: text/plain\r\n\r\nfor second",
        &["second@hylki.invalid".into()],
        Some(&own),
    )
    .expect("encrypted");
    let mail = format!("From: me@hylki.invalid\r\nMIME-Version: 1.0\r\nContent-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\r\n\r\n--e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--e\r\nContent-Type: application/octet-stream\r\n\r\n{}\r\n--e--\r\n", String::from_utf8_lossy(&ct));
    // The second key has a passphrase, fed through the loopback for the test.
    let gpg_pw = Gpg {
        home: Some(home.clone()),
    };
    let out = run(
        &gpg_pw,
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "pw",
            "--decrypt",
        ],
        &ct,
        None,
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("for second"),
        "{:?}",
        out.status_lines
    );
    let _ = mail;
    let (sig, micalg) = sign_detached(&gpg2, b"data", &own).expect("signed");
    assert!(String::from_utf8_lossy(&sig).contains("BEGIN PGP SIGNATURE"));
    assert!(micalg.starts_with("pgp-sha"), "{micalg}");
    assert!(secret_key_by_fingerprint(&gpg2, &own).is_some());
    delete_key(&gpg2, &fpr, false).expect("deleted");
    assert!(key_by_fingerprint(&gpg2, &fpr).is_none());
    delete_key(&gpg2, &own, true).expect("deleted own");
    assert!(list_keys(&gpg2, true).is_empty());

    for h in [&home, &home2] {
        let _ = std::process::Command::new("gpgconf")
            .env("GNUPGHOME", h)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(h);
    }
}

// Characterization tests (restructure, area D).

#[test]
fn pinned_armored_blocks_and_entities() {
    let text = "pre\n-----BEGIN PGP MESSAGE-----\nabc\n-----END PGP MESSAGE-----\npost";
    assert_eq!(
        armored_block(text, "MESSAGE"),
        "-----BEGIN PGP MESSAGE-----\nabc\n-----END PGP MESSAGE-----"
    );
    assert_eq!(armored_block("no block", "MESSAGE"), "no block");
    assert_eq!(
        text_entity(b"hi"),
        b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\nhi"
            .to_vec()
    );
}

#[test]
fn pinned_signed_bytes_end_at_the_first_boundary_prefix() {
    let crlf = b"Content-Type: x\r\n\r\nbody\r\n--b\r\n";
    assert_eq!(
        signed_bytes(crlf, 0, "b").as_deref(),
        Some(&b"Content-Type: x\r\n\r\nbody"[..])
    );
    assert_eq!(
        signed_bytes(b"head\n\nbody\n--b\n", 0, "b").as_deref(),
        Some(&b"head\n\nbody"[..])
    );
    // An inner boundary that starts with the outer one cuts it short.
    assert_eq!(
        signed_bytes(b"a\r\n--bb\r\ninner\r\n--b\r\n", 0, "b").as_deref(),
        Some(&b"a"[..])
    );
    assert_eq!(signed_bytes(b"abc", 3, "b"), None);
    assert_eq!(signed_bytes(b"abc", 0, "b"), None);
}

#[test]
fn pinned_protected_header_blocks() {
    assert!(protected_header_block(
        "Subject: x\r\nContent-Type: text/plain\r\n\r\nbody"
    ));
    assert!(!protected_header_block("Subject: x\n\nbody"));
    assert!(!protected_header_block("not a header line\n"));
    assert!(!protected_header_block("Content-Type: a\n folded\n"));
    assert!(protected_header_block("Content-Type: a\n folded\n\n"));
}

#[test]
fn pinned_colon_uids_unescape_only_colons() {
    let text = "\
pub:u:255:22:ABCDEF0123456789:1700000000:::u:::scESC:\n\
fpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\n\
uid:u::::1700000000::HASH::Ann \\x5c Bee \\x3a x <ann@example.org>::::::::::0:\n\
pub:e:255:22:1111222233334444:1700000000:1::e:::scD:\n\
fpr:::::::::AAAABBBBCCCCDDDDEEEEFFFF1111222233334444:\n";
    let keys = parse_colons(text, true);
    assert_eq!(keys.len(), 2);
    let k = &keys[0];
    assert_eq!(k.uids, vec!["Ann \\x5c Bee : x <ann@example.org>"]);
    assert_eq!(k.primary_uid(), "Ann \\x5c Bee : x <ann@example.org>");
    assert_eq!(k.emails(), vec!["ann@example.org"]);
    assert!(k.matches_address(" ANN@example.org "));
    assert!(k.usable() && k.secret && k.can_sign && k.can_encrypt);
    assert_eq!(
        k.fingerprint_display(),
        "0123 4567 89AB CDEF 0123 4567 89AB CDEF 0123 4567"
    );
    let old = &keys[1];
    assert_eq!(old.expires, Some(1));
    assert_eq!(old.validity, KeyValidity::Expired);
    assert!(old.disabled && !old.usable() && old.uids.is_empty());
    assert_eq!(old.primary_uid(), "");
    assert!(!KeyValidity::Marginal.trusted() && KeyValidity::Full.trusted());
}
