use super::*;

fn utc(t: NaiveDateTime) -> i64 {
    t.and_utc().timestamp()
}

/// `19970902T090000`, or `19970902` for 9:00 that day.
fn at(text: &str) -> NaiveDateTime {
    let date = NaiveDate::parse_from_str(&text[..8], "%Y%m%d").unwrap();
    if text.len() > 8 {
        NaiveDateTime::parse_from_str(text, "%Y%m%dT%H%M%S").unwrap()
    } else {
        date.and_hms_opt(9, 0, 0).unwrap()
    }
}

fn list(texts: &[&str]) -> Vec<NaiveDateTime> {
    texts.iter().map(|t| at(t)).collect()
}

/// The first `n` starts of a series.
fn first(rule: &str, start: &str, n: usize) -> Vec<NaiveDateTime> {
    let rule = Rule::parse(rule).unwrap();
    expand(&rule, at(start), at(start), NaiveDateTime::MAX, &utc, n)
}

fn whole(rule: &str, start: &str) -> Vec<NaiveDateTime> {
    let rule = Rule::parse(rule).unwrap();
    all(&rule, at(start), &utc, 10_000).unwrap()
}

#[test]
fn daily_for_ten() {
    assert_eq!(
        whole("FREQ=DAILY;COUNT=10", "19970902"),
        list(&[
            "19970902", "19970903", "19970904", "19970905", "19970906", "19970907",
            "19970908", "19970909", "19970910", "19970911"
        ])
    );
}

#[test]
fn daily_until_christmas_eve() {
    let got = whole("FREQ=DAILY;UNTIL=19971224T000000Z", "19970902");
    assert_eq!(got.len(), 113);
    assert_eq!(got.last(), Some(&at("19971223")));
}

#[test]
fn every_other_day() {
    assert_eq!(
        first("FREQ=DAILY;INTERVAL=2", "19970902", 4),
        list(&["19970902", "19970904", "19970906", "19970908"])
    );
}

#[test]
fn every_ten_days_five_times() {
    assert_eq!(
        whole("FREQ=DAILY;INTERVAL=10;COUNT=5", "19970902"),
        list(&["19970902", "19970912", "19970922", "19971002", "19971012"])
    );
}

#[test]
fn every_day_in_january_for_three_years_both_ways() {
    let yearly = whole(
        "FREQ=YEARLY;UNTIL=20000131T140000Z;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
        "19980101",
    );
    let daily = whole("FREQ=DAILY;UNTIL=20000131T140000Z;BYMONTH=1", "19980101");
    assert_eq!(yearly.len(), 93);
    assert_eq!(yearly, daily);
    assert_eq!(yearly[31], at("19990101"));
    assert_eq!(yearly.last(), Some(&at("20000131")));
}

#[test]
fn weekly_for_ten() {
    assert_eq!(
        whole("FREQ=WEEKLY;COUNT=10", "19970902"),
        list(&[
            "19970902", "19970909", "19970916", "19970923", "19970930", "19971007",
            "19971014", "19971021", "19971028", "19971104"
        ])
    );
}

#[test]
fn every_other_week_monday_wednesday_friday() {
    assert_eq!(
        whole(
            "FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224T000000Z;WKST=SU;BYDAY=MO,WE,FR",
            "19970901"
        ),
        list(&[
            "19970901", "19970903", "19970905", "19970915", "19970917", "19970919",
            "19970929", "19971001", "19971003", "19971013", "19971015", "19971017",
            "19971027", "19971029", "19971031", "19971110", "19971112", "19971114",
            "19971124", "19971126", "19971128", "19971208", "19971210", "19971212",
            "19971222"
        ])
    );
}

#[test]
fn every_other_week_tuesday_thursday_eight_times() {
    assert_eq!(
        whole("FREQ=WEEKLY;INTERVAL=2;COUNT=8;WKST=SU;BYDAY=TU,TH", "19970902"),
        list(&[
            "19970902", "19970904", "19970916", "19970918", "19970930", "19971002",
            "19971014", "19971016"
        ])
    );
}

#[test]
fn monthly_first_friday_for_ten() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=10;BYDAY=1FR", "19970905"),
        list(&[
            "19970905", "19971003", "19971107", "19971205", "19980102", "19980206",
            "19980306", "19980403", "19980501", "19980605"
        ])
    );
}

#[test]
fn every_other_month_first_and_last_sunday() {
    assert_eq!(
        whole("FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU", "19970907"),
        list(&[
            "19970907", "19970928", "19971102", "19971130", "19980104", "19980125",
            "19980301", "19980329", "19980503", "19980531"
        ])
    );
}

#[test]
fn monthly_second_to_last_monday() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=6;BYDAY=-2MO", "19970922"),
        list(&[
            "19970922", "19971020", "19971117", "19971222", "19980119", "19980216"
        ])
    );
}

#[test]
fn monthly_third_to_last_day() {
    assert_eq!(
        first("FREQ=MONTHLY;BYMONTHDAY=-3", "19970928", 6),
        list(&[
            "19970928", "19971029", "19971128", "19971229", "19980129", "19980226"
        ])
    );
}

#[test]
fn monthly_second_and_fifteenth() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=10;BYMONTHDAY=2,15", "19970902"),
        list(&[
            "19970902", "19970915", "19971002", "19971015", "19971102", "19971115",
            "19971202", "19971215", "19980102", "19980115"
        ])
    );
}

#[test]
fn monthly_first_and_last_day() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1", "19970930"),
        list(&[
            "19970930", "19971001", "19971031", "19971101", "19971130", "19971201",
            "19971231", "19980101", "19980131", "19980201"
        ])
    );
}

#[test]
fn every_eighteen_months_tenth_to_fifteenth() {
    assert_eq!(
        whole(
            "FREQ=MONTHLY;INTERVAL=18;COUNT=10;BYMONTHDAY=10,11,12,13,14,15",
            "19970910"
        ),
        list(&[
            "19970910", "19970911", "19970912", "19970913", "19970914", "19970915",
            "19990310", "19990311", "19990312", "19990313"
        ])
    );
}

#[test]
fn every_tuesday_every_other_month() {
    assert_eq!(
        first("FREQ=MONTHLY;INTERVAL=2;BYDAY=TU", "19970902", 14),
        list(&[
            "19970902", "19970909", "19970916", "19970923", "19970930", "19971104",
            "19971111", "19971118", "19971125", "19980106", "19980113", "19980120",
            "19980127", "19980303"
        ])
    );
}

#[test]
fn yearly_in_june_and_july() {
    assert_eq!(
        whole("FREQ=YEARLY;COUNT=10;BYMONTH=6,7", "19970610"),
        list(&[
            "19970610", "19970710", "19980610", "19980710", "19990610", "19990710",
            "20000610", "20000710", "20010610", "20010710"
        ])
    );
}

#[test]
fn every_other_year_january_to_march() {
    assert_eq!(
        whole("FREQ=YEARLY;INTERVAL=2;COUNT=10;BYMONTH=1,2,3", "19970310"),
        list(&[
            "19970310", "19990110", "19990210", "19990310", "20010110", "20010210",
            "20010310", "20030110", "20030210", "20030310"
        ])
    );
}

#[test]
fn every_third_year_on_days_one_hundred_and_two_hundred() {
    assert_eq!(
        whole("FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200", "19970101"),
        list(&[
            "19970101", "19970410", "19970719", "20000101", "20000409", "20000718",
            "20030101", "20030410", "20030719", "20060101"
        ])
    );
}

#[test]
fn every_twentieth_monday() {
    assert_eq!(
        first("FREQ=YEARLY;BYDAY=20MO", "19970519", 3),
        list(&["19970519", "19980518", "19990517"])
    );
}

#[test]
fn monday_of_week_twenty() {
    assert_eq!(
        first("FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", "19970512", 3),
        list(&["19970512", "19980511", "19990517"])
    );
}

#[test]
fn every_thursday_in_march() {
    assert_eq!(
        first("FREQ=YEARLY;BYMONTH=3;BYDAY=TH", "19970313", 11),
        list(&[
            "19970313", "19970320", "19970327", "19980305", "19980312", "19980319",
            "19980326", "19990304", "19990311", "19990318", "19990325"
        ])
    );
}

#[test]
fn every_thursday_in_summer() {
    let got = first("FREQ=YEARLY;BYDAY=TH;BYMONTH=6,7,8", "19970605", 14);
    assert_eq!(
        got,
        list(&[
            "19970605", "19970612", "19970619", "19970626", "19970703", "19970710",
            "19970717", "19970724", "19970731", "19970807", "19970814", "19970821",
            "19970828", "19980604"
        ])
    );
}

#[test]
fn every_friday_the_thirteenth() {
    // DTSTART is not a Friday 13th but still counts; the RFC example
    // takes it out with an EXDATE, which is the caller's to apply.
    assert_eq!(
        first("FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", "19970902", 6),
        list(&[
            "19970902", "19980213", "19980313", "19981113", "19990813", "20001013"
        ])
    );
}

#[test]
fn first_saturday_after_first_sunday() {
    assert_eq!(
        first("FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13", "19970913", 10),
        list(&[
            "19970913", "19971011", "19971108", "19971213", "19980110", "19980207",
            "19980307", "19980411", "19980509", "19980613"
        ])
    );
}

#[test]
fn us_presidential_election_day() {
    assert_eq!(
        first(
            "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8",
            "19961105",
            3
        ),
        list(&["19961105", "20001107", "20041102"])
    );
}

#[test]
fn third_tuesday_wednesday_or_thursday() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3", "19970904"),
        list(&["19970904", "19971007", "19971106"])
    );
}

#[test]
fn second_to_last_weekday_of_the_month() {
    assert_eq!(
        first("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2", "19970929", 7),
        list(&[
            "19970929", "19971030", "19971127", "19971230", "19980129", "19980226",
            "19980330"
        ])
    );
}

#[test]
fn every_three_hours_through_a_day() {
    assert_eq!(
        whole("FREQ=HOURLY;INTERVAL=3;UNTIL=19970902T170000Z", "19970902"),
        list(&["19970902T090000", "19970902T120000", "19970902T150000"])
    );
}

#[test]
fn every_fifteen_minutes_six_times() {
    assert_eq!(
        whole("FREQ=MINUTELY;INTERVAL=15;COUNT=6", "19970902"),
        list(&[
            "19970902T090000",
            "19970902T091500",
            "19970902T093000",
            "19970902T094500",
            "19970902T100000",
            "19970902T101500"
        ])
    );
}

#[test]
fn every_twenty_minutes_in_working_hours() {
    let got = first(
        "FREQ=DAILY;BYHOUR=9,10,11,12,13,14,15,16;BYMINUTE=0,20,40",
        "19970902",
        25,
    );
    assert_eq!(got[1], at("19970902T092000"));
    assert_eq!(got[23], at("19970902T164000"));
    assert_eq!(got[24], at("19970903T090000"));
    // The same with MINUTELY.
    let minutely = first(
        "FREQ=MINUTELY;INTERVAL=20;BYHOUR=9,10,11,12,13,14,15,16",
        "19970902",
        25,
    );
    assert_eq!(got, minutely);
}

#[test]
fn the_week_start_changes_which_days_fall_in_a_period() {
    assert_eq!(
        whole("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO", "19970805"),
        list(&["19970805", "19970810", "19970819", "19970824"])
    );
    assert_eq!(
        whole("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU", "19970805"),
        list(&["19970805", "19970817", "19970819", "19970831"])
    );
}

#[test]
fn a_month_without_the_day_is_skipped() {
    assert_eq!(
        whole("FREQ=MONTHLY;COUNT=5", "20070131"),
        list(&["20070131", "20070331", "20070531", "20070731", "20070831"])
    );
    assert_eq!(
        whole("FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5", "20070115"),
        list(&["20070115", "20070130", "20070215", "20070315", "20070330"])
    );
}

#[test]
fn the_twenty_ninth_of_february_comes_in_leap_years() {
    assert_eq!(
        whole("FREQ=YEARLY;COUNT=3", "20240229"),
        list(&["20240229", "20280229", "20320229"])
    );
}

#[test]
fn a_window_decades_on_is_quick_and_right() {
    let rule = Rule::parse("FREQ=DAILY").unwrap();
    let started = std::time::Instant::now();
    let got = expand(
        &rule,
        at("19960101"),
        at("20261102T000000"),
        at("20261109T000000"),
        &utc,
        100,
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(50));
    assert_eq!(got.len(), 7);
    assert_eq!(got[0], at("20261102"));

    let rule = Rule::parse("FREQ=WEEKLY;INTERVAL=3;BYDAY=MO,TH").unwrap();
    let far = expand(
        &rule,
        at("19960101"),
        at("20261101T000000"),
        at("20261201T000000"),
        &utc,
        100,
    );
    // Counted the slow way, from the start.
    let slow: Vec<NaiveDateTime> = expand(
        &rule,
        at("19960101"),
        at("19960101"),
        at("20261201T000000"),
        &utc,
        100_000,
    )
    .into_iter()
    .filter(|t| *t >= at("20261101T000000"))
    .collect();
    assert!(!far.is_empty());
    assert_eq!(far, slow);

    let rule = Rule::parse("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH").unwrap();
    assert_eq!(
        expand(
            &rule,
            at("19801127"),
            at("20260101T000000"),
            at("20270101T000000"),
            &utc,
            10
        ),
        list(&["20261126"])
    );
}

#[test]
fn a_count_still_counts_from_the_first_start() {
    let rule = Rule::parse("FREQ=DAILY;COUNT=10").unwrap();
    let got = expand(
        &rule,
        at("19970902"),
        at("19970909T000000"),
        at("19971001T000000"),
        &utc,
        100,
    );
    assert_eq!(got, list(&["19970909", "19970910", "19970911"]));
}

#[test]
fn until_in_each_form() {
    let date = whole("FREQ=DAILY;UNTIL=19970905", "19970902");
    assert_eq!(date.last(), Some(&at("19970905")));
    let floating = whole("FREQ=DAILY;UNTIL=19970905T085959", "19970902");
    assert_eq!(floating.last(), Some(&at("19970904")));
    // Nine o'clock in a zone four hours behind UTC is 13:00Z.
    let rule = Rule::parse("FREQ=DAILY;UNTIL=19970905T130000Z").unwrap();
    let east = |t: NaiveDateTime| utc(t) + 4 * 3600;
    let zoned = all(&rule, at("19970902"), &east, 100).unwrap();
    assert_eq!(zoned.last(), Some(&at("19970905")));
    let rule = Rule::parse("FREQ=DAILY;UNTIL=19970905T125959Z").unwrap();
    let zoned = all(&rule, at("19970902"), &east, 100).unwrap();
    assert_eq!(zoned.last(), Some(&at("19970904")));
}

#[test]
fn an_endless_rule_has_no_whole_list() {
    let rule = Rule::parse("FREQ=WEEKLY").unwrap();
    assert!(all(&rule, at("19970902"), &utc, 10).is_none());
}

#[test]
fn a_rule_that_never_matches_stops() {
    let rule = Rule::parse("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30;COUNT=3").unwrap();
    let started = std::time::Instant::now();
    let got = all(&rule, at("20260101"), &utc, 10).unwrap();
    assert_eq!(got, list(&["20260101"]));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn the_window_and_limit_bound_the_answer() {
    let rule = Rule::parse("FREQ=DAILY").unwrap();
    let got = expand(
        &rule,
        at("20260101"),
        at("20260101"),
        at("20260105"),
        &utc,
        100,
    );
    assert_eq!(got.len(), 4);
    let got = expand(
        &rule,
        at("20260101"),
        at("20260101"),
        at("20270101"),
        &utc,
        3,
    );
    assert_eq!(got.len(), 3);
    assert!(expand(&rule, at("20260101"), at("20260101"), at("20270101"), &utc, 0).is_empty());
}

#[test]
fn rules_read_and_write_back() {
    for text in [
        "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE",
        "FREQ=MONTHLY;COUNT=10;BYDAY=1SU,-1SU",
        "FREQ=YEARLY;UNTIL=20261231T235959Z;BYDAY=TU;BYMONTHDAY=2,3,4;BYMONTH=11",
        "FREQ=DAILY;UNTIL=20261231",
        "FREQ=DAILY;UNTIL=20261231T090000",
        "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2",
        "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
        "FREQ=YEARLY;BYYEARDAY=1,-1;BYWEEKNO=20",
        "FREQ=HOURLY;BYSECOND=0;BYMINUTE=0,30;BYHOUR=9,17",
    ] {
        assert_eq!(Rule::parse(text).unwrap().to_ical(), text);
    }
    let rule = Rule::parse("rrule:freq=weekly;byday=+2mo;x-name=a;interval=0;wkst=mo").unwrap();
    assert_eq!(rule.to_ical(), "FREQ=WEEKLY;BYDAY=2MO");
    assert_eq!(
        Rule::parse("BYDAY=MO;INTERVAL=2;FREQ=WEEKLY").unwrap().to_ical(),
        "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO"
    );
    assert!(Rule::parse("INTERVAL=2").is_none());
    assert!(Rule::parse("FREQ=FORTNIGHTLY").is_none());
    assert_eq!(
        Rule::parse("FREQ=DAILY;UNTIL=20261231T235959Z").unwrap().until,
        Some(Until::Utc(at("20261231T235959").and_utc().timestamp()))
    );
}

#[test]
fn week_numbers_follow_the_week_start() {
    // 1 January 2026 is a Thursday: week 1 starts on Monday 29 December.
    let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    assert_eq!(week_number(d(2025, 12, 29), Weekday::Mon), (1, 53));
    assert_eq!(week_number(d(2026, 1, 5), Weekday::Mon), (2, 53));
    assert_eq!(week_number(d(2027, 1, 1), Weekday::Mon), (53, 53));
    assert_eq!(week_number(d(2027, 1, 4), Weekday::Mon), (1, 52));
}
