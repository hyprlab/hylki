package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	proton "github.com/ProtonMail/go-proton-api"
	"github.com/emersion/go-message"
)

func TestLoginReturnsAResumableSessionWithTheKeyPassphrase(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	session := loginAs(t, "alice")
	for _, key := range []string{"login", "user_id", "uid", "access_token", "refresh_token", "key_pass"} {
		if value, _ := session[key].(string); value == "" {
			t.Errorf("session.%s is empty", key)
		}
	}
	if session["login"] != "alice" || session["password_mode"] != float64(proton.OnePasswordMode) {
		t.Errorf("session identity = %v / %v", session["login"], session["password_mode"])
	}
	for key, value := range session {
		if s, ok := value.(string); ok && strings.Contains(s, fixturePassword) {
			t.Errorf("session.%s carries the password", key)
		}
	}
}

func TestLoginRejectsAWrongPassword(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	reply := call(t, map[string]any{"op": "login", "email": "alice", "password": "wrong", "totp": nil})
	if got := errorCode(t, reply); got != "login_failed" {
		t.Fatalf("code = %q, want login_failed", got)
	}
}

// asTwoPassword rewrites the auth reply to say the account has a separate
// mailbox password. The fixture's keys are locked with the login password,
// so that same string is the "mailbox password" that opens them.
func asTwoPassword(w http.ResponseWriter, r *http.Request, forward func(http.ResponseWriter)) bool {
	if r.Method != http.MethodPost || r.URL.Path != "/auth/v4" {
		return false
	}
	recorder := httptest.NewRecorder()
	forward(recorder)
	var auth map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &auth); err != nil {
		w.WriteHeader(recorder.Code)
		_, _ = w.Write(recorder.Body.Bytes())
		return true
	}
	auth["PasswordMode"] = int(proton.TwoPasswordMode)
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(recorder.Code)
	_ = json.NewEncoder(w).Encode(auth)
	return true
}

func TestLoginWithASeparateMailboxPassword(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	f.setIntercept(asTwoPassword)
	login := map[string]any{"op": "login", "email": "alice", "password": fixturePassword, "totp": nil}
	if got := errorCode(t, call(t, login)); got != "mailbox_password_required" {
		t.Fatalf("without a mailbox password: code = %q", got)
	}
	login["mailbox_password"] = "not it"
	if got := errorCode(t, call(t, login)); got != "mailbox_password_failed" {
		t.Fatalf("wrong mailbox password: code = %q", got)
	}
	login["mailbox_password"] = fixturePassword
	session := result(t, call(t, login))["session"].(map[string]any)
	if session["password_mode"] != float64(proton.TwoPasswordMode) || session["key_pass"] == "" {
		t.Fatalf("session = %v", session)
	}
}

func TestRefusalsTheHelperCannotResolveAreNamed(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	session := loginAs(t, "alice")
	refuse := func(status int, body string) {
		f.setIntercept(func(w http.ResponseWriter, _ *http.Request, _ func(http.ResponseWriter)) bool {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(status)
			_, _ = w.Write([]byte(body))
			return true
		})
	}
	refuse(http.StatusUnprocessableEntity, `{"Code":9001,"Error":"captcha please","Details":{"HumanVerificationToken":"secret"}}`)
	reply := call(t, map[string]any{"op": "login", "email": "alice", "password": fixturePassword, "totp": nil})
	if got := errorCode(t, reply); got != "human_verification_required" {
		t.Fatalf("human verification: code = %q", got)
	}
	refuse(http.StatusBadRequest, `{"Code":5003,"Error":"update the app"}`)
	if got := errorCode(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 1})); got != "client_outdated" {
		t.Fatalf("outdated client: code = %q", got)
	}
	// 500, not 503: resty waits out a 503's Retry-After, which would only
	// slow the test down.
	refuse(http.StatusInternalServerError, `{"Code":500,"Error":"down"}`)
	if got := errorCode(t, call(t, map[string]any{"op": "message", "session": session, "id": "abc"})); got != "transport_failed" {
		t.Fatalf("outage: code = %q", got)
	}
}

func TestSyncListsFoldersPagesAndTheIDSnapshot(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := f.account("alice")
	first := alice.deliver(t, "first", "one", false)
	second := alice.deliver(t, "second", "two", true)
	third := alice.deliver(t, "third", "three", true)
	session := loginAs(t, "alice")

	got := result(t, call(t, map[string]any{"op": "sync", "session": session, "folder": nil, "limit": 2, "offset": 0}))
	if got["folder"] != proton.InboxLabel || got["exists"] != float64(3) || got["offset"] != float64(0) {
		t.Fatalf("folder/exists/offset = %v/%v/%v", got["folder"], got["exists"], got["offset"])
	}
	if page := ids(got["messages"].([]any)); !slices.Equal(page, []string{third, second}) {
		t.Fatalf("first page = %v, want newest first", page)
	}
	snapshot := got["all_ids"].([]any)
	if len(snapshot) != 3 {
		t.Fatalf("all_ids = %v", snapshot)
	}
	if got["event_id"] == "" || got["next_cursor"] != second {
		t.Fatalf("event_id/next_cursor = %v/%v", got["event_id"], got["next_cursor"])
	}
	header := got["messages"].([]any)[0].(map[string]any)
	if header["subject"] != "third" || header["is_unread"] != true {
		t.Fatalf("header = %v", header)
	}
	if sender := header["sender"].(map[string]any); sender["address"] != "sender@example.com" || sender["name"] != "Sender" {
		t.Fatalf("sender = %v", sender)
	}
	if recipient := header["recipient"].(map[string]any); recipient["address"] != "alice@proton.local" {
		t.Fatalf("recipient = %v", recipient)
	}
	// The in-process server keeps neither the Date nor the Message-ID of
	// an import, so only their presence and type are checked here.
	if _, ok := header["date_ms"].(float64); !ok {
		t.Fatalf("date_ms = %v", header["date_ms"])
	}
	if _, ok := header["message_id"].(string); !ok {
		t.Fatalf("message_id = %v", header["message_id"])
	}
	if labels := header["label_ids"].([]any); !slices.Contains(labels, any(proton.InboxLabel)) {
		t.Fatalf("label_ids = %v", labels)
	}
	roles := map[string]string{}
	for _, row := range got["folders"].([]any) {
		folder := row.(map[string]any)
		roles[folder["id"].(string)] = folder["role"].(string)
		if folder["id"] == proton.InboxLabel && (folder["total"] != float64(3) || folder["unread"] != float64(2)) {
			t.Errorf("inbox counts = %v/%v", folder["total"], folder["unread"])
		}
	}
	for id, role := range map[string]string{"0": "inbox", "7": "sent", "8": "drafts", "3": "trash", "4": "junk", "6": "archive", "10": "starred", "5": "other"} {
		if roles[id] != role {
			t.Errorf("folder %s role = %q, want %q", id, roles[id], role)
		}
	}

	next := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 2, "cursor": second}))
	if page := ids(next["messages"].([]any)); !slices.Equal(page, []string{first}) || next["all_ids"] != nil || next["next_cursor"] != nil {
		t.Fatalf("cursor page = %v, all_ids %v, next %v", page, next["all_ids"], next["next_cursor"])
	}
	offset := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 2, "offset": 2}))
	if page := ids(offset["messages"].([]any)); !slices.Equal(page, []string{first}) || offset["all_ids"] != nil {
		t.Fatalf("offset page = %v, all_ids %v", page, offset["all_ids"])
	}
	empty := result(t, call(t, map[string]any{"op": "sync", "session": session, "folder": proton.TrashLabel, "limit": 10, "offset": 0}))
	if len(empty["messages"].([]any)) != 0 || len(empty["all_ids"].([]any)) != 0 {
		t.Fatalf("empty folder = %v", empty)
	}
	if code := errorCode(t, call(t, map[string]any{"op": "sync", "session": session, "folder": "nope", "limit": 1})); code != "folder_not_found" {
		t.Fatalf("unknown folder code = %q", code)
	}
}

func TestSyncSinceAnEventReportsChanges(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := f.account("alice")
	kept := alice.deliver(t, "kept", "one", true)
	doomed := alice.deliver(t, "doomed", "two", true)
	session := loginAs(t, "alice")
	since := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 10}))["event_id"].(string)

	if err := alice.client.MarkMessagesRead(context.Background(), kept); err != nil {
		t.Fatal(err)
	}
	if err := alice.client.DeleteMessage(context.Background(), doomed); err != nil {
		t.Fatal(err)
	}
	arrived := alice.deliver(t, "arrived", "three", true)

	got := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 10, "since": since}))
	changes := got["changes"].(map[string]any)
	if changes["refresh"] != false || got["all_ids"] != nil {
		t.Fatalf("changes = %v, all_ids = %v", changes, got["all_ids"])
	}
	updated := map[string]map[string]any{}
	for _, row := range changes["updated"].([]any) {
		header := row.(map[string]any)
		updated[header["id"].(string)] = header
	}
	if updated[kept] == nil || updated[kept]["is_unread"] != false || updated[arrived] == nil {
		t.Fatalf("updated = %v", updated)
	}
	if deleted := changes["deleted"].([]any); !slices.Contains(deleted, any(doomed)) {
		t.Fatalf("deleted = %v", deleted)
	}
	if got["event_id"] == since {
		t.Fatal("event id did not advance")
	}

	stale := result(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 10, "since": "abc"}))
	if stale["changes"].(map[string]any)["refresh"] != true || stale["event_id"] == "" {
		t.Fatalf("an unknown event id must ask for a refresh: %v", stale["changes"])
	}
}

func TestMessageReturnsDecryptedMIME(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := f.account("alice")
	id := alice.deliver(t, "hello", "Grüße from the fixture", true)
	session := loginAs(t, "alice")

	got := result(t, call(t, map[string]any{"op": "message", "session": session, "id": id}))
	mime := decodeMIME(t, got["mime"])
	entity, err := message.Read(strings.NewReader(mime))
	if err != nil {
		t.Fatalf("mime does not parse: %v", err)
	}
	if subject := entity.Header.Get("Subject"); subject != "hello" {
		t.Fatalf("Subject = %q", subject)
	}
	parts := mimeParts(t, mime)
	if len(parts) != 1 || parts[0][0] != "text/plain" || !strings.Contains(parts[0][2], "Grüße from the fixture") {
		t.Fatalf("parts = %q", parts)
	}
	if got["header"].(map[string]any)["id"] != id || got["session"] == nil {
		t.Fatalf("header/session = %v/%v", got["header"], got["session"])
	}
	if code := errorCode(t, call(t, map[string]any{"op": "message", "session": session, "id": "../core/v4/users"})); code != "invalid_id" {
		t.Fatalf("path-shaped id code = %q", code)
	}
	broken := cloneSession(session)
	broken["key_pass"] = base64.StdEncoding.EncodeToString([]byte("not the key passphrase"))
	if code := errorCode(t, call(t, map[string]any{"op": "message", "session": broken, "id": id})); code != "session_invalid" {
		t.Fatalf("wrong key_pass code = %q", code)
	}
}

func TestMarkReadReportsEachID(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := f.account("alice")
	one := alice.deliver(t, "one", "1", true)
	two := alice.deliver(t, "two", "2", true)
	session := loginAs(t, "alice")

	got := result(t, call(t, map[string]any{"op": "mark_read", "session": session, "ids": []string{one, two}, "is_read": true}))
	for _, row := range got["outcomes"].([]any) {
		if row.(map[string]any)["code"] != nil {
			t.Fatalf("outcomes = %v", got["outcomes"])
		}
	}
	if alice.metadata(t, one).Unread || alice.metadata(t, two).Unread {
		t.Fatal("messages are still unread")
	}

	// The live API answers a batch naming an unknown id with a 422; the
	// in-process server panics instead, so the proxy plays the live part.
	missing := "bWlzc2luZw=="
	f.setIntercept(func(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
		body, _ := io.ReadAll(r.Body)
		if r.Method != http.MethodPut || !strings.Contains(string(body), missing) {
			r.Body = io.NopCloser(bytes.NewReader(body))
			return false
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnprocessableEntity)
		_, _ = w.Write([]byte(`{"Code":2501,"Error":"Message does not exist"}`))
		return true
	})
	got = result(t, call(t, map[string]any{"op": "mark_read", "session": session, "ids": []string{one, missing}, "is_read": false}))
	outcomes := got["outcomes"].([]any)
	if outcomes[0].(map[string]any)["code"] != nil || outcomes[1].(map[string]any)["code"] != "request_failed" {
		t.Fatalf("partial outcomes = %v", outcomes)
	}
	if outcomes[1].(map[string]any)["id"] != missing {
		t.Fatalf("outcome ids out of order: %v", outcomes)
	}
	if !alice.metadata(t, one).Unread {
		t.Fatal("the valid id was not applied")
	}
	tooMany := make([]string, batchLimit+1)
	for i := range tooMany {
		tooMany[i] = one
	}
	if code := errorCode(t, call(t, map[string]any{"op": "mark_read", "session": session, "ids": tooMany, "is_read": true})); code != "invalid_batch" {
		t.Fatalf("oversized batch code = %q", code)
	}
}

func TestStarAddsAndRemovesTheStarredLabel(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := f.account("alice")
	id := alice.deliver(t, "one", "1", true)
	session := loginAs(t, "alice")

	got := result(t, call(t, map[string]any{"op": "star", "session": session, "ids": []string{id}, "is_starred": true}))
	if got["outcomes"].([]any)[0].(map[string]any)["code"] != nil {
		t.Fatalf("outcomes = %v", got["outcomes"])
	}
	if got["session"] == nil {
		t.Fatal("star reply carries no session")
	}
	labels := alice.metadata(t, id).LabelIDs
	if !slices.Contains(labels, proton.StarredLabel) || !slices.Contains(labels, proton.InboxLabel) {
		t.Fatalf("labels after star = %v", labels)
	}
	result(t, call(t, map[string]any{"op": "star", "session": session, "ids": []string{id}, "is_starred": false}))
	if labels := alice.metadata(t, id).LabelIDs; slices.Contains(labels, proton.StarredLabel) {
		t.Fatalf("labels after unstar = %v", labels)
	}
	if code := errorCode(t, call(t, map[string]any{"op": "star", "session": session, "ids": []string{id}})); code != "invalid_request" {
		t.Fatalf("missing is_starred code = %q", code)
	}
}

func TestMoveFilesIntoFoldersOnly(t *testing.T) {
	f := newFixture(t)
	userID := f.user("alice")
	alice := f.account("alice")
	id := alice.deliver(t, "one", "1", true)
	folder, err := f.srv.CreateLabel(userID, "Projects", "", proton.LabelTypeFolder)
	if err != nil {
		t.Fatal(err)
	}
	label, err := f.srv.CreateLabel(userID, "Important", "", proton.LabelTypeLabel)
	if err != nil {
		t.Fatal(err)
	}
	session := loginAs(t, "alice")

	got := result(t, call(t, map[string]any{"op": "move", "session": session, "ids": []string{id}, "destination": proton.ArchiveLabel}))
	if got["outcomes"].([]any)[0].(map[string]any)["code"] != nil {
		t.Fatalf("outcomes = %v", got["outcomes"])
	}
	labels := alice.metadata(t, id).LabelIDs
	if !slices.Contains(labels, proton.ArchiveLabel) || slices.Contains(labels, proton.InboxLabel) {
		t.Fatalf("labels after archive = %v", labels)
	}
	result(t, call(t, map[string]any{"op": "move", "session": session, "ids": []string{id}, "destination": folder}))
	if labels := alice.metadata(t, id).LabelIDs; !slices.Contains(labels, folder) || slices.Contains(labels, proton.ArchiveLabel) {
		t.Fatalf("labels after folder move = %v", labels)
	}
	for destination, want := range map[string]string{
		proton.AllMailLabel: "invalid_destination",
		proton.SentLabel:    "invalid_destination",
		label:               "invalid_destination",
		"bm9mb2xkZXI=":      "folder_not_found",
		"a/b":               "invalid_id",
	} {
		reply := call(t, map[string]any{"op": "move", "session": session, "ids": []string{id}, "destination": destination})
		if got := errorCode(t, reply); got != want {
			t.Errorf("move to %s code = %q, want %q", destination, got, want)
		}
	}
	listed := result(t, call(t, map[string]any{"op": "sync", "session": session, "folder": folder, "limit": 5}))
	if !slices.Equal(ids(listed["messages"].([]any)), []string{id}) {
		t.Fatalf("folder listing = %v", listed["messages"])
	}
	for _, row := range listed["folders"].([]any) {
		entry := row.(map[string]any)
		if entry["id"] == folder && (entry["kind"] != "folder" || entry["label"] != "Projects") {
			t.Errorf("folder row = %v", entry)
		}
		if entry["id"] == label && entry["kind"] != "label" {
			t.Errorf("label row = %v", entry)
		}
	}
}

func TestRevokeEndsTheSessionIdempotently(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	session := loginAs(t, "alice")
	if got := result(t, call(t, map[string]any{"op": "revoke", "session": session})); len(got) != 0 {
		t.Fatalf("revoke result = %v", got)
	}
	if code := errorCode(t, call(t, map[string]any{"op": "sync", "session": session, "limit": 1})); code != "session_revoked" {
		t.Fatalf("sync after revoke code = %q", code)
	}
	result(t, call(t, map[string]any{"op": "revoke", "session": session}))
}

func cloneSession(session map[string]any) map[string]any {
	clone := make(map[string]any, len(session))
	for key, value := range session {
		clone[key] = value
	}
	return clone
}

// mimeParts walks a MIME message into (content type, disposition, body).
func mimeParts(t *testing.T, raw string) [][3]string {
	t.Helper()
	entity, err := message.Read(strings.NewReader(raw))
	if err != nil {
		t.Fatalf("mime does not parse: %v", err)
	}
	var parts [][3]string
	err = entity.Walk(func(_ []int, part *message.Entity, err error) error {
		if err != nil {
			return err
		}
		kind, _, _ := part.Header.ContentType()
		if strings.HasPrefix(kind, "multipart/") {
			return nil
		}
		disposition, _, _ := part.Header.ContentDisposition()
		var body bytes.Buffer
		if _, err := body.ReadFrom(part.Body); err != nil {
			return err
		}
		parts = append(parts, [3]string{kind, disposition, body.String()})
		return nil
	})
	if err != nil {
		t.Fatalf("mime walk: %v", err)
	}
	return parts
}
