package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ProtonMail/gluon/rfc822"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

const attachmentText = "attachment payload"

func sendRequest(session map[string]any, to []map[string]any, overrides map[string]any) map[string]any {
	request := map[string]any{
		"op":                 "send",
		"session":            session,
		"sender":             "alice@proton.local",
		"sender_name":        "Alice",
		"to":                 to,
		"cc":                 []map[string]any{},
		"bcc":                []map[string]any{},
		"reply_tos":          []map[string]any{},
		"subject":            "Greetings",
		"body_html":          "<p>Hello <b>there</b></p>",
		"delivery":           "ordinary",
		"ordinary_confirmed": true,
		"attachments": []map[string]any{{
			"name": "note.txt", "mime_type": "text/plain", "cid": nil,
			"data": base64.StdEncoding.EncodeToString([]byte(attachmentText)),
		}},
	}
	for key, value := range overrides {
		request[key] = value
	}
	return request
}

func to(address string) []map[string]any {
	return []map[string]any{{"name": "", "address": address}}
}

func inboxOf(t *testing.T, session map[string]any) []any {
	t.Helper()
	return result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 10}))["messages"].([]any)
}

func TestSendToProtonRecipientsEncryptsAndHonoursBcc(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	f.user("bob")
	f.user("carol")
	alice := loginAs(t, "alice")

	got := result(t, call(t, sendRequest(alice, to("bob@proton.local"), map[string]any{
		"bcc":        []map[string]any{{"name": "Carol", "address": "carol@proton.local"}},
		"message_id": "<greetings-1@proton.local>",
	})))
	if got["message_id"] != "greetings-1@proton.local" || got["internal_recipients"] != float64(2) || got["external_recipients"] != float64(0) {
		t.Fatalf("send result = %v", got)
	}
	sentID := got["sent_mail"].(string)
	// The in-process server leaves Time unset on a sent copy; the live API
	// stamps it. Only the field's type is the contract here.
	if _, ok := got["sent_date_ms"].(float64); !ok {
		t.Fatalf("sent_date_ms = %v", got["sent_date_ms"])
	}
	request := f.lastSend()
	if len(request.Packages) != 1 || request.Packages[0].Type != proton.InternalScheme || len(request.Packages[0].Addresses) != 2 {
		t.Fatalf("packages = %+v", request.Packages)
	}
	for address, recipient := range request.Packages[0].Addresses {
		if recipient.BodyKeyPacket == "" || len(recipient.AttachmentKeyPackets) != 1 || request.Packages[0].BodyKey != nil {
			t.Fatalf("%s is not end-to-end encrypted: %+v", address, recipient)
		}
	}

	sent := result(t, call(t, map[string]any{"op": "sync", "session": alice, "folder": proton.SentLabel, "limit": 10}))
	if !slices.Contains(ids(sent["messages"].([]any)), sentID) {
		t.Fatalf("the server did not file %s in Sent: %v", sentID, sent["messages"])
	}

	for _, name := range []string{"bob", "carol"} {
		session := loginAs(t, name)
		inbox := inboxOf(t, session)
		if len(inbox) != 1 {
			t.Fatalf("%s inbox = %v", name, inbox)
		}
		reply := result(t, call(t, map[string]any{"op": "message", "session": session, "id": ids(inbox)[0]}))
		parts := mimeParts(t, decodeMIME(t, reply["mime"]))
		if len(parts) != 2 || parts[0][0] != "text/html" || !strings.Contains(parts[0][2], "Hello <b>there</b>") {
			t.Fatalf("%s body parts = %v", name, parts)
		}
		if parts[1][1] != "attachment" || parts[1][2] != attachmentText {
			t.Fatalf("%s attachment part = %v", name, parts[1])
		}
		rows := reply["attachments"].([]any)
		if len(rows) != 1 || rows[0].(map[string]any)["name"] != "note.txt" || rows[0].(map[string]any)["data"] != nil {
			t.Fatalf("%s attachment rows = %v", name, rows)
		}
	}
}

func TestSendToAnExternalRecipientInCleartext(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := loginAs(t, "alice")
	var draftBody []byte
	f.setIntercept(func(_ http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
		if r.Method == http.MethodPost && r.URL.Path == draftsPath {
			draftBody, _ = io.ReadAll(r.Body)
			r.Body = io.NopCloser(bytes.NewReader(draftBody))
		}
		return false
	})

	got := result(t, call(t, sendRequest(alice, to("dave@example.com"), map[string]any{
		"reply_tos": []map[string]any{{"name": "Replies", "address": "replies@proton.local"}},
	})))
	if got["internal_recipients"] != float64(0) || got["external_recipients"] != float64(1) {
		t.Fatalf("counts = %v", got)
	}
	if !strings.HasSuffix(got["message_id"].(string), "@proton.local") {
		t.Fatalf("generated message_id = %v", got["message_id"])
	}
	var draft struct {
		Message struct {
			ReplyTos []struct{ Name, Address string }
			Body     string
		}
	}
	if err := json.Unmarshal(draftBody, &draft); err != nil {
		t.Fatalf("draft create body: %v", err)
	}
	if len(draft.Message.ReplyTos) != 1 || draft.Message.ReplyTos[0].Address != "replies@proton.local" {
		t.Fatalf("draft Reply-To = %+v", draft.Message.ReplyTos)
	}
	if !strings.HasPrefix(draft.Message.Body, "-----BEGIN PGP MESSAGE-----") {
		t.Fatal("the draft body must be stored encrypted")
	}
	request := f.lastSend()
	if len(request.Packages) != 1 {
		t.Fatalf("packages = %+v", request.Packages)
	}
	pkg := request.Packages[0]
	recipient := pkg.Addresses["dave@example.com"]
	if pkg.Type != proton.ClearScheme || recipient == nil || recipient.BodyKeyPacket != "" || pkg.BodyKey == nil || len(pkg.AttachmentKeys) != 1 {
		t.Fatalf("cleartext package = %+v", pkg)
	}
	key, err := base64.StdEncoding.DecodeString(pkg.BodyKey.Key)
	if err != nil {
		t.Fatal(err)
	}
	data, err := base64.StdEncoding.DecodeString(pkg.Body)
	if err != nil {
		t.Fatal(err)
	}
	plain, err := crypto.NewSessionKeyFromToken(key, pkg.BodyKey.Algorithm).Decrypt(data)
	if err != nil {
		t.Fatalf("server could not open the cleartext package: %v", err)
	}
	if plain.GetString() != "<p>Hello <b>there</b></p>" {
		t.Fatalf("cleartext body = %q", plain.GetString())
	}
	meta := f.account("alice").metadata(t, got["sent_mail"].(string))
	if !slices.Contains(meta.LabelIDs, proton.SentLabel) {
		t.Fatalf("sent copy labels = %v", meta.LabelIDs)
	}
}

func TestSendToAnExternalRecipientWithAKeyUsesPGPMIME(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := loginAs(t, "alice")
	key, err := crypto.GenerateKey("Erin", "erin@example.org", "x25519", 0)
	if err != nil {
		t.Fatal(err)
	}
	public, err := key.GetArmoredPublicKey()
	if err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.externalKeys["erin@example.org"] = public
	f.mu.Unlock()

	got := result(t, call(t, sendRequest(alice, to("erin@example.org"), map[string]any{"delivery": "secure", "ordinary_confirmed": false})))
	if got["external_recipients"] != float64(1) {
		t.Fatalf("counts = %v", got)
	}
	pkg := f.lastSend().Packages[0]
	recipient := pkg.Addresses["erin@example.org"]
	if pkg.Type != proton.PGPMIMEScheme || pkg.MIMEType != rfc822.MultipartMixed || recipient == nil || pkg.BodyKey != nil {
		t.Fatalf("PGP/MIME package = %+v", pkg)
	}
	keyPacket, err := base64.StdEncoding.DecodeString(recipient.BodyKeyPacket)
	if err != nil {
		t.Fatal(err)
	}
	ring, err := crypto.NewKeyRing(key)
	if err != nil {
		t.Fatal(err)
	}
	sessionKey, err := ring.DecryptSessionKey(keyPacket)
	if err != nil {
		t.Fatalf("recipient key cannot open the body key: %v", err)
	}
	data, _ := base64.StdEncoding.DecodeString(pkg.Body)
	plain, err := sessionKey.Decrypt(data)
	if err != nil {
		t.Fatal(err)
	}
	parts := mimeParts(t, plain.GetString())
	if len(parts) != 2 || parts[0][0] != "text/html" || parts[1][2] != attachmentText {
		t.Fatalf("encrypted MIME parts = %v", parts)
	}
}

func TestSendRefusesBeforeWritingAnything(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := loginAs(t, "alice")
	cases := map[string]struct {
		request map[string]any
		want    string
	}{
		"secure to a keyless outsider": {sendRequest(alice, to("dave@example.com"), map[string]any{"delivery": "secure"}), "confidential_send_unsupported"},
		"ordinary without consent":     {sendRequest(alice, to("dave@example.com"), map[string]any{"ordinary_confirmed": false}), "ordinary_delivery_requires_consent"},
		"no recipients":                {sendRequest(alice, []map[string]any{}, nil), "invalid_recipient"},
		"malformed recipient":          {sendRequest(alice, to("not an address"), nil), "invalid_recipient"},
		"someone else's sender":        {sendRequest(alice, to("dave@example.com"), map[string]any{"sender": "mallory@proton.local"}), "sender_unknown"},
		"undecodable attachment":       {sendRequest(alice, to("dave@example.com"), map[string]any{"attachments": []map[string]any{{"name": "x", "mime_type": "text/plain", "cid": nil, "data": "!!"}}}), "attachment_failed"},
		"unknown delivery":             {sendRequest(alice, to("dave@example.com"), map[string]any{"delivery": "whenever"}), "invalid_request"},
		"unknown field":                {sendRequest(alice, to("dave@example.com"), map[string]any{"priority": "high"}), "invalid_request"},
	}
	for name, tc := range cases {
		if got := errorCode(t, call(t, tc.request)); got != tc.want {
			t.Errorf("%s: code = %q, want %q", name, got, tc.want)
		}
	}
	if f.sendCount() != 0 {
		t.Fatal("a refused send reached the submit endpoint")
	}
	drafts := result(t, call(t, map[string]any{"op": "sync", "session": alice, "folder": proton.DraftsLabel, "limit": 10}))
	if len(drafts["messages"].([]any)) != 0 {
		t.Fatalf("a refused send left drafts: %v", drafts["messages"])
	}
}

// draftsOf lists the account's drafts directly.
func draftsOf(t *testing.T, f *fixture) []string {
	t.Helper()
	alice := f.account("alice")
	metas, err := alice.client.GetMessageMetadataPage(context.Background(), 0, pageSize, proton.MessageFilter{LabelID: proton.DraftsLabel})
	if err != nil {
		t.Fatal(err)
	}
	out := make([]string, len(metas))
	for i, meta := range metas {
		out[i] = meta.ID
	}
	return out
}

func isSubmit(r *http.Request) bool {
	return r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, sendPathPrefix) &&
		!strings.Contains(strings.TrimPrefix(r.URL.Path, sendPathPrefix), "/") && r.URL.Path != sendPathPrefix+"import"
}

// drop closes the connection without a response, as a lost link would.
func drop(t *testing.T, w http.ResponseWriter) {
	conn, _, err := w.(http.Hijacker).Hijack()
	if err != nil {
		t.Errorf("hijack: %v", err)
		return
	}
	_ = conn.Close()
}

func TestSendFailureModes(t *testing.T) {
	t.Run("a refusal is definite and removes the draft", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnprocessableEntity)
			_, _ = w.Write([]byte(`{"Code":2500,"Error":"server-side detail that must not leak"}`))
			return true
		})
		reply := call(t, sendRequest(alice, to("dave@example.com"), nil))
		if got := errorCode(t, reply); got != "send_failed" {
			t.Fatalf("code = %q", got)
		}
		if detail := reply["error"].(map[string]any); detail["session"] == nil || len(detail) != 2 {
			t.Fatalf("error detail = %v", detail)
		}
		if drafts := draftsOf(t, f); len(drafts) != 0 {
			t.Fatalf("draft survived a definite failure: %v", drafts)
		}
	})
	t.Run("too large is named", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnprocessableEntity)
			_, _ = w.Write([]byte(`{"Code":2024,"Error":"too large"}`))
			return true
		})
		if got := errorCode(t, call(t, sendRequest(alice, to("dave@example.com"), nil))); got != "message_too_large" {
			t.Fatalf("code = %q", got)
		}
	})
	t.Run("a lost submit is uncertain and keeps the draft", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			drop(t, w)
			return true
		})
		if got := errorCode(t, call(t, sendRequest(alice, to("dave@example.com"), nil))); got != "send_uncertain" {
			t.Fatalf("code = %q", got)
		}
		if drafts := draftsOf(t, f); len(drafts) != 1 {
			t.Fatalf("an uncertain send must keep its draft: %v", drafts)
		}
	})
	t.Run("a lost reply to a submit that went through is a success", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		delivered := false
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, forward func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			// The first attempt reaches the server; every answer is lost.
			if !delivered {
				delivered = true
				forward(&discardWriter{header: http.Header{}})
			}
			drop(t, w)
			return true
		})
		got := result(t, call(t, sendRequest(alice, to("dave@example.com"), nil)))
		if got["sent_mail"] == "" {
			t.Fatalf("result = %v", got)
		}
	})
	t.Run("a refusal of a retried submit never deletes the sent copy", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		attempts := 0
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, forward func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			attempts++
			if attempts == 1 {
				// Delivered, but the answer is lost: the client retries.
				forward(&discardWriter{header: http.Header{}})
				drop(t, w)
				return true
			}
			// The retry is refused because the draft is already sent.
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnprocessableEntity)
			_, _ = w.Write([]byte(`{"Code":2500,"Error":"already sent"}`))
			return true
		})
		got := result(t, call(t, sendRequest(alice, to("dave@example.com"), nil)))
		if attempts < 2 {
			t.Fatalf("the submit was not retried (%d attempt)", attempts)
		}
		meta := f.account("alice").metadata(t, got["sent_mail"].(string))
		if !slices.Contains(meta.LabelIDs, proton.SentLabel) {
			t.Fatalf("sent copy labels = %v", meta.LabelIDs)
		}
	})
	t.Run("a submit that outlives the budget is uncertain", func(t *testing.T) {
		f := newFixture(t)
		f.user("alice")
		alice := loginAs(t, "alice")
		f.setIntercept(func(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
			if !isSubmit(r) {
				return false
			}
			select {
			case <-r.Context().Done():
			case <-time.After(10 * time.Second):
			}
			return true
		})
		reply := callWithin(t, 3*time.Second, sendRequest(alice, to("dave@example.com"), nil))
		if got := errorCode(t, reply); got != "send_uncertain" {
			t.Fatalf("code = %q", got)
		}
	})
}

func TestTokenRotationReachesTheReply(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	f.srv.SetAuthLife(time.Second)
	session := loginAs(t, "alice")
	time.Sleep(1500 * time.Millisecond)

	got := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 1}))
	rotated := got["session"].(map[string]any)
	if rotated["access_token"] == session["access_token"] || rotated["refresh_token"] == session["refresh_token"] {
		t.Fatal("the reply still carries the pre-refresh tokens")
	}
	if rotated["key_pass"] != session["key_pass"] || rotated["login"] != "alice" {
		t.Fatalf("rotation lost session fields: %v", rotated)
	}
	// The old refresh token is spent: a caller that dropped the rotated
	// session would be signed out.
	time.Sleep(1500 * time.Millisecond)
	if code := errorCode(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 1})); code != "session_revoked" {
		t.Fatalf("stale session code = %q", code)
	}
	// A failure after a rotation still hands the new session back.
	reply := call(t, map[string]any{"op": "sync", "session": rotated, "folder": "bm9wZQ==", "limit": 1})
	if errorCode(t, reply) != "folder_not_found" {
		t.Fatalf("reply = %v", reply)
	}
	again := reply["error"].(map[string]any)["session"].(map[string]any)
	if again["refresh_token"] == rotated["refresh_token"] {
		t.Fatal("the error reply dropped the rotated session")
	}
}
