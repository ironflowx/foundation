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
        (Scope::Execute, "execute"),
    ] {
        assert_eq!(scope.to_string(), text);
        assert_eq!(text.parse::<Scope>(), Ok(scope));
    }
    for bad in ["", "Read", "READ", "admin ", "all", "Execute", "exec"] {
        assert!(bad.parse::<Scope>().is_err(), "accepted {bad:?}");
    }
}

/// The kind table (architecture.md, "Errors").
#[test]
fn each_kind_has_its_exit_code_and_only_unavailable_is_retryable() {
    for (kind, retryable, exit) in [
        (ErrorKind::Internal, false, 1),
        (ErrorKind::Invalid, false, 2),
        (ErrorKind::Unauthenticated, false, 3),
        (ErrorKind::PermissionDenied, false, 4),
        (ErrorKind::NotFound, false, 5),
        (ErrorKind::Unknown, false, 6),
        (ErrorKind::ResourceExhausted, false, 7),
        (ErrorKind::Unavailable, true, 8),
        (ErrorKind::Conflict, false, 9),
    ] {
        assert_eq!(kind.retryable(), retryable, "{kind:?}");
        assert_eq!(kind.exit_code(), exit, "{kind:?}");
    }
}
