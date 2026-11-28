//! What the steps of `events.feature` and `tenancy.feature` call: an engine in the test's process,
//! or the proto client against an `ironflowx-doors` router over one, served on a loopback socket.

use std::path::Path;
use std::sync::Arc;

use cucumber::gherkin::Scenario;
use ironflowx_cell::HostClock;
use ironflowx_core::Clock;
use ironflowx_doors::{Readiness, Slot, WaitingOn, router};
use ironflowx_operations::Engine;
use ironflowx_proto::{
    BeginStepRequest, BeginStepResponse, Call, Claim, ClaimRunRequest, Client, CompleteRunRequest,
    CompleteStepRequest, CompleteStepResponse, CreateKeyRequest, CreatedKey, EmitBatchRequest,
    EmitBatchResponse, EmitRequest, EmitResponse, Environment, Event, EventPage, Function,
    FunctionPage, GetEventRequest, GetFunctionRequest, GetRunRequest, KeyPage,
    ListEnvironmentsRequest, ListEventsRequest, ListFunctionsRequest, ListKeysRequest, OpError,
    Operations, PutFunctionRequest, PutFunctionResponse, RevokeKeyRequest, Run, RunEnd,
    StartRunRequest, StartRunResponse, Whoami, WhoamiRequest,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// How many scenarios run at once through the door, each with an engine and a door of its own.
pub const SERVED: usize = 8;

/// Whether a scenario runs through the door: not one tagged `@local`.
pub fn runs_remotely(scenario: &Scenario) -> bool {
    !scenario.tags.iter().any(|tag| tag == "local")
}

pub enum Adapter {
    Engine(Engine),
    Door(Box<Remote>),
}

pub struct Remote {
    client: Client,
    slot: Arc<Slot<Engine, Open>>,
    engine: Arc<Engine>,
    server: JoinHandle<()>,
}

/// The door's `/ready`, which no step asks.
pub struct Open;

impl Readiness for Open {
    async fn ready(&self) -> Result<(), WaitingOn> {
        Ok(())
    }
}

impl Adapter {
    /// Opens a fresh engine on `data_dir`, on the machine's clock as a host's is, and serves it
    /// through the door: the adapter, and the bootstrap key.
    pub async fn served(data_dir: &Path) -> (Self, String) {
        Self::served_on(data_dir, HostClock).await
    }

    /// As [`Self::served`], on `clock`.
    pub async fn served_on(
        data_dir: &Path,
        clock: impl Clock + Clone + Send + Sync + 'static,
    ) -> (Self, String) {
        let opened = Engine::open(data_dir, clock, None).await.unwrap();
        let key = opened.minted.expect("a fresh engine mints a key");
        let engine = Arc::new(opened.engine);
        let slot = Arc::new(Slot::starting());
        slot.open(engine.clone(), Open);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(slot.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let remote = Remote {
            client: Client::new(&format!("http://{addr}")).unwrap(),
            slot,
            engine,
            server,
        };
        (Self::Door(Box::new(remote)), key.expose().to_owned())
    }

    /// `close`, not a drop, of an engine: dropping joins the writer thread on the runtime.
    pub async fn close(self) {
        match self {
            Self::Engine(engine) => engine.close().await,
            Self::Door(remote) => {
                let Remote {
                    client,
                    slot,
                    engine,
                    server,
                } = *remote;
                drop(client);
                server.abort();
                // The slot lets go of its engine; no call is in flight once a scenario ends.
                slot.close();
                let engine = Arc::try_unwrap(engine)
                    .ok()
                    .expect("only the test holds the engine");
                engine.close().await;
            }
        }
    }
}

macro_rules! either {
    ($($name:ident($request:ty) -> $answer:ty;)*) => {
        impl Operations for Adapter {
            $(async fn $name(&self, cx: &Call, req: $request) -> Result<$answer, OpError> {
                match self {
                    Self::Engine(engine) => engine.$name(cx, req).await,
                    Self::Door(remote) => remote.client.$name(cx, req).await,
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
    put_function(PutFunctionRequest) -> PutFunctionResponse;
    get_function(GetFunctionRequest) -> Function;
    list_functions(ListFunctionsRequest) -> FunctionPage;
    start_run(StartRunRequest) -> StartRunResponse;
    get_run(GetRunRequest) -> Run;
    claim_run(ClaimRunRequest) -> Option<Claim>;
    begin_step(BeginStepRequest) -> BeginStepResponse;
    complete_step(CompleteStepRequest) -> CompleteStepResponse;
    complete_run(CompleteRunRequest) -> RunEnd;
}
