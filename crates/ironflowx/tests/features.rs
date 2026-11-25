//! Exercise the facade in isolation: the workspace's CLI enables TLS on the proto client.

#[test]
fn facade_features_are_opt_in() {
    let expected = match std::env::var("IRONFLOWX_FACADE_FEATURES").as_deref() {
        Err(_) => return, // Workspace tests do not prescribe a facade feature selection.
        Ok("none") => (false, false),
        Ok("client") => (true, false),
        Ok("client-tls") => (true, true),
        Ok(other) => panic!("unexpected facade feature selection: {other}"),
    };
    assert_eq!(
        (cfg!(feature = "client"), cfg!(feature = "client-tls")),
        expected,
    );
}

#[cfg(feature = "client")]
#[test]
fn facade_client_supports_http() {
    assert!(ironflowx::proto::Client::new("http://127.0.0.1:8080").is_ok());
}

#[cfg(feature = "client")]
#[test]
fn facade_client_supports_https_only_with_tls() {
    let Ok(features) = std::env::var("IRONFLOWX_FACADE_FEATURES") else {
        return; // Workspace builds may enable TLS through another crate.
    };
    assert_eq!(
        ironflowx::proto::Client::new("https://127.0.0.1:8080").is_ok(),
        features == "client-tls",
    );
}
