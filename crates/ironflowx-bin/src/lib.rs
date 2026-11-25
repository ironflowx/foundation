//! The host: opens an engine on a data directory, serves it and answers for how it is
//! (architecture.md, "Host").

#![forbid(unsafe_code)]

mod accept;
mod key_file;

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ironflowx_cell::{HostClock, LedgerState};
use ironflowx_core::ErrorKind;
use ironflowx_doors::{Readiness, Slot, WaitingOn, router};
use ironflowx_operations::{ControlPlaneState, Engine, EngineError, Opened};
use ironflowx_store::{BootstrapState, ControlPlane};

/// Why a host did not serve.
#[derive(Debug)]
pub enum ServeError {
    /// The data directory holds an `admin.key` that no finished bootstrap of it wrote.
    StaleKeyFile(PathBuf),
    Listen(io::Error),
    DataDir(PathBuf, io::Error),
    Engine(EngineError),
    /// The engine opened and minted a key, which could not be written to `admin.key`.
    KeyFile(PathBuf, io::Error),
}

impl ServeError {
    /// The process's exit code.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::StaleKeyFile(_) => ErrorKind::Invalid,
            Self::Listen(_) | Self::DataDir(..) | Self::Engine(_) | Self::KeyFile(..) => {
                ErrorKind::Internal
            }
        }
        .exit_code()
    }
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleKeyFile(path) => write!(
                f,
                "{} is another install's: no engine of this data directory wrote it",
                path.display()
            ),
            Self::Listen(e) => write!(f, "cannot listen: {e}"),
            Self::DataDir(path, e) => write!(f, "cannot create {}: {e}", path.display()),
            Self::Engine(e) => e.fmt(f),
            Self::KeyFile(path, e) => write!(f, "cannot write {}: {e}", path.display()),
        }
    }
}

impl std::error::Error for ServeError {}

/// Listens on `listen`, opens the engine in `data_dir` and serves it, until it cannot.
///
/// # Errors
///
/// [`ServeError`], before anything is served or the data directory changed for a stale
/// `admin.key` without a `control.db` and for an address it cannot listen on.
pub async fn serve(data_dir: &Path, listen: SocketAddr) -> Result<Infallible, ServeError> {
    let listener = bind(data_dir, listen)?;
    run(listener, data_dir).await
}

/// The steps before the data directory is touched (D62).
fn bind(data_dir: &Path, listen: SocketAddr) -> Result<TcpListener, ServeError> {
    if key_file::exists(data_dir) && !data_dir.join("control.db").exists() {
        return Err(ServeError::StaleKeyFile(key_file::path(data_dir)));
    }
    let listener = TcpListener::bind(listen)
        .map_err(|e| ServeError::Listen(io::Error::new(e.kind(), format!("{listen}: {e}"))))?;
    let bound = listener.local_addr().map_err(ServeError::Listen)?;
    tracing::info!(url = format!("http://{bound}"), "listening");
    if !bound.ip().is_loopback() {
        tracing::warn!("keys travel in clear without a TLS proxy");
    }
    Ok(listener)
}

/// Serves `/health` and the door at once, and the engine in `data_dir` when it has opened.
///
/// # Errors
///
/// As [`serve`].
pub async fn run(listener: TcpListener, data_dir: &Path) -> Result<Infallible, ServeError> {
    let listener = listener
        .set_nonblocking(true)
        .and_then(|()| tokio::net::TcpListener::from_std(listener))
        .map_err(ServeError::Listen)?;
    let slot = Arc::new(Slot::starting());
    let accepting = tokio::spawn(accept::connections(listener, router(slot.clone())));
    let engine = Arc::new(open(data_dir).await?);
    slot.open(engine.clone(), EngineReadiness(engine));
    match accepting.await {
        Ok(never) => match never {},
        Err(panic) => std::panic::resume_unwind(panic.into_panic()),
    }
}

/// Opens the engine in `data_dir`, creating it if it is missing, and writes a key it minted.
async fn open(data_dir: &Path) -> Result<Engine, ServeError> {
    let dir = data_dir.to_owned();
    blocking(move || prepare(&dir)).await?;
    let Opened { engine, minted, .. } = Engine::open(data_dir, HostClock, None)
        .await
        .map_err(ServeError::Engine)?;
    if let Some(key) = minted {
        let dir = data_dir.to_owned();
        let path = key_file::path(data_dir);
        match blocking(move || key_file::write(&dir, &key)).await {
            Ok(()) => tracing::info!(path = %path.display(), "the admin key is written"),
            Err(e) => {
                engine.close().await;
                return Err(ServeError::KeyFile(path, e));
            }
        }
    }
    Ok(engine)
}

/// Creates the data directory, and refuses an `admin.key` that no open of it wrote: one writes it
/// only once the bootstrap is marked.
fn prepare(data_dir: &Path) -> Result<(), ServeError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data_dir)
        .map_err(|e| ServeError::DataDir(data_dir.to_owned(), e))?;
    if key_file::exists(data_dir) {
        match ControlPlane::bootstrap_state(&data_dir.join("control.db")) {
            // The open answers what a failed read means.
            Ok(BootstrapState::Marked { .. }) | Err(_) => {}
            Ok(
                BootstrapState::NoFile
                | BootstrapState::Unbootstrapped
                | BootstrapState::Unmarked { .. },
            ) => return Err(ServeError::StaleKeyFile(key_file::path(data_dir))),
        }
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("the host's blocking steps do not panic")
}

/// An open engine, as `/ready` sees it.
struct EngineReadiness(Arc<Engine>);

impl Readiness for EngineReadiness {
    async fn ready(&self) -> Result<(), WaitingOn> {
        let health = self.0.health().await;
        match (health.control_plane, health.ledger) {
            (ControlPlaneState::Failed, _) => Err(WaitingOn::ControlPlane),
            (ControlPlaneState::Ok, LedgerState::Unavailable) => Err(WaitingOn::Ledger),
            (ControlPlaneState::Ok, LedgerState::Open) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ironflowx_proto::{Call, EmitRequest, ListEnvironmentsRequest, Operations as _};

    use super::*;

    /// SRV-9: each part of the engine's health that keeps `/ready` from 200.
    #[tokio::test]
    async fn readiness_is_the_engines_health() {
        let dir = tempfile::tempdir().unwrap();
        let opened = Engine::open(dir.path(), HostClock, None).await.unwrap();
        let cx = Call::with_key(opened.minted.unwrap());
        let engine = Arc::new(opened.engine);
        let readiness = EngineReadiness(engine.clone());
        assert_eq!(readiness.ready().await, Ok(()));

        let control = rusqlite::Connection::open(dir.path().join("control.db")).unwrap();
        let gone = "ALTER TABLE environments RENAME TO gone";
        let back = "ALTER TABLE gone RENAME TO environments";
        control.execute_batch(gone).unwrap();
        assert_eq!(readiness.ready().await, Err(WaitingOn::ControlPlane));
        control.execute_batch(back).unwrap();
        assert_eq!(readiness.ready().await, Ok(()));

        // The fold of the next event fails, and so the ledger.
        let listed = engine.list_environments(&cx, ListEnvironmentsRequest {});
        let env = listed.await.unwrap().remove(0).env;
        let ledger = dir.path().join("ledgers").join(env.id().to_string());
        let trigger =
            "CREATE TRIGGER t BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'x'); END";
        let read = rusqlite::Connection::open(ledger.join("read.db")).unwrap();
        read.execute_batch(trigger).unwrap();
        let faulted = engine.faulted();
        let event = EmitRequest {
            name: "order.placed".into(),
            data: None,
            metadata: None,
            version: None,
            idempotency_key: None,
        };
        engine.emit(&cx, event).await.unwrap();
        let late = Duration::from_secs(10);
        tokio::time::timeout(late, faulted).await.expect("faulted");
        assert_eq!(readiness.ready().await, Err(WaitingOn::Ledger));
        control.execute_batch(gone).unwrap();
        assert_eq!(readiness.ready().await, Err(WaitingOn::ControlPlane));
    }
}
