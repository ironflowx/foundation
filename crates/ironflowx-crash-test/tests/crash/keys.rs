//! Kills around a key change's commit and a recorded expiry's, and once the answer is in hand
//! (invariants.md "Crash test"). `Key` is what each request should have left, checked against the
//! directory and through two engines after the kill; the tests name each row's outcome beside it,
//! over what was seen there, not over what `Key` expects.

use std::collections::BTreeSet;
use std::path::Path;

use ironflowx_authz::parse;
use ironflowx_cell::ManualClock;
use ironflowx_core::{Clock, FaultPoint, KeyId, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_operations::Engine;
use ironflowx_proto::{KeyInfo, KeyStatus, ListKeysRequest, Operations, WhoamiRequest};
use ironflowx_store::ControlPlane;

use super::opens::{assert_uniform, call, copy_control};
use super::{Driver, Home, Rng, SEED, alone, clock, key_id};

/// The points around a commit: before it, then after it.
type Around = [FaultPoint; 2];
const CREATE: Around = [
    FaultPoint::BeforeCreateCommit,
    FaultPoint::AfterCreateCommit,
];
const REVOKE: Around = [
    FaultPoint::BeforeRevokeCommit,
    FaultPoint::AfterRevokeCommit,
];
const EXPIRY: Around = [
    FaultPoint::BeforeExpiryCommit,
    FaultPoint::AfterExpiryCommit,
];

/// The seed and the child's second key, by their place in `Work::keys`.
const ADMIN: usize = 0;
const SECOND: usize = 1;
/// Before every time the child's clock reads, and so every expiry.
const STEPPED_BACK: i64 = -3_600_000;

/// When the child is killed on a request: at a point around its commit, which its answer follows,
/// or once the parent has its answer.
#[derive(Debug, Clone, Copy)]
enum At {
    Before,
    After,
    Answered,
}

const EVERY: [At; 3] = [At::Before, At::After, At::Answered];

/// A key the child's engine was told to store, as it should be.
struct Key {
    /// Its ID, once the child has answered it.
    answered: Option<KeyId>,
    /// Its row was committed.
    stored: bool,
    scopes: BTreeSet<Scope>,
    created_by: Option<usize>,
    created_at: i64,
    expires: Option<i64>,
    expiry_recorded: bool,
    revoked: Option<i64>,
}

impl Key {
    /// Whether the key is accepted at `now`. A refusal as newly expired records that, unless the
    /// kill came before it `landed`.
    fn accepted(&mut self, now: i64, landed: bool) -> bool {
        let valid = self.stored && self.revoked.is_none() && !self.expiry_recorded;
        let expired = self.expires.is_some_and(|t| now >= t);
        self.expiry_recorded |= valid && expired && landed;
        valid && !expired
    }

    fn status(&self, now: i64) -> KeyStatus {
        if self.revoked.is_some() {
            KeyStatus::Revoked
        } else if self.expiry_recorded || self.expires.is_some_and(|t| now >= t) {
            KeyStatus::Expired
        } else {
            KeyStatus::Active
        }
    }
}

enum Step {
    /// The caller creates a key, the clock stepping once the caller is admitted and before the
    /// key's transaction.
    Create {
        by: usize,
        expires: Option<i64>,
        step: i64,
    },
    /// The seed revokes the key.
    Revoke(usize),
    /// The key asks who it is.
    Use(usize),
    Clock(i64),
}

/// How a request is answered.
enum Answer {
    Ok,
    /// With the ID of the key it created.
    Created,
    Refused,
}

/// A working child, and what its control plane should hold.
struct Work {
    child: Driver,
    home: Home,
    /// Every key the child told, in that order.
    keys: Vec<Key>,
    /// The child's clock.
    now: i64,
    /// How many requests it was sent.
    sent: usize,
}

impl Work {
    fn start(root: &Path) -> Self {
        let (child, home) = Driver::spawn(root, "-", 1);
        let now = clock().now().as_millis();
        let key = |id, scopes: &[Scope], created_by| Key {
            answered: Some(id),
            stored: true,
            scopes: scopes.iter().copied().collect(),
            created_by,
            created_at: now,
            expires: None,
            expiry_recorded: false,
            revoked: None,
        };
        let keys = vec![
            key(
                key_id(SEED),
                &[Scope::Read, Scope::Write, Scope::Admin, Scope::Execute],
                None,
            ),
            key(home.second, &[Scope::Write], Some(ADMIN)),
        ];
        Self {
            child,
            home,
            keys,
            now,
            sent: 0,
        }
    }

    fn id(&self, key: usize) -> KeyId {
        self.keys[key].answered.expect("an answered key")
    }

    /// What the child calls the key by.
    fn caller(&self, key: usize) -> String {
        match key {
            ADMIN => "1".to_owned(),
            SECOND => "2".to_owned(),
            _ => self.id(key).to_string(),
        }
    }

    /// The request's line, and its label.
    fn line(&mut self, step: &Step) -> (String, String) {
        self.sent += 1;
        let line = match step {
            Step::Create { by, expires, step } => {
                let expires = expires.map_or("-".to_owned(), |t| t.to_string());
                format!("create {} {expires} {step}", self.caller(*by))
            }
            Step::Revoke(key) => format!("revoke 1 {}", self.id(*key)),
            Step::Use(key) => format!("whoami {}", self.caller(*key)),
            Step::Clock(step) => format!("clock {step}"),
        };
        (format!("request {}: {line}", self.sent), line)
    }

    /// What the request leaves, if its commit `landed`, and how it is answered.
    fn apply(&mut self, step: &Step, landed: bool) -> Answer {
        match *step {
            Step::Create { by, expires, step } => {
                self.now += step;
                let accepted = self.keys[by].accepted(self.now, landed);
                self.keys.push(Key {
                    answered: None,
                    stored: accepted && landed,
                    scopes: BTreeSet::from([Scope::Read, Scope::Write, Scope::Admin]),
                    created_by: Some(by),
                    created_at: self.now,
                    expires,
                    expiry_recorded: false,
                    revoked: None,
                });
                if accepted {
                    Answer::Created
                } else {
                    Answer::Refused
                }
            }
            Step::Revoke(key) => {
                if landed {
                    self.keys[key].revoked.get_or_insert(self.now);
                }
                Answer::Ok
            }
            Step::Use(key) => {
                if self.keys[key].accepted(self.now, landed) {
                    Answer::Ok
                } else {
                    Answer::Refused
                }
            }
            Step::Clock(step) => {
                self.now += step;
                Answer::Ok
            }
        }
    }

    fn send(&mut self, step: &Step) {
        self.request(step, true);
    }

    /// Sends the request, whose commit `lands` or fails, and checks its answer.
    fn request(&mut self, step: &Step, lands: bool) {
        let (label, line) = self.line(step);
        self.child.send(&line);
        let want = self.apply(step, lands);
        let answer = self.child.answer(&label);
        match want {
            Answer::Ok => assert_eq!(answer, "ok", "{label}"),
            Answer::Refused => assert_eq!(answer, "err:Unauthenticated", "{label}"),
            Answer::Created => {
                let id = answer
                    .strip_prefix("key ")
                    .unwrap_or_else(|| panic!("{label}: answered {answer}"));
                let id = id.parse().expect("a key ID");
                self.keys.last_mut().expect("created").answered = Some(id);
            }
        }
    }

    /// The seed creates a key; which.
    fn create(&mut self, expires: Option<i64>) -> usize {
        self.send(&Step::Create {
            by: ADMIN,
            expires,
            step: 0,
        });
        self.keys.len() - 1
    }

    /// Any key but the seed, while each was created.
    fn pick(&self, rng: &mut Rng) -> usize {
        SECOND + rng.below(self.keys.len() as u64 - 1) as usize
    }

    /// The child stops at its next pass of `point`.
    fn arm(&mut self, point: FaultPoint) {
        self.child.send(&format!("arm {}", point.name()));
        assert_eq!(self.child.answer("arm"), "ok");
    }

    /// Sends the request, kills the child at the one of its points that `at` says or once it is
    /// answered, and checks what that leaves in the directory.
    fn kill(mut self, step: &Step, points: Around, at: At) -> Killed {
        let point = match at {
            At::Before => Some(points[0]),
            At::After => Some(points[1]),
            At::Answered => None,
        };
        if let Some(point) = point {
            self.arm(point);
            let (label, line) = self.line(step);
            self.child.send(&line);
            self.child.stop(&label, point);
            self.apply(step, matches!(at, At::After));
        } else {
            self.send(step);
        }
        self.finish()
    }

    /// Kills the child and checks what that leaves in the directory.
    fn finish(self) -> Killed {
        let told = self.child.kill();
        let env = self.home.env;
        assert!(told.iter().all(|t| t.env == env), "the same environment");
        let secrets: Vec<bool> = told.iter().map(|t| t.secret.is_some()).collect();
        let want: Vec<bool> = (0..self.keys.len()).map(|k| k != ADMIN).collect();
        assert_eq!(secrets, want, "every key is told, once, the seed without");
        let told: Vec<(KeyId, String)> = told
            .into_iter()
            .map(|t| (t.key, t.secret.unwrap_or_else(|| SEED.to_owned())))
            .collect();
        assert_eq!(told[SECOND].0, self.home.second, "the second key told");
        for ((id, text), key) in told.iter().zip(&self.keys) {
            assert!(*id == key_id(text), "{id} is told with another's secret");
            let same = key.answered.is_none_or(|answered| answered == *id);
            assert!(same, "{:?} is answered for {id} told", key.answered);
        }
        let killed = Killed {
            home: self.home,
            keys: self.keys,
            told,
            listed: Vec::new(),
            accepted: Vec::new(),
        };
        killed.check_rows();
        killed
    }
}

/// What a kill left, and what the last open showed of it.
struct Killed {
    home: Home,
    keys: Vec<Key>,
    /// Each key's ID and text, in the order of `keys`.
    told: Vec<(KeyId, String)>,
    listed: Vec<KeyId>,
    accepted: Vec<KeyId>,
}

impl Killed {
    /// Checks each row's ID, digest, revoke and recorded expiry.
    fn check_rows(&self) {
        let copy = copy_control(&self.home.root);
        let control = ControlPlane::open(&copy.path().join("control.db"));
        let rows = control
            .expect("the copy opens")
            .list_keys(&self.home.env, None, 1_000);
        let mut rows = rows.expect("the keys list");
        rows.reverse();
        let seen: Vec<_> = rows
            .iter()
            .map(|r| {
                let r = &r.record;
                (r.id, r.expiry_recorded, r.revoked_at)
            })
            .collect();
        let stored = || self.told.iter().zip(&self.keys).filter(|(_, k)| k.stored);
        let want: Vec<_> = stored()
            .map(|((id, _), k)| {
                let revoked_at = k.revoked.map(Timestamp::from_millis);
                (*id, k.expiry_recorded, revoked_at)
            })
            .collect();
        assert_eq!(seen, want, "each key, its recorded expiry and its revoke");
        for (row, ((id, text), _)) in rows.iter().zip(stored()) {
            let Ok(told) = parse(&Secret::new(text.clone())) else {
                panic!("{id} is told malformed");
            };
            let same = row.record.digest.ct_eq(&told.digest);
            assert!(same, "{id} holds another secret than was told");
        }
    }

    /// Opens the engine at `now`, checks that exactly the stored keys are listed, then that each
    /// key told is accepted only if it should be.
    fn open(&mut self, now: i64) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let at = ManualClock::new(Timestamp::from_millis(now));
            let engine = self.home.open(at).await;
            self.check(&engine, now).await;
            engine.close().await;
        });
        self.check_rows();
    }

    async fn check(&mut self, engine: &Engine, now: i64) {
        let cx = self.home.call();
        let page = engine.list_keys(&cx, ListKeysRequest::default()).await;
        let mut page = page.unwrap_or_else(|e| panic!("lists: {:?}", e.kind));
        assert_eq!(page.next, None, "one page holds every key");
        page.keys.reverse();
        let stored = self.told.iter().zip(&self.keys).filter(|(_, k)| k.stored);
        let want: Vec<KeyInfo> = stored
            .map(|((id, _), k)| KeyInfo {
                key_id: *id,
                label: None,
                scopes: k.scopes.clone(),
                origin: match k.created_by {
                    None => KeyOrigin::Seed,
                    Some(_) => KeyOrigin::Created,
                },
                created_by: k.created_by.map(|by| self.told[by].0),
                created_at: Timestamp::from_millis(k.created_at),
                expires_at: k.expires.map(Timestamp::from_millis),
                status: k.status(now),
            })
            .collect();
        assert_eq!(page.keys, want, "the keys listed at {now}");
        self.listed = page.keys.iter().map(|k| k.key_id).collect();

        self.accepted.clear();
        for ((id, text), key) in self.told.iter().zip(&mut self.keys) {
            let want = key.accepted(now, true);
            match engine.whoami(&call(text), WhoamiRequest {}).await {
                Ok(me) => {
                    assert!(want, "{id} is accepted at {now}");
                    assert_eq!((me.key_id, &me.scopes), (*id, &key.scopes));
                    let expires = key.expires.map(Timestamp::from_millis);
                    assert_eq!(me.expires_at, expires, "{id}");
                    self.accepted.push(*id);
                }
                Err(e) => {
                    // Not the error's text: no failure prints what the engine made of a secret.
                    assert!(!want, "{id} is refused at {now}: {:?}", e.kind);
                    assert_uniform(*id, &e);
                }
            }
        }
    }

    /// Opens twice with the clock stepped back before every expiry.
    fn open_stepped_back(&mut self) {
        let now = clock().now().as_millis() + STEPPED_BACK;
        self.open(now);
        self.open(now);
    }

    fn lists(&self, key: usize) -> bool {
        self.listed.contains(&self.told[key].0)
    }

    fn accepts(&self, key: usize) -> bool {
        self.accepted.contains(&self.told[key].0)
    }
}

/// The key told before its boundary is listed, and works, only if its `create_key` committed.
#[test]
fn killed_around_a_created_keys_commit() {
    let _alone = alone();
    for at in EVERY {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut work = Work::start(dir.path());
        let earlier = work.create(None);
        let create = Step::Create {
            by: ADMIN,
            expires: Some(work.now + 1_000),
            step: 0,
        };
        let mut killed = work.kill(&create, CREATE, at);
        let key = earlier + 1;
        killed.open_stepped_back();
        let landed = !matches!(at, At::Before);
        assert_eq!(killed.lists(key), landed, "{at:?}");
        assert_eq!(killed.accepts(key), landed, "{at:?}");
        assert!(killed.accepts(earlier), "{at:?}");
    }
}

/// The key is refused only if its `revoke_key` committed; it stays listed either way.
#[test]
fn killed_around_a_revokes_commit() {
    let _alone = alone();
    for at in EVERY {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut work = Work::start(dir.path());
        let earlier = work.create(None);
        let key = work.create(None);
        let other = work.create(None);
        work.send(&Step::Revoke(earlier));
        // Again, which keeps the time of the first.
        work.send(&Step::Clock(1));
        work.send(&Step::Revoke(earlier));
        work.send(&Step::Use(key));
        let mut killed = work.kill(&Step::Revoke(key), REVOKE, at);
        killed.open_stepped_back();
        let landed = !matches!(at, At::Before);
        assert_eq!(killed.accepts(key), !landed, "{at:?}");
        assert!(killed.lists(key), "{at:?}");
        assert!(!killed.accepts(earlier), "{at:?}");
        assert!(killed.accepts(other), "{at:?}");
    }
}

/// A key refused as expired stays refused with the clock stepped back; one whose expiry was not
/// yet recorded, and so was not yet refused, does not.
#[test]
fn killed_around_an_expirys_refusal() {
    let _alone = alone();
    for at in EVERY {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut work = Work::start(dir.path());
        let expiry = work.now + 1_000;
        let earlier = work.create(Some(expiry));
        let key = work.create(Some(expiry));
        let unused = work.create(Some(expiry));
        work.send(&Step::Clock(1_000));
        work.send(&Step::Use(earlier));
        let mut killed = work.kill(&Step::Use(key), EXPIRY, at);
        killed.open_stepped_back();
        let recorded = !matches!(at, At::Before);
        assert_eq!(killed.accepts(key), !recorded, "{at:?}");
        assert!(!killed.accepts(earlier), "{at:?}");
        assert!(killed.accepts(unused), "{at:?}");
    }
}

/// The same for a key that expires between its admission and its `create_key`'s transaction, which
/// records the expiry itself and creates nothing.
#[test]
fn killed_around_a_key_changes_expiry_refusal() {
    let _alone = alone();
    for at in EVERY {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut work = Work::start(dir.path());
        let expiry = work.now + 1_000;
        let key = work.create(Some(expiry));
        let unused = work.create(Some(expiry));
        let create = Step::Create {
            by: key,
            expires: None,
            step: 1_000,
        };
        let mut killed = work.kill(&create, EXPIRY, at);
        killed.open_stepped_back();
        let recorded = !matches!(at, At::Before);
        assert_eq!(killed.accepts(key), !recorded, "{at:?}");
        assert!(killed.accepts(unused), "{at:?}");
        assert!(!killed.lists(unused + 1), "{at:?}");
    }
}

/// An expiry whose commit fails is refused alike, reaches no point that says it is durable, and,
/// not recorded, leaves a key that works with the clock stepped back (TEN-13's stated exception).
#[test]
fn killed_after_an_expiry_that_failed_to_commit() {
    let _alone = alone();
    for in_create in [false, true] {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut work = Work::start(dir.path());
        let key = work.create(Some(work.now + 1_000));
        let db = rusqlite::Connection::open(dir.path().join("control.db"));
        // The update works, and breaks a constraint SQLite checks at the commit.
        db.expect("the control plane opens")
            .execute_batch(
                "CREATE TABLE late (t REFERENCES tenants(id) DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER fail AFTER UPDATE ON keys
                 BEGIN INSERT INTO late VALUES ('none'); END;",
            )
            .expect("the trigger is made");
        work.arm(FaultPoint::AfterExpiryCommit);
        let step = if in_create {
            Step::Create {
                by: key,
                expires: None,
                step: 1_000,
            }
        } else {
            work.send(&Step::Clock(1_000));
            Step::Use(key)
        };
        work.request(&step, false);
        let mut killed = work.finish();
        killed.open_stepped_back();
        assert!(killed.accepts(key), "in a create: {in_create}");
    }
}

/// Sends up to ten requests the seed picks, then kills the child on a key change or an expiry's
/// refusal, at a point around its commit or once answered; opens at a time the seed picks, which
/// may record more expiries, then twice with the clock stepped back.
pub fn kill_seeded(rng: &mut Rng) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut work = Work::start(dir.path());
    for _ in 0..rng.below(11) {
        let key = work.pick(rng);
        let step = match rng.below(8) {
            0..3 => pick_create(rng, work.now),
            3 => Step::Revoke(key),
            4..6 => Step::Use(key),
            _ => Step::Clock(rng.step(10_000)),
        };
        work.send(&step);
    }
    let (step, points) = match rng.below(3) {
        0 => (pick_create(rng, work.now), CREATE),
        1 => (Step::Revoke(work.pick(rng)), REVOKE),
        _ => {
            let expiry = 1 + pick_ms(rng);
            let key = work.create(Some(work.now + expiry));
            let past = expiry + pick_ms(rng);
            if rng.below(2) == 0 {
                work.send(&Step::Clock(past));
                (Step::Use(key), EXPIRY)
            } else {
                let create = Step::Create {
                    by: key,
                    expires: None,
                    step: past,
                };
                (create, EXPIRY)
            }
        }
    };
    let at = EVERY[rng.below(3) as usize];
    let mut killed = work.kill(&step, points, at);
    killed.open(clock().now().as_millis() + rng.step(20_000));
    killed.open_stepped_back();
}

/// A key the seed creates, with an expiry or without.
fn pick_create(rng: &mut Rng, now: i64) -> Step {
    Step::Create {
        by: ADMIN,
        expires: (rng.below(4) > 0).then(|| now + 1 + pick_ms(rng)),
        step: 0,
    }
}

fn pick_ms(rng: &mut Rng) -> i64 {
    i64::try_from(rng.below(10_000)).expect("small")
}
