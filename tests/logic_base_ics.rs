//! Byte-equality of `contest_ics` against output of the Python original (fixture generated with
//! `.venv/bin/python -I`: src.logic.contest_ics for each case, stored in tests/fixtures/logic_base_ics.json).

use lightningccp::cf::models::Contest;
use lightningccp::logic::base::contest_ics;
use serde_json::Value;

#[test]
fn contest_ics_is_byte_equal_to_python() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/logic_base_ics.json")).unwrap();
    assert!(cases.len() >= 3);
    for c in cases {
        let int = |k: &str| c[k].as_i64().unwrap();
        let contest = Contest {
            id: int("id"),
            name: c["name"].as_str().unwrap().to_string(),
            kind: "CF".into(),
            phase: "BEFORE".into(),
            start_time: Some(int("start")),
            duration: int("duration"),
        };
        let got = contest_ics(&contest, int("now"), int("alarm")).unwrap();
        assert_eq!(got.as_bytes(), c["ics"].as_str().unwrap().as_bytes(), "contest {}", contest.id);
    }
}

/// Python: contest_ics with start_time None raises TypeError; with a timestamp outside years 1..9999,
/// datetime.fromtimestamp raises ValueError/OverflowError. Both reach report_error's generic reply.
#[test]
fn contest_ics_errors_where_python_raises() {
    let mk = |start| Contest { id: 1, name: "x".into(), kind: "CF".into(), phase: "BEFORE".into(), start_time: start, duration: 60 };
    let err = contest_ics(&mk(None), 0, 10).unwrap_err();
    assert!(err.downcast_ref::<lightningccp::errors::BotError>().is_none());
    assert_eq!(lightningccp::errors::error_message(&err), "Something went wrong. Try again later.");
    assert!(contest_ics(&mk(Some(253_402_300_739)), 0, 10).is_ok());
    assert!(contest_ics(&mk(Some(253_402_300_740)), 0, 10).is_err()); // DTEND past year 9999
    assert!(contest_ics(&mk(Some(253_402_300_800)), 0, 10).is_err());
    assert!(contest_ics(&mk(Some(i64::MAX)), 0, 10).is_err());
    assert!(contest_ics(&mk(Some(0)), -62_135_596_801, 10).is_err());
    assert!(contest_ics(&mk(Some(0)), -62_135_596_800, 10).is_ok());
}
