//! What the steps of `events.feature` and `tenancy.feature` call: an engine in the test's process,
//! or the `ironflowx` binary through the proto client.

use std::path::Path;

use cucumber::gherkin::Scenario;
use ironflowx_operations::Engine;
use ironflowx_proto::{
    Call, Client, CreateKeyRequest, CreatedKey, EmitBatchRequest, EmitBatchResponse, EmitRequest,
    EmitResponse, Environment, Event, EventPage, GetEventRequest, KeyPage, ListEnvironmentsRequest,
    ListEventsRequest, ListKeysRequest, OpError, Operations, RevokeKeyRequest, Whoami,
    WhoamiRequest,
};

use super::serve::Served;

/// How many scenarios run at once against the binary, each with one of its own to start.
pub const BINARIES: usize = 8;

/// Whether a scenario runs against the binary: not one tagged `@local`.
pub fn runs_remotely(scenario: &Scenario) -> bool {
    !scenario.tags.iter().any(|tag| tag == "local")
}

pub enum Adapter {
    Engine(Engine),
    // The binary is killed when it is dropped.
    Binary(Box<Client>, #[allow(dead_code)] Served),
}

impl Adapter {
    /// Serves a fresh data directory: the adapter, and the key in `admin.key`.
    pub async fn served(data_dir: &Path) -> (Self, String) {
        let key_file = data_dir.join("admin.key");
        let data_dir = data_dir.to_owned();
        // Off the thread every scenario runs on.
        let start = tokio::task::spawn_blocking(move || {
            let served = Served::start(&data_dir, "127.0.0.1:0");
            served.ready();
            served
        });
        let served = start.await.unwrap();
        let key = std::fs::read_to_string(key_file).unwrap();
        let key = key.strip_suffix('\n').expect("a newline").to_owned();
        let client = Client::new(&served.url()).unwrap();
        (Self::Binary(Box::new(client), served), key)
    }

    /// `close`, not a drop, of an engine: dropping joins the writer thread on the runtime.
    pub async fn close(self) {
        if let Self::Engine(engine) = self {
            engine.close().await;
        }
    }
}

macro_rules! either {
    ($($name:ident($request:ty) -> $answer:ty;)*) => {
        impl Operations for Adapter {
            $(async fn $name(&self, cx: &Call, req: $request) -> Result<$answer, OpError> {
                match self {
                    Self::Engine(engine) => engine.$name(cx, req).await,
                    Self::Binary(client, _) => client.$name(cx, req).await,
                }
            })*
        }
    };
}

either! {
    emit(EmitRequest) -> EmitResponse;
    emit_batch(EmitBatchRequest) -> EmitBatchResponse;
    get_event(GetEventRequest) -> Event;
    list_events(ListEventsRequest) -> EventPage;
    whoami(WhoamiRequest) -> Whoami;
    list_environments(ListEnvironmentsRequest) -> Vec<Environment>;
    create_key(CreateKeyRequest) -> CreatedKey;
    list_keys(ListKeysRequest) -> KeyPage;
    revoke_key(RevokeKeyRequest) -> ();
}
