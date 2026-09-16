# Cloudflare Workflows — Engineering Review

## Why I am reviewing it

Temporal is a useful reference for durable execution, but it asks developers to understand replay, determinism, workflow history, and a fairly large orchestration model.

Cloudflare Workflows takes a different position.

The developer writes ordinary-looking code, marks important boundaries as durable steps, and lets the platform persist step results, retry failed work, sleep for long periods, and resume execution later.

That makes it a useful comparison for Aegis Flow.

The question I want to answer here is:

> How much durable execution can we get without adopting Temporal's full replay model?

---

## The programming model

A workflow is a class with a `run` method.

Durability is introduced through the `step` API.

The important operations are:

```text
step.do(...)
step.sleep(...)
step.sleepUntil(...)
step.waitForEvent(...)
```

A simplified workflow looks like:

```text
start
  |
  v
step.do("prepare")
  |
  v
step.do("broadcast")
  |
  v
step.sleep("wait")
  |
  v
step.do("check confirmation")
```

The interesting part is that successful step results are persisted.

If execution is interrupted after a successful step, the workflow can continue from the last durable boundary instead of repeating everything that happened before it.

That is a much simpler mental model than replaying an entire workflow history.

---

## Where durable state lives

Cloudflare manages workflow state internally.

The developer does not need to explicitly create a database table for every step result.

A `step.do()` callback may return serializable data, and that result becomes durable workflow state.

Cloudflare's documentation is explicit about an important detail:

changes made only to the original event payload are not durable.

If state needs to survive failures, it should be returned from a durable step.

That distinction matters.

Conceptually:

```text
ordinary local variable
        |
        | process disappears
        v
      lost

step.do result
        |
        | platform persists it
        v
   available after resume
```

For Aegis Flow, I like the explicit idea that only clearly marked values become durable.

I do not want accidental process memory to look durable.

---

## Step boundaries are the unit of recovery

Cloudflare's documentation suggests choosing step boundaries by asking a practical question:

> If the next operation fails, do I want this work to run again?

That is a useful way to think about durable execution.

For example:

```text
fetch transaction metadata
        |
        v
calculate transaction
        |
        v
broadcast transaction
```

If these are all inside one retryable step and the final broadcast fails, the whole step may execute again.

If they are separate durable steps, earlier successful results can be reused.

This makes step design part of correctness, not only code organization.

For Aegis Flow, this is worth adopting conceptually:

> Recovery boundaries should be visible in the workflow definition.

---

## Retries

Each `step.do()` can define retry behaviour.

The platform supports:

```text
retry limit
retry delay
constant backoff
linear backoff
exponential backoff
dynamic retry delay
per-attempt timeout
```

There is also a non-retryable error type for cases where repeating the step makes no sense.

This is a strong developer experience.

Retry policy sits next to the operation it applies to instead of being hidden in infrastructure configuration.

For example, an RPC call and a validation failure clearly need different policies.

```text
RPC 429
    |
    v
retry later

invalid transaction format
    |
    v
do not retry
```

That separation is useful for Aegis Flow.

---

## Idempotency is still the application's responsibility

This is one of the most important findings.

Cloudflare explicitly recommends that API and binding calls inside retryable steps be idempotent.

That means the platform does not make an arbitrary external side effect exactly-once.

Consider:

```text
step.do("charge customer")
        |
        v
external API succeeds
        |
        v
response is lost
        |
        v
step retries
```

The workflow engine cannot infer whether the first external call changed state.

The application still needs a strategy such as:

```text
idempotency key
stable transaction identifier
deduplication
external state lookup
reconciliation
```

This is the same fundamental issue we saw with Temporal.

That gives me more confidence that Aegis Flow should not promise exactly-once side effects.

The better design target is:

```text
durable orchestration
+
at-least-once external work
+
explicit idempotency / reconciliation
```

---

## Durable sleeps

Cloudflare supports durable sleeping through `step.sleep()` and `step.sleepUntil()`.

The workflow does not need to keep a process alive while waiting.

A workflow may wait for long periods and later resume.

This is directly useful for Aegis Flow.

Examples:

```text
wait 30 seconds before RPC retry
wait for another block
wait before checking confirmations
wait for provider cooldown
wait until a scheduled operation
```

A long-running workflow should not require an active Rust task sitting in memory for hours.

That principle is clearly worth keeping.

---

## Waiting for external events

`step.waitForEvent()` allows a workflow to pause until an external event arrives.

This is especially interesting because not every long-running workflow is timer-driven.

A workflow may need to wait for:

```text
human approval
external webhook
another service
manual intervention
external state transition
```

This will matter later for the broader Aegis platform.

For example:

```text
transaction prepared
        |
        v
wait for approval
        |
        v
signing permitted
```

Aegis Flow does not need this in its first Rust milestone, but the execution model should not make it impossible later.

---

## Workflow instance states

Cloudflare exposes workflow instance states such as:

```text
queued
running
waiting
paused
errored
terminated
complete
```

This is another good reminder that a workflow's lifecycle is not the same thing as one worker task.

For Aegis Flow I want the domain model to distinguish between:

```text
workflow state
activity state
worker state
```

Those are related, but they are not the same state machine.

Mixing them would make recovery logic harder to reason about.

---

## Waiting should not consume an active worker

Cloudflare treats waiting workflows differently from running workflows.

A workflow sleeping, waiting for retry, or waiting for an event does not need to occupy an active execution slot in the same way as running work.

That is important.

A naive implementation might accidentally create:

```text
10,000 workflows
        |
        v
10,000 sleeping async tasks
```

That may work at small scale, but it ties durable workflow state to runtime resources.

Aegis Flow should persist a wake-up condition instead.

Conceptually:

```text
WAITING
wake_at = 2026-09-17T12:00:00Z
```

and let a scheduler make it runnable later.

---

## Step result limits are a useful design constraint

Cloudflare limits how much durable state a step can return and how much state a workflow instance can persist.

The exact limits are platform-specific, but the design lesson is broader:

> Workflow history/state should not become an object store.

Large data should usually live elsewhere, while workflow state keeps references and the minimum information necessary to continue execution.

For Aegis Flow:

```text
bad:
store 500 MB response in workflow state

better:
store object_key / content_hash / database reference
```

That will keep persistence and replay/recovery manageable.

---

## Developer experience

This is where Cloudflare differs most strongly from Temporal.

Temporal exposes a deeper workflow programming model.

Cloudflare gives the developer something much closer to:

```text
normal code
+
explicit durable checkpoints
```

The benefit is obvious:

- less orchestration machinery to understand;
- no application-visible replay model;
- retry configuration is local to a step;
- sleep and event waiting are simple APIs;
- operational infrastructure is managed by Cloudflare.

This is attractive for Aegis Flow because I want the first version to stay understandable.

---

## What Cloudflare hides

The simplicity also comes from the fact that Cloudflare owns the platform underneath it.

The developer does not control:

```text
workflow persistence implementation
internal scheduler
storage engine
worker placement
execution infrastructure
scaling implementation
```

That is fine for a managed service.

Aegis Flow is different.

The point of Aegis Flow is partly to understand and implement those mechanisms ourselves.

So we can adopt Cloudflare's developer-facing ideas without pretending we can copy its infrastructure model.

---

## Strongest parts of the approach

### 1. Durable boundaries are explicit

`step.do()` makes it clear which pieces of execution should survive a restart.

### 2. The developer model is simple

The system does not require application developers to reason about full workflow replay.

### 3. Retries live near the operation

Retry policy can match the failure characteristics of each step.

### 4. Long waits are durable

Timers and external waits do not require a process to stay alive.

### 5. Waiting and running are different states

A workflow can remain durable without consuming active execution capacity.

### 6. Idempotency is treated honestly

The platform retries work, but still tells developers to make external operations safe to repeat.

That is the correct boundary.

---

## What worries me

### Platform internals are intentionally opaque

Cloudflare's public developer model tells us what guarantees the product provides, but not enough to directly reproduce its scheduler or persistence architecture.

That makes it a good API/design reference, but a weaker implementation reference for Aegis Flow.

### Step boundaries can become application architecture

If developers choose step boundaries poorly, retry behaviour may be surprising.

A step is not just a function.

It is a recovery and persistence boundary.

Aegis Flow will need to make that distinction very clear.

### Managed infrastructure hides operational cost

The API looks small because Cloudflare operates the underlying distributed system.

If Aegis Flow implements its own durable scheduler, queue, storage, and wake-up mechanism, that complexity does not disappear.

---

## Ideas worth adopting for Aegis Flow

After this review, I would keep these ideas:

1. **Make durable boundaries explicit.**

   A developer should be able to see which operations produce durable progress.

2. **Persist results at those boundaries.**

   Completed work should not need to run again after an unrelated later failure.

3. **Keep retry policy close to the activity.**

   Different operations fail differently.

4. **Support non-retryable failures explicitly.**

   Not every error should enter a retry loop.

5. **Treat durable waiting as persisted state, not a sleeping process.**

6. **Keep workflow state small.**

   Large artifacts should live in external storage.

7. **Separate workflow lifecycle from worker lifecycle.**

8. **Assume external side effects may run more than once.**

   Require idempotency, deduplication, or reconciliation.

---

## Ideas I would not adopt directly

### A fully managed black-box scheduler

Aegis Flow needs an execution model that we can explain, test, and operate ourselves.

### Cloudflare-specific limits or APIs

The exact Workers execution limits are product constraints, not architecture principles.

### JavaScript-style workflow definitions as the core model

Aegis Flow is a Rust project.

Its domain model should use Rust types and explicit state transitions rather than imitate the Cloudflare API shape.

---

## What this changes in the Aegis Flow direction

The Temporal review made event-history replay look attractive because it provides a strong recovery model.

Cloudflare shows that there is another useful option:

```text
explicit durable steps
+
persisted step results
+
persistent workflow state
+
retryable activities
+
durable timers
```

without exposing full deterministic replay to the application developer.

That makes me less convinced that Aegis Flow needs replay in the first version.

A simpler model may be enough:

```text
WorkflowInstance
        |
        v
current durable state
        |
        v
next activity
        |
        v
persist result / transition
        |
        v
next state
```

The remaining question is whether we can implement this cleanly and safely with PostgreSQL.

That is why the database-backed review is the next important one.

---

## Questions left open

1. Can PostgreSQL be the authoritative source for workflow state and runnable work?
2. How should workers atomically claim activities?
3. Do we use leases, acknowledgements, or both?
4. How do expired activities become runnable again?
5. How are durable timers represented in the database?
6. How do we prevent two workers from committing the same transition?
7. Do we need an append-only event history in addition to current state?
8. How should ambiguous external results be represented?
9. Can the first version avoid a separate message broker entirely?

---

## Current comparison

| Question | Temporal / Cadence | Cloudflare Workflows | Aegis Flow direction |
|---|---|---|---|
| Durable progress | Event History | Durable steps/results | Required |
| Worker is source of truth | No | No | No |
| Full deterministic replay | Yes | Not exposed as developer model | Probably not in v1 |
| External side effects | Activities | Retryable steps | Explicit activities |
| Duplicate external work | Must be handled | Must be handled | Must be handled |
| Durable timers | Yes | Yes | Required |
| Retry policy | Activity policy | Per-step policy | Per-activity policy |
| Long waits consume worker | No | No | Must not |
| Developer complexity | Higher | Lower | Prefer lower |
| Infrastructure ownership | Self-hosted / managed Temporal | Cloudflare managed | We own it |

---

## Sources reviewed

### Cloudflare Workflows documentation

- Overview  
  https://developers.cloudflare.com/workflows/

- Build your first Workflow  
  https://developers.cloudflare.com/workflows/get-started/guide/

- Rules of Workflows  
  https://developers.cloudflare.com/workflows/build/rules-of-workflows/

- Sleeping and retrying  
  https://developers.cloudflare.com/workflows/build/sleeping-and-retrying/

- Events and parameters  
  https://developers.cloudflare.com/workflows/build/events-and-parameters/

- Trigger and inspect Workflows  
  https://developers.cloudflare.com/workflows/build/trigger-workflows/

- Limits  
  https://developers.cloudflare.com/workflows/reference/limits/

- Glossary  
  https://developers.cloudflare.com/workflows/reference/glossary/

### Cloudflare engineering

- Workflows GA: production-ready durable execution  
  https://blog.cloudflare.com/workflows-ga-production-ready-durable-execution/

- Workflow diagrams and AST-based visualization  
  https://blog.cloudflare.com/workflow-diagrams/