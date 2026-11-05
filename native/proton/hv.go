package main

import (
	"errors"

	proton "github.com/ProtonMail/go-proton-api"
)

// Upper bound on an opaque challenge or solved token: far above any token
// Proton issues, low enough that a broken reply cannot balloon ours.
const maxHVTokenBytes = 4096

// hvChallenge is the human-verification challenge a 9001 refusal carries:
// the token to open verify.proton.me with and the methods it may offer.
// Both are opaque values copied from the error's Details, never its text.
type hvChallenge struct {
	Token   string   `json:"token"`
	Methods []string `json:"methods"`
}

// loginRefusal is the reply for a failed login request. A human-verification
// demand carries its challenge so the caller can show it; everything else is
// the bare code.
func loginRefusal(err error) envelope {
	reply := failure(loginFailure(err))
	if reply.Error.Code == "human_verification_required" {
		reply.Error.HV = challengeOf(err)
	}
	return reply
}

// challengeOf extracts the challenge from a 9001 error, or nil when the
// details are missing or malformed — the code alone still says what
// happened, it just cannot be solved from this reply.
func challengeOf(err error) *hvChallenge {
	var apiErr *proton.APIError
	if !errors.As(err, &apiErr) {
		return nil
	}
	details, detailErr := apiErr.GetHVDetails()
	if detailErr != nil || details == nil || !headerSafeValue(details.Token) {
		return nil
	}
	methods := []string{}
	for _, method := range details.Methods {
		if validMethod(method) {
			methods = append(methods, method)
		}
	}
	return &hvChallenge{Token: details.Token, Methods: methods}
}

// headerSafeValue accepts a non-empty token that can travel as an HTTP
// header value and in a URL query untouched by escaping surprises: printable
// ASCII without spaces.
func headerSafeValue(value string) bool {
	if value == "" || len(value) > maxHVTokenBytes {
		return false
	}
	for i := range len(value) {
		if value[i] <= ' ' || value[i] > '~' {
			return false
		}
	}
	return true
}

// validMethod accepts a verification method name such as "captcha" or
// "ownership-email".
func validMethod(method string) bool {
	if method == "" || len(method) > 64 {
		return false
	}
	for i := range len(method) {
		c := method[i]
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9', c == '-', c == '_':
		default:
			return false
		}
	}
	return true
}
