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
fn the_columns_come_from_the_first_line_alone() {
    let dir = std::env::temp_dir().join(format!("hylki-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("people.csv");
    let mut csv = String::from("Name;;\"E-mail\"\n");
    // Past the 64 KiB read: a row cut in half there is no harm.
    for i in 0..5000 {
        csv.push_str(&format!("Person {i};x;p{i}@example.com\n"));
    }
    std::fs::write(&path, csv).unwrap();
    assert_eq!(csv_columns(&path, false), ["Name", "Column 2", "E-mail"]);
    assert_eq!(csv_columns(&path, true), ["E-mail"]);
    assert!(csv_columns(&dir.join("missing.csv"), false).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
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
    let out = t.letter_for(&letter, 0, ComposeFormat::Plain, false);
    assert_eq!(out.to, "ann@example.com");
    assert_eq!(out.subject, "Hello Ann");
    assert_eq!(out.text, "Dear Ann, {{Unknown}} stays.");
    assert_eq!(
        t.missing_fields(&letter),
        [Missing {
            name: "Unknown".into(),
            places: vec![Place::Text],
        }]
    );
}

#[test]
fn fields_match_without_their_punctuation() {
    let t = group_table(&[crate::contacts::Contact {
        name: "Ann Smith".into(),
        email: "ann@example.com".into(),
    }]);
    let letter = Letter {
        to: "{{E-mail}}".into(),
        text: "Hi {{first_name}} {{LastName}}, {{ - }}".into(),
        ..Default::default()
    };
    assert!(t.missing_fields(&letter).is_empty());
    let out = t.letter_for(&letter, 0, ComposeFormat::Plain, false);
    assert_eq!(out.to, "ann@example.com");
    assert_eq!(out.text, "Hi Ann Smith, {{ - }}");
}

#[test]
fn missing_fields_are_named_once_with_where_they_are() {
    let t = table("Name,Email\nAnn,ann@example.com\n");
    let letter = Letter {
        to: "{{Email}}".into(),
        subject: "{{Company}} news".into(),
        body: "<p>{{company}} and {{<b>Order</b>}}</p>".into(),
        text: "{{company}} and {{Order}}".into(),
        ..Default::default()
    };
    assert_eq!(
        t.missing_fields(&letter),
        [
            Missing {
                name: "Company".into(),
                places: vec![Place::Subject, Place::Text],
            },
            Missing {
                name: "Order".into(),
                places: vec![Place::Text],
            },
        ]
    );
    // Left empty on request: the rest of the text stays.
    let out = t.letter_for(&letter, 0, ComposeFormat::Rich, true);
    assert_eq!(out.subject, " news");
    assert_eq!(out.body, "<p> and </p>");
    assert_eq!(out.text, " and ");
    assert_eq!(out.to, "ann@example.com");
}

#[test]
fn a_rich_body_gets_escaped_values_and_its_markup_ignored() {
    let t = table("Company,Email\nSmith & <Sons>,a@example.com\n");
    let letter = Letter {
        body: "<p>Hi {{<b>Company</b>}}&nbsp;and {{Company&nbsp;}}</p>".into(),
        text: "Hi {{Company}}".into(),
        ..Default::default()
    };
    let out = t.letter_for(&letter, 0, ComposeFormat::Rich, false);
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
        t.letter_for(&md, 0, ComposeFormat::Markdown, false).body,
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
        t.letter_for(&letter, 0, ComposeFormat::Plain, false).text,
        "{{}} {1 1 }}{{ {{A"
    );
    assert!(t.missing_fields(&letter).is_empty());
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
    assert_eq!(group_columns(true), [group_columns(false)[3].clone()]);
}

#[test]
fn a_draft_gives_its_fields_back() {
    let both = "{{First Name}} <{{Email}}>";
    let saved = format!(
        "\"{both}\" <{}>, Ann <ann@example.com>, \"{{{{Boss}}}}\" <{}>",
        draft_field_address(both),
        draft_field_address("{{Boss}}"),
    );
    assert_eq!(
        fields_from_draft(&saved),
        "{{First Name}} <{{Email}}>, Ann <ann@example.com>, {{Boss}}"
    );
    // A listing that kept only the addresses still has the fields.
    let bare = format!("{}, ann@example.com", draft_field_address("{{Email}}"));
    assert_eq!(fields_from_draft(&bare), "{{Email}}, ann@example.com");
    // Too long to spell out: the display name alone carries it.
    let long = "{{The Address We Have On File For Them}}";
    assert_eq!(draft_field_address(long), "merge@hylki.invalid");
    let saved = format!("\"{long}\" <merge@hylki.invalid>");
    assert_eq!(fields_from_draft(&saved), long);
    assert!(draft_field_address(both).len() <= 64 + "@hylki.invalid".len());
    assert_eq!(fields_from_draft("ann@example.com"), "ann@example.com");
}

#[test]
fn every_column_of_addresses_is_offered_to_to() {
    let t = table("Name,Work,Home\nAnn,ann@work.example,ann@home.example\nBob,bob@work.example,\n");
    assert_eq!(t.address_columns(), [1, 2]);
    assert!(is_field("{{Work}}"));
    assert!(is_field("{{First Name}} <{{Email}}>"));
    assert!(!is_field("ann@example.com"));
}
