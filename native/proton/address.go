package main

import (
	"net/mail"
	"strings"
	"unicode"
)

// mailAddress is the {name, address} pair every reply and request uses, as
// in the Tuta helper.
type mailAddress struct {
	Name    string `json:"name"`
	Address string `json:"address"`
}

func toMail(list []*mail.Address) []*mailAddress {
	rows := make([]*mailAddress, 0, len(list))
	for _, address := range list {
		if address != nil {
			rows = append(rows, &mailAddress{Name: address.Name, Address: address.Address})
		}
	}
	return rows
}

func fromMail(list []mailAddress) []*mail.Address {
	rows := make([]*mail.Address, len(list))
	for i, address := range list {
		rows[i] = &mail.Address{Name: address.Name, Address: address.Address}
	}
	return rows
}

// validAddress is the Tuta helper's deliberately loose check: exactly one
// @, both sides non-empty, no whitespace or control characters. The server
// is authoritative; a strict check here could only wrongly refuse mail.
func validAddress(address string) bool {
	local, domain, ok := strings.Cut(address, "@")
	if !ok || local == "" || domain == "" || strings.Contains(domain, "@") {
		return false
	}
	return !strings.ContainsFunc(address, func(r rune) bool {
		return unicode.IsSpace(r) || unicode.IsControl(r)
	})
}
