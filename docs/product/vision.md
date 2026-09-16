# Vision

## The question behind Aegis Flow

Aegis Flow started from a failure scenario rather than from a technology choice.

Consider a system that sends a transaction to an external network:

```text
Worker
   |
   | request
   v
External system
   |
   | operation succeeds
   v
State changes

...but the response never reaches the worker.
```

The worker observes a timeout. The external system may have already completed the operation.

> Did the operation actually happen?

This immediately raises several questions:

- Is retrying safe?
- How can another worker continue the operation?
- Which state should be considered authoritative?
- How can the system explain what happened later?

These questions appear in many distributed systems.

Blockchain infrastructure makes them especially interesting because an unsafe retry may represent a real financial operation.

## What I want to explore

Aegis Flow is an engineering exploration of durable execution.

The project investigates how to represent long-running work in a way that remains understandable and recoverable when processes, machines, networks, and external services fail.

The main areas of interest are:

- explicit workflow state;
- durable execution;
- failure recovery;
- idempotency;
- retries and backoff;
- ambiguous external side effects;
- worker coordination;
- event history;
- observability;
- distributed state machines.

Rust is used not only as the implementation language, but as part of the experiment.

The project will explore how ownership, enums, newtypes, traits, explicit error types, and later concurrency primitives can make important system invariants visible in code.

## First real-world use case

The first domain used to exercise the engine will be blockchain transaction execution.

A simplified transaction lifecycle may look like:

```text
Prepare transaction
        |
        v
Broadcast
        |
        v
Observe propagation
        |
        v
Wait for confirmation
        |
        v
Wait for finality
```

The workflow may remain active for seconds, minutes, or hours.

During that time:

- the process may restart;
- the worker may disappear;
- the RPC provider may fail;
- the same activity may be delivered again;
- the result of a remote call may be unknown;
- the blockchain may reorganize.

The workflow must not depend on the lifetime of a single worker.

## Not a blockchain-specific engine

Blockchain is the first demanding use case, not a dependency of the core architecture.

Domain-specific behaviour should live outside the workflow core.

The same execution model should eventually be capable of supporting workflows such as:

- blockchain transaction tracking;
- data import;
- report generation;
- external API orchestration;
- long-running data processing;
- AI inference pipelines.

## What success looks like

Aegis Flow should eventually be able to answer four questions clearly:

1. What should happen next?
2. What has already happened?
3. What happens if the current worker disappears?
4. Can an operator reconstruct why the system reached its current state?

If the architecture cannot answer one of these questions clearly, the execution model is incomplete.