//! `Engine::open`: lock the root, then a bootstrapping or a later open (architecture.md, "Open"
//! and "Bootstrap").

use std::fmt;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use ironflowx_authz::{KeyParts, mint};
use ironflowx_cell::{Ledger, OpenError, new_env_ref};
use ironflowx_core::{Clock, EnvRef, Secret};
use ironflowx_store::{Bootstrap, BootstrapState, ControlPlane, StoreError};

use crate::{Engine, Opened};

/// Why an engine did not open.
#[derive(Debug)]
pub enum EngineError {
    /// Another engine holds the root.
    Locked,
    Ledger(OpenError),
    Store(StoreError),
    Io(std::io::Error),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => f.write_str("the engine root is in use"),
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

impl Engine {
    /// Opens the data directory `root`, creating and bootstrapping it on the first open, which
    /// alone returns the bootstrap key's secret (`Opened::minted`).
    ///
    /// # Errors
    ///
    /// `Locked` if another engine holds `root`; a store, ledger or I/O failure otherwise.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or if the OS random source fails.
    pub async fn open(
        root: &Path,
        clock: impl Clock + Clone + Send + Sync + 'static,
    ) -> Result<Opened, EngineError> {
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
                Ok((lock, state))
            })
            .await?
        };
        let control = {
            let path = control_path.clone();
            Arc::new(blocking(move || Ok(ControlPlane::open(&path)?)).await?)
        };
        let (env, minted) = match state {
            BootstrapState::NoFile | BootstrapState::Unbootstrapped => {
                let (env, secret) = bootstrap(&root, &control, clock.clone()).await?;
                (env, Some(secret))
            }
            BootstrapState::Marked { env } => (env, None),
            // Finishing an earlier open's bootstrap is not built yet: refuse, changing nothing.
            // shortcut: refuses until finishing opens exist; upgrade with them.
            BootstrapState::Unmarked { .. } => {
                return Err(EngineError::Io(std::io::Error::other(
                    "an earlier open did not finish the bootstrap",
                )));
            }
        };
        let ledger = Ledger::open(&ledger_dir(&root, env), env, clock).await?;
        Ok(Opened {
            engine: Engine {
                ledger,
                control,
                env,
                _lock: lock,
            },
            minted,
        })
    }
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

fn uuid7<T: std::str::FromStr>() -> T
where
    T::Err: fmt::Debug,
{
    uuid::Uuid::now_v7()
        .to_string()
        .parse()
        .expect("a UUIDv7 parses")
}

/// A bootstrapping open: mint the environment and its admin key, bootstrap the control plane
/// in one transaction, build the ledger aside and rename it into place, then mark it (D40).
async fn bootstrap(
    root: &Path,
    control: &Arc<ControlPlane>,
    clock: impl Clock + Clone + Send + Sync + 'static,
) -> Result<(EnvRef, Secret), EngineError> {
    let env = new_env_ref();
    let key_id = uuid7();
    let mut bits = [0; 32];
    getrandom::fill(&mut bits).expect("the OS random source works");
    let (secret, digest) = mint(key_id, bits);
    let plan = Bootstrap {
        env,
        tenant: uuid7(),
        project: uuid7(),
        key: KeyParts { id: key_id, digest },
    };
    {
        let (control, clock) = (control.clone(), clock.clone());
        blocking(move || Ok(control.ensure_bootstrap(&plan, &clock)?)).await?;
    }
    let staging = staging(root);
    Ledger::open(&staging, env, clock.clone())
        .await?
        .close()
        .await;
    let ledgers = root.join("ledgers");
    let target = ledger_dir(root, env);
    blocking(move || {
        std::fs::rename(&staging, &target)?;
        File::open(&ledgers)?.sync_all()?;
        Ok(())
    })
    .await?;
    let control = control.clone();
    blocking(move || Ok(control.mark_ledger_created(&env, &clock)?)).await?;
    Ok((env, secret))
}
