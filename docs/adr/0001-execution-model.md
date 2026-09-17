# ADR-0001: Execution Model for Aegis Flow v1

## Status

Accepted for the first implementation.

## Context

Aegis Flow needs to execute long-running operations that may survive:

- worker crashes;
- process restarts;
- RPC timeouts;
- duplicate delivery;
- delayed retries;
- ambiguous external side effects.

The research phase compared three approaches:

- Temporal / Cadence-style durable execution with workflow history and deterministic replay;
- Cloudflare Workflows-style explicit durable steps;
- PostgreSQL-backed execution with atomic claiming, leases, and persisted timers.

The first version of Aegis Flow should be small enough to understand end to end while still making failure behaviour explicit.

The goal is not to reproduce Temporal or build a general-purpose distributed scheduler before the project needs one.

## Decision

Aegis Flow v1 will use PostgreSQL as the authoritative source of workflow execution state.

The execution model will use:

- explicit Workflow state;
- explicit Activity state;
- at-least-once Activity execution;
- PostgreSQL-backed atomic Activity claiming;
- short-lived database transactions for claims and state transitions;
- leases for in-flight Activity ownership;
- unique lease tokens to reject stale Worker completion;
- persisted timestamps for retries and durable waiting;
- an append-only event log for audit and debugging;
- explicit handling of ambiguous external outcomes;
- pull-based Workers.

The first version will not use deterministic workflow replay.

The event log will not be the mechanism used to reconstruct arbitrary user code.

Current state tables remain authoritative.

## Activity execution semantics

An Activity may execute more than once.

Aegis Flow therefore does not claim exactly-once external side effects.

Side-effecting Activities must use an explicit safety strategy such as:

- idempotency;
- deduplication;
- reconciliation.

A Worker reports one of these outcomes:

```text
Success
RetryableFailure
PermanentFailure
Ambiguous
```

`Ambiguous` is a first-class outcome.

It exists for cases where the engine cannot safely conclude whether the external operation happened.

Example:

```text
broadcast transaction
        |
        v
RPC accepts request
        |
        v
response is lost
        |
        v
worker sees timeout
```

The system must not automatically treat this as a normal failure.

## Worker ownership

Workers are disposable.

A Worker claims work in a short PostgreSQL transaction and executes it outside that transaction.

A claim contains:

```text
worker_id
attempt
lease_until
lease_token
```

Completion is accepted only if the Worker still owns the active lease token.

This prevents a stale Worker from overwriting a newer Activity attempt after its lease has expired.

## Durable waiting

Waiting is represented in persisted state rather than by keeping an active Worker or async task alive.

The first version will use an eligibility timestamp such as:

```text
available_at
```

This will support:

- retry delays;
- provider cooldowns;
- confirmation polling;
- scheduled checks.

## Event history

Aegis Flow will maintain an append-only event log.

Its purpose in v1 is:

- audit;
- debugging;
- observability;
- recovery evidence.

It is not used for Temporal-style deterministic workflow replay.

## Alternatives considered

### Temporal-style deterministic replay

#### Why it is attractive

- strong recovery model;
- mature handling of long-running workflows;
- durable timers and retries;
- clear separation between workflow logic and side effects.

#### Why it is not selected for v1

It introduces a programming model around replay, determinism, and workflow versioning that Aegis Flow does not yet need.

The first implementation should test the underlying failure model before adopting that complexity.

### Managed durable-step model

Cloudflare Workflows demonstrates a clean developer experience built around explicit durable steps.

The step-boundary idea is useful and influences Aegis Flow's Activity model.

However, Cloudflare owns the scheduler, persistence, and execution infrastructure.

Aegis Flow needs an execution model that can be inspected and implemented directly.

### External broker as the primary queue

A separate broker may become useful later.

It is not selected for v1 because PostgreSQL can initially provide:

- authoritative state;
- atomic claiming;
- retry eligibility;
- transactional creation of follow-up work.

Adding a broker now would create another consistency boundary before there is evidence that it is needed.

## Consequences

### Positive

- the first system has a small operational surface;
- workflow state can be inspected directly;
- state transitions and creation of follow-up work can be transactional;
- worker crashes do not destroy workflow state;
- durable retries do not require sleeping processes;
- the model maps naturally to explicit Rust domain types;
- external side-effect ambiguity is represented instead of hidden.

### Negative

- PostgreSQL becomes a hot coordination point;
- queue performance will depend on indexing, polling behaviour, cleanup, and lock contention;
- lease recovery introduces race conditions that require careful testing;
- at-least-once execution pushes idempotency and reconciliation requirements into Activity design;
- the first version will not have Temporal-style replay guarantees;
- scaling beyond a single PostgreSQL-backed coordination layer may require architectural changes later.

## Rejected assumptions

This ADR deliberately does not assume that:

- a task executes only once;
- a timeout means failure;
- a Worker remains alive;
- a notification is durable;
- an external side effect can always be safely retried;
- PostgreSQL will remain the correct queueing mechanism at every scale.

## Validation plan

Before this decision is considered proven in implementation, the system must pass tests for:

- two Workers attempting to claim the same Activity;
- Worker crash before an external side effect;
- Worker crash after an external side effect but before completion;
- expired lease recovery;
- stale Worker completion;
- retry scheduling;
- durable waiting across process restart;
- lost wake-up notification;
- atomic completion of one Activity and creation of the next;
- ambiguous external outcome;
- terminal-state transition rejection.

## Follow-up

The next implementation step is a pure Rust domain crate:

```text
crates/flow-domain
```

The crate should model:

- `WorkflowId`;
- `ActivityId`;
- `WorkflowState`;
- `ActivityState`;
- `ActivityOutcome`;
- `RetryPolicy`;
- `LeaseToken`;
- legal and illegal state transitions;
- domain errors.

It should not initially depend on:

- Axum;
- SQLx;
- PostgreSQL;
- Redis;
- an external message broker.

The goal is to make the execution rules compile before making the system distributed.