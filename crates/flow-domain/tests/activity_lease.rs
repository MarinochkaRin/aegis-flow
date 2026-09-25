//! Public-API smoke tests for the Activity + Lease boundary.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flow_domain::{
    Activity, ActivityError, ActivityId, ActivityOutcome, ActivityState, ExpiredLeaseAction,
    ExternalEffectPolicy, Lease, LeaseError, LeaseToken, Reconciliation, WorkflowId,
};

fn start() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(100_000)
}

fn new_lease() -> Lease {
    Lease::new(LeaseToken::new(), start(), Duration::from_secs(20)).unwrap()
}

#[test]
fn late_result_does_not_complete_an_expired_attempt() {
    let mut activity = Activity::new(ActivityId::new(), WorkflowId::new());
    let lease = new_lease();
    activity.claim(lease, start()).unwrap();
    let deadline = lease.expires_at();

    assert_eq!(
        activity.finish(&lease.token(), deadline, ActivityOutcome::Success),
        Err(ActivityError::Lease(LeaseError::Expired))
    );
    assert_eq!(activity.state(), ActivityState::Running);
}

#[test]
fn ambiguous_expiry_cannot_be_retried_without_reconciliation() {
    let mut activity = Activity::new(ActivityId::new(), WorkflowId::new());
    activity.claim(new_lease(), start()).unwrap();
    assert_eq!(
        activity.recover_expired(
            start() + Duration::from_secs(20),
            ExternalEffectPolicy::RequiresReconciliation,
        ),
        Ok(ExpiredLeaseAction::MarkAmbiguous)
    );
    assert_eq!(activity.state(), ActivityState::Ambiguous);
    assert!(activity.scheduler_mark_ready().is_err());
    activity
        .resolve_ambiguous(Reconciliation::SafeToRetry)
        .unwrap();
    activity.scheduler_mark_ready().unwrap();
    let retry_at = start() + Duration::from_secs(20);
    let retry_lease = Lease::new(LeaseToken::new(), retry_at, Duration::from_secs(20)).unwrap();
    assert_eq!(activity.claim(retry_lease, retry_at).unwrap().value(), 2);
}
