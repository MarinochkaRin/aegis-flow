use std::fmt;
use uuid::Uuid;

/// Identifies one workflow independently of the worker executing it.
///
/// A workflow ID and an activity ID are not interchangeable:
///
/// ```compile_fail
/// use flow_domain::{ActivityId, WorkflowId};
/// fn find_workflow(_id: WorkflowId) {}
/// find_workflow(ActivityId::new());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkflowId(Uuid);

impl WorkflowId {
    #[expect(
        clippy::new_without_default,
        reason = "Workflow IDs must be generated explicitly"
    )]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifies one activity; using a separate type prevents ID mix-ups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActivityId(Uuid);

impl ActivityId {
    #[expect(
        clippy::new_without_default,
        reason = "Activity IDs must be generated explicitly"
    )]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for ActivityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifies a particular lease. A later storage layer must compare this
/// token in conditional updates; the token alone cannot prevent external
/// side effects by stale workers.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseToken(Uuid);

impl LeaseToken {
    #[expect(
        clippy::new_without_default,
        reason = "Lease tokens must be generated explicitly for each lease"
    )]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

// Avoid accidentally dumping lease tokens into diagnostic logs.
impl fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LeaseToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityId, LeaseToken, WorkflowId};

    #[test]
    fn workflow_id_round_trip() {
        let id = WorkflowId::new();
        assert_eq!(WorkflowId::from_uuid(id.as_uuid()), id);
    }

    #[test]
    fn activity_id_round_trip() {
        let id = ActivityId::new();
        assert_eq!(ActivityId::from_uuid(id.as_uuid()), id);
    }

    #[test]
    fn identifiers_are_copyable_values() {
        let original = WorkflowId::new();
        let copied = original;
        assert_eq!(original, copied);
    }

    #[test]
    fn lease_token_debug_output_is_redacted() {
        let token = LeaseToken::new();

        assert_eq!(format!("{token:?}"), "LeaseToken(<redacted>)");

        assert_eq!(LeaseToken::from_uuid(token.as_uuid()), token);
    }
}
