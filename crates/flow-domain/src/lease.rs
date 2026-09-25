//! In-memory lease rules. Persistent lease decisions must use the database's
//! authoritative time and atomic updates; this module alone is not a lock.

use std::time::{Duration, SystemTime};

use crate::LeaseToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseError {
    ZeroDuration,
    DeadlineOverflow,
    StaleToken,
    Expired,
}

/// Records the token and deadline for *one* claim of an activity.
///
/// `Lease` is deliberately independent of a system clock. The caller passes
/// `now`, which must come from the authoritative clock in the storage layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    token: LeaseToken,
    expires_at: SystemTime,
}

impl Lease {
    pub fn new(token: LeaseToken, now: SystemTime, duration: Duration) -> Result<Self, LeaseError> {
        if duration.is_zero() {
            return Err(LeaseError::ZeroDuration);
        }
        let expires_at = now
            .checked_add(duration)
            .ok_or(LeaseError::DeadlineOverflow)?;
        Ok(Self { token, expires_at })
    }

    pub fn token(&self) -> LeaseToken {
        self.token
    }

    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    /// At the exact deadline, ownership has already expired.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at
    }

    /// Validate a completion attempt against both the token and the deadline.
    /// The storage layer must repeat these checks in a single atomic update.
    pub fn validate_completion(
        &self,
        token: &LeaseToken,
        now: SystemTime,
    ) -> Result<(), LeaseError> {
        if &self.token != token {
            return Err(LeaseError::StaleToken);
        }
        if self.is_expired(now) {
            return Err(LeaseError::Expired);
        }
        Ok(())
    }

    /// Extend a live lease. An early heartbeat never shortens its deadline.
    pub fn renew(
        &mut self,
        token: &LeaseToken,
        now: SystemTime,
        duration: Duration,
    ) -> Result<SystemTime, LeaseError> {
        self.validate_completion(token, now)?;
        if duration.is_zero() {
            return Err(LeaseError::ZeroDuration);
        }
        let proposed = now
            .checked_add(duration)
            .ok_or(LeaseError::DeadlineOverflow)?;
        self.expires_at = self.expires_at.max(proposed);
        Ok(self.expires_at)
    }
}

/// The caller classifies the external effect *before* starting an activity.
/// A missing worker heartbeat is not proof that an external call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEffectPolicy {
    /// Retrying is safe because the activity is pure or idempotent.
    SafeToRetry,
    /// External state must be checked before retrying.
    RequiresReconciliation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredLeaseAction {
    ScheduleRetry,
    MarkAmbiguous,
}

impl ExternalEffectPolicy {
    pub fn on_lease_expired(self) -> ExpiredLeaseAction {
        match self {
            Self::SafeToRetry => ExpiredLeaseAction::ScheduleRetry,
            Self::RequiresReconciliation => ExpiredLeaseAction::MarkAmbiguous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn start() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10_000)
    }

    #[test]
    fn lease_rejects_zero_duration() {
        assert_eq!(
            Lease::new(LeaseToken::new(), start(), Duration::ZERO),
            Err(LeaseError::ZeroDuration)
        );
    }

    #[test]
    fn lease_expires_at_its_deadline() {
        let lease = Lease::new(LeaseToken::new(), start(), Duration::from_secs(30)).unwrap();
        assert!(!lease.is_expired(start() + Duration::from_secs(29)));
        assert!(lease.is_expired(start() + Duration::from_secs(30)));
    }

    #[test]
    fn a_wrong_token_cannot_complete() {
        let lease = Lease::new(LeaseToken::new(), start(), Duration::from_secs(30)).unwrap();
        assert_eq!(
            lease.validate_completion(&LeaseToken::new(), start()),
            Err(LeaseError::StaleToken)
        );
    }

    #[test]
    fn completion_after_deadline_is_rejected() {
        let token = LeaseToken::new();
        let lease = Lease::new(token, start(), Duration::from_secs(30)).unwrap();
        assert_eq!(
            lease.validate_completion(&token, start() + Duration::from_secs(30)),
            Err(LeaseError::Expired)
        );
    }

    #[test]
    fn a_valid_early_heartbeat_cannot_shorten_the_lease() {
        let token = LeaseToken::new();
        let mut lease = Lease::new(token, start(), Duration::from_secs(60)).unwrap();
        let initial_deadline = lease.expires_at();
        let renewed = lease
            .renew(
                &token,
                start() + Duration::from_secs(5),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(renewed, initial_deadline);
    }

    #[test]
    fn a_valid_heartbeat_can_extend_the_lease() {
        let token = LeaseToken::new();
        let mut lease = Lease::new(token, start(), Duration::from_secs(30)).unwrap();
        let new_deadline = lease
            .renew(
                &token,
                start() + Duration::from_secs(20),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(new_deadline, start() + Duration::from_secs(50));
    }

    #[test]
    fn failed_renewal_does_not_mutate_the_lease() {
        let token = LeaseToken::new();
        let mut lease = Lease::new(token, start(), Duration::from_secs(30)).unwrap();
        let before = lease.expires_at();
        assert_eq!(
            lease.renew(&LeaseToken::new(), start(), Duration::from_secs(60)),
            Err(LeaseError::StaleToken)
        );
        assert_eq!(lease.expires_at(), before);
        assert_eq!(
            lease.renew(
                &token,
                start() + Duration::from_secs(31),
                Duration::from_secs(60)
            ),
            Err(LeaseError::Expired)
        );
        assert_eq!(lease.expires_at(), before);
    }

    #[test]
    fn a_missing_heartbeat_does_not_prove_the_external_call_failed() {
        assert_eq!(
            ExternalEffectPolicy::SafeToRetry.on_lease_expired(),
            ExpiredLeaseAction::ScheduleRetry
        );
        assert_eq!(
            ExternalEffectPolicy::RequiresReconciliation.on_lease_expired(),
            ExpiredLeaseAction::MarkAmbiguous
        );
    }
}
