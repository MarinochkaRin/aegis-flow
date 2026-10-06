//! PostgreSQL adapter for the Aegis Flow worker-facing SQL contract.
//!
//! PostgreSQL's functions are authoritative for claiming, heartbeats, and
//! completion. This adapter never decides lease validity using its own clock.
//! A Tokio runtime must drive the `tokio_postgres::Connection` associated
//! with the `Client` passed to [`PgStorage::new`].

use std::time::Duration;

use chrono::{DateTime, Utc};
use flow_domain::{ActivityId, ActivityOutcome, Attempt, LeaseToken, WorkflowId};
use thiserror::Error;
use tokio_postgres::Client;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
    #[error("duration must be representable as a nonzero number of microseconds")]
    InvalidDuration,
    #[error("database returned invalid attempt: {0}")]
    InvalidAttempt(i64),
    #[error("RetryableFailure requires a durable next-available timestamp")]
    MissingRetryTimestamp,
    #[error("only RetryableFailure accepts a next-available timestamp")]
    UnexpectedRetryTimestamp,
}

/// The authoritative claim result returned by PostgreSQL.
///
/// `lease_until` uses database time; do not reconstruct this deadline from
/// the worker's local clock. Even with a valid token, completion can fail
/// if the lease expired before PostgreSQL processed the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimedActivity {
    pub activity_id: ActivityId,
    pub workflow_id: WorkflowId,
    pub attempt: Attempt,
    pub lease_token: LeaseToken,
    pub lease_until: DateTime<Utc>,
}

/// A single-connection adapter. Use one client per worker for now; replace
/// with an explicitly chosen connection pool in a later milestone.
pub struct PgStorage {
    client: Client,
}

impl PgStorage {
    /// The caller owns the lifetime of the separate PostgreSQL connection
    /// future and must poll/spawn it, as required by `tokio-postgres`.
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Atomically claims one due activity, or returns None if none is available.
    pub async fn claim_activity(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<ClaimedActivity>, StorageError> {
        let interval = interval_literal(lease_duration, false)?;
        let row = self
            .client
            .query_opt(
                "SELECT * FROM aegis.claim_activity($1, $2::text::interval)",
                &[&worker_id, &interval],
            )
            .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let raw_attempt: i64 = row.get("attempt");
        let attempt_u32 =
            u32::try_from(raw_attempt).map_err(|_| StorageError::InvalidAttempt(raw_attempt))?;
        let attempt =
            Attempt::new(attempt_u32).map_err(|_| StorageError::InvalidAttempt(raw_attempt))?;
        Ok(Some(ClaimedActivity {
            activity_id: ActivityId::from_uuid(row.get("activity_id")),
            workflow_id: WorkflowId::from_uuid(row.get("workflow_id")),
            attempt,
            lease_token: LeaseToken::from_uuid(row.get("lease_token")),
            lease_until: row.get("lease_until"),
        }))
    }

    /// None means the token is stale, the activity is not running, or its
    /// lease already expired. It does not prove an external effect failed.
    pub async fn heartbeat_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        extension: Duration,
    ) -> Result<Option<DateTime<Utc>>, StorageError> {
        let interval = interval_literal(extension, false)?;
        let row = self
            .client
            .query_one(
                "SELECT aegis.heartbeat_activity($1, $2, $3::text::interval)",
                &[&activity_id.as_uuid(), &lease_token.as_uuid(), &interval],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `false` means the database rejected this completion. A rejected
    /// completion must never be interpreted as confirmation that an external
    /// transaction did not happen.
    pub async fn finish_activity(
        &self,
        activity_id: ActivityId,
        lease_token: LeaseToken,
        outcome: ActivityOutcome,
        next_available_at: Option<DateTime<Utc>>,
    ) -> Result<bool, StorageError> {
        let outcome_label = match outcome {
            ActivityOutcome::Success => "Success",
            ActivityOutcome::RetryableFailure => "RetryableFailure",
            ActivityOutcome::PermanentFailure => "PermanentFailure",
            ActivityOutcome::Ambiguous => "Ambiguous",
        };
        match (outcome, next_available_at.is_some()) {
            (ActivityOutcome::RetryableFailure, false) => {
                return Err(StorageError::MissingRetryTimestamp);
            }
            (ActivityOutcome::RetryableFailure, true) => {}
            (_, true) => return Err(StorageError::UnexpectedRetryTimestamp),
            (_, false) => {}
        }
        let row = self
            .client
            .query_one(
                "SELECT aegis.finish_activity($1, $2, $3, $4)",
                &[
                    &activity_id.as_uuid(),
                    &lease_token.as_uuid(),
                    &outcome_label,
                    &next_available_at,
                ],
            )
            .await?;
        Ok(row.get(0))
    }

    /// Moves up to 100 expired Running activities to RetryWaiting or Ambiguous
    /// according to their immutable effect policy. Zero delay is permitted.
    pub async fn recover_expired(&self, retry_delay: Duration) -> Result<i32, StorageError> {
        let interval = interval_literal(retry_delay, true)?;
        let row = self
            .client
            .query_one("SELECT aegis.recover_expired($1::interval)", &[&interval])
            .await?;
        Ok(row.get(0))
    }

    /// Makes up to 100 due RetryWaiting activities available for claims.
    pub async fn release_due_retries(&self) -> Result<i32, StorageError> {
        let row = self
            .client
            .query_one("SELECT aegis.release_due_retries()", &[])
            .await?;
        Ok(row.get(0))
    }
}

/// Build a parameterized PostgreSQL INTERVAL literal without rounding a
/// sub-microsecond duration up to a nonzero lease.
fn interval_literal(duration: Duration, allow_zero: bool) -> Result<String, StorageError> {
    let micros = i64::try_from(duration.as_micros()).map_err(|_| StorageError::InvalidDuration)?;
    if micros == 0 && !allow_zero {
        return Err(StorageError::InvalidDuration);
    }
    Ok(format!("{micros} microseconds"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_sub_microsecond_claims() {
        assert!(matches!(
            interval_literal(Duration::ZERO, false),
            Err(StorageError::InvalidDuration)
        ));
        assert!(matches!(
            interval_literal(Duration::from_nanos(999), false),
            Err(StorageError::InvalidDuration)
        ));
    }

    #[test]
    fn permits_zero_recovery_delay_and_preserves_microseconds() {
        assert_eq!(
            interval_literal(Duration::ZERO, true).unwrap(),
            "0 microseconds"
        );
        assert_eq!(
            interval_literal(Duration::from_micros(1_500), false).unwrap(),
            "1500 microseconds"
        );
    }
}
