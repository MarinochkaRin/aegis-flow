# PostgreSQL worker adapter (Step 6C)

## Why this crate exists

`flow-domain` owns in-memory rules. PostgreSQL is authoritative for worker
ownership and deadlines: the same token-and-deadline predicate must guard
state changes atomically in the database. Recomputing lease validity with a
worker's local clock would undermine the concurrency guarantees tested in 6B.

`flow-storage-pg` is a deliberately narrow bridge between the two. It maps
UUIDs to `WorkflowId`, `ActivityId` and `LeaseToken`; maps the database's
nonzero attempt to `Attempt`; and preserves PostgreSQL's actual deadline.
It does **not** reconstruct the full `Activity` entity from a row or run any
external action. Hydration, an activity registry, workflow orchestration,
connection pooling and production retry policy are later milestones.

## Implemented API

- `claim_activity(worker_id, duration) -> Option<ClaimedActivity>`
- `heartbeat_activity(id, token, extension) -> Option<UTC timestamp>`
- `finish_activity(id, token, outcome, retry_at) -> bool`
- `recover_expired(retry_delay) -> count`
- `release_due_retries() -> count`

A `None` from heartbeat or `false` from completion means the DB rejected the
operation (possibly because of expiry). It does **not** establish whether
some external side effect occurred. A worker must reconcile ambiguous effects
rather than treating a lost lease as a safe retry.

Durations are passed as **parameterized** PostgreSQL `INTERVAL` values, with
microsecond precision. Sub-microsecond nonzero leases are rejected instead
of silently becoming zero. PostgreSQL's clock, not the Rust clock, decides
whether the lease is still valid.

`PgStorage::new` takes a `tokio_postgres::Client`. Its caller must drive the
separate connection future; see the integration test for an example. This
initial single-connection API intentionally avoids silently adding a pool.

## Testing boundaries

The existing SQL test and 6B two-session tests establish the initial
PostgreSQL claim/fencing contract, including overlapping SQL transactions.
The new Rust integration test checks that **independent Tokio PostgreSQL
clients** observe the same behavior through the Rust adapter, plus ID/attempt
mapping, heartbeat and stale completion rejection.

`scripts/pg-rust-test.sh` creates a temporary, disposable database, applies
our checked-in `0001_init.sql`, runs the integration test and drops the
temporary database even if the test command fails. It does not reset the
normal `aegis_flow` development database.

Running ordinary `cargo test --workspace` without `AEGIS_TEST_DATABASE_URL`
skips the live PostgreSQL integration test. For a meaningful storage check,
run `bash scripts/pg-rust-test.sh` explicitly. Do not describe ordinary Cargo
test alone as proof that the database adapter works.

## Explicitly out of scope

- TLS, connection pooling, retry-on-connection-failure and graceful shutdown
- Workflow and Activity insertion APIs (integration test creates fixtures)
- Mapping snapshots into full in-memory domain entities
- A live worker poll/execute loop or scheduler cadence
- Cross-transaction idempotency for external effects
- Storage-level reconciliation API for `Ambiguous` activities

Those are separate design and validation tasks. In particular, an SQL claim
committing is **not** proof that an external operation executed exactly once.
