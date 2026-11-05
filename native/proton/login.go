package main

import (
	"context"
	"encoding/base64"
	"errors"
	"net/http"

	"github.com/ProtonMail/gluon/async"
	proton "github.com/ProtonMail/go-proton-api"
)

type loginParams struct {
	envelopeFields
	Email    string  `json:"email"`
	Password string  `json:"password"`
	TOTP     *string `json:"totp"`
	// Only two-password accounts need it; one-password accounts unlock
	// their keys with the login password.
	MailboxPassword *string `json:"mailbox_password"`
	// A solved human-verification challenge: the token and type the
	// verify page reported. Both or neither.
	HVToken *string `json:"hv_token"`
	HVType  *string `json:"hv_type"`
}

// verification is the solved challenge login presents, or nil.
func (p loginParams) verification() (*proton.APIHVDetails, bool) {
	token, kind := "", ""
	if p.HVToken != nil {
		token = *p.HVToken
	}
	if p.HVType != nil {
		kind = *p.HVType
	}
	switch {
	case token == "" && kind == "":
		return nil, true
	case token == "" || kind == "" || !headerSafeValue(token) || !validMethod(kind):
		return nil, false
	}
	// go-proton-api sends Methods joined with "," as the token type, so a
	// single entry is sent verbatim.
	return &proton.APIHVDetails{Token: token, Methods: []string{kind}}, true
}

// login proves the SRP credentials, completes a TOTP challenge, then unlocks
// the mailbox keys once to prove the passphrase. The session it returns
// carries that passphrase already salted (key_pass), so later operations
// decrypt without the password ever being stored.
func login(ctx context.Context, raw []byte) envelope {
	var params loginParams
	if !decodeStrict(raw, &params) {
		return failure("invalid_request")
	}
	if params.Email == "" || params.Password == "" {
		return failure("invalid_request")
	}
	hv, ok := params.verification()
	if !ok {
		return failure("invalid_request")
	}
	manager := newManager()
	client, auth, err := manager.NewClientWithLoginWithHVToken(ctx, params.Email, []byte(params.Password), hv)
	if err != nil {
		manager.Close()
		return loginRefusal(err)
	}
	session := attach(manager, client, sessionPayload{
		Login:        params.Email,
		UserID:       auth.UserID,
		UID:          auth.UID,
		AccessToken:  auth.AccessToken,
		RefreshToken: auth.RefreshToken,
		PasswordMode: int(auth.PasswordMode),
	})
	defer session.close()
	totp := ""
	if params.TOTP != nil {
		totp = *params.TOTP
	}
	if code, ok := secondFactor(ctx, client, auth, totp); !ok {
		abandon(client)
		return failure(code)
	}
	keyPassword := params.Password
	if auth.PasswordMode == proton.TwoPasswordMode {
		if params.MailboxPassword == nil || *params.MailboxPassword == "" {
			abandon(client)
			return failure("mailbox_password_required")
		}
		keyPassword = *params.MailboxPassword
	}
	keyPass, refused := saltedKeyPass(ctx, client, []byte(keyPassword), hv)
	if refused != nil {
		abandon(client)
		return *refused
	}
	session.payload.KeyPass = base64.StdEncoding.EncodeToString(keyPass)
	return success(map[string]any{"session": session.current()})
}

// abandon best-effort closes a session login could not finish, so a failed
// attempt does not linger in the account's session list.
func abandon(client *proton.Client) {
	ctx, cancel := context.WithTimeout(context.Background(), abandonTimeout)
	defer cancel()
	_ = client.AuthDelete(ctx)
}

// saltedKeyPass derives the key passphrase the way the official clients do
// (bcrypt over the primary user key's salt) and proves it unlocks the user
// and at least one address key. The user fetch carries the solved challenge
// too: the API may ask for verification there as well as at /auth (Bridge
// sends it on both).
func saltedKeyPass(ctx context.Context, client *proton.Client, password []byte, hv *proton.APIHVDetails) ([]byte, *envelope) {
	refuse := func(code string) *envelope {
		reply := failure(code)
		return &reply
	}
	user, err := client.GetUserWithHV(ctx, hv)
	if err != nil {
		reply := loginRefusal(err)
		return nil, &reply
	}
	primary := ""
	for _, key := range user.Keys {
		if key.Primary {
			primary = key.ID
			break
		}
	}
	if primary == "" {
		return nil, refuse("mailbox_password_failed")
	}
	salts, err := client.GetSalts(ctx)
	if err != nil {
		return nil, refuse(loginFailure(err))
	}
	keyPass, err := salts.SaltForKey(password, primary)
	if err != nil || len(keyPass) == 0 {
		return nil, refuse("mailbox_password_failed")
	}
	addresses, err := client.GetAddresses(ctx)
	if err != nil {
		return nil, refuse(loginFailure(err))
	}
	if _, _, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{}); err != nil {
		return nil, refuse("mailbox_password_failed")
	}
	return keyPass, nil
}

// secondFactor completes a TOTP challenge. It reports ok unless the account
// needs a code it was not given, cannot use, or rejected.
func secondFactor(ctx context.Context, client *proton.Client, auth proton.Auth, totp string) (string, bool) {
	if auth.TwoFA.Enabled == 0 {
		return "", true
	}
	if auth.TwoFA.Enabled&proton.HasTOTP == 0 {
		return "unsupported_second_factor", false
	}
	if totp == "" {
		return "second_factor_required", false
	}
	if len(totp) != 6 || !allDigits(totp) {
		return "invalid_totp", false
	}
	if err := client.Auth2FA(ctx, proton.Auth2FAReq{TwoFactorCode: totp}); err != nil {
		if code := loginFailure(err); code == "timeout" || code == "transport_failed" {
			return code, false
		}
		return "second_factor_failed", false
	}
	return "", true
}

func allDigits(value string) bool {
	for _, c := range []byte(value) {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

// loginFailure keeps the fixed vocabulary: credential rejections read as
// login_failed, an expired context as timeout, and everything else through
// the shared classifier. During login there is no session to have been
// revoked, so a 401 is the credentials being refused too. Server text never
// leaves this function.
func loginFailure(err error) string {
	var apiErr *proton.APIError
	if errors.As(err, &apiErr) {
		switch {
		case apiErr.Code == proton.PasswordWrong, apiErr.Code == proton.UsernameInvalid:
			return "login_failed"
		case apiErr.Code == proton.HumanValidationInvalidToken:
			return "human_verification_failed"
		case apiErr.Status == http.StatusUnauthorized:
			return "login_failed"
		}
	}
	return apiFailure(err)
}
