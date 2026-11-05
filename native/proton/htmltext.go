package main

import (
	"strings"

	"golang.org/x/net/html"
)

// blockTags end a line in the plain-text rendering.
var blockTags = map[string]bool{
	"p": true, "div": true, "br": true, "li": true, "tr": true, "table": true,
	"h1": true, "h2": true, "h3": true, "h4": true, "h5": true, "h6": true,
	"blockquote": true, "pre": true, "hr": true, "ul": true, "ol": true,
}

// htmlToText renders an HTML body as plain text for recipients whose
// preferences ask for text/plain (PGP/Inline, or a contact set to plain).
// Text content and line structure survive; markup, scripts and styles do
// not.
func htmlToText(body string) string {
	tokenizer := html.NewTokenizer(strings.NewReader(body))
	var out strings.Builder
	skip := 0
	lineBreak := func() {
		text := out.String()
		if text != "" && !strings.HasSuffix(text, "\n") {
			out.WriteString("\n")
		}
	}
	for {
		switch tokenizer.Next() {
		case html.ErrorToken:
			return strings.TrimSpace(collapseBlankLines(out.String())) + "\n"
		case html.StartTagToken, html.SelfClosingTagToken:
			name, _ := tokenizer.TagName()
			tag := string(name)
			if tag == "script" || tag == "style" || tag == "head" {
				skip++
			}
			if tag == "br" {
				out.WriteString("\n")
			} else if blockTags[tag] {
				lineBreak()
			}
		case html.EndTagToken:
			name, _ := tokenizer.TagName()
			tag := string(name)
			if (tag == "script" || tag == "style" || tag == "head") && skip > 0 {
				skip--
			}
			if blockTags[tag] {
				lineBreak()
			}
		case html.TextToken:
			if skip > 0 {
				continue
			}
			text := string(tokenizer.Text())
			words := strings.Fields(text)
			if len(words) == 0 {
				if text != "" {
					out.WriteString(" ")
				}
				continue
			}
			if strings.TrimLeft(text, " \t\r\n") != text {
				out.WriteString(" ")
			}
			out.WriteString(strings.Join(words, " "))
			if strings.TrimRight(text, " \t\r\n") != text {
				out.WriteString(" ")
			}
		}
	}
}

func collapseBlankLines(text string) string {
	lines := strings.Split(text, "\n")
	kept := lines[:0]
	blank := 0
	for _, line := range lines {
		line = strings.TrimSpace(line)
		if line == "" {
			blank++
			if blank > 1 {
				continue
			}
		} else {
			blank = 0
		}
		kept = append(kept, line)
	}
	return strings.Join(kept, "\n")
}
