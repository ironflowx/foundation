//! Two recoveries pass file preflight before either delivers a key (SRV-7).

#[allow(dead_code)]
#[path = "../../ironflowx-operations/tests/leak/mod.rs"]
mod leak;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ironflowx_bin::recover_key;
use ironflowx_core::{EnvRef, ErrorKind, KeyId, KeyOrigin, Secret, set_key_hook};
use ironflowx_operations::{Engine, HostClock};
use ironflowx_proto::{Call, ListKeysRequest, OpError, Operations as _, RevokeKeyRequest};

type Gate = (Sender<(KeyId, Secret)>, Receiver<()>);

static GATES: Mutex<VecDeque<Gate>> = Mutex::new(VecDeque::new());
const LATE: Duration = Duration::from_secs(10);

fn hold_recovery(_: EnvRef, id: KeyId, secret: Option<&Secret>) {
    let gate = GATES.lock().unwrap().pop_front();
    if let Some((reached, release)) = gate {
        reached
            .send((id, Secret::new(secret.unwrap().expose().to_owned())))
            .unwrap();
        release.recv_timeout(LATE).expect("release the recovery");
    }
}

fn recover_at_gate(dir: &Path) -> (JoinHandle<Result<(), OpError>>, KeyId, Secret, Sender<()>) {
    let (tell, reached) = channel();
    let (release, released) = channel();
    GATES.lock().unwrap().push_back((tell, released));
    let dir = dir.to_owned();
    let worker = std::thread::spawn(move || recover_key(&dir, false));
    let (id, secret) = reached
        .recv_timeout(LATE)
        .expect("recovery passed preflight");
    (worker, id, secret, release)
}

#[tokio::test]
async fn the_loser_names_only_its_undelivered_key_and_keeps_the_winners_file() {
    let dir = tempfile::tempdir().unwrap();
    let opened = Engine::open(dir.path(), HostClock, None).await.unwrap();
    let bootstrap = opened.minted.unwrap();
    set_key_hook(hold_recovery);

    // Each hook is reached after destination preflight, before the insert. Only then let a
    // recovery proceed all the way through the insert and the link. No timing assumption.
    let (winner, winner_id, winner_key, go) = recover_at_gate(dir.path());
    let (loser, loser_id, loser_key, follow) = recover_at_gate(dir.path());
    go.send(()).unwrap();
    winner.join().unwrap().unwrap();
    follow.send(()).unwrap();
    let error = loser.join().unwrap().unwrap_err();

    assert_eq!(error.kind, ErrorKind::Internal);
    assert_eq!(error.kind.exit_code(), 1);
    assert!(error.message.contains(&format!(
        "cannot deliver recovered key {loser_id} to admin.key"
    )));
    assert!(!error.message.contains(&winner_id.to_string()));
    let hint = error.hint.unwrap();
    assert!(hint.ends_with(&format!("then revoke {loser_id}")));
    for diagnostic in [&error.message, &hint] {
        assert_eq!(
            leak::leak(diagnostic, [winner_key.expose(), loser_key.expose()]),
            None
        );
    }

    let file = dir.path().join("admin.key");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.strip_suffix('\n') == Some(winner_key.expose()),
        "kept the winner's whole key"
    );
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        assert!(!entry.file_name().as_encoded_bytes().ends_with(b".tmp"));
    }
    let cx = Call::with_key(winner_key);
    let engine = &opened.engine;
    let page = engine
        .list_keys(&cx, ListKeysRequest::default())
        .await
        .unwrap();
    let records: Vec<_> = page
        .keys
        .iter()
        .map(|key| (key.key_id, key.origin))
        .collect();
    assert_eq!(
        records,
        [
            (loser_id, KeyOrigin::Recovered),
            (winner_id, KeyOrigin::Recovered),
            (
                ironflowx_operations::key_id(&bootstrap).unwrap(),
                KeyOrigin::Minted
            ),
        ]
    );
    engine
        .revoke_key(
            &cx,
            RevokeKeyRequest {
                key_id: loser_id.to_string(),
            },
        )
        .await
        .unwrap();
    opened.engine.close().await;
}
