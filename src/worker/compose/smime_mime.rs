//! S/MIME signing of an outgoing message (#478): the body as one MIME
//! entity, signed through gpgsm into `multipart/signed` (RFC 8551), laid out
//! byte for byte as OpenPGP's is.

use lettre::message::header::{ContentDisposition, ContentType};
use lettre::message::SinglePart;

use crate::config::AccountConfig;
use crate::mail::smime::{sign_detached, Gpgsm};
use crate::worker::compose::pgp_mime::{signed_multipart, PgpEntity};
use crate::worker::smtp::SmtpError;

/// `entity` signed with the certificate for the From address, else the
/// one for the account's own address (an alias the certificate does not
/// name still goes out signed, as Thunderbird signs it).
pub(super) fn smime_sign(
    gpgsm: &Gpgsm,
    account: &AccountConfig,
    from_addr: &str,
    entity: PgpEntity,
) -> Result<SinglePart, SmtpError> {
    let inner = entity.formatted();
    let (signature, micalg) = sign_detached(gpgsm, &inner, from_addr).or_else(|e| {
        if from_addr.eq_ignore_ascii_case(&account.email) {
            Err(e)
        } else {
            sign_detached(gpgsm, &inner, &account.email).map_err(|_| e)
        }
    })?;
    let sig_part = SinglePart::builder()
        .header(
            ContentType::parse("application/pkcs7-signature; name=\"smime.p7s\"")
                .expect("static content type"),
        )
        .header(ContentDisposition::attachment("smime.p7s"))
        .body(signature);
    signed_multipart(
        &inner,
        &sig_part.formatted(),
        "application/pkcs7-signature",
        micalg,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::sample_account;
    use base64::Engine;
    use std::io::Write;
    use std::process::Command;

    /// Stops the keybox's gpg-agent and removes it, also when the test
    /// fails: a live agent would outlast the test run.
    struct DropKeybox(std::path::PathBuf);

    impl Drop for DropKeybox {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .env("GNUPGHOME", &self.0)
                .args(["--kill", "all"])
                .status();
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A throwaway keybox holding a self-signed certificate for
    /// `sender@hylki.invalid` that gpgsm trusts, or `None` without gpgsm.
    fn keybox(home: &std::path::Path) -> Option<Gpgsm> {
        let gpgsm = Gpgsm::at(home.to_path_buf());
        if gpgsm.command().arg("--version").output().is_err() {
            return None;
        }
        let _ = std::fs::remove_dir_all(home);
        std::fs::create_dir_all(home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let params = home.join("params");
        std::fs::write(
            &params,
            "Key-Type: RSA\nKey-Length: 2048\nKey-Usage: sign\nSerial: random\n\
             Name-DN: CN=Sender\nName-Email: sender@hylki.invalid\n",
        )
        .unwrap();
        let cert = home.join("cert.der");
        // An empty passphrase, as an empty line, through loopback: no
        // pinentry in a test. A closed stdin instead leaves a key that
        // gpg-agent later calls corrupted.
        let mut gen = gpgsm
            .command()
            .args(["--pinentry-mode", "loopback", "--passphrase-fd", "0"])
            .arg("--generate-key")
            .arg("-o")
            .arg(&cert)
            .arg(&params)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        gen.stdin.take().unwrap().write_all(b"\n").unwrap();
        assert!(gen.wait().unwrap().success(), "certificate made");
        assert!(gpgsm
            .command()
            .arg("--import")
            .arg(&cert)
            .output()
            .unwrap()
            .status
            .success());
        let listing = gpgsm
            .command()
            .args(["--with-colons", "-K"])
            .output()
            .unwrap();
        let fpr = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .find(|l| l.starts_with("fpr:"))
            .and_then(|l| l.split(':').nth(9).map(str::to_string))
            .expect("a secret key");
        std::fs::write(home.join("trustlist.txt"), format!("{fpr} S relax\n")).unwrap();
        Some(gpgsm)
    }

    /// The bytes a `multipart/signed` covers and its decoded signature.
    fn split_signed(raw: &str) -> (String, Vec<u8>) {
        let boundary = raw
            .split("boundary=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a boundary");
        let open = format!("--{boundary}\r\n");
        let mid = format!("\r\n--{boundary}\r\n");
        let start = raw.find(&open).unwrap() + open.len();
        let end = start + raw[start..].find(&mid).unwrap();
        let sig_part = &raw[end + mid.len()..];
        let body = sig_part.split("\r\n\r\n").nth(1).unwrap();
        let b64: String = body
            .split(&format!("--{boundary}--"))
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        let sig = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .unwrap_or_else(|e| panic!("base64 signature: {e}\n{raw}"));
        (raw[start..end].to_string(), sig)
    }

    /// Signed through a real gpgsm: the message comes out as S/MIME
    /// multipart/signed whose signature gpgsm verifies over the exact
    /// bytes of the signed part, from the account's address and from an
    /// alias the certificate does not name. Skipped where gpgsm is missing.
    #[test]
    fn signs_into_multipart_signed_that_verifies() {
        let home = std::env::temp_dir().join(format!("hylki-gpgsm-{}", std::process::id()));
        let _cleanup = DropKeybox(home.clone());
        let Some(gpgsm) = keybox(&home) else {
            eprintln!("gpgsm not installed; skipping");
            return;
        };
        let mut account = sample_account();
        account.email = "sender@hylki.invalid".into();
        for from in ["sender@hylki.invalid", "alias@hylki.invalid"] {
            let entity = PgpEntity::Single(SinglePart::plain(String::from("signed body text")));
            let part = smime_sign(&gpgsm, &account, from, entity).expect("signed");
            let raw = String::from_utf8(part.formatted()).unwrap();
            assert!(
                raw.contains("protocol=\"application/pkcs7-signature\""),
                "{raw}"
            );
            assert!(raw.contains("micalg=\"sha-256\""), "{raw}");
            assert!(raw.contains("filename=\"smime.p7s\""), "{raw}");
            let (signed, sig) = split_signed(&raw);
            assert!(signed.contains("signed body text"), "{signed}");
            let (sig_path, data_path) = (home.join("sig.p7s"), home.join("data"));
            std::fs::write(&sig_path, sig).unwrap();
            std::fs::write(&data_path, signed).unwrap();
            let out = gpgsm
                .command()
                .arg("--verify")
                .arg(&sig_path)
                .arg(&data_path)
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&out.stderr);
            assert!(status.contains("[GNUPG:] GOODSIG"), "{status}");
        }
    }
}
