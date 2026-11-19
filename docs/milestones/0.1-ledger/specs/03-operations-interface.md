# Spec 3: The `Operations` interface

Build-order step 3 of [0.1 Ledger](../README.md). Needs Spec 1.

## Problem Statement

Tests call the ledger in process today; from 0.3 a server and a remote client will call it over the network, and from 0.2 every call will carry a credential. Without one fixed interface, each caller would grow its own request shapes, and the behavior scenarios would have to be rewritten for each door.

## Solution

One trait, `Operations`, names the four things a client can do in 0.1: emit, emit a batch, get an event, list events. Its request and answer types are plain Rust types. Every caller, in process now and remote later, goes through it, so one set of behavior steps drives every adapter.

## User Stories

1. As a client, I want one operation to emit an event with a name, optional data, optional metadata, an optional version and an optional idempotency key, so that absence is explicit and never encoded as an empty value.
2. As a client, I want the emit answer to give the event ID and whether it was a duplicate, so that I can tell a retry from a new event.
3. As a client, I want one operation to emit a batch and get one result per item in input order, so that I can match results to what I sent.
4. As a client, I want one operation to read an event by ID, taking the ID as text, so that a malformed ID is answered "not found" and not a type error.
5. As a client, I want one operation to list a page of events, with an optional limit and an optional cursor, so that I can page through an environment.
6. As a client, I want a zero limit to be impossible to express, so that I cannot ask for an empty page by mistake.
7. As a client, I want every failure as an error kind and a message, so that I can decide whether to retry from the kind alone.
8. As a client, I want data and metadata passed as JSON text, so that the engine sees exactly the bytes I sent.
9. As an implementer of the 0.2 operation layer, I want every call to take an opaque call context, so that adding the credential does not change the trait's shape.
10. As an implementer of the 0.3 door, I want hand-written Rust message types, so that I can map a wire format onto them without the interface depending on that format.
11. As a test author, I want adapters to be generic over the trait, so that the same steps run against the in-process adapter now and a remote one later.
12. As a maintainer, I want no operation to take an environment parameter, so that every request names exactly one environment by construction (LED-7).

## Implementation Decisions

- **Module:** the `Operations` interface, in the interface crate. It depends on core only and holds no logic.
- **Trait:** four async operations (`emit`, `emit_batch`, `get_event`, `list_events`), each taking a call context and a request. Futures are `Send`. No `dyn` use in 0.1; adapters are generic (D1).
- **Messages:** the field lists in the architecture doc's "Interfaces" section. Optional fields are `Option`; data and metadata are JSON text; the list limit is a non-zero integer.
- **Call context:** an opaque value with no fields in 0.1 and one constructor for an anonymous call (D2).
- **Error:** an error kind from core and a message.
- **Defaults and limits are not here.** The version default, the page-limit default and clamp, and every check belong to event rules.
- **Names may change** in review; behavior may not.

## Testing Decisions

- This module is types and a trait, so there is little behavior to test on its own. A good test is one that fails if the interface stops being implementable or changes shape.
- **Compile test:** a trivial generic adapter implements the trait and is called through a generic function.
- **Public-API snapshot** (from Spec 1) covers the crate, so a changed field or signature is a reviewed change.
- The interface's behavior is proven through `LocalOperations` by the scenarios in Spec 4.
- **Prior art:** none in this repo.

## Out of Scope

- Any adapter: `LocalOperations` is Spec 4; the remote client is 0.3.
- Wire formats, status codes and serialization (0.3).
- Credentials and scopes (0.2).
- Operations beyond these four (0.4 onwards).

## Further Notes

- Decisions D1 and D2, and the message shapes: [`../architecture.md`](../architecture.md#interfaces).
- This is the seam the behavior suite runs through: [`../behaviors/README.md`](../behaviors/README.md).
