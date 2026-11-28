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

/// A direct ledger user reaches every event type the ledger's API names through `cell`, with no
/// `events` module in the facade.
#[test]
fn facade_cell_names_the_ledgers_event_types() {
    use ironflowx::cell::{
        Command, Emit, Event, EventPage, EventSubmission, ItemOutcome, Refusal, Submission,
    };
    fn named(_: Option<(Event, EventPage, ItemOutcome, Refusal)>) {}
    named(None);
    // The check is that this compiles: a ledger caller builds a submission of either rule module
    // from the facade alone.
    let _ = Submission::Events(EventSubmission::Emit(Emit::default()));
    let _ = Submission::Execution(Command::StartRun {
        function: "charge".into(),
    });
}
