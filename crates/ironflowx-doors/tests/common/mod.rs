//! A door in front of a recording `Operations`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use ironflowx_core::{Address, Scope, Timestamp};
use ironflowx_doors::{Readiness, Slot, WaitingOn, router};
use ironflowx_proto::{
    Call, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyPage, ListEnvironmentsRequest,
    ListEventsRequest, ListKeysRequest, OpError, Operations, RevokeKeyRequest, Whoami,
    WhoamiRequest,
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

/// Records each call's key, then answers what it was set to.
pub struct Recording {
    pub keys: Mutex<Vec<Option<String>>>,
    pub answer: Mutex<Result<Whoami, OpError>>,
    /// Whether a call waits for good instead.
    pub hangs: AtomicBool,
}

impl Recording {
    pub fn answering(answer: Result<Whoami, OpError>) -> Arc<Self> {
        Arc::new(Self {
            keys: Mutex::default(),
            answer: Mutex::new(answer),
            hangs: AtomicBool::new(false),
        })
    }

    /// The keys of the calls since the last time this was asked.
    pub fn take(&self) -> Vec<Option<String>> {
        std::mem::take(&mut self.keys.lock().unwrap())
    }
}

impl Operations for Recording {
    async fn whoami(&self, cx: &Call, req: WhoamiRequest) -> Result<Whoami, OpError> {
        assert_eq!(req, WhoamiRequest {});
        let key = cx.key().map(|k| k.expose().to_owned());
        self.keys.lock().unwrap().push(key);
        if self.hangs.load(Relaxed) {
            std::future::pending::<()>().await;
        }
        self.answer.lock().unwrap().clone()
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

    async fn list_environments(
        &self,
        _: &Call,
        _: ListEnvironmentsRequest,
    ) -> Result<Vec<Environment>, OpError> {
        unreachable!()
    }

    async fn create_key(&self, _: &Call, _: CreateKeyRequest) -> Result<CreatedKey, OpError> {
        unreachable!()
    }

    async fn list_keys(&self, _: &Call, _: ListKeysRequest) -> Result<KeyPage, OpError> {
        unreachable!()
    }

    async fn revoke_key(&self, _: &Call, _: RevokeKeyRequest) -> Result<(), OpError> {
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
