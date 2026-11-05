package main

import (
	"context"
	"errors"
	"net/http"

	proton "github.com/ProtonMail/go-proton-api"
)

// The API's metadata page cap.
const pageSize = 150

// The folder id snapshot walks every message in the folder, one request
// per page. Past this size the walk would not fit the operation budget, so
// all_ids is null (no snapshot) rather than a timeout.
const maxSnapshot = 10_000

// Event pages: GetEvent already chains up to 50 events per call.
const maxEventRounds = 10

type syncParams struct {
	envelopeFields
	sessionRequest
	Folder *string `json:"folder"`
	Limit  *int    `json:"limit"`
	Offset *int    `json:"offset"`
	Cursor *string `json:"cursor"`
	Since  *string `json:"since"`
}

// systemFolder is one of the system labels shown as a folder. The others
// (all-drafts, all-sent, outbox, scheduled, snoozed, ...) are views or
// transient states, not places a message lives.
type systemFolder struct {
	id, role, label string
	// movable: whether `move` may file mail here.
	movable bool
}

var systemFolders = []systemFolder{
	{proton.InboxLabel, "inbox", "Inbox", true},
	{proton.DraftsLabel, "drafts", "Drafts", false},
	{proton.SentLabel, "sent", "Sent", false},
	{proton.StarredLabel, "starred", "Starred", false},
	{proton.ArchiveLabel, "archive", "Archive", true},
	{proton.SpamLabel, "junk", "Spam", true},
	{proton.TrashLabel, "trash", "Trash", true},
	{proton.AllMailLabel, "other", "All Mail", false},
}

func syncOp(ctx context.Context, raw []byte) envelope {
	var params syncParams
	if !decodeStrict(raw, &params) || params.Limit == nil {
		return failure("invalid_request")
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	offset := 0
	if params.Offset != nil {
		offset = *params.Offset
	}
	limit := *params.Limit
	if limit <= 0 || limit > 1000 || offset < 0 || offset > 1_000_000 {
		return failure("invalid_page")
	}
	if params.Cursor != nil && offset != 0 {
		return failure("invalid_page")
	}
	target := proton.InboxLabel
	if params.Folder != nil {
		target = *params.Folder
	}
	for _, id := range []*string{&target, params.Cursor, params.Since} {
		if id != nil && !validID(*id) {
			return failure("invalid_id")
		}
	}
	session := resume(*params.Session)
	defer session.close()
	return session.sync(ctx, target, limit, offset, params.Cursor, params.Since)
}

func (s *apiSession) sync(ctx context.Context, target string, limit, offset int, cursor, since *string) envelope {
	result := map[string]any{}
	// The event id is taken before anything is listed: whatever changes
	// while the listing runs is delivered again from it, never skipped.
	if since == nil {
		eventID, err := s.client.GetLatestEventID(ctx)
		if err != nil {
			return s.failErr(err)
		}
		result["event_id"] = eventID
	} else {
		changes, eventID, err := s.changes(ctx, *since)
		if err != nil {
			return s.failErr(err)
		}
		result["changes"] = changes
		result["event_id"] = eventID
	}
	folders, kinds, err := s.folders(ctx)
	if err != nil {
		return s.failErr(err)
	}
	if _, ok := kinds[target]; !ok {
		return s.fail("folder_not_found")
	}
	exists := 0
	for _, row := range folders {
		if row["id"] == target {
			exists = row["total"].(int)
		}
	}
	var (
		page     []proton.MessageMetadata
		snapshot []string
	)
	switch {
	case cursor != nil:
		page, err = s.keyset(ctx, target, *cursor, limit)
	case offset == 0 && since == nil && exists <= maxSnapshot:
		var all []proton.MessageMetadata
		all, err = s.keyset(ctx, target, "", -1)
		snapshot = make([]string, len(all))
		for i, meta := range all {
			snapshot[i] = meta.ID
		}
		page = all[:min(limit, len(all))]
	default:
		page, err = s.window(ctx, target, offset, limit)
	}
	if err != nil {
		return s.failErr(err)
	}
	messages := make([]map[string]any, len(page))
	for i, meta := range page {
		messages[i] = headerRow(meta)
	}
	result["folders"] = folders
	result["messages"] = messages
	result["folder"] = target
	result["exists"] = exists
	result["offset"] = offset
	if snapshot != nil {
		result["all_ids"] = snapshot
	} else {
		result["all_ids"] = nil
	}
	if len(page) == limit && len(page) > 0 {
		result["next_cursor"] = page[len(page)-1].ID
	} else {
		result["next_cursor"] = nil
	}
	return s.ok(result)
}

// folders lists the system folders, then the user's folders and labels,
// with per-label totals and unread counts. kinds maps every listed id to
// its kind, which is what `move` and the target check consult.
func (s *apiSession) folders(ctx context.Context) ([]map[string]any, map[string]string, error) {
	labels, err := s.client.GetLabels(ctx, proton.LabelTypeFolder, proton.LabelTypeLabel)
	if err != nil {
		return nil, nil, err
	}
	counts, err := s.client.GetGroupedMessageCount(ctx)
	if err != nil {
		return nil, nil, err
	}
	totals := make(map[string]proton.MessageGroupCount, len(counts))
	for _, count := range counts {
		totals[count.LabelID] = count
	}
	rows := make([]map[string]any, 0, len(systemFolders)+len(labels))
	kinds := make(map[string]string, cap(rows))
	add := func(id, role, label, parent, kind string) {
		rows = append(rows, map[string]any{
			"id": id, "role": role, "label": label, "parent": parent, "kind": kind,
			"total": totals[id].Total, "unread": totals[id].Unread,
		})
		kinds[id] = kind
	}
	for _, folder := range systemFolders {
		add(folder.id, folder.role, folder.label, "", "system")
	}
	for _, label := range labels {
		if !validID(label.ID) {
			continue
		}
		kind := "label"
		if label.Type == proton.LabelTypeFolder {
			kind = "folder"
		}
		add(label.ID, "other", label.Name, label.ParentID, kind)
	}
	return rows, kinds, nil
}

// keyset lists a folder newest-first from cursor (exclusive; "" starts at
// the newest) until want messages are collected, or to the end when want is
// negative. Each page continues from the last id seen — EndID is inclusive,
// so that id is dropped — so mail arriving mid-walk cannot shift pages and
// hide a message, as offset paging would.
func (s *apiSession) keyset(ctx context.Context, label, cursor string, want int) ([]proton.MessageMetadata, error) {
	var all []proton.MessageMetadata
	filter := proton.MessageFilter{LabelID: label, Desc: true, EndID: cursor}
	for want < 0 || len(all) < want {
		page, err := s.client.GetMessageMetadataPage(ctx, 0, pageSize, filter)
		if err != nil {
			return nil, err
		}
		isFull := len(page) == pageSize
		if filter.EndID != "" && len(page) > 0 && page[0].ID == filter.EndID {
			page = page[1:]
		}
		if len(page) == 0 {
			break
		}
		all = append(all, page...)
		filter.EndID = page[len(page)-1].ID
		if !isFull {
			break
		}
	}
	if want >= 0 && len(all) > want {
		all = all[:want]
	}
	return all, nil
}

// window is the offset page: the API pages in fixed sizes, so the covering
// pages are fetched and sliced.
func (s *apiSession) window(ctx context.Context, label string, offset, limit int) ([]proton.MessageMetadata, error) {
	filter := proton.MessageFilter{LabelID: label, Desc: true}
	var covered []proton.MessageMetadata
	first, last := offset/pageSize, (offset+limit-1)/pageSize
	for number := first; number <= last; number++ {
		page, err := s.client.GetMessageMetadataPage(ctx, number, pageSize, filter)
		if err != nil {
			return nil, err
		}
		covered = append(covered, page...)
		if len(page) < pageSize {
			break
		}
	}
	start := offset - first*pageSize
	if start >= len(covered) {
		return nil, nil
	}
	return covered[start:min(start+limit, len(covered))], nil
}

// changes replays the event log from since: which messages were deleted and
// which were created or changed (with their current metadata, re-read so a
// partial flag update never reaches the caller as a whole header). An event
// id the server no longer knows, or a refresh the server demands, comes back
// as refresh: the caller resyncs from scratch from the returned event id.
func (s *apiSession) changes(ctx context.Context, since string) (map[string]any, string, error) {
	type change struct{ deleted bool }
	order := []string{}
	seen := map[string]*change{}
	refresh, more := false, false
	eventID := since
	for range maxEventRounds {
		events, pending, err := s.client.GetEvent(ctx, eventID)
		if err != nil {
			if !staleEvent(err) || s.wasDeauthed() {
				return nil, "", err
			}
			latest, err := s.client.GetLatestEventID(ctx)
			if err != nil {
				return nil, "", err
			}
			return refreshChanges(), latest, nil
		}
		for _, event := range events {
			if event.EventID != "" {
				eventID = event.EventID
			}
			if event.Refresh&proton.RefreshMail != 0 {
				refresh = true
			}
			for _, item := range event.Messages {
				if !validID(item.ID) {
					continue
				}
				entry, ok := seen[item.ID]
				if !ok {
					entry = &change{}
					seen[item.ID] = entry
					order = append(order, item.ID)
				}
				entry.deleted = item.Action == proton.EventDelete
			}
		}
		more = pending
		if !pending {
			break
		}
	}
	if refresh {
		return refreshChanges(), eventID, nil
	}
	var deleted, changed []string
	for _, id := range order {
		if seen[id].deleted {
			deleted = append(deleted, id)
		} else {
			changed = append(changed, id)
		}
	}
	updated := []map[string]any{}
	found := map[string]bool{}
	for start := 0; start < len(changed); start += pageSize {
		chunk := changed[start:min(start+pageSize, len(changed))]
		metas, err := s.client.GetMessageMetadataPage(ctx, 0, pageSize, proton.MessageFilter{ID: chunk})
		if err != nil {
			return nil, "", err
		}
		for _, meta := range metas {
			found[meta.ID] = true
			updated = append(updated, headerRow(meta))
		}
	}
	// Changed, then gone before it could be read: deleted after all.
	for _, id := range changed {
		if !found[id] {
			deleted = append(deleted, id)
		}
	}
	if deleted == nil {
		deleted = []string{}
	}
	return map[string]any{"refresh": false, "more": more, "deleted": deleted, "updated": updated}, eventID, nil
}

func refreshChanges() map[string]any {
	return map[string]any{"refresh": true, "more": false, "deleted": []string{}, "updated": []map[string]any{}}
}

// staleEvent reports whether the server refused the event id itself (too
// old, unknown) rather than failing the request: a client error that is not
// about authentication or throttling.
func staleEvent(err error) bool {
	var apiErr *proton.APIError
	if !errors.As(err, &apiErr) {
		return false
	}
	switch apiErr.Status {
	case http.StatusBadRequest, http.StatusNotFound, http.StatusConflict, http.StatusUnprocessableEntity:
		return true
	}
	return false
}

// headerRow is the Tuta helper's header shape plus what Proton's metadata
// offers for free: the original Message-ID, label ids, star, attachments.
func headerRow(meta proton.MessageMetadata) map[string]any {
	var recipient any
	for _, list := range [][]*mailAddress{toMail(meta.ToList), toMail(meta.CCList), toMail(meta.BCCList)} {
		if len(list) > 0 {
			recipient = list[0]
			break
		}
	}
	labels := meta.LabelIDs
	if labels == nil {
		labels = []string{}
	}
	sender := &mailAddress{}
	if meta.Sender != nil {
		sender = &mailAddress{Name: meta.Sender.Name, Address: meta.Sender.Address}
	}
	return map[string]any{
		"id":              meta.ID,
		"subject":         meta.Subject,
		"date_ms":         meta.Time * 1000,
		"sender":          sender,
		"recipient":       recipient,
		"is_unread":       bool(meta.Unread),
		"confidential":    meta.Flags.Has(proton.MessageFlagE2E),
		"message_id":      meta.ExternalID,
		"label_ids":       labels,
		"is_starred":      meta.Starred(),
		"is_draft":        meta.IsDraft(),
		"has_attachments": meta.NumAttachments > 0,
		"size":            meta.Size,
	}
}
