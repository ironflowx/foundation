//! A key change re-checks its calling key inside its own transaction (D43, TEN-2): key K is
//! admitted, then revoked before its `create_key` or `revoke_key` transaction starts, and the
//! change fails, changing nothing.
//!
//! The barrier: another connection holds SQLite's write lock with K's revoke written but not
//! committed, so admission still reads K as valid; the clock signals admission's reading; the
//! revoke then commits, and the change's transaction, waiting on the lock, starts after it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use ironflowx_core::{Clock, ErrorKind, KeyId, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, CreateKeyRequest, OpError, RevokeKeyRequest};

const T: i64 = 1_800_000_000_000;

/// A fixed clock that, once armed, tells a channel about each reading.
#[derive(Clone, Default)]
struct Signalling(Arc<Mutex<Option<Sender<()>>>>);

impl Signalling {
    fn arm(&self) -> Receiver<()> {
        let (tx, rx) = channel();
        *self.0.lock().unwrap() = Some(tx);
        rx
    }
}

impl Clock for Signalling {
    fn now(&self) -> Timestamp {
        if let Some(tx) = &*self.0.lock().unwrap() {
            let _ = tx.send(());
        }
        Timestamp::from_millis(T)
    }
}

struct Setup {
    engine: Engine,
    clock: Signalling,
    /// K, an admin key, and another key for K to revoke.
    k: String,
    k_id: KeyId,
    other_id: KeyId,
}

async fn setup(dir: &Path) -> Setup {
    let clock = Signalling::default();
    let opened = Engine::open(dir, clock.clone()).await.unwrap();
    let admin = Call::with_key(opened.minted.unwrap());
    let mut made = Vec::new();
    for scopes in [vec![Scope::Admin, Scope::Read], vec![Scope::Read]] {
        let req = CreateKeyRequest {
            scopes: scopes.into_iter().collect(),
            ..CreateKeyRequest::default()
        };
        made.push(opened.engine.create_key(&admin, req).await.unwrap());
    }
    Setup {
        engine: opened.engine,
        clock,
        k: made[0].key.expose().to_owned(),
        k_id: made[0].info.key_id,
        other_id: made[1].info.key_id,
    }
}

/// Runs `change` as K while K's revoke waits, uncommitted, behind the write lock; commits it
/// once admission has read the clock. The change's answer.
async fn with_k_revoked_after_admission<F: Future<Output = Result<(), OpError>>>(
    dir: &Path,
    s: &Setup,
    change: impl FnOnce(Call) -> F,
) -> Result<(), OpError> {
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute(
        "UPDATE keys SET revoked_at = ?1 WHERE id = ?2",
        rusqlite::params![T, s.k_id.to_string()],
    )
    .unwrap();
    let admitted = s.clock.arm();
    let committer = std::thread::spawn(move || {
        admitted.recv().expect("admission reads the clock");
        db.execute_batch("COMMIT").unwrap();
    });
    let answer = change(Call::with_key(Secret::new(s.k.clone()))).await;
    committer.join().unwrap();
    answer
}

fn count(dir: &Path, sql: &str) -> i64 {
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn assert_refused(answer: Result<(), OpError>) {
    let err = answer.expect_err("K was revoked before the change's transaction");
    assert_eq!(
        (err.kind, err.message.as_str(), err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", None)
    );
}

#[tokio::test]
async fn a_key_revoked_after_admission_creates_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path()).await;
    let before = count(dir.path(), "SELECT count(*) FROM keys");
    let answer = with_k_revoked_after_admission(dir.path(), &s, async |k| {
        let req = CreateKeyRequest {
            scopes: BTreeSet::from([Scope::Read]),
            ..CreateKeyRequest::default()
        };
        s.engine.create_key(&k, req).await.map(drop)
    })
    .await;
    assert_refused(answer);
    assert_eq!(count(dir.path(), "SELECT count(*) FROM keys"), before);
    s.engine.close().await;
}

#[tokio::test]
async fn a_key_revoked_after_admission_revokes_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path()).await;
    let answer = with_k_revoked_after_admission(dir.path(), &s, async |k| {
        let req = RevokeKeyRequest {
            key_id: s.other_id.to_string(),
        };
        s.engine.revoke_key(&k, req).await
    })
    .await;
    assert_refused(answer);
    let other = format!(
        "SELECT count(*) FROM keys WHERE id = '{}' AND revoked_at IS NULL",
        s.other_id
    );
    assert_eq!(count(dir.path(), &other), 1, "the other key is not revoked");
    s.engine.close().await;
}
