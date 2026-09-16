# Durable Workflow Engines — Research

## Purpose

This document compares existing durable workflow execution systems before Aegis Flow defines its own execution model.

I do not want to start with an assumption such as "we need a queue" or "we need event sourcing" and then design around it. The point of this review is to understand what mature systems gain from their execution model, what complexity comes with it, and which parts are actually useful for Aegis Flow.

## Systems under review

- Temporal / Cadence
- Cloudflare Workflows
- AWS Step Functions
- Database-backed workflow and job engines

## Evaluation criteria

For each system I am looking at the same questions:

- Where does authoritative workflow state live?
- How is state reconstructed after a process or worker disappears?
- What execution guarantees are actually provided?
- How are duplicate executions handled?
- How are external side effects isolated?
- How do retries, timers, and long-running work survive restarts?
- What restrictions does the model place on application code?
- What operational complexity is required to get those guarantees?

---

# Temporal / Cadence

## Why I started here

Temporal and Cadence are useful references because they solve almost exactly the class of problem Aegis Flow is interested in: a workflow may run for a long time, workers may disappear, and execution still needs to continue from durable state.

The part I care about most is not their API. It is the mechanism that lets a new worker reconstruct what a previous worker was doing without treating worker memory as authoritative.

## The model in one sentence

The workflow is a durable program whose logical state can be reconstructed from persisted history, while external side effects are executed separately as Activities.

A simplified view is:

```text
Client
   |
   v
Temporal / Cadence Service
   |
   +---- durable workflow history/state
   |
   +---- task queue
              |
              v
           Worker
        /          \
 Workflow code   Activity code
```

The worker is not where durable workflow truth lives.

That distinction is important for Aegis Flow.

## Where state lives

Temporal persists workflow state in its service-side persistence layer. Its History Service maintains the ordered Event History and associated workflow state.

The history records things such as:

```text
Workflow started
Workflow task scheduled
Activity scheduled
Activity completed
Timer started
Timer fired
Workflow completed
```

A worker can disappear and another worker can later reconstruct the workflow's logical state by replaying that history.

Cadence follows the same broad event-sourcing model. Its server stores durable state in the configured persistence layer, while workers host user workflow and activity code.

### What I like about this

A worker becomes replaceable.

That gives a clean answer to one of Aegis Flow's original questions:

> What happens if the process that currently understands the workflow disappears?

The answer is not "restore that process."

It is "reconstruct the workflow from durable information."

That is a principle I want to keep.

## Workflow history and replay

Temporal does not persist the in-memory object graph of a workflow after every line of code.

Instead, important decisions and external results become history events.

When a worker receives a Workflow Task, the SDK uses the history to run or replay the workflow code until it reaches the state represented by that history.

For example:

```text
Workflow code asks for Activity A
        |
        v
ScheduleActivity command
        |
        v
server persists ActivityTaskScheduled
        |
        v
Activity executes
        |
        v
server persists ActivityTaskCompleted
        |
        v
workflow is replayed / resumed with that result
```

This is one of the most interesting parts of the design.

The durable representation is not "the program counter is currently on line 54."

It is a history of decisions and outcomes from which the program can reconstruct its state.

## Why workflow code must be deterministic

Replay only works if running the workflow again against the same history produces compatible decisions.

This means workflow code cannot freely do things such as:

```text
read the wall clock directly
generate uncontrolled random values
perform HTTP calls
read an arbitrary external file
```

If replay produces a different sequence of commands than the recorded history expects, the workflow becomes non-deterministic.

This is why Temporal and Cadence provide workflow-safe APIs for things such as timers and why side effects are moved into Activities.

### The benefit

This gives a very strong recovery model.

A worker can reconstruct execution without serializing every detail of the worker's memory.

### The price

Application code has rules that normal Rust or Go code does not have.

Changing a running workflow's code can also become a versioning problem. A code change that changes the sequence of durable decisions may no longer replay old histories correctly.

Cadence addresses this with versioning APIs and replay/shadowing tests. Temporal SDKs similarly detect nondeterminism during replay.

This is powerful, but it is not free.

## Workflows and Activities

The split between Workflow and Activity is one of the ideas I think is worth keeping conceptually.

### Workflow

The workflow decides **what should happen next**.

It should be deterministic and should not directly perform arbitrary external side effects.

### Activity

An Activity performs work in the outside world.

Examples:

```text
broadcast a Bitcoin transaction
call an RPC provider
write to an external database
send an email
invoke another service
```

This boundary solves an important problem: external calls cannot be replayed as if they were pure program logic.

For Aegis Flow, I do not yet know whether I want the same API shape, but I do want a similarly explicit boundary between:

```text
durable decision logic

and

non-deterministic side effects
```

## What happens when a worker crashes?

Workers poll task queues for work.

If a worker disappears, durable workflow state remains in the service.

Temporal can deliver workflow work to another worker, which reconstructs logical state from history.

Temporal also uses "sticky" task queues as an optimization: a worker can cache an already-reconstructed workflow so it does not replay the entire history every time. If that worker disappears, the sticky assignment times out and work can return to the normal queue.

This is a detail I like because it separates:

```text
correctness mechanism = durable history + replay

performance optimization = cached workflow state
```

The cache is useful, but correctness does not depend on it.

That is a good design rule for Aegis Flow.

## The uncomfortable case: Activity succeeded, completion was lost

This is directly relevant to the original Aegis Flow problem.

Imagine:

```text
Worker
   |
   | broadcast transaction
   v
Bitcoin RPC
   |
   | broadcast succeeds
   v
Bitcoin network

Worker crashes before reporting Activity completion.
```

From the workflow engine's perspective, the Activity may later need to run again.

The engine cannot magically prove that an arbitrary external side effect did not already happen.

Temporal's architecture therefore places responsibility on Activity design: side-effecting Activities should be idempotent, or they must be configured/handled so that repeating them is not allowed.

This is a very important lesson:

> Durable execution does not remove ambiguity from external systems.

It gives us a framework in which that ambiguity can be handled deliberately.

For Aegis Flow, blockchain operations will need explicit reconciliation logic.

For example, after an ambiguous transaction broadcast, blindly broadcasting "again" should not be the first action. The system should first inspect network state using a stable transaction identity.

## Timers

Durable timers are another idea worth keeping.

A normal process sleep is not durable:

```text
sleep(6 hours)
```
https://github.com/MarinochkaRin?tab=overview&from=2026-09-01&to=2026-09-16
If the process disappears after three hours, that in-memory timer disappears with it.

Temporal and Cadence represent timer decisions in durable workflow history. A later worker can therefore reconstruct the fact that the workflow is waiting for a timer rather than starting the wait from scratch.

For Aegis Flow this matters for:

```text
retry after backoff
wait for another confirmation
wait before reconciliation
provider cooldown
scheduled re-check
```

I do not want long waits to require a process to remain alive.

## Retries and heartbeats

Both systems support retry policies around Activities.

Cadence also documents heartbeats for long-running Activities. Heartbeats can serve two purposes:

- detect that a worker executing long-running work has disappeared;
- record progress that a later retry can use.

This is useful, but I want to be careful not to use heartbeats for everything.

A short RPC call probably does not need a heartbeat.

A 40-minute indexing Activity might.

The mechanism should match the duration and failure mode of the work.

## Task queues

Workers poll task queues rather than the orchestration service calling workers directly.

That gives several useful properties:

- workers do not need inbound service discovery just to receive work;
- work can remain pending while workers are unavailable;
- workers pull work when they have capacity;
- multiple workers can naturally share load.

Cadence explicitly persists unmatched work when no worker is available.

The general pull-based model is attractive for Aegis Flow.

I do not yet want to commit to Temporal-style Matching Service architecture, because that may be far more machinery than the first version needs.

## Strongest parts of the approach

### 1. Workers are disposable

Durability is not tied to a specific process.

### 2. Recovery has a clear mechanism

State reconstruction is based on persisted execution history rather than ad-hoc recovery code.

### 3. Side effects have an explicit boundary

Workflow logic and external work are treated differently.

### 4. Timers are durable

Long waits do not depend on process uptime.

### 5. Failure is part of normal execution

Worker crashes and transient Activity failures are not exceptional architecture paths added later.

### 6. Replay can be tested

Cadence's replayer and shadower are especially interesting. They allow workflow code changes to be tested against existing histories to detect nondeterministic changes before deployment.

That is a strong idea for long-lived workflows.

## What worries me

### History-driven execution adds mental overhead

Developers need to understand replay and determinism, not just ordinary async code.

### Code evolution becomes harder

A harmless-looking refactor can become incompatible with histories of already-running workflows if it changes durable decisions.

### The server architecture is substantial

Temporal and Cadence include specialized History and Matching services, persistence, task queues, sharding, and a large operational surface.

Aegis Flow does not need to reproduce that architecture to learn from its execution model.

### Event history can grow

Long-running workflows need strategies for keeping history manageable. Mature engines have mechanisms such as continue-as-new and history limits.

That is another piece of complexity I do not want to inherit before it is necessary.

## Ideas worth adopting for Aegis Flow

At this point I would keep these ideas:

1. **Workers are never the source of truth.**
2. **Workflow state must be recoverable from durable data.**
3. **Durable decision logic and external side effects need a visible boundary.**
4. **External work should be designed assuming duplicate execution is possible.**
5. **Timers must be durable rather than process-local.**
6. **Task execution should be pull-based so workers remain replaceable and horizontally scalable.**
7. **Correctness must not depend on an in-memory cache.**
8. **Existing execution histories should eventually become test fixtures for compatibility/recovery tests.**

## Ideas I would not adopt yet

### Full deterministic code replay

This is the biggest open question.

Temporal's replay model is elegant, but it introduces a significant programming and versioning model.

Aegis Flow may not need to replay arbitrary user workflow code from the beginning.

A simpler first design could persist an explicit state machine and commands directly.

I want to compare this with database-backed engines before deciding.

### Separate History and Matching services

They solve real scaling problems, but starting with separate distributed subsystems would add complexity before Aegis Flow has evidence that it needs them.

### Temporal-compatible API concepts

The goal is not to build a small Temporal clone.

If Aegis Flow adopts a concept, it should be because the concept solves one of our stated problems, not because Temporal exposes it.

## Questions this review leaves open

The next comparisons need to answer these:

1. Do we need event-sourced replay, or is an explicit persisted state machine enough?
2. Should workflow history be authoritative, or should current state be authoritative with history used for audit/recovery?
3. What is the smallest durable timer model we can build?
4. Can PostgreSQL safely implement task claiming and worker leases for the first version?
5. How should ambiguous external side effects be represented as domain state?
6. How much determinism should Aegis Flow require from workflow definitions?
7. At what point would a dedicated queue/matching subsystem become justified?

I do not want to answer these from Temporal alone.

The next useful comparison is a system with a simpler developer model and then a deliberately simple PostgreSQL-backed design.

---

## Sources reviewed

### Temporal

- Temporal architecture — https://docs.temporal.io/
- Temporal server architecture and Event History — https://github.com/temporalio/documentation/tree/main/docs/encyclopedia
- Temporal server architecture notes — https://github.com/temporalio/temporal/tree/main/docs/architecture
- Temporal Rust/Core SDK architecture — https://github.com/temporalio/sdk-rust/blob/main/ARCHITECTURE.md
- Temporal SDK internals and replay notes — https://github.com/temporalio/sdk-rust/blob/main/arch_docs/sdks_intro.md
- Sticky task queue design — https://github.com/temporalio/sdk-rust/blob/main/arch_docs/sticky_queues.md

### Cadence

- Workflow engine concepts — https://cadenceworkflow.io/docs/concepts/workflow-engine
- Deployment topology — https://cadenceworkflow.io/docs/concepts/topology
- Task lists — https://cadenceworkflow.io/docs/concepts/task-lists
- Activities — https://cadenceworkflow.io/docs/concepts/activities
- Timers — https://cadenceworkflow.io/docs/concepts/timers
- Retries — https://cadenceworkflow.io/docs/go-client/retries
- Workflow versioning — https://cadenceworkflow.io/docs/go-client/workflow-versioning
- Workflow replay and shadowing — https://cadenceworkflow.io/docs/go-client/workflow-replay-shadowing

---

# Cloudflare Workflows

Research pending.

# AWS Step Functions

Research pending.

# Database-backed approaches

Research pending.

# Aegis Flow conclusions

To be written after the individual reviews are complete.