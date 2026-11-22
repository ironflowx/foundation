//! `tenancy.feature` on the engine. Only the scenarios in [`REGISTERED`] run; each ticket that
//! lands a scenario adds its ID, and every registered scenario must run and pass.

use std::fmt;

use cucumber::writer::Stats as _;
use cucumber::{World, given, then, when};
use ironflowx_cell::ManualClock;
use ironflowx_core::{EnvRef, Scope, Secret, Timestamp};
use ironflowx_operations::{Engine, Opened};
use ironflowx_proto::{Call, Environment, ListEnvironmentsRequest, OpError, Whoami, WhoamiRequest};
use tempfile::TempDir;

/// The tenancy scenarios that run, by tag.
pub const REGISTERED: &[&str] = &["ACC-001", "ACC-002", "ACC-003", "ACC-009"];

#[derive(World)]
#[world(init = Self::new)]
pub struct Tenancy {
    // Before `dir`: the engine must close before its directory goes.
    engine: Option<Engine>,
    /// A second engine, with its own directory (ACC-009).
    other: Option<(Engine, TempDir)>,
    dir: TempDir,
    clock: ManualClock,
    /// The bootstrap key's text, as the first open returned it.
    bootstrap_key: Option<String>,
    /// What the last open returned in `minted`.
    returned: Option<bool>,
    whoami: Option<Result<Whoami, OpError>>,
    /// The environment before a reopen (ACC-003).
    env_before: Option<EnvRef>,
    /// The second engine's environment, as its own key lists it (ACC-009).
    other_env: Option<Environment>,
}

impl fmt::Debug for Tenancy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tenancy")
            .field("returned", &self.returned)
            .field("whoami", &self.whoami)
            .finish_non_exhaustive()
    }
}

fn clock() -> ManualClock {
    ManualClock::new(Timestamp::from_millis(1_800_000_000_000))
}

/// A call presenting `key`; tests copy the text, since a `Secret` cannot be cloned.
fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

fn scopes(text: &str) -> std::collections::BTreeSet<Scope> {
    text.split(['"', ',', ' '])
        .filter(|s| !s.is_empty() && *s != "and")
        .map(|s| s.parse().expect("a scope"))
        .collect()
}

impl Tenancy {
    fn new() -> Self {
        Self {
            engine: None,
            other: None,
            dir: tempfile::tempdir().unwrap(),
            clock: clock(),
            bootstrap_key: None,
            returned: None,
            whoami: None,
            env_before: None,
            other_env: None,
        }
    }

    async fn open(&mut self) {
        let Opened { engine, minted } = Engine::open(self.dir.path(), self.clock.clone())
            .await
            .expect("the engine opens");
        self.returned = Some(minted.is_some());
        if let Some(key) = minted {
            self.bootstrap_key = Some(key.expose().to_owned());
        }
        self.engine = Some(engine);
    }

    fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("an open engine")
    }

    fn key(&self) -> Call {
        call(self.bootstrap_key.as_deref().expect("the bootstrap key"))
    }

    async fn whoami(&self) -> Result<Whoami, OpError> {
        self.engine().whoami(&self.key(), WhoamiRequest {}).await
    }

    async fn environments(&self) -> Vec<Environment> {
        self.engine()
            .list_environments(&self.key(), ListEnvironmentsRequest {})
            .await
            .expect("the key lists environments")
    }
}

#[given("a fresh engine directory")]
fn fresh(_: &mut Tenancy) {}

#[when("the engine is opened")]
#[given("the engine was opened and returned its bootstrap key")]
async fn opened(w: &mut Tenancy) {
    w.open().await;
    assert_eq!(
        w.returned,
        Some(true),
        "the first open returns the bootstrap key"
    );
}

#[given("a client with the bootstrap key")]
async fn client(w: &mut Tenancy) {
    w.open().await;
}

#[then("exactly one key is returned")]
fn one_key(w: &mut Tenancy) {
    assert_eq!(w.returned, Some(true));
    let key = w.bootstrap_key.as_deref().unwrap();
    assert!(key.starts_with("ixk_") && key.len() == 84, "{key}");
}

#[then("no key is returned")]
fn no_key(w: &mut Tenancy) {
    assert_eq!(w.returned, Some(false));
}

#[then(regex = r#"^with it, whoami names scopes (.+) and environment "([^"]+)"$"#)]
async fn whoami_names(w: &mut Tenancy, names: String, address: String) {
    let me = w.whoami().await.expect("the bootstrap key authenticates");
    assert_eq!(me.scopes, scopes(&names));
    assert_eq!(me.environment.to_string(), address);
}

#[when("the client asks whoami")]
async fn asks_whoami(w: &mut Tenancy) {
    w.whoami = Some(w.whoami().await);
}

#[then(
    regex = r#"^the answer names the key's ID, scopes (.+), environment "([^"]+)" and no expiry$"#
)]
fn answer_names(w: &mut Tenancy, names: String, address: String) {
    let me = w
        .whoami
        .as_ref()
        .unwrap()
        .as_ref()
        .expect("whoami succeeds");
    let key = w.bootstrap_key.as_deref().unwrap();
    // The key's ID is its text between `ixk_` and the secret.
    assert_eq!(me.key_id.to_string(), key[4..40]);
    assert_eq!(me.scopes, scopes(&names));
    assert_eq!(me.environment.to_string(), address);
    assert_eq!(me.expires_at, None);
}

#[then(regex = r#"^listing environments shows one environment, named "([^"]+)"$"#)]
async fn lists_one(w: &mut Tenancy, name: String) {
    let envs = w.environments().await;
    assert_eq!(envs.len(), 1, "{envs:?}");
    assert_eq!(envs[0].name, name);
}

#[when("the engine is closed and opened again")]
async fn reopened(w: &mut Tenancy) {
    w.env_before = Some(w.environments().await[0].env);
    w.engine.take().unwrap().close().await;
    w.open().await;
}

#[then("the bootstrap key still authenticates")]
async fn still_authenticates(w: &mut Tenancy) {
    w.whoami().await.expect("the bootstrap key authenticates");
}

#[then("listing environments still shows one environment, with the same ID")]
async fn same_environment(w: &mut Tenancy) {
    let envs = w.environments().await;
    assert_eq!(envs.len(), 1, "{envs:?}");
    assert_eq!(Some(envs[0].env), w.env_before);
}

#[when("two engines are opened in two fresh directories")]
async fn two_engines(w: &mut Tenancy) {
    w.open().await;
    let dir = tempfile::tempdir().unwrap();
    let Opened { engine, minted } = Engine::open(dir.path(), clock()).await.unwrap();
    let key = call(minted.expect("the second engine's bootstrap key").expose());
    let mut envs = engine
        .list_environments(&key, ListEnvironmentsRequest {})
        .await
        .unwrap();
    assert_eq!(envs.len(), 1, "{envs:?}");
    w.other_env = envs.pop();
    w.other = Some((engine, dir));
}

#[then("their environments have different IDs and different project IDs")]
async fn different_ids(w: &mut Tenancy) {
    let first = w.environments().await.remove(0);
    let second = w.other_env.as_ref().unwrap();
    assert_ne!(first.env.id(), second.env.id());
    assert_ne!(first.project_id, second.project_id);
}

#[then(regex = r#"^both environments have the address "([^"]+)"$"#)]
async fn both_addresses(w: &mut Tenancy, address: String) {
    assert_eq!(w.environments().await[0].address.to_string(), address);
    assert_eq!(w.other_env.as_ref().unwrap().address.to_string(), address);
}

/// Runs the registered tenancy scenarios. True if any failed or one did not run.
pub async fn run(parser: super::Unescaped) -> bool {
    let writer = Tenancy::cucumber::<&str>()
        .with_parser(parser)
        .after(|_, _, _, _, world| {
            Box::pin(async move {
                if let Some(w) = world {
                    if let Some(engine) = w.engine.take() {
                        engine.close().await;
                    }
                    if let Some((engine, _dir)) = w.other.take() {
                        engine.close().await;
                    }
                }
            })
        })
        .fail_on_skipped()
        .filter_run(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/behaviors/tenancy.feature"
            ),
            |_, _, scenario| {
                scenario
                    .tags
                    .iter()
                    .any(|t| REGISTERED.contains(&t.as_str()))
            },
        )
        .await;
    let passed = writer.scenarios_stats().passed;
    writer.execution_has_failed() || passed != REGISTERED.len()
}
