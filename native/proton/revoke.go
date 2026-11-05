package main

import (
	"context"
)

type revokeParams struct {
	envelopeFields
	sessionRequest
}

// revokeOp logs the session out server-side (DELETE /auth/v4). Idempotent:
// a session the server already forgot needs no closing.
func revokeOp(ctx context.Context, raw []byte) envelope {
	var params revokeParams
	if !decodeStrict(raw, &params) {
		return failure("invalid_request")
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	err := session.client.AuthDelete(ctx)
	if err == nil {
		return success(map[string]any{})
	}
	switch code := session.classify(err); code {
	case "session_revoked":
		return success(map[string]any{})
	case "timeout":
		return session.fail(code)
	}
	return session.fail("revoke_failed")
}
