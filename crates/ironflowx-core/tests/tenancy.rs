use ironflowx_core::{Address, ErrorKind, Scope};

#[test]
fn an_address_is_project_slash_environment() {
    assert_eq!(
        Address::new("default", "default").to_string(),
        "default/default"
    );
    assert_eq!(Address::new("web", "prod").to_string(), "web/prod");
}

#[test]
fn scopes_have_one_text_form_each() {
    for (scope, text) in [
        (Scope::Read, "read"),
        (Scope::Write, "write"),
        (Scope::Admin, "admin"),
    ] {
        assert_eq!(scope.to_string(), text);
        assert_eq!(text.parse::<Scope>(), Ok(scope));
    }
    for bad in ["", "Read", "READ", "admin ", "all"] {
        assert!(bad.parse::<Scope>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn only_unavailable_and_unknown_are_retryable_so_far() {
    for (kind, retryable) in [
        (ErrorKind::Invalid, false),
        (ErrorKind::NotFound, false),
        (ErrorKind::Unauthenticated, false),
        (ErrorKind::PermissionDenied, false),
        (ErrorKind::Unavailable, true),
        (ErrorKind::Unknown, true),
    ] {
        assert_eq!(kind.retryable(), retryable, "{kind:?}");
    }
}
