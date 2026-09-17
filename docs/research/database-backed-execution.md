# Database-Backed Execution — Engineering Review

## Why I am reviewing this approach

Temporal showed what a full durable workflow engine can do.

Cloudflare showed how much of that complexity can be hidden behind explicit durable steps.

Now I want to test the opposite idea:

> How far can Aegis Flow get with PostgreSQL as the source of truth and a deliberately small execution model?

I do not want to introduce a broker, replay engine, or several coordination services just because mature systems have them.

If PostgreSQL can safely handle the first version, that gives Aegis Flow a much smaller operational surface.

The question is not whether PostgreSQL can become "Temporal in SQL."

The question is whether it can support the guarantees we actually need.

---

## The simplest possible model

At minimum, Aegis Flow needs to persist:

```text
workflow
activity
current state
next eligible execution time
attempt count
worker ownership
result / failure
```

A rough activity row could eventually contain:

```text
id
workflow_id
kind
state
attempt
available_at
lease_owner
lease_until
payload
result
last_error
created_at
updated_at
```

The database would be authoritative.

Workers would only borrow runnable work.

```text
PostgreSQL
    |
    | runnable activity
    v
 Worker A

Worker A disappears

PostgreSQL
    |
    | activity becomes available again
    v
 Worker B
```

The worker can disappear without taking the workflow state with it.

---

## Atomic job claiming with row locks

PostgreSQL row locking gives us a useful primitive for multiple workers competing for work.

A typical claim looks like:

```sql
SELECT ...
FROM activities
WHERE state = 'pending'
  AND available_at <= now()
ORDER BY available_at
FOR UPDATE SKIP LOCKED
LIMIT 1;
```

`FOR UPDATE` locks the selected row.

`SKIP LOCKED` allows another worker to skip rows already claimed by a concurrent transaction instead of waiting for them.

The important part is not the exact SQL above. It is the atomicity of:

```text
find available work
        +
claim that work
```

inside one database transaction.

Graphile Worker and pg-boss both use PostgreSQL locking patterns for concurrent job fetching, which makes this a useful production reference rather than a toy idea.

---

## Why I do not want to say "exactly once"

A queue can atomically prevent two workers from claiming the same row at the same moment.

That still does not mean an external side effect happens exactly once.

There is still a failure window:

```text
worker claims activity
        |
        v
external side effect succeeds
        |
        v
worker crashes
        |
        v
database never records completion
```

After recovery, the activity may be attempted again.

So for Aegis Flow I want to keep the distinction explicit:

```text
single atomic claim
!=
exactly-once external side effect
```

The engine should assume external work may be repeated.

That means side-effecting activities need one of:

```text
idempotency key
deduplication
stable external identifier
reconciliation
```

This matches what we already found in Temporal and Cloudflare.

---

## Leases instead of permanent ownership

A row lock only exists while the database transaction is open.

I do not want a worker to hold a PostgreSQL transaction open for the whole lifetime of a 30-second or 30-minute activity.

A better model is:

```text
short transaction:
    claim activity
    set lease_owner
    set lease_until
    set state = running
commit

execute outside transaction
```

The worker owns the activity only until a deadline.

If it disappears, a recovery process can find:

```text
state = running
AND lease_until < now()
```

and make the activity eligible again.

This is closer to a visibility timeout than to holding a database lock for the whole execution.

---

## The stale-worker problem

A lease introduces another race:

```text
Worker A claims activity
        |
        v
A becomes slow or partitioned

lease expires

Worker B claims activity
        |
        v
A wakes up and tries to complete it
```

A plain `activity_id` is not enough.

The claim should carry a generation or lease token.

For example:

```text
activity_id
attempt = 4
lease_token = random unique value
```

Completion should only succeed when the worker presents the active token.

Conceptually:

```sql
UPDATE activities
SET state = 'completed'
WHERE id = ?
  AND state = 'running'
  AND lease_token = ?;
```

If the lease was replaced, the stale worker updates zero rows.

This does not undo an external side effect the stale worker may already have made, but it prevents stale workers from overwriting newer engine state.

This is one of the first mechanisms I would want to test hard.

---

## Heartbeats

Long-running work may need to extend its lease.

A worker can periodically move:

```text
lease_until = now() + lease_duration
```

I do not want heartbeats on every operation.

A two-second RPC call probably does not need them.

A 30-minute indexing activity might.

So heartbeat support should probably belong to activity policy rather than be mandatory globally.

---

## Durable timers are persisted eligibility

A retry can become:

```text
state = pending
available_at = now() + backoff
```

A workflow waiting until tomorrow can use the same idea.

Workers only fetch work where:

```text
available_at <= now()
```

That means waiting work consumes database rows, not sleeping worker tasks.

For the first version, this may be enough for:

```text
retry delay
provider cooldown
confirmation polling
scheduled re-check
```

---

## LISTEN / NOTIFY as a wake-up hint

Pure polling creates a trade-off:

```text
poll frequently
    -> lower latency
    -> more database traffic

poll slowly
    -> lower database traffic
    -> higher start latency
```

PostgreSQL provides `LISTEN` / `NOTIFY`, and production queues such as Graphile Worker use notifications to reduce wake-up latency.

I like this as an optimization, but I do not want correctness to depend on it.

My preferred rule is:

```text
database query = correctness

LISTEN / NOTIFY = wake-up optimization
```

If a notification is lost or a worker reconnects, polling the authoritative table still finds runnable work.

---

## Transactional enqueueing is a major advantage

One reason PostgreSQL is attractive is that workflow state and newly-created work can be committed atomically.

For example:

```text
complete Activity A
        +
update Workflow state
        +
create Activity B
```

can happen in one database transaction.

Either all of it commits, or none of it does.

That removes a common failure window where application state commits but a message to another queue is never published.

---

## If we later publish events: outbox

If Aegis Flow later publishes events to another service or broker, this is unsafe as two unrelated operations:

```text
COMMIT database state
        |
        v
publish event
```

The process could crash between them.

The transactional outbox pattern stores the event in the same database transaction as the workflow change:

```text
transaction
  |
  +-- update workflow/activity
  |
  +-- insert outbox event
  |
  v
commit
```

A separate relay publishes it later.

That relay can still publish a message more than once after an unlucky crash, so consumers still need idempotency.

I would not build the outbox before we need external event publishing, but the architecture should leave room for it.

---

## Advisory locks

PostgreSQL also has application-defined advisory locks.

They may be useful later for coarse coordination:

```text
one scheduler leader
one maintenance job
one operation per logical resource
```

I would not use them as the primary ownership mechanism for normal activities.

Job ownership should be visible in durable domain data.

A row containing:

```text
lease_owner
lease_until
lease_token
```

is easier to inspect and recover than a session-level advisory lock.

So for now:

```text
row state + lease = activity ownership

advisory locks = optional coordination tool
```

---

## Current state versus event history

This is the biggest design question left after Temporal and Cloudflare.

Temporal can reconstruct execution from event history.

A simple PostgreSQL engine could keep only current state:

```text
workflow.status = waiting
activity.status = running
```

That is easier, but it makes questions like these harder:

```text
Why is this workflow waiting?
Which worker attempted this before?
Why was this retry scheduled?
What state did it have yesterday?
```

I do not think Aegis Flow needs Temporal-style replay history in v1.

I do think it needs an append-only execution log.

For example:

```text
workflow_events

id
workflow_id
event_type
activity_id
attempt
occurred_at
metadata
```

The distinction would be:

```text
current tables
    = authoritative operational state

event log
    = audit / debugging / recovery evidence
```

not:

```text
event log
    = replay arbitrary workflow code
```

This looks like a useful middle ground.

---

## What I like about this approach

### 1. The source of truth is obvious

PostgreSQL owns workflow and activity state.

### 2. There is very little infrastructure

For an early version:

```text
Aegis Flow
PostgreSQL
workers
```

may be enough.

### 3. State changes and new work can be transactional

This avoids several dual-write problems.

### 4. The mechanics are inspectable

A developer can query the tables and understand:

```text
what is pending
what is running
who owns it
when the lease expires
why it failed
when it becomes eligible again
```

I like that a lot for a first implementation.

### 5. Durable timers are simple

`available_at` covers a surprising amount of the problem space.

### 6. The design maps naturally to Rust domain types

We can model the state machine in pure Rust and let the storage adapter enforce atomic transitions.

---

## What worries me

### Database contention can become the bottleneck

A queue table is a hot coordination structure.

Indexes, polling patterns, batch sizes, cleanup, autovacuum, and lock contention will matter as volume grows.

Graphile Worker and pg-boss have years of optimization around these issues.

A naive query should not be assumed to scale indefinitely.

### Leases create subtle races

Expired worker versus new worker is a real correctness problem.

Lease tokens and conditional updates need careful tests.

### PostgreSQL can become too many things

There is a temptation to use one database as:

```text
workflow store
queue
timer service
event broker
pub/sub system
analytics store
```

I want to resist that.

Using PostgreSQL for the first version is a deliberate simplification, not a claim that every future subsystem belongs there.

### Large backlogs need maintenance

High-churn job tables can create bloat and operational pressure.

Production queues have cleanup and retention policies for a reason.

### LISTEN / NOTIFY is not a durable queue

It should wake workers up, not carry authoritative work.

---

## What I would adopt for Aegis Flow v1

My preferred direction is becoming fairly concrete.

### PostgreSQL is authoritative

It stores:

```text
workflow instances
activities
current workflow state
leases
retry eligibility
event log
```

### Workers pull work

Workers claim eligible activities rather than receiving direct pushes.

### Claims are short database transactions

No database transaction stays open while user code executes.

### Activity ownership uses leases

Each running attempt has:

```text
worker id
lease deadline
lease token
attempt number
```

### Completion is conditional

A stale worker must not be able to commit over a newer attempt.

### Delivery model is at-least-once

The engine does not promise exactly-once external effects.

### Side effects require explicit safety strategy

An Activity should document how repetition is handled.

### Timers use persisted timestamps

Waiting does not consume an active worker.

### Event history exists for audit, not deterministic replay

At least in v1.

### LISTEN / NOTIFY may reduce latency

But polling PostgreSQL remains the correctness path.

### No external broker in v1

Unless measurements or a concrete requirement show that we need one.

---

## A rough first execution cycle

The model I currently want to test is:

```text
1. Worker asks PostgreSQL for eligible work

2. PostgreSQL atomically claims one activity

3. Claim transaction commits

4. Worker executes activity outside the transaction

5. Worker reports one of:

   success
   retryable failure
   permanent failure
   ambiguous outcome

6. PostgreSQL conditionally commits the transition
   using the active lease token

7. The workflow becomes:

   runnable
   waiting
   completed
   failed
   or awaiting reconciliation
```

The interesting state here is:

```text
ambiguous outcome
```

I want that to be explicit rather than collapsed into "failed."

For blockchain operations, that difference matters.

---

## A first possible state split

I would keep workflow state and activity state separate.

### Workflow

```text
Created
Running
Waiting
Completed
Failed
Cancelled
```

### Activity

```text
Pending
Running
RetryWaiting
Succeeded
Failed
Ambiguous
```

These are not final names yet.

The Rust domain-model phase should test whether some states can be made more precise or even impossible through types.

---

## What this means after the three reviews

| Question | Temporal / Cadence | Cloudflare Workflows | PostgreSQL-backed direction |
|---|---|---|---|
| Durable unit | Event history / commands | Durable step | Activity/state transition |
| Worker source of truth | No | No | No |
| Deterministic replay | Yes | Not exposed to developer | No in v1 |
| Current state stored | Service-managed | Platform-managed | Explicit DB rows |
| External work | Activities | Durable/retryable steps | Activities |
| Duplicate side effects | Application must handle | Application must handle | Application must handle |
| Durable timers | Yes | Yes | `available_at` / persisted deadline |
| Worker recovery | History + redelivery | Managed resume | Lease expiry + reclaim |
| Queue mechanism | Dedicated service | Managed platform | PostgreSQL claim query |
| Operational complexity | High | Hidden by provider | Low initially |
| Best fit for Aegis v1 | Too much machinery | Great UX reference | Strong candidate |

My current bias is:

> Start with the PostgreSQL-backed model, but borrow the explicit Activity boundary from Temporal and the visible durable-step philosophy from Cloudflare.

That is not yet an architecture decision.

The next step is to write the execution model precisely enough that we can try to break it.

---

## Questions that must be answered before ADR-0001

1. What is the exact atomic claim query?
2. What isolation level do we need for each transition?
3. How are expired leases reclaimed safely?
4. How do we prevent stale-worker completion?
5. How long is a lease and who may extend it?
6. What is the state transition for an ambiguous side effect?
7. How is retry backoff persisted?
8. What event history is mandatory?
9. Which workflow transitions and activity transitions are legal?
10. What invariants must hold across workflow, activity, and event tables?
11. Which operations need a single database transaction?
12. What metrics expose stuck or unhealthy execution?

Those answers should become `docs/design/execution-model.md`.

Only after that do I want to write the storage implementation.

---

## References reviewed

### PostgreSQL

- Transactions  
  https://www.postgresql.org/docs/18/tutorial-transactions.html

- UPDATE / `SKIP LOCKED` usage  
  https://www.postgresql.org/docs/18/sql-update.html

- `LISTEN`  
  https://www.postgresql.org/docs/18/sql-listen.html

- Asynchronous notifications  
  https://www.postgresql.org/docs/18/libpq-notify.html

- Advisory lock functions  
  https://www.postgresql.org/docs/18/functions-admin.html

- Lock monitoring  
  https://www.postgresql.org/docs/18/monitoring-locks.html

### Graphile Worker

- Documentation  
  https://worker.graphile.org/docs

- Source repository  
  https://github.com/graphile/worker

- Performance notes  
  https://github.com/graphile/worker/blob/main/website/docs/performance.md

### pg-boss

- Introduction  
  https://pgboss.io/introduction

- Source repository  
  https://github.com/timgit/pg-boss

### Transactional Outbox

- Transactional outbox pattern  
  https://microservices.io/patterns/data/transactional-outbox