//! The search field's language: words, quoted phrases, `field:value`
//! filters, `before:`/`after:` dates, `AND`, `OR`, `NOT`/`-` and
//! parentheses. Parsed once per edit into a [`Query`] that the message list
//! evaluates locally and the workers translate for a search on the server
//! (IMAP `SEARCH`, Microsoft Graph `$search`).
//!
//! Words next to each other must all match, in any order and any field, so
//! `invoice march` finds a message whose subject says "invoice" and whose
//! body says "March". `OR` binds looser than that implicit `AND`.
//! Anything that is not a recognised `field:` stays an ordinary word, so a
//! search for `re:` or a URL still works as typed.

use chrono::NaiveDate;

/// Which part of a message a word is looked for in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// Any of the others.
    Any,
    /// The sender's name and address.
    From,
    /// The To recipients.
    To,
    /// The Cc recipients.
    Cc,
    /// The Reply-To addresses.
    ReplyTo,
    Subject,
    /// The message text, as far as it is stored locally.
    Body,
    /// Attachment file names and the text extracted from them.
    Attachment,
}

impl Field {
    fn from_key(key: &str) -> Option<Field> {
        Some(match key.to_ascii_lowercase().as_str() {
            "from" => Field::From,
            "to" => Field::To,
            "cc" => Field::Cc,
            "replyto" | "reply-to" => Field::ReplyTo,
            "subject" => Field::Subject,
            "body" => Field::Body,
            "attachment" | "filename" => Field::Attachment,
            _ => return None,
        })
    }
}

/// One node of a parsed search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Text to find in `field`. Lowercased; a quoted phrase keeps its spaces.
    Term {
        field: Field,
        text: String,
    },
    /// Dated before this day (exclusive), local time.
    Before(NaiveDate),
    /// Dated on or after this day, local time.
    After(NaiveDate),
    /// Carries at least one attachment.
    HasAttachment,
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

/// A parsed search. `root` is `None` for an empty field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub root: Option<Node>,
}

/// Why a search could not be read, for the line under the search field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnclosedQuote,
    UnclosedParen,
    UnexpectedParen,
    /// `before:`/`after:` with something that is not a date.
    BadDate(String),
    /// `from:` and the like with nothing after the colon.
    EmptyValue(String),
    /// `AND`, `OR` or `NOT` with nothing to combine.
    DanglingOperator(String),
}

impl ParseError {
    pub fn text(&self) -> String {
        use crate::i18n::i18n;
        match self {
            ParseError::UnclosedQuote => i18n("A quotation mark is not closed"),
            ParseError::UnclosedParen => i18n("A parenthesis is not closed"),
            ParseError::UnexpectedParen => i18n("A closing parenthesis has no opening one"),
            ParseError::BadDate(key) => i18n("“{key}” needs a date, such as 2026-10-14").replace("{key}", key),
            ParseError::EmptyValue(key) => i18n("“{key}” needs something to search for").replace("{key}", key),
            ParseError::DanglingOperator(op) => i18n("“{op}” needs a search term on both sides").replace("{op}", op),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word or quoted phrase, with the `key:` in front of it if any.
    Word {
        key: Option<String>,
        text: String,
        quoted: bool,
        negated: bool,
    },
    And,
    Or,
    Not,
    Open {
        negated: bool,
    },
    Close,
}

fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Token::Close);
            i += 1;
            continue;
        }
        let negated = c == '-' && chars.get(i + 1).is_some_and(|n| !n.is_whitespace());
        if negated {
            i += 1;
        }
        if chars[i] == '(' {
            out.push(Token::Open { negated });
            i += 1;
            continue;
        }
        // A bare word, which may turn out to be `key:` followed by a value.
        let (mut word, quoted) = if chars[i] == '"' {
            let (s, next) = quoted_at(&chars, i)?;
            i = next;
            (s, true)
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' {
                if chars[i] == ':' {
                    break;
                }
                i += 1;
            }
            (chars[start..i].iter().collect::<String>(), false)
        };
        let mut key = None;
        let mut quoted_value = quoted;
        if !quoted && chars.get(i) == Some(&':') && is_known_key(&word) {
            key = Some(word.to_ascii_lowercase());
            i += 1;
            if chars.get(i) == Some(&'"') {
                let (s, next) = quoted_at(&chars, i)?;
                i = next;
                word = s;
                quoted_value = true;
            } else {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != ')' {
                    i += 1;
                }
                word = chars[start..i].iter().collect();
            }
        } else if !quoted {
            // An unknown `key:` (or a URL) stays part of the word.
            while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' {
                word.push(chars[i]);
                i += 1;
            }
        }
        if key.is_none() && !quoted_value && !negated {
            match word.as_str() {
                "AND" => {
                    out.push(Token::And);
                    continue;
                }
                "OR" => {
                    out.push(Token::Or);
                    continue;
                }
                "NOT" => {
                    out.push(Token::Not);
                    continue;
                }
                _ => {}
            }
        }
        out.push(Token::Word { key, text: word, quoted: quoted_value, negated });
    }
    Ok(out)
}

fn is_known_key(word: &str) -> bool {
    Field::from_key(word).is_some() || matches!(word.to_ascii_lowercase().as_str(), "before" | "after" | "has")
}

/// The phrase that starts with the quote at `start`, and the index past its
/// closing quote. `\"` inside it is a literal quote.
fn quoted_at(chars: &[char], start: usize) -> Result<(String, usize), ParseError> {
    let mut s = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' if chars.get(i + 1) == Some(&'"') => {
                s.push('"');
                i += 2;
            }
            '"' => return Ok((s, i + 1)),
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(ParseError::UnclosedQuote)
}

/// Read a date the way people type one: ISO `2026-10-14`, `2026/10/14`, or
/// the day-first `14.10.2026`.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%Y/%m/%d"))
        .or_else(|_| NaiveDate::parse_from_str(s, "%d.%m.%Y"))
        .ok()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// or := and (OR and)*
    fn or(&mut self) -> Result<Option<Node>, ParseError> {
        let first = self.and()?;
        if self.peek() != Some(&Token::Or) {
            return Ok(first);
        }
        let Some(first) = first else { return Err(ParseError::DanglingOperator("OR".into())) };
        let mut parts = vec![first];
        while let Some(Token::Or) = self.peek() {
            self.pos += 1;
            match self.and()? {
                Some(right) => parts.push(right),
                None => return Err(ParseError::DanglingOperator("OR".into())),
            }
        }
        Ok(Some(Node::Or(parts)))
    }

    /// and := unary ((AND)? unary)*
    fn and(&mut self) -> Result<Option<Node>, ParseError> {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None | Some(Token::Or) | Some(Token::Close) => break,
                Some(Token::And) => {
                    self.pos += 1;
                    if parts.is_empty() {
                        return Err(ParseError::DanglingOperator("AND".into()));
                    }
                    match self.peek() {
                        None | Some(Token::Or) | Some(Token::Close) | Some(Token::And) => {
                            return Err(ParseError::DanglingOperator("AND".into()))
                        }
                        _ => {}
                    }
                }
                _ => {
                    if let Some(n) = self.unary()? {
                        parts.push(n);
                    }
                }
            }
        }
        Ok(match parts.len() {
            0 => None,
            1 => parts.pop(),
            _ => Some(Node::And(parts)),
        })
    }

    fn unary(&mut self) -> Result<Option<Node>, ParseError> {
        let Some(tok) = self.peek().cloned() else { return Ok(None) };
        self.pos += 1;
        match tok {
            Token::Not => match self.peek() {
                None | Some(Token::Or) | Some(Token::And) | Some(Token::Close) => {
                    Err(ParseError::DanglingOperator("NOT".into()))
                }
                _ => Ok(self.unary()?.map(|n| Node::Not(Box::new(n)))),
            },
            Token::Open { negated } => {
                let inner = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err(ParseError::UnclosedParen);
                }
                self.pos += 1;
                Ok(inner.map(|n| if negated { Node::Not(Box::new(n)) } else { n }))
            }
            Token::Close => Err(ParseError::UnexpectedParen),
            Token::Word { key, text, quoted, negated } => {
                let node = word_node(key.as_deref(), &text, quoted)?;
                Ok(node.map(|n| if negated { Node::Not(Box::new(n)) } else { n }))
            }
            Token::And | Token::Or => unreachable!("handled by the callers"),
        }
    }
}

fn word_node(key: Option<&str>, text: &str, quoted: bool) -> Result<Option<Node>, ParseError> {
    let value = text.trim();
    match key {
        None => {
            if value.is_empty() {
                // `""` asks for nothing.
                return Ok(None);
            }
            Ok(Some(Node::Term { field: Field::Any, text: value.to_lowercase() }))
        }
        Some(k @ ("before" | "after")) => {
            let date = parse_date(value).ok_or_else(|| ParseError::BadDate(format!("{k}:")))?;
            Ok(Some(if k == "before" { Node::Before(date) } else { Node::After(date) }))
        }
        Some("has") => match value.to_ascii_lowercase().as_str() {
            "attachment" | "attachments" => Ok(Some(Node::HasAttachment)),
            // `has:` and anything else it is not known for is a word.
            _ => Ok(Some(Node::Term { field: Field::Any, text: format!("has:{value}").to_lowercase() })),
        },
        Some(k) => {
            if value.is_empty() && !quoted {
                return Err(ParseError::EmptyValue(format!("{k}:")));
            }
            let field = Field::from_key(k).expect("only known keys reach here");
            Ok(Some(Node::Term { field, text: value.to_lowercase() }))
        }
    }
}

impl Query {
    pub fn parse(input: &str) -> Result<Query, ParseError> {
        let tokens = tokenize(input)?;
        let mut p = Parser { tokens, pos: 0 };
        let root = p.or()?;
        match p.peek() {
            None => Ok(Query { root }),
            Some(Token::Close) => Err(ParseError::UnexpectedParen),
            Some(_) => Err(ParseError::DanglingOperator("OR".into())),
        }
    }

    /// Every word the query looks for, with its field, once each: what the
    /// local full-text index is asked about.
    pub fn terms(&self) -> Vec<(Field, String)> {
        fn walk(n: &Node, out: &mut Vec<(Field, String)>) {
            match n {
                Node::Term { field, text } => {
                    if !out.iter().any(|(f, t)| f == field && t == text) {
                        out.push((*field, text.clone()));
                    }
                }
                Node::And(v) | Node::Or(v) => v.iter().for_each(|c| walk(c, out)),
                Node::Not(c) => walk(c, out),
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            walk(r, &mut out);
        }
        out
    }

    /// Whether a message matches. `fields` reads one field of the message
    /// (lowercased), and `content` says whether the index found a term in
    /// the message's stored text or attachments.
    pub fn matches(
        &self,
        timestamp: i64,
        has_attachment: bool,
        fields: &dyn Fn(Field) -> String,
        content: &dyn Fn(Field, &str) -> bool,
    ) -> bool {
        fn eval(
            n: &Node,
            ts: i64,
            att: bool,
            fields: &dyn Fn(Field) -> String,
            content: &dyn Fn(Field, &str) -> bool,
        ) -> bool {
            match n {
                Node::Term { field, text } => {
                    let in_headers = match field {
                        Field::Any => [Field::Subject, Field::From, Field::To, Field::Cc, Field::ReplyTo, Field::Body]
                            .into_iter()
                            .any(|f| fields(f).contains(text.as_str())),
                        Field::Attachment => false,
                        f => fields(*f).contains(text.as_str()),
                    };
                    in_headers || content(*field, text)
                }
                Node::Before(d) => ts != 0 && ts < local_midnight(*d),
                Node::After(d) => ts != 0 && ts >= local_midnight(*d),
                Node::HasAttachment => att,
                Node::And(v) => v.iter().all(|c| eval(c, ts, att, fields, content)),
                Node::Or(v) => v.iter().any(|c| eval(c, ts, att, fields, content)),
                Node::Not(c) => !eval(c, ts, att, fields, content),
            }
        }
        match &self.root {
            None => true,
            Some(r) => eval(r, timestamp, has_attachment, fields, content),
        }
    }
}

/// One field of a message, lowercased, as the list holds it. The body is
/// its preview; the full text is the index's to search.
pub fn message_field(m: &crate::models::Message, f: Field) -> String {
    match f {
        Field::Subject => m.subject.to_lowercase(),
        Field::From => format!("{} {}", m.from_name, m.from_addr).to_lowercase(),
        Field::To => m.to.to_lowercase(),
        Field::Cc => m.cc.to_lowercase(),
        Field::ReplyTo => m.reply_to.to_lowercase(),
        Field::Body => m.preview.to_lowercase(),
        Field::Any | Field::Attachment => String::new(),
    }
}

/// Every message in the local cache that `query` matches, within `accounts`
/// and, when given, one folder: the rows, and what the full-text index found
/// for each term (the list uses those for messages it already holds). With
/// `rows` false only the index is asked.
pub fn search_cache(
    cache: &crate::cache::Cache,
    query: &Query,
    accounts: &[u32],
    folder: Option<&str>,
    rows: bool,
) -> (Vec<(u32, String, crate::models::Message)>, crate::cache::TermHits) {
    let hits = cache.search_hits(&query.terms(), accounts, folder);
    let mut out = Vec::new();
    if rows {
        for &account_id in accounts {
            let found = cache.search_scan(account_id, folder, |path, m| {
                let content = |f: Field, t: &str| {
                    hits.get(&(f, t.to_string())).is_some_and(|s| s.contains(&(account_id, path.to_string(), m.uid)))
                };
                query.matches(m.timestamp, m.has_attachment, &|f| message_field(m, f), &content)
            });
            out.extend(found.into_iter().map(|(path, m)| (account_id, path, m)));
        }
    }
    (out, hits)
}

/// The Unix time at which `day` starts here.
pub fn local_midnight(day: NaiveDate) -> i64 {
    use chrono::TimeZone;
    let naive = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        // A day that starts inside a DST gap starts at the first instant
        // that exists.
        .unwrap_or_else(|| chrono::Local.from_utc_datetime(&naive))
        .timestamp()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn term(field: Field, text: &str) -> Node {
        Node::Term { field, text: text.into() }
    }

    fn parse(s: &str) -> Node {
        Query::parse(s).unwrap().root.unwrap()
    }

    #[test]
    fn words_side_by_side_must_all_match() {
        assert_eq!(parse("Invoice March"), Node::And(vec![term(Field::Any, "invoice"), term(Field::Any, "march")]));
    }

    #[test]
    fn a_quoted_phrase_stays_one_term() {
        assert_eq!(parse("\"quarterly report\""), term(Field::Any, "quarterly report"));
        assert_eq!(parse("subject:\"Q3 plan\""), term(Field::Subject, "q3 plan"));
        assert_eq!(parse(r#""say \"hi\"""#), term(Field::Any, "say \"hi\""));
    }

    #[test]
    fn fields_are_recognised_in_any_case() {
        assert_eq!(parse("FROM:anna"), term(Field::From, "anna"));
        assert_eq!(parse("to:bob@example.org"), term(Field::To, "bob@example.org"));
        assert_eq!(parse("cc:carol"), term(Field::Cc, "carol"));
        assert_eq!(parse("reply-to:list"), term(Field::ReplyTo, "list"));
        assert_eq!(parse("attachment:budget.xlsx"), term(Field::Attachment, "budget.xlsx"));
    }

    #[test]
    fn an_unknown_key_is_an_ordinary_word() {
        assert_eq!(parse("re:meeting"), term(Field::Any, "re:meeting"));
        assert_eq!(parse("https://example.org/a"), term(Field::Any, "https://example.org/a"));
    }

    #[test]
    fn or_binds_looser_than_the_implicit_and() {
        assert_eq!(
            parse("a b OR c"),
            Node::Or(vec![Node::And(vec![term(Field::Any, "a"), term(Field::Any, "b")]), term(Field::Any, "c")])
        );
        assert_eq!(
            parse("a (b OR c)"),
            Node::And(vec![term(Field::Any, "a"), Node::Or(vec![term(Field::Any, "b"), term(Field::Any, "c")])])
        );
        assert_eq!(parse("a AND b"), Node::And(vec![term(Field::Any, "a"), term(Field::Any, "b")]));
        assert_eq!(
            parse("a OR b OR c"),
            Node::Or(vec![term(Field::Any, "a"), term(Field::Any, "b"), term(Field::Any, "c")])
        );
    }

    #[test]
    fn lowercase_operators_are_words() {
        assert_eq!(
            parse("rock or roll"),
            Node::And(vec![term(Field::Any, "rock"), term(Field::Any, "or"), term(Field::Any, "roll"),])
        );
    }

    #[test]
    fn negation() {
        assert_eq!(parse("-spam"), Node::Not(Box::new(term(Field::Any, "spam"))));
        assert_eq!(parse("NOT from:x"), Node::Not(Box::new(term(Field::From, "x"))));
        assert_eq!(
            parse("-(a OR b)"),
            Node::Not(Box::new(Node::Or(vec![term(Field::Any, "a"), term(Field::Any, "b")])))
        );
        // A lone dash is a word.
        assert_eq!(
            parse("a - b"),
            Node::And(vec![term(Field::Any, "a"), term(Field::Any, "-"), term(Field::Any, "b")])
        );
    }

    #[test]
    fn dates() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        assert_eq!(parse("before:2026-10-14"), Node::Before(d));
        assert_eq!(parse("after:2026/10/14"), Node::After(d));
        assert_eq!(parse("after:14.10.2026"), Node::After(d));
        assert_eq!(Query::parse("before:tomorrow"), Err(ParseError::BadDate("before:".into())));
        assert_eq!(Query::parse("after:2026-02-30"), Err(ParseError::BadDate("after:".into())));
    }

    #[test]
    fn invalid_input_says_what_is_wrong() {
        assert_eq!(Query::parse("\"open"), Err(ParseError::UnclosedQuote));
        assert_eq!(Query::parse("(a OR b"), Err(ParseError::UnclosedParen));
        assert_eq!(Query::parse("a)"), Err(ParseError::UnexpectedParen));
        assert_eq!(Query::parse("from:"), Err(ParseError::EmptyValue("from:".into())));
        assert_eq!(Query::parse("OR a"), Err(ParseError::DanglingOperator("OR".into())));
        assert_eq!(Query::parse("a OR"), Err(ParseError::DanglingOperator("OR".into())));
        assert_eq!(Query::parse("a AND"), Err(ParseError::DanglingOperator("AND".into())));
        assert_eq!(Query::parse("NOT"), Err(ParseError::DanglingOperator("NOT".into())));
    }

    #[test]
    fn empty_and_unicode() {
        assert!(Query::parse("   ").unwrap().root.is_none());
        assert!(Query::parse("\"\"").unwrap().root.is_none());
        assert_eq!(parse("Größe"), term(Field::Any, "größe"));
        assert_eq!(parse("subject:Ελληνικά"), term(Field::Subject, "ελληνικά"));
        assert_eq!(parse("東京"), term(Field::Any, "東京"));
    }

    #[test]
    fn terms_are_listed_once() {
        let q = Query::parse("a from:b a OR from:b c").unwrap();
        assert_eq!(q.terms(), vec![(Field::Any, "a".into()), (Field::From, "b".into()), (Field::Any, "c".into())]);
    }

    fn eval(q: &str, subject: &str, from: &str, to: &str, cc: &str, reply_to: &str, body_hits: &[&str]) -> bool {
        let q = Query::parse(q).unwrap();
        let fields = |f: Field| -> String {
            match f {
                Field::Subject => subject.to_lowercase(),
                Field::From => from.to_lowercase(),
                Field::To => to.to_lowercase(),
                Field::Cc => cc.to_lowercase(),
                Field::ReplyTo => reply_to.to_lowercase(),
                _ => String::new(),
            }
        };
        let content = |f: Field, t: &str| matches!(f, Field::Any | Field::Body) && body_hits.contains(&t);
        q.matches(1_000, false, &fields, &content)
    }

    #[test]
    fn recipients_match_independently_of_subject_and_preview() {
        assert!(eval("dana", "Hello", "a@x", "dana@example.org", "", "", &[]));
        assert!(eval("cc:carol", "Hello", "a@x", "", "Carol <carol@x>", "", &[]));
        assert!(eval("replyto:list", "Hello", "a@x", "", "", "list@x", &[]));
        assert!(!eval("to:carol", "Hello", "a@x", "", "carol@x", "", &[]));
    }

    #[test]
    fn words_combine_across_fields_and_content() {
        assert!(eval("invoice march", "Invoice", "a@x", "", "", "", &["march"]));
        assert!(!eval("invoice march", "Invoice", "a@x", "", "", "", &[]));
        assert!(eval("invoice OR march", "Hello", "a@x", "", "", "", &["march"]));
        assert!(!eval("-invoice", "Invoice", "a@x", "", "", "", &[]));
    }

    pub(crate) fn test_message(uid: u32, subject: &str, to: &str, cc: &str) -> crate::models::Message {
        crate::models::Message {
            id: uid,
            account_id: 1,
            folder_id: 0,
            uid,
            from_name: "Ann".into(),
            from_addr: "ann@example.org".into(),
            reply_to: String::new(),
            to: to.into(),
            cc: cc.into(),
            subject: subject.into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 1_000 + i64::from(uid),
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }

    /// The whole local index is searched, not just what the list holds:
    /// a recipient, a Cc and a word deep in a stored body each find their
    /// message, and words combine across them.
    #[test]
    fn the_cache_is_searched_with_the_index() {
        let c = crate::cache::Cache::in_memory().unwrap();
        let add = |folder: &str, uid: u32, subject: &str, to: &str, cc: &str, body: Option<&str>| {
            let m = test_message(uid, subject, to, cc);
            c.upsert_messages(1, folder, &[m]);
            if let Some(b) = body {
                c.save_body(1, folder, uid, b);
            }
        };
        let long = format!("<p>{} the zebra crossing</p>", "filler text ".repeat(50));
        add("INBOX", 1, "Weekly", "dana@example.org", "", None);
        add("Archive", 2, "Weekly", "", "carol@example.org", Some(&long));
        add("Archive", 3, "Other", "", "", None);
        while c.index_pending(100).unwrap() > 0 {}

        let find = |q: &str, folder: Option<&str>| -> Vec<u32> {
            let q = Query::parse(q).unwrap();
            let mut v: Vec<u32> =
                search_cache(&c, &q, &[1], folder, true).0.into_iter().map(|(_, _, m)| m.uid).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(find("dana", None), vec![1]);
        assert_eq!(find("cc:carol", None), vec![2]);
        assert_eq!(find("zebra", None), vec![2], "past the preview, in the stored body");
        assert_eq!(find("weekly zebra", None), vec![2]);
        assert_eq!(find("weekly OR other", None), vec![1, 2, 3]);
        assert_eq!(find("weekly", Some("INBOX")), vec![1]);
        assert_eq!(find("weekly -zebra", None), vec![1]);
        assert!(find("nothing", None).is_empty());
    }

    #[test]
    fn dates_bound_the_timestamp() {
        let q = Query::parse("after:2026-10-14 before:2026-10-15").unwrap();
        let start = local_midnight(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap());
        let end = local_midnight(NaiveDate::from_ymd_opt(2026, 10, 15).unwrap());
        let none = |_: Field| String::new();
        let no = |_: Field, _: &str| false;
        assert!(!q.matches(start - 1, false, &none, &no));
        assert!(q.matches(start, false, &none, &no));
        assert!(q.matches(end - 1, false, &none, &no));
        assert!(!q.matches(end, false, &none, &no));
        // An undated message is never inside a date range.
        assert!(!q.matches(0, false, &none, &no));
    }
}
