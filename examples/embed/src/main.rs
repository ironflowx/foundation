//! Bootstraps an engine with a seed it wrote to disk first, emits one event with it, creates a
//! read-only key, reads the event back with it and sees its emit refused; then restarts the engine
//! and reads the event back with the seed from disk, using only `ironflowx` and `tokio`. Unix only:
//! the seed file is written with mode 0600.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind as IoErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use ironflowx::cell::{HostClock, new_env_ref};
use ironflowx::core::{Clock, ErrorKind, Scope, Secret};
use ironflowx::operations::{Engine, SeedState, generate_seed};
use ironflowx::proto::{
    Call, CreateKeyRequest, EmitRequest, Event, GetEventRequest, OpError, Operations,
};

/// The app's seed, read from `path`. On the first start it is generated and on disk, synced,
/// before any open sees it, so no open can make a key the app then loses.
fn seed(path: &Path) -> Secret {
    let dir = path.parent().expect("the seed's directory");
    if !path.exists() {
        let temp = dir.join(format!("seed.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .expect("the temporary seed file is created");
        file.write_all(generate_seed().1.expose().as_bytes())
            .expect("the seed is written");
        file.sync_all().expect("the seed is synced");
        // A link, unlike a rename, never replaces a seed another start already put there.
        match std::fs::hard_link(&temp, path) {
            Ok(()) => {}
            Err(e) if e.kind() == IoErrorKind::AlreadyExists => {}
            Err(e) => panic!("the seed is linked: {e}"),
        }
        std::fs::remove_file(&temp).expect("the temporary seed file is removed");
        File::open(dir)
            .and_then(|d| d.sync_all())
            .expect("the seed's directory is synced");
    }
    Secret::new(std::fs::read_to_string(path).expect("the seed is read"))
}

fn main() {
    let dir = std::env::temp_dir().join(format!("ironflowx-embed-{}", new_env_ref()));
    std::fs::create_dir(&dir).expect("the app's directory is created");
    // The seed lives beside the engine's root, not in it.
    let seed_path = dir.join("seed");
    let root = dir.join("engine");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let opened = Engine::open(&root, HostClock, Some(seed(&seed_path)))
            .await
            .expect("the engine opens");
        assert!(opened.minted.is_none(), "a seeded bootstrap mints no key");
        assert_eq!(opened.seed, Some(SeedState::Current));
        let engine = opened.engine;
        // The seed is the bootstrap key.
        let admin = Call::with_key(seed(&seed_path));
        let order = EmitRequest {
            name: "order.placed".to_owned(),
            data: Some(br#"{"total":42}"#.to_vec()),
            metadata: Some(br#"{"source":"embed"}"#.to_vec()),
            version: None,
            idempotency_key: Some("order-1".to_owned()),
        };
        let before = HostClock.now();
        let emitted = engine
            .emit(&admin, order.clone())
            .await
            .expect("the emit is acknowledged");
        let after = HostClock.now();
        assert!(!emitted.duplicate, "a new key stores a new event");
        // No scopes asked for: the key holds `read` alone.
        let created = engine
            .create_key(&admin, CreateKeyRequest::default())
            .await
            .expect("the bootstrap key creates a key");
        assert_eq!(created.info.scopes, BTreeSet::from([Scope::Read]));
        let reader = Call::with_key(created.key);
        let event = engine
            .get_event(
                &reader,
                GetEventRequest {
                    event_id: emitted.event_id.to_string(),
                },
            )
            .await
            .expect("the event reads back");
        assert!(
            (before..=after).contains(&event.time),
            "the event takes the host clock's time"
        );
        let bootstrap = created.info.created_by.expect("the creating key");
        assert_eq!(
            event,
            Event {
                event_id: emitted.event_id,
                name: "order.placed".to_owned(),
                data: br#"{"total":42}"#.to_vec(),
                metadata: Some(br#"{"source":"embed"}"#.to_vec()),
                version: 1,
                idempotency_key: Some("order-1".to_owned()),
                time: event.time,
                emitted_by: format!("key:{bootstrap}").parse().expect("an emitter"),
            }
        );
        let refused = engine
            .emit(&reader, order)
            .await
            .expect_err("a `read` key cannot emit");
        assert_eq!(
            refused,
            OpError {
                kind: ErrorKind::PermissionDenied,
                message: "missing scope: write".to_owned(),
                hint: Some("create a key with scope write, or use one that has it".to_owned()),
            }
        );
        engine.close().await;

        // A restart: the seed comes from disk, and still works.
        let opened = Engine::open(&root, HostClock, Some(seed(&seed_path)))
            .await
            .expect("the engine opens again");
        assert!(opened.minted.is_none(), "a later open mints no key");
        assert_eq!(opened.seed, Some(SeedState::Current));
        let engine = opened.engine;
        let again = engine
            .get_event(
                &Call::with_key(seed(&seed_path)),
                GetEventRequest {
                    event_id: emitted.event_id.to_string(),
                },
            )
            .await
            .expect("the seed reads the event after the restart");
        assert_eq!(again, event);
        engine.close().await;
    });
    std::fs::remove_dir_all(&dir).expect("the app's directory is removed");
    println!(
        "embed: bootstrapped an engine with a seed, emitted one event, read it back with a read key \
         and saw that key's emit refused, then restarted it and read the event with the seed"
    );
}
