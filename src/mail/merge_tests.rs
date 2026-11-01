use super::*;

fn table(csv: &str) -> Table {
    parse_csv(csv.as_bytes()).expect("a table")
}

#[test]
fn reads_quoted_fields_and_line_breaks() {
    let t = table(
        "Name,Email,Note\r\n\"Smith, Ann\",ann@example.com,\"two\nlines, \"\"quoted\"\"\"\r\n",
    );
    assert_eq!(t.columns, ["Name", "Email", "Note"]);
    assert_eq!(
        t.rows,
        [["Smith, Ann", "ann@example.com", "two\nlines, \"quoted\""]]
    );
}

#[test]
fn finds_the_delimiter() {
    let t = table("Vorname;E-Mail\nAnn;ann@example.com\n");
    assert_eq!(t.columns, ["Vorname", "E-Mail"]);
    let t = table("Name\tEmail\nAnn\tann@example.com");
    assert_eq!(t.rows, [["Ann", "ann@example.com"]]);
    // A comma inside quotes in the header is not the delimiter.
    let t = table("\"Last, First\";Email\nSmith, Ann;ann@example.com\n");
    assert_eq!(t.columns, ["Last, First", "Email"]);
}

#[test]
fn pads_short_rows_names_blank_columns_and_skips_blank_lines() {
    let t = table("Email,,Town\n\nann@example.com\n,,\nbob@example.com,x,Leeds,extra\n");
    assert_eq!(t.columns, ["Email", "Column 2", "Town"]);
    assert_eq!(
        t.rows,
        [
            vec!["ann@example.com", "", ""],
            vec!["bob@example.com", "x", "Leeds"],
        ]
    );
}

#[test]
fn decodes_byte_order_marks_and_windows_1252() {
    let mut utf8 = vec![0xEF, 0xBB, 0xBF];
    utf8.extend_from_slice("Name,Email\nJosé,j@example.com\n".as_bytes());
    assert_eq!(parse_csv(&utf8).unwrap().columns[0], "Name");
    let mut utf16 = vec![0xFF, 0xFE];
    for u in "Name\tEmail\nZoë\tz@example.com\n".encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(parse_csv(&utf16).unwrap().rows[0][0], "Zoë");
    let cp1252 = b"Name,Email\nJos\xe9,j@example.com\n";
    assert_eq!(parse_csv(cp1252).unwrap().rows[0][0], "José");
}

#[test]
fn an_empty_file_or_a_header_alone_is_refused() {
    assert!(parse_csv(b"").is_err());
    assert!(parse_csv(b"Name,Email\n").is_err());
}

#[test]
fn the_address_column_is_found_by_its_values() {
    let t = table("Name,Contact\nAnn,ann@example.com\nBob,bob@example.com\nCy,\n");
    assert_eq!(t.address_column(), Some(1));
    let t = table("Name,Town\nAnn,Leeds\n");
    assert_eq!(t.address_column(), None);
}

#[test]
fn fills_fields_in_any_case_and_spacing() {
    let t = table("First Name,Email\nAnn,ann@example.com\n");
    let letter = Letter {
        to: "{{ email }}".into(),
        subject: "Hello {{first  name}}".into(),
        text: "Dear {{First Name}}, {{Unknown}} stays.".into(),
        ..Default::default()
    };
    let out = t.letter_for(&letter, 0, ComposeFormat::Plain);
    assert_eq!(out.to, "ann@example.com");
    assert_eq!(out.subject, "Hello Ann");
    assert_eq!(out.text, "Dear Ann, {{Unknown}} stays.");
    assert_eq!(t.unknown_fields(&letter), ["Unknown"]);
}

#[test]
fn a_rich_body_gets_escaped_values_and_its_markup_ignored() {
    let t = table("Company,Email\nSmith & <Sons>,a@example.com\n");
    let letter = Letter {
        body: "<p>Hi {{<b>Company</b>}}&nbsp;and {{Company&nbsp;}}</p>".into(),
        text: "Hi {{Company}}".into(),
        ..Default::default()
    };
    let out = t.letter_for(&letter, 0, ComposeFormat::Rich);
    assert_eq!(
        out.body,
        "<p>Hi Smith &amp; &lt;Sons&gt;&nbsp;and Smith &amp; &lt;Sons&gt;</p>"
    );
    assert_eq!(out.text, "Hi Smith & <Sons>");
    // Markdown is source text: the value goes in as written.
    let md = Letter {
        body: "Hi {{Company}}".into(),
        ..Default::default()
    };
    assert_eq!(
        t.letter_for(&md, 0, ComposeFormat::Markdown).body,
        "Hi Smith & <Sons>"
    );
}

#[test]
fn braces_that_name_nothing_are_left_alone() {
    let t = table("A,Email\n1,a@example.com\n");
    let letter = Letter {
        text: "{{}} {{{A}} {{ A }} }}{{ {{A".into(),
        ..Default::default()
    };
    assert_eq!(
        t.letter_for(&letter, 0, ComposeFormat::Plain).text,
        "{{}} {1 1 }}{{ {{A"
    );
    assert!(t.unknown_fields(&letter).is_empty());
}

#[test]
fn a_message_needs_an_address_in_to() {
    let ok = Letter {
        to: "Ann <ann@example.com>".into(),
        ..Default::default()
    };
    assert!(ok.sendable());
    let empty = Letter::default();
    assert!(!empty.sendable());
    let bad = Letter {
        to: "ann@example.com".into(),
        cc: "not an address".into(),
        ..Default::default()
    };
    assert!(!bad.sendable());
    assert!(Letter {
        to: "{{Email}}".into(),
        ..Default::default()
    }
    .to_has_field());
}

#[test]
fn a_group_splits_names_and_skips_members_without_an_address() {
    use crate::contacts::Contact;
    let t = group_table(&[
        Contact {
            name: "Ann van Dyke".into(),
            email: "ann@example.com".into(),
        },
        Contact {
            name: "bob@example.com".into(),
            email: "bob@example.com".into(),
        },
        Contact {
            name: "Cy".into(),
            email: String::new(),
        },
    ]);
    assert_eq!(
        t.rows,
        [
            vec!["Ann van Dyke", "Ann", "van Dyke", "ann@example.com"],
            vec!["", "", "", "bob@example.com"],
        ]
    );
    assert_eq!(t.address_column(), Some(3));
}
