package main

import (
	"context"
	"slices"
	"strings"

	proton "github.com/ProtonMail/go-proton-api"
)

const (
	// RFC 5322's line limit bounds a single Message-ID.
	maxMessageIDBytes = 998
	// A References header past this is not a thread, it is a payload.
	maxReferences      = 100
	maxReferencesBytes = 16 * 1024
	// Lookups per send: In-Reply-To, then the newest references.
	maxParentLookups = 4
)

// threading is how a send relates to an earlier message: the Message-IDs
// that may name its parent, most specific first, and what the send does
// to that parent.
type threading struct {
	candidates []string
	action     proton.CreateDraftAction
}

var draftActions = map[string]proton.CreateDraftAction{
	"reply":     proton.ReplyAction,
	"reply_all": proton.ReplyAllAction,
	"forward":   proton.ForwardAction,
}

// threadingOf validates the send's optional in_reply_to, references and
// action. Every Message-ID must be well formed; a malformed one refuses
// the send rather than being dropped, so the caller learns its input was
// wrong. An action without any Message-ID to apply it to is refused too.
func threadingOf(inReplyTo, references, action *string) (threading, bool) {
	var ids []string
	if inReplyTo != nil {
		id, ok := bareMessageID(*inReplyTo)
		if !ok {
			return threading{}, false
		}
		ids = append(ids, id)
	}
	if references != nil {
		if len(*references) > maxReferencesBytes {
			return threading{}, false
		}
		fields, ok := referenceIDs(*references)
		if !ok || len(fields) > maxReferences {
			return threading{}, false
		}
		// The newest reference is the parent's own Message-ID, so look the
		// list up from its end.
		for i := len(fields) - 1; i >= 0; i-- {
			id, ok := bareMessageID(fields[i])
			if !ok {
				return threading{}, false
			}
			ids = append(ids, id)
		}
	}
	chosen := proton.ReplyAction
	if action != nil {
		value, known := draftActions[*action]
		if !known || len(ids) == 0 {
			return threading{}, false
		}
		chosen = value
	}
	var candidates []string
	for _, id := range ids {
		if !slices.Contains(candidates, id) && len(candidates) < maxParentLookups {
			candidates = append(candidates, id)
		}
	}
	return threading{candidates: candidates, action: chosen}, true
}

// referenceIDs splits a References value into its Message-IDs. Bracketed
// ids are read bracket to bracket, so a space inside one is caught rather
// than splitting it in two, and glued ids ("<a@b><c@d>") still separate;
// only whitespace may sit between them. A value with no brackets at all is
// taken as whitespace-separated bare ids.
func referenceIDs(value string) ([]string, bool) {
	if !strings.ContainsAny(value, "<>") {
		return strings.Fields(value), true
	}
	var ids []string
	rest := value
	for {
		rest = strings.TrimLeft(rest, " \t\r\n")
		if rest == "" {
			return ids, true
		}
		end := strings.IndexByte(rest, '>')
		if rest[0] != '<' || end < 0 {
			return nil, false
		}
		ids = append(ids, rest[:end+1])
		rest = rest[end+1:]
	}
}

// bareMessageID strips one pair of angle brackets and accepts the rest when
// it is printable ASCII without spaces or brackets, the form Proton stores
// as a message's ExternalID. No "@" is required: real mail carries ids
// without one, and the lookup is an exact match either way.
func bareMessageID(value string) (string, bool) {
	id := strings.TrimSpace(value)
	id = strings.TrimSuffix(strings.TrimPrefix(id, "<"), ">")
	if id == "" || len(id) > maxMessageIDBytes {
		return "", false
	}
	for i := range len(id) {
		if c := id[i]; c <= ' ' || c > '~' || c == '<' || c == '>' {
			return "", false
		}
	}
	return id, true
}

// findParent looks the candidates up by ExternalID in the account's mail,
// in order, and returns the first match's Proton id, or "" when none is
// there. Drafts never count. Several copies of one Message-ID (the same mail
// reached two of the account's addresses, or a message sent to oneself) are
// resolved toward the copy on the sending address, then a sent copy, then
// the first, as Bridge's lookup prefers sent copies.
func (s *apiSession) findParent(ctx context.Context, thread threading, senderAddressID string) (string, string) {
	for _, id := range thread.candidates {
		metas, err := s.client.GetMessageMetadataPage(ctx, 0, pageSize, proton.MessageFilter{ExternalID: id})
		if err != nil {
			return "", s.preSubmit(err)
		}
		var matches []proton.MessageMetadata
		for _, meta := range metas {
			if meta.ExternalID == id && !meta.IsDraft() && validID(meta.ID) {
				matches = append(matches, meta)
			}
		}
		if len(matches) == 0 {
			continue
		}
		for _, meta := range matches {
			if meta.AddressID == senderAddressID {
				return meta.ID, ""
			}
		}
		for _, meta := range matches {
			if meta.Flags.Has(proton.MessageFlagSent) {
				return meta.ID, ""
			}
		}
		return matches[0].ID, ""
	}
	return "", ""
}
