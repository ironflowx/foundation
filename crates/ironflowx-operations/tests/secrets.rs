//! TEN-3, end to end: no secret an engine makes, is given or is shown is stored in its data
//! directory, while it runs or once it is closed, or printed by its process. The key hook
//! (`__fault-points`) tells even of a key the engine makes and returns to no one. No published
//! crate depends on a logging one (`cargo tree`, run for #101), so what a process prints is all it
//! logs: this test runs itself as a child that plays a host, which prints every answer and error it
//! gets.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use ironflowx_authz::{mint, parse};
use ironflowx_cell::{ManualClock, new_env_ref};
use ironflowx_core::{Clock, EnvRef, ErrorKind, KeyId, Scope, Secret, Timestamp, set_key_hook};
use ironflowx_operations::{Engine, EngineError, Opened, recover_admin_key};
use ironflowx_proto::{
    Call, CreateKeyRequest, EmitBatchRequest, EmitRequest, GetEventRequest,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operations,
    RevokeKeyRequest, WhoamiRequest,
};
use ironflowx_store::{Bootstrap, ControlPlane};

const T: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
/// Where the child writes its secrets for its parent.
const TOLD: &str = "IRONFLOWX_TEST_TOLD_SECRETS";

const fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

/// The text of every key the engine made that it told the hook of.
static MINTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn minted(_: EnvRef, _: KeyId, secret: Option<&Secret>) {
    if let Some(secret) = secret {
        println!("{}", announced(secret.expose()));
        MINTED.lock().unwrap().push(secret.expose().to_owned());
    }
}

/// A key as the child announces it to its parent: its ID and a hash of its text.
fn announced(key: &str) -> String {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    format!("a key: {} {:016x}", &key[4..40], hash.finish())
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

/// The halves of a key's secret: of its text, and of the 256 bits that encodes. Half of it is
/// enough to matter.
fn halves(key: &str) -> [Vec<u8>; 4] {
    let (text, bits) = (&key.as_bytes()[41..], bits(&key[41..]));
    let (a, b) = text.split_at(21);
    let (c, d) = bits.split_at(16);
    [a, b, c, d].map(<[u8]>::to_vec)
}

/// How bytes are written in the clear: as they are, in hex, or as a list, as `{:?}` and its hex
/// forms print one.
fn written(bytes: &[u8]) -> Vec<String> {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut out = vec![latin1(bytes), hex.to_uppercase(), hex];
    for list in [
        format!("{bytes:?}"),
        format!("{bytes:x?}"),
        format!("{bytes:X?}"),
        format!("{bytes:02x?}"),
        format!("{bytes:02X?}"),
    ] {
        out.push(list[1..list.len() - 1].to_owned());
    }
    out
}

/// What gives a secret away in `text`: half of one of `keys`, or the start of any key's text.
fn leak<'a>(text: &str, keys: impl IntoIterator<Item = &'a str>) -> Option<String> {
    // `{:#?}` writes a list a line each.
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let holds = |w: &String| text.contains(w) || one_line.contains(w);
    for key in keys {
        for half in halves(key) {
            let found = written(&half).into_iter().find(holds);
            if found.is_some() {
                return found;
            }
        }
    }
    // The key format a hint names goes on with `<`.
    let mut starts = text.match_indices("ixk_");
    let start = starts.find(|(at, _)| text[at + 4..].starts_with(|c: char| c.is_ascii_hexdigit()));
    start.map(|_| "a key's text".to_owned())
}

/// Every file and directory under `root`, by its path there, a directory empty. Copied by another
/// process: closing a file this one opened would drop the locks SQLite holds on it here.
fn files(root: &Path) -> Vec<(PathBuf, String)> {
    let copy = tempfile::tempdir().unwrap();
    let copied = copy.path().join("root");
    let cp = Command::new("cp").arg("-R").arg(root).arg(&copied).status();
    assert!(cp.unwrap().success());
    let (mut out, mut stack) = (Vec::new(), vec![copied.clone()]);
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let mut bytes = String::new();
            if path.is_dir() {
                stack.push(path.clone());
            } else {
                bytes = latin1(&std::fs::read(&path).unwrap());
            }
            out.push((path.strip_prefix(&copied).unwrap().to_owned(), bytes));
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
        println!("{}", announced(&text));
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

    /// Every secret so far: the host's, and those the engine made.
    fn all(&self) -> BTreeSet<String> {
        let minted = MINTED.lock().unwrap().clone();
        let kept = self.secrets.iter().map(|(key, _)| key.clone());
        kept.chain(minted).collect()
    }

    /// No file of the root, or any path in it, holds a secret. The search does read what is
    /// stored: the key stored last is found by its ID in the WAL while the engine runs, and every
    /// stored key's is in `control.db` once it is closed, with no WAL left.
    fn search(&self, running: bool, case: &str) {
        let files = files(self.root.path());
        let secrets = self.all();
        for (path, bytes) in &files {
            let text = format!("{}\n{bytes}", path.display());
            let held = leak(&text, secrets.iter().map(String::as_str));
            assert_eq!(held, None, "{case}: {path:?} holds a secret");
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
            assert!(wal.contains(&last[4..40]), "{case}: the WAL is not read");
            assert!(!shm.is_empty(), "{case}: the shared memory is not read");
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

/// Every operation with `key`, a key of all three scopes if it `works`, every answer printed and
/// as expected.
async fn every_call(engine: &Engine, key: &str, works: bool) {
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
    let event_id = event_id.unwrap_or_default();
    let items = vec![emit()];
    let batch = engine.emit_batch(&cx(), EmitBatchRequest { items }).await;
    let got = engine.get_event(&cx(), GetEventRequest { event_id }).await;
    let req = ListEventsRequest {
        limit: None,
        cursor: None,
    };
    let events = engine.list_events(&cx(), req).await;
    let me = engine.whoami(&cx(), WhoamiRequest {}).await;
    let req = ListEnvironmentsRequest {};
    let envs = engine.list_environments(&cx(), req).await;
    let keys = engine.list_keys(&cx(), ListKeysRequest::default()).await;
    let key_id = "none".into();
    let revoked = engine.revoke_key(&cx(), RevokeKeyRequest { key_id }).await;
    // Refused, whoever asks, its key made all the same if the caller is admitted.
    let req = CreateKeyRequest {
        label: Some("\n".into()),
        ..CreateKeyRequest::default()
    };
    let created = engine.create_key(&cx(), req).await;
    println!("{emitted:?} {batch:?} {got:?} {events:?} {me:?} {envs:?} {keys:?}");
    println!("{revoked:?} {created:?}");

    // The kind, the message, and whether there is a hint; `None` for a success.
    type Answer = Option<(ErrorKind, String, bool)>;
    let of = |e: OpError| (e.kind, e.message, e.hint.is_some());
    let answers: [Answer; 9] = [
        emitted.err().map(of),
        batch.err().map(of),
        got.err().map(of),
        events.err().map(of),
        me.err().map(of),
        envs.err().map(of),
        keys.err().map(of),
        revoked.err().map(of),
        created.err().map(of),
    ];
    let mut expected: [Answer; 9] = Default::default();
    if works {
        let not_found = "key not found".to_owned();
        let label = "a label is at most 128 bytes, with no control character".to_owned();
        expected[7] = Some((ErrorKind::NotFound, not_found, false));
        expected[8] = Some((ErrorKind::Invalid, label, false));
    } else {
        // Only a malformed key's refusal hints.
        let malformed = parse(&Secret::new(key.to_owned())).is_err();
        let message = "invalid API key".to_owned();
        expected.fill(Some((ErrorKind::Unauthenticated, message, malformed)));
    }
    assert_eq!(answers, expected);
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
    every_call(&engine, &admin, true).await;

    let created = create(&mut host, &engine, &admin, None).await;
    let expiring = create(&mut host, &engine, &created, Some(T + HOUR)).await;
    host.search(true, "created");
    every_call(&engine, &created, true).await;
    host.clock.set(at(T + HOUR));
    every_call(&engine, &expiring, false).await;

    // Malformed keys that hold a secret, then a well-formed key no one has, and a wrong secret.
    for text in [
        format!("Bearer {admin}"),
        format!("{admin} "),
        admin[..83].to_owned(),
        admin[4..].to_owned(),
    ] {
        every_call(&engine, &text, false).await;
    }
    let unknown = host.keep(&key(1), false);
    every_call(&engine, &unknown, false).await;
    let wrong = format!("{}{}", &admin[..41], &unknown[41..]);
    every_call(&engine, &wrong, false).await;
    host.search(true, "called");

    let recovered = recover_admin_key(host.root.path(), host.clock.clone()).unwrap();
    let recovered = host.keep(&recovered, true);
    every_call(&engine, &recovered, true).await;
    let cx = Call::with_key(Secret::new(recovered));
    let key_id = admin[4..40].to_owned();
    let revoked = engine.revoke_key(&cx, RevokeKeyRequest { key_id }).await;
    revoked.expect("revoked");
    every_call(&engine, &admin, false).await;
    host.search(true, "recovered and revoked");
    engine.close().await;
    host.search(false, "closed");

    let a = host.keep(&key(2), true);
    let malformed = &a[..83];
    let refused = host.open(Some(malformed)).await.err();
    assert!(matches!(refused, Some(EngineError::MalformedSeed)));
    let (engine, _) = host.open(Some(&a)).await.unwrap();
    every_call(&engine, &a, true).await;
    host.search(true, "seeded");
    engine.close().await;
    host.search(false, "closed after a seed");

    let conflicting = host.keep(&key(4), false);
    let conflicting = format!("{}{}", &a[..41], &conflicting[41..]);
    let refused = host.open(Some(&conflicting)).await.err();
    assert!(matches!(refused, Some(EngineError::SeedConflict)));
    let b = host.keep(&key(3), true);
    let (engine, _) = host.open(Some(&b)).await.unwrap();
    every_call(&engine, &a, false).await;
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
    every_call(&engine, &seed, true).await;
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
    every_call(&engine, &replacement.expect("a replacement"), true).await;
    host.search(true, "finished");
    engine.close().await;
    host.search(false, "closed, finished");
    host
}

/// What the child runs: every secret to `told`, once nothing more is printed.
async fn play(told: &Path) {
    set_key_hook(minted);
    let mut keys = BTreeSet::new();
    for host in [
        bootstrapped_root().await,
        seeded_root().await,
        finished_root().await,
    ] {
        keys.extend(host.all());
    }
    let lines: Vec<String> = keys.into_iter().collect();
    std::fs::write(told, lines.join("\n")).unwrap();
}

#[test]
fn no_secret_is_stored_or_logged() {
    if let Some(told) = std::env::var_os(TOLD) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        return rt.block_on(play(Path::new(&told)));
    }
    // The search finds each half alone, however written, and a whole key it does not know.
    let probe = key(224);
    let found = |text: &str| leak(text, [probe.expose()]).is_some();
    let raw: Vec<u8> = (224..=255).collect();
    let text = "4OHi4-Tl5ufo6err7O3u7_Dx8vP09fb3-Pn6-_z9_v8".as_bytes();
    assert_eq!(&probe.expose().as_bytes()[41..], text);
    for half in [&text[..21], &text[21..], &raw[..16], &raw[16..]] {
        let hex: String = half.iter().map(|b| format!("{b:02x}")).collect();
        let upper: String = half.iter().map(|b| format!("{b:02X}")).collect();
        for form in [
            latin1(half),
            hex,
            upper,
            format!("{half:?}"),
            format!("{half:x?}"),
            format!("{half:X?}"),
            format!("{half:#?}"),
        ] {
            assert!(found(&form), "{form}");
        }
    }
    // Bytes below 16 are what tell `{:02x?}` from `{:x?}`.
    let low = key(0);
    let low_bits: Vec<u8> = (0..32).collect();
    for half in [&low_bits[..16], &low_bits[16..]] {
        for form in [
            format!("{half:x?}"),
            format!("{half:X?}"),
            format!("{half:02x?}"),
            format!("{half:02X?}"),
        ] {
            assert!(leak(&form, [low.expose()]).is_some(), "{form}");
        }
    }
    assert!(found(&format!("x {}", key(8).expose())));
    assert!(!found("the key format is ixk_<key-id>_<secret>"));

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
    let keys: BTreeSet<&str> = told.lines().collect();
    // Kept: of the bootstrapped root, the seeded one, and the finished one; and one for each
    // refused `create_key` of a key that works. Each is the one the child announced.
    assert_eq!(keys.len(), 8 + 1 + 2 + 6);
    for key in &keys {
        assert!(logs.contains(&announced(key)), "{}", &key[4..40]);
    }
    assert_eq!(leak(&logs, keys), None, "the logs hold a secret");
}
