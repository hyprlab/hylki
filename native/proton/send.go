package main

import (
	"bytes"
	"context"
	cryptorand "crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"net/http"
	"net/mail"
	"slices"
	"strings"
	"unicode"

	"github.com/ProtonMail/gluon/rfc822"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
	"github.com/emersion/go-message"
	"github.com/go-resty/resty/v2"
)

// API codes for refusals that name what was wrong (from Bridge's
// internal/services/smtp/errors.go).
const (
	codeValidationFailed      proton.Code = 2001
	codeInvalidRecipientList  proton.Code = 2002
	codeMessageTooLarge       proton.Code = 2024
	codeAddressDoesNotExist   proton.Code = 33102
	draftsPath                            = "/mail/v4/messages"
	sendPathPrefix                        = "/mail/v4/messages/"
	deliverySecure                        = "secure"
	deliveryOrdinary                      = "ordinary"
	defaultAttachmentFilename             = "attachment"
)

type sendAttachment struct {
	Name     string  `json:"name"`
	MIMEType string  `json:"mime_type"`
	CID      *string `json:"cid"`
	// Standard-base64 file bytes.
	Data string `json:"data"`
}

type sendParams struct {
	envelopeFields
	sessionRequest
	Sender            string           `json:"sender"`
	SenderName        string           `json:"sender_name"`
	To                []mailAddress    `json:"to"`
	Cc                []mailAddress    `json:"cc"`
	Bcc               []mailAddress    `json:"bcc"`
	ReplyTos          []mailAddress    `json:"reply_tos"`
	Subject           string           `json:"subject"`
	BodyHTML          string           `json:"body_html"`
	Delivery          string           `json:"delivery"`
	OrdinaryConfirmed bool             `json:"ordinary_confirmed"`
	Attachments       []sendAttachment `json:"attachments"`
	// Optional Message-ID (with or without angle brackets) for the sent
	// mail; one is generated when absent so the reply can always name it.
	MessageID *string `json:"message_id"`
	// Optional threading: the Message-ID being answered, the References
	// chain (space-separated Message-IDs, oldest first), and what the send
	// does to that parent — "reply" (default), "reply_all" or "forward".
	InReplyTo  *string `json:"in_reply_to"`
	References *string `json:"references"`
	Action     *string `json:"action"`
}

// stagedAttachment is a decoded attachment ready to upload.
type stagedAttachment struct {
	name, mimeType, cid string
	disposition         proton.Disposition
	data                []byte
}

func sendOp(ctx context.Context, raw []byte) envelope {
	var params sendParams
	if !decodeStrict(raw, &params) {
		return failure("invalid_request")
	}
	if params.Delivery != deliverySecure && params.Delivery != deliveryOrdinary {
		return failure("invalid_request")
	}
	if params.Delivery == deliveryOrdinary && !params.OrdinaryConfirmed {
		return failure("ordinary_delivery_requires_consent")
	}
	recipients := recipientList(params.To, params.Cc, params.Bcc)
	if len(recipients) == 0 || slices.ContainsFunc(recipients, func(a string) bool { return !validAddress(a) }) ||
		slices.ContainsFunc(params.ReplyTos, func(a mailAddress) bool { return !validAddress(a.Address) }) {
		return failure("invalid_recipient")
	}
	if !validAddress(params.Sender) {
		return failure("sender_unknown")
	}
	externalID, ok := messageID(params.MessageID, params.Sender)
	if !ok {
		return failure("invalid_request")
	}
	thread, ok := threadingOf(params.InReplyTo, params.References, params.Action)
	if !ok {
		return failure("invalid_request")
	}
	staged, ok := stageAttachments(params.Attachments)
	if !ok {
		return failure("attachment_failed")
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	return session.send(ctx, params, recipients, externalID, staged, thread)
}

// recipientList is every envelope recipient once, in To, Cc, Bcc order.
func recipientList(lists ...[]mailAddress) []string {
	var addresses []string
	seen := map[string]bool{}
	for _, list := range lists {
		for _, address := range list {
			key := strings.ToLower(address.Address)
			if !seen[key] {
				seen[key] = true
				addresses = append(addresses, address.Address)
			}
		}
	}
	return addresses
}

// messageID normalises a caller-supplied Message-ID to Proton's bare
// ExternalID form, or makes one on the sender's domain.
func messageID(requested *string, sender string) (string, bool) {
	if requested == nil {
		token := make([]byte, 16)
		if _, err := cryptorand.Read(token); err != nil {
			return "", false
		}
		_, domain, _ := strings.Cut(sender, "@")
		return hex.EncodeToString(token) + "@" + domain, true
	}
	id := strings.TrimSuffix(strings.TrimPrefix(strings.TrimSpace(*requested), "<"), ">")
	if !validAddress(id) || strings.ContainsAny(id, "<>") {
		return "", false
	}
	return id, true
}

func stageAttachments(attachments []sendAttachment) ([]stagedAttachment, bool) {
	staged := make([]stagedAttachment, 0, len(attachments))
	for _, attachment := range attachments {
		data, err := base64.StdEncoding.DecodeString(attachment.Data)
		if err != nil {
			return nil, false
		}
		name := headerSafe(attachment.Name)
		if name == "" {
			name = defaultAttachmentFilename
		}
		item := stagedAttachment{
			name:        name,
			mimeType:    normaliseMIME(attachment.MIMEType),
			disposition: proton.AttachmentDisposition,
			data:        data,
		}
		// The API refuses an inline attachment without a Content-ID, so
		// only a cid makes one inline (Bridge's rule).
		if attachment.CID != nil {
			if cid := headerSafe(strings.Trim(*attachment.CID, " <>")); cid != "" {
				item.cid = cid
				item.disposition = proton.InlineDisposition
			}
		}
		staged = append(staged, item)
	}
	return staged, true
}

// headerSafe drops control characters, so request data never starts a
// header of its own in the MIME this helper writes.
func headerSafe(text string) string {
	return strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return -1
		}
		return r
	}, strings.TrimSpace(text))
}

// normaliseMIME is a type/subtype pair, or the octet-stream fallback.
func normaliseMIME(mimeType string) string {
	main, sub, ok := strings.Cut(strings.ToLower(strings.TrimSpace(mimeType)), "/")
	isToken := func(part string) bool {
		return part != "" && !strings.ContainsFunc(part, func(r rune) bool {
			return r > unicode.MaxASCII || unicode.IsSpace(r) || unicode.IsControl(r) || strings.ContainsRune(`()<>@,;:\"/[]?=`, r)
		})
	}
	if !ok || !isToken(main) || !isToken(sub) {
		return "application/octet-stream"
	}
	return main + "/" + sub
}

// send composes and submits one message the way Bridge does: every
// recipient's preferences are settled before anything is written server
// side, then a draft is created and its attachments uploaded, then the
// per-scheme packages go out in one submit. Internal recipients get the body
// encrypted to their key; external ones get PGP when the API or their
// contact card has a key, cleartext otherwise. The server files the Sent
// copy and delivers Bcc from the packages, never from a header.
func (s *apiSession) send(ctx context.Context, params sendParams, recipients []string, externalID string, staged []stagedAttachment, thread threading) envelope {
	keys, code := s.keyrings(ctx)
	if code != "" {
		return s.fail(code)
	}
	index := slices.IndexFunc(keys.addresses, func(a proton.Address) bool { return strings.EqualFold(a.Email, params.Sender) })
	if index < 0 || !bool(keys.addresses[index].Send) || keys.addresses[index].Status != proton.AddressStatusEnabled {
		return s.fail("sender_unknown")
	}
	sender := keys.addresses[index]
	senderKR, ok := keys.addrKRs[sender.ID]
	if !ok {
		return s.fail("send_failed")
	}
	addrKR, err := senderKR.FirstKey()
	if err != nil {
		return s.fail("send_failed")
	}
	settings, err := s.client.GetMailSettings(ctx)
	if err != nil {
		return s.fail(s.preSubmit(err))
	}
	prefs := make(map[string]proton.SendPreferences, len(recipients))
	internal := 0
	for _, address := range recipients {
		pref, isInternal, code := s.recipientPrefs(ctx, keys.userKR, settings, address)
		if code != "" {
			return s.fail(code)
		}
		if params.Delivery == deliverySecure && !pref.Encrypt {
			return s.fail("confidential_send_unsupported")
		}
		if isInternal {
			internal++
		}
		prefs[address] = pref
	}
	// The parent is resolved before anything is written: a lookup that
	// fails for want of a connection fails the send definitively.
	parentID, code := s.findParent(ctx, thread, sender.ID)
	if code != "" {
		return s.fail(code)
	}
	// With no parent the action is moot; Bridge sends forward.
	action := proton.ForwardAction
	if parentID != "" {
		action = thread.action
	}
	if len(params.ReplyTos) > 0 {
		replyTos := fromMail(params.ReplyTos)
		s.client.AddPreRequestHook(func(_ *resty.Client, r *resty.Request) error {
			return withReplyTos(r, replyTos)
		})
	}
	draft, err := s.client.CreateDraft(ctx, addrKR, proton.CreateDraftReq{
		Message: proton.DraftTemplate{
			Subject:    params.Subject,
			Sender:     &mail.Address{Name: params.SenderName, Address: sender.Email},
			ToList:     fromMail(params.To),
			CCList:     fromMail(params.Cc),
			BCCList:    fromMail(params.Bcc),
			Body:       params.BodyHTML,
			MIMEType:   rfc822.TextHTML,
			ExternalID: externalID,
		},
		ParentID: parentID,
		Action:   action,
	})
	if err != nil {
		return s.fail(s.refusal(err))
	}
	if !validID(draft.ID) {
		return s.fail("send_failed")
	}
	attKeys := make(map[string]*crypto.SessionKey, len(staged))
	for _, attachment := range staged {
		uploaded, err := s.client.UploadAttachment(ctx, addrKR, proton.CreateAttachmentReq{
			MessageID:   draft.ID,
			Filename:    attachment.name,
			MIMEType:    rfc822.MIMEType(attachment.mimeType),
			Disposition: attachment.disposition,
			ContentID:   attachment.cid,
			Body:        attachment.data,
		})
		if err != nil {
			code := s.classify(err)
			if code == "request_failed" {
				code = "attachment_failed"
			}
			s.discard(draft.ID)
			return s.fail(code)
		}
		key, ok := attachmentKey(addrKR, uploaded)
		if !ok {
			s.discard(draft.ID)
			return s.fail("attachment_failed")
		}
		attKeys[uploaded.ID] = key
	}
	mimeBody, err := buildMIMEBody(params.BodyHTML, staged)
	if err != nil {
		s.discard(draft.ID)
		return s.fail("send_failed")
	}
	request, err := packages(addrKR, mimeBody, params.BodyHTML, htmlToText(params.BodyHTML), prefs, attKeys)
	if err != nil {
		s.discard(draft.ID)
		return s.fail("send_failed")
	}
	sent, code := s.submit(ctx, draft.ID, request)
	if code != "" {
		return s.fail(code)
	}
	if sent.ExternalID != "" {
		externalID = sent.ExternalID
	}
	return s.ok(map[string]any{
		"message_id":          externalID,
		"sent_mail":           sent.ID,
		"sent_date_ms":        sent.Time * 1000,
		"internal_recipients": internal,
		"external_recipients": len(recipients) - internal,
	})
}

// recipientPrefs looks one recipient up: its API keys (and whether it is a
// Proton address) and its contact card, then applies the preference table.
func (s *apiSession) recipientPrefs(ctx context.Context, userKR *crypto.KeyRing, settings proton.MailSettings, address string) (proton.SendPreferences, bool, string) {
	keys, recipientType, err := s.client.GetPublicKeys(ctx, address)
	if err != nil {
		var apiErr *proton.APIError
		if errors.As(err, &apiErr) && apiErr.Code == codeAddressDoesNotExist {
			return proton.SendPreferences{}, false, "invalid_recipient"
		}
		return proton.SendPreferences{}, false, s.preSubmit(err)
	}
	contact, code := s.contactSettings(ctx, userKR, address)
	if code != "" {
		return proton.SendPreferences{}, false, code
	}
	isInternal := recipientType == proton.RecipientTypeInternal
	prefs, err := buildSendPrefs(contact, settings, keys, rfc822.TextHTML, isInternal)
	if err != nil {
		return proton.SendPreferences{}, false, "send_failed"
	}
	return prefs, isInternal, ""
}

// contactSettings reads a contact card's per-address PGP settings. A card
// that cannot be read fails the send rather than falling back to defaults:
// it may be what pins a key or demands encryption.
func (s *apiSession) contactSettings(ctx context.Context, userKR *crypto.KeyRing, address string) (proton.ContactSettings, string) {
	emails, err := s.client.GetAllContactEmails(ctx, address)
	if err != nil {
		return proton.ContactSettings{}, s.preSubmit(err)
	}
	index := slices.IndexFunc(emails, func(e proton.ContactEmail) bool { return strings.EqualFold(e.Email, address) })
	if index < 0 {
		return proton.ContactSettings{}, ""
	}
	if !validID(emails[index].ContactID) {
		return proton.ContactSettings{}, "send_failed"
	}
	contact, err := s.client.GetContact(ctx, emails[index].ContactID)
	if err != nil {
		return proton.ContactSettings{}, s.preSubmit(err)
	}
	settings, err := contact.GetSettings(userKR, address, proton.CardTypeSigned)
	if err != nil {
		return proton.ContactSettings{}, "send_failed"
	}
	return settings, ""
}

// withReplyTos adds Reply-To addresses to the draft-create body, which
// go-proton-api's DraftTemplate has no field for. Only that one request is
// touched: a POST to the drafts path whose body is a CreateDraftReq.
func withReplyTos(r *resty.Request, replyTos []*mail.Address) error {
	draft, ok := r.Body.(proton.CreateDraftReq)
	if !ok || r.Method != http.MethodPost || r.URL != draftsPath {
		return nil
	}
	type template struct {
		proton.DraftTemplate
		ReplyTos []*mail.Address
	}
	r.SetBody(struct {
		Message              template
		AttachmentKeyPackets []string
		ParentID             string `json:",omitempty"`
		Action               proton.CreateDraftAction
	}{
		Message:              template{DraftTemplate: draft.Message, ReplyTos: replyTos},
		AttachmentKeyPackets: draft.AttachmentKeyPackets,
		ParentID:             draft.ParentID,
		Action:               draft.Action,
	})
	return nil
}

func attachmentKey(addrKR *crypto.KeyRing, attachment proton.Attachment) (*crypto.SessionKey, bool) {
	if !validID(attachment.ID) {
		return nil, false
	}
	keyPacket, err := base64.StdEncoding.DecodeString(attachment.KeyPackets)
	if err != nil {
		return nil, false
	}
	key, err := addrKR.DecryptSessionKey(keyPacket)
	return key, err == nil
}

// buildMIMEBody is the multipart/mixed body PGP/MIME and signed-cleartext
// recipients receive: the HTML part plus every attachment, since those
// schemes carry attachments inside the encrypted or signed body.
func buildMIMEBody(body string, attachments []stagedAttachment) (string, error) {
	var buffer bytes.Buffer
	var header message.Header
	header.SetContentType(string(rfc822.MultipartMixed), nil)
	writer, err := message.CreateWriter(&buffer, header)
	if err != nil {
		return "", err
	}
	var textHeader message.Header
	textHeader.SetContentType(string(rfc822.TextHTML), map[string]string{"charset": "utf-8"})
	textHeader.Set("Content-Transfer-Encoding", "quoted-printable")
	if err := writePart(writer, textHeader, []byte(body)); err != nil {
		return "", err
	}
	for _, attachment := range attachments {
		var partHeader message.Header
		partHeader.SetContentType(attachment.mimeType, map[string]string{"name": attachment.name})
		partHeader.SetContentDisposition(string(attachment.disposition), map[string]string{"filename": attachment.name})
		partHeader.Set("Content-Transfer-Encoding", "base64")
		if attachment.cid != "" {
			partHeader.Set("Content-ID", "<"+attachment.cid+">")
		}
		if err := writePart(writer, partHeader, attachment.data); err != nil {
			return "", err
		}
	}
	if err := writer.Close(); err != nil {
		return "", err
	}
	return buffer.String(), nil
}

func writePart(writer *message.Writer, header message.Header, data []byte) error {
	part, err := writer.CreatePart(header)
	if err != nil {
		return err
	}
	if _, err := part.Write(data); err != nil {
		return err
	}
	return part.Close()
}

// packages groups recipients by scheme into the send request, as Bridge's
// createSendReq does: one MIME package for PGP/MIME and signed-cleartext
// recipients, one text package per body format for the rest.
func packages(
	addrKR *crypto.KeyRing,
	mimeBody, richBody, plainBody string,
	prefs map[string]proton.SendPreferences,
	attKeys map[string]*crypto.SessionKey,
) (proton.SendDraftReq, error) {
	var request proton.SendDraftReq
	pick := func(match func(proton.SendPreferences) bool) map[string]proton.SendPreferences {
		picked := map[string]proton.SendPreferences{}
		for address, pref := range prefs {
			if match(pref) {
				picked[address] = pref
			}
		}
		return picked
	}
	mimeRecipients := pick(func(p proton.SendPreferences) bool {
		return p.EncryptionScheme == proton.PGPMIMEScheme || p.EncryptionScheme == proton.ClearMIMEScheme
	})
	if len(mimeRecipients) > 0 {
		if err := request.AddMIMEPackage(addrKR, mimeBody, mimeRecipients); err != nil {
			return proton.SendDraftReq{}, err
		}
	}
	isText := func(p proton.SendPreferences) bool {
		return p.EncryptionScheme == proton.InternalScheme || p.EncryptionScheme == proton.ClearScheme ||
			p.EncryptionScheme == proton.PGPInlineScheme
	}
	for _, format := range []struct {
		mimeType rfc822.MIMEType
		body     string
	}{{rfc822.TextHTML, richBody}, {rfc822.TextPlain, plainBody}} {
		recipients := pick(func(p proton.SendPreferences) bool { return isText(p) && p.MIMEType == format.mimeType })
		if len(recipients) == 0 {
			continue
		}
		if err := request.AddTextPackage(addrKR, format.body, format.mimeType, recipients, attKeys); err != nil {
			return proton.SendDraftReq{}, err
		}
	}
	covered := 0
	for _, pkg := range request.Packages {
		covered += len(pkg.Addresses)
	}
	if covered != len(prefs) {
		return proton.SendDraftReq{}, errors.New("a recipient fits no package")
	}
	return request, nil
}

// submit sends the draft. Before the request goes out nothing can have been
// delivered; after it, only a refusal the server decided before executing
// anything is definite (send_failed and friends — the draft is deleted, a
// retry is safe). Everything else may have delivered: the draft is probed,
// and unless it already reads as sent the result is send_uncertain, the
// draft is kept, and the caller must not retry automatically.
func (s *apiSession) submit(ctx context.Context, draftID string, request proton.SendDraftReq) (proton.Message, string) {
	attempts := 0
	s.client.AddPreRequestHook(func(_ *resty.Client, r *resty.Request) error {
		if r.Method == http.MethodPost && r.URL == sendPathPrefix+draftID {
			attempts++
		}
		return nil
	})
	sent, err := s.client.SendDraft(ctx, draftID, request)
	if err == nil {
		return sent, ""
	}
	// resty retries dropped connections and throttling; once a second
	// attempt went out, a refusal may only mean the first one succeeded.
	if attempts <= 1 {
		if code, isDefinite := definiteRefusal(err, s.wasDeauthed()); isDefinite {
			if code != "session_revoked" {
				s.discard(draftID)
			}
			return proton.Message{}, code
		}
	}
	if sent, ok := s.probeSent(draftID); ok {
		return sent, ""
	}
	return proton.Message{}, "send_uncertain"
}

// definiteRefusal reports whether a failed submit was refused before the
// server executed it, and with which code.
func definiteRefusal(err error, wasDeauthed bool) (string, bool) {
	if wasDeauthed {
		return "session_revoked", true
	}
	var apiErr *proton.APIError
	if !errors.As(err, &apiErr) {
		return "", false
	}
	switch {
	case apiErr.Status == http.StatusUnauthorized:
		return "session_revoked", true
	case apiErr.Status >= 500 || apiErr.Status == 0:
		return "", false
	case apiErr.Status == http.StatusTooManyRequests:
		return "transport_failed", true
	}
	return refusalCode(apiErr), true
}

// refusalCode names a 4xx refusal.
func refusalCode(apiErr *proton.APIError) string {
	switch apiErr.Code {
	case codeMessageTooLarge:
		return "message_too_large"
	case codeValidationFailed, codeInvalidRecipientList, codeAddressDoesNotExist:
		return "invalid_recipient"
	case proton.AppVersionBadCode, proton.AppVersionMissingCode:
		return "client_outdated"
	case proton.HumanVerificationRequired:
		return "human_verification_required"
	}
	return "send_failed"
}

// refusal classifies a failed draft create: nothing was sent, so every code
// is definite.
func (s *apiSession) refusal(err error) string {
	var apiErr *proton.APIError
	if !s.wasDeauthed() && errors.As(err, &apiErr) && apiErr.Status >= 400 && apiErr.Status < 500 &&
		apiErr.Status != http.StatusUnauthorized && apiErr.Status != http.StatusTooManyRequests {
		return refusalCode(apiErr)
	}
	return s.preSubmit(err)
}

// preSubmit classifies a failure before any submit: transport, session and
// timeout codes stay; anything else is a definite send_failed.
func (s *apiSession) preSubmit(err error) string {
	if code := s.classify(err); code != "request_failed" {
		return code
	}
	return "send_failed"
}

// probeSent checks whether an uncertain submit did go out after all.
func (s *apiSession) probeSent(draftID string) (proton.Message, bool) {
	ctx, cancel := context.WithTimeout(context.Background(), abandonTimeout)
	defer cancel()
	msg, err := s.client.GetMessage(ctx, draftID)
	if err != nil || !msg.Flags.Has(proton.MessageFlagSent) {
		return proton.Message{}, false
	}
	return msg, true
}

// discard best-effort deletes a draft whose send definitely failed, so a
// failed send leaves no orphan in Drafts. The caller's outbox row stays the
// recovery point either way.
func (s *apiSession) discard(draftID string) {
	ctx, cancel := context.WithTimeout(context.Background(), abandonTimeout)
	defer cancel()
	_ = s.client.DeleteMessage(ctx, draftID)
}
