# Aegis Flow — Execution Model

## Status

Draft.

This document defines the first execution model for Aegis Flow.

It is based on the research completed so far:

- Temporal / Cadence
- Cloudflare Workflows
- PostgreSQL-backed execution systems

The purpose of this document is not to freeze the architecture forever.

It is to make the first implementation precise enough that we can test it, break it, and improve it deliberately.

---

## What I am choosing for v1

Aegis Flow v1 will use a PostgreSQL-backed execution model.

The main reasons are:

- PostgreSQL can be the authoritative source of workflow state;
- work can be claimed atomically;
- workflow state and new activities can be committed in one transaction;
- durable timers can be represented as persisted timestamps;
- the system can start without a separate broker or matching service;
- the execution model stays small enough to understand end to end.

I am deliberately **not** choosing deterministic workflow replay for v1.

Temporal proves that replay is a strong recovery model, but it also introduces a programming and versioning model that Aegis Flow does not yet need.

The first version will instead use:

```text
explicit workflow state
+
explicit activity state
+
append-only event history
+
durable database transitions
```

---

# Core model

Aegis Flow separates three things:

```text
Workflow
Activity
Worker
```

They are related, but they are not the same object.

## Workflow

A Workflow represents the long-running business process.

Examples:

```text
TrackTransaction
IndexBlockRange
WaitForConfirmation
GenerateReport
```

The Workflow knows what stage the process is currently in.

## Activity

An Activity is one unit of external or non-trivial work.

Examples:

```text
BroadcastTransaction
FetchReceipt
QueryBlockHeight
CallProvider
```

Activities may fail or run more than once.

## Worker

A Worker executes Activities.

Workers are disposable.

No Workflow depends on a particular Worker remaining alive.

---

# Workflow states

The first Workflow state model is:

```text
Created
Running
Waiting
Completed
Failed
Cancelled
```

A simplified transition model:

```text
Created
   |
   v
Running
   |
   +------> Waiting
   |           |
   |           v
   |        Running
   |
   +------> Completed
   |
   +------> Failed
   |
   +------> Cancelled
```

Important rule:

```text
Completed
Failed
Cancelled
```

are terminal states.

A terminal Workflow cannot silently become Running again.

If a future requirement needs reopening, that should be a separate explicit operation or a new Workflow.

---

# Activity states

Activities need a separate state machine.

The first version is:

```text
Pending
Running
RetryWaiting
Succeeded
Failed
Ambiguous
```

A simplified transition model:

```text
Pending
   |
   v
Running
   |
   +------> Succeeded
   |
   +------> Failed
   |
   +------> Ambiguous
   |
   +------> RetryWaiting
                |
                v
             Pending
```

The important state here is:

```text
Ambiguous
```

This is intentional.

A network timeout does not always mean the operation failed.

For example:

```text
broadcast transaction
        |
        v
RPC accepts transaction
        |
        v
response is lost
        |
        v
worker sees timeout
```

The engine does not know enough to classify this as `Failed`.

The Activity should move into a state that requires reconciliation.

---

# Why Ambiguous exists

Without an explicit ambiguous state, retry logic becomes dangerous.

A naive model would do:

```text
timeout
   |
   v
Failed
   |
   v
Retry
```

For an external side effect, that may be wrong.

Aegis Flow should instead allow:

```text
timeout
   |
   v
Ambiguous
   |
   v
Reconcile external state
   |
   +------> Succeeded
   |
   +------> RetryWaiting
   |
   +------> Failed
```

The exact reconciliation interface is not part of the first domain crate, but the state model must allow it.

---

# Source of truth

PostgreSQL is the authoritative source for v1.

A Worker may cache data while executing an Activity, but cached state is never authoritative.

A future message broker may help distribute work, but it will not own workflow truth.

The model is:

```text
PostgreSQL
    |
    +---- Workflow state
    |
    +---- Activity state
    |
    +---- Lease state
    |
    +---- Retry eligibility
    |
    +---- Event history
```

---

# Activity claiming

Workers pull work.

A Worker asks PostgreSQL for an eligible Activity.

An Activity is eligible when:

```text
state = Pending
AND available_at <= now()
```

The claim must be atomic.

Conceptually:

```text
begin transaction

find eligible Activity
FOR UPDATE SKIP LOCKED

mark Running
set worker_id
set lease_until
set lease_token
increment attempt

commit
```

Execution happens **after** the transaction commits.

The database transaction must not remain open while the Worker performs external work.

---

# Lease model

A Worker does not permanently own an Activity.

It receives a lease.

A claim contains:

```text
worker_id
attempt
lease_until
lease_token
```

Example:

```text
activity_id = act_123
worker_id   = worker_7
attempt     = 3
lease_until = 12:30:00
lease_token = 8f4c...
```

If the Worker disappears, the lease eventually expires.

The Activity can then be recovered.

---

# Why lease_token exists

A lease deadline alone is not enough.

Consider:

```text
Worker A claims Activity
        |
        v
A becomes slow

lease expires

Worker B claims Activity

A wakes up
        |
        v
A tries to report success
```

A stale Worker must not overwrite the current Activity attempt.

Completion therefore requires the active lease token.

Conceptually:

```text
complete Activity
WHERE activity_id = X
AND lease_token = token_from_worker
AND state = Running
```

If the Worker is stale, the update affects zero rows.

That protects engine state.

It does **not** undo an external side effect the stale Worker may already have produced.

That problem still requires idempotency or reconciliation.

---

# Lease expiration

An expired Activity is not immediately assumed to have failed.

The engine knows only that the Worker stopped proving ownership.

Recovery logic will decide what happens next.

For v1:

```text
Running + expired lease
        |
        v
RetryWaiting
```

unless the Activity policy requires reconciliation first.

For Activities with potentially ambiguous side effects:

```text
Running + expired lease
        |
        v
Ambiguous
```

may be safer.

This distinction should be configurable by Activity type or policy.

---

# Heartbeats

A long-running Activity may extend its lease.

Heartbeats are optional.

They should be used when Activity execution time is long enough that a fixed lease would be impractical.

Example:

```text
lease duration = 30 seconds

Activity takes 20 minutes

Worker heartbeat every 10 seconds
```

A short RPC request does not need heartbeats.

The engine should not require heartbeat traffic for every Activity.

---

# Retry model

Retry policy belongs to the Activity.

The first RetryPolicy should contain:

```text
max_attempts
initial_delay
max_delay
backoff
jitter
```

Possible backoff modes:

```text
Fixed
Linear
Exponential
```

A retryable failure becomes:

```text
Running
   |
   v
RetryWaiting
```

and receives:

```text
available_at = now() + calculated_delay
```

When the timestamp is reached:

```text
RetryWaiting
   |
   v
Pending
```

or the scheduler may treat `RetryWaiting` with an expired delay as eligible directly.

The exact storage representation can be decided later.

---

# Non-retryable failure

Not every error should be retried.

Examples:

```text
invalid payload
unsupported operation
invalid transaction format
policy violation
```

These should move directly to:

```text
Failed
```

The Activity result should record enough structured information to explain why.

---

# Durable timers

Aegis Flow does not keep a Worker or async task alive just to wait.

Waiting is represented in durable state.

For v1 the simplest representation is:

```text
available_at
```

Example:

```text
Activity:
    state = RetryWaiting
    available_at = 2026-09-17T14:10:00Z
```

Workers only claim work whose eligibility time has arrived.

This model should cover:

```text
retry delays
confirmation polling
provider cooldowns
scheduled checks
```

Longer-term event waiting may require a separate primitive, but v1 does not need it yet.

---

# Completion model

A Worker reports one of four outcomes:

```text
Success
RetryableFailure
PermanentFailure
Ambiguous
```

The engine translates the outcome into an Activity transition.

Example:

```text
Success
   |
   v
Succeeded
```

```text
RetryableFailure
   |
   v
RetryWaiting
```

```text
PermanentFailure
   |
   v
Failed
```

```text
Ambiguous
   |
   v
Ambiguous
```

The transition must be conditional on the active lease token.

---

# Workflow progression

When an Activity reaches a stable state, the Workflow may need to advance.

For v1, Activity completion and Workflow transition should happen in the same database transaction when they belong to the same logical change.

Example:

```text
Activity A -> Succeeded
        +
Workflow -> Waiting
        +
Activity B -> Pending
```

should commit atomically.

This avoids a state such as:

```text
Activity A succeeded
but Activity B was never created
```

because the process crashed between two unrelated commits.

---

# Event history

Aegis Flow v1 will keep an append-only event log.

The event log is **not** used to replay arbitrary workflow code.

Its first purpose is:

```text
audit
debugging
recovery evidence
observability
```

Example events:

```text
WorkflowCreated
WorkflowStarted
ActivityCreated
ActivityClaimed
ActivityLeaseExtended
ActivitySucceeded
ActivityRetryScheduled
ActivityFailed
ActivityMarkedAmbiguous
WorkflowWaiting
WorkflowCompleted
WorkflowFailed
```

A rough event record may contain:

```text
event_id
workflow_id
activity_id
event_type
attempt
worker_id
occurred_at
metadata
```

Current state tables remain authoritative for v1.

---

# Stable identity

Every important object needs a stable identifier.

At minimum:

```text
WorkflowId
ActivityId
WorkerId
EventId
LeaseToken
```

In Rust these should not all be plain `String`.

The domain model should use newtypes.

Example:

```rust
pub struct WorkflowId(Uuid);
pub struct ActivityId(Uuid);
```

This prevents accidentally passing an Activity ID where a Workflow ID is expected.

---

# Execution semantics

Aegis Flow v1 will target:

```text
at-least-once Activity execution
```

This means the same Activity may execute more than once.

The engine does not promise:

```text
exactly-once external side effects
```

Every side-effecting Activity must therefore have an explicit safety strategy.

Possible strategies:

```text
Idempotent
Deduplicated
Reconciled
```

This will likely become a domain type later.

---

# First invariants

These are the rules I want the Rust domain layer to protect first.

## Workflow invariants

1. A terminal Workflow cannot transition back to Running.
2. A Workflow cannot become Completed while required Activities are unfinished.
3. A Cancelled Workflow does not create new normal Activities.
4. Every Workflow transition is explicit.

## Activity invariants

1. Only Pending Activities can be claimed.
2. Only the active lease can complete a Running Activity.
3. A Succeeded Activity cannot be retried.
4. A Failed Activity is terminal unless an explicit recovery operation creates a new attempt or Activity.
5. Attempt number never decreases.
6. Retry eligibility cannot be in the past at the moment a retry is scheduled.
7. Ambiguous is not silently treated as Failed.

## Ownership invariants

1. Workers never own durable Workflow state.
2. A lease always has an expiration.
3. Lease replacement invalidates previous lease tokens.

---

# Failure scenarios we must test

The design is not accepted until these scenarios are covered.

## Worker dies before external call

Expected:

```text
lease expires
Activity becomes recoverable
external side effect did not happen
```

## Worker dies after external call but before completion

Expected:

```text
Activity may become Ambiguous
reconciliation required
```

## Two Workers try to claim the same Activity

Expected:

```text
one succeeds
one receives another Activity or no work
```

## Stale Worker reports completion

Expected:

```text
completion rejected because lease token is stale
```

## Database transaction fails while creating next Activity

Expected:

```text
previous Activity completion and Workflow transition roll back together
```

## Notification is lost

Expected:

```text
polling eventually discovers eligible work
```

## Worker restarts

Expected:

```text
no Workflow information is required from its previous memory
```

---

# What v1 deliberately does not include

To keep the first implementation focused:

```text
no deterministic workflow replay
no external message broker
no multi-region replication
no exactly-once side-effect guarantee
no distributed consensus layer
no dynamic workflow code upload
no arbitrary user code execution
no complex DAG scheduler
```

These are not rejected forever.

They are simply not required to test the first execution model.

---

# Initial component view

```text
                 +------------------+
                 | Workflow Client  |
                 +--------+---------+
                          |
                          v
                 +------------------+
                 |  Aegis Flow API  |
                 +--------+---------+
                          |
                          v
                 +------------------+
                 |    PostgreSQL    |
                 |                  |
                 | workflows        |
                 | activities       |
                 | events           |
                 +--------+---------+
                          ^
                          |
             +------------+------------+
             |                         |
             v                         v
       +-----------+             +-----------+
       | Worker A  |             | Worker B  |
       +-----------+             +-----------+
             |                         |
             +------------+------------+
                          |
                          v
                 +------------------+
                 | External Systems |
                 +------------------+
```

For v1, there is no separate scheduler service yet.

Scheduling can initially be the act of Workers querying eligible Activities.

If that becomes a bottleneck or mixes responsibilities badly, the architecture can evolve.

---

# What should become Rust first

The first crate should still be infrastructure-free:

```text
flow-domain
```

It should contain only domain concepts such as:

```text
WorkflowId
ActivityId

WorkflowState
ActivityState

ActivityOutcome
RetryPolicy

LeaseToken
Attempt

Workflow transition rules
Activity transition rules

Domain errors
```

It should not depend on:

```text
Axum
SQLx
PostgreSQL
Redis
Tokio networking
```

The point is to make the execution model compile before making it distributed.

---

# Questions still open

These should be answered either by implementation experiments or by the first ADR.

1. Should `RetryWaiting` be a separate Activity state or simply `Pending` with a future `available_at`?
2. Does `Ambiguous` belong to every Activity or only side-effecting Activities?
3. Should lease expiration move directly to retry or first create a separate recovery state?
4. Which isolation level is required for claim and completion transactions?
5. Should the event log be written in the same transaction as every state change?
6. How much Activity result data belongs in PostgreSQL?
7. Should `ActivityOutcome` include structured retry metadata?
8. How should cancellation interact with an already-running external side effect?
9. Do we need a separate reconciliation Activity type?
10. What state should a Workflow enter while one Activity is Ambiguous?

I do not want to answer all of these in prose before writing the domain model.

Several are better tested by trying to model them in Rust.

---

# Next step

The next architecture artifact should be:

```text
docs/adr/0001-execution-model.md
```

That ADR will capture the decisions that are now stable enough to accept:

- PostgreSQL as authoritative state for v1;
- at-least-once Activity execution;
- explicit Workflow and Activity state machines;
- leases with lease tokens;
- durable timers based on persisted eligibility;
- append-only event history without deterministic replay;
- no external broker in v1.

After ADR-0001 is accepted, the first Rust workspace and `flow-domain` crate can be created.