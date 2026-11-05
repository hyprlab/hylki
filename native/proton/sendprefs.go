package main

// Per-recipient send preferences, ported from Proton Mail Bridge
// (internal/services/smtp/smtp_prefs.go, GPL-3.0-or-later): the same
// decision table the official clients apply, so a recipient gets the same
// encryption, signing and MIME format from this helper as from Proton's own
// apps. Deviations: panics on malformed contact data are errors, and a key
// the API marks as no longer active is skipped while an active one exists.

import (
	"errors"

	"github.com/ProtonMail/gluon/rfc822"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

const (
	pgpInline  = "pgp-inline"
	pgpMIME    = "pgp-mime"
	pmInternal = "internal"
)

var errNoRecipientKey = errors.New("recipient needs a key but none was usable")

type contactPrefs struct {
	keys             []*crypto.Key
	scheme           string
	sign             bool
	isSignSet        bool
	encrypt          bool
	encryptUntrusted bool
	mimeType         rfc822.MIMEType
}

func newContactPrefs(settings proton.ContactSettings) (contactPrefs, error) {
	prefs := contactPrefs{encryptUntrusted: true}
	if settings.MIMEType != nil {
		prefs.mimeType = *settings.MIMEType
	}
	if settings.Sign != nil {
		prefs.sign = *settings.Sign
		prefs.isSignSet = true
	}
	if settings.Encrypt != nil {
		prefs.encrypt = *settings.Encrypt
	}
	if settings.EncryptUntrusted != nil {
		prefs.encryptUntrusted = *settings.EncryptUntrusted
	}
	if settings.Scheme != nil {
		switch *settings.Scheme {
		case proton.PGPMIMEScheme:
			prefs.scheme = pgpMIME
		case proton.PGPInlineScheme:
			prefs.scheme = pgpInline
		default:
			return contactPrefs{}, errors.New("unknown contact scheme")
		}
	}
	prefs.keys = settings.Keys
	return prefs, nil
}

func buildSendPrefs(
	contact proton.ContactSettings,
	mail proton.MailSettings,
	apiKeys []proton.PublicKey,
	mimeType rfc822.MIMEType,
	isInternal bool,
) (proton.SendPreferences, error) {
	vcard, err := newContactPrefs(contact)
	if err != nil {
		return proton.SendPreferences{}, err
	}
	builder := &sendPrefsBuilder{}
	if err := builder.setPGPSettings(vcard, apiKeys, isInternal); err != nil {
		return proton.SendPreferences{}, err
	}
	builder.setEncryptionPreferences(mail)
	builder.setMIMEPreferences(mimeType)
	return builder.build(), nil
}

type sendPrefsBuilder struct {
	internal  bool
	encrypt   *bool
	sign      *bool
	scheme    *string
	mimeType  *rfc822.MIMEType
	publicKey *crypto.KeyRing
}

func (b *sendPrefsBuilder) withEncrypt(v bool) { b.encrypt = &v }

func (b *sendPrefsBuilder) withEncryptDefault(v bool) {
	if b.encrypt == nil {
		b.encrypt = &v
	}
}

func (b *sendPrefsBuilder) shouldEncrypt() bool { return b.encrypt != nil && *b.encrypt }

func (b *sendPrefsBuilder) withSign(v bool) { b.sign = &v }

func (b *sendPrefsBuilder) withSignDefault() {
	if b.sign == nil {
		v := true
		b.sign = &v
	}
}

func (b *sendPrefsBuilder) shouldSign() bool { return b.sign != nil && *b.sign }

func (b *sendPrefsBuilder) withScheme(v string) { b.scheme = &v }

func (b *sendPrefsBuilder) withSchemeDefault(v string) {
	if b.scheme == nil {
		b.scheme = &v
	}
}

func (b *sendPrefsBuilder) getScheme() string {
	if b.scheme != nil {
		return *b.scheme
	}
	return ""
}

func (b *sendPrefsBuilder) withMIMEType(v rfc822.MIMEType) { b.mimeType = &v }

func (b *sendPrefsBuilder) withMIMETypeDefault(v rfc822.MIMEType) {
	if b.mimeType == nil {
		b.mimeType = &v
	}
}

func (b *sendPrefsBuilder) build() proton.SendPreferences {
	prefs := proton.SendPreferences{Encrypt: b.shouldEncrypt(), PubKey: b.publicKey}
	if b.mimeType != nil {
		prefs.MIMEType = *b.mimeType
	}
	if b.shouldSign() {
		prefs.SignatureType = proton.DetachedSignature
	} else {
		prefs.SignatureType = proton.NoSignature
	}
	switch {
	case b.internal:
		prefs.EncryptionScheme = proton.InternalScheme
	case b.shouldSign() && b.shouldEncrypt():
		if b.getScheme() == pgpInline {
			prefs.EncryptionScheme = proton.PGPInlineScheme
		} else {
			prefs.EncryptionScheme = proton.PGPMIMEScheme
		}
	case b.shouldSign() && !b.shouldEncrypt():
		if b.getScheme() == pgpInline {
			prefs.EncryptionScheme = proton.ClearScheme
		} else {
			prefs.EncryptionScheme = proton.ClearMIMEScheme
		}
	default:
		prefs.EncryptionScheme = proton.ClearScheme
	}
	return prefs
}

// setPGPSettings: a Proton recipient (the API says internal) is always
// end-to-end encrypted to its API key; an external recipient with keys the
// API found (WKD, key transparency) is PGP-encrypted unless the contact
// opted out; any other external recipient follows its contact card, which
// by default means cleartext.
func (b *sendPrefsBuilder) setPGPSettings(vcard contactPrefs, apiKeys []proton.PublicKey, isInternal bool) error {
	if isInternal {
		b.internal = true
		if len(apiKeys) == 0 {
			return errNoRecipientKey
		}
		b.withEncrypt(true)
		b.withSign(true)
		b.withScheme(pmInternal)
		if vcard.mimeType != "" {
			b.withMIMEType(vcard.mimeType)
		}
		key, err := pickSendingKey(vcard, apiKeys)
		if err != nil {
			return err
		}
		b.publicKey = key
		return nil
	}
	if usable := usableAPIKeys(apiKeys); len(usable) > 0 {
		b.withEncrypt(vcard.encryptUntrusted)
		if vcard.encryptUntrusted {
			b.withSign(true)
		} else if vcard.isSignSet {
			b.withSign(vcard.sign)
		}
		if vcard.scheme != "" {
			b.withScheme(vcard.scheme)
		}
		switch vcard.scheme {
		case pgpMIME:
			b.mimeType = nil
		case pgpInline:
			b.withMIMEType(rfc822.TextPlain)
		}
		key, err := pickSendingKey(vcard, usable)
		if err != nil {
			return err
		}
		b.publicKey = key
		return nil
	}
	b.withEncrypt(vcard.encrypt)
	if vcard.isSignSet {
		b.withSign(vcard.sign)
	}
	if vcard.encrypt {
		b.withSign(true)
	}
	if vcard.scheme != "" {
		b.withScheme(vcard.scheme)
	}
	if vcard.sign {
		switch vcard.scheme {
		case pgpMIME:
			b.mimeType = nil
		case pgpInline:
			b.withMIMEType(rfc822.TextPlain)
		}
	} else if vcard.mimeType != "" {
		b.withMIMEType(vcard.mimeType)
	}
	if len(vcard.keys) > 0 {
		key, err := crypto.NewKeyRing(vcard.keys[0])
		if err != nil {
			return err
		}
		b.publicKey = key
	}
	if vcard.encrypt && b.publicKey == nil {
		return errNoRecipientKey
	}
	return nil
}

// usableAPIKeys drops keys that do not parse, as Bridge does.
func usableAPIKeys(keys []proton.PublicKey) []proton.PublicKey {
	usable := make([]proton.PublicKey, 0, len(keys))
	for _, key := range keys {
		if _, err := crypto.NewKeyFromArmored(key.PublicKey); err == nil {
			usable = append(usable, key)
		}
	}
	return usable
}

// pickSendingKey prefers an API key the contact card pins, then the first
// active API key, then the first API key at all.
func pickSendingKey(vcard contactPrefs, rawAPIKeys []proton.PublicKey) (*crypto.KeyRing, error) {
	var apiKeys, activeKeys []*crypto.Key
	for _, raw := range rawAPIKeys {
		key, err := crypto.NewKeyFromArmored(raw.PublicKey)
		if err != nil {
			return nil, err
		}
		apiKeys = append(apiKeys, key)
		if raw.Flags&proton.KeyStateActive != 0 {
			activeKeys = append(activeKeys, key)
		}
	}
	if len(apiKeys) == 0 {
		return nil, errNoRecipientKey
	}
	pinned := make(map[string]bool, len(vcard.keys))
	for _, key := range vcard.keys {
		pinned[key.GetFingerprint()] = true
	}
	for _, key := range apiKeys {
		if pinned[key.GetFingerprint()] {
			return crypto.NewKeyRing(key)
		}
	}
	if len(activeKeys) > 0 {
		return crypto.NewKeyRing(activeKeys[0])
	}
	return crypto.NewKeyRing(apiKeys[0])
}

func (b *sendPrefsBuilder) setEncryptionPreferences(mail proton.MailSettings) {
	b.withEncryptDefault(false)
	if mail.Sign > 0 {
		b.withSignDefault()
	}
	if b.shouldEncrypt() {
		b.withSign(true)
	}
	switch mail.PGPScheme {
	case proton.PGPInlineScheme:
		b.withSchemeDefault(pgpInline)
	case proton.PGPMIMEScheme:
		b.withSchemeDefault(pgpMIME)
	}
	if b.shouldSign() && b.getScheme() == pgpInline {
		b.withMIMEType(rfc822.TextPlain)
	} else if mail.DraftMIMEType != "" {
		b.withMIMETypeDefault(mail.DraftMIMEType)
	}
}

func (b *sendPrefsBuilder) setMIMEPreferences(composerMIMEType rfc822.MIMEType) {
	switch {
	case !b.internal && b.shouldSign():
		if b.getScheme() == pgpInline {
			b.withMIMEType(rfc822.TextPlain)
		} else {
			b.withMIMEType(rfc822.MultipartMixed)
		}
	case composerMIMEType == rfc822.TextPlain:
		b.withMIMEType(rfc822.TextPlain)
	}
	// The composer's format is the last default: a recipient no setting
	// spoke for gets the body as written.
	b.withMIMETypeDefault(composerMIMEType)
}
