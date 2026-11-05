package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ProtonMail/gluon/async"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/go-proton-api/server"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
	"github.com/bradenaw/juniper/stream"
	"github.com/sirupsen/logrus"
)

func TestMain(m *testing.M) {
	logrus.SetOutput(io.Discard)
	os.Exit(m.Run())
}

const fixturePassword = "correct horse battery staple"

// fixture is go-proton-api's in-process API server behind a reverse proxy
// the tests control: the proxy serves keys for external PGP recipients,
// records send requests, and injects the failures a live API can produce.
type fixture struct {
	t      *testing.T
	srv    *server.Server
	direct *proton.Manager

	mu sync.Mutex
	// externalKeys answers GET /core/v4/keys for addresses the test server
	// treats as external, as WKD would.
	externalKeys map[string]string
	// sends holds the bodies of submitted drafts, in order.
	sends [][]byte
	// intercept, when set, handles a request instead of the server. It
	// reports whether it did.
	intercept func(w http.ResponseWriter, r *http.Request, forward func(http.ResponseWriter)) bool
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	srv := server.New(server.WithTLS(false))
	t.Cleanup(srv.Close)
	f := &fixture{
		t:            t,
		srv:          srv,
		externalKeys: map[string]string{},
		direct:       proton.New(proton.WithHostURL(srv.GetHostURL()), proton.WithLogger(quietLogger{})),
	}
	t.Cleanup(f.direct.Close)
	target, err := url.Parse(srv.GetHostURL())
	if err != nil {
		t.Fatal(err)
	}
	backend := httputil.NewSingleHostReverseProxy(target)
	proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		f.serve(w, r, backend)
	}))
	t.Cleanup(proxy.Close)
	previousURL, previousTransport := apiURL, apiTransport
	apiURL, apiTransport = proxy.URL, http.DefaultTransport
	t.Cleanup(func() { apiURL, apiTransport = previousURL, previousTransport })
	return f
}

func (f *fixture) serve(w http.ResponseWriter, r *http.Request, backend *httputil.ReverseProxy) {
	body, _ := io.ReadAll(r.Body)
	r.Body = io.NopCloser(bytes.NewReader(body))
	forward := func(target http.ResponseWriter) {
		r.Body = io.NopCloser(bytes.NewReader(body))
		backend.ServeHTTP(target, r)
	}
	f.mu.Lock()
	intercept := f.intercept
	key, isExternalKey := f.externalKeys[strings.ToLower(r.URL.Query().Get("Email"))]
	isSend := isSubmit(r)
	if isSend {
		f.sends = append(f.sends, body)
	}
	f.mu.Unlock()
	if intercept != nil && intercept(w, r, forward) {
		return
	}
	if r.Method == http.MethodGet && r.URL.Path == "/core/v4/keys" && isExternalKey {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"Code":          1000,
			"Keys":          []map[string]any{{"Flags": 3, "PublicKey": key}},
			"RecipientType": proton.RecipientTypeExternal,
		})
		return
	}
	forward(w)
}

func (f *fixture) setIntercept(fn func(w http.ResponseWriter, r *http.Request, forward func(http.ResponseWriter)) bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.intercept = fn
}

// discardWriter is a response nobody reads: the request reached the server,
// its answer is lost.
type discardWriter struct{ header http.Header }

func (d *discardWriter) Header() http.Header         { return d.header }
func (d *discardWriter) Write(p []byte) (int, error) { return len(p), nil }
func (d *discardWriter) WriteHeader(int)             {}

func (f *fixture) lastSend() proton.SendDraftReq {
	f.t.Helper()
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.sends) == 0 {
		f.t.Fatal("no send request was made")
	}
	var request proton.SendDraftReq
	if err := json.Unmarshal(f.sends[len(f.sends)-1], &request); err != nil {
		f.t.Fatalf("send request is not JSON: %v", err)
	}
	return request
}

func (f *fixture) sendCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.sends)
}

// user creates an account on the server: username@proton.local.
func (f *fixture) user(name string) string {
	f.t.Helper()
	userID, _, err := f.srv.CreateUser(name, []byte(fixturePassword))
	if err != nil {
		f.t.Fatal(err)
	}
	return userID
}

// account is a direct (helper-free) client for arranging and checking
// server state.
type account struct {
	client  *proton.Client
	address proton.Address
	addrKR  *crypto.KeyRing
}

func (f *fixture) account(name string) account {
	f.t.Helper()
	ctx := context.Background()
	client, _, err := f.direct.NewClientWithLogin(ctx, name, []byte(fixturePassword))
	if err != nil {
		f.t.Fatal(err)
	}
	f.t.Cleanup(client.Close)
	user, err := client.GetUser(ctx)
	if err != nil {
		f.t.Fatal(err)
	}
	addresses, err := client.GetAddresses(ctx)
	if err != nil {
		f.t.Fatal(err)
	}
	salts, err := client.GetSalts(ctx)
	if err != nil {
		f.t.Fatal(err)
	}
	keyPass, err := salts.SaltForKey([]byte(fixturePassword), user.Keys.Primary().ID)
	if err != nil {
		f.t.Fatal(err)
	}
	_, addrKRs, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{})
	if err != nil {
		f.t.Fatal(err)
	}
	return account{client: client, address: addresses[0], addrKR: addrKRs[addresses[0].ID]}
}

// deliver imports a message into the account's inbox.
func (a account) deliver(t *testing.T, subject, body string, isUnread bool) string {
	t.Helper()
	literal := "From: Sender <sender@example.com>\r\nTo: " + a.address.Email +
		"\r\nSubject: " + subject + "\r\nMessage-ID: <" + strings.ReplaceAll(subject, " ", "-") + "@example.com>" +
		"\r\nDate: Mon, 02 Nov 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n" + body + "\r\n"
	ctx := context.Background()
	results, err := a.client.ImportMessages(ctx, a.addrKR, 1, 1, proton.ImportReq{
		Metadata: proton.ImportMetadata{
			AddressID: a.address.ID,
			LabelIDs:  []string{proton.InboxLabel},
			Unread:    proton.Bool(isUnread),
			Flags:     proton.MessageFlagReceived,
		},
		Message: []byte(literal),
	})
	if err != nil {
		t.Fatal(err)
	}
	imported, err := stream.Collect(ctx, results)
	if err != nil || len(imported) != 1 || imported[0].Code != proton.SuccessCode {
		t.Fatalf("import failed: %v %+v", err, imported)
	}
	return imported[0].MessageID
}

func (a account) metadata(t *testing.T, id string) proton.MessageMetadata {
	t.Helper()
	metas, err := a.client.GetMessageMetadataPage(context.Background(), 0, pageSize, proton.MessageFilter{ID: []string{id}})
	if err != nil || len(metas) != 1 {
		t.Fatalf("message %s not found: %v", id, err)
	}
	return metas[0]
}

// call runs one helper request through the real framing and decodes the
// single reply line.
func call(t *testing.T, request map[string]any) map[string]any {
	t.Helper()
	return callWithin(t, operationTimeout, request)
}

func callWithin(t *testing.T, budget time.Duration, request map[string]any) map[string]any {
	t.Helper()
	request["version"] = protocolVersion
	raw, err := json.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), budget)
	defer cancel()
	var out bytes.Buffer
	write(&out, dispatch(ctx, raw))
	if strings.Count(out.String(), "\n") != 1 {
		t.Fatalf("reply is not exactly one line: %q", out.String())
	}
	var reply map[string]any
	if err := json.Unmarshal(out.Bytes(), &reply); err != nil {
		t.Fatalf("reply is not JSON: %v", err)
	}
	if reply["version"] != float64(protocolVersion) {
		t.Fatalf("reply version = %v", reply["version"])
	}
	return reply
}

// result is the reply's result object; the test fails on an error reply.
func result(t *testing.T, reply map[string]any) map[string]any {
	t.Helper()
	if detail, ok := reply["error"]; ok {
		t.Fatalf("unexpected error reply: %v", detail)
	}
	return reply["result"].(map[string]any)
}

// errorCode is the reply's error code; the test fails on a success reply.
func errorCode(t *testing.T, reply map[string]any) string {
	t.Helper()
	detail, ok := reply["error"].(map[string]any)
	if !ok {
		t.Fatalf("expected an error reply, got %v", reply)
	}
	return detail["code"].(string)
}

// loginAs signs a fixture user in through the helper and returns the
// session object later requests carry.
func loginAs(t *testing.T, name string) map[string]any {
	t.Helper()
	reply := result(t, call(t, map[string]any{"op": "login", "email": name, "password": fixturePassword, "totp": nil}))
	return reply["session"].(map[string]any)
}

func ids(rows []any) []string {
	out := make([]string, len(rows))
	for i, row := range rows {
		out[i] = row.(map[string]any)["id"].(string)
	}
	return out
}

func decodeMIME(t *testing.T, value any) string {
	t.Helper()
	raw, err := base64.StdEncoding.DecodeString(value.(string))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}
