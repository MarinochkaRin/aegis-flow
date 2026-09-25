#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Pending,
    Running,
    RetryWaiting,
    Succeeded,
    Failed,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    Success,
    RetryableFailure,
    PermanentFailure,
    Ambiguous,
}

impl ActivityState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Running)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::RetryWaiting)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Ambiguous)
                | (Self::RetryWaiting, Self::Pending)
                | (Self::Ambiguous, Self::Succeeded)
                | (Self::Ambiguous, Self::RetryWaiting)
                | (Self::Ambiguous, Self::Failed)
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityState;

    #[test]
    fn pending_activity_can_be_claimed() {
        assert!(ActivityState::Pending.can_transition_to(ActivityState::Running));
    }

    #[test]
    fn running_activity_can_become_ambiguous() {
        assert!(ActivityState::Running.can_transition_to(ActivityState::Ambiguous));
    }

    #[test]
    fn ambiguous_activity_can_be_retried() {
        assert!(ActivityState::Ambiguous.can_transition_to(ActivityState::RetryWaiting));
    }

    #[test]
    fn succeeded_activity_is_terminal() {
        assert!(ActivityState::Succeeded.is_terminal());

        assert!(!ActivityState::Succeeded.can_transition_to(ActivityState::Running));
    }
}
