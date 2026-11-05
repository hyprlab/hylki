package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"sync"
	"testing"

	proton "github.com/ProtonMail/go-proton-api"
	"github.com/emersion/go-message"
)

// draftRecorder keeps the body of every draft-create request.
type draftRecorder struct {
	mu     sync.Mutex
	bodies [][]byte
}

func (d *draftRecorder) intercept(_ http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
	if r.Method == http.MethodPost && r.URL.Path == draftsPath && r.Header.Get("X-HTTP-Method-Override") == "" {
		body, _ := io.ReadAll(r.Body)
		r.Body = io.NopCloser(bytes.NewReader(body))
		d.mu.Lock()
		d.bodies = append(d.bodies, body)
		d.mu.Unlock()
	}
	return false
}

// last is the newest draft request's ParentID and Action.
func (d *draftRecorder) last(t *testing.T) (string, proton.CreateDraftAction) {
	t.Helper()
	d.mu.Lock()
	defer d.mu.Unlock()
	if len(d.bodies) == 0 {
		t.Fatal("no draft was created")
	}
	var draft struct {
		ParentID string
		Action   proton.CreateDraftAction
	}
	if err := json.Unmarshal(d.bodies[len(d.bodies)-1], &draft); err != nil {
		t.Fatal(err)
	}
	return draft.ParentID, draft.Action
}

// threadFixture: bob has mailed alice the parent, Message-ID
// <parent-1@proton.local>; it is the only message in alice's inbox.
func threadFixture(t *testing.T) (*fixture, *draftRecorder, map[string]any, string) {
	t.Helper()
	f := newFixture(t)
	f.user("alice")
	f.user("bob")
	bob := loginAs(t, "bob")
	result(t, call(t, sendRequest(bob, to("alice@proton.local"), map[string]any{
		"sender": "bob@proton.local", "sender_name": "Bob", "message_id": "<parent-1@proton.local>",
	})))
	alice := loginAs(t, "alice")
	inbox := inboxOf(t, alice)
	if len(inbox) != 1 || inbox[0].(map[string]any)["message_id"] != "parent-1@proton.local" {
		t.Fatalf("alice's inbox = %v", inbox)
	}
	recorder := &draftRecorder{}
	f.setIntercept(recorder.intercept)
	return f, recorder, alice, inbox[0].(map[string]any)["id"].(string)
}

func TestReplyThreadsThroughTheParent(t *testing.T) {
	f, drafts, alice, parent := threadFixture(t)
	result(t, call(t, sendRequest(alice, to("bob@proton.local"), map[string]any{
		"subject":     "Re: Greetings",
		"in_reply_to": "<parent-1@proton.local>",
		"references":  "<root@example.com> <parent-1@proton.local>",
	})))
	if parentID, action := drafts.last(t); parentID != parent || action != proton.ReplyAction {
		t.Fatalf("draft parent/action = %q/%d, want %q/reply", parentID, action, parent)
	}
	if meta := f.account("alice").metadata(t, parent); !meta.Flags.Has(proton.MessageFlagReplied) {
		t.Fatalf("parent flags = %b, want replied", meta.Flags)
	}

	// The server threads the reply: bob's copy names the parent.
	bob := loginAs(t, "bob")
	var reply string
	for _, row := range inboxOf(t, bob) {
		if row.(map[string]any)["subject"] == "Re: Greetings" {
			reply = row.(map[string]any)["id"].(string)
		}
	}
	got := result(t, call(t, map[string]any{"op": "message", "session": bob, "id": reply}))
	entity, err := message.Read(strings.NewReader(decodeMIME(t, got["mime"])))
	if err != nil {
		t.Fatal(err)
	}
	if header := entity.Header.Get("In-Reply-To"); header != "<parent-1@proton.local>" {
		t.Fatalf("In-Reply-To = %q", header)
	}
}

func TestForwardAndReferencesOnlyFindTheParent(t *testing.T) {
	f, drafts, alice, parent := threadFixture(t)
	result(t, call(t, sendRequest(alice, to("carol@example.com"), map[string]any{
		"subject":    "Fwd: Greetings",
		"references": "<unknown@example.com><parent-1@proton.local>",
		"action":     "forward",
	})))
	if parentID, action := drafts.last(t); parentID != parent || action != proton.ForwardAction {
		t.Fatalf("draft parent/action = %q/%d, want %q/forward", parentID, action, parent)
	}
	if meta := f.account("alice").metadata(t, parent); !meta.Flags.Has(proton.MessageFlagForwarded) {
		t.Fatalf("parent flags = %b, want forwarded", meta.Flags)
	}
}

func TestAReplyToAnUnknownMessageStillSends(t *testing.T) {
	_, drafts, alice, _ := threadFixture(t)
	got := result(t, call(t, sendRequest(alice, to("bob@proton.local"), map[string]any{
		"in_reply_to": "nobody-knows-this@example.com",
		"action":      "reply_all",
	})))
	if got["sent_mail"] == "" {
		t.Fatalf("result = %v", got)
	}
	if parentID, _ := drafts.last(t); parentID != "" {
		t.Fatalf("an unknown parent was linked: %q", parentID)
	}
}

func TestThreadingInputsAreValidated(t *testing.T) {
	f := newFixture(t)
	f.user("alice")
	alice := loginAs(t, "alice")
	long := "<" + strings.Repeat("a", maxMessageIDBytes) + "@x>"
	many := strings.Repeat("<a@b> ", maxReferences+1)
	cases := map[string]map[string]any{
		"empty in_reply_to":       {"in_reply_to": ""},
		"brackets only":           {"in_reply_to": "<>"},
		"space inside":            {"in_reply_to": "<a b@c>"},
		"header injection":        {"in_reply_to": "<a@b>\r\nBcc: x@y"},
		"non-ASCII":               {"in_reply_to": "<é@b>"},
		"oversized id":            {"in_reply_to": long},
		"bad reference":           {"references": "<a@b> <c d>"},
		"too many references":     {"references": many},
		"oversized references":    {"references": strings.Repeat("<a@b>", maxReferencesBytes/5+1)},
		"unknown action":          {"in_reply_to": "<a@b>", "action": "redirect"},
		"action without a parent": {"action": "forward"},
	}
	for name, overrides := range cases {
		if got := errorCode(t, call(t, sendRequest(alice, to("bob@example.com"), overrides))); got != "invalid_request" {
			t.Errorf("%s: code = %q, want invalid_request", name, got)
		}
	}
	if f.sendCount() != 0 {
		t.Fatal("an invalid threading input reached the submit endpoint")
	}
}

func TestThreadingCandidates(t *testing.T) {
	inReplyTo, references := "<c@x>", "<a@x> <b@x><c@x>   <d@x> <e@x>"
	thread, ok := threadingOf(&inReplyTo, &references, nil)
	if !ok || thread.action != proton.ReplyAction {
		t.Fatalf("threading = %+v, %v", thread, ok)
	}
	// In-Reply-To first, then references newest first, deduplicated and
	// capped.
	if got := strings.Join(thread.candidates, " "); got != "c@x e@x d@x b@x" {
		t.Fatalf("candidates = %q", got)
	}
	if thread, ok := threadingOf(nil, nil, nil); !ok || len(thread.candidates) != 0 {
		t.Fatalf("no threading = %+v, %v", thread, ok)
	}
}
