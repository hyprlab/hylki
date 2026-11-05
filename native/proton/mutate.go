package main

import (
	"context"
	"slices"

	proton "github.com/ProtonMail/go-proton-api"
)

// The Tuta helper's batch cap: larger batches are refused rather than
// half-applied, so callers chunk first.
const batchLimit = 50

type markReadParams struct {
	envelopeFields
	sessionRequest
	IDs    []string `json:"ids"`
	IsRead *bool    `json:"is_read"`
}

type moveParams struct {
	envelopeFields
	sessionRequest
	IDs         []string `json:"ids"`
	Destination string   `json:"destination"`
}

type starParams struct {
	envelopeFields
	sessionRequest
	IDs       []string `json:"ids"`
	IsStarred *bool    `json:"is_starred"`
}

// checkBatch validates ids before anything is sent: the batch cap, then
// every id's shape (ids are spliced into requests).
func checkBatch(ids []string) string {
	if ids == nil {
		return "invalid_request"
	}
	if len(ids) > batchLimit {
		return "invalid_batch"
	}
	for _, id := range ids {
		if !validID(id) {
			return "invalid_id"
		}
	}
	return ""
}

func markReadOp(ctx context.Context, raw []byte) envelope {
	var params markReadParams
	if !decodeStrict(raw, &params) || params.IsRead == nil {
		return failure("invalid_request")
	}
	if code := checkBatch(params.IDs); code != "" {
		return failure(code)
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	isRead := *params.IsRead
	return session.batch(ctx, params.IDs, func(ctx context.Context, ids ...string) error {
		if isRead {
			return session.client.MarkMessagesRead(ctx, ids...)
		}
		return session.client.MarkMessagesUnread(ctx, ids...)
	})
}

func moveOp(ctx context.Context, raw []byte) envelope {
	var params moveParams
	if !decodeStrict(raw, &params) {
		return failure("invalid_request")
	}
	if code := checkBatch(params.IDs); code != "" {
		return failure(code)
	}
	if !validID(params.Destination) {
		return failure("invalid_id")
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	if code := session.checkDestination(ctx, params.Destination); code != "" {
		return session.fail(code)
	}
	return session.batch(ctx, params.IDs, func(ctx context.Context, ids ...string) error {
		// Labelling with a folder (an exclusive label) is Proton's move: the
		// server drops the message's previous folder.
		return session.client.LabelMessages(ctx, ids, params.Destination)
	})
}

// starOp adds or removes the Starred system label. Starring is a label, not
// a folder, so the message stays where it is.
func starOp(ctx context.Context, raw []byte) envelope {
	var params starParams
	if !decodeStrict(raw, &params) || params.IsStarred == nil {
		return failure("invalid_request")
	}
	if code := checkBatch(params.IDs); code != "" {
		return failure(code)
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	isStarred := *params.IsStarred
	return session.batch(ctx, params.IDs, func(ctx context.Context, ids ...string) error {
		if isStarred {
			return session.client.LabelMessages(ctx, ids, proton.StarredLabel)
		}
		return session.client.UnlabelMessages(ctx, ids, proton.StarredLabel)
	})
}

// checkDestination accepts the system folders mail can be filed into and
// the user's own folders. A label is not a place (adding one would leave the
// message where it is), nor are views like All Mail, Sent or Starred.
func (s *apiSession) checkDestination(ctx context.Context, destination string) string {
	for _, folder := range systemFolders {
		if folder.id == destination {
			if folder.movable {
				return ""
			}
			return "invalid_destination"
		}
	}
	labels, err := s.client.GetLabels(ctx, proton.LabelTypeFolder, proton.LabelTypeLabel)
	if err != nil {
		return s.classify(err)
	}
	index := slices.IndexFunc(labels, func(label proton.Label) bool { return label.ID == destination })
	switch {
	case index < 0:
		return "folder_not_found"
	case labels[index].Type != proton.LabelTypeFolder:
		return "invalid_destination"
	}
	return ""
}

// batch applies one mutation to ids and reports per-id outcomes in input
// order, the Tuta helper's shape: {"id", "code"} with a null code for an
// applied id. A failure that says nothing about the ids (revoked session,
// outage, timeout) fails the whole request. A failure the server reported
// against the batch is replayed one id at a time — every mutation here is
// idempotent — so each id gets its own outcome; an outage mid-replay marks
// that id and leaves the rest not_attempted.
func (s *apiSession) batch(ctx context.Context, ids []string, apply func(ctx context.Context, ids ...string) error) envelope {
	rows := make([]map[string]any, len(ids))
	for i, id := range ids {
		rows[i] = map[string]any{"id": id, "code": nil}
	}
	if len(ids) == 0 {
		return s.ok(map[string]any{"outcomes": rows})
	}
	err := apply(ctx, ids...)
	if err == nil {
		return s.ok(map[string]any{"outcomes": rows})
	}
	if code := s.classify(err); code != "request_failed" {
		return s.fail(code)
	}
	for i, id := range ids {
		err := apply(ctx, id)
		if err == nil {
			continue
		}
		code := s.classify(err)
		rows[i]["code"] = code
		if code != "request_failed" {
			for _, row := range rows[i+1:] {
				row["code"] = "not_attempted"
			}
			break
		}
	}
	return s.ok(map[string]any{"outcomes": rows})
}
