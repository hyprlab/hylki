# Security policy

Hylki reads untrusted content (every message it opens was written by someone
else), so security reports are welcome and taken seriously.

## Reporting a vulnerability

**Please report privately, not as a public issue.** Either channel works:

- **GitHub private vulnerability reporting:** <https://github.com/hyprlab/hylki/security/advisories/new>
  (Security → Report a vulnerability). This is the preferred route: it keeps the
  report, the discussion and the eventual advisory in one place.
- **Email:** hyprlab@proton.me

You should get a first reply within **5 days**. If you don't, assume the mail
went astray and open a public issue saying only that you're trying to reach a
maintainer privately, with no details.

Useful things to include, none of them required: affected version, the file and
line if you have it, what an attacker gains, and anything that reproduces it
(a `.eml` file is ideal for reader bugs).

## Disclosure

Report privately, and there is no deadline being run against you. The intent is
to ship a fix, publish a GitHub Security Advisory, and credit you by whatever
name and link you'd like, or not at all, if you'd rather. If you want to write
the finding up, say so and a date can be agreed; **90 days** is a reasonable
default if nothing else is proposed.

Nothing here is meant to restrict what you may say about your own research.

## Supported versions

Fixes go into the next release from `main`. Older versions are not patched
separately. Hylki ships as a Flatpak, an RPM and an Arch package, and all three
track releases.

## Scope

In scope, roughly in order of interest:

- Anything letting message content escape the reader's sandbox, run script, or
  read other messages.
- Anything defeating remote-content blocking, or otherwise causing a network
  request the user didn't ask for.
- Credential handling: the keyring, OAuth, what reaches disk.
- TLS: verification, downgrade, `STARTTLS` handling.
- Sandbox escapes and over-broad Flatpak permissions.
- File permissions on anything Hylki writes.

Out of scope: vulnerabilities in the user's own mail server, findings that need
an attacker who already has local code execution as the user, and reports
consisting only of scanner output.

## Notes on the design

Two things are deliberate and are not bugs:

- **Message bodies render with JavaScript disabled**, in a `sandbox`ed iframe
  without `allow-scripts`, under `default-src 'none'`. The wrapper document
  around them does run one script (it sizes the frames) and carries a
  nonce-based CSP so nothing else in that document can run.
- **Remote content is blocked by default** and the blocking follows the user's
  setting, not a guess about whether a message contains remote references. A
  detector miss should cost you the "blocked" banner, never the blocking.

If you find a case where either isn't true, that's exactly the kind of report
this file is for.
