//! The wire mapping, at its edges.

use ironflowx_core::{Address, ErrorKind, Scope, Timestamp};
use ironflowx_proto::wire::{self, ErrorInfo, Undecodable, WhoamiResponse};
use ironflowx_proto::{OpError, Whoami};

const ID: &str = "0190b5f0-0000-7000-8000-000000000001";

fn whoami(expires_at: Option<i64>) -> Whoami {
    Whoami {
        key_id: ID.parse().unwrap(),
        scopes: [Scope::Write].into(),
        environment: Address::new("default", "main"),
        expires_at: expires_at.map(Timestamp::from_millis),
    }
}

/// D76: 0001-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z, in milliseconds since the epoch.
const FIRST: i64 = -62_135_596_800_000;
const LAST: i64 = 253_402_300_799_999;

#[test]
fn a_time_outside_the_wires_years_is_sent_as_the_nearer_bound() {
    for (stored, seconds, nanos, read) in [
        (0, 0, 0, 0),
        (
            1_800_000_000_123,
            1_800_000_000,
            123_000_000,
            1_800_000_000_123,
        ),
        (-1, -1, 999_000_000, -1),
        (FIRST, -62_135_596_800, 0, FIRST),
        (FIRST - 1, -62_135_596_800, 0, FIRST),
        (i64::MIN, -62_135_596_800, 0, FIRST),
        (LAST, 253_402_300_799, 999_000_000, LAST),
        (LAST + 1, 253_402_300_799, 999_000_000, LAST),
        (i64::MAX, 253_402_300_799, 999_000_000, LAST),
    ] {
        let sent = WhoamiResponse::from(whoami(Some(stored)));
        let time = sent.expires_at.as_option().unwrap();
        assert_eq!((time.seconds, time.nanos), (seconds, nanos), "{stored}");
        assert_eq!(Whoami::try_from(sent), Ok(whoami(Some(read))), "{stored}");
    }
    let sent = WhoamiResponse::from(whoami(None));
    assert!(sent.expires_at.as_option().is_none());
    assert_eq!(Whoami::try_from(sent), Ok(whoami(None)));
}

#[test]
fn an_answer_that_is_not_what_the_proto_says_does_not_decode() {
    let good = || WhoamiResponse::from(whoami(Some(0)));
    let time = |seconds, nanos| {
        let mut a = good();
        let t = a.expires_at.get_or_insert_default();
        (t.seconds, t.nanos) = (seconds, nanos);
        a
    };
    for (why, answer) in [
        (
            "no key ID",
            WhoamiResponse {
                key_id: String::new(),
                ..good()
            },
        ),
        (
            "a key ID in another spelling",
            WhoamiResponse {
                key_id: ID.to_uppercase(),
                ..good()
            },
        ),
        (
            "a scope no key holds",
            WhoamiResponse {
                scopes: vec!["read".into(), "own".into()],
                ..good()
            },
        ),
        (
            "an address with no slash",
            WhoamiResponse {
                environment: "main".into(),
                ..good()
            },
        ),
        ("a fraction finer than a millisecond", time(0, 1)),
        ("a fraction finer than a millisecond", time(0, 999_999)),
        ("a negative fraction", time(0, -1)),
        ("a negative fraction", time(0, -1_000_000)),
        ("a second of fraction", time(0, 1_000_000_000)),
        ("a time before year 1", time(FIRST / 1_000 - 1, 999_000_000)),
        ("a time after year 9999", time(LAST / 1_000 + 1, 0)),
        ("a time no i64 holds", time(i64::MAX, 0)),
        ("a time no i64 holds", time(i64::MIN, 0)),
    ] {
        assert_eq!(Whoami::try_from(answer), Err(Undecodable), "{why}");
    }
    let edge = time(LAST / 1_000, 999_000_000);
    assert_eq!(Whoami::try_from(edge), Ok(whoami(Some(LAST))));
}

#[test]
fn an_error_info_names_a_kind_by_its_text() {
    for (kind, text) in [
        (ErrorKind::Invalid, "invalid"),
        (ErrorKind::Unauthenticated, "unauthenticated"),
        (ErrorKind::PermissionDenied, "permission_denied"),
        (ErrorKind::NotFound, "not_found"),
        (ErrorKind::Unknown, "unknown"),
        (ErrorKind::ResourceExhausted, "resource_exhausted"),
        (ErrorKind::Unavailable, "unavailable"),
        (ErrorKind::Internal, "internal"),
    ] {
        let e = OpError {
            kind,
            message: "m".into(),
            hint: Some("h".into()),
        };
        let info = ErrorInfo::from(&e);
        assert_eq!((&*info.kind, &*info.message), (text, "m"));
        assert_eq!(info.hint.as_deref(), Some("h"));
        assert_eq!(OpError::try_from(info), Ok(e));
    }
    for unknown in ["", "Invalid", "invalid_argument"] {
        let info = ErrorInfo {
            kind: unknown.into(),
            ..ErrorInfo::default()
        };
        assert_eq!(OpError::try_from(info), Err(Undecodable), "{unknown}");
    }
}

/// The error of an answer is its first `ErrorInfo`, whatever else the answer carries.
#[test]
fn an_answer_without_an_error_info_holds_no_error() {
    let e = OpError {
        kind: ErrorKind::NotFound,
        message: "m".into(),
        hint: None,
    };
    let sent = wire::connect_error(&e);
    assert_eq!(wire::op_error(&sent), Some(e.clone()));
    let mut padded = sent.clone();
    for detail in &mut padded.details {
        let value = detail.value.as_mut().unwrap();
        while !value.len().is_multiple_of(4) {
            value.push('=');
        }
    }
    assert_eq!(wire::op_error(&padded), Some(e));
    let mut other = sent.clone();
    other.details[0].type_url = "google.rpc.ErrorInfo".into();
    assert_eq!(wire::op_error(&other), None);
    let mut empty = sent.clone();
    empty.details[0].value = None;
    assert_eq!(wire::op_error(&empty), None);
    for value in ["!", "Cg"] {
        let mut broken = sent.clone();
        broken.details[0].value = Some(value.into());
        assert_eq!(wire::op_error(&broken), None, "{value}");
    }
}
