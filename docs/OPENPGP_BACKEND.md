# OpenPGP backend: GnuPG, Sequoia or RNP

A proposal, not a decision. Hylki runs the system `gpg` for every OpenPGP
operation today. This page compares that with the two libraries a mail client
could embed instead, mainly for OpenPGP v6 (RFC 9580), and recommends a path.

## What Hylki needs from a backend

- Decrypt, verify, sign and encrypt PGP/MIME, read inline PGP.
- Key management: generate, import, export, delete, local trust.
- Two keyrings: the user's, and Hylki's own for keys found through Autocrypt,
  gossip and WKD, which must never mix.
- Secret keys and passphrases kept out of Hylki's process where possible.
- Something that works inside the Flatpak.

## The three candidates

| | GnuPG 2.4 / 2.5 (today) | Sequoia (`sequoia-openpgp`) | RNP (`librnp`) |
| --- | --- | --- | --- |
| Form | A program, driven over pipes | A Rust library | A C library, with Rust bindings by third parties |
| RFC 9580 (v6 keys, AEAD, Argon2) | No. GnuPG follows LibrePGP, which diverged from the IETF draft before v6 | Yes, since sequoia-openpgp 2.0 | In progress upstream; check the release in use |
| RFC 4880 (v4) interoperability | Yes | Yes | Yes |
| Secret keys | In gpg-agent, out of Hylki's process; pinentry asks for passphrases | In the process, unless the keystore crate (`sequoia-keystore`) or gpg-agent is used | In the process |
| Keyring | `~/.gnupg`, shared with every other GnuPG tool | Its own `pgp.cert.d` store, or anything Hylki writes; can read a GnuPG keybox | Its own files, or GnuPG's keybox (read) |
| Smartcards and tokens | Yes, through scdaemon | Through `openpgp-card` crates | Limited |
| Flatpak today | The runtime's gpg and the host's agent socket | Built into the binary | Built into the binary |
| Licence | GPL-3.0-or-later | LGPL-2.0-or-later | BSD-2-Clause |

## Trade-offs

- **GnuPG** is what users already have: their keys, their trust decisions,
  their smartcards and their agent all work unchanged, and Hylki never holds
  a secret key. Its costs are a text protocol of status lines, a process per
  operation, keyring locks (#316), and no path to v6: the GnuPG project has
  said it will not implement RFC 9580's v6 key format. A correspondent with a
  v6 key cannot be encrypted to.
- **Sequoia** is a Rust crate, so it fits the codebase, needs no process per
  operation, and supports v6 and v4. Hylki would have to take on key storage
  and passphrase prompts, or keep gpg-agent for secret keys through
  Sequoia's agent support. Users with keys in `~/.gnupg` would need them
  read from there.
- **RNP** is what Thunderbird uses. It brings a C dependency and its own
  keyring format into a Rust codebase, with less to gain over Sequoia.

## Recommendation

Keep GnuPG for the user's own keys, and move Hylki's own keyring of found
keys (Autocrypt, gossip, WKD; see `mail::pgp::discovery`) to Sequoia first.
That keyring only ever needs public keys: importing, listing, exporting and
encrypting to them, which Sequoia does in-process, with v6 support and no
second gpg home. Reading and writing the user's mail with their own key would
stay with gpg and its agent. Moving that part as well is worth it only once
v6 keys are common among Hylki's users, and is a maintainer decision because
it changes where secret keys live.
