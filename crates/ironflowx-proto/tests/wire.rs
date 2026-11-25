//! The wire mapping, at its edges.

use std::num::NonZeroU32;

use buffa::{HasMessageView, Message as _, OwnedView};
use ironflowx_core::{Address, ErrorKind, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_proto::wire::{self, ErrorInfo, Undecodable, WhoamiResponse};
use ironflowx_proto::{
    CreateKeyRequest, CreatedKey, Environment, KeyInfo, KeyPage, KeyStatus,
    ListEnvironmentsRequest, ListKeysRequest, OpError, RevokeKeyRequest, Whoami, WhoamiRequest,
};

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

const KEY: &str =
    "ixk_0190b5f0-0000-7000-8000-000000000001_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn info() -> KeyInfo {
    KeyInfo {
        key_id: ID.parse().unwrap(),
        label: None,
        scopes: [Scope::Read].into(),
        origin: KeyOrigin::Created,
        created_by: Some("0190b5f0-0000-7000-8000-000000000002".parse().unwrap()),
        created_at: Timestamp::from_millis(0),
        expires_at: None,
        status: KeyStatus::Active,
    }
}

fn created() -> CreatedKey {
    CreatedKey {
        key: Secret::new(KEY.to_owned()),
        info: info(),
    }
}

fn environment() -> Environment {
    Environment {
        env: format!("{ID}.00112233445566778899aabbccddeeff")
            .parse()
            .unwrap(),
        name: "main".into(),
        address: Address::new("default", "main"),
        project_id: ID.parse().unwrap(),
        created_at: Timestamp::from_millis(0),
    }
}

/// LED-9, TEN-8, D76.
#[test]
fn an_environment_is_its_reference_and_its_address_prints_back() {
    for (address, read) in [
        (("a", "b/c"), ("a", "b/c")),
        (("a/b", "c"), ("a", "b/c")),
        (("", ""), ("", "")),
    ] {
        for (stored, shown) in [(0, 0), (i64::MAX, LAST), (i64::MIN, FIRST)] {
            let at = |t| Timestamp::from_millis(t);
            let sent = Environment {
                address: Address::new(address.0, address.1),
                created_at: at(stored),
                ..environment()
            };
            let answer = wire::ListEnvironmentsResponse::from(vec![sent.clone(), environment()]);
            assert_eq!(
                answer.environments[0].env,
                "0190b5f0-0000-7000-8000-000000000001.00112233445566778899aabbccddeeff"
            );
            let text = format!("{}/{}", address.0, address.1);
            assert_eq!(answer.environments[0].address, text);
            let got = Vec::<Environment>::try_from(answer).unwrap();
            assert_eq!(got[0].address.to_string(), text);
            let expected = Environment {
                address: Address::new(read.0, read.1),
                created_at: at(shown),
                ..sent
            };
            assert_eq!(got, [expected, environment()]);
        }
    }
}

#[test]
fn an_environment_that_is_not_what_the_proto_says_does_not_decode() {
    let decode = |change: fn(&mut wire::Environment)| {
        let mut answer = wire::ListEnvironmentsResponse::from(vec![environment()]);
        change(&mut answer.environments[0]);
        Vec::<Environment>::try_from(answer)
    };
    assert_eq!(decode(|_| {}), Ok(vec![environment()]));
    let changes: [fn(&mut wire::Environment); 6] = [
        // A bare ID (LED-9).
        |e| e.env = ID.into(),
        |e| e.env.clear(),
        |e| e.address = "main".into(),
        |e| e.project_id = ID.to_uppercase(),
        |e| e.created_at = None.into(),
        |e| e.created_at.get_or_insert_default().nanos = 1,
    ];
    for (i, change) in changes.into_iter().enumerate() {
        assert_eq!(decode(change), Err(Undecodable), "{i}");
    }
}

#[test]
fn a_keys_information_crosses_whole() {
    for (origin, text) in [
        (KeyOrigin::Minted, "minted"),
        (KeyOrigin::Seed, "seed"),
        (KeyOrigin::Recovered, "recovered"),
        (KeyOrigin::Created, "created"),
    ] {
        let sent = KeyInfo { origin, ..info() };
        let wire = wire::KeyInfo::from(sent.clone());
        assert_eq!(wire.origin, text);
        assert_eq!(KeyInfo::try_from(wire), Ok(sent));
    }
    for (status, text) in [
        (KeyStatus::Active, "active"),
        (KeyStatus::Expired, "expired"),
        (KeyStatus::Revoked, "revoked"),
    ] {
        let sent = KeyInfo { status, ..info() };
        let wire = wire::KeyInfo::from(sent.clone());
        assert_eq!(wire.status, text);
        assert_eq!(KeyInfo::try_from(wire), Ok(sent));
    }
    // D76, in each of its times.
    for (stored, shown) in [(i64::MIN, FIRST), (i64::MAX, LAST), (1, 1)] {
        let at = |t| Timestamp::from_millis(t);
        let sent = KeyInfo {
            label: Some(String::new()),
            created_at: at(stored),
            expires_at: Some(at(stored)),
            ..info()
        };
        let expected = KeyInfo {
            created_at: at(shown),
            expires_at: Some(at(shown)),
            ..sent.clone()
        };
        let page = KeyPage {
            keys: vec![sent, info()],
            next: Some(String::new()),
        };
        let got = KeyPage::try_from(wire::ListKeysResponse::from(page)).unwrap();
        assert_eq!(got.keys, [expected, info()]);
        assert_eq!(got.next.as_deref(), Some(""));
    }
}

#[test]
fn a_key_that_is_not_what_the_proto_says_does_not_decode() {
    let changes: [fn(&mut wire::KeyInfo); 9] = [
        |k| k.key_id = ID.to_uppercase(),
        |k| k.scopes.push("own".into()),
        |k| k.origin = "Minted".into(),
        |k| k.origin.clear(),
        |k| k.status = "unknown".into(),
        |k| k.created_by = Some(String::new()),
        |k| k.created_at = None.into(),
        |k| k.created_at.get_or_insert_default().nanos = 1,
        |k| k.expires_at.get_or_insert_default().nanos = -1,
    ];
    for (i, change) in changes.into_iter().enumerate() {
        let page = KeyPage {
            keys: vec![info()],
            next: None,
        };
        let mut page = wire::ListKeysResponse::from(page);
        change(&mut page.keys[0]);
        assert_eq!(KeyPage::try_from(page), Err(Undecodable), "{i}");
        let mut answer = wire::CreateKeyResponse::from(created());
        change(answer.info.get_or_insert_default());
        assert_eq!(CreatedKey::try_from(answer).err(), Some(Undecodable), "{i}");
    }
    let changes: [fn(&mut wire::CreateKeyResponse); 8] = [
        |a| a.info = None.into(),
        // No key, or one no engine accepts (D46).
        |a| a.key.clear(),
        |a| a.key = "x".into(),
        |a| a.key.push('A'),
        |a| a.key.truncate(83),
        |a| a.key.replace_range(0..4, "IXK_"),
        |a| a.key.replace_range(41..42, "+"),
        // One of another ID.
        |a| a.key.replace_range(39..40, "2"),
    ];
    for (i, change) in changes.into_iter().enumerate() {
        let mut answer = wire::CreateKeyResponse::from(created());
        change(&mut answer);
        assert_eq!(CreatedKey::try_from(answer).err(), Some(Undecodable), "{i}");
    }
}

/// TEN-3: the message that holds a key, its views included, prints none.
#[test]
fn a_create_key_answer_prints_no_key() {
    let answer = wire::CreateKeyResponse::from(created());
    let bytes = answer.encode_to_vec();
    assert!(bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
    let view = wire::CreateKeyResponse::decode_view(&bytes).unwrap();
    assert_eq!(view.key, KEY);
    let owned =
        OwnedView::<<wire::CreateKeyResponse as HasMessageView>::View<'static>>::from_owned(
            &answer,
        )
        .unwrap();
    for shown in [
        format!("{answer:?}"),
        format!("{answer:#?}"),
        format!("{view:?}"),
        format!("{owned:?}"),
    ] {
        // The secret, the part after the ID, which the rest prints.
        assert!(!shown.contains(&KEY[41..]), "{shown}");
        assert!(!shown.contains("ixk_"), "{shown}");
        assert!(shown.contains(ID), "{shown}");
    }
    let read = CreatedKey::try_from(answer).unwrap();
    assert_eq!(read.key.expose(), KEY);
    assert_eq!(read.info, info());
}

#[test]
fn a_request_crosses_whole_or_is_invalid() {
    for sent in [
        CreateKeyRequest::default(),
        CreateKeyRequest {
            scopes: [Scope::Read, Scope::Admin].into(),
            expires_at: Some(Timestamp::from_millis(FIRST)),
            label: Some(String::new()),
        },
        CreateKeyRequest {
            expires_at: Some(Timestamp::from_millis(LAST)),
            ..Default::default()
        },
    ] {
        let wire = wire::CreateKeyRequest::try_from(sent.clone()).unwrap();
        assert_eq!(CreateKeyRequest::try_from(wire), Ok(sent));
    }
    // A bound in place of the time asked for would be another expiry.
    for outside in [FIRST - 1, LAST + 1, i64::MIN, i64::MAX] {
        let sent = CreateKeyRequest {
            expires_at: Some(Timestamp::from_millis(outside)),
            ..Default::default()
        };
        let refused = OpError {
            kind: ErrorKind::Invalid,
            message: "a time is in whole milliseconds and in years 0001 to 9999".into(),
            hint: None,
        };
        assert_eq!(wire::CreateKeyRequest::try_from(sent), Err(refused));
    }
    for (limit, cursor) in [
        (None, None),
        (NonZeroU32::new(1), Some("")),
        (Some(NonZeroU32::MAX), Some("x")),
    ] {
        let cursor = cursor.map(str::to_owned);
        let sent = ListKeysRequest { limit, cursor };
        let wire = wire::ListKeysRequest::try_from(sent.clone()).unwrap();
        assert_eq!(ListKeysRequest::try_from(wire), Ok(sent));
    }
    for key_id in ["", ID, "no ID"] {
        let key_id = key_id.to_owned();
        let sent = RevokeKeyRequest { key_id };
        let wire = wire::RevokeKeyRequest::try_from(sent.clone()).unwrap();
        assert_eq!(RevokeKeyRequest::try_from(wire), Ok(sent));
    }
    let wire = wire::WhoamiRequest::try_from(WhoamiRequest {}).unwrap();
    assert_eq!(WhoamiRequest::try_from(wire), Ok(WhoamiRequest {}));
    let sent = ListEnvironmentsRequest {};
    let wire = wire::ListEnvironmentsRequest::try_from(sent.clone()).unwrap();
    assert_eq!(ListEnvironmentsRequest::try_from(wire), Ok(sent));
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
