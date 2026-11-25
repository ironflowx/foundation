# Architecture

One environment's commit path and the engine in front of it, as foundation crates linked into a host's or an embedder's process; and the first host, `ironflowx serve`, which serves the engine over HTTP, with a CLI that calls one.

## C1: Context

![C1 system context](img/c1-context.svg)

The engine is a library: a host opens it on a data directory, and every call to it carries a key. An embedder that needs no keys opens a ledger in a directory instead.

`ironflowx serve` is a host as a process. Outside it:

- a client calls the engine over HTTP, by Connect, gRPC or gRPC-Web, with a key, and reaches it no other way; the CLI, in the same binary, is one, run by an agent in a shell;
- an orchestrator watches `/health` and `/ready`, with no key;
- the operator takes the admin key from `admin.key` in the data directory.

## C2: Crates

![C2 crates](img/c2-crates.svg)

Event rules, credentials and core are pure. Only the ledger, the control plane, the engine and the host touch disk, the clock and randomness.

Around the engine, not in the diagram, each depending on what its line names:

- `ironflowx-proto`, with a wire feature on: the proto, its generated messages and their mapping, and the proto client. It links `connectrpc` and `buffa`, and at build time `connectrpc-build` and `protox`. With none on, it links only `ironflowx-core`.
- `ironflowx-doors`: the HTTP door, on `ironflowx-proto` and `ironflowx-core`, and no engine crate (SRV-1). Unpublished.
- `ironflowx-cli`: the CLI, on `ironflowx-proto` and `ironflowx-core`, and no engine crate (SRV-1); at build time, on `ironflowx-codegen`. Unpublished.
- `ironflowx-codegen`: the generator, which `ironflowx-cli`'s `build.rs` calls; on no crate of ours. Unpublished.
- `ironflowx-bin`: the `ironflowx` binary and its host, on `ironflowx-cli`, `ironflowx-doors`, `ironflowx-operations` and `ironflowx-core`. The host reaches the ledger, control plane, event rules and credentials through the engine. Unpublished.

The `ironflowx` facade lets an embedder opt in to the proto client. The facade's `client` feature enables `ironflowx-proto/client`. Its `client-tls` feature enables `client` and `ironflowx-proto/client-tls`. Both are off by default, so a standalone in-process embedder builds no HTTP stack.

## C3: Modules and seams

![C3 modules and seams](img/c3-modules.svg)

Inside the ledger: its interface is the only way in, and everything behind it is implementation. The engine in front of it, with its credentials and control plane, is in [Modules](#modules), a call's way through them in [Keys](#keys) ("Admission").

In front of the engine, in `ironflowx serve`, a call goes through:

1. the host's accept loop, which reads the request ([Wire](#wire), "Timeouts");
2. the HTTP door, which decodes it, takes its key from `Authorization`, and converts it to an `Operations` request;
3. the slot, which answers for the engine until it is open;
4. the engine, through `Operations`; the answer, or the error, goes back the same way.

A CLI command is a client's side of that:

1. the CLI reads the command line, the key it was given and the engine's URL;
2. the generated call of the command asks the proto client, through `Operations`, which sends it with the key and the deadline;
3. the answer, as wire JSON with its defaults and its JSON text as values, goes to stdout; a failure, as the envelope, to stderr, with its kind's exit code ([CLI](#cli)).

`Operations` is a real seam: the engine in process and the proto client over Connect are its two adapters.

## Flows

![Emit, then read your write](img/flow-emit-read.svg)

How a write becomes visible to every reader: [Commit and read](#commit-and-read).

![Open after a crash, and a failed append](img/flow-restart.svg)

The rows behind it: [Open](#open); an engine's are in [Engine open](#engine-open).

## Modules

![Foundation's layers: a host calls the engine with a key; the engine checks the key against the control plane, then runs event calls on the ledger, which commits events durably and reads them back; an embedder without keys calls the ledger directly; beneath them are the pure credentials, event rules and core types](img/layers.svg)

| Module | Does | Interface | Crate |
|---|---|---|---|
| Ledger | Commits one environment's changes; answers reads that see every acknowledged commit; mints event IDs and stamps event time; reports whether it has failed, and signals a ledger fault | `open`, `open_blocking`, `open_existing_blocking`, `submit`, `read`, `state`, `faulted`, `close`, `HostClock`, `ManualClock` | `ironflowx-cell` |
| Event rules | Decides what an event is and when an emit is refused. Pure | `decide`, `fold`, queries | `ironflowx-events` |
| `Operations` interface | The operations a client calls, with their request and answer types. Hand-written Rust types, which the wire maps onto (D53) | `Operations` trait and messages | `ironflowx-proto` |
| Proto | Defines an operation once, on the wire (D49) | `proto/ironflowx/v1/*.proto`: services, messages, the method options `cli`, `summary` and `example`, the field option `json_text` | `ironflowx-proto` (the `.proto` files; compiled, `wire::FILE_DESCRIPTOR_SET`) |
| Wire mapping | Converts `Operations` requests and answers to and from wire messages, validating what arrives | `TryFrom` for requests, both ways; `From` for answers sent and `TryFrom` for answers received; `OpError` ↔ `ErrorInfo` ↔ the Connect error | `ironflowx-proto` (`wire`, `server`) |
| Proto client | Calls `Operations` over Connect, sending each `Call`'s key | `Client::new(url)`, `with_timeout`, `Operations` | `ironflowx-proto` (`client`; `client-tls` for `https`) |
| HTTP door | Serves `Operations` over Connect, gRPC and gRPC-Web, with `/health` and `/ready`. Holds no rule | `router(slot)`, `Slot`, `Readiness` | `ironflowx-doors` (`http`) |
| Host | Turns a data directory and an address into a served engine | `ironflowx serve`, `admin.key`, exit codes | `ironflowx-bin` |
| CLI | Calls an operation from a shell | The generated commands, one per method of the proto; `version`; the envelope and exit codes | `ironflowx-cli` |
| Generator | Derives the CLI's commands from the proto at build time, and refuses a proto that does not say enough | `generate(descriptor_set, out_dir)`, from `build.rs` | `ironflowx-codegen` |
| Engine | Reads whether the bootstrap is marked; opens a data directory, bootstrapping it on first open; admits every call (its key parsed, looked up and verified, and its scope checked) before the handler reaches the ledger or the key records; reports its health and a ledger fault to the host | `is_bootstrapped`, `Engine::open`, `health`, `faulted`, `close`, `Operations`, `recover_admin_key` | `ironflowx-operations` |
| Credentials | The key format and its digest; whether a presented key is valid for an environment at a time, whether a grant holds a scope, and what a new key may hold. Pure | `parse`, `mint`, `verify`, `recheck`, `verify_seed`, `admit`, `may_create` | `ironflowx-authz` |
| Control plane | Remembers which tenants, projects, environments and keys exist, in `control.db` | `ControlPlane::open`, `bootstrap_state`, `ensure_bootstrap`, `undo_bootstrap`, `mark_ledger_created`, `finish_bootstrap`, `check_seed`, `apply_seed`, `lookup_key`, `record_expiry`, `insert_key`, `insert_recovered`, `revoke_key`, `list_keys`, `environment` | `ironflowx-store` |
| Core types | Validated IDs, `EnvRef`, `Emitter`, `SubmitAs`, `Scope`, `KeyOrigin`, `Address`, `Secret`, `Timestamp`, `SequencedTime`, the `Clock` trait, errors | Types | `ironflowx-core` |
| Facade | Gives embedders one dependency on foundation | Crate re-exports. `proto::Client` with `client`, also accepting `https` with `client-tls` | `ironflowx` |

`Engine` is the one in-process `Operations`, so `Operations` always means credentialed (D36). For an event operation it maps the request to a submission or a query and the outcome to an answer, and holds no event rule: every check, default and limit is in event rules. It submits as its verified grant's `submit_as()`, so an event emitted with a key records `key:<key-id>`. An embedder that needs no keys calls `Ledger::submit` and `read` directly, as `SubmitAs::embedder(name)`; the ledger trusts it.

| Type | Is |
|---|---|
| `Timestamp` | A UTC instant in whole milliseconds since the Unix epoch. Private fields; `from_millis` and `as_millis` (`i64`) are the only conversions (D18) |
| `SequencedTime` | A reading of the event clock. Its one constructor is `#[doc(hidden)]`, and a lint allows it only in `ironflowx-cell` (D17); it compares with itself, never with `Timestamp` or `std::time` |
| `Clock` | A trait in core: `fn now(&self) -> Timestamp`. Core declares it and never calls it. `now` must not panic: it runs on the ledger's writer and under the control plane's lock |
| `EnvRef` | `{ id: EnvironmentId, token: IncarnationToken }`. Text form `<uuid>.<32 hex digits>` via `Display` and `FromStr`, so an embedder can store it. Only the canonical spelling parses: a lowercase, hyphenated UUIDv7 and 32 lowercase hex digits; `EnvironmentId`, `EventId`, `TenantId`, `ProjectId`, `KeyId` and `IncarnationToken` parse the same way |
| `Emitter` | Who emitted an event, as read back: text `key:<key-id>` or `embedder:<name>` via `Display` and `FromStr`. Read-only: nothing converts it into a `SubmitAs` (D36) |
| `SubmitAs` | Who a submission is attributed to, as written: `SubmitAs::embedder(name)`, where the name is 1–128 bytes with no control (Cc), format (Cf) or line or paragraph separator (Zl, Zp) character, so what a reader sees is what was stored, or `SubmitAs::key(KeyId)`, which a lint confines to the credentials crate (D36). Every event of a submission carries its one emitter |
| `Scope` | What a key may do: `read`, `write`, `admin`, text forms of the same names. A key holds a set; none implies another |
| `KeyOrigin` | How a key came to be: `Minted`, `Seed`, `Recovered` or `Created`. In core, since the control plane stores it and the interface lists it, and neither depends on the other |
| `Address` | Where an environment is, by name: `project/environment` while the single tenant is hidden. Derived from names, never stored, and never parsed into an ID: no `FromStr` (TEN-8) |
| `Secret` | A whole key's text. Implements none of `Debug`, `Display`, `Serialize` and `Clone`; `expose()` is its only read; zeroed on drop (TEN-3) |
| `KeyDigest` | SHA-256 of a key's whole text, in `ironflowx-authz`. Private bytes, no `PartialEq`, `AsRef` or `Deref`; `ct_eq` is its only comparison; `to_stored` and `from_stored`, its storage codec, are confined to `ironflowx-store` by a lint (TEN-4) |
| `KeyRecord` | What credentials need about a stored key: `{ id, env: EnvRef, scopes, digest, expires_at, expiry_recorded, revoked_at }`, `env` being the environment ID and token current when the key was made |
| `Grant` | What a verified key may do: its key ID, scopes, `EnvRef` and expiry, and `submit_as()`, the key's attribution, the only `SubmitAs::key` call, which clippy confines to the engine (D36). Only `verify` and `recheck` make one; never stored |
| `Authorized` | Proof of admission, `{ grant }`, its field private to the engine's admission module: a handler reaches the engine's ledger and key records only with one (TEN-1) |

## Seams

| Seam | Adapters | Today |
|---|---|---|
| `Operations` | The engine (`Engine`) · the proto client (`Client`) | Both. The behaviors drive each |
| `Clock` | `HostClock` · `ManualClock`, which a test sets and steps, backwards too | Both in `ironflowx-cell`. The crash test and the behaviors on the engine use `ManualClock`. The host uses `HostClock` through `ironflowx-operations` |
| Control plane | SQLite · a shared database for clusters | SQLite only: hypothetical, so no trait |
| Ledger, event rules, credentials, door, host, CLI, generator | One adapter each | Module interfaces, not seams |

The segment log has a storage seam of its own, private to `segment-log-core`; foundation does not reach it.

## Interfaces

Shapes, abridged. Each crate's full public API is its snapshot in [`public-api/`](../public-api/), which CI checks (D21).

```rust
// ironflowx-proto. No dyn use; callers are generic over the trait.
trait Operations: Send + Sync {
    fn emit(&self, cx: &Call, req: EmitRequest) -> impl Future<Output = Result<EmitResponse, OpError>> + Send;
    fn emit_batch(&self, cx: &Call, req: EmitBatchRequest) -> impl Future<Output = Result<EmitBatchResponse, OpError>> + Send;
    fn get_event(&self, cx: &Call, req: GetEventRequest) -> impl Future<Output = Result<Event, OpError>> + Send;
    fn list_events(&self, cx: &Call, req: ListEventsRequest) -> impl Future<Output = Result<EventPage, OpError>> + Send;
    fn whoami(&self, cx: &Call, req: WhoamiRequest) -> impl Future<Output = Result<Whoami, OpError>> + Send;
    fn list_environments(&self, cx: &Call, req: ListEnvironmentsRequest) -> impl Future<Output = Result<Vec<Environment>, OpError>> + Send;
    fn create_key(&self, cx: &Call, req: CreateKeyRequest) -> impl Future<Output = Result<CreatedKey, OpError>> + Send;
    fn list_keys(&self, cx: &Call, req: ListKeysRequest) -> impl Future<Output = Result<KeyPage, OpError>> + Send;
    fn revoke_key(&self, cx: &Call, req: RevokeKeyRequest) -> impl Future<Output = Result<(), OpError>> + Send;
}
enum Operation { Emit, EmitBatch, GetEvent, ListEvents, Whoami, ListEnvironments, CreateKey, ListKeys, RevokeKey }
impl Operation {
    const ALL: &'static [Operation];               // one per method of the trait, in its order
    const fn rpc(self) -> &'static str;            // "ironflowx.v1.EventService/Emit": an exhaustive match
    const fn changes_state(self) -> bool;          // Emit, EmitBatch, CreateKey, RevokeKey: an exhaustive match
}
const fn required_scope(op: Operation) -> Option<Scope>;   // an exhaustive match (D37); None: any valid key
// EmitRequest      { name: String, data: Option<Vec<u8>> /* one JSON value (D22); None stores null */,
//                    metadata: Option<Vec<u8>> /* one JSON object */, version: Option<i64> /* None stores 1 */,
//                    idempotency_key: Option<String> /* None: no key */ }
// EmitResponse     { event_id: EventId, duplicate: bool }
// EmitBatchRequest { items: Vec<EmitRequest> }
// EmitBatchResponse{ items: Vec<Result<EmitResponse, OpError>> }  // one per input, in input order
// GetEventRequest  { event_id: String }                            // an ID that does not parse is NotFound
// Event            { event_id, name, data: Vec<u8> /* the bytes as sent */, metadata: Option<Vec<u8>>, version: i64,
//                    idempotency_key: Option<String>, time: Timestamp, emitted_by: Emitter }
// ListEventsRequest{ limit: Option<NonZeroU32> /* None 20, above 200 clamps to 200 */, cursor: Option<String> }
// EventPage        { events: Vec<Event> /* newest first; data and metadata within 3 MiB, or one event (D73) */,
//                    next: Option<String> /* None on the last page, else continues after the last event */ }
// Call             Call::anonymous() | Call::with_key(Secret)   // the key as presented (D45); not Clone
// OpError          { kind: ErrorKind, message: String, hint: Option<String> }   // see Errors
// Whoami           { key_id: KeyId, scopes: BTreeSet<Scope>, environment: Address, expires_at: Option<Timestamp> }
// Environment      { env: EnvRef, name, address: Address, project_id: ProjectId, created_at: Timestamp }
// WhoamiRequest {} · ListEnvironmentsRequest {}   // no request takes an environment (LED-7)
// CreateKeyRequest { scopes: BTreeSet<Scope> /* empty: {read} */, expires_at: Option<Timestamp>, label: Option<String> }
// CreatedKey       { key: Secret /* returned once */, info: KeyInfo }   // no Clone or PartialEq; Debug prints no key
// KeyInfo          { key_id, label, scopes, origin: KeyOrigin /* core: Minted | Seed | Recovered | Created */,
//                    created_by: Option<KeyId>, created_at, expires_at, status: Active | Expired | Revoked }
// ListKeysRequest  { limit: Option<NonZeroU32> /* None 100, above 1,000 clamps */, cursor: Option<String> }
// KeyPage          { keys: Vec<KeyInfo> /* newest first, by seq */, next: Option<String> /* <token>.<seq> */ }
// RevokeKeyRequest { key_id: String }   // an ID that does not parse is NotFound

// ironflowx-proto, feature "client" (tokio): the proto client. It names no dependency's type.
impl Client {
    fn new(url: &str) -> Result<Client, InvalidUrl>;       // http://host[:port][/path], no user, query or fragment;
                                                           // https:// too with the feature "client-tls"
    fn with_timeout(self, timeout: Duration) -> Client;    // the most each call takes; a new client sets no end
}
impl Operations for Client { .. }   // sends Call's key as the Bearer key; Call::anonymous() sends none

// ironflowx-proto, feature "wire": the messages connectrpc-build generates from the protox-compiled set, and their mapping.
// Feature "server": also the generated service traits a door implements. Both expose buffa's and connectrpc's types, follow
// their 0.x versions and are left out of public-api/, which has the default features and `client`.
// These four for each operation, as for `create_key`:
impl TryFrom<wire::CreateKeyRequest> for CreateKeyRequest { type Error = OpError; }       // the door; Invalid on what the wire cannot carry
impl TryFrom<CreateKeyRequest> for wire::CreateKeyRequest { type Error = OpError; }       // the client; Invalid on the same
impl From<CreatedKey> for wire::CreateKeyResponse { }                                     // the door, and the CLI, to print it
impl TryFrom<wire::CreateKeyResponse> for CreatedKey { type Error = wire::Undecodable; }  // the client
impl From<&OpError> for wire::ErrorInfo { }
impl TryFrom<wire::ErrorInfo> for OpError { type Error = wire::Undecodable; }       // an unknown kind is no ErrorInfo
fn wire::connect_error(e: &OpError) -> ConnectError;        // the kind's code, the message, ErrorInfo as a detail
fn wire::op_error(e: &ConnectError) -> Option<OpError>;     // the engine's own error, if the answer carries one
const wire::FILE_DESCRIPTOR_SET: &[u8];                     // the compiled proto, imports and custom options included

// ironflowx-doors; `router` behind the feature "http"
trait Readiness: Send + Sync { fn ready(&self) -> impl Future<Output = Result<(), WaitingOn>> + Send; }
// WaitingOn  Open | ControlPlane | Ledger
impl<O: Operations, R: Readiness> Slot<O, R> {              // Starting → Open → Draining → Closed
    const fn starting() -> Self;
    fn open(&self, operations: Arc<O>, readiness: R);       // once, while Starting
    fn drain(&self);                                       // can also stop from Starting
    fn close(&self);
}
fn router<O: Operations + 'static, R: Readiness + 'static>(slot: Arc<Slot<O, R>>) -> axum::Router;

// ironflowx-codegen: build time only
fn generate(descriptor_set: &[u8], out_dir: &Path) -> Result<(), CodegenError>;   // writes out_dir/cli.rs; what it refuses: Proto

// ironflowx-cli. The host adds its commands to `root`, and runs its own.
fn parse(root: clap::Command) -> Result<ArgMatches, ExitCode>;   // Err: --help printed its text, or a usage error its envelope;
                                                                 // or clap would print the arguments, so none was read
fn run(matches: &ArgMatches) -> ExitCode;                        // a command of the CLI's own

// ironflowx-bin (tokio)
async fn serve(data_dir: &Path, listen: SocketAddr) -> Result<(), ServeError>;
async fn run(listener: std::net::TcpListener, data_dir: &Path) -> Result<(), ServeError>;   // bound listener. The caller handles signals
async fn serve_with_seed(data_dir: &Path, listen: SocketAddr, seed: Option<Secret>) -> Result<(), ServeError>;
// ServeError StaleKeyFile(PathBuf) | Listen(io::Error) | Signals(io::Error) | LedgerFault
// | DataDir(PathBuf, io::Error) | KeyLookup(PathBuf, io::Error) | Engine(EngineError)
// | Probe(PathBuf, io::Error) | KeyFile(PathBuf, io::Error, KeyId)

// ironflowx-operations also exports HostClock, OpenError and StoreError.
fn is_bootstrapped(root: &Path) -> Result<bool, EngineError>;   // blocking; reads the bootstrap mark
// These Engine methods use tokio.
impl Engine {
    async fn open(root: &Path, clock: impl Clock + Clone + Send + Sync + 'static, seed: Option<Secret>)
        -> Result<Opened, EngineError>;
    async fn health(&self) -> Health;   // reads the control plane once (no key record), then the ledger's state; no key, not an operation
    fn faulted(&self) -> impl Future<Output = Fault> + Send + 'static;   // Ledger::faulted; no key, not an operation
    async fn close(self);
    async fn close_with_state(self) -> LedgerState;   // ledger state at the start of close
}
// Each call is admitted first, with no cache; each submission is attributed to the calling key,
// from its grant (TEN-6).
impl Operations for Engine { .. }
// Opened      { engine: Engine,
//               minted: Option<Secret>,      // a bootstrapping open without a seed, or a finishing open that replaced a lost minted key
//               lost_secret: bool,           // a finishing open revoked a minted key whose secret no open returned
//               seed: Option<SeedState> }    // Current | Revoked; None when no seed was given
// Health      { control_plane: Ok | Failed, ledger: LedgerState }
// EngineError MalformedSeed | NotAnEngineRoot | Locked | Format | Orphaned | LedgerMissing | SeedConflict
//             | Ledger(OpenError) | Store(StoreError) | Io(io::Error)
fn recover_admin_key(root: &Path, clock: impl Clock) -> Result<Secret, RecoverError>; // host-only, blocking; not an operation

// ironflowx-authz: pure, depends only on core. The engine passes in the random secret.
fn parse(raw: &Secret) -> Result<KeyParts, Malformed>;   // KeyParts { id: KeyId, digest: KeyDigest }
fn mint(id: KeyId, secret: [u8; 32]) -> (Secret, KeyDigest);
fn verify(parts: &KeyParts, record: Option<&KeyRecord>, env: &EnvRef, now: Timestamp) -> Result<Grant, Refused>;
fn recheck(record: &KeyRecord, env: &EnvRef, now: Timestamp) -> Result<Grant, Refused>;   // verify, without the secret
// Refused: one answer for every bad key; newly_expired() says to record the expiry (D42), never shapes the answer
// What the key stored under the seed's key ID makes of the seed: SeedState, Current or Revoked
fn verify_seed(seed: &KeyParts, record: &KeyRecord, origin: KeyOrigin, env: &EnvRef) -> Result<SeedState, SeedConflict>;
fn admit(grant: &Grant, required: Option<Scope>) -> Result<(), MissingScope>;   // MissingScope::scope() names it
fn may_create(creator: &Grant, scopes: &BTreeSet<Scope>, expires_at: Option<Timestamp>, label: Option<&str>,
              now: Timestamp) -> Result<(), CreateRefusal>;

// ironflowx-store: blocking; async Engine methods call it under spawn_blocking.
// is_bootstrapped and recover_admin_key use the caller's thread.
// A write takes SQLite's write lock (BEGIN IMMEDIATE) first, then reads now from the clock it is given.
impl ControlPlane {
    fn open(path: &Path) -> Result<ControlPlane, StoreError>;               // creates the schema; refuses another format
    fn bootstrap_state(path: &Path) -> Result<BootstrapState, StoreError>;  // writes no row, may rebuild -shm; refuses another format
    // StoreError Format | Failed(String)
    // BootstrapState: NoFile | Unbootstrapped | Unmarked { env: EnvRef } | Marked { env: EnvRef }
    fn ensure_bootstrap(&self, b: &Bootstrap, clock: &dyn Clock) -> Result<(), StoreError>; // one transaction
    // Bootstrap { env: EnvRef, tenant: TenantId, project: ProjectId, key: KeyParts, by_seed: bool }
    fn undo_bootstrap(&self, env: &EnvRef) -> Result<(), StoreError>;       // one transaction; refuses once marked
    fn mark_ledger_created(&self, env: &EnvRef, clock: &dyn Clock) -> Result<(), StoreError>; // refuses once marked
    // One transaction: marks, refusing once marked; revokes the minted key, if any, and then stores `replacement` in its
    // place. Whether it revoked one
    fn finish_bootstrap(&self, env: &EnvRef, replacement: Option<&KeyParts>, clock: &dyn Clock) -> Result<bool, StoreError>;
    fn check_seed(path: &Path, env: &EnvRef, seed: &KeyParts) -> Result<(), SeedError>; // apply_seed's conflict rule, as bootstrap_state reads
    fn apply_seed(&self, env: &EnvRef, seed: &KeyParts, clock: &dyn Clock) -> Result<SeedState, SeedError>; // one transaction
    // SeedError Conflict | Store(StoreError)
    // Admission's one read: the key's record, then now, as one step no recorded expiry falls between
    fn lookup_key(&self, id: KeyId, env: &EnvRef, clock: &dyn Clock) -> Result<(Option<KeyRecord>, Timestamp), StoreError>;
    fn record_expiry(&self, env: &EnvRef, id: KeyId) -> Result<(), StoreError>;             // sets expiry_recorded, once
    // One transaction: reads the creator's record again and checks it (recheck), then the request (may_create), then
    // that fewer than `cap` created keys hold a slot
    fn insert_key(&self, env: &EnvRef, new: NewKey, creator: KeyId, clock: &dyn Clock, cap: u32)
        -> Result<KeyRow, KeyChangeError>;
    // One transaction: refuses unless the bootstrap is marked, then inserts an admin key of origin `Recovered`
    fn insert_recovered(&self, key: &KeyParts, clock: &dyn Clock) -> Result<(), RecoverError>;
    // RecoverError NotReady | Store(StoreError), re-exported by ironflowx-operations
    // One transaction: reads `by`'s record again and checks it (recheck), then sets revoked_at once; the row stays
    fn revoke_key(&self, env: &EnvRef, id: KeyId, by: KeyId, clock: &dyn Clock) -> Result<(), KeyChangeError>;
    fn list_keys(&self, env: &EnvRef, before_seq: Option<u64>, limit: u32) -> Result<Vec<KeyRow>, StoreError>; // newest first
    // NewKey { id, digest, scopes, label, expires_at }
    // KeyRow { seq, record: KeyRecord, origin: KeyOrigin, created_by: Option<KeyId>, label, created_at }
    // KeyChangeError Caller | Refused(CreateRefusal) | Full | NotFound | Store(StoreError)
    fn environment(&self, env: &EnvRef) -> Result<EnvironmentRecord, StoreError>;
}

// ironflowx-cell (tokio)
impl Ledger {
    // open, read and close panic outside a tokio runtime; open and close also on one that has shut down, and close drops the ledger then
    async fn open(dir: &Path, env: EnvRef, clock: impl Clock + Send + 'static) -> Result<Ledger, OpenError>;
    // As open, on the calling thread, outside a runtime too
    fn open_blocking(dir: &Path, env: EnvRef, clock: impl Clock + Send + 'static) -> Result<Ledger, OpenError>;
    // As open_blocking, but Missing for a directory that is missing, or whose log is missing, empty or begins with a record of events; creates neither
    fn open_existing_blocking(dir: &Path, env: EnvRef, clock: impl Clock + Send + 'static) -> Result<Ledger, OpenError>;
    async fn submit(&self, sub: Submission, by: SubmitAs) -> Result<Committed, SubmitError>;   // trusts `by` (D36)
    // q is a query from event rules (get_event, list_events) over the read store; a q that panics fails the ledger
    async fn read<R: Send + 'static>(&self, q: impl FnOnce(&ReadStore) -> Result<R, Error<StoreError>> + Send + 'static)
        -> Result<R, ReadError>;
    fn env(&self) -> EnvRef;
    fn state(&self) -> LedgerState;   // Open | Unavailable
    fn faulted(&self) -> impl Future<Output = Fault> + Send + 'static;   // resolves once, when the ledger fails; a close is no fault
    async fn close(self);   // drains in-flight submits and the fold, then releases the directory lock
    async fn close_with_state(self) -> LedgerState;   // orders the start of close with fault publication
}
fn new_env_ref() -> EnvRef;   // a fresh UUIDv7 ID and a 128-bit token from the OS random source
// Committed   { outcome: Vec<ItemOutcome> }   // one per item, in input order
// ItemOutcome New(EventId) | Duplicate(EventId) | Refused(Refusal)   // Refused only inside a batch
// SubmitError Refused(Refusal) | Unknown | Unavailable              // Refused: a single emit, or a whole batch
// ReadError   Refused(Refusal) | Unavailable                        // a read-store failure fails the ledger

// ironflowx-events: pure. `fresh` yields one event ID per new event, minted by the writer.
fn decide<V: EventsView>(view: &V, sub: &Submission, now: SequencedTime,
          fresh: &mut impl Iterator<Item = EventId>) -> Result<Decision, Error<V::Error>>;
fn fold<T: EventTables>(tx: &mut T, record: &Record, emitted_by: &Emitter) -> Result<(), T::Error>;
fn get_event<V: EventsView>(view: &V, id: &str) -> Result<Event, Error<V::Error>>;
fn list_events<V: EventsView>(view: &V, token: IncarnationToken, limit: Option<NonZeroU32>,
          cursor: Option<&str>) -> Result<EventPage, Error<V::Error>>;
// EventsView also has event_by_key, last_sequence and event.
// The budget is asked about each event in turn, before it is loaded; the first it refuses ends the read
fn EventsView::events_before(&self, before: Option<u64>, budget: &mut PageBudget) -> Result<Vec<Event>, Self::Error>;
fn PageBudget::take(&mut self, bytes: usize) -> bool;   // only list_events makes a PageBudget
// Error<E> Refused(Refusal) | Store(E)   // E: the view's own read error
```

## Event rules

`decide` runs in this order; steps 1 to 3, every check on the request, run before the idempotency lookup (D5):

1. **Batch count:** more than 100 items refuses the whole batch.
2. **Item rules**, below, per item. A breach refuses the emit, or that batch item, as `Invalid`.
3. **Batch bytes:** the data and metadata of the items that passed step 2, as sent, total at most 4 MiB; else the whole batch is refused.
4. **Idempotency lookup**, in input order. A key seen earlier in the same batch counts (EVT-022).
5. **IDs:** one drawn from `fresh` per new event.

| Field | Rule | Scenarios |
|---|---|---|
| Name | 1–256 bytes of UTF-8 (bytes, not characters) | EVT-010, EVT-011, EVT-012 |
| Name | Segments split on `.`; none empty. No Unicode whitespace (`char::is_whitespace`), no control character (`char::is_control`) | EVT-011, EVT-012 |
| Name | Owned, so refused, in exact case: `ironflowx`, `ironflowx.…`, `$system`, `$system.…` | EVT-012, EVT-013 |
| Data | Absent stores `null`. Otherwise one JSON value of at most 524,288 bytes as sent, stored and returned as those bytes | EVT-003, EVT-004, EVT-017, EVT-039 |
| Data, metadata | Valid UTF-8. Nested at most 127 deep; no number outside `f64` range; no key repeated in one object at any depth, comparing keys after unescaping; no NUL in any key or string after unescaping; no unpaired surrogate escape | EVT-016, EVT-039 |
| Data | A top-level object key `$redacted`, whatever its value, is refused; nested ones are accepted | EVT-014, EVT-015 |
| Metadata | Absent, or a JSON object of at most 65,536 bytes as sent | EVT-016, EVT-038 |
| Version | Absent stores 1; otherwise 1 to 2,147,483,647 | EVT-002, EVT-008, EVT-009 |
| Idempotency key | Absent means no key; otherwise 1–256 bytes, no NUL. One key per environment, across names, for as long as its event exists | EVT-030–037 |
| Batch | An empty batch answers with no results | EVT-023 |

If a build turns on `serde_json`'s `arbitrary_precision` feature, which Cargo's feature unification can do from any crate in the embedder's graph, an object whose first key is `$serde_json::private::Number` is read as a number and so may be refused. A default build reads that key like any other. A door, or the CLI's `--json`, in such a build also refuses a JSON `limit` or `version` written with a fraction or an exponent (`5.0`, `1e0`), which a default build reads as the nearest `f64`, so a fraction too fine for one is lost. CI runs the event-rule tests in both builds, and the CLI's own: what it prints of an event is read the same in either.

A list query, not `decide`, checks the cursor: a token that does not decode, or names another incarnation, is `Invalid` (EVT-045, EVT-055). A cursor encodes the incarnation token and the last event sequence returned, and stays valid across reopen. A page lists the events older than the cursor's sequence, whether or not an event has that sequence, so a cursor past every sequence lists from the newest. It is an encoding, not a signature: a forged one is not detected.

A page holds 20 events unless asked otherwise, at most 200, and stops before its data and metadata, as stored, would pass 3 MiB; exactly 3 MiB fits. It holds at least one event if any is left, so a larger event is a page of its own (D73). `next` continues after the last event whichever limit ended the page, and only the last page has none, so a page shorter than its limit is not necessarily the last. Names, keys and the other fields do not count. The read stops with the page: the read store loads no event past it. Of the event that ended the page it takes the size, for which SQLite reads the way to its row and none of its overflow pages, and it asks for no older one.

## Keys

| Part | Rule |
|---|---|
| Text | `ixk_` + the key ID's 36-character text form + `_` + 43 base64url characters without padding (256 random bits): always 84 characters, parsed by position, since the secret may hold `_` and `-` (D46). Anything else is malformed, including an uppercase or non-v7 key ID and a non-canonical last character |
| Key ID | The key's UUIDv7. Not a secret |
| Stored | The `KeyDigest` of the whole key text, and the environment ID and token current when the key was made. The secret is never stored, logged or returned after creation |
| Valid | Parses; a record exists for the engine's environment ID and token; digests match, compared in constant time; not revoked; not expired: `now < expires_at` when an expiry is set, by the clock the engine was opened with, and `expiry_recorded` unset |
| Scopes | A non-empty set of `read`, `write`, `admin`, none implying another (D27). The bootstrap key holds all three |
| Created | By a key holding `admin`. Scopes default to `{read}` when none are asked for, and must be a subset of the creator's. The expiry must be after now and no later than the creator's; no expiry only when the creator has none (D28). A label is 0–128 bytes of UTF-8 with no control character (Unicode Cc). At most 1,000 created keys per environment hold a slot: a key holds one until it is revoked or its expiry is recorded, whatever the clock reads. All checked inside the transaction, with now read after the write lock, the creator's record read again there |
| Origin | `Minted` for a bootstrap key the engine minted; `Seed` for a key the host supplied; `Recovered` for one recovered from the data directory, which is labelled `recovered`; none of these has a creator or holds a slot. `Created` keys name their creator in `created_by` (D35) |
| Status | `Revoked` when `revoked_at` is set, whatever the expiry; otherwise `Expired` from `now ≥ expires_at`, by the clock, or once its expiry is recorded; otherwise `Active` |
| Revoked | `revoked_at` is set; the row stays, so every `key:` emitter names a row (D35). Revoking a revoked key succeeds and keeps the first time |
| Order | Keys list newest first by `seq`, a counter assigned under the write lock, so keys made in the same millisecond never tie. A page holds 100 keys unless asked otherwise, 1,000 at most; its cursor is `<incarnation token>.<seq>`, as event cursors are |

**Admission.** Every call runs, in order: no key, or an empty one → `Unauthenticated`; `parse`; `lookup_key(id, env)`, a read-only lookup with no cache (D45) that then reads now, both under the control plane's lock, so no recorded expiry falls between them; `verify` at that now, and `record_expiry` when `Refused::newly_expired()` is true; `admit` against `required_scope(op)` (D37). Only then does the handler run, and it reaches the ledger only through `ledger(&Authorized)` and the key records only through `keys(&Authorized)`. Admission's one write is that flag. Of an open `Engine`'s methods, besides `close`, only the host's `health` and `faulted` reach either without a key, and they reach no event and no key record.

**Recorded expiry sticks.** A key is expired when `now ≥ expires_at` or its `expiry_recorded` flag is set (D42). The flag is set, once, when the key is refused as expired with a matching digest and not revoked (`record_expiry`), or when a key change finds its calling key newly expired, in that change's transaction. A key whose expiry was recorded is never accepted again, whatever the clock does later, across restarts too. If recording fails, the call still gets the one uniform refusal, but nothing is recorded, unless its commit is unsettled and lands (D48): until the unsettled commit lands or a later refusal records it, a clock set back can accept that key again. A clock that runs ahead and is corrected expires for good only the keys refused while it ran ahead; a recorded expiry does not spread to the keys a key created, so a child can stay usable until its own expiry. `list_keys` reports status by the clock and the flag and records nothing, so a key listed as expired can list as active again after the clock is set back. Event time is never compared with key time (LED-1).

**Revocation and calls in flight.** A revoke, or the revoke a seed's rotation makes, which has no calling key, takes effect when its transaction commits: a call whose `lookup_key` runs after it is refused; a call that read the key before may finish. `insert_key` and `revoke_key` take the write lock, read now, then read their calling key's record again and `recheck` it: a key revoked or expired by then changes no key, except that a newly expired one has its own expiry recorded, and the call is `Unauthenticated` (D43). No lock spans the control plane and the ledger, so an emit admitted before a revoke may still commit after it. Key and identity operations do not touch the ledger, so they keep working while it is `Unavailable`. Cancelling a key change only drops its answer: once it is admitted it can still commit, even after `close`. A created key's secret is then lost and its slot held: list the keys and revoke it.

| Operation | Scope |
|---|---|
| `emit`, `emit_batch` | `write` |
| `get_event`, `list_events`, `list_environments` | `read` |
| `whoami` | any valid key |
| `create_key`, `list_keys`, `revoke_key` | `admin` |

## Errors

The kind is contract. Nine messages are contract too: `event name is required`, `event data exceeds maximum size`, `batch size {n} exceeds maximum of 100`, `event not found`, `authentication required`, `invalid API key`, `missing scope: <scope>`, `key limit reached` and `key not found`. Other messages are not.

| Kind | When | Retry |
|---|---|---|
| `Invalid` | A rule refused the request, or one batch item; or `create_key` with more scopes than the creator, an expiry not after now or later than the creator's, or a bad label; or `list_keys` with a cursor that does not decode or names another incarnation | No: the same request is refused the same way, unless it names an expiry that a clock set back makes valid |
| `Invalid` | Through a door, before the key is read and whether or not the engine is open: a request the wire cannot carry ([Proto](#proto)). From the proto client, before it sends anything: the same, an expiry outside years 0001–9999, or data or metadata that is not UTF-8, on any item of a batch | No |
| `NotFound` | No event has that ID, or the ID does not parse | No |
| `NotFound` | `revoke_key` with an ID that does not parse or names no key in the environment: `key not found` | No |
| `Unknown` | The append's outcome is unknown; the ledger is now `Unavailable` | After reopen, only if it has an idempotency key, or every item of its batch has one. On the retry, a valid item whose key is already stored returns its original event, a new key stores one event, and a refused item is refused again. An item without a key may store a second event, so check first. The hint says which: of an `emit` by whether it has a key, of an `emit_batch` both |
| `Unavailable` | The ledger failed (append, fold, read store, a panic of its writer or of a `read` query, or a `read` that finds its runtime shut down before its query ran) and refuses every call until it is reopened. Reads waiting at the gate end with it | After reopen |
| `Unavailable` | A control-plane read or write failed during admission or a handler: `lookup_key`, `environment`, `list_keys` or a key change. The call fails closed and changes nothing, but for an [unsettled commit](#after-a-failed-open-or-change) (D48), so the hint of a `create_key` whose transaction failed says to list the keys before the retry, and to list them again after a reopen; the engine stays open (D44). A failure to record an expiry is not this: the call keeps its uniform refusal. A `create_key` whose answer was lost may have committed. Once a call has panicked under the control plane's lock (a host `Clock` that panics), every later call that reaches the control plane answers so until the engine is reopened, though its hint still says at once | Yes, at once; a `create_key` whose transaction failed, once the keys are listed, which show an unsettled one only after a reopen; after such a panic, after reopen |
| `Unavailable` | Through a door, the engine is not open yet: every call the wire can carry, with the hint `the engine is starting` or, while stopping, `the engine is stopping` | Yes |
| `Unavailable` | From the proto client, any call whose connection never opened, a server whose certificate is not trusted included: nothing ran | Yes |
| `Unavailable` | From the proto client, a read that no engine answered ([Wire](#wire), rule 2): the connection dropped, no answer by the client's timeout, a proxy's error, or an answer that does not decode or is over 16 MiB. The message names the HTTP status, the Connect code, or that the answer does not decode, and nothing of the body, which can hold event data | Yes |
| `Unknown` | From the proto client, `emit` or `emit_batch` in any of the cases of the row above; or a batch answered with another number of results than it has items | At once, but only with the same idempotency key, on every item of a batch; without one, check first. The hint says which, as the engine's own does |
| `Unknown` | From the proto client, `create_key` or `revoke_key`, likewise | A `revoke_key`, at once: the hint says so, and that a key which revoked itself is refused. A `create_key` once the keys are listed; after a reopen, list them once more. Its hint says so |
| `Unauthenticated` | No key, or an empty one: `authentication required` | No |
| `Unauthenticated` | A malformed key: `invalid API key`, with the key format in `hint` | No |
| `Unauthenticated` | An unknown key ID, a wrong secret, a revoked or expired key, or a key of another environment or incarnation: `invalid API key`, the same answer for each, with no hint, whether or not recording an expiry failed (D29, D30). Also a key change whose calling key was revoked or expired before its transaction (D43) | No |
| `PermissionDenied` | The key lacks the operation's scope: `missing scope: <scope>`, with `hint` saying to create or use a key with that scope (D31) | No |
| `ResourceExhausted` | `create_key` when 1,000 created keys hold a slot: `key limit reached`, with `hint` saying to revoke keys, expired ones first (D44) | After a revoke, or once an expiry is recorded |
| `ResourceExhausted` | From the proto client: a request over 16 MiB, which it does not send; or `resource_exhausted` without the engine's own error ([Wire](#wire), rule 4). No hint | No: a request too large stays so |
| `Invalid` | From the proto client: `invalid_argument`, `out_of_range` or `failed_precondition` without the engine's own error ([Wire](#wire), rule 4) | No |
| `Internal` | From the proto client: `unimplemented` without the engine's own error ([Wire](#wire), rule 4), with the hint `the engine is older than this client` | No |
| `Invalid` | From the CLI, before it sends anything: a usage error, or a `keys create` whose request repeats the key the CLI was given ([CLI](#cli)) | No |
| `Internal` | From the CLI: an answer it cannot write to stdout (one that is closed, or not open for writing, takes it and drops it), the hint of a `keys create` saying that the key is stored and its secret lost; one that repeats the key the CLI was given; or one whose JSON text is no JSON, which no engine stores | No |

| Kind | `retryable` | Exit | Connect code |
|---|---|---|---|
| `Internal` | false | 1 | `internal` |
| `Invalid` | false | 2 | `invalid_argument` |
| `Unauthenticated` | false | 3 | `unauthenticated` |
| `PermissionDenied` | false | 4 | `permission_denied` |
| `NotFound` | false | 5 | `not_found` |
| `Unknown` | false: send it again only with the same idempotency key, on every item of a batch, or check first; the hint says which | 6 | `unknown` |
| `ResourceExhausted` | false: something must change first | 7 | `resource_exhausted` |
| `Unavailable` | true | 8 | `unavailable` |

`ErrorKind::retryable()` is true only when the same request, sent again unchanged, may succeed and stores no second event (D70). `OpError::hint`, when there is one, says what to do; "retry at once" is wrong after a call panicked under the control plane's lock (above). `retryable()` promises no second event, not no second key: a `create_key` that answered `Unavailable`, as one that answered `Unknown`, may have stored its key, whose secret is then lost; its summary in the proto and its hint say so. List the keys before sending it again, and again after a reopen: a key whose commit is [unsettled](#after-a-failed-open-or-change) (D48) is listed only then, if it lands. Revoke the one no holder has.

`ErrorKind::exit_code()` is the exit code of a process that ends with that kind; 0 is success. `Internal` is a server fault (D69), or, from the proto client, an engine without the operation; no operation of the engine answers it yet.

## Commit and read

1. The writer processes one submission at a time (D15). Up to 16 more can wait in its queue. A submit waits asynchronously for a slot. Cancelling before admission leaves no job for the writer. Cancelling after admission, or timing out, only drops the answer: the write can still commit. Its outcome is unknown to the caller, so retry with the same idempotency key; no reopen is needed. For each submission it reads the event clock and calls `decide` against the read store, drawing IDs from a UUIDv7 generator.
2. Nothing to append (all refused, or all duplicates): it answers at once.
3. Otherwise it appends one record holding every new event and waits for the log's fsync, off the async threads. Each new event takes the next event sequence, from 1, in input order.
4. It publishes the new committed head to the read gate, then acknowledges. Then it folds the record, before it takes the next submission, so the read store holds every earlier record when `decide` runs.
5. A read snapshots the committed head when it arrives and waits until the applied position reaches it. So it sees every write acknowledged before it arrived, whoever made it.

If the writer panics, submissions still queued or being decided return `Unavailable`. A submission whose append began and lost its reply returns `Unknown`. Cancelling a read only drops its answer: a query that has started runs on, and fails the ledger if it panics or the read store fails.

**A ledger fault** is signalled, so a host needs no traffic to learn of it. `faulted` resolves once, when the ledger becomes `Unavailable`, whichever way it failed ([Errors](#errors)) and whether or not a call waits for an answer; at once if it has failed already. The future holds nothing of the ledger or the engine. It resolves for no refusal and no control-plane failure, and a close is no fault: what is pending when `close` begins resolves only if the ledger fails before `close` returns, as by the fold it finishes or a panic as the writer ends, and after that stays pending for good. A drop without `close` is the same. So once `faulted` resolves the ledger failed, and a host that closes its engine stops waiting for it. The engine does not reopen itself: the host closes it and opens it again.

`Engine::health` reports each part alone, as it was when read: the control plane by one read of the environment, which reads no key record, and then the ledger's state, so a ledger that failed meanwhile shows. It waits for the control plane's lock, which a key change waiting for another process's write lock holds for seconds. It has no deadline: a host that needs one sets it, and `ironflowx serve` sets none. A failed control-plane read leaves the engine open, and a later one may succeed, unless a call panicked under the control plane's lock ([Errors](#errors)).

The committed head and the applied position are exclusive positions: one past the last record, as the log's watermark is.

## Open

`open` syncs the parent of each directory it creates. It takes the ledger lock before opening the log and then the read store.

| Directory state | Result |
|---|---|
| Empty or missing, or holding neither a log nor `read.db`, whatever else it holds | Creates the log and appends the genesis record (format version, environment ID, incarnation token) at position 0, then creates `read.db` and folds it; ready |
| Log, genesis matches the `EnvRef` and format | Runs SQLite's `quick_check` over `read.db`, so open takes longer as it grows, and checks that the newest stored event ID and sequence match the last applied record, rebuilding on either failure. Replays one record at a time from the applied position |
| Log, genesis differs in environment ID or token | `OpenError::WrongIncarnation` |
| Log, genesis differs in format version, or the log's own format is unsupported | `OpenError::Format` |
| `read.db` missing, without `meta`, unreadable, its tables damaged, or `meta` disagrees with the genesis record | Deletes `read.db` and its `-wal` and `-shm` files, rebuilds from position 0 |
| Empty log and an existing `read.db` whose applied position cannot be read | `OpenError::Io` preserves the store for inspection |
| Empty log and a readable `read.db` with a positive applied position, for any incarnation | `OpenError::Corrupt` before writing genesis or replacing the store |
| The log ends in a torn last frame, after a crash | The log truncates it; opens with the records before it |
| `read.db` names this environment and its applied position is beyond the log's watermark | `OpenError::Corrupt`, before a genesis record is written: the fold follows fsync, so the log lost durable records: an older copy of the log restored, or, after a crash, a bad last frame the log truncated as a torn tail (a crash writes no close frame, so nothing after it shows it was acknowledged). Without `read.db`, that last record is lost unreported |
| A replayed record carries another token or format, or does not decode | `OpenError::Corrupt` |
| The record at position 0 is not a genesis record | `OpenError::Corrupt` |
| The log reports corruption (including a bad frame in its newest segment with a later record or a close frame after it), a gap or a foreign segment | `OpenError::Corrupt` |
| Another process or `Ledger` holds the directory | `OpenError::Locked`, including on sealed logs. The OS releases the ledger lock when the process exits |
| Any other I/O failure | `OpenError::Io` |

`open_existing_blocking` answers `OpenError::Missing` for a directory that is missing, or whose log is missing, has no segment or holds no record, whatever `read.db` beside it holds, and for a log that begins with a record of events instead of a genesis record, unless the `read.db` beside it is ahead of it, which is `Corrupt`. It creates neither the directory nor a genesis record. For any other directory the table above applies.

`Ledger::open` does not recognise an engine's root: opened there, it makes a ledger, and the engine answers `NotAnEngineRoot` while that ledger's `log/` is there.

The genesis record is the directory's identity; `read.db` is derived state and never trusted over it. The log binds every segment to one log ID, so a segment from another log does not open.

Dropping a `Ledger` without `close` stops the writer after the submission in hand; queued submits, never decided, end `Unavailable`. The drop joins the writer, so it blocks, and the lock is released before it returns; async code calls `close`. Dropping an `Engine`, or an `Opened`, without `close` drops its `Ledger` the same way, then releases the root.

### When open answers `Corrupt`

`Corrupt` is for a person to resolve; the ledger never repairs itself. It comes from one of three places, and all look the same to the embedder:

- **The log refused.** The segment log found damage it will not truncate on its own.
- **The ledger refused.** The log opened, but `read.db` names this environment and has folded records the log no longer holds, or the log is empty and any `read.db` records progress; then `read.db` is the last copy of those events. The fold runs only after fsync, so the log lost acknowledged events: after a crash, a bad last frame the log truncated as a torn tail (a clean close writes a close frame that proves the records before it; a crash writes none, so the log cannot tell damage from a torn write), an older copy of the log restored, a deleted segment, or a lossy repair (below).
- **The ledger refused a record.** The log is sound, but a record in it is not one this ledger wrote: another incarnation's token or format, a record that does not decode, or no genesis record at position 0.

Steps, with the embedder stopped:

1. **Copy the whole directory** before changing anything.
2. **Ask the log:** `segment-log-server repair verify` on `<dir>/log`. The `repair` subcommands, flags and exit codes are in the segment log's [Operator repair](https://github.com/ironflowx/segment-log/blob/v0.2.0/docs/architecture/api.md#operator-repair) section, the only source for them.
3. **If verify reports damage**, the log refused. Only damage in the newest segment is repairable: run `repair truncate`, a dry run that prints the plan, then apply it.
   - **Lossless plan:** reopen the ledger. It replays and opens.
   - **Lossy plan:** acknowledged records are dropped and the tool needs `--drop-acknowledged`, which leaves the log sealed. Go to step 4, then read the note on a sealed log.
     The segment log's repair class F ([segment-log ADR 0068](https://github.com/ironflowx/segment-log/blob/v0.2.0/docs/decisions/0068-open-refuses-damage-with-acknowledged-groups-after-it.md)) is a bad frame in the newest unsealed segment with a later record or a close frame after it. Its plan is always lossy: apply it with `--drop-acknowledged`. The tool refuses it (exit code 3) when the damage is in the second-newest segment, when the second-newest segment lacks its closing run, or when the seal run would reach the last acknowledged record after the cut and that record is a group frame; then restore from a backup. When the proof after the cut is a close frame, which every clean `close` writes, the plan copies it past the seal run and can apply. The segment log's section above is the authority on these cases.
   - **Other damage** (an older segment, the headers) is not repairable: restore the directory from a backup.
4. **If verify is clean**, or after a lossy repair, the ledger refused. Prefer restoring the directory from a backup at least as new as the last acknowledged emit. To accept the loss instead (if the log was empty, the step 1 copy of `read.db` is the only copy of those events: keep it), delete `<dir>/read.db` and its `-wal` and `-shm` files and reopen: the ledger rebuilds the read store from the log as it now is. This deletion is the one step that accepts lost events.
5. **If it still answers `Corrupt`** after the deletion, the ledger refused a record: rebuilding cannot get past it. Restore the directory from a backup taken before that record was written. The step 1 copy holds the same record and will not open; keep it for inspection.

A backup is a copy of the whole directory (of an engine, its whole `<root>`), `-wal` files included, taken while it is closed. Restoring an older `control.db` undoes the revokes and recorded expiries made since, so a key that was valid in the backup and revoked since may work again: revoke it again.

After accepting a loss, the lost events are gone for good, and so are their idempotency keys: a client's keyed retry stores a new event instead of returning the original. New events reuse the lost event sequences, so a cursor issued before the loss can page into events stored after it.

A sealed log, after a lossy repair, opens and reads until the first emit that appends. That append fails the ledger: the emit answers `Unavailable`, and so does every later call, reads included, until it is reopened (D19). An emit that appends nothing, a duplicate or a refusal, is answered as usual. Serve a sealed ledger read-only: do not emit to it. Foundation cannot unseal it. To take writes again, create a new environment (`new_env_ref`) in a new directory; foundation moves no events into it.

`WrongIncarnation` and `Format` are not damage: the `EnvRef` or the build does not match the directory. Open it with the `EnvRef` it was created with, or with a build that reads its format.

## Engine open

`Engine::open(root, clock, seed)` runs these steps in order (D32):

1. Parse the seed, if it is given one: a malformed seed is `MalformedSeed`, which names the key format and never echoes the seed. Nothing is read.
2. If `root` holds a directory `log/`, it is a ledger, not an engine root: `NotAnEngineRoot`. Nothing is created.
3. Create `root` if missing, open or create `<root>/engine.lock` and take its OS lock, held until `close`. Another holder: `Locked`. The file is never deleted, so every contender locks the same one.
4. Read the bootstrap state of `control.db` without creating it, refusing another format first; delete a leftover `ledgers/.staging/`, which never holds anything acknowledged; classify the root by the table below.
5. Run the open of that kind.

| `control.db` | `ledgers/` | Result |
|---|---|---|
| Another format version | | `Format` |
| Not a control plane | | `Store` |
| Missing, with nothing in it, or never bootstrapped | Missing, or empty but for staging and plain files | A bootstrapping open |
| Missing, with nothing in it, or never bootstrapped | Holds anything else: a ledger directory, or a symbolic link | `Orphaned`: the control plane was lost |
| An unmarked bootstrap | Anything | A finishing open |
| A marked bootstrap | The environment's ledger missing, emptied or beginning with a record of events, as `open_existing_blocking` judges it ([Open](#open)) | `LedgerMissing`: the ledger was lost |
| A marked bootstrap | The environment's ledger present | A later open; the ledger's [Open](#open) table applies, the `read.db` rebuild included, and its refusals are `Ledger` (a ledger of another environment or incarnation: `WrongIncarnation`) |

An open is one of three kinds (D41):

| Open | Does | `Opened` |
|---|---|---|
| Bootstrapping | `new_env_ref()`; `ensure_bootstrap`, one transaction creating the tenant, project and environment named `default`, with fresh UUIDv7s, and the bootstrap key holding all three scopes: the seed if one is given, else a minted key; build the ledger in `ledgers/.staging/`, close it, rename it to `ledgers/<environment-id>/` (D40) and open it there; then `mark_ledger_created`, the last fallible step, so no failure after it can lose the secret (its own commit, unsettled, can: D48) | Without a seed, `minted`: the bootstrap key's secret. With one, `seed: Current` |
| Finishing | Open the ledger at its final name if anything is there, else build it as above; `check_seed`, if a seed is given, which refuses `SeedConflict` before `control.db` is opened to write; `finish_bootstrap`, one transaction that marks, revokes a minted key, whose secret no open returned, and, without a seed, stores a fresh minted key in its place; then `apply_seed`, if a seed is given | `lost_secret` if it revoked a minted key; `minted` if it stored a replacement; `seed` from `apply_seed` |
| Later | `open_existing_blocking` on the ledger; `check_seed`, if a seed is given, as above; then `apply_seed`, if a seed is given | `seed` from `apply_seed` |

**Before the mark, a bootstrapping open that fails tries to undo itself:** it deletes the staging directory, if it can, and, unless something is at the ledger's final name, the rows it inserted, in a transaction that checks they are unmarked. If the ledger is there already, the rows stay; if the undo fails, so do any it did not delete. Rows that stay are an unmarked bootstrap, whose seed, if it was given one, is current; a finishing open completes it. **A finishing or later open undoes nothing:** a failure leaves what it did before failing: on a finishing open, the ledger if it built it, and, if the mark committed, the mark, with the minted key revoked and any replacement stored. The previous seed stays current, unless the failure is an unsettled commit that lands (D48). The engine never deletes a directory at a ledger's final name.

**What a refused open may change:** a layout refusal (steps 1–4, `LedgerMissing`, a ledger of another incarnation) changes nothing but the lock file and the removal of a leftover staging directory. `SeedConflict` changes no key and writes nothing to `control.db` or its WAL, but the ledger opened before it may have rebuilt its `read.db`, or, on a finishing open, been built. A finishing open checks its seed before the mark, so a conflict leaves the bootstrap unmarked. After a crash, or with a damaged ledger, some recovery comes before a refusal can: reading a WAL rebuilds `control.db-shm`, which SQLite derives from it; and a log that has a segment is opened, making its lock files, before its genesis record can be read. As it opens, it drops a torn tail or a torn segment header, which hold nothing acknowledged but for the bad last frame the [Open](#open) table names, and finishes an interrupted roll; closed by the refusal, it may get a close frame. If it holds a record, its `read.db`, if there is one, is opened too, which may checkpoint that file's WAL. No refusal writes a genesis record, but for the ledger a finishing open builds before its `SeedConflict`.

**A cancelled open** may still finish the step it was in. Each step, the ledger's own open and the staging build included, holds the root's lock until it ends and what it made is closed, so no later open runs beside it. It starts no undo: it leaves no bootstrap, an unmarked one or a marked one, as its last commit left it.

**The first secret is returned at most once.** A finishing open that succeeds reports `lost_secret` for the minted key it revoked and, without a seed, returns a replacement; a seeded bootstrap has no minted key, so its seed stays the bootstrap key unless the open is given another. If an open's mark commits, bootstrapping or finishing, and it does not return, since it dies, is cancelled or, finishing with a seed, fails at that seed, or if a mark it answered as failed is unsettled and lands (D48), any minted secret or `lost_secret` it would have returned is lost: no later open reports it, and nothing can tell. A lost secret's key stays listed with origin `Minted`. The host, which knows what it saved, recovers an admin key and revokes that one with it.

**Recovery.** `recover_admin_key(root, clock)` stores a fresh admin key, with all three scopes and no expiry, of origin `Recovered` and labelled `recovered`, and returns its secret, once (D34). Reaching the data directory is the authority. It is a host's function, not an operation: it takes no engine lock and works with or without a running engine, whose next call accepts the key, since every call reads keys from the control plane. With it, revoking the last admin key, your own included, needs no guard.

It is refused, `RecoverError::NotReady`, unless the bootstrap is marked. `insert_recovered` checks that in the transaction that inserts, and an undo checks "unmarked" in its own, so a recovered key is never undone (TEN-14). With no `control.db`, or an empty file with no WAL, it creates and changes nothing; on an unmarked one it waits for a mark under way, which it then follows, and stores no row otherwise. After a crash, a refused recovery can still change what SQLite keeps: reading a WAL rebuilds `control.db-shm`, as for a refused open, and on an unmarked bootstrap, which it opens to write, the WAL is checkpointed into `control.db`. `control.db` in another format is `Store(Format)`. A recovery and an engine's writes wait a bounded time for each other's write lock; the one that gives up fails and changes nothing.

The current seed is the one unrevoked key of origin `Seed`; there is at most one. A seed must parse as a key; nothing else about it is judged, and it is dropped once parsed, its digest alone going on. `apply_seed` runs in its own transaction, on a finishing or later open, after the ledger opens, so a failed open rotates no seed, but for an unsettled commit (D33, D48, TEN-9):

| Seed | Result | `Opened.seed` |
|---|---|---|
| None | Nothing changes | None |
| The current seed | Nothing changes | `Current` |
| A revoked seed, same key ID and digest | Nothing changes; it stays revoked, since a revoked key never returns | `Revoked`: the host should warn |
| A seed with a new key ID | Revokes the current seed, if any, and inserts the new one, with all three scopes and no expiry; other admin keys are untouched | `Current` |
| A seed whose key ID is a key with another digest, of another origin or of another incarnation | Refused: `EngineError::SeedConflict`; no key changes | |

### After a failed open or change

SQLite writes a commit to `control.db-wal`, syncs it, then shows it in the WAL index. If it fails after the write, at the sync or after it, the commit answers an error and is rolled back, but its frames are in the file: it is **unsettled** (D48). No open connection sees it. The next open of `control.db` with no other connection on it, an engine's, a recovery's or a refused one's, replays the WAL and may find it landed, whole. Two things clear it first:

- a later write, while a connection is still open, that overwrites its frames. A key change that succeeds on the same engine does, if it writes; one that errors too may leave the first to land, or land itself. A retried open or recovery can be the open that finds it;
- the close of the last connection, if it can checkpoint. While the disk still fails it may not, so no crash is needed.

This holds for every change TEN-11 lists, and for the schema an open creates, which lands empty and is bootstrapped as any empty file. A change that fails before its commit, or at a constraint its commit checks, is not unsettled. What is found when one lands, and what the host does:

- **A bootstrapping open's mark** (`mark_ledger_created`): the bootstrap is marked and no later open reports it. Without a seed, its minted key is active and its secret lost: recover an admin key and revoke the minted one with it. With one, open with that seed again.
- **A finishing open's mark** (`finish_bootstrap`): the bootstrap is marked and no later open reports it. A minted key, if the bootstrap had one, is revoked and, if that open had no seed, its replacement is the active minted key, its secret lost: recover an admin key and revoke it. A seeded bootstrap keeps its seed. If that open had a seed, open with it again.
- **A bootstrap** (`ensure_bootstrap`), its open's undo having found nothing to delete: an unmarked bootstrap, which a finishing open completes.
- **An undo** (`undo_bootstrap`): no bootstrap; the next open makes one.
- **A seed's rotation** (`apply_seed`): the new seed is current and the one before it, if any, revoked. Open with the new seed again; the old one now answers `Revoked`.
- **A created or recovered key** (`insert_key`, `insert_recovered`): an unrevoked key whose secret no call returned. List the keys and revoke it. A key's text holds its ID, so the one to revoke is the one no holder has; where the holders cannot be asked, replace the candidates before revoking them.
- **A revoke** (`revoke_key`): the key is revoked, its `revoked_at` the time the failed call read. Retry the revoke, so that the key is revoked either way.
- **A recorded expiry** (`record_expiry`, or a key change refused as its calling key expired): the expiry is recorded.

Seen with a fault-injecting VFS that fails the WAL's sync (macOS, [#135](https://github.com/ironflowx/foundation/issues/135)); no test in the suite produces it. A power loss and Linux were not tried.

## What is stored

The engine and the ledger set no file modes, so the process umask applies, and encrypt nothing: the host keeps the directory private. `ironflowx serve` creates it, and what is missing above it, asking for mode `0700`; a directory that is there keeps its mode. A umask that takes one of the owner's bits leaves a new one narrower, and a start whose permissions then keep it from opening the engine exits 1 (root's usually do not). On Linux a default ACL of the parent's stands in for the umask, and a setgid parent passes its bit on. On macOS an inherited ACL entry can grant access beyond the mode bits, to `admin.key` too if it propagates to files. Who can read it reads every event; who can write it has an admin's authority. A ledger has no delete and no retention.

An engine's data directory, `<root>`:

| Where | Holds |
|---|---|
| `<root>/engine.lock` | The engine's lock file; never deleted |
| `<root>/control.db` (SQLite, WAL, `synchronous=FULL`, `fullfsync`) | `tenants` (id, name, created_at) · `projects` (id, tenant_id, name, created_at) · `environments` (id, project_id, name, token, created_at, ledger_created_at) · `keys` (seq, id, environment_id, environment_token, scopes, digest, origin, created_by, label, expires_at, expiry_recorded, revoked_at, created_at) · `meta` (format version, 1: a change to this schema changes it, D47). Times are `Timestamp`'s `i64` milliseconds (D18); the digest is `KeyDigest::to_stored()`; no last-used time (D38) |
| `<root>/ledgers/<environment-id>/` | The environment's ledger, laid out as below |
| `<root>/ledgers/.staging/` | A ledger being built; deleted by any open that locks the root and reads its control plane |

What `ironflowx serve` adds to it:

| Where | Holds |
|---|---|
| `<root>/admin.key` | The minted admin key and a newline, mode `0400`. Never overwritten. Deleting it is the operator's call |
| `<root>/admin.key.<random>.tmp` | The temporary file used to publish the key, mode `0400`. A leftover may hold the only copy. The host warns once and keeps it. A warning identifies its path when safe |
| `<root>/.admin-key-probe.<random>` and its `.link` | Dummy zero bytes only, mode `0400`. A live probe removes its own entries unless the OS refuses cleanup, in which case the error names the paths |

**Where a secret may appear.** A served engine's key text reaches only these places: `admin.key` and the temporary file it is linked from, the `Authorization` header of a call, the `CreateKey` answer on the wire, stdout of `keys create` and `seed generate`, and the variable or file given to the CLI or host. A key's text sent where no key goes is data, as any: a label is stored and listed (the CLI refuses to create a key whose label holds its own), and a text that does not decode can be repeated to its sender.

A ledger's directory, `<dir>`:

| Where | Holds |
|---|---|
| `<dir>/ledger.lock` | File used for exclusive ownership of the ledger directory, including during read-store rebuild |
| `<dir>/log/` (segment log) | Position 0: the genesis record, with the ledger's own format version, 2 (D39; a format-1 directory from 0.1 answers `Format`). Then one record per submit that appends: format version, incarnation token, emitter, event-clock time, and for each new event its sequence, ID, name, version, key, metadata and data. Binary and length-prefixed; data and metadata are the bytes as sent |
| `<dir>/read.db` (SQLite) | `events` keyed by sequence, unique by ID, each with its `emitted_by` · `idempotency` key → event ID · `meta`: applied position, environment ID, incarnation token, format version |

A record's worst case is 100 × (about 600 bytes of fixed fields and lengths) + 4 MiB of data and metadata, under 4.1 MiB.

## Proto

Package `ironflowx.v1`, in `crates/ironflowx-proto/proto/ironflowx/v1/`, so the published crate ships it. `build.rs` compiles it with `protox` only when a wire feature is on, and needs no `protoc` and no `buf` (SRV-14).

| File | Holds |
|---|---|
| `identity.proto` | `IdentityService`: `Whoami`, with its request and answer |
| `environment.proto` | `EnvironmentService`: `ListEnvironments`, likewise, and `Environment` |
| `event.proto` | `EventService`: `Emit`, `EmitBatch`, `GetEvent` and `ListEvents`, likewise, `Event`, which is `GetEvent`'s answer, and `EmitBatchItem` |
| `key.proto` | `KeyService`: `CreateKey`, `ListKeys` and `RevokeKey`, likewise, and `KeyInfo` |
| `options.proto` | The custom options, which the generator reads |
| `error.proto` | `ErrorInfo { kind, message, hint }`, the error a call fails with |

| Option | On | Says |
|---|---|---|
| `(ironflowx.v1.cli)` | Method | The command, as words in kebab case: `"events emit"`. Required |
| `(ironflowx.v1.summary)` | Method | One line about it. Required |
| `(ironflowx.v1.example)` | Method | One request, as wire JSON. Required |
| `(ironflowx.v1.json_text)` | Field | A `string` that holds JSON text |
| `idempotency_level = NO_SIDE_EFFECTS` | Method | A read: Connect may call it with GET |

**Generator.** `ironflowx-cli`'s `build.rs` gives `generate` the descriptor set, so a proto it refuses is a build that fails (D52). It refuses:

- a method without a `cli`, a `summary` or an `example` (a blank one is none), a `cli` that is not kebab-case words, a `summary` of more than one line, and a command that is another's, or the group another is in;
- an example that is not JSON, that its request's schema does not allow, or that does not decode as the request (SRV-12). The decoder also checks the cardinality of a `oneof`. The schema is JSON Schema 2020-12, with `additionalProperties: false`: no field the request does not have, at any depth. It requires none, a time is in whole milliseconds, and of a `json_text` field it asks only a string, whose text it does not read;
- a field, in any message of ours, whose `json_name` is not its own snake_case name (D78);
- `json_text` on a field that is not a `string`, and a type it has no schema for: today bytes, floats, enums, maps and Google's messages other than `Timestamp`.

It does not know the hand-written commands: one that takes a name of theirs, or starts with it, panics each start of a debug build, the tests' included.

Nor does it know the CLI's own flags: a scalar field of a request named `help`, `json`, `key_file` or `url` panics each run of its command, likewise.

It writes `cli.rs`: each command with its RPC name, summary, input and output schemas, example, the fields of its request and answer (name, kind, whether a list, default), and its call through `Operations`, which decodes the request from wire JSON.

| Rule | Is |
|---|---|
| Names | `Operation::rpc()` is `<package>.<Service>/<Method>`, and the methods of the proto are those (D49) |
| Field names in JSON | The proto's snake_case names: every multiword field sets `json_name` to its own name (D78). A camelCase name is an unknown field. The generator refuses a field without it |
| Unknown JSON fields | The door ignores them, as protobuf does; the CLI refuses them in `--json` (D79) |
| A JSON array for a message | Read as the message's fields in the proto's order (`[["read"]]` is `{"scopes":["read"]}`), by the library; then checked as any |
| Event data and metadata | `optional string` fields with `json_text`: the JSON text itself, so bytes, key order and large integers survive every codec (D54, SRV-5). A client sends `"data": "{\"id\": 1}"`; a JSON value that is neither a string nor `null` does not decode. An event's `data` is always there |
| Presence | A field whose absence means something (`data`, `metadata`, `version`, `idempotency_key`, `expires_at`, `label`, `created_by`, `limit`, `cursor`, `next`, `hint`) is `optional`; absent, and in JSON `null`, is `None` |
| 64-bit integers | Strings in JSON, as protobuf prints them: `"version":"1"`. A number is read too |
| A batch item's result | `oneof result { EmitResponse ok; ErrorInfo error; }`: one per item, in their order. `Client` takes no other number of them |
| Who emitted | `emitted_by` is the `Emitter`'s text: `key:<key-id>` or `embedder:<name>` |
| Times | `google.protobuf.Timestamp`, RFC 3339 in JSON, in whole milliseconds and in years 0001–9999. What the library reads as one is one: it also takes a `+` in place of the first digit of a part (`2027-+1-15T08:00:00Z`). A stored time outside years 0001–9999, which only an in-process caller or a wrong clock can make, is sent as the nearer bound, `0001-01-01T00:00:00Z` or `9999-12-31T23:59:59.999Z` (D76); the stored value still decides expiry |
| IDs | Strings in their text form; an environment travels as its `EnvRef`'s, `env`, never a bare ID (LED-9) |
| Addresses | The text `project/environment`. The client splits it at the first `/` for `Address::new`, so it prints back exactly, and never makes an ID from it (D75, TEN-8) |
| Scopes | `repeated string`: `read`, `write`, `admin`; none asked for is `{read}` (D71) |
| A key's origin and status | Strings: `minted`, `seed`, `recovered`, `created`; `active`, `expired`, `revoked` |
| A created key | `CreateKeyResponse.key`, the one field of any message that holds a key, marked `debug_redact`: built only to send, or to print as the answer of `keys create`, and read once, into a `Secret`, with its ID the answer's `key_id`; its `Debug`, and its view's, print none of it (TEN-3) |
| What the wire cannot carry | `Invalid` before the key is read (D74): a `limit` of 0, a scope that is none of the three, a time with a fraction finer than a millisecond or outside years 0001–9999. A text that is not UTF-8, in any field, and a JSON text that is no such time (year 10000, ten fraction digits), do not decode, so they are the library's `invalid_argument`, with no `ErrorInfo`, whose message can repeat the text to its sender. `Client` sends no data or metadata that is not UTF-8, and no batch that holds one |
| Errors | `ErrorInfo`'s `kind` is the kind's name in snake_case (`permission_denied`). An `ErrorInfo` of a kind its reader does not know is no such error: what the client makes of its answer is [Wire](#wire)'s rule 2 |

## Wire

| Part | Rule |
|---|---|
| Protocols | One handler answers Connect (JSON and binary), gRPC and gRPC-Web, over HTTP/1.1 and cleartext HTTP/2. Connect is the documented protocol (D50). No TLS and no compression |
| Key | `Authorization: Bearer <key>` and nothing else: no other header, cookie or query parameter is read (D55). The scheme is case-insensitive (RFC 9110). No header, or `Bearer` with no text, is a call with no key. Any other value (another scheme, a key with no scheme, two `Authorization` headers, bytes that are not text) is a malformed key, answered as such by `Operations`: the door passes on a text no engine accepts, never the value |
| A key no header holds as it is | One with a byte outside visible ASCII is not well formed. The client sends a malformed key in its place, so the engine answers both alike |
| Size | A request body and a message are at most 16 MiB each, so on gRPC and gRPC-Web, whose body holds a 5-byte envelope too, a message is at most 16 MiB less 5 bytes; over either, the library answers `resource_exhausted` and nothing of it is stored. HTTP/1.1 then closes the connection, so a client still sending can lose that answer (`connectrpc`'s own did, of a body of 17 MiB and of 40, not each time): `Client` sends no message over 16 MiB, and answers `ResourceExhausted` itself. `Client` accepts an answer of 16 MiB. A page of events at each limit of the event rules (200 events, 3 MiB, the longest names and keys) is under 3.2 MiB in binary, and in JSON, where escaping can double data and metadata, under 6.5 MiB: a gRPC client with the common 4 MiB receive limit, in its usual binary encoding, needs no setting, and **a Connect JSON client with a 4 MiB limit raises it**, to 16 MiB, as does one of gRPC in JSON (D73) |
| Timeouts | A connection sends its first request's head within 10 s, and on HTTP/1.1 each later one within 10 s of its first byte. A later head that arrives with the request before it (as far as the host has read it when it answers that one), and later HTTP/2 heads, count as idle; no idle limit of its own (a proxy sets one); `Connect-Timeout-Ms` is honored; no deadline of its own, on `/ready` either |
| Door | Holds no rule: decode, convert (`TryFrom`), call `Operations` once, convert back (SRV-1). A request that does not decode is `invalid_argument` before its key is read |
| Errors | The kind's [Connect code](#errors), the message, and `ErrorInfo` as a detail, on every protocol. gRPC carries all of it in trailers, the message three times and once percent-encoded, so an error whose trailers pass its client's limit on them loses its `ErrorInfo`, then fails: `connectrpc`'s own client reads a message of 4,096 ASCII letters whole, loses the `ErrorInfo` of 4,096 `%` or 8,192 ASCII letters, and fails at 16,384. No error of the engine is that long. Any other answer without `ErrorInfo` is the library's or a proxy's |
| While opening or draining | Every call the wire can carry ([Proto](#proto)) answers `Unavailable`, hint `the engine is starting` or `the engine is stopping`. Wire decoding and conversion still precede the slot check |
| Panics | The dev and release profiles build with `panic = "abort"` (D63). A panic ends the process. A writer panic can abort before it signals a ledger fault. The infallible event conversion can panic on a damaged `read.db` row with non-UTF-8 data or metadata. It reports a fixed message with no event bytes. Normal writes validate the text before storing it (LED-5). A restart alone can leave such a damaged row in place |

`Client` speaks Connect binary. **How it classifies a failure** of a call it sent or tried to send (its own refusals are in [Errors](#errors)), first match wins (D56, SRV-4):

1. The answer is a failure whose body is an error object of Connect's, in UTF-8, with no byte below 0x20, and its first `ErrorInfo` is of a kind it knows: that `OpError`, exactly. Such a body begins with `{` and holds `code`, `message` and `details` and nothing else, `code` and `message` texts, `null` or absent, and each detail an object, with fields of its own or not. The status, the type and the encoding it came with do not matter, nor its code: the detail decides. The engine writes it so, and a proxy that adds a field beside those three, or prints it over lines, loses it: a write is then `Unknown`, a read `Unavailable`.
2. The request may have reached the server and no valid answer came back: the connection dropped, the deadline passed (while it was still connecting, too), a proxy answered with an error, or a success did not decode or was over 16 MiB. `Unknown` of an operation that changes state (`Operation::changes_state`), `Unavailable` of a read.
3. The connection never opened, as the transport's error tells (`is_connect`), not its text: `Unavailable`. A TLS handshake that fails is one that never opened.
4. A Connect code without `ErrorInfo`, from the server's own library: a failure whose body is such an object, of type `application/json` (alone, or with `charset=utf-8`), with no encoding, no detail, and a `message` that is a text, not `null`, if there, that names one, under the HTTP status a Connect server gives that code (its own; `unimplemented` also 404, of a method the server does not have). `invalid_argument`, `out_of_range` and `failed_precondition` → `Invalid`; `resource_exhausted` → `ResourceExhausted`; `unimplemented` → `Internal`, hint `the engine is older than this client`; every other code → rule 2. The engine's own `unauthenticated`, `permission_denied` and `not_found` always carry `ErrorInfo`, so without it they are a proxy's, as any other failure is: a bare 401, 403, 404 or 503, or a code under another status. One with a detail, such as `ErrorInfo` of a kind it does not know, is rule 2 too: the library attaches none.

A success that is not Connect's (a 2xx other than 200, or of a type other than `application/proto`) is rule 2's: an empty one would decode as an answer, as an empty one of Connect's does where nothing is required: a revoke's, or a list that holds nothing. A message made from an HTTP status names it, and any other the Connect code, or that the answer does not decode: none holds anything of the body, since what answered may be no engine, and its body can hold a key (TEN-3) or event data. The engine's own error is passed on as it is.

**Known limits**, all of an answer the client cannot tell from the engine's, so that what it says of a call may not be what became of it, a commit included:

- Rule 4 takes a proxy that answers in place of the engine exactly as a Connect server would, status, type and code alike, for one: where it dropped the engine's own answer, a commit it names refused has run. The answer can copy the request, where that holds nothing, as an empty `create_key` does.
- Rule 1 takes any well-formed `ErrorInfo` of a known kind for the engine's: one a proxy forged, cached, or replays from another call.
- An intermediary that copies the raw request, unescaped, having trimmed or replaced its control bytes, into an answer of only `code`, `message` and `details`, can make one of an event's name or idempotency key. A copy that keeps the bytes is told apart: every request that stores an event or changes a key holds one below 0x20, an empty `create_key` aside, and no error the engine's door answers `Client` with does.

With `client-tls`, `Client` calls an `https` URL as it does an `http` one. It trusts the roots of the system's store, so a private authority's works, and reads them once, in `Client::new`; `SSL_CERT_FILE` or `SSL_CERT_DIR`, where set, replaces the store, on macOS too. With no roots, no server is trusted. Without `client-tls`, an `https` URL is `InvalidUrl`.

## Host

`ironflowx serve` binds, opens the engine, and serves it. SIGINT, SIGTERM or a ledger fault
starts a drain. Every acknowledgment already followed `fsync`.

| Setting | Flag | Variable | Default |
|---|---|---|---|
| Data directory | `--data-dir` | `IRONFLOWX_DATA_DIR` | `./.ironflowx` |
| Listen IP address and port | `--listen` | `IRONFLOWX_LISTEN` | `127.0.0.1:9310` |
| Seed | `--seed-file` | `IRONFLOWX_SEED_FILE` or `IRONFLOWX_SEED` | None |
| Log level | `--log-level` | `IRONFLOWX_LOG_LEVEL` | `info` |
| Log format | `--log-format` | `IRONFLOWX_LOG_FORMAT` | `json` |

A flag beats its variable. A seed file and an inline seed are two sources and are refused together.
The host trims surrounding whitespace off a seed and validates it before binding. An empty variable
is a value and is refused. The host warns once for each unread `IRONFLOWX_*` variable, naming the
variable without its value. It hides names that could carry credentials or arbitrary bytes.
`IRONFLOWX_URL` and `IRONFLOWX_KEY` are known CLI variables. There is no
settings file.

**Start.** In order:

1. Read and validate settings and any seed. Bad settings, a malformed seed or an unreadable seed file exit 2.
2. If the data directory holds `admin.key` and `control.db` is confirmed absent, exit 2: a stale key file, from another install.
3. Bind the listen address. In use exits 8, any other bind error exits 2. No file has been written (D62).
4. Log `listening` with the bound URL, also when port 0 picked the address. An address that is not loopback also logs a warning that keys travel in clear without a TLS proxy (D59).
5. Serve `/health` and the door, the slot `Starting`.
6. Create the data directory, and what is missing above it, asking for mode `0700`. Warn once about each leftover `admin.key.*.tmp` and keep it. Only a confirmed unmarked bootstrap with an occupied `admin.key` is stale (exit 2). For a seedless start, confirm the unmarked state after seeing the name is occupied, since another host can mark and publish concurrently. A seeded start leaves a bootstrap read error for the open. A seedless start on an unmarked or unreadable bootstrap checks the destination with `symlink_metadata`: an occupied name, including a dangling link, yields the stale refusal or the original read error, respectively. A lookup error other than not found is a refusal. When the destination is absent, probe before opening. If a healthy marked root has an occupied key path and the first read fails transiently, retry the start after the failure clears.
7. Open with `Engine::open(data_dir, HostClock, seed)`. Warn if it revoked an earlier minted key whose secret was lost, or if the supplied seed is revoked. Write a minted key to `admin.key` and log its path. On failure log the minted key ID and the instruction to run `ironflowx recover-key` and revoke that ID, close the engine and exit 1. Set the slot `Open` on success.

**Pre-mint probe.** Write and sync dummy data in the data directory, hard-link it, then remove
both probe-created names and sync the directory. Attempt cleanup of each created name even after
an earlier failure, and keep any existing entry that the probe hit in a collision. If the OS refuses
removal, name each failed removal by an exact safe absolute path or an exact filename relative to
the configured data directory. Any probe failure exits 1 before open, with nothing minted.

The host uses an absolute locator in these warnings and errors only when the configured directory
is absolute and the locator is unambiguous and safe to print. Otherwise it names the file relative
to the configured data directory. When an old temporary's name itself cannot be shown safely,
the warning says the name was withheld and directs the operator to inspect `admin.key.*.tmp`
in the configured data directory locally.

`is_bootstrapped` reads the control plane without creating a directory or database. It answers
`true` for a marked bootstrap and `false` for a missing control plane, a control plane that
has never been bootstrapped, or an unmarked bootstrap. The read takes no engine lock and does
not check the ledger. SQLite can rebuild `control.db-shm` from a WAL left by a crash.

**Writing a key file.** Write the key and a newline to a new file in the data directory, `admin.key.<random>.tmp`, mode `0400`, and sync it; hard-link it to `admin.key`, which fails if the name is taken, by a symbolic link to nothing too; remove the temporary name; sync the directory. A crash leaves either no `admin.key` or the whole key, and at most the temporary file. A crash after the open marked the bootstrap and before the link leaves no `admin.key` and no sign to the next open: recover an admin key and revoke the minted one.

**Health.** Plain HTTP `GET`, no key:

| Path | 200 when | Else |
|---|---|---|
| `/health` | The process serves | No answer |
| `/ready` | The slot is `Open` and `Engine::health()` reports both parts ok: `{"ready":true}` | 503 with `{"ready":false,"waiting_on":"open" \| "control_plane" \| "ledger"}`, the control plane first |

`/ready` asks the engine on each request and has no deadline of its own, so it can answer as late as `Engine::health` does ([Commit and read](#commit-and-read)): a prober sets its own.

**Stop.** SIGINT, SIGTERM or `Engine::faulted()` starts a drain (D63, D72).
The slot stops admitting and the listener closes. Existing connections can still receive
the stopping refusal while requests in flight drain. The host allows up to 10 seconds, including
time to flush responses and close connections, then drops outstanding request futures, including
HTTP/2 handlers. It marks the slot closed and awaits `Engine::close_with_state` before shutting
down the runtime. Closing joins the ledger writer, which finishes queued submissions and the fold; event
acknowledgments already followed `fsync`. A dropped read can still hold `read.db` after engine
close until its blocking query ends or the process exits. A detached control-plane write still
uses a single SQLite transaction (D48, TEN-11).

A signal during open drains the transport on the same deadline while open finishes. The host
publishes any minted key before it closes the engine.
A second signal received during shutdown exits 1 immediately. The operating system can coalesce
identical pending signals. A signal alone exits 0. A ledger fault present when close starts exits 8,
even when a signal started the drain. The ledger reads its state at entry to close using the
same progress gate that publishes faults. A control-plane failure makes `/ready` answer
503 `control_plane` and serving continues. Restart runs the ledger's usual open and replay.
A sealed ledger opens again and faults on any emit that appends (D19). Serve it read-only.

**Logs.** JSON lines on stderr by default, or text with `--log-format text`.
`--log-level` takes `off`, `error`, `warn`, `info`, `debug` or `trace`.
At `info`, the host logs one line per call, with the operation, duration, outcome kind and the
presented key's ID, if the key is well formed. The ID is recorded even for a key the engine refuses.
Before `Operations`, the door logs without a key ID. A dropped request awaiting admission has
outcome `cancelled`. A transport refusal before the door can determine a domain kind has outcome
`refused`. It labels an unknown RPC in
`ironflowx.v1` as `unknown` without recording its path. Logs carry no key, seed, event data or
metadata. In a diagnostic path or message, the host hides words that could be credentials and
escapes control characters. The refusal's envelope is the last stderr line, regardless of the log format or level.

### Exit codes

Host errors use the same table as the CLI, from `ErrorKind::exit_code()`.

| Exit | Kind | Host outcome |
|---|---|---|
| 0 | Success | Stopped by a signal |
| 1 | `internal` | Engine or ledger format, orphaned engine, missing ledger, wrong incarnation, corrupt ledger, data directory creation failure, failed pre-mint probe, key-file publication failure, signal registration failure. A second signal also exits 1 immediately |
| 2 | `invalid` | Bad settings, malformed or unreadable seed, a directory belonging to a ledger, a seed conflict, stale `admin.key`, bind error other than address in use |
| 3–7 | [Call errors](#errors) | Calls only |
| 8 | `unavailable` | Address in use, engine or ledger lock or I/O error, control-plane fault at open, destination lookup error, a ledger fault before close |

**Seed generation.** `ironflowx seed generate` prints one JSON object with `key` and `key_id`.
It indents the object on a terminal. It opens no engine and writes no file.

## CLI

`ironflowx <command>` calls an engine and prints what it answers. An agent in a shell branches on the exit code and reads JSON (D64, D65).

| Part | Rule |
|---|---|
| Commands | One per method of the proto, generated (D52): `environments list`, `events emit`, `events emit-batch`, `events get`, `events list`, `keys create`, `keys list`, `keys revoke` and `whoami`. Hand-written: `describe` and `version` here, `serve` and `seed generate` in the host |
| Engine | `--url`, else `IRONFLOWX_URL`, else `http://127.0.0.1:9310`. `https://` too, for an engine behind a TLS proxy ([Wire](#wire)) |
| Key | `--key-file <path>`, else `IRONFLOWX_KEY`, else none, with surrounding whitespace trimmed. Nothing else: the CLI never reads `admin.key` unasked (D61). No key, and an empty one, is a call with no key, which the engine refuses; a file or a variable that is there and empty is still the one that was given. Text that is not UTF-8 is a malformed key |
| Arguments | A flag for each scalar field of the request, its name in kebab case (`--event-id`); the flag of a repeated field may be repeated (`--scopes read --scopes write`), and a field that holds messages has none. Its value is what wire JSON reads from a string (text, a number, an RFC 3339 time) or, for a `bool`, `true` or `false`; one its field does not take is a usage error that names the flag, but for what the wire mapping or the engine refuses (a limit of 0, an unknown scope). A `json_text` field takes JSON text, also text that begins with `-`, or `@<file>`, the file's text, and sends it byte for byte. Any other value that begins with `-` is given after `=`: `--label=-x`. A field without its flag is absent |
| `--json` | The whole request, as wire JSON, in place of its flags, with one of which it is a usage error: the text itself, `@<file>`, or `-`, stdin. It is `Invalid`, and nothing is sent, if a message in it, at any depth, has a field that its type does not have, which the message names (`the request has no field 'items[1].idempotncy_key'`), where the door ignores it (D79); if a `json_text` field is anything but a string, `null` included, with the hint `data is JSON text: pass a string`; if a message is an array, which the door reads as its fields in order; or if it does not decode, as a field given twice does not. Any other field that is `null` is absent |
| Output | The answer as wire JSON on stdout: one line, or indented if stdout is a terminal. A `json_text` field is printed as the JSON value it holds, on one line, with no whitespace between its tokens: its values, the order of its keys and the text of its numbers and strings are as sent (D65). Every field that is not `optional` is printed, defaults included; an absent `optional` field stays absent. (A message field has no default: one that is not `optional` is always set, since the answer printed is mapped from the domain type, D53.) The wire is unchanged. `--help` prints text and exits 0: the one exception |
| Errors | One line on stderr, the envelope; its kind's [exit code](#errors); nothing on stdout, unless a write to it failed midway. What answered may be no engine, so a message or a hint that repeats 43 characters in a row of the key the CLI was given, as many as a secret has, is not printed, and the envelope says so in its place; nor is an answer that does: `Internal`, also one that holds the caller's own key with reason, as stored event data can. So `events get` of an event that holds the key, in any field, and `events list` of a page with one, print nothing, whole; the event stays stored, and a caller with another key reads it. A `keys create` whose request repeats the key is `Invalid` and not sent, since its answer holds the label and the new key's only copy. A repeat that was changed (its case, split, encoded or cut shorter) is not found. A message can repeat the URL's path, where no key goes (D55) |
| Usage errors | An argument that is missing, unknown or of the wrong form, a URL that is not `http[s]://host[:port][/path]`, a `--key-file`, or a file of `--json` or a `json_text` flag, that cannot be read, and what `--json` refuses, are `Invalid`, exit 2, and nothing is sent. The message is one line and names a bad argument, or a value in `--json`, its whitespace as single spaces, or an unknown field in `--json`, as it is, but `<hidden>` for a word or a field that holds a key's `ixk_` or is as long as a key's secret; it never holds what `--key-file`, `--url` or `IRONFLOWX_URL` was set to, nor the file `--json` names, which may be or hold a secret. A build with clap's `debug` feature, which prints each argument as it parses it, reads none: every start is `Invalid`, `clap is built with its debug feature` (TEN-3) |
| Deadline | 30 s a call, sent as `Connect-Timeout-Ms` and held to by the client too: after it, `Unavailable` for a read and `Unknown` for a state change |

**The envelope** of a failure: `{"code": …, "message": …, "retryable": …, "hint": …}`. `code` is the kind's name in snake_case, as in `ErrorInfo`; `retryable` is `ErrorKind::retryable()`; `hint` is absent when there is none.

**`describe`** works offline, without a key (D67). Its catalog contains operations only.
Their scope and whether they change state come from the same rules as the engine and the client.

| Call | Answers |
|---|---|
| `ironflowx describe` | `{version, protocol, operations: [{command, rpc, scope, summary, changes_state}], exit_codes: [{code, kind, retryable}]}` |
| `ironflowx describe events emit` | The named operation's entry plus `input`, `output` and `example` |

The schemas use JSON Schema 2020-12 and close each declared message object with
`additionalProperties: false`. `input` describes the canonical wire JSON for `--json`,
with `json_text` as strings and 64-bit integers as bounded decimal strings. Timestamp schemas
check the local year and whole-millisecond precision. Enable the validator's `format`
assertions to check calendar dates. The wire decoder also checks that
applying the UTC offset leaves the instant in years 0001 through 9999. Run the example with `--json @file` or pipe it to
`--json -`. The wire and engine also validate the request. `output` describes the CLI's
printed answer, requiring the fields it always prints and allowing arbitrary JSON values
inside `json_text` fields.

After `unknown`, follow the operation's summary and the [error's hint](#errors). In particular,
retry an emit only with the same idempotency key on every item, or check what landed first.
After `unknown` or `unavailable` from `keys create`, use `keys list` before retrying, and
again after an engine reopen. Revoke any key that landed whose secret was lost.

**`version`** prints `{"version": …, "commit": …, "protocol": "ironflowx.v1"}`, and asks no engine. `commit` is the one the binary was built from, or `unknown` if it was not built in a git checkout.

## Depends on

| Crate | Version | Used | Relied on |
|---|---|---|---|
| `segment-log-core` | Pinned exactly (D16), from crates.io (MIT) | `SegmentLog::open`, `append` → `Pending::wait`, `read(from, max)`, `watermark`, `stream_head` | See below |
| `rusqlite` | A caret range (0.43.x), `bundled` (D26), which compiles SQLite with a C compiler; its `libsqlite3-sys` links `sqlite3`, so an embedder's graph can hold no other version of it | Read store; the control plane | One transaction per folded record; one per control-plane change. `length()` of a blob reads none of its overflow pages (D73) |
| `serde_json` | Pinned exactly (D26); the 127 depth limit is checked against it | Event rules | D12 |
| `serde_core` | A caret range | Event rules' JSON visitor; the CLI's, of `--json` | D12 |
| `uuid` | A caret range; `v7` in `ironflowx-cell` and `ironflowx-operations` only | Event, environment, tenant, project and key IDs | |
| `getrandom` | A caret range (D26) | `new_env_ref`; a key's secret | OS random source |
| `sha2`, `subtle`, `base64` | Caret ranges | Credentials: the key digest, its constant-time comparison, the key text | Pure: `ci/purity/ironflowx-authz.allow` names everything they pull in, `libc` included (only `cpufeatures`' CPU detection on AArch64 uses it) |
| `zeroize` | A caret range | `Secret` is zeroed on drop | In `ironflowx-core.allow` |
| `tokio` | A caret range (D26) | `spawn_blocking` for open, reads, close, admission and control-plane calls; the submission queue; the read gate; the answer to a submission; the host's runtime and listener; the CLI's runtime | |
| `connectrpc`, `connectrpc-build` | Caret ranges (0.9.x), without default features: no gzip, no zstd | `ironflowx-proto`'s wire features and `ironflowx-doors`: the server and the client; code from a precompiled descriptor set | Connect, gRPC and gRPC-Web on the same handlers (D50) |
| `buffa`, `buffa-types` | Caret ranges (0.9.x) | `ironflowx-proto`'s wire features: generated messages, `Timestamp` | |
| `protox` | A caret range (0.10.x) | `ironflowx-proto`'s build, with a wire feature on | Compiles the proto in Rust; `encode_file_descriptor_set()` keeps custom options (D51) |
| `prost-reflect` | A caret range (0.16.x) | `ironflowx-codegen`: reads the descriptor set and its custom options, at build time | Keeps options it does not know (D52) |
| `jsonschema` | A caret range, without default features: it fetches nothing | `ironflowx-codegen`: checks each example, at build time | 2020-12, `format` checked (SRV-12) |
| `base64`, `http`, `serde` | Caret ranges | `ironflowx-proto`'s wire features: an error detail's value, the client's URL and header, the generated JSON | |
| `axum` | A caret range | `ironflowx-doors`: `/health` and `/ready` beside the RPCs | |
| `hyper`, `hyper-util` | Caret ranges | `ironflowx-bin`: the accept loop. `hyper-util` also `ironflowx-proto`'s `client` | hyper times an HTTP/1.1 head with the `Timer` it is given. The error of a connection that never opened `is_connect`, and no other is; `connectrpc` keeps it as its error's source. With another `hyper-util` than `connectrpc`'s, rule 3 ([Wire](#wire)) would end as rule 2 |
| `rustls`, `rustls-native-certs` | Caret ranges (0.23.x, 0.8.x) | `ironflowx-proto`'s `client-tls`, with `aws-lc-rs`, which compiles C | The roots of the system's store, or what `SSL_CERT_FILE` and `SSL_CERT_DIR` name; none, if there are none |
| `clap` | Pinned exactly, in two unpublished crates: a later one could keep its tracing and drop the backtrace that shows it, silently | `ironflowx-cli`, `ironflowx-bin`: the command line | A usage error's message is its text before the usage, if its context has one, and the footer, each by its last `\n\nUsage: ` and `\n\nFor more information`. With its `debug` feature, and only then, an error made from no text prints more than `error: `: a backtrace, by which `parse` knows that clap would print the arguments (TEN-3) |
| `tracing`, `tracing-subscriber`, `tempfile` | Caret ranges | `ironflowx-bin`: the log, the temporary key file | |

How the ledger uses the log:

- **Options:** `group_byte_cap` 8 MiB (the library's maximum, above the 4.1 MiB worst case), `pending_byte_budget` at least that, `linger` 0, since the writer sends one record at a time.
- **Records:** one stream, `events`; the genesis record expects `ExpectedRevision::Empty`, each later one `At(last revision)`. No log key: the ledger keeps its own idempotency.
- **Errors:** `CommitError::Indeterminate` is `Unknown`. `CommitError::Poisoned` and `AppendError::Poisoned` are `Unavailable`. Every other append or commit error (`Full`, `TooLarge`, `Invalid`, `Sealed`, `WrongRevision`, `KeyConflict`, `PartialRetry`, `Exhausted`) cannot happen by construction; if one does, the ledger treats it as a fault: `Unavailable` until reopened.
- **A sealed log:** the ledger never seals one; only the log's operator repair tool does. The library opens it without an error and cannot say it is sealed until an append, so `open` succeeds, reads work, and the first append is the fault above, after which reads answer `Unavailable` too (D19).
- **Names:** the log's `Record`, `Position` and `Committed` types stay private to `ironflowx-cell`. This repo calls them the log record and the log offset; the ledger's own terms are in [`GLOSSARY.md`](../GLOSSARY.md).
- **Durability:** `wait` returns after the record's `sync_data`, which is a full flush on Linux and macOS. Crash durability is claimed on both, the two platforms CI tests. Power-loss durability is unclaimed until the simulator proves it.
- **Close:** `close`, and dropping a `Ledger`, drop the log cleanly, so it writes a close frame after the last record (segment-log format version 4, [ADR 0071](https://github.com/ironflowx/segment-log/blob/v0.2.0/docs/decisions/0071-a-clean-drop-writes-a-close-frame.md)). The upgrade is one-way: once a build on `segment-log-core` 0.2 has opened a directory, a build on 0.1 answers `Format` for it.

The decisions behind this design: [`decisions.md`](decisions.md).
