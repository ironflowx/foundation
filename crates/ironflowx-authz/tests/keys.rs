//! The key format (D46) and verification, through the crate's public functions.

use std::collections::BTreeSet;

use ironflowx_authz::{KeyRecord, mint, parse, verify};
use ironflowx_core::{EnvRef, KeyId, Scope, Secret, Timestamp};

const NOW: Timestamp = Timestamp::from_millis(1_800_000_000_000);

const ID: &str = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f";
const ENV: &str = "019a9f3e-7c2b-7d41-8a3e-000000000001.00112233445566778899aabbccddeeff";

fn id() -> KeyId {
    ID.parse().unwrap()
}

fn env() -> EnvRef {
    ENV.parse().unwrap()
}

/// 0xFB, 0xFF bytes encode to `-` and `_` in base64url: the secret may hold the separator.
const AWKWARD: [u8; 32] = [0xFB; 32];

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

#[test]
fn a_minted_key_is_84_characters_and_parses_back_to_its_id_and_digest() {
    for bytes in [[0; 32], [0xFF; 32], AWKWARD] {
        let (key, digest) = mint(id(), bytes);
        let text = key.expose();
        assert_eq!(text.len(), 84, "{text}");
        assert!(text.starts_with(&format!("ixk_{ID}_")), "{text}");
        let parts = parse(&key).expect("a minted key parses");
        assert_eq!(parts.id, id());
        assert!(parts.digest.ct_eq(&digest));
    }
    assert!(mint(id(), AWKWARD).0.expose()[41..].contains(['-', '_']));
}

#[test]
fn different_secrets_have_different_digests() {
    let (_, a) = mint(id(), [1; 32]);
    let (_, b) = mint(id(), [2; 32]);
    assert!(!a.ct_eq(&b));
}

#[test]
fn malformed_keys_are_refused() {
    let good = mint(id(), AWKWARD).0.expose().to_owned();
    let tail = &good[41..];
    let bad = [
        String::new(),
        "not-a-key".to_owned(),
        good[..83].to_owned(),
        format!("{good}A"),
        good.replacen("ixk_", "ixq_", 1),
        good.replacen("ixk_", "IXK_", 1),
        format!("ixk_{}_{tail}", ID.to_uppercase()),
        format!("ixk_3f2b8c1e-4d5a-4b6c-9d7e-8f9a0b1c2d3e_{tail}"), // a UUIDv4
        format!("ixk_{ID}-{tail}"),                                 // wrong separator
        format!("ixk_{ID}_{}=", &tail[..42]),                       // padding
        format!("ixk_{ID}_{}+", &tail[..42]),                       // standard base64
        format!("ixk_{ID}_{}B", &tail[..42]), // non-canonical: low bits of the last character set
        format!("ixk_{ID}_{}é", &tail[..41]), // 84 bytes, not 84 characters
        format!("ixk_{}é{tail}", &ID[..35]),  // 84 bytes, a character across byte 40
    ];
    for text in bad {
        assert!(parse(&secret(&text)).is_err(), "accepted {text:?}");
    }
}

#[test]
fn malformed_names_the_key_format_and_never_echoes_the_key() {
    let text = format!("ixk_{}_{}", ID.to_uppercase(), "A".repeat(43));
    let message = parse(&secret(&text)).expect_err("refused").to_string();
    assert!(message.contains("ixk_<key-id>_<secret>"), "{message}");
    assert!(
        !message.contains(&text) && !message.contains(&ID.to_uppercase()),
        "{message}"
    );
}

fn record(bytes: [u8; 32], env: EnvRef) -> KeyRecord {
    KeyRecord {
        id: id(),
        env,
        scopes: BTreeSet::from([Scope::Read, Scope::Admin]),
        digest: mint(id(), bytes).1,
        expires_at: None,
    }
}

#[test]
fn a_matching_key_is_granted_its_record() {
    let (key, _) = mint(id(), [7; 32]);
    let grant = verify(
        &parse(&key).unwrap(),
        Some(&record([7; 32], env())),
        &env(),
        NOW,
    )
    .unwrap();
    assert_eq!(grant.key_id(), id());
    assert_eq!(grant.scopes(), &BTreeSet::from([Scope::Read, Scope::Admin]));
    assert_eq!(grant.env(), env());
}

#[test]
fn unknown_wrong_secret_and_foreign_keys_are_refused_alike() {
    let parts = parse(&mint(id(), [7; 32]).0).unwrap();
    let other_token: EnvRef = format!("{}.{:032x}", env().id(), 1).parse().unwrap();
    let other_id: EnvRef = format!("019a9f3e-7c2b-7d41-8a3e-000000000002.{}", env().token())
        .parse()
        .unwrap();
    let mut renamed = record([7; 32], env());
    renamed.id = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e10".parse().unwrap();
    for (case, record) in [
        ("unknown", None),
        ("wrong secret", Some(record([8; 32], env()))),
        ("another incarnation", Some(record([7; 32], other_token))),
        ("another environment", Some(record([7; 32], other_id))),
        ("another key's record", Some(renamed)),
    ] {
        let refused = verify(&parts, record.as_ref(), &env(), NOW);
        assert!(refused.is_err(), "{case}: granted");
    }
}
