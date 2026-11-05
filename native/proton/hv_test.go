package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"

	proton "github.com/ProtonMail/go-proton-api"
)

const (
	hvChallengeToken = "challenge-AbC_123"
	hvSolvedToken    = "challenge-AbC_123:solved-XyZ"
	hvServerText     = "Human verification required, server says so"
)

// verifier plays the live API's human-verification gate in front of the
// in-process server (which has none): /auth/v4 without the solved token is
// refused with 9001 and a challenge, a wrong token with 12087.
type verifier struct {
	mu      sync.Mutex
	headers []http.Header
	// guarded lists the paths the gate covers.
	guarded map[string]bool
}

func (v *verifier) intercept(w http.ResponseWriter, r *http.Request, _ func(http.ResponseWriter)) bool {
	if !v.guarded[r.URL.Path] {
		return false
	}
	v.mu.Lock()
	v.headers = append(v.headers, r.Header.Clone())
	v.mu.Unlock()
	token := r.Header.Get("x-pm-human-verification-token")
	kind := r.Header.Get("x-pm-human-verification-token-type")
	refuse := func(body string) bool {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnprocessableEntity)
		_, _ = w.Write([]byte(body))
		return true
	}
	switch {
	case token == "":
		return refuse(fmt.Sprintf(`{"Code":9001,"Error":%q,"Details":{"HumanVerificationToken":%q,`+
			`"HumanVerificationMethods":["captcha","email","bad method!"],"WebUrl":"https://verify.proton.me/?methods=captcha&token=%s"}}`,
			hvServerText, hvChallengeToken, hvChallengeToken))
	case token != hvSolvedToken || kind != "captcha":
		return refuse(`{"Code":12087,"Error":"CAPTCHA validation failed"}`)
	}
	return false
}

func (v *verifier) seen() []http.Header {
	v.mu.Lock()
	defer v.mu.Unlock()
	return append([]http.Header(nil), v.headers...)
}

func rawLogin(t *testing.T, request map[string]any) ([]byte, map[string]any) {
	t.Helper()
	reply := call(t, request)
	raw, err := json.Marshal(reply)
	if err != nil {
		t.Fatal(err)
	}
	return raw, reply
}

func TestLoginSurfacesAndAcceptsHumanVerification(t *testing.T) {
	for _, guarded := range []string{"/auth/v4", "/core/v4/users"} {
		t.Run(guarded, func(t *testing.T) {
			f := newFixture(t)
			f.user("alice")
			gate := &verifier{guarded: map[string]bool{guarded: true}}
			f.setIntercept(gate.intercept)
			request := map[string]any{"op": "login", "email": "alice", "password": fixturePassword, "totp": nil}

			raw, reply := rawLogin(t, request)
			if got := errorCode(t, reply); got != "human_verification_required" {
				t.Fatalf("code = %q", got)
			}
			hv, ok := reply["error"].(map[string]any)["hv"].(map[string]any)
			if !ok {
				t.Fatalf("no challenge in %s", raw)
			}
			if hv["token"] != hvChallengeToken {
				t.Fatalf("hv.token = %v", hv["token"])
			}
			if methods := fmt.Sprint(hv["methods"]); methods != "[captcha email]" {
				t.Fatalf("hv.methods = %v (malformed names must be dropped)", methods)
			}
			if bytes.Contains(raw, []byte(hvServerText)) || bytes.Contains(raw, []byte("WebUrl")) {
				t.Fatalf("server text leaked: %s", raw)
			}

			request["hv_token"], request["hv_type"] = "wrong", "captcha"
			if got := errorCode(t, call(t, request)); got != "human_verification_failed" {
				t.Fatalf("wrong token: code = %q", got)
			}

			request["hv_token"] = hvSolvedToken
			session := result(t, call(t, request))["session"].(map[string]any)
			if session["key_pass"] == "" {
				t.Fatalf("session = %v", session)
			}
			last := gate.seen()[len(gate.seen())-1]
			if last.Get("x-pm-human-verification-token") != hvSolvedToken || last.Get("x-pm-human-verification-token-type") != "captcha" {
				t.Fatalf("solved retry headers = %v", last)
			}
		})
	}
}

func TestHumanVerificationRequestShape(t *testing.T) {
	login := func(extra string) string {
		raw := `{"version":1,"op":"login","email":"a@b","password":"x"` + extra + `}`
		reply := dispatchRaw(t, raw)
		if reply.Error == nil {
			t.Fatalf("%s: expected an error", extra)
		}
		return reply.Error.Code
	}
	for _, extra := range []string{
		`,"hv_token":"t"`,
		`,"hv_type":"captcha"`,
		`,"hv_token":"","hv_type":"captcha"`,
		`,"hv_token":"a b","hv_type":"captcha"`,
		`,"hv_token":"t","hv_type":"cap tcha"`,
		`,"hv_token":"` + strings.Repeat("t", maxHVTokenBytes+1) + `","hv_type":"captcha"`,
	} {
		if got := login(extra); got != "invalid_request" {
			t.Errorf("%s: code = %q, want invalid_request", extra, got)
		}
	}
}

func TestChallengeExtraction(t *testing.T) {
	hvError := func(details string) error {
		return &proton.APIError{Status: 422, Code: proton.HumanVerificationRequired, Message: hvServerText, Details: proton.ErrDetails(details)}
	}
	cases := map[string]struct {
		err  error
		want *hvChallenge
	}{
		"challenge":       {hvError(`{"HumanVerificationToken":"t1","HumanVerificationMethods":["captcha"]}`), &hvChallenge{Token: "t1", Methods: []string{"captcha"}}},
		"no methods":      {hvError(`{"HumanVerificationToken":"t1"}`), &hvChallenge{Token: "t1", Methods: []string{}}},
		"no details":      {&proton.APIError{Status: 422, Code: proton.HumanVerificationRequired}, nil},
		"garbled details": {hvError(`not json`), nil},
		"unsafe token":    {hvError(`{"HumanVerificationToken":"t\r\nX: y","HumanVerificationMethods":["captcha"]}`), nil},
		"not an HV error": {&proton.APIError{Status: 422, Code: proton.PasswordWrong}, nil},
	}
	for name, tc := range cases {
		got := challengeOf(tc.err)
		if fmt.Sprint(got) != fmt.Sprint(tc.want) {
			t.Errorf("%s: challenge = %+v, want %+v", name, got, tc.want)
		}
	}
	if reply := loginRefusal(hvError(`{"HumanVerificationToken":"t1"}`)); reply.Error.HV == nil || reply.Error.Code != "human_verification_required" {
		t.Errorf("refusal = %+v", reply.Error)
	}
	if reply := loginRefusal(&proton.APIError{Status: 422, Code: proton.PasswordWrong}); reply.Error.HV != nil {
		t.Errorf("a password refusal carries a challenge: %+v", reply.Error)
	}
}

func dispatchRaw(t *testing.T, raw string) envelope {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), operationTimeout)
	defer cancel()
	return dispatch(ctx, []byte(raw))
}
