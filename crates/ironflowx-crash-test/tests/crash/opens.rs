//! Kills inside an open: while it bootstraps, undoes a failed bootstrap, finishes one, or rotates
//! a seed (invariants.md "Crash test"). `Root` is what each kill and open should have left, by the
//! Open tables of architecture.md, checked against the directory after each; the tests name each
//! row's outcome beside it, over what was seen there, not over what `Root` expects.

use std::num::NonZeroU32;
use std::path::PathBuf;

use ironflowx_authz::parse;
use ironflowx_core::{EnvRef, ErrorKind, FaultPoint, KeyId, KeyOrigin, Scope, Secret};
use ironflowx_operations::{Engine, SeedState, recover_admin_key};
use ironflowx_proto::{
    Call, EmitRequest, ListEnvironmentsRequest, ListEventsRequest, Operations, WhoamiRequest,
};
use ironflowx_store::{BootstrapState, ControlPlane};
use tempfile::TempDir;

use super::{Rng, SEED, alone, clock, emitter, key_id, kill_inside_open};

/// The seeds the parent chooses from, the first of them `SEED`.
const A: &str = SEED;
const B: &str =
    "ixk_019a9f3e-7c2b-7d41-8a3e-200000000002_AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI";
const C: &str =
    "ixk_019a9f3e-7c2b-7d41-8a3e-200000000003_AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM";

/// The points of a bootstrapping open before its bootstrap commits.
const BEFORE_BOOTSTRAP: [FaultPoint; 2] = [
    FaultPoint::BeforeSchemaCommit,
    FaultPoint::BeforeBootstrapCommit,
];
/// Then those before its ledger is at its final name.
const STAGED: [FaultPoint; 4] = [
    FaultPoint::AfterBootstrapCommit,
    FaultPoint::BeforeGenesis,
    FaultPoint::AfterGenesis,
    FaultPoint::BeforeRename,
];
/// Then those before the mark.
const RENAMED: [FaultPoint; 2] = [FaultPoint::AfterRename, FaultPoint::BeforeMarkCommit];
const ROTATION: [FaultPoint; 2] = [FaultPoint::BeforeSeedCommit, FaultPoint::AfterSeedCommit];

/// A point, and what makes the child's open fail, if anything does.
type At = (FaultPoint, Option<&'static str>);
/// The undo after a failure in staging, then after one past the rename, which undoes nothing.
const UNDO_AT: [At; 5] = [
    (FaultPoint::BeforeUndo, Some("staged")),
    (FaultPoint::BeforeUndoCommit, Some("staged")),
    (FaultPoint::AfterUndo, Some("staged")),
    (FaultPoint::BeforeUndo, Some("renamed")),
    (FaultPoint::AfterUndo, Some("renamed")),
];

fn unmarked_points() -> impl Iterator<Item = FaultPoint> {
    STAGED.into_iter().chain(RENAMED)
}

/// A key row as it should be. Not `Debug`: a failed assertion must not print its text.
struct Key {
    text: String,
    origin: KeyOrigin,
    revoked: bool,
}

/// A bootstrap as it should be, its keys oldest first.
struct Boot {
    env: EnvRef,
    /// Its ledger is at its final name.
    placed: bool,
    marked: bool,
    keys: Vec<Key>,
}

impl Boot {
    fn add(&mut self, text: &str, origin: KeyOrigin) {
        self.keys.push(Key {
            text: text.to_owned(),
            origin,
            revoked: false,
        });
    }

    /// Revokes the keys of `origin`; whether there was one to revoke.
    fn revoke(&mut self, origin: KeyOrigin) -> bool {
        let live = |k: &&mut Key| k.origin == origin && !k.revoked;
        let revoked = self.keys.iter_mut().filter(live).map(|k| k.revoked = true);
        revoked.count() > 0
    }

    /// The mark of a finishing open, which loses a minted key's secret: whether it did.
    fn finish(&mut self, replacement: Option<&str>) -> bool {
        self.placed = true;
        self.marked = true;
        let lost = self.revoke(KeyOrigin::Minted);
        if lost && let Some(text) = replacement {
            self.add(text, KeyOrigin::Minted);
        }
        lost
    }

    fn rotates(&self, seed: &str) -> bool {
        self.keys.iter().all(|k| k.text != seed)
    }

    fn apply(&mut self, seed: &str) -> SeedState {
        if self.rotates(seed) {
            self.revoke(KeyOrigin::Seed);
            self.add(seed, KeyOrigin::Seed);
        }
        match self.keys.iter().find(|k| k.text == seed) {
            Some(k) if k.revoked => SeedState::Revoked,
            _ => SeedState::Current,
        }
    }
}

/// What an open returned, its secret aside: that goes to the root's keys.
#[derive(Debug, PartialEq, Eq)]
struct Returned {
    minted: bool,
    lost_secret: bool,
    seed: Option<SeedState>,
}

/// What an open without a seed returns that neither bootstraps nor finishes.
const NOTHING_NEW: Returned = Returned {
    minted: false,
    lost_secret: false,
    seed: None,
};

/// A data directory, and what it should hold.
struct Root {
    dir: TempDir,
    /// Its control plane has a schema.
    schema: bool,
    boot: Option<Boot>,
    /// The text of every key the parent was ever told of, stored or not.
    known: Vec<String>,
    /// Where the bootstrap stood, as last read from the directory.
    state: BootstrapState,
    /// Each key's origin and whether it is revoked, oldest first, as last read from the directory.
    keys: Vec<(KeyOrigin, bool)>,
    /// The text of every known key the last open accepted.
    accepted: Vec<String>,
    /// Every event the parent emitted, oldest first: its ID, and the emitter it names.
    events: Vec<(String, String)>,
}

impl Root {
    fn fresh() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a temporary directory"),
            schema: false,
            boot: None,
            known: [A, B, C].map(str::to_owned).to_vec(),
            state: BootstrapState::NoFile,
            keys: Vec::new(),
            accepted: Vec::new(),
            events: Vec::new(),
        }
    }

    /// A root whose child was killed bootstrapping it, at `point`.
    fn killed(seed: Option<&str>, point: FaultPoint) -> Self {
        let mut root = Self::fresh();
        root.kill(seed, (point, None));
        root
    }

    /// The points an open with `seed` reaches now.
    fn points(&self, seed: Option<&str>) -> Vec<At> {
        let Some(boot) = &self.boot else {
            let schema = usize::from(self.schema);
            let mut points = BEFORE_BOOTSTRAP[schema..].to_vec();
            points.extend(unmarked_points());
            points.push(FaultPoint::AfterMarkCommit);
            let points = points.into_iter().map(|p| (p, None));
            return points.chain(UNDO_AT).collect();
        };
        let mut points = Vec::new();
        if !boot.placed {
            points.extend(&STAGED[1..]);
            points.push(FaultPoint::AfterRename);
        }
        if !boot.marked {
            points.extend([FaultPoint::BeforeMarkCommit, FaultPoint::AfterMarkCommit]);
        }
        if seed.is_some_and(|s| boot.rotates(s)) {
            points.extend(ROTATION);
        }
        points.into_iter().map(|p| (p, None)).collect()
    }

    /// Kills the child inside an open with `seed`, at `point`, and checks what that leaves.
    fn kill(&mut self, seed: Option<&str>, (point, obstruct): At) {
        let renamed = obstruct == Some("renamed");
        let kept = self.boot.as_ref().is_some_and(|b| b.placed);
        let told = kill_inside_open(self.dir.path(), point, seed, obstruct);
        let name = point.name();
        let at_mark = [FaultPoint::BeforeMarkCommit, FaultPoint::AfterMarkCommit].contains(&point);
        let makes_key = match &self.boot {
            None => point != FaultPoint::BeforeSchemaCommit,
            Some(boot) => ROTATION.contains(&point) || !boot.marked && seed.is_none() && at_mark,
        };
        assert_eq!(
            told.len(),
            usize::from(makes_key),
            "killed at {name}: how many keys the child was about to store"
        );
        let told = told.into_iter().next();
        if let Some(told) = &told {
            let text = told.secret.as_deref();
            assert_eq!(
                text.is_some(),
                seed.is_none(),
                "killed at {name}: a secret is told only of a minted key"
            );
            let text = text.or(seed).expect("one of them");
            assert!(
                told.key == key_id(text),
                "killed at {name}: the key told is the minted one, or the seed"
            );
            if let Some(boot) = &self.boot {
                assert_eq!(told.env, boot.env, "killed at {name}: the same environment");
            }
            self.known.push(text.to_owned());
        }
        let text = told.as_ref().and_then(|t| t.secret.as_deref());
        let marked = point == FaultPoint::AfterMarkCommit;
        self.schema |= point != FaultPoint::BeforeSchemaCommit;
        match &mut self.boot {
            None => {
                let undone = point == FaultPoint::AfterUndo && !renamed;
                if !undone && !BEFORE_BOOTSTRAP.contains(&point) {
                    let mut boot = Boot {
                        env: told.as_ref().expect("told above").env,
                        placed: marked || renamed || RENAMED.contains(&point),
                        marked,
                        keys: Vec::new(),
                    };
                    match (seed, text) {
                        (Some(seed), _) => boot.add(seed, KeyOrigin::Seed),
                        (None, text) => boot.add(text.expect("told above"), KeyOrigin::Minted),
                    }
                    self.boot = Some(boot);
                }
            }
            Some(boot) => {
                boot.placed |= at_mark || RENAMED.contains(&point) || ROTATION.contains(&point);
                if !boot.marked && (marked || ROTATION.contains(&point)) {
                    boot.finish(text);
                }
                if point == FaultPoint::AfterSeedCommit {
                    boot.apply(seed.expect("a rotation has a seed"));
                }
            }
        }
        let staging = match point {
            FaultPoint::BeforeGenesis => Some((false, false)),
            FaultPoint::AfterGenesis => Some((true, false)),
            FaultPoint::BeforeRename => Some((true, true)),
            FaultPoint::BeforeUndo if !renamed => Some((true, true)),
            _ => None,
        };
        self.check_rows(&format!("killed at {name}"));
        self.check_ledgers(staging, &format!("killed at {name}"));
        self.check_marks(kept, &format!("killed at {name}"));
    }

    /// Checks that the ledger is still the directory it was, if it was `kept`, and that neither it
    /// nor staging is one an earlier kill left in staging; then marks what is there now.
    fn check_marks(&self, kept: bool, when: &str) {
        let staging = self.dir.path().join("ledgers/.staging");
        let ledger = self
            .boot
            .as_ref()
            .filter(|b| b.placed)
            .map(|_| self.ledger());
        if let Some(ledger) = &ledger {
            let is_kept = ledger.join("kept").exists();
            assert_eq!(is_kept, kept, "{when}: a ledger in place is never replaced");
            let built_again = !ledger.join("staged").exists();
            assert!(
                built_again,
                "{when}: a ledger left in staging is never used"
            );
            std::fs::write(ledger.join("kept"), "").expect("the ledger's directory");
        }
        if staging.exists() {
            let built_again = !staging.join("staged").exists();
            assert!(built_again, "{when}: a leftover staging is deleted first");
            std::fs::write(staging.join("staged"), "").expect("the staging directory");
        }
    }

    fn ledger(&self) -> PathBuf {
        let env = self.boot.as_ref().expect("a bootstrap").env;
        self.dir.path().join("ledgers").join(env.id().to_string())
    }

    /// Checks the bootstrap state, how many rows each table holds, and each key's ID, origin,
    /// digest and whether it is revoked, on a copy: reading a WAL in place would recover it before
    /// the next open does.
    fn check_rows(&mut self, when: &str) {
        let copy = tempfile::tempdir().expect("a temporary directory");
        for name in ["control.db", "control.db-wal"] {
            let from = self.dir.path().join(name);
            if from.exists() {
                std::fs::copy(from, copy.path().join(name)).expect("copies");
            }
        }
        let path = copy.path().join("control.db");
        let state = ControlPlane::bootstrap_state(&path).expect("the state reads");
        let want = match &self.boot {
            None => BootstrapState::Unbootstrapped,
            Some(b) if b.marked => BootstrapState::Marked { env: b.env },
            Some(b) => BootstrapState::Unmarked { env: b.env },
        };
        assert_eq!(state, want, "{when}: the bootstrap");
        self.state = state;
        self.keys = Vec::new();
        let db = rusqlite::Connection::open(&path).expect("the copy opens");
        let count = |table: &str| -> rusqlite::Result<usize> {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        };
        assert_eq!(
            count("sqlite_master").expect("counts") > 0,
            self.schema,
            "{when}: the schema"
        );
        if !self.schema {
            return;
        }
        assert_eq!(count("meta").expect("counts"), 1, "{when}: the format");
        for table in ["tenants", "projects", "environments"] {
            let rows = count(table).expect("counts");
            assert_eq!(rows, usize::from(self.boot.is_some()), "{when}: {table}");
        }
        let keys = self.boot.as_ref().map_or(0, |b| b.keys.len());
        assert_eq!(count("keys").expect("counts"), keys, "{when}: keys");
        if let Some(boot) = &self.boot {
            let control = ControlPlane::open(&path).expect("the copy opens");
            let rows = control.list_keys(&boot.env, None, 100);
            let mut rows = rows.expect("the keys list");
            rows.reverse();
            let seen: Vec<(KeyId, KeyOrigin, bool)> = rows
                .iter()
                .map(|r| (r.record.id, r.origin, r.record.revoked_at.is_some()))
                .collect();
            let want: Vec<(KeyId, KeyOrigin, bool)> = boot
                .keys
                .iter()
                .map(|k| (key_id(&k.text), k.origin, k.revoked))
                .collect();
            assert_eq!(
                seen, want,
                "{when}: each key, its origin and whether revoked"
            );
            // The very secret the parent was told or chose, whether or not the key still works.
            for (row, key) in rows.iter().zip(&boot.keys) {
                let Ok(told) = parse(&Secret::new(key.text.clone())) else {
                    panic!("{when}: {} is told malformed", row.record.id);
                };
                let same = row.record.digest.ct_eq(&told.digest);
                assert!(same, "{when}: {} holds another secret", row.record.id);
            }
            self.keys = seen.into_iter().map(|(_, o, r)| (o, r)).collect();
        }
    }

    /// Checks what `ledgers/` holds: the staging directory, if `staging`, with or without a
    /// genesis record and a read store, and the bootstrap's ledger, if placed.
    fn check_ledgers(&self, staging: Option<(bool, bool)>, when: &str) {
        let ledgers = self.dir.path().join("ledgers");
        let mut names: Vec<String> = match std::fs::read_dir(&ledgers) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            entries => entries
                .expect("ledgers/ reads")
                .map(|e| e.expect("an entry").file_name().into_string())
                .collect::<Result<_, _>>()
                .expect("UTF-8 names"),
        };
        names.sort();
        let mut want: Vec<String> = staging.iter().map(|_| ".staging".to_owned()).collect();
        let placed = self.boot.as_ref().filter(|b| b.placed);
        want.extend(placed.map(|b| b.env.id().to_string()));
        assert_eq!(names, want, "{when}: ledgers/");
        if let Some(staging) = staging {
            let env = self.boot.as_ref().expect("a bootstrap").env.to_string();
            let dir = ledgers.join(".staging");
            let segment = std::fs::read(dir.join("log/00000000000000000000.seg"));
            // The genesis record holds the environment reference as text.
            let genesis = segment
                .expect("the staged log's segment")
                .windows(env.len())
                .any(|w| w == env.as_bytes());
            let read_store = dir.join("read.db").exists();
            assert_eq!(
                (genesis, read_store),
                staging,
                "{when}: staging's genesis record and read store"
            );
        }
    }

    /// Opens with `seed`, checks what that returns, that each key the parent knows works only if
    /// it should and each event so far names its emitter, emits with each key that works, and
    /// checks what is left.
    fn open(&mut self, seed: Option<&str>) -> Returned {
        let kept = self.boot.as_ref().is_some_and(|b| b.placed);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let returned = runtime.block_on(async {
            let seeded = seed.map(|s| Secret::new(s.to_owned()));
            let opened = Engine::open(self.dir.path(), clock(), seeded).await;
            let opened = opened.unwrap_or_else(|e| panic!("the engine opens after the kill: {e}"));
            let minted = opened.minted.as_ref().map(Secret::expose);
            let returned = Returned {
                minted: minted.is_some(),
                lost_secret: opened.lost_secret,
                seed: opened.seed,
            };
            self.known.extend(minted.map(str::to_owned));
            let want = self.expect_open(seed, minted);
            assert_eq!(returned, want, "what the open returned");
            self.accepted.clear();
            for text in self.known.clone() {
                if self.check_key(&opened.engine, &text).await {
                    self.accepted.push(text);
                }
            }
            self.check_events(&opened.engine).await;
            opened.engine.close().await;
            returned
        });
        self.check_rows("after an open");
        self.check_ledgers(None, "after an open");
        self.check_marks(kept, "after an open");
        returned
    }

    /// What an open with `seed` that minted `minted` should leave, and return.
    fn expect_open(&mut self, seed: Option<&str>, minted: Option<&str>) -> Returned {
        self.schema = true;
        let Some(boot) = &mut self.boot else {
            let path = self.dir.path().join("control.db");
            let state = ControlPlane::bootstrap_state(&path).expect("the state reads");
            let BootstrapState::Marked { env } = state else {
                panic!("a bootstrapping open leaves {state:?}");
            };
            let mut boot = Boot {
                env,
                placed: true,
                marked: true,
                keys: Vec::new(),
            };
            match seed {
                Some(seed) => boot.add(seed, KeyOrigin::Seed),
                None => {
                    let text = minted.expect("a bootstrap without a seed mints a key");
                    boot.add(text, KeyOrigin::Minted);
                }
            }
            self.boot = Some(boot);
            return Returned {
                minted: seed.is_none(),
                lost_secret: false,
                seed: seed.map(|_| SeedState::Current),
            };
        };
        let lost_secret = !boot.marked && boot.finish(minted);
        Returned {
            minted: lost_secret && seed.is_none(),
            lost_secret,
            seed: seed.map(|s| boot.apply(s)),
        }
    }

    /// Whether the key whose text is `text` should work.
    fn works(&self, text: &str) -> bool {
        let mut keys = self.boot.iter().flat_map(|b| &b.keys);
        keys.any(|k| k.text == text && !k.revoked)
    }

    /// Checks that a key that should work does, as an admin key, and any other is refused; which.
    async fn check_key(&self, engine: &Engine, text: &str) -> bool {
        let id = key_id(text);
        let me = engine.whoami(&call(text), WhoamiRequest {}).await;
        match me {
            Ok(me) => {
                assert!(self.works(text), "{id} is accepted");
                assert_eq!(me.key_id, id);
                let every = [Scope::Read, Scope::Write, Scope::Admin];
                assert_eq!(me.scopes, every.into(), "{id} is an admin key");
                true
            }
            Err(e) => {
                // Not the error's text: no failure prints what the engine made of a secret.
                assert!(!self.works(text), "{id} is refused: {:?}", e.kind);
                assert_eq!(e.kind, ErrorKind::Unauthenticated, "{id}");
                let uniform = e.message == "invalid API key" && e.hint.is_none();
                assert!(uniform, "{id} is refused in other words");
                false
            }
        }
    }

    /// Emits with each key that works, or with a recovered one if none does, then checks that
    /// every event so far is listed, once and in order, with the key that emitted it.
    async fn check_events(&mut self, engine: &Engine) {
        let mut keys: Vec<String> = self
            .known
            .iter()
            .filter(|k| self.works(k))
            .cloned()
            .collect();
        if keys.is_empty() {
            keys.push(self.recover());
        }
        for key in &keys {
            let emit = EmitRequest {
                name: "crash.open".into(),
                data: None,
                metadata: None,
                version: None,
                idempotency_key: None,
            };
            let id = engine.emit(&call(key), emit).await;
            let id = id
                .unwrap_or_else(|e| panic!("emits: {:?}", e.kind))
                .event_id;
            self.events.push((id.to_string(), emitter(key)));
        }
        let page = ListEventsRequest {
            limit: NonZeroU32::new(200),
            cursor: None,
        };
        let page = engine.list_events(&call(&keys[0]), page).await;
        let page = page.unwrap_or_else(|e| panic!("the events list: {:?}", e.kind));
        let listed: Vec<(String, String)> = page
            .events
            .iter()
            .rev()
            .map(|e| (e.event_id.to_string(), e.emitted_by.to_string()))
            .collect();
        assert_eq!(listed, self.events, "every event and its emitter");
        let cx = call(&keys[0]);
        let envs = engine
            .list_environments(&cx, ListEnvironmentsRequest {})
            .await;
        let envs = envs.unwrap_or_else(|e| panic!("environments list: {:?}", e.kind));
        let envs: Vec<EnvRef> = envs.iter().map(|e| e.env).collect();
        let env = self.boot.as_ref().expect("a bootstrap").env;
        assert_eq!(envs, [env], "the open serves the environment stored");
    }

    /// Recovers an admin key from the directory.
    fn recover(&mut self) -> String {
        let secret = recover_admin_key(self.dir.path(), clock());
        let secret = secret.unwrap_or_else(|e| panic!("a key is recovered: {e}"));
        let text = secret.expose().to_owned();
        let boot = self.boot.as_mut().expect("a bootstrap");
        boot.add(&text, KeyOrigin::Recovered);
        self.known.push(text.clone());
        text
    }

    /// Whether the last open accepted the key whose text is `text`.
    fn accepts(&self, text: &str) -> bool {
        self.accepted.iter().any(|k| k == text)
    }
}

fn call(key: &str) -> Call {
    Call::with_key(Secret::new(key.to_owned()))
}

/// Nothing exists, so the next open bootstraps.
#[test]
fn killed_before_the_bootstrap_commits() {
    let _alone = alone();
    for point in BEFORE_BOOTSTRAP {
        let mut root = Root::killed(Some(A), point);
        assert_eq!(root.state, BootstrapState::Unbootstrapped);
        let returned = root.open(None);
        let bootstrapped = Returned {
            minted: true,
            lost_secret: false,
            seed: None,
        };
        assert_eq!(returned, bootstrapped, "{}", point.name());
        assert!(!root.accepts(A), "{}", point.name());
        assert_eq!(root.open(None), NOTHING_NEW);
    }
}

/// A finishing open builds the ledger again, or opens the one at its final name, and marks it:
/// the seed works.
#[test]
fn killed_before_the_mark_with_a_seed() {
    let _alone = alone();
    for point in unmarked_points() {
        let mut root = Root::killed(Some(A), point);
        assert_eq!(root.open(None), NOTHING_NEW, "{}", point.name());
        assert!(root.accepts(A), "{}", point.name());
        assert_eq!(root.keys, [(KeyOrigin::Seed, false)]);
        assert_eq!(root.open(None), NOTHING_NEW);
    }
}

/// Either nothing exists or an unmarked bootstrap that the next open completes, never `Orphaned`:
/// the next open succeeds.
#[test]
fn killed_during_the_undo() {
    let _alone = alone();
    for seed in [Some(A), None] {
        for at in UNDO_AT {
            let mut root = Root::fresh();
            root.kill(seed, at);
            let left = at != (FaultPoint::AfterUndo, Some("staged"));
            let unmarked = matches!(root.state, BootstrapState::Unmarked { .. });
            assert_eq!(unmarked, left, "{at:?}");
            let returned = root.open(None);
            let want = Returned {
                minted: !left || seed.is_none(),
                lost_secret: left && seed.is_none(),
                seed: None,
            };
            assert_eq!(returned, want, "{at:?}");
            assert_eq!(root.accepts(A), left && seed.is_some(), "{at:?}");
            assert_eq!(root.open(None), NOTHING_NEW);
        }
    }
}

/// The finishing open revokes the very key the parent was told of, says its secret is lost and
/// returns a replacement, which works as an admin key.
#[test]
fn killed_before_the_mark_without_a_seed() {
    let _alone = alone();
    for point in unmarked_points() {
        let mut root = Root::killed(None, point);
        let told = root.known.last().expect("told").clone();
        let replaced = Returned {
            minted: true,
            lost_secret: true,
            seed: None,
        };
        assert_eq!(root.open(None), replaced, "{}", point.name());
        assert!(!root.accepts(&told), "{}", point.name());
        assert!(root.accepts(root.known.last().expect("minted")));
        let keys = [(KeyOrigin::Minted, true), (KeyOrigin::Minted, false)];
        assert_eq!(root.keys, keys);
        assert_eq!(root.open(None), NOTHING_NEW);
    }
}

/// The secret is lost and nothing can tell: the key stays listed. A recovered key gets back in.
#[test]
fn killed_after_the_mark_without_a_seed() {
    let _alone = alone();
    let mut root = Root::killed(None, FaultPoint::AfterMarkCommit);
    assert_eq!(root.open(None), NOTHING_NEW);
    let recovered = root.recover();
    assert_eq!(root.open(None), NOTHING_NEW);
    assert!(root.accepts(&recovered));
    let keys = [(KeyOrigin::Minted, false), (KeyOrigin::Recovered, false)];
    assert_eq!(root.keys, keys);
}

#[test]
fn killed_after_the_mark_with_a_seed() {
    let _alone = alone();
    let mut root = Root::killed(Some(A), FaultPoint::AfterMarkCommit);
    assert_eq!(root.open(None), NOTHING_NEW);
    assert!(root.accepts(A));
    assert_eq!(root.keys, [(KeyOrigin::Seed, false)]);
    assert_eq!(root.open(None), NOTHING_NEW);
}

/// The finishing open marks, then rotates to the other seed; a minted key is lost, not replaced.
#[test]
fn killed_before_the_mark_then_opened_with_another_seed() {
    let _alone = alone();
    for (bootstrapped, origin) in [(Some(A), KeyOrigin::Seed), (None, KeyOrigin::Minted)] {
        for point in unmarked_points() {
            let mut root = Root::killed(bootstrapped, point);
            let rotated = Returned {
                minted: false,
                lost_secret: bootstrapped.is_none(),
                seed: Some(SeedState::Current),
            };
            assert_eq!(root.open(Some(B)), rotated, "{}", point.name());
            assert_eq!(root.accepted, [B], "{}", point.name());
            assert_eq!(root.keys, [(origin, true), (KeyOrigin::Seed, false)]);
            assert_eq!(root.open(None), NOTHING_NEW);
        }
    }
}

/// Exactly one seed works: the new one only once its transaction committed. The events emitted
/// with each, before and after, name their own.
#[test]
fn killed_while_a_later_open_rotates_the_seed() {
    let _alone = alone();
    for point in ROTATION {
        let mut root = Root::fresh();
        root.open(Some(A));
        root.kill(Some(B), (point, None));
        assert_eq!(root.open(None), NOTHING_NEW, "{}", point.name());
        let committed = point == FaultPoint::AfterSeedCommit;
        assert_eq!(root.accepts(B), committed, "{}", point.name());
        assert_eq!(root.accepts(A), !committed, "{}", point.name());
        assert_eq!(root.open(None), NOTHING_NEW);
    }
}

/// A finishing open killed in turn, at each point it reaches, leaves what the next open finishes,
/// or a marked bootstrap. A replacement whose secret the kill lost is recovered past.
#[test]
fn killed_while_a_finishing_open_runs() {
    let _alone = alone();
    let [staged, renamed] = [FaultPoint::AfterBootstrapCommit, FaultPoint::AfterRename];
    let starts = [
        (Some(A), staged),
        (None, staged),
        (Some(A), renamed),
        (None, renamed),
    ];
    for (bootstrapped, first) in starts {
        for seed in [None, Some(B)] {
            for (point, _) in Root::killed(bootstrapped, first).points(seed) {
                let mut root = Root::killed(bootstrapped, first);
                root.kill(seed, (point, None));
                let finished = point == FaultPoint::AfterMarkCommit || ROTATION.contains(&point);
                let marked = matches!(root.state, BootstrapState::Marked { .. });
                assert_eq!(marked, finished, "{}", point.name());
                let replaced = Returned {
                    minted: !finished && bootstrapped.is_none(),
                    lost_secret: !finished && bootstrapped.is_none(),
                    seed: None,
                };
                assert_eq!(root.open(None), replaced, "{}", point.name());
                // A kill that took the minted key and left B unapplied leaves no key: one is
                // recovered.
                use KeyOrigin::{Minted, Recovered, Seed};
                let applied = point == FaultPoint::AfterSeedCommit;
                let want = match (bootstrapped.is_some(), seed.is_some(), finished) {
                    (true, _, _) if applied => vec![(Seed, true), (Seed, false)],
                    (true, _, _) => vec![(Seed, false)],
                    (false, true, true) if applied => vec![(Minted, true), (Seed, false)],
                    (false, true, true) => vec![(Minted, true), (Recovered, false)],
                    (false, _, _) => vec![(Minted, true), (Minted, false)],
                };
                assert_eq!(root.keys, want, "{}", point.name());
                assert_eq!(root.accepts(B), applied, "{}", point.name());
                assert_eq!(root.open(None), NOTHING_NEW);
            }
        }
    }
}

/// Kills up to three opens the seed picks, each with a seed or without and at any point it
/// reaches, opens between them, then inspects the root without a seed, with one the seed picks, and
/// with a recovered key.
pub fn kill_seeded(rng: &mut Rng) {
    let mut root = Root::fresh();
    for i in 0..1 + rng.below(3) {
        let seed = pick_seed(rng);
        let points = root.points(seed);
        if points.is_empty() || i > 0 && rng.below(4) == 0 {
            root.open(seed);
            continue;
        }
        let at = points[rng.below(points.len() as u64) as usize];
        root.kill(seed, at);
    }
    root.open(None);
    root.open(pick_seed(rng));
    root.recover();
    root.open(None);
}

fn pick_seed(rng: &mut Rng) -> Option<&'static str> {
    [None, Some(A), Some(B), Some(C)][rng.below(4) as usize]
}
