//! `Engine::open`: check and lock the root, then a bootstrapping, a finishing or a later open
//! (architecture.md, "Engine open").

use std::fmt;
use std::fs::File;
use std::io::ErrorKind::NotFound;
use std::path::{Path, PathBuf};
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
    /// The directory holds a ledger, not an engine.
    NotAnEngineRoot,
    /// Another engine holds the root.
    Locked,
    /// The control plane is in another format version.
    Format,
    /// A ledger without its control plane: the control plane was lost.
    Orphaned,
    /// The control plane without its environment's ledger: the ledger was lost, emptied, or begins
    /// with a record of events.
    LedgerMissing,
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
            Self::NotAnEngineRoot => f.write_str("the directory holds a ledger, not an engine"),
            Self::Locked => f.write_str("the engine root is in use"),
            Self::Format => f.write_str("the control plane is in another format version"),
            Self::Orphaned => f.write_str("the control plane is missing, and a ledger is not"),
            Self::LedgerMissing => f.write_str("the environment's ledger is missing"),
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
        match e {
            StoreError::Format => Self::Format,
            e @ StoreError::Failed(_) => Self::Store(e),
        }
    }
}

impl From<SeedError> for EngineError {
    fn from(e: SeedError) -> Self {
        match e {
            SeedError::Conflict => Self::SeedConflict,
            SeedError::Store(e) => e.into(),
        }
    }
}

impl From<OpenError> for EngineError {
    fn from(e: OpenError) -> Self {
        match e {
            OpenError::Missing => Self::LedgerMissing,
            e => Self::Ledger(e),
        }
    }
}

/// Reads whether the bootstrap in `root` is marked. Creates no directory or database.
/// SQLite can rebuild the shared-memory file of a WAL left by a crash.
///
/// This blocking read takes no engine lock and does not check the ledger.
///
/// # Errors
///
/// [`EngineError::Format`] for another control-plane format, or [`EngineError::Store`]
/// if the state cannot be read.
pub fn is_bootstrapped(root: &Path) -> Result<bool, EngineError> {
    Ok(matches!(
        bootstrap_state(root)?,
        BootstrapState::Marked { .. }
    ))
}

fn bootstrap_state(root: &Path) -> Result<BootstrapState, StoreError> {
    ControlPlane::bootstrap_state(&root.join("control.db"))
}

/// Runs blocking work off the runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, EngineError> + Send + 'static,
) -> Result<T, EngineError> {
    tokio::task::spawn_blocking(f)
        .await
        .expect("open's blocking steps do not panic")
}

/// A locked engine root.
struct Root {
    dir: PathBuf,
    lock: Arc<File>,
}

impl Root {
    /// Runs a step of an open off the runtime. The step holds the root's lock itself, since it
    /// outlives an open that is cancelled: no later open runs beside it.
    async fn step<T: Send + 'static>(
        &self,
        step: impl FnOnce() -> Result<T, EngineError> + Send + 'static,
    ) -> Result<T, EngineError> {
        let lock = self.lock.clone();
        // With what it made: a cancelled open's ledger closes before the lock goes.
        let (made, _lock) = blocking(move || Ok((step()?, lock))).await?;
        Ok(made)
    }

    /// Opens the control plane for writing.
    async fn control(&self) -> Result<Arc<ControlPlane>, EngineError> {
        let path = self.dir.join("control.db");
        self.step(move || Ok(Arc::new(ControlPlane::open(&path)?)))
            .await
    }

    /// Opens the control plane of a bootstrapped root for writing, unless `seed` conflicts with a
    /// key in it: then nothing is written to it, not even a WAL that a crash left (D33).
    async fn control_for(
        &self,
        env: EnvRef,
        seed: Option<KeyParts>,
    ) -> Result<Arc<ControlPlane>, EngineError> {
        if let Some(seed) = seed {
            let path = self.dir.join("control.db");
            self.step(move || Ok(ControlPlane::check_seed(&path, &env, &seed)?))
                .await?;
        }
        self.control().await
    }

    /// Opens `env`'s ledger at its final name if anything is there, else builds it in staging and
    /// renames it into place first (D40).
    async fn create_ledger(
        &self,
        env: EnvRef,
        clock: impl Clock + Clone + Send + 'static,
    ) -> Result<Ledger, EngineError> {
        let (staging, target) = (staging(&self.dir), ledger_dir(&self.dir, env));
        let ledgers = self.dir.join("ledgers");
        self.step(move || {
            if !target.try_exists()? {
                drop(Ledger::open_blocking(&staging, env, clock.clone())?);
                #[cfg(feature = "__fault-points")]
                ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeRename);
                std::fs::rename(&staging, &target)?;
                #[cfg(feature = "__fault-points")]
                ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterRename);
            }
            // Also for a rename an earlier open died after: the mark must not outlive it.
            File::open(&ledgers)?.sync_all()?;
            Ok(Ledger::open_blocking(&target, env, clock)?)
        })
        .await
    }
}

/// What an open of any kind makes.
struct Made {
    env: EnvRef,
    ledger: Ledger,
    control: Arc<ControlPlane>,
    minted: Option<Secret>,
    lost_secret: bool,
    seed: Option<SeedState>,
}

impl Engine {
    /// Opens the data directory `root`, creating and bootstrapping it on the first open. `seed`
    /// is a key the host supplies: the first open makes it the bootstrap key, and returns the
    /// secret of the one it mints when there is none (`Opened::minted`); a later open applies it
    /// once the ledger is open, rotating to it if its key ID is new (D33). An open that finds a
    /// bootstrap an earlier one did not finish, and succeeds, finishes it: a minted key whose
    /// secret that one never returned is revoked (`Opened::lost_secret`), and replaced if there is
    /// no seed.
    ///
    /// # Errors
    ///
    /// `MalformedSeed` before anything is read. `NotAnEngineRoot`, `Locked`, `Format` and
    /// `Orphaned` change nothing but the lock file and a leftover staging directory, nor does
    /// `LedgerMissing`, but for what is recovered to find them: `control.db-shm`, which reading a
    /// WAL a crash left rebuilds, and, for `LedgerMissing`, what a log that has a segment, and the
    /// `read.db` beside it, recover as they open. `SeedConflict`
    /// if the seed's key ID is already another key, with no key changed; a store, ledger or I/O
    /// failure otherwise.
    ///
    /// An open that fails while it bootstraps tries to undo it, unless its ledger is already in
    /// place, and returns the failure, not the undo's. A bootstrap left in place, its ledger built
    /// or its undo failed, is for a later open to finish. No other open undoes anything, and one
    /// that fails rotates no seed, but as below. A cancelled open may still finish the step it was
    /// in, and starts no undo: `root` stays locked until that step ends.
    ///
    /// If it failed because a write to the control plane failed (`Store`), that write may still
    /// land when `control.db` is next opened. A later open may then return no key for a bootstrap
    /// this one, given no seed, began or was finishing, with `lost_secret` false, and unless a
    /// seed began it, its key's secret is lost: [`crate::recover_admin_key`] is the way in. Or it
    /// may find this one's seed current and the previous current seed, if any, revoked: open
    /// with the same seed again.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime or on one that has shut down, or if the OS random source fails.
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
        let dir = root.to_owned();
        let (root, state) = blocking(move || {
            refuse_bare_ledger(&dir)?;
            let lock = Arc::new(lock_root(&dir)?);
            let state = classify(&dir)?;
            Ok((Root { dir, lock }, state))
        })
        .await?;
        let made = match state {
            BootstrapState::NoFile | BootstrapState::Unbootstrapped => {
                bootstrap(&root, clock.clone(), seed).await?
            }
            BootstrapState::Unmarked { env } => finish(&root, env, clock.clone(), seed).await?,
            BootstrapState::Marked { env } => reopen(&root, env, clock.clone(), seed).await?,
        };
        Ok(Opened {
            engine: Engine {
                ledger: made.ledger,
                control: made.control,
                env: made.env,
                clock: Arc::new(clock),
                _lock: root.lock,
            },
            minted: made.minted,
            lost_secret: made.lost_secret,
            seed: made.seed,
        })
    }
}

/// Refuses a ledger's own directory before anything is created in it (D32).
fn refuse_bare_ledger(root: &Path) -> Result<(), EngineError> {
    match std::fs::metadata(root.join("log")) {
        Ok(log) if log.is_dir() => Err(EngineError::NotAnEngineRoot),
        Err(e) if e.kind() != NotFound => Err(e.into()),
        _ => Ok(()),
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

/// Reads which kind of open this is, deleting a leftover staging directory.
fn classify(root: &Path) -> Result<BootstrapState, EngineError> {
    let state = bootstrap_state(root)?;
    // A staging directory never holds anything acknowledged.
    match std::fs::remove_dir_all(staging(root)) {
        Err(e) if e.kind() != NotFound => return Err(e.into()),
        _ => {}
    }
    let unbootstrapped = matches!(
        state,
        BootstrapState::NoFile | BootstrapState::Unbootstrapped
    );
    if unbootstrapped && holds_ledger(root)? {
        return Err(EngineError::Orphaned);
    }
    Ok(state)
}

/// Whether `ledgers/` holds anything that could be a ledger: all but plain files, so that a
/// symbolic link to one counts.
fn holds_ledger(root: &Path) -> std::io::Result<bool> {
    let entries = match std::fs::read_dir(root.join("ledgers")) {
        Err(e) if e.kind() == NotFound => return Ok(false),
        entries => entries?,
    };
    for entry in entries {
        if !entry?.file_type()?.is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn staging(root: &Path) -> PathBuf {
    root.join("ledgers").join(".staging")
}

fn ledger_dir(root: &Path, env: EnvRef) -> PathBuf {
    root.join("ledgers").join(env.id().to_string())
}

/// Makes a seed for a host to deliver. It grants nothing until an engine takes it as a seed.
///
/// # Panics
///
/// If the OS random source fails.
pub fn generate_seed() -> (KeyId, Secret) {
    let (id, secret, _) = new_key();
    (id, secret)
}

/// The presented key's ID, only if the complete key format is valid.
/// The key is not verified against an engine.
pub fn key_id(key: &Secret) -> Option<KeyId> {
    parse(key).ok().map(|parts| parts.id)
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

/// A fresh key as the control plane stores an admin key, and its secret.
pub(crate) fn new_admin_key() -> (KeyParts, Secret) {
    let (id, secret, digest) = new_key();
    (KeyParts { id, digest }, secret)
}

/// A bootstrapping open: mint the environment, and its admin key unless it is the seed;
/// bootstrap the control plane in one transaction; build the ledger; then mark it (D41). The mark
/// is the last fallible step, so no failure after it can lose the secret; its own commit, if it
/// fails, can (D48). A failure before it tries to undo the bootstrap, unless the ledger is already
/// in place.
async fn bootstrap(
    root: &Root,
    clock: impl Clock + Clone + Send + Sync + 'static,
    seed: Option<KeyParts>,
) -> Result<Made, EngineError> {
    let control = root.control().await?;
    let env = new_env_ref();
    // The seed is the bootstrap key, so there is nothing to apply.
    let state = seed.as_ref().map(|_| SeedState::Current);
    let (key, secret) = match seed {
        Some(seed) => (seed, None),
        None => {
            let (key, secret) = new_admin_key();
            (key, Some(secret))
        }
    };
    #[cfg(feature = "__fault-points")]
    ironflowx_core::tell_key(env, key.id, secret.as_ref());
    let plan = Bootstrap {
        env,
        tenant: uuid7(),
        project: uuid7(),
        key,
        by_seed: secret.is_none(),
    };
    match create(root, &control, plan, clock).await {
        Ok(ledger) => Ok(Made {
            env,
            ledger,
            control,
            minted: secret,
            lost_secret: false,
            seed: state,
        }),
        Err(e) => {
            let dir = root.dir.clone();
            let undone = root.step(move || {
                #[cfg(feature = "__fault-points")]
                ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::BeforeUndo);
                let _ = std::fs::remove_dir_all(staging(&dir));
                if !ledger_dir(&dir, env).try_exists()? {
                    control.undo_bootstrap(&env)?;
                }
                #[cfg(feature = "__fault-points")]
                ironflowx_core::reach_fault_point(ironflowx_core::FaultPoint::AfterUndo);
                Ok(())
            });
            // What an undo that fails leaves is for a later open to finish.
            let _ = undone.await;
            Err(e)
        }
    }
}

/// The steps of a bootstrapping open whose failure it tries to undo.
async fn create(
    root: &Root,
    control: &Arc<ControlPlane>,
    plan: Bootstrap,
    clock: impl Clock + Clone + Send + Sync + 'static,
) -> Result<Ledger, EngineError> {
    let env = plan.env;
    {
        let (control, clock) = (control.clone(), clock.clone());
        root.step(move || Ok(control.ensure_bootstrap(&plan, &clock)?))
            .await?;
    }
    let ledger = root.create_ledger(env, clock.clone()).await?;
    let control = control.clone();
    root.step(move || Ok(control.mark_ledger_created(&env, &clock)?))
        .await?;
    Ok(ledger)
}

/// A finishing open: complete the bootstrap an earlier open left unmarked. It undoes nothing
/// (D41), and refuses a conflicting seed before it writes to `control.db` (D33).
async fn finish(
    root: &Root,
    env: EnvRef,
    clock: impl Clock + Clone + Send + Sync + 'static,
    seed: Option<KeyParts>,
) -> Result<Made, EngineError> {
    let ledger = root.create_ledger(env, clock.clone()).await?;
    let control = root.control_for(env, seed.clone()).await?;
    let (replacement, secret) = seed.is_none().then(new_admin_key).unzip();
    #[cfg(feature = "__fault-points")]
    if let Some(key) = &replacement {
        ironflowx_core::tell_key(env, key.id, secret.as_ref());
    }
    let lost_secret = {
        let (control, clock) = (control.clone(), clock.clone());
        root.step(move || Ok(control.finish_bootstrap(&env, replacement.as_ref(), &clock)?))
            .await?
    };
    let seed = apply_seed(root, &control, env, seed, clock).await?;
    Ok(Made {
        env,
        ledger,
        control,
        // The replacement was stored only if it replaced a key.
        minted: secret.filter(|_| lost_secret),
        lost_secret,
        seed,
    })
}

/// A later open: the bootstrap is finished, so a ledger that is not there was lost.
async fn reopen(
    root: &Root,
    env: EnvRef,
    clock: impl Clock + Clone + Send + Sync + 'static,
    seed: Option<KeyParts>,
) -> Result<Made, EngineError> {
    let (dir, ledger_clock) = (ledger_dir(&root.dir, env), clock.clone());
    let ledger = root
        .step(move || Ok(Ledger::open_existing_blocking(&dir, env, ledger_clock)?))
        .await?;
    // After the ledger, so that refusing a lost one does not write to `control.db`.
    let control = root.control_for(env, seed.clone()).await?;
    let seed = apply_seed(root, &control, env, seed, clock).await?;
    Ok(Made {
        env,
        ledger,
        control,
        minted: None,
        lost_secret: false,
        seed,
    })
}

/// Applies the seed, if the open was given one.
async fn apply_seed(
    root: &Root,
    control: &Arc<ControlPlane>,
    env: EnvRef,
    seed: Option<KeyParts>,
    clock: impl Clock + Send + 'static,
) -> Result<Option<SeedState>, EngineError> {
    let Some(seed) = seed else {
        return Ok(None);
    };
    let control = control.clone();
    root.step(move || Ok(Some(control.apply_seed(&env, &seed, &clock)?)))
        .await
}
