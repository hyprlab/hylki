package main

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
)

func codeOf(t *testing.T, raw string) string {
	t.Helper()
	var out bytes.Buffer
	run(strings.NewReader(raw), &out)
	var decoded struct {
		Version int `json:"version"`
		Error   struct {
			Code string `json:"code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(out.Bytes(), &decoded); err != nil {
		t.Fatalf("helper did not emit an envelope: %v", err)
	}
	if decoded.Version != protocolVersion {
		t.Fatalf("envelope version = %d, want %d", decoded.Version, protocolVersion)
	}
	return decoded.Error.Code
}

func TestMalformedRequestsAreRejected(t *testing.T) {
	cases := map[string]string{
		"not json":        `{"version":`,
		"wrong shape":     `[]`,
		"missing version": `{"op":"sync"}`,
		"float version":   `{"version":1.5,"op":"sync"}`,
		"missing op":      `{"version":1}`,
		"unknown op":      `{"version":1,"op":"star"}`,
		"unknown field":   `{"version":1,"op":"revoke","session":{},"extra":1}`,
		"sync sans limit": `{"version":1,"op":"sync","session":{}}`,
		"read sans flag":  `{"version":1,"op":"mark_read","session":{},"ids":[]}`,
		"move sans ids":   `{"version":1,"op":"move","session":{},"destination":"0"}`,
		"login sans pass": `{"version":1,"op":"login","email":"a@b"}`,
	}
	for name, raw := range cases {
		if got := codeOf(t, raw); got != "invalid_request" {
			t.Errorf("%s code = %q, want invalid_request", name, got)
		}
	}
}

func TestRequestsAreValidatedBeforeAnyNetwork(t *testing.T) {
	// apiURL is never reached: every case fails on its shape first.
	session := `{"login":"a","user_id":"u","uid":"x","access_token":"t","refresh_token":"r","key_pass":""}`
	cases := map[string]struct{ raw, want string }{
		"no session":          {`{"version":1,"op":"sync","limit":1}`, "session_invalid"},
		"empty session":       {`{"version":1,"op":"message","session":{},"id":"a"}`, "session_invalid"},
		"zero limit":          {`{"version":1,"op":"sync","session":` + session + `,"limit":0}`, "invalid_page"},
		"huge limit":          {`{"version":1,"op":"sync","session":` + session + `,"limit":1001}`, "invalid_page"},
		"negative offset":     {`{"version":1,"op":"sync","session":` + session + `,"limit":1,"offset":-1}`, "invalid_page"},
		"cursor with offset":  {`{"version":1,"op":"sync","session":` + session + `,"limit":1,"offset":3,"cursor":"abc"}`, "invalid_page"},
		"path-shaped folder":  {`{"version":1,"op":"sync","session":` + session + `,"limit":1,"folder":"../x"}`, "invalid_id"},
		"path-shaped cursor":  {`{"version":1,"op":"sync","session":` + session + `,"limit":1,"cursor":"a/b"}`, "invalid_id"},
		"path-shaped id":      {`{"version":1,"op":"message","session":` + session + `,"id":"a?b"}`, "invalid_id"},
		"oversized batch":     {`{"version":1,"op":"mark_read","session":` + session + `,"is_read":true,"ids":[` + strings.Repeat(`"a",`, batchLimit) + `"a"]}`, "invalid_batch"},
		"bad id in batch":     {`{"version":1,"op":"move","session":` + session + `,"ids":["a","b c"],"destination":"0"}`, "invalid_id"},
		"bad destination":     {`{"version":1,"op":"move","session":` + session + `,"ids":["a"],"destination":""}`, "invalid_id"},
		"revoke sans session": {`{"version":1,"op":"revoke"}`, "session_invalid"},
	}
	for name, tc := range cases {
		if got := codeOf(t, tc.raw); got != tc.want {
			t.Errorf("%s code = %q, want %q", name, got, tc.want)
		}
	}
}

func TestIncompatibleVersionNeverConnects(t *testing.T) {
	if got := codeOf(t, `{"version":2,"op":"login"}`); got != "unsupported_version" {
		t.Fatalf("code = %q, want unsupported_version", got)
	}
}

func TestOversizedInputIsRejected(t *testing.T) {
	if got := codeOf(t, `{"version":1,"op":"sync","pad":"`+strings.Repeat("x", maxRequestBytes)+`"}`); got != "invalid_request" {
		t.Fatalf("code = %q, want invalid_request", got)
	}
}

func TestReplyIsOneLine(t *testing.T) {
	var out bytes.Buffer
	run(strings.NewReader("  \n"+`{"version":1,"op":"sync"}`+"\n"), &out)
	if lines := strings.Count(out.String(), "\n"); lines != 1 {
		t.Fatalf("reply has %d lines, want exactly one", lines)
	}
}
