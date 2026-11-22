use ironflowx_core::{
    Emitter, EnvRef, EnvironmentId, EventId, IncarnationToken, KeyId, ProjectId, SubmitAs, TenantId,
};

const V7: &str = "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f";
const V4: &str = "3f2b8c1e-4d5a-4b6c-9d7e-8f9a0b1c2d3e";
const TOKEN: &str = "00112233445566778899aabbccddeeff";

#[test]
fn environment_id_parses_only_a_uuidv7() {
    assert_eq!(V7.parse::<EnvironmentId>().unwrap().to_string(), V7);
    for bad in [
        "",
        V4,
        "org/project/env",
        "irn:ironflow:acme:web:environment:prod:1",
        "019A9F3E-7C2B-7D41-8A3E-5F6B7C8D9E0F", // uppercase is another form
        "019a9f3e7c2b7d418a3e5f6b7c8d9e0f",     // simple form
        "{019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f}",
        "urn:uuid:019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f",
        "019a9f3e-7c2b-7d41-0a3e-5f6b7c8d9e0f", // version 7, wrong variant
    ] {
        assert!(bad.parse::<EnvironmentId>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn event_id_parses_only_a_uuidv7() {
    assert_eq!(V7.parse::<EventId>().unwrap().to_string(), V7);
    assert!(V4.parse::<EventId>().is_err());
    assert!("".parse::<EventId>().is_err());
}

#[test]
fn incarnation_token_is_32_lowercase_hex_digits() {
    assert_eq!(
        TOKEN.parse::<IncarnationToken>().unwrap().to_string(),
        TOKEN
    );
    for bad in [
        "",
        "00112233445566778899aabbccddeef",   // 31 digits
        "00112233445566778899aabbccddeeff0", // 33 digits
        "00112233445566778899AABBCCDDEEFF",
        "+0112233445566778899aabbccddeeff",
        "00112233445566778899aabbccddeefg",
    ] {
        assert!(bad.parse::<IncarnationToken>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn env_ref_round_trips_through_its_text_form() {
    let text = format!("{V7}.{TOKEN}");
    let env: EnvRef = text.parse().unwrap();
    assert_eq!(env.to_string(), text);
    assert_eq!(env.id(), V7.parse().unwrap());
    assert_eq!(env.token(), TOKEN.parse().unwrap());
}

#[test]
fn malformed_env_ref_is_refused() {
    for bad in [
        String::new(),
        V7.to_string(),
        format!("{V7}."),
        format!(".{TOKEN}"),
        format!("{V4}.{TOKEN}"),
        format!("{V7}.{TOKEN}.{TOKEN}"),
        format!("{V7}:{TOKEN}"),
        format!("{V7}.{}", &TOKEN[1..]),
    ] {
        assert!(bad.parse::<EnvRef>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn key_id_parses_only_a_uuidv7() {
    assert_eq!(V7.parse::<KeyId>().unwrap().to_string(), V7);
    for bad in [
        "",
        V4,
        "019A9F3E-7C2B-7D41-8A3E-5F6B7C8D9E0F", // uppercase is another form
        "019a9f3e7c2b7d418a3e5f6b7c8d9e0f",
        "{019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0f}",
        "019a9f3e-7c2b-7d41-0a3e-5f6b7c8d9e0f", // version 7, wrong variant
        "019a9f3e-7c2b-7d41-8a3e-5f6b7c8d9e0",  // malformed
    ] {
        assert!(bad.parse::<KeyId>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn an_embedder_name_is_1_to_128_bytes_of_readable_characters() {
    for good in [
        "local-operations",
        "a",
        "批处理:app",
        "café au lait",
        &"x".repeat(128),
    ] {
        let emitter = Emitter::from(SubmitAs::embedder(good).unwrap());
        assert_eq!(emitter.to_string(), format!("embedder:{good}"));
    }
    for bad in [
        "",
        &"x".repeat(129),
        &format!("{}é", "x".repeat(127)), // 128 characters, 129 bytes
        "tab\there",
        "nul\0",
        "new\nline",
        "del\u{7f}",
        "billing\u{202E}nimda", // right-to-left override (Cf)
        "zero\u{200B}width",    // Cf
        "soft\u{AD}hyphen",     // Cf
        "tag\u{E0041}",         // Cf, outside the BMP
        "line\u{2028}sep",      // Zl
        "para\u{2029}sep",      // Zp
    ] {
        assert!(SubmitAs::embedder(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn an_emitter_round_trips_through_its_text_form() {
    for text in [format!("key:{V7}"), "embedder:app:main".to_owned()] {
        assert_eq!(text.parse::<Emitter>().unwrap().to_string(), text);
    }
    for bad in [
        String::new(),
        "key:".to_owned(),
        format!("key:{V4}"),
        "embedder:".to_owned(),
        "embedder:a\nb".to_owned(),
        format!("Key:{V7}"),
        "local-operations".to_owned(),
        format!("tenant:{V7}"),
    ] {
        assert!(bad.parse::<Emitter>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn tenant_and_project_ids_parse_only_a_uuidv7() {
    assert_eq!(V7.parse::<TenantId>().unwrap().to_string(), V7);
    assert_eq!(V7.parse::<ProjectId>().unwrap().to_string(), V7);
    for bad in [
        "",
        V4,
        "default",
        "019A9F3E-7C2B-7D41-8A3E-5F6B7C8D9E0F",
        "019a9f3e7c2b7d418a3e5f6b7c8d9e0f",
        "019a9f3e-7c2b-7d41-0a3e-5f6b7c8d9e0f",
    ] {
        assert!(bad.parse::<TenantId>().is_err(), "tenant accepted {bad:?}");
        assert!(
            bad.parse::<ProjectId>().is_err(),
            "project accepted {bad:?}"
        );
    }
}
