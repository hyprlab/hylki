package main

import (
	"context"
	"errors"
	"fmt"
	"testing"

	proton "github.com/ProtonMail/go-proton-api"
)

func TestLoginFailureKeepsTheFixedVocabulary(t *testing.T) {
	wrapped := func(err error) error { return fmt.Errorf("transport: %w", err) }
	cases := map[string]struct {
		err  error
		want string
	}{
		"wrong password":        {&proton.APIError{Code: proton.PasswordWrong}, "login_failed"},
		"unknown user":          {&proton.APIError{Code: proton.UsernameInvalid}, "login_failed"},
		"wrapped rejection":     {wrapped(&proton.APIError{Code: proton.PasswordWrong}), "login_failed"},
		"human verification":    {&proton.APIError{Code: proton.HumanVerificationRequired, Status: 422}, "human_verification_required"},
		"outdated client":       {&proton.APIError{Code: proton.AppVersionBadCode, Status: 400}, "client_outdated"},
		"unexpected API error":  {&proton.APIError{Code: proton.InvalidValue, Status: 422}, "request_failed"},
		"expired context":       {context.DeadlineExceeded, "timeout"},
		"cancelled context":     {context.Canceled, "timeout"},
		"wrapped timeout":       {wrapped(context.DeadlineExceeded), "timeout"},
		"network fault":         {&proton.NetError{Cause: errors.New("dial tcp: connection refused")}, "transport_failed"},
		"opaque failure":        {errors.New("something else"), "request_failed"},
		"server text never out": {&proton.APIError{Code: proton.PasswordWrong, Message: "secret"}, "login_failed"},
	}
	for name, tc := range cases {
		if got := loginFailure(tc.err); got != tc.want {
			t.Errorf("%s: code = %q, want %q", name, got, tc.want)
		}
	}
}

func TestSecondFactorStates(t *testing.T) {
	twoFA := func(status proton.TwoFAStatus) proton.Auth {
		return proton.Auth{TwoFA: proton.TwoFAInfo{Enabled: status}}
	}
	cases := map[string]struct {
		auth proton.Auth
		totp string
		code string
		ok   bool
	}{
		"no second factor": {twoFA(0), "", "", true},
		"fido2 only":       {twoFA(proton.HasFIDO2), "123456", "unsupported_second_factor", false},
		"totp missing":     {twoFA(proton.HasTOTP), "", "second_factor_required", false},
		"totp short":       {twoFA(proton.HasTOTP), "12345", "invalid_totp", false},
		"totp not digits":  {twoFA(proton.HasTOTP), "12345a", "invalid_totp", false},
	}
	for name, tc := range cases {
		// A nil client is safe here: these paths return before any request.
		if code, ok := secondFactor(context.Background(), nil, tc.auth, tc.totp); code != tc.code || ok != tc.ok {
			t.Errorf("%s: = (%q, %v), want (%q, %v)", name, code, ok, tc.code, tc.ok)
		}
	}
}

func TestLoginRejectsShapelessRequestsWithoutNetwork(t *testing.T) {
	for _, raw := range []string{
		`{"version":1,"op":"login"}`,
		`{"version":1,"op":"login","email":"a@b"}`,
		`{"version":1,"op":"login","password":"x"}`,
		`{"version":1,"op":"login","email":"","password":""}`,
		`{"version":1,"op":"login","email":"a@b","password":"x","otp":"1"}`,
		`[]`,
	} {
		if got := login(context.Background(), []byte(raw)); got.Error == nil || got.Error.Code != "invalid_request" {
			t.Errorf("%s: = %+v, want invalid_request", raw, got)
		}
	}
}
