use super::*;
use crate::contacts::parse_vcard_fields;

fn read(csv: &str) -> CsvContacts {
    csv_contacts(csv.as_bytes()).expect("contacts")
}

fn person(card: &str) -> crate::contacts::ContactDetails {
    parse_vcard_fields(card).expect("a contact")
}

#[test]
fn reads_a_google_export() {
    let csv = "First Name,Middle Name,Last Name,Name Prefix,Nickname,Organization Name,\
Organization Title,Organization Department,Birthday,Notes,Labels,E-mail 1 - Label,\
E-mail 1 - Value,E-mail 2 - Label,E-mail 2 - Value,Phone 1 - Label,Phone 1 - Value,\
Address 1 - Label,Address 1 - Formatted,Address 1 - Street,Address 1 - City,\
Address 1 - Postal Code,Address 1 - Country,Website 1 - Label,Website 1 - Value\r\n\
Ann,B.,Smith,Dr,Annie,Acme,Engineer,R&D,1980-05-04,\"two\nlines\",* myContacts ::: Friends ::: Book club,\
* Home,ann@home.org ::: ann@old.org,Work,ann@acme.com,Mobile,+44 7700 900000,\
Home,\"1 High St, Leeds\",1 High St,Leeds,LS1 1AA,UK,,https://ann.example\r\n";
    let found = read(csv);
    assert_eq!(found.cards.len(), 1);
    assert_eq!(found.skipped, 0);
    let card = &found.cards[0];
    let c = person(card);
    assert_eq!(c.name, "Dr Ann B. Smith");
    assert!(card.contains("\r\nN:Smith;Ann;B.;Dr;\r\n"), "{card}");
    assert_eq!(c.nickname, "Annie");
    assert!(card.contains("\r\nORG:Acme;R&D\r\n"), "{card}");
    assert_eq!(c.title, "Engineer");
    assert_eq!(c.birthday_raw, "1980-05-04");
    assert_eq!(c.note, "two\nlines");
    let emails: Vec<_> = c.emails.iter().map(|e| (&*e.label, &*e.value)).collect();
    assert_eq!(
        emails,
        [
            ("Home", "ann@home.org"),
            ("Home", "ann@old.org"),
            ("Work", "ann@acme.com")
        ]
    );
    assert_eq!(c.phones[0].label, "Mobile");
    assert_eq!(c.phones[0].value, "+44 7700 900000");
    assert_eq!(c.postal.len(), 1);
    assert_eq!(c.postal[0].street, "1 High St");
    assert_eq!(c.postal[0].locality, "Leeds");
    assert_eq!(c.postal[0].label, "Home");
    assert_eq!(c.urls, ["https://ann.example"]);
    assert!(card.contains("CATEGORIES:Friends,Book club"), "{card}");
    let groups: Vec<_> = found.groups.iter().map(|(g, m)| (&**g, m.len())).collect();
    assert_eq!(groups, [("Friends", 1), ("Book club", 1)]);
    assert_eq!(found.groups[0].1[0].email, "ann@home.org");
}

#[test]
fn reads_an_outlook_export() {
    let csv = "Title,First Name,Last Name,Company,Job Title,Business Street,Business Street 2,\
Business City,Business Postal Code,Home Phone,Mobile Phone,Business Fax,Assistant's Phone,\
E-mail Address,E-mail Type,E-mail 2 Address,Birthday,Categories,Web Page\r\n\
Mr.,Bob,Jones,Initech,Manager,1 Main St,Suite 4,Austin,78701,555-0100,555-0101,555-0102,\
555-0199,bob@initech.com,SMTP,bob@initech.com,5/4/1975,Work;Golf,initech.com\r\n";
    let found = read(csv);
    let card = &found.cards[0];
    let c = person(card);
    assert_eq!(c.name, "Mr. Bob Jones");
    assert_eq!(c.title, "Manager");
    assert_eq!(c.org, "Initech");
    let phones: Vec<_> = c.phones.iter().map(|p| (&*p.label, &*p.value)).collect();
    assert_eq!(
        phones,
        [
            ("Home", "555-0100"),
            ("Mobile", "555-0101"),
            ("Fax", "555-0102")
        ]
    );
    // The same address twice is one, and the E-mail Type column is not one.
    assert_eq!(c.emails.len(), 1);
    assert_eq!(c.postal[0].street, "1 Main St\nSuite 4");
    assert_eq!(c.postal[0].label, "Work");
    assert_eq!(c.birthday_raw, "1975-05-04");
    assert_eq!(found.groups.len(), 2);
}

#[test]
fn reads_a_thunderbird_export_and_a_plain_sheet() {
    let csv = "First Name,Last Name,Display Name,Primary Email,Secondary Email,Work Phone,\
Home Address,Home City,Home ZipCode,Birth Year,Birth Month,Birth Day\r\n\
Cy,Lee,Cy Lee,cy@x.org,cy@y.org,0100,2 Low Rd,York,YO1,,7,9\r\n";
    let c = person(&read(csv).cards[0]);
    assert_eq!(c.name, "Cy Lee");
    assert_eq!(c.emails.len(), 2);
    assert_eq!(c.phones[0].label, "Work");
    assert_eq!(c.postal[0].locality, "York");
    assert_eq!(c.birthday_raw, "--07-09");

    let found = read("Name;E-mail;Phone;Groups\nDee;dee@x.org;0200;Team, Choir\n");
    let c = person(&found.cards[0]);
    assert_eq!((&*c.name, c.emails.len(), c.phones.len()), ("Dee", 1, 1));
    assert_eq!(found.groups.len(), 2);
}

#[test]
fn an_unnamed_column_of_addresses_is_read_as_email() {
    let found = read("Nom,Courriel\nAnn,ann@x.org\nBob,bob@x.org\n");
    let c = person(&found.cards[0]);
    assert_eq!(c.primary_email(), Some("ann@x.org"));
    // Only the address names the person; "Nom" is not read.
    assert_eq!(c.name, "ann@x.org");
}

#[test]
fn rows_naming_nobody_are_skipped_and_files_naming_nothing_refused() {
    let found = read("Name,Email,Notes\nAnn,,\n,,a note\n");
    assert_eq!((found.cards.len(), found.skipped), (1, 1));
    assert!(csv_contacts(b"Colour,Size\nred,big\n").is_err());
    // A person with only a phone is a contact; no group without an address.
    let found = read("Phone,Labels\n0300,Friends\n");
    assert_eq!(found.cards.len(), 1);
    assert!(found.groups.is_empty());
}

#[test]
fn reads_birthdays() {
    assert_eq!(birthday("1980-05-04").as_deref(), Some("1980-05-04"));
    assert_eq!(birthday("--05-04").as_deref(), Some("--05-04"));
    assert_eq!(birthday("5/4/1980").as_deref(), Some("1980-05-04"));
    assert_eq!(birthday("25/4/1980").as_deref(), Some("1980-04-25"));
    assert_eq!(birthday("4.5.1980").as_deref(), Some("1980-05-04"));
    assert_eq!(birthday("1980/5/4").as_deref(), Some("1980-05-04"));
    assert_eq!(birthday("5/4/80").as_deref(), Some("--05-04"));
    assert_eq!(
        birthday("1980-05-04 00:00:00").as_deref(),
        Some("1980-05-04")
    );
    assert_eq!(birthday("0/0/00"), None);
    assert_eq!(birthday(""), None);
}

#[test]
fn reads_headers() {
    assert_eq!(norm("E-mail 2 - Value"), "email 2 value");
    assert_eq!(norm("Home Country/Region"), "home country region");
    assert_eq!(classify("title", false), Some(Col::Single(Single::Title)));
    assert_eq!(classify("title", true), Some(Col::Single(Single::Prefix)));
    assert_eq!(classify("assistant s phone", false), None);
    assert_eq!(classify("email type", false), None);
}
