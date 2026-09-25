//! Domain entities. No async runtime, SQL, or network dependencies.
//! Time is supplied by the caller; database time will be authoritative later.

use std::time::{Duration, SystemTime};

use crate::{
    ActivityId, ActivityOutcome, ActivityState, Attempt, AttemptError, ExpiredLeaseAction,
    ExternalEffectPolicy, Lease, LeaseError, LeaseToken, WorkflowId, WorkflowState,
};

/// A single workflow's current domain state.
///
/// `Workflow` does not track child activities yet. Completion preconditions
/// belong to the future workflow coordinator; this entity validates state edges.
#[derive(Debug)]
pub struct Workflow {
    id: WorkflowId,
    state: WorkflowState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowError {
    InvalidTransition {
        from: WorkflowState,
        to: WorkflowState,
    },
}

impl Workflow {
    pub fn new(id: WorkflowId) -> Self {
        Self {
            id,
            state: WorkflowState::Created,
        }
    }

    pub fn id(&self) -> WorkflowId {
        self.id
    }

    pub fn state(&self) -> WorkflowState {
        self.state
    }

    pub fn transition_to(&mut self, next: WorkflowState) -> Result<(), WorkflowError> {
        if !self.state.can_transition_to(next) {
            return Err(WorkflowError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

/// A reconciliation decision made *after* checking the external system.
/// The engine cannot establish this evidence on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    ConfirmedSuccess,
    ConfirmedFailure,
    SafeToRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityError {
    InvalidTransition {
        from: ActivityState,
        to: ActivityState,
    },
    NoActiveLease,
    LeaseNotExpired,
    Lease(LeaseError),
    Attempt(AttemptError),
}

/// A single unit of work belonging to a workflow.
///
/// The active lease includes both identity and deadline. These in-memory
/// checks are not a distributed lock: storage must enforce token + deadline
/// predicates in one atomic database update using authoritative DB time.
pub struct Activity {
    id: ActivityId,
    workflow_id: WorkflowId,
    state: ActivityState,
    attempt: Option<Attempt>,
    active_lease: Option<Lease>,
}

impl Activity {
    pub fn new(id: ActivityId, workflow_id: WorkflowId) -> Self {
        Self {
            id,
            workflow_id,
            state: ActivityState::Pending,
            attempt: None,
            active_lease: None,
        }
    }

    pub fn id(&self) -> ActivityId {
        self.id
    }

    pub fn workflow_id(&self) -> WorkflowId {
        self.workflow_id
    }

    pub fn state(&self) -> ActivityState {
        self.state
    }

    pub fn attempt(&self) -> Option<Attempt> {
        self.attempt
    }

    pub fn active_lease(&self) -> Option<Lease> {
        self.active_lease
    }

    /// Start an attempt using an unexpired lease. `now` must eventually be
    /// provided by PostgreSQL, not the worker's independent wall clock.
    pub fn claim(&mut self, lease: Lease, now: SystemTime) -> Result<Attempt, ActivityError> {
        if !self.state.can_transition_to(ActivityState::Running) {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: ActivityState::Running,
            });
        }

        let token = lease.token();
        lease
            .validate_completion(&token, now)
            .map_err(ActivityError::Lease)?;

        // Check overflow BEFORE modifying any field.
        let next_attempt = match self.attempt {
            Some(previous) => previous.next(),
            None => Attempt::new(1),
        }
        .map_err(ActivityError::Attempt)?;

        self.attempt = Some(next_attempt);
        self.active_lease = Some(lease);
        self.state = ActivityState::Running;
        Ok(next_attempt)
    }

    /// Renew only the current, unexpired lease while the activity is running.
    pub fn heartbeat(
        &mut self,
        token: &LeaseToken,
        now: SystemTime,
        duration: Duration,
    ) -> Result<SystemTime, ActivityError> {
        if self.state != ActivityState::Running {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: ActivityState::Running,
            });
        }

        self.active_lease
            .as_mut()
            .ok_or(ActivityError::NoActiveLease)?
            .renew(token, now, duration)
            .map_err(ActivityError::Lease)
    }

    /// Finish the attempt only if the token matches AND the lease is live.
    /// The storage adapter must make this check atomic with state mutation.
    pub fn finish(
        &mut self,
        token: &LeaseToken,
        now: SystemTime,
        outcome: ActivityOutcome,
    ) -> Result<ActivityState, ActivityError> {
        let next = match outcome {
            ActivityOutcome::Success => ActivityState::Succeeded,
            ActivityOutcome::RetryableFailure => ActivityState::RetryWaiting,
            ActivityOutcome::PermanentFailure => ActivityState::Failed,
            ActivityOutcome::Ambiguous => ActivityState::Ambiguous,
        };

        if !self.state.can_transition_to(next) {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }

        self.active_lease
            .as_ref()
            .ok_or(ActivityError::NoActiveLease)?
            .validate_completion(token, now)
            .map_err(ActivityError::Lease)?;

        self.state = next;
        self.active_lease = None;
        Ok(next)
    }

    /// After expiry, select recovery according to the activity's declared
    /// effect policy. Storage must persist this policy before execution.
    /// Expiry does not prove whether an external call succeeded.
    pub fn recover_expired(
        &mut self,
        now: SystemTime,
        policy: ExternalEffectPolicy,
    ) -> Result<ExpiredLeaseAction, ActivityError> {
        let action = policy.on_lease_expired();
        let next = match action {
            ExpiredLeaseAction::ScheduleRetry => ActivityState::RetryWaiting,
            ExpiredLeaseAction::MarkAmbiguous => ActivityState::Ambiguous,
        };

        if !self.state.can_transition_to(next) {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        let lease = self
            .active_lease
            .as_ref()
            .ok_or(ActivityError::NoActiveLease)?;
        if !lease.is_expired(now) {
            return Err(ActivityError::LeaseNotExpired);
        }

        self.state = next;
        self.active_lease = None;
        Ok(action)
    }

    /// The scheduler calls this only after its *durable* backoff has elapsed.
    /// This domain method cannot verify a persisted timer by itself.
    pub fn scheduler_mark_ready(&mut self) -> Result<(), ActivityError> {
        if !self.state.can_transition_to(ActivityState::Pending) {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: ActivityState::Pending,
            });
        }
        self.state = ActivityState::Pending;
        Ok(())
    }

    /// Only the application can provide evidence for a reconciliation result.
    pub fn resolve_ambiguous(
        &mut self,
        resolution: Reconciliation,
    ) -> Result<ActivityState, ActivityError> {
        let next = match resolution {
            Reconciliation::ConfirmedSuccess => ActivityState::Succeeded,
            Reconciliation::ConfirmedFailure => ActivityState::Failed,
            Reconciliation::SafeToRetry => ActivityState::RetryWaiting,
        };

        if !self.state.can_transition_to(next) {
            return Err(ActivityError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn start() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10_000)
    }

    fn lease() -> Lease {
        Lease::new(LeaseToken::new(), start(), Duration::from_secs(30)).unwrap()
    }

    fn activity() -> Activity {
        Activity::new(ActivityId::new(), WorkflowId::new())
    }

    #[test]
    fn workflow_rejects_invalid_transition_without_mutating_state() {
        let id = WorkflowId::new();
        let mut workflow = Workflow::new(id);
        assert_eq!(workflow.id(), id);
        assert_eq!(workflow.state(), WorkflowState::Created);
        assert_eq!(
            workflow.transition_to(WorkflowState::Completed),
            Err(WorkflowError::InvalidTransition {
                from: WorkflowState::Created,
                to: WorkflowState::Completed,
            })
        );
        assert_eq!(workflow.state(), WorkflowState::Created);
    }

    #[test]
    fn terminal_workflow_does_not_resume() {
        let mut workflow = Workflow::new(WorkflowId::new());
        workflow.transition_to(WorkflowState::Running).unwrap();
        workflow.transition_to(WorkflowState::Completed).unwrap();
        assert!(workflow.transition_to(WorkflowState::Running).is_err());
    }

    #[test]
    fn activity_starts_without_attempt_and_can_be_claimed() {
        let mut activity = activity();
        assert_eq!(activity.attempt(), None);
        let attempt = activity.claim(lease(), start()).unwrap();
        assert_eq!(attempt.value(), 1);
        assert_eq!(activity.state(), ActivityState::Running);
        assert!(activity.active_lease().is_some());
    }

    #[test]
    fn stale_lease_cannot_complete_activity() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        assert_eq!(
            activity.finish(&LeaseToken::new(), start(), ActivityOutcome::Success),
            Err(ActivityError::Lease(LeaseError::StaleToken))
        );
        assert_eq!(activity.state(), ActivityState::Running);
        assert_eq!(
            activity.finish(&lease.token(), start(), ActivityOutcome::Success),
            Ok(ActivityState::Succeeded)
        );
        assert_eq!(activity.active_lease(), None);
    }

    #[test]
    fn succeeded_activity_cannot_be_claimed_again() {
        let mut activity = activity();
        let first = lease();
        activity.claim(first, start()).unwrap();
        activity
            .finish(&first.token(), start(), ActivityOutcome::Success)
            .unwrap();
        assert_eq!(
            activity.claim(lease(), start()),
            Err(ActivityError::InvalidTransition {
                from: ActivityState::Succeeded,
                to: ActivityState::Running,
            })
        );
        assert_eq!(activity.attempt().unwrap().value(), 1);
    }

    #[test]
    fn ambiguous_result_needs_explicit_reconciliation() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        assert_eq!(
            activity.finish(&lease.token(), start(), ActivityOutcome::Ambiguous),
            Ok(ActivityState::Ambiguous)
        );
        assert_eq!(
            activity.resolve_ambiguous(Reconciliation::ConfirmedSuccess),
            Ok(ActivityState::Succeeded)
        );
        assert_eq!(activity.attempt().unwrap().value(), 1);
    }

    #[test]
    fn cannot_reconcile_before_ambiguous_result() {
        let mut activity = activity();
        assert_eq!(
            activity.resolve_ambiguous(Reconciliation::SafeToRetry),
            Err(ActivityError::InvalidTransition {
                from: ActivityState::Pending,
                to: ActivityState::RetryWaiting,
            })
        );
    }

    #[test]
    fn retryable_failure_waits_for_scheduler() {
        let mut activity = activity();
        let current = lease();
        activity.claim(current, start()).unwrap();
        assert_eq!(
            activity.finish(&current.token(), start(), ActivityOutcome::RetryableFailure),
            Ok(ActivityState::RetryWaiting)
        );
        assert!(activity.claim(lease(), start()).is_err());
        assert_eq!(activity.attempt().unwrap().value(), 1);
    }

    #[test]
    fn attempt_overflow_leaves_activity_unmodified() {
        let mut activity = activity();
        activity.attempt = Some(Attempt::new(u32::MAX).unwrap());
        assert_eq!(
            activity.claim(lease(), start()),
            Err(ActivityError::Attempt(AttemptError::Overflow))
        );
        assert_eq!(activity.state(), ActivityState::Pending);
        assert_eq!(activity.active_lease(), None);
    }

    #[test]
    fn claim_rejects_expired_lease_without_mutation() {
        let mut activity = activity();
        assert_eq!(
            activity.claim(lease(), start() + Duration::from_secs(30)),
            Err(ActivityError::Lease(LeaseError::Expired))
        );
        assert_eq!(activity.state(), ActivityState::Pending);
        assert_eq!(activity.attempt(), None);
    }

    #[test]
    fn heartbeat_extends_the_active_lease() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        let updated = activity
            .heartbeat(
                &lease.token(),
                start() + Duration::from_secs(25),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(updated, start() + Duration::from_secs(55));
        assert_eq!(activity.active_lease().unwrap().expires_at(), updated);
    }

    #[test]
    fn stale_heartbeat_does_not_extend_lease() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        assert_eq!(
            activity.heartbeat(&LeaseToken::new(), start(), Duration::from_secs(60)),
            Err(ActivityError::Lease(LeaseError::StaleToken))
        );
        assert_eq!(
            activity.active_lease().unwrap().expires_at(),
            lease.expires_at()
        );
    }

    #[test]
    fn expired_lease_cannot_finish() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        assert_eq!(
            activity.finish(
                &lease.token(),
                start() + Duration::from_secs(30),
                ActivityOutcome::Success
            ),
            Err(ActivityError::Lease(LeaseError::Expired))
        );
        assert_eq!(activity.state(), ActivityState::Running);
    }

    #[test]
    fn recovery_before_deadline_is_rejected() {
        let mut activity = activity();
        let lease = lease();
        activity.claim(lease, start()).unwrap();
        assert_eq!(
            activity.recover_expired(start(), ExternalEffectPolicy::SafeToRetry),
            Err(ActivityError::LeaseNotExpired)
        );
        assert_eq!(activity.state(), ActivityState::Running);
        assert_eq!(activity.active_lease(), Some(lease));
    }

    #[test]
    fn expired_idempotent_work_can_be_scheduled_for_retry() {
        let mut activity = activity();
        activity.claim(lease(), start()).unwrap();
        let recovered_at = start() + Duration::from_secs(30);
        assert_eq!(
            activity.recover_expired(recovered_at, ExternalEffectPolicy::SafeToRetry),
            Ok(ExpiredLeaseAction::ScheduleRetry)
        );
        assert_eq!(activity.state(), ActivityState::RetryWaiting);
        assert_eq!(activity.active_lease(), None);
        activity.scheduler_mark_ready().unwrap();
        let next = Lease::new(LeaseToken::new(), recovered_at, Duration::from_secs(30)).unwrap();
        assert_eq!(activity.claim(next, recovered_at).unwrap().value(), 2);
    }

    #[test]
    fn expired_external_side_effect_requires_reconciliation() {
        let mut activity = activity();
        activity.claim(lease(), start()).unwrap();
        assert_eq!(
            activity.recover_expired(
                start() + Duration::from_secs(30),
                ExternalEffectPolicy::RequiresReconciliation
            ),
            Ok(ExpiredLeaseAction::MarkAmbiguous)
        );
        assert_eq!(activity.state(), ActivityState::Ambiguous);
        assert_eq!(activity.active_lease(), None);
        assert!(activity.scheduler_mark_ready().is_err());
        activity
            .resolve_ambiguous(Reconciliation::SafeToRetry)
            .unwrap();
        assert_eq!(activity.state(), ActivityState::RetryWaiting);
    }

    #[test]
    fn previous_token_is_rejected_after_retry_claim() {
        let mut activity = activity();
        let old = lease();
        activity.claim(old, start()).unwrap();
        let recovered_at = start() + Duration::from_secs(30);
        activity
            .recover_expired(recovered_at, ExternalEffectPolicy::SafeToRetry)
            .unwrap();
        activity.scheduler_mark_ready().unwrap();
        let current = Lease::new(LeaseToken::new(), recovered_at, Duration::from_secs(30)).unwrap();
        activity.claim(current, recovered_at).unwrap();
        assert_eq!(
            activity.finish(&old.token(), recovered_at, ActivityOutcome::Success),
            Err(ActivityError::Lease(LeaseError::StaleToken))
        );
        assert_eq!(activity.state(), ActivityState::Running);
        assert_eq!(
            activity.finish(&current.token(), recovered_at, ActivityOutcome::Success),
            Ok(ActivityState::Succeeded)
        );
    }
}
