package main

import (
	"context"
	"encoding/base64"
	"strings"

	"github.com/ProtonMail/gluon/rfc822"
	proton "github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

type messageParams struct {
	envelopeFields
	sessionRequest
	ID string `json:"id"`
}

func messageOp(ctx context.Context, raw []byte) envelope {
	var params messageParams
	if !decodeStrict(raw, &params) {
		return failure("invalid_request")
	}
	if !validID(params.ID) {
		return failure("invalid_id")
	}
	if !params.Session.usable() {
		return failure("session_invalid")
	}
	session := resume(*params.Session)
	defer session.close()
	return session.message(ctx, params.ID)
}

// message fetches one message, unlocks the receiving address's key with the
// stored key passphrase, and returns the decrypted RFC 5322 message —
// attachments downloaded, decrypted and embedded, the way Bridge serves it
// over IMAP. The attachment rows beside it are metadata only: their bytes
// are already in the MIME, and shipping them twice would double the reply.
func (s *apiSession) message(ctx context.Context, id string) envelope {
	msg, err := s.client.GetMessage(ctx, id)
	if err != nil {
		return s.failErr(err)
	}
	keys, code := s.keyrings(ctx)
	if code != "" {
		return s.fail(code)
	}
	kr, ok := keys.addrKRs[msg.AddressID]
	if !ok {
		return s.fail("decryption_failed")
	}
	if _, err := msg.Decrypt(kr); err != nil {
		return s.fail("decryption_failed")
	}
	data := map[string][]byte{}
	// A PGP/MIME message carries its attachments inside the encrypted body;
	// only Proton-native messages store them as separate objects.
	if msg.MIMEType != rfc822.MultipartMixed {
		for _, att := range msg.Attachments {
			if !validID(att.ID) {
				return s.fail("attachment_failed")
			}
			blob, err := s.client.GetAttachment(ctx, att.ID)
			if err != nil {
				if code := s.classify(err); code != "request_failed" {
					return s.fail(code)
				}
				return s.fail("attachment_failed")
			}
			if !decryptsAttachment(kr, att, blob) {
				return s.fail("attachment_failed")
			}
			data[att.ID] = blob
		}
	}
	built, err := proton.BuildRFC822(kr, msg, data)
	if err != nil {
		return s.fail("decryption_failed")
	}
	rows := make([]map[string]any, len(msg.Attachments))
	for i, att := range msg.Attachments {
		var cid any
		if value := headerValue(att.Headers, "content-id"); value != "" {
			cid = strings.Trim(value, " <>")
		}
		rows[i] = map[string]any{
			"id":          att.ID,
			"name":        att.Name,
			"mime_type":   string(att.MIMEType),
			"cid":         cid,
			"size":        att.Size,
			"disposition": string(att.Disposition),
		}
	}
	return s.ok(map[string]any{
		"header":      headerRow(msg.MessageMetadata),
		"mime":        base64.StdEncoding.EncodeToString(built),
		"attachments": rows,
	})
}

// decryptsAttachment checks an attachment opens before the message is
// built, so a broken attachment reports attachment_failed rather than
// failing the whole body as undecryptable.
func decryptsAttachment(kr *crypto.KeyRing, att proton.Attachment, data []byte) bool {
	keyPackets, err := base64.StdEncoding.DecodeString(att.KeyPackets)
	if err != nil {
		return false
	}
	_, err = kr.Decrypt(crypto.NewPGPSplitMessage(keyPackets, data).GetPGPMessage(), nil, crypto.GetUnixTime())
	return err == nil
}

func headerValue(headers proton.Headers, name string) string {
	for key, values := range headers.Values {
		if strings.EqualFold(key, name) && len(values) > 0 {
			return values[0]
		}
	}
	return ""
}
