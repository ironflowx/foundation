//! TEN-3, end to end: no secret the engine makes, takes or is shown is stored in its data
//! directory, while it runs or once it is closed, or printed by its process. No published crate
//! depends on a logging one, so what a process prints is all it logs: this test runs itself as a
//! child that plays a host, which prints every answer and error it gets.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use ironflowx_authz::{mint, parse};
use ironflowx_cell::{ManualClock, new_env_ref};
use ironflowx_core::{Clock, Scope, Secret, Timestamp};
use ironflowx_operations::{Engine, EngineError, Opened, recover_admin_key};
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, Operations, RevokeKeyRequest,
    WhoamiRequest,
};
use ironflowx_store::{Bootstrap, ControlPlane};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
/// Where the child writes its secrets for its parent.
const TOLD: &str = "IRONFLOWX_TEST_TOLD_SECRETS";

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

/// The 256 bits a key's secret part encodes.
fn bits(secret: &str) -> Vec<u8> {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let (mut held, mut held_bits, mut out) = (0u32, 0, Vec::new());
    for c in secret.chars() {
        let six = alphabet.find(c).expect("base64url");
        held = held << 6 | u32::try_from(six).unwrap();
        held_bits += 6;
        if held_bits >= 8 {
            held_bits -= 8;
            out.push((held >> held_bits & 0xff) as u8);
        }
    }
    out
}

/// Bytes as a string `str::contains` searches: each byte the character of that number.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}

/// What gives `key`'s secret away, in the clear: either half of its text or of its bits, as bytes,
/// in hex or as `{:?}` prints bytes.
fn needles(key: &str) -> Vec<String> {
    let text = &key[41..];
    let bits = bits(text);
    let mut out = Vec::new();
    for half in [
        &text.as_bytes()[..21],
        &text.as_bytes()[21..],
        &bits[..16],
        &bits[16..],
    ] {
        let hex: String = half.iter().map(|b| format!("{b:02x}")).collect();
        let listed = format!("{half:?}");
        out.extend([latin1(half), hex.to_uppercase(), hex]);
        out.push(listed[1..listed.len() - 1].to_owned());
    }
    out
}

/// Every file under `root`, by its path there. Copied by another process: closing a file this one
/// opened would drop the locks SQLite holds on it here.
fn files(root: &Path) -> Vec<(PathBuf, String)> {
    let copy = tempfile::tempdir().unwrap();
    let copied = copy.path().join("root");
    let cp = Command::new("cp").arg("-R").arg(root).arg(&copied).status();
    assert!(cp.unwrap().success());
    let (mut out, mut stack) = (Vec::new(), vec![copied.clone()]);
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = latin1(&std::fs::read(&path).unwrap());
                out.push((path.strip_prefix(&copied).unwrap().to_owned(), bytes));
            }
        }
    }
    out
}

/// What a host does, and every secret in it.
struct Host {
    root: tempfile::TempDir,
    clock: ManualClock,
    /// Every key's text, and which are stored.
    secrets: Vec<(String, bool)>,
}

impl Host {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            clock: ManualClock::new(at(T)),
            secrets: Vec::new(),
        }
    }

    /// Keeps a secret to search for; its text.
    fn keep(&mut self, key: &Secret, stored: bool) -> String {
        let text = key.expose().to_owned();
        self.secrets.push((text.clone(), stored));
        text
    }

    /// Opens the engine, with `seed` if given, keeping what it returns.
    async fn open(&mut self, seed: Option<&str>) -> Result<(Engine, Option<String>), EngineError> {
        let seed = seed.map(|s| Secret::new(s.to_owned()));
        let opened = Engine::open(self.root.path(), self.clock.clone(), seed).await;
        let Opened {
            engine,
            minted,
            lost_secret,
            seed,
        } = opened.inspect_err(|e| println!("open: {e} {e:?}"))?;
        println!("opened: lost {lost_secret}, seed {seed:?}");
        Ok((engine, minted.map(|key| self.keep(&key, true))))
    }

    /// No file of the root holds a secret. The search does read what is stored: the key stored
    /// last is found by its ID in the WAL while the engine runs, and every stored key's is in
    /// `control.db` once it is closed, with no WAL left.
    fn search(&self, running: bool, case: &str) {
        let files = files(self.root.path());
        for (key, _) in &self.secrets {
            for needle in needles(key) {
                for (path, bytes) in &files {
                    assert!(!bytes.contains(&needle), "{case}: {path:?} holds {key}");
                }
            }
        }
        let file = |name: &str| {
            let found = files.iter().find(|(path, _)| path == Path::new(name));
            found.map(|(_, bytes)| bytes)
        };
        let side = [file("control.db-wal"), file("control.db-shm")];
        let mut stored = self.secrets.iter().filter(|(_, stored)| *stored);
        if running {
            let [Some(wal), Some(shm)] = side else {
                panic!("{case}: {side:?}");
            };
            let (last, _) = stored.next_back().expect("a stored key");
            assert!(wal.contains(&last[4..40]) && !shm.is_empty(), "{case}");
        } else {
            assert_eq!(side, [None, None], "{case}");
            let control = file("control.db").expect("control.db");
            assert!(
                stored.all(|(key, _)| control.contains(&key[4..40])),
                "{case}"
            );
        }
        println!("searched: {case}");
    }
}

/// Every operation with `key`, every answer printed.
async fn every_call(engine: &Engine, key: &str) {
    let cx = || Call::with_key(Secret::new(key.to_owned()));
    println!("{:?}", cx());
    let emit = || EmitRequest {
        name: "order.placed".into(),
        data: None,
        metadata: None,
        version: None,
        idempotency_key: None,
    };
    let emitted = engine.emit(&cx(), emit()).await;
    let event_id = emitted.as_ref().map(|e| e.event_id.to_string());
    println!("{emitted:?}");
    let items = vec![emit()];
    let batch = engine.emit_batch(&cx(), EmitBatchRequest { items }).await;
    println!("{batch:?}");
    let event_id = event_id.unwrap_or_default();
    let got = engine.get_event(&cx(), GetEventRequest { event_id }).await;
    println!("{got:?}");
    let req = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    println!("{:?}", engine.list_events(&cx(), req).await);
    println!("{:?}", engine.whoami(&cx(), WhoamiRequest {}).await);
    let req = ListEnvironmentsRequest {};
    println!("{:?}", engine.list_environments(&cx(), req).await);
    let keys = engine.list_keys(&cx(), ListKeysRequest::default()).await;
    println!("{keys:?}");
    let key_id = "none".into();
    let revoked = engine.revoke_key(&cx(), RevokeKeyRequest { key_id }).await;
    println!("{revoked:?}");
    // Refused, whoever asks: the one secret it makes is nobody's.
    let req = CreateKeyRequest {
        label: Some("\n".into()),
        ..CreateKeyRequest::default()
    };
    println!("{:?}", engine.create_key(&cx(), req).await);
}

/// A key of all three scopes that `by` creates, expiring at `expires_at`.
async fn create(host: &mut Host, engine: &Engine, by: &str, expires_at: Option<i64>) -> String {
    let req = CreateKeyRequest {
        scopes: BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]),
        expires_at: expires_at.map(at),
        label: Some("agent-7".into()),
    };
    let cx = Call::with_key(Secret::new(by.to_owned()));
    let created = engine.create_key(&cx, req).await.unwrap();
    println!("{created:?}");
    host.keep(&created.key, true)
}

/// A key's text, its ID from `n` and its secret 32 distinct bytes from `n` on.
fn key(n: u8) -> Secret {
    let id = format!("019a9f3e-7c2b-7d41-8a3e-2000000000{n:02x}");
    let secret = std::array::from_fn(|i| n.wrapping_add(u8::try_from(i).unwrap()));
    mint(id.parse().unwrap(), secret).0
}

/// One root: bootstrapped, each operation called with valid and malformed keys, keys created,
/// expired, revoked and recovered, then seeds applied and refused.
async fn bootstrapped_root() -> Host {
    let mut host = Host::new();
    let (engine, minted) = host.open(None).await.unwrap();
    let admin = minted.expect("the bootstrap key");
    host.search(true, "bootstrapped");
    every_call(&engine, &admin).await;

    let created = create(&mut host, &engine, &admin, None).await;
    let expiring = create(&mut host, &engine, &created, Some(T + HOUR)).await;
    host.search(true, "created");
    every_call(&engine, &created).await;
    host.clock.set(at(T + HOUR));
    every_call(&engine, &expiring).await;

    // Malformed keys that hold a secret, then a well-formed key no one has, and a wrong secret.
    for text in [
        format!("Bearer {admin}"),
        format!("{admin} "),
        admin[..83].to_owned(),
        admin[4..].to_owned(),
    ] {
        every_call(&engine, &text).await;
    }
    let unknown = host.keep(&key(1), false);
    every_call(&engine, &unknown).await;
    let wrong = format!("{}{}", &admin[..41], &unknown[41..]);
    every_call(&engine, &wrong).await;
    host.search(true, "called");

    let recovered = recover_admin_key(host.root.path(), host.clock.clone()).unwrap();
    let recovered = host.keep(&recovered, true);
    every_call(&engine, &recovered).await;
    let cx = Call::with_key(Secret::new(recovered));
    let key_id = admin[4..40].to_owned();
    let revoked = engine.revoke_key(&cx, RevokeKeyRequest { key_id }).await;
    revoked.expect("revoked");
    every_call(&engine, &admin).await;
    host.search(true, "recovered and revoked");
    engine.close().await;
    host.search(false, "closed");

    let a = host.keep(&key(2), true);
    let malformed = &a[..83];
    assert!(host.open(Some(malformed)).await.is_err());
    let (engine, _) = host.open(Some(&a)).await.unwrap();
    every_call(&engine, &a).await;
    host.search(true, "seeded");
    engine.close().await;
    host.search(false, "closed after a seed");

    let conflicting = host.keep(&key(4), false);
    let conflicting = format!("{}{}", &a[..41], &conflicting[41..]);
    assert!(host.open(Some(&conflicting)).await.is_err());
    let b = host.keep(&key(3), true);
    let (engine, _) = host.open(Some(&b)).await.unwrap();
    every_call(&engine, &a).await;
    host.search(true, "rotated");
    engine.close().await;
    host.search(false, "closed after a rotation");
    host
}

/// A root bootstrapped by a seed.
async fn seeded_root() -> Host {
    let mut host = Host::new();
    let seed = host.keep(&key(5), true);
    let (engine, _) = host.open(Some(&seed)).await.unwrap();
    every_call(&engine, &seed).await;
    host.search(true, "bootstrapped by a seed");
    engine.close().await;
    host.search(false, "closed, bootstrapped by a seed");
    host
}

/// A root whose first open died before its mark: the next one replaces the minted key.
async fn finished_root() -> Host {
    let mut host = Host::new();
    let lost = key(6);
    let bootstrap = Bootstrap {
        env: new_env_ref(),
        tenant: "019a9f3e-7c2b-7d41-8a3e-100000000001".parse().unwrap(),
        project: "019a9f3e-7c2b-7d41-8a3e-100000000002".parse().unwrap(),
        key: parse(&lost).unwrap(),
        by_seed: false,
    };
    let control = ControlPlane::open(&host.root.path().join("control.db")).unwrap();
    let clock: &dyn Clock = &host.clock;
    control.ensure_bootstrap(&bootstrap, clock).unwrap();
    drop(control);
    host.keep(&lost, true);
    let (engine, replacement) = host.open(None).await.unwrap();
    every_call(&engine, &replacement.expect("a replacement")).await;
    host.search(true, "finished");
    engine.close().await;
    host.search(false, "closed, finished");
    host
}

/// What the child runs: every secret to `told`, once nothing more is printed.
async fn host(told: &Path) {
    let mut text = String::new();
    for host in [
        bootstrapped_root().await,
        seeded_root().await,
        finished_root().await,
    ] {
        for (key, _) in host.secrets {
            text += &format!("{key}\n");
        }
    }
    std::fs::write(told, text).unwrap();
}

#[test]
fn no_secret_is_stored_or_logged() {
    if let Some(told) = std::env::var_os(TOLD) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        return rt.block_on(host(Path::new(&told)));
    }
    // The search finds each form of a secret.
    let probe = key(7);
    let (text, raw) = (&probe.expose()[41..], bits(&probe.expose()[41..]));
    assert_eq!(raw, (7..39).collect::<Vec<u8>>());
    for form in [
        text.to_owned(),
        latin1(&raw),
        format!("{raw:?}"),
        format!("{raw:02x?}").replace([',', ' '], ""),
        format!("{raw:02X?}").replace([',', ' '], ""),
        format!("{:?}", text.as_bytes()),
    ] {
        assert!(needles(probe.expose()).iter().any(|n| form.contains(n)));
    }

    let dir = tempfile::tempdir().unwrap();
    let told = dir.path().join("told");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "no_secret_is_stored_or_logged", "--nocapture"])
        .env(TOLD, &told)
        .env("RUST_BACKTRACE", "full")
        .env("RUST_LOG", "trace")
        .output()
        .unwrap();
    let logs = format!("{}{}", latin1(&child.stdout), latin1(&child.stderr));
    assert!(child.status.success(), "{logs}");
    // The logs are the host's: what it printed of each root, and of each refusal.
    for printed in [
        "searched: closed after a rotation",
        "searched: closed, bootstrapped by a seed",
        "searched: closed, finished",
        "invalid API key",
        "malformed seed",
        "the seed's key ID is already another key",
    ] {
        assert!(logs.contains(printed), "{printed}");
    }
    let told = std::fs::read_to_string(told).unwrap();
    // Of the bootstrapped root, the seeded one, and the finished one.
    assert_eq!(told.lines().count(), 8 + 1 + 2);
    for key in told.lines() {
        for needle in needles(key) {
            assert!(!logs.contains(&needle), "the logs hold {key}");
        }
    }
}
