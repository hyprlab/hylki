package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"strings"
	"testing"

	"github.com/ProtonMail/gluon/rfc822"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

func TestAPIFailureMapsEveryErrorToAFixedCode(t *testing.T) {
	cases := map[string]struct {
		err  error
		want string
	}{
		"deadline":           {fmt.Errorf("wrapped: %w", context.DeadlineExceeded), "timeout"},
		"unauthorized":       {&proton.APIError{Status: 401}, "session_revoked"},
		"refresh rejected":   {&proton.APIError{Status: 422, Code: proton.AuthRefreshTokenInvalid}, "session_revoked"},
		"throttled":          {&proton.APIError{Status: 429}, "transport_failed"},
		"server error":       {&proton.APIError{Status: 503}, "transport_failed"},
		"outdated":           {&proton.APIError{Status: 400, Code: proton.AppVersionMissingCode}, "client_outdated"},
		"human verification": {&proton.APIError{Status: 422, Code: proton.HumanVerificationRequired}, "human_verification_required"},
		"refused":            {&proton.APIError{Status: 422, Code: 2501, Message: "Message does not exist"}, "request_failed"},
		"net error":          {&proton.NetError{Cause: errors.New("x")}, "transport_failed"},
		"op error":           {&net.OpError{Op: "dial", Err: errors.New("refused")}, "transport_failed"},
		"dns":                {&net.DNSError{Name: "mail.proton.me"}, "transport_failed"},
		"anything else":      {errors.New("decoding failed"), "request_failed"},
	}
	for name, tc := range cases {
		if got := apiFailure(tc.err); got != tc.want {
			t.Errorf("%s: code = %q, want %q", name, got, tc.want)
		}
	}
}

func TestSubmitFailuresAreDefiniteOnlyWhenRefused(t *testing.T) {
	cases := map[string]struct {
		err        error
		deauthed   bool
		code       string
		isDefinite bool
	}{
		"validation":          {&proton.APIError{Status: 422, Code: 2001}, false, "invalid_recipient", true},
		"too large":           {&proton.APIError{Status: 422, Code: 2024}, false, "message_too_large", true},
		"no such address":     {&proton.APIError{Status: 422, Code: 33102}, false, "invalid_recipient", true},
		"other refusal":       {&proton.APIError{Status: 400, Code: 2500}, false, "send_failed", true},
		"throttled":           {&proton.APIError{Status: 429}, false, "transport_failed", true},
		"unauthorized":        {&proton.APIError{Status: 401}, false, "session_revoked", true},
		"refresh died":        {errors.New("failed to refresh auth"), true, "session_revoked", true},
		"server error":        {&proton.APIError{Status: 500}, false, "", false},
		"bad gateway":         {&proton.APIError{Status: 502}, false, "", false},
		"lost connection":     {&proton.NetError{Cause: errors.New("EOF")}, false, "", false},
		"deadline":            {context.DeadlineExceeded, false, "", false},
		"unparsed status":     {&proton.APIError{Status: 0}, false, "", false},
		"anything unexpected": {errors.New("?"), false, "", false},
	}
	for name, tc := range cases {
		code, isDefinite := definiteRefusal(tc.err, tc.deauthed)
		if code != tc.code || isDefinite != tc.isDefinite {
			t.Errorf("%s: = (%q, %v), want (%q, %v)", name, code, isDefinite, tc.code, tc.isDefinite)
		}
	}
}

func TestStaleEventIsAClientRefusal(t *testing.T) {
	for status, want := range map[int]bool{400: true, 404: true, 409: true, 422: true, 401: false, 429: false, 500: false} {
		if got := staleEvent(&proton.APIError{Status: status}); got != want {
			t.Errorf("status %d: stale = %v, want %v", status, got, want)
		}
	}
	if staleEvent(errors.New("network")) {
		t.Error("a transport error is not a stale event id")
	}
}

func TestIDsCannotSteerRequestPaths(t *testing.T) {
	for _, id := range []string{"0", "10", "abc-DEF_123==", "550e8400-e29b-41d4-a716-446655440000"} {
		if !validID(id) {
			t.Errorf("%q should be valid", id)
		}
	}
	for _, id := range []string{"", "a/b", "..", "a?b", "a b", "a#b", "%2e", strings.Repeat("a", 513)} {
		if validID(id) {
			t.Errorf("%q should be refused", id)
		}
	}
}

func TestAddressAndMessageIDShapes(t *testing.T) {
	for address, want := range map[string]bool{
		"a@b": true, "ada@proton.me": true, "": false, "a": false, "@b": false, "a@": false,
		"a@b@c": false, "a b@c": false, "a@b\n": false,
	} {
		if got := validAddress(address); got != want {
			t.Errorf("validAddress(%q) = %v, want %v", address, got, want)
		}
	}
	requested := "<abc@example.com>"
	if id, ok := messageID(&requested, "ada@proton.me"); !ok || id != "abc@example.com" {
		t.Errorf("messageID(%q) = %q, %v", requested, id, ok)
	}
	broken := "<a b@c>"
	if _, ok := messageID(&broken, "ada@proton.me"); ok {
		t.Error("a Message-ID with whitespace must be refused")
	}
	generated, ok := messageID(nil, "ada@proton.me")
	if !ok || !strings.HasSuffix(generated, "@proton.me") || len(generated) != 32+len("@proton.me") {
		t.Errorf("generated Message-ID = %q", generated)
	}
}

func TestAttachmentStaging(t *testing.T) {
	cid := " <logo> "
	staged, ok := stageAttachments([]sendAttachment{
		{Name: "", MIMEType: "TEXT/Plain", Data: "aGk="},
		{Name: "a\r\nInjected: yes.png", MIMEType: "image/png", CID: &cid, Data: ""},
		{Name: "x", MIMEType: "nonsense", Data: ""},
	})
	if !ok || len(staged) != 3 {
		t.Fatalf("staged = %+v, %v", staged, ok)
	}
	if staged[0].name != defaultAttachmentFilename || staged[0].mimeType != "text/plain" || string(staged[0].data) != "hi" ||
		staged[0].disposition != proton.AttachmentDisposition {
		t.Errorf("first = %+v", staged[0])
	}
	if staged[1].name != "aInjected: yes.png" || staged[1].cid != "logo" || staged[1].disposition != proton.InlineDisposition {
		t.Errorf("second = %+v", staged[1])
	}
	if staged[2].mimeType != "application/octet-stream" {
		t.Errorf("third = %+v", staged[2])
	}
	if _, ok := stageAttachments([]sendAttachment{{Data: "%%%"}}); ok {
		t.Error("undecodable data must fail")
	}
}

func TestHTMLToText(t *testing.T) {
	got := htmlToText("<html><head><style>p{}</style></head><body><p>Hello <b>there</b>,</p><p>line&nbsp;two<br>three &amp; four</p><script>x()</script></body></html>")
	// A no-break space is whitespace to the plain-text reader as well.
	want := "Hello there,\nline two\nthree & four\n"
	if got != want {
		t.Fatalf("htmlToText = %q, want %q", got, want)
	}
}

func TestSendPreferencesFollowTheOfficialTable(t *testing.T) {
	key, err := crypto.GenerateKey("R", "r@example.org", "x25519", 0)
	if err != nil {
		t.Fatal(err)
	}
	armored, err := key.GetArmoredPublicKey()
	if err != nil {
		t.Fatal(err)
	}
	apiKeys := []proton.PublicKey{{Flags: proton.KeyStateTrusted | proton.KeyStateActive, PublicKey: armored}}
	html := proton.MailSettings{DraftMIMEType: rfc822.TextHTML, PGPScheme: proton.PGPMIMEScheme}
	yes, no := true, false
	cases := map[string]struct {
		contact    proton.ContactSettings
		mail       proton.MailSettings
		keys       []proton.PublicKey
		isInternal bool
		scheme     proton.EncryptionScheme
		mimeType   rfc822.MIMEType
		encrypt    bool
	}{
		"proton recipient":         {proton.ContactSettings{}, html, apiKeys, true, proton.InternalScheme, rfc822.TextHTML, true},
		"outsider without keys":    {proton.ContactSettings{}, html, nil, false, proton.ClearScheme, rfc822.TextHTML, false},
		"outsider, signing on":     {proton.ContactSettings{}, proton.MailSettings{DraftMIMEType: rfc822.TextHTML, Sign: 1}, nil, false, proton.ClearMIMEScheme, rfc822.MultipartMixed, false},
		"outsider with a WKD key":  {proton.ContactSettings{}, html, apiKeys, false, proton.PGPMIMEScheme, rfc822.MultipartMixed, true},
		"WKD key, opted out":       {proton.ContactSettings{EncryptUntrusted: &no}, html, apiKeys, false, proton.ClearScheme, rfc822.TextHTML, false},
		"contact pins inline":      {proton.ContactSettings{Encrypt: &yes, Scheme: new(proton.PGPInlineScheme), Keys: []*crypto.Key{key}}, html, nil, false, proton.PGPInlineScheme, rfc822.TextPlain, true},
		"contact wants plain text": {proton.ContactSettings{MIMEType: new(rfc822.TextPlain)}, html, nil, false, proton.ClearScheme, rfc822.TextPlain, false},
	}
	for name, tc := range cases {
		prefs, err := buildSendPrefs(tc.contact, tc.mail, tc.keys, rfc822.TextHTML, tc.isInternal)
		if err != nil {
			t.Errorf("%s: %v", name, err)
			continue
		}
		if prefs.EncryptionScheme != tc.scheme || prefs.MIMEType != tc.mimeType || prefs.Encrypt != tc.encrypt {
			t.Errorf("%s: = (scheme %d, %s, encrypt %v), want (%d, %s, %v)", name,
				prefs.EncryptionScheme, prefs.MIMEType, prefs.Encrypt, tc.scheme, tc.mimeType, tc.encrypt)
		}
		if prefs.Encrypt && prefs.PubKey == nil {
			t.Errorf("%s: encrypting without a key", name)
		}
	}
	if _, err := buildSendPrefs(proton.ContactSettings{}, html, nil, rfc822.TextHTML, true); err == nil {
		t.Error("a Proton recipient without a key must not be sent to")
	}
	if _, err := buildSendPrefs(proton.ContactSettings{Encrypt: &yes}, html, nil, rfc822.TextHTML, false); err == nil {
		t.Error("a contact demanding encryption without a key must not fall back to cleartext")
	}
}
