// Command hylki-proton is the native Proton Mail helper.
//
// One UTF-8 JSON object on stdin, one versioned envelope on stdout, exit 0
// always: {"version":1,"result":{...}} or {"version":1,"error":{"code":"..."}}.
// Framing, versions and error codes mirror the Tuta helper so both backends
// stay symmetric. Nothing is logged anywhere — stdin carries passwords and
// session material, and the libraries' own loggers are silenced in main.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/sirupsen/logrus"
)

const protocolVersion = 1

// Attachments ride the same stdin pipe as the request (base64), so the cap
// covers a large mail, not just a JSON envelope — the Tuta helper's figure.
const maxRequestBytes = 32 * 1024 * 1024

// The parent allows 100 seconds including process startup.
const operationTimeout = 90 * time.Second

// Best-effort cleanup (closing a half-made login, deleting an unsent draft)
// gets its own short budget: it must run even when the operation's own
// deadline is what failed it.
const abandonTimeout = 10 * time.Second

type request struct {
	Version *int   `json:"version"`
	Op      string `json:"op"`
}

// envelope is the single reply line. A session-bearing operation that
// failed after the server rotated its tokens still hands the new session
// back inside the error, so the caller never keeps a dead refresh token.
type envelope struct {
	Version int `json:"version"`
	// any, not map: omitempty drops an empty map, but revoke's result is
	// exactly {} and must still be present.
	Result any         `json:"result,omitempty"`
	Error  *codedError `json:"error,omitempty"`
}

type codedError struct {
	Code    string          `json:"code"`
	Session *sessionPayload `json:"session,omitempty"`
	// HV is the challenge of a human_verification_required login refusal.
	HV *hvChallenge `json:"hv,omitempty"`
}

func (e *codedError) Error() string { return e.Code }

type handler func(ctx context.Context, raw []byte) envelope

var operations = map[string]handler{
	"login":     login,
	"sync":      syncOp,
	"message":   messageOp,
	"mark_read": markReadOp,
	"move":      moveOp,
	"star":      starOp,
	"send":      sendOp,
	"revoke":    revokeOp,
}

func dispatch(ctx context.Context, raw []byte) (reply envelope) {
	var decoded request
	if err := json.Unmarshal(raw, &decoded); err != nil {
		return failure("invalid_request")
	}
	if decoded.Version == nil {
		return failure("invalid_request")
	}
	if *decoded.Version != protocolVersion {
		return failure("unsupported_version")
	}
	run, ok := operations[decoded.Op]
	if !ok {
		return failure("invalid_request")
	}
	// A library panic must still produce exactly one envelope, and its
	// value — which could quote request data — is never printed.
	defer func() {
		if recover() != nil {
			reply = failure("helper_failed")
		}
	}()
	return run(ctx, raw)
}

func failure(code string) envelope {
	return envelope{Version: protocolVersion, Error: &codedError{Code: code}}
}

func success(result map[string]any) envelope {
	return envelope{Version: protocolVersion, Result: result}
}

func run(stdin io.Reader, stdout io.Writer) {
	input, err := io.ReadAll(io.LimitReader(stdin, maxRequestBytes+1))
	if err != nil || len(input) > maxRequestBytes {
		write(stdout, failure("invalid_request"))
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), operationTimeout)
	defer cancel()
	write(stdout, dispatch(ctx, bytes.TrimSpace(input)))
}

func write(out io.Writer, response envelope) {
	encoded, err := json.Marshal(response)
	if err != nil {
		encoded = []byte(`{"version":1,"error":{"code":"helper_failed"}}`)
	}
	fmt.Fprintln(out, string(encoded))
}

func main() {
	// go-proton-api logs key ids and request URLs through logrus; nothing
	// it says is wanted, and stderr is discarded by the parent anyway.
	logrus.SetOutput(io.Discard)
	run(os.Stdin, os.Stdout)
}
