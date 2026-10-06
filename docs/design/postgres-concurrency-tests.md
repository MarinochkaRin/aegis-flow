# PostgreSQL concurrency contract — Step 6B

**Status:** executable local integration tests; not a production-ready worker or proof of external exactly-once execution.

Step 6A checked SQL behavior one operation at a time. This test opens two **independent, persistent PostgreSQL sessions** with the same migration-defined storage contract. We hold Worker A's claim transaction open while Worker B calls `claim_activity` on its own connection. No simulated threads operating on in-memory objects can replace this test.

## Two scenarios

**1. One pending activity.** A claims a due activity but does not commit. B runs `claim_activity` with `lock_timeout=1500ms` and `statement_timeout=3s`. B must return no work *before* A commits. An implementation that waits on A's row would fail or time out, rather than passing after A finishes. After A commits, B still cannot claim the leased activity. A mismatched token cannot finish A's activity; its committed owner can. We then check durable state, attempt count, and claim-event cardinality.

**2. Two pending activities.** A begins a transaction and claims the older due activity, holding the row lock. B must claim the other due activity before A commits. Both commits result in distinct leases, exactly one claimed event for each activity, and no third available claim. This tests `FOR UPDATE ... SKIP LOCKED` under overlapping transactions, including the ability to do useful work rather than simply block.

We intentionally use **PostgreSQL's lock timeout** to catch blocked tests. The test does not rely on a sleep to manufacture a race: A remains in an uncommitted transaction until B responds.

## Isolation and execution

Run with the local `compose.postgres.yml` service started:

```bash
bash scripts/db-concurrency-test.sh
```

Python 3 and Docker Compose are the only test-driver dependencies. The script generates a new database named `aegis_concurrency_<random>`, applies `0001_init.sql` (and any subsequent checked-in migrations), performs both scenarios, and drops **only its own** temporary database in `finally`. It does not modify `aegis_flow` or require wiping Docker volumes. If interrupted by an uncatchable kill, inspect and remove any temporary databases manually.

Do not point the script at production PostgreSQL. The Docker Compose credentials are development credentials and its default DB user is privileged.

## What the test does NOT establish

- Workers have no external operation in this milestone: a lease protects *our database updates*, not a Bitcoin network broadcast or other side effect.
- PostgreSQL uses its own wall clock and short claims in production. A claim transaction must commit before external work starts; this test deliberately delays that commit to exercise locking only.
- This is a deterministic two-connection test, not a load test. Later Rust integration tests should add a pool, many worker tasks, graceful shutdown, crash/restart cases, deadlocks/timeouts and chaos scenarios.
- The temporary database test depends on the checked-in migration chain. Keep it up to date when adding migrations.
