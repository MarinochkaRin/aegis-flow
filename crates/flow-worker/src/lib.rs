//! Worker runtime for Aegis Flow.
//!
//! This crate coordinates one claimed activity at a time: claim -> execute ->
//! heartbeat -> finish. PostgreSQL remains authoritative for lease ownership.
//! A worker never treats a lost lease as proof that an external side effect did
//! not happen.

use std::time::Duration;

use async_trait::async_trait;
use flow_domain::{ActivityId, ActivityOutcome, LeaseToken};
use flow_storage_pg::{ClaimedActivity, PgStorage, StorageError};
use thiserror::Error;
use tokio::time::{self, MissedTickBehavior};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    worker_id: String,
    lease_duration: Duration,
    heartbeat_interval: Duration,
    retry_delay: Duration,
}

impl WorkerConfig {
    pub fn new(
        worker_id: impl Into<String>,
        lease_duration: Duration,
        heartbeat_interval: Duration,
        retry_delay: Duration,
    ) -> Result<Self, WorkerConfigError> {
        let worker_id = worker_id.into();
        if worker_id.trim().is_empty() {
            return Err(WorkerConfigError::EmptyWorkerId);
        }
        if lease_duration.is_zero() {
            return Err(WorkerConfigError::ZeroLeaseDuration);
        }
        if heartbeat_interval.is_zero() {
            return Err(WorkerConfigError::ZeroHeartbeatInterval);
        }
        if heartbeat_interval >= lease_duration {
            return Err(WorkerConfigError::HeartbeatNotBeforeLeaseExpiry);
        }

        Ok(Self {
            worker_id,
            lease_duration,
            heartbeat_interval,
            retry_delay,
        })
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    pub fn heartbeat_interval(&self) -> Duration {
        self.heartbeat_interval
    }

    pub fn retry_delay(&self) -> Duration {
        self.retry_delay
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkerConfigError {
    #[error("worker ID must not be empty")]
    EmptyWorkerId,
    #[error("lease duration must be positive")]
    ZeroLeaseDuration,
    #[error("heartbeat interval must be positive")]
    ZeroHeartbeatInterval,
    #[error("heartbeat interval must be shorter than the lease duration")]
    HeartbeatNotBeforeLeaseExpiry,
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// The result of one claim/execute cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunResult {
    /// No eligible activity was available.
    Idle,
    /// The database accepted the worker's final result while the lease was valid.
    Finished {
        activity_id: ActivityId,
        outcome: ActivityOutcome,
    },
    /// Ownership was lost before the result could be durably accepted.
    ///
    /// This does not mean the external operation failed or was cancelled.
    LeaseLost { activity_id: ActivityId },
}

/// Application code implements the actual activity side effect.
///
/// Returning `Ambiguous` is required when the executor cannot determine whether
/// an external effect happened (for example, an RPC timed out after submission).
#[async_trait]
pub trait ActivityExecutor: Send + Sync {
    async fn execute(&self, activity: ClaimedActivity) -> ActivityOutcome;
}

/// Minimal storage surface needed by the worker runtime.
///
/// Keeping this trait smaller than `PgStorage` makes the worker independently
/// testable and keeps PostgreSQL-specific details outside the execution loop.
#[async_trait]
pub trait WorkerStorage: Send + Sync {
    async fn claim_activity(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<ClaimedActivity>, StorageError>;

    async fn heartbeat_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        extension: Duration,
    ) -> Result<bool, StorageError>;

    async fn finish_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        outcome: ActivityOutcome,
        retry_delay: Option<Duration>,
    ) -> Result<bool, StorageError>;
}

#[async_trait]
impl WorkerStorage for PgStorage {
    async fn claim_activity(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<ClaimedActivity>, StorageError> {
        PgStorage::claim_activity(self, worker_id, lease_duration).await
    }

    async fn heartbeat_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        extension: Duration,
    ) -> Result<bool, StorageError> {
        Ok(
            PgStorage::heartbeat_activity(self, activity_id, lease_token, extension)
                .await?
                .is_some(),
        )
    }

    async fn finish_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        outcome: ActivityOutcome,
        retry_delay: Option<Duration>,
    ) -> Result<bool, StorageError> {
        PgStorage::finish_activity_after(self, activity_id, lease_token, outcome, retry_delay).await
    }
}

pub struct Worker<S, E> {
    storage: S,
    executor: E,
    config: WorkerConfig,
}

impl<S, E> Worker<S, E>
where
    S: WorkerStorage,
    E: ActivityExecutor,
{
    pub fn new(storage: S, executor: E, config: WorkerConfig) -> Self {
        Self {
            storage,
            executor,
            config,
        }
    }

    pub fn config(&self) -> &WorkerConfig {
        &self.config
    }

    /// Claims and executes at most one activity.
    ///
    /// Heartbeats extend ownership while the executor is running. If heartbeat
    /// renewal or final completion is rejected, the worker stops claiming
    /// ownership of the result and returns `LeaseLost`.
    pub async fn run_once(&self) -> Result<RunResult, WorkerError> {
        let Some(claim) = self
            .storage
            .claim_activity(self.config.worker_id(), self.config.lease_duration())
            .await?
        else {
            return Ok(RunResult::Idle);
        };

        let mut execution = Box::pin(self.executor.execute(claim));
        let mut heartbeat = time::interval(self.config.heartbeat_interval());
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Tokio intervals tick immediately once. Consume that first tick so the
        // first heartbeat happens after the configured interval.
        heartbeat.tick().await;

        loop {
            tokio::select! {
                outcome = &mut execution => {
                    let retry_delay = match outcome {
                        ActivityOutcome::RetryableFailure => Some(self.config.retry_delay()),
                        _ => None,
                    };
                    let accepted = self.storage.finish_activity(
                        claim.activity_id,
                        claim.lease_token,
                        outcome,
                        retry_delay,
                    ).await?;

                    return Ok(if accepted {
                        RunResult::Finished {
                            activity_id: claim.activity_id,
                            outcome,
                        }
                    } else {
                        RunResult::LeaseLost {
                            activity_id: claim.activity_id,
                        }
                    });
                }
                _ = heartbeat.tick() => {
                    let still_owned = self.storage.heartbeat_activity(
                        claim.activity_id,
                        claim.lease_token,
                        self.config.lease_duration(),
                    ).await?;

                    if !still_owned {
                        return Ok(RunResult::LeaseLost {
                            activity_id: claim.activity_id,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use chrono::Utc;
    use flow_domain::{Attempt, WorkflowId};
    use tokio::sync::Mutex;

    use super::*;

    type FinishRecord = (ActivityId, ActivityOutcome, Option<Duration>);

    #[derive(Clone)]
    struct FakeStorage {
        claim: Arc<Mutex<Option<ClaimedActivity>>>,
        heartbeat_allowed: Arc<AtomicBool>,
        heartbeats: Arc<AtomicUsize>,
        finishes: Arc<Mutex<Vec<FinishRecord>>>,
    }

    impl FakeStorage {
        fn empty() -> Self {
            Self {
                claim: Arc::new(Mutex::new(None)),
                heartbeat_allowed: Arc::new(AtomicBool::new(true)),
                heartbeats: Arc::new(AtomicUsize::new(0)),
                finishes: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_claim() -> Self {
            let claim = ClaimedActivity {
                activity_id: ActivityId::new(),
                workflow_id: WorkflowId::new(),
                attempt: Attempt::new(1).unwrap(),
                lease_token: LeaseToken::new(),
                lease_until: Utc::now(),
            };

            Self {
                claim: Arc::new(Mutex::new(Some(claim))),
                heartbeat_allowed: Arc::new(AtomicBool::new(true)),
                heartbeats: Arc::new(AtomicUsize::new(0)),
                finishes: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl WorkerStorage for FakeStorage {
        async fn claim_activity(
            &self,
            _worker_id: &str,
            _lease_duration: Duration,
        ) -> Result<Option<ClaimedActivity>, StorageError> {
            Ok(self.claim.lock().await.take())
        }

        async fn heartbeat_activity(
            &self,
            _activity_id: ActivityId,
            _lease_token: LeaseToken,
            _extension: Duration,
        ) -> Result<bool, StorageError> {
            self.heartbeats.fetch_add(1, Ordering::SeqCst);
            Ok(self.heartbeat_allowed.load(Ordering::SeqCst))
        }

        async fn finish_activity(
            &self,
            activity_id: ActivityId,
            _lease_token: LeaseToken,
            outcome: ActivityOutcome,
            retry_delay: Option<Duration>,
        ) -> Result<bool, StorageError> {
            self.finishes
                .lock()
                .await
                .push((activity_id, outcome, retry_delay));
            Ok(true)
        }
    }

    struct FakeExecutor {
        delay: Duration,
        outcome: ActivityOutcome,
    }

    #[async_trait]
    impl ActivityExecutor for FakeExecutor {
        async fn execute(&self, _activity: ClaimedActivity) -> ActivityOutcome {
            time::sleep(self.delay).await;
            self.outcome
        }
    }

    fn config() -> WorkerConfig {
        WorkerConfig::new(
            "worker-test",
            Duration::from_millis(200),
            Duration::from_millis(20),
            Duration::from_millis(50),
        )
        .unwrap()
    }

    #[test]
    fn config_rejects_unsafe_heartbeat_schedule() {
        assert!(matches!(
            WorkerConfig::new(
                "worker",
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::ZERO,
            ),
            Err(WorkerConfigError::HeartbeatNotBeforeLeaseExpiry)
        ));
    }

    #[tokio::test]
    async fn idle_when_no_activity_is_available() {
        let worker = Worker::new(
            FakeStorage::empty(),
            FakeExecutor {
                delay: Duration::ZERO,
                outcome: ActivityOutcome::Success,
            },
            config(),
        );

        assert_eq!(worker.run_once().await.unwrap(), RunResult::Idle);
    }

    #[tokio::test]
    async fn heartbeats_while_execution_is_running_and_finishes_once() {
        let storage = FakeStorage::with_claim();
        let observations = storage.clone();
        let worker = Worker::new(
            storage,
            FakeExecutor {
                delay: Duration::from_millis(75),
                outcome: ActivityOutcome::Success,
            },
            config(),
        );

        let result = worker.run_once().await.unwrap();
        assert!(matches!(
            result,
            RunResult::Finished {
                outcome: ActivityOutcome::Success,
                ..
            }
        ));
        assert!(observations.heartbeats.load(Ordering::SeqCst) >= 2);
        assert_eq!(observations.finishes.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn retryable_failure_uses_configured_retry_delay() {
        let storage = FakeStorage::with_claim();
        let observations = storage.clone();
        let worker = Worker::new(
            storage,
            FakeExecutor {
                delay: Duration::ZERO,
                outcome: ActivityOutcome::RetryableFailure,
            },
            config(),
        );

        let result = worker.run_once().await.unwrap();
        assert!(matches!(
            result,
            RunResult::Finished {
                outcome: ActivityOutcome::RetryableFailure,
                ..
            }
        ));
        let finishes = observations.finishes.lock().await;
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].2, Some(Duration::from_millis(50)));
    }

    #[tokio::test]
    async fn rejected_heartbeat_prevents_stale_finish() {
        let storage = FakeStorage::with_claim();
        storage.heartbeat_allowed.store(false, Ordering::SeqCst);
        let observations = storage.clone();
        let worker = Worker::new(
            storage,
            FakeExecutor {
                delay: Duration::from_millis(100),
                outcome: ActivityOutcome::Success,
            },
            config(),
        );

        assert!(matches!(
            worker.run_once().await.unwrap(),
            RunResult::LeaseLost { .. }
        ));
        assert!(observations.finishes.lock().await.is_empty());
    }
}
