//! A door in front of a recording `Operations`.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use ironflowx_core::{Address, KeyOrigin, Scope, Secret, Timestamp};
use ironflowx_doors::{Readiness, Slot, WaitingOn, router};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyInfo, KeyPage, KeyStatus,
    ListEnvironmentsRequest, ListEventsRequest, ListKeysRequest, OpError, Operations,
    RevokeKeyRequest, Whoami, WhoamiRequest,
};
use tokio::net::TcpListener;

pub const KEY: &str =
    "ixk_0190b5f0-0000-7000-8000-000000000001_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub fn whoami() -> Whoami {
    Whoami {
        key_id: KEY[4..40].parse().unwrap(),
        scopes: [Scope::Read, Scope::Admin].into(),
        environment: Address::new("default", "main"),
        expires_at: Some(Timestamp::from_millis(1_800_000_000_123)),
    }
}

/// The key a `create_key` answers.
pub const CREATED: &str =
    "ixk_0190b5f0-0000-7000-8000-000000000002_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA";

pub fn environments() -> Vec<Environment> {
    vec![Environment {
        env: "0190b5f0-0000-7000-8000-000000000003.00112233445566778899aabbccddeeff"
            .parse()
            .unwrap(),
        name: "main".into(),
        address: Address::new("default", "main"),
        project_id: "0190b5f0-0000-7000-8000-000000000004".parse().unwrap(),
        created_at: Timestamp::from_millis(1_700_000_000_000),
    }]
}

pub fn created() -> CreatedKey {
    CreatedKey {
        key: Secret::new(CREATED.to_owned()),
        info: KeyInfo {
            key_id: CREATED[4..40].parse().unwrap(),
            label: Some("agent é".into()),
            scopes: [Scope::Read, Scope::Write].into(),
            origin: KeyOrigin::Created,
            created_by: Some(KEY[4..40].parse().unwrap()),
            created_at: Timestamp::from_millis(1_700_000_000_001),
            expires_at: Some(Timestamp::from_millis(1_800_000_000_120)),
            status: KeyStatus::Active,
        },
    }
}

pub fn key_page() -> KeyPage {
    let bootstrap = KeyInfo {
        key_id: KEY[4..40].parse().unwrap(),
        label: None,
        scopes: [Scope::Admin].into(),
        origin: KeyOrigin::Minted,
        created_by: None,
        created_at: Timestamp::from_millis(0),
        expires_at: None,
        status: KeyStatus::Revoked,
    };
    KeyPage {
        keys: vec![created().info, bootstrap],
        next: Some("0123.7".into()),
    }
}

/// Records each call's key and request, then answers the error it was set to, or what its
/// operation's function above does, a `whoami` what it was set to.
pub struct Recording {
    pub keys: Mutex<Vec<Option<String>>>,
    /// Each as it prints.
    pub requests: Mutex<Vec<String>>,
    pub answer: Mutex<Result<Whoami, OpError>>,
    /// Whether a call waits for good instead.
    pub hangs: AtomicBool,
}

impl Recording {
    pub fn answering(answer: Result<Whoami, OpError>) -> Arc<Self> {
        Arc::new(Self {
            keys: Mutex::default(),
            requests: Mutex::default(),
            answer: Mutex::new(answer),
            hangs: AtomicBool::new(false),
        })
    }

    /// The keys of the calls since the last time this was asked.
    pub fn take(&self) -> Vec<Option<String>> {
        std::mem::take(&mut self.keys.lock().unwrap())
    }

    /// Their requests, likewise.
    pub fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }

    async fn record(&self, cx: &Call, req: impl fmt::Debug) -> Result<Whoami, OpError> {
        let key = cx.key().map(|k| k.expose().to_owned());
        self.keys.lock().unwrap().push(key);
        self.requests.lock().unwrap().push(format!("{req:?}"));
        if self.hangs.load(Relaxed) {
            std::future::pending::<()>().await;
        }
        self.answer.lock().unwrap().clone()
    }
}

impl Operations for Recording {
    async fn whoami(&self, cx: &Call, req: WhoamiRequest) -> Result<Whoami, OpError> {
        self.record(cx, req).await
    }

    async fn list_environments(
        &self,
        cx: &Call,
        req: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        self.record(cx, req).await.map(|_| environments())
    }

    async fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        self.record(cx, req).await.map(|_| created())
    }

    async fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> Result<KeyPage, OpError> {
        self.record(cx, req).await.map(|_| key_page())
    }

    async fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> Result<(), OpError> {
        self.record(cx, req).await.map(|_| ())
    }

    async fn emit(&self, _: &Call, _: EmitRequest) -> Result<EmitResponse, OpError> {
        unreachable!()
    }

    async fn emit_batch(
        &self,
        _: &Call,
        _: EmitBatchRequest,
    ) -> Result<EmitBatchResponse, OpError> {
        unreachable!()
    }

    async fn get_event(&self, _: &Call, _: GetEventRequest) -> Result<Event, OpError> {
        unreachable!()
    }

    async fn list_events(&self, _: &Call, _: ListEventsRequest) -> Result<EventPage, OpError> {
        unreachable!()
    }
}

pub struct Ready(pub Arc<Mutex<Result<(), WaitingOn>>>);

impl Readiness for Ready {
    async fn ready(&self) -> Result<(), WaitingOn> {
        *self.0.lock().unwrap()
    }
}

pub type TestSlot = Slot<Recording, Ready>;

/// Serves the door of `slot` on a port of its own.
pub async fn serve(slot: Arc<TestSlot>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(slot)).await });
    addr
}

/// Serves the door of an open slot that answers `answer`.
pub async fn serving(answer: Result<Whoami, OpError>) -> (SocketAddr, Arc<Recording>) {
    let operations = Recording::answering(answer);
    let slot = Arc::new(Slot::starting());
    slot.open(operations.clone(), Ready(Arc::new(Mutex::new(Ok(())))));
    (serve(slot).await, operations)
}
