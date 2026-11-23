//! `Engine::open`: lock the root, then a bootstrapping or a later open (architecture.md, "Open"
//! and "Bootstrap").

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use ironflowx_authz::{KeyDigest, KeyParts, Malformed, SeedConflict, SeedState, mint, parse};
use ironflowx_cell::{Ledger, OpenError, new_env_ref};
use ironflowx_core::{Clock, EnvRef, KeyId, Secret};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane, SeedError, StoreError};

use crate::{Engine, Opened};

/// Why an engine did not open.
#[derive(Debug)]
pub enum EngineError {
    /// The seed is not in the key format. Its message names the format, never the seed.
    MalformedSeed,
    /// Another engine holds the root.
    Locked,
    /// The seed's key ID is already a key, and not this seed.
    SeedConflict,
    Ledger(OpenError),
    Store(StoreError),
    Io(std::io::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedSeed => write!(f, "malformed seed: {Malformed}"),
            Self::Locked => f.write_str("the engine root is in use"),
            Self::SeedConflict => SeedConflict.fmt(f),
            Self::Ledger(e) => write!(f, "ledger: {e}"),
            Self::Store(e) => e.fmt(f),
            Self::Io(e) => write!(f, "engine I/O: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<StoreError> for EngineError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<SeedError> for EngineError {
    fn from(e: SeedError) -> Self {
        match e {
            SeedError::Conflict => Self::SeedConflict,
            SeedError::Store(e) => Self::Store(e),
        }
    }
}

impl From<OpenError> for EngineError {
    fn from(e: OpenError) -> Self {
        Self::Ledger(e)
    }
}

/// Runs blocking work off the runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, EngineError> + Send + 'static,
) -> Result<T, EngineError> {
    tokio::task::spawn_blocking(f)
        .await
        .expect("open's blocking steps do not panic")
}

/// Runs a step of an open off the runtime. The step holds the root's lock itself, since it
/// outlives an open that is cancelled: no later open runs beside it.
async fn locked<T: Send + 'static>(
    lock: &Arc<File>,
    step: impl FnOnce() -> Result<T, EngineError> + Send + 'static,
) -> Result<T, EngineError> {
    let lock = lock.clone();
    blocking(move || {
        let done = step();
        drop(lock);
        done
    })
    .await
}

impl Engine {
    /// Opens the data directory `root`, creating and bootstrapping it on the first open. `seed`
    /// is a key the host supplies: the first open makes it the bootstrap key, and returns the
    /// secret of the one it mints when there is none (`Opened::minted`); a later open applies it
    /// once the ledger is open, rotating to it if its key ID is new (D33).
    ///
    /// # Errors
    ///
    /// `MalformedSeed` before anything is read; `Locked` if another engine holds `root`;
    /// `SeedConflict` if the seed's key ID is already another key; a store, ledger or I/O
    /// failure otherwise. A failed later open changes no seed; a first open that fails before its
    /// bootstrap is marked can leave it, seed included, and later opens refuse it. A cancelled
    /// open may still finish the write it was in: `root` stays locked until that write ends.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or if the OS random source fails.
    pub async fn open(
        root: &Path,
        clock: impl Clock + Clone + Send + Sync + 'static,
        seed: Option<Secret>,
    ) -> Result<Opened, EngineError> {
        // The secret goes out of memory here; only its digest goes on.
        let seed = match seed.map(|s| parse(&s)) {
            Some(Err(Malformed)) => return Err(EngineError::MalformedSeed),
            Some(Ok(parts)) => Some(parts),
            None => None,
        };
        let root = root.to_owned();
        let control_path = root.join("control.db");
        let (lock, state) = {
            let (root, control_path) = (root.clone(), control_path.clone());
            blocking(move || {
                let lock = lock_root(&root)?;
                let state = ControlPlane::bootstrap_state(&control_path)?;
                // A staging directory never holds anything acknowledged.
                match std::fs::remove_dir_all(staging(&root)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
                Ok((Arc::new(lock), state))
            })
            .await?
        };
        let control = {
            let path = control_path.clone();
            Arc::new(locked(&lock, move || Ok(ControlPlane::open(&path)?)).await?)
        };
        let engine_clock = Arc::new(clock.clone());
        let (env, ledger, minted, seed) = match state {
            BootstrapState::NoFile | BootstrapState::Unbootstrapped => {
                // The seed is the bootstrap key, so there is nothing to apply.
                let state = seed.as_ref().map(|_| SeedState::Current);
                let (env, ledger, secret) = bootstrap(&root, &lock, &control, clock, seed).await?;
                (env, ledger, secret, state)
            }
            BootstrapState::Marked { env } => {
                let ledger = Ledger::open(&ledger_dir(&root, env), env, clock.clone()).await?;
                let state = apply_seed(&control, &lock, env, seed, clock).await?;
                (env, ledger, None, state)
            }
            // Finishing an earlier open's bootstrap is not built yet: refuse, changing nothing.
            // shortcut: refuses until finishing opens exist; upgrade with them.
            BootstrapState::Unmarked { .. } => {
                return Err(EngineError::Io(std::io::Error::other(
                    "an earlier open did not finish the bootstrap",
                )));
            }
        };
        Ok(Opened {
            engine: Engine {
                ledger,
                control,
                env,
                clock: engine_clock,
                _lock: lock,
            },
            minted,
            seed,
        })
    }
}

/// Applies the seed of a later open, if it was given one.
async fn apply_seed(
    control: &Arc<ControlPlane>,
    lock: &Arc<File>,
    env: EnvRef,
    seed: Option<KeyParts>,
    clock: impl Clock + Send + 'static,
) -> Result<Option<SeedState>, EngineError> {
    let Some(seed) = seed else {
        return Ok(None);
    };
    let control = control.clone();
    locked(lock, move || {
        Ok(Some(control.apply_seed(&env, &seed, &clock)?))
    })
    .await
}

fn lock_root(root: &Path) -> Result<File, EngineError> {
    std::fs::create_dir_all(root)?;
    // Never deleted, so every contender locks the same file.
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("engine.lock"))?;
    lock.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => EngineError::Locked,
        std::fs::TryLockError::Error(e) => EngineError::Io(e),
    })?;
    Ok(lock)
}

fn staging(root: &Path) -> std::path::PathBuf {
    root.join("ledgers").join(".staging")
}

fn ledger_dir(root: &Path, env: EnvRef) -> std::path::PathBuf {
    root.join("ledgers").join(env.id().to_string())
}

/// A fresh key: a UUIDv7 ID and 256 bits from the OS random source.
pub(crate) fn new_key() -> (KeyId, Secret, KeyDigest) {
    let id = uuid7();
    let mut bits = [0; 32];
    getrandom::fill(&mut bits).expect("the OS random source works");
    let (secret, digest) = mint(id, bits);
    (id, secret, digest)
}

fn uuid7<T: std::str::FromStr>() -> T
where
    T::Err: fmt::Debug,
{
    uuid::Uuid::now_v7()
        .to_string()
        .parse()
        .expect("a UUIDv7 parses")
}

/// A bootstrapping open: mint the environment, and its admin key unless it is the seed;
/// bootstrap the control plane in one transaction; build the ledger aside, rename it into place
/// and open it; then mark it (D40). The mark is the last fallible step, so no failure after it
/// can lose the secret.
// shortcut: a failure before the mark leaves an unmarked bootstrap, which the next open refuses
// rather than undoing or finishing; upgrade with finishing opens and the undo.
async fn bootstrap(
    root: &Path,
    lock: &Arc<File>,
    control: &Arc<ControlPlane>,
    clock: impl Clock + Clone + Send + Sync + 'static,
    seed: Option<KeyParts>,
) -> Result<(EnvRef, Ledger, Option<Secret>), EngineError> {
    let env = new_env_ref();
    let by_seed = seed.is_some();
    let (key, secret) = seed.map_or_else(
        || {
            let (id, secret, digest) = new_key();
            (KeyParts { id, digest }, Some(secret))
        },
        |seed| (seed, None),
    );
    let plan = Bootstrap {
        env,
        tenant: uuid7(),
        project: uuid7(),
        key,
        by_seed,
    };
    {
        let (control, clock) = (control.clone(), clock.clone());
        locked(lock, move || Ok(control.ensure_bootstrap(&plan, &clock)?)).await?;
    }
    let staging = staging(root);
    Ledger::open(&staging, env, clock.clone())
        .await?
        .close()
        .await;
    let ledgers = root.join("ledgers");
    let target = ledger_dir(root, env);
    {
        let target = target.clone();
        locked(lock, move || {
            std::fs::rename(&staging, &target)?;
            File::open(&ledgers)?.sync_all()?;
            Ok(())
        })
        .await?;
    }
    let ledger = Ledger::open(&target, env, clock.clone()).await?;
    let control = control.clone();
    locked(lock, move || Ok(control.mark_ledger_created(&env, &clock)?)).await?;
    Ok((env, ledger, secret))
}
