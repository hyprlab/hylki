package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"sync"

	"github.com/ProtonMail/gluon/async"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

// The API allowlists client identities; "Other" is the third-party slot.
// Impersonating an official client would be dishonest and fragile.
const appVersion = "Other"

// apiURL and apiTransport are the only seams the offline tests replace: they
// point the helper at go-proton-api's in-process server.
var (
	apiURL                         = proton.DefaultHostURL
	apiTransport http.RoundTripper = http.DefaultTransport
)

// quietLogger swallows resty's own diagnostics, which quote request URLs.
type quietLogger struct{}

func (quietLogger) Errorf(string, ...any) {}
func (quietLogger) Warnf(string, ...any)  {}
func (quietLogger) Debugf(string, ...any) {}

func newManager() *proton.Manager {
	return proton.New(
		proton.WithHostURL(apiURL),
		proton.WithAppVersion(appVersion),
		proton.WithTransport(apiTransport),
		proton.WithLogger(quietLogger{}),
	)
}

// sessionPayload is the resumable session the caller stores in its keyring
// and hands back on every operation. Deliberately has no String method: it
// holds tokens and the salted key passphrase.
type sessionPayload struct {
	Login        string `json:"login"`
	UserID       string `json:"user_id"`
	UID          string `json:"uid"`
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
	PasswordMode int    `json:"password_mode"`
	// KeyPass is the mailbox passphrase already salted for the primary user
	// key (base64) — what go-proton-api's Unlock wants. Never the password.
	KeyPass string `json:"key_pass"`
}

func (s *sessionPayload) usable() bool {
	return s != nil && s.Login != "" && s.UserID != "" && s.UID != "" &&
		s.AccessToken != "" && s.RefreshToken != ""
}

// apiSession is one operation's authenticated client. Tokens the library
// refreshes mid-operation (401 → /auth/v4/refresh, which rotates the refresh
// token) are captured so the reply can hand the new session back.
type apiSession struct {
	manager  *proton.Manager
	client   *proton.Client
	mu       sync.Mutex
	payload  sessionPayload
	deauthed bool
}

func resume(payload sessionPayload) *apiSession {
	manager := newManager()
	return attach(manager, manager.NewClient(payload.UID, payload.AccessToken, payload.RefreshToken), payload)
}

// attach wraps an authenticated client so token rotation and de-auth are
// observed for the reply.
func attach(manager *proton.Manager, client *proton.Client, payload sessionPayload) *apiSession {
	session := &apiSession{
		manager: manager,
		client:  client,
		payload: payload,
	}
	session.client.AddAuthHandler(func(auth proton.Auth) {
		session.mu.Lock()
		defer session.mu.Unlock()
		if auth.UID != "" {
			session.payload.UID = auth.UID
		}
		session.payload.AccessToken = auth.AccessToken
		session.payload.RefreshToken = auth.RefreshToken
	})
	session.client.AddDeauthHandler(func() {
		session.mu.Lock()
		defer session.mu.Unlock()
		session.deauthed = true
	})
	return session
}

func (s *apiSession) close() {
	s.client.Close()
	s.manager.Close()
}

// current is the session as it stands now, rotated tokens included.
func (s *apiSession) current() *sessionPayload {
	s.mu.Lock()
	defer s.mu.Unlock()
	payload := s.payload
	return &payload
}

func (s *apiSession) wasDeauthed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.deauthed
}

// ok returns a success envelope carrying the current session.
func (s *apiSession) ok(result map[string]any) envelope {
	result["session"] = s.current()
	return success(result)
}

// fail returns an error envelope; the session rides along so a rotation
// that happened before the failure is not lost.
func (s *apiSession) fail(code string) envelope {
	return envelope{Version: protocolVersion, Error: &codedError{Code: code, Session: s.current()}}
}

// failErr classifies err into the fixed vocabulary, then fails with it.
func (s *apiSession) failErr(err error) envelope {
	return s.fail(s.classify(err))
}

func (s *apiSession) classify(err error) string {
	if s.wasDeauthed() {
		return "session_revoked"
	}
	return apiFailure(err)
}

// apiFailure maps any library error onto the fixed vocabulary. Server text
// never leaves this function.
func apiFailure(err error) string {
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return "timeout"
	}
	var apiErr *proton.APIError
	if errors.As(err, &apiErr) {
		switch {
		case apiErr.Code == proton.AppVersionBadCode || apiErr.Code == proton.AppVersionMissingCode:
			return "client_outdated"
		case apiErr.Code == proton.HumanVerificationRequired:
			return "human_verification_required"
		case apiErr.Code == proton.AuthRefreshTokenInvalid || apiErr.Status == http.StatusUnauthorized:
			return "session_revoked"
		case apiErr.Status == http.StatusTooManyRequests || apiErr.Status >= 500:
			return "transport_failed"
		}
		return "request_failed"
	}
	var netErr *proton.NetError
	var opErr *net.OpError
	var dnsErr *net.DNSError
	if errors.As(err, &netErr) || errors.As(err, &opErr) || errors.As(err, &dnsErr) {
		return "transport_failed"
	}
	return "request_failed"
}

// decodeStrict parses one request object into target, refusing unknown
// fields the way the Tuta helper's serde(deny_unknown_fields) does.
func decodeStrict(raw []byte, target any) bool {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return false
	}
	return !decoder.More()
}

// envelopeFields are the two keys every request carries beside its own.
type envelopeFields struct {
	Version int    `json:"version"`
	Op      string `json:"op"`
}

// validID accepts a Proton object id: system label ids are small integers,
// everything else is URL-safe base64. Ids are spliced into request paths,
// so anything else — a slash, a dot-dot — is refused before it is sent.
func validID(id string) bool {
	if id == "" || len(id) > 512 {
		return false
	}
	for i := range len(id) {
		c := id[i]
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		case c == '-' || c == '_' || c == '=':
		default:
			return false
		}
	}
	return true
}

// keyrings unlocks the user and address keys with the stored key passphrase.
// A passphrase that no longer opens them (password changed elsewhere) means
// the stored session is unusable: session_invalid, and the account must sign
// in again.
func (s *apiSession) keyrings(ctx context.Context) (unlocked, string) {
	keyPass, err := base64.StdEncoding.DecodeString(s.payload.KeyPass)
	if err != nil || len(keyPass) == 0 {
		return unlocked{}, "session_invalid"
	}
	user, err := s.client.GetUser(ctx)
	if err != nil {
		return unlocked{}, s.classify(err)
	}
	addresses, err := s.client.GetAddresses(ctx)
	if err != nil {
		return unlocked{}, s.classify(err)
	}
	userKR, addrKRs, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{})
	if err != nil {
		return unlocked{}, "session_invalid"
	}
	return unlocked{addresses: addresses, userKR: userKR, addrKRs: addrKRs}, ""
}

// unlocked is the account's addresses with their unlocked keyrings.
type unlocked struct {
	addresses []proton.Address
	userKR    *crypto.KeyRing
	// addrKRs maps an address id to its keyring.
	addrKRs map[string]*crypto.KeyRing
}

// sessionRequest is the shape shared by every operation that resumes one.
type sessionRequest struct {
	Session *sessionPayload `json:"session"`
}
