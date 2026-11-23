//! A key change re-checks its calling key inside its own transaction (D43, TEN-2): key K is
//! admitted, then revoked, or the clock passes its expiry, before its `create_key` or
//! `revoke_key` transaction starts. The change fails and changes nothing, except that K's
//! expiry, if K was not revoked, is recorded (D42).
//!
//! The barrier: another connection holds SQLite's write lock, with K's revoke, if any, written
//! but not committed, so admission still reads K as valid; the clock signals admission's
//! reading, and may move on; the other connection then commits, and the change's transaction,
//! waiting on the lock, starts after it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use ironflowx_core::{Clock, ErrorKind, KeyId, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{Call, CreateKeyRequest, OpError, RevokeKeyRequest};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

/// A clock that reads what it was set to and, once armed, tells a channel about each reading.
#[derive(Clone)]
struct Signalling(Arc<Mutex<(i64, Option<Sender<()>>)>>);

impl Signalling {
    fn arm(&self) -> Receiver<()> {
        let (tx, rx) = channel();
        self.0.lock().unwrap().1 = Some(tx);
        rx
    }

    fn set(&self, ms: i64) {
        self.0.lock().unwrap().0 = ms;
    }
}

impl Clock for Signalling {
    fn now(&self) -> Timestamp {
        let (ms, armed) = &*self.0.lock().unwrap();
        if let Some(tx) = armed {
            let _ = tx.send(());
        }
        Timestamp::from_millis(*ms)
    }
}

struct Setup {
    engine: Engine,
    clock: Signalling,
    /// K, an admin key expiring at T + 1 h, and another key for K to revoke.
    k: String,
    k_id: KeyId,
    other_id: KeyId,
}

async fn setup(dir: &Path) -> Setup {
    let clock = Signalling(Arc::new(Mutex::new((T, None))));
    let opened = Engine::open(dir, clock.clone()).await.unwrap();
    let admin = Call::with_key(opened.minted.unwrap());
    let mut made = Vec::new();
    for scopes in [vec![Scope::Admin, Scope::Read], vec![Scope::Read]] {
        let req = CreateKeyRequest {
            scopes: scopes.into_iter().collect(),
            expires_at: Some(Timestamp::from_millis(T + HOUR)),
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

/// What happens to K between admission and its change's transaction.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Meanwhile {
    Revoked,
    Expired,
    RevokedAndExpired,
}

const EVERY: [Meanwhile; 3] = [
    Meanwhile::Revoked,
    Meanwhile::Expired,
    Meanwhile::RevokedAndExpired,
];

/// Every key's ID, revoke time, recorded expiry, and other columns as one text, in order.
fn rows(dir: &Path) -> Vec<(String, Option<i64>, bool, String)> {
    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT id, revoked_at, expiry_recorded, concat_ws('|', seq, environment_id,
                environment_token, scopes, hex(digest), origin, created_by, label, expires_at,
                created_at)
             FROM keys ORDER BY seq",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)));
    rows.unwrap().map(Result::unwrap).collect()
}

/// Runs `change` as K, with `meanwhile` happening to K once admission has read the clock and
/// before the change's transaction: the change is refused, and the rows hold only what happened
/// and, for a K that only expired, its recorded expiry.
async fn assert_refused<F: Future<Output = Result<(), OpError>>>(
    dir: &Path,
    s: &Setup,
    meanwhile: Meanwhile,
    change: impl FnOnce(Call) -> F,
) {
    let (revoked, expired) = (
        meanwhile != Meanwhile::Expired,
        meanwhile != Meanwhile::Revoked,
    );
    let mut expected = rows(dir);
    let k = expected
        .iter_mut()
        .find(|(id, ..)| *id == s.k_id.to_string())
        .unwrap();
    assert_eq!((k.1, k.2), (None, false), "K, before");
    k.1 = revoked.then_some(T);
    k.2 = !revoked;

    let db = rusqlite::Connection::open(dir.join("control.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    if revoked {
        db.execute(
            "UPDATE keys SET revoked_at = ?1 WHERE id = ?2",
            rusqlite::params![T, s.k_id.to_string()],
        )
        .unwrap();
    }
    let admitted = s.clock.arm();
    let clock = s.clock.clone();
    let committer = std::thread::spawn(move || {
        admitted.recv().expect("admission reads the clock");
        if expired {
            clock.set(T + HOUR);
        }
        db.execute_batch("COMMIT").unwrap();
    });
    let answer = change(Call::with_key(Secret::new(s.k.clone()))).await;
    committer.join().unwrap();

    let err = answer.expect_err("K is invalid by the change's transaction");
    assert_eq!(
        (err.kind, err.message.as_str(), err.hint),
        (ErrorKind::Unauthenticated, "invalid API key", None),
        "{meanwhile:?}"
    );
    assert_eq!(rows(dir), expected, "{meanwhile:?}");
}

#[tokio::test]
async fn a_key_revoked_or_expired_after_admission_creates_no_key() {
    for meanwhile in EVERY {
        let dir = tempfile::tempdir().unwrap();
        let s = setup(dir.path()).await;
        assert_refused(dir.path(), &s, meanwhile, async |k| {
            let req = CreateKeyRequest {
                scopes: BTreeSet::from([Scope::Read]),
                // Within K and after T: nothing but K's own state refuses it at T.
                expires_at: Some(Timestamp::from_millis(T + HOUR)),
                ..CreateKeyRequest::default()
            };
            s.engine.create_key(&k, req).await.map(drop)
        })
        .await;
        s.engine.close().await;
    }
}

#[tokio::test]
async fn a_key_revoked_or_expired_after_admission_revokes_no_key() {
    for meanwhile in EVERY {
        let dir = tempfile::tempdir().unwrap();
        let s = setup(dir.path()).await;
        assert_refused(dir.path(), &s, meanwhile, async |k| {
            let req = RevokeKeyRequest {
                key_id: s.other_id.to_string(),
            };
            s.engine.revoke_key(&k, req).await
        })
        .await;
        s.engine.close().await;
    }
}
