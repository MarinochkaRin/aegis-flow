#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowState {
    Created,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Created, Self::Running)
                | (Self::Created, Self::Cancelled)
                | (Self::Running, Self::Waiting)
                | (Self::Running, Self::Completed)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
                | (Self::Waiting, Self::Running)
                | (Self::Waiting, Self::Failed)
                | (Self::Waiting, Self::Cancelled)
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::WorkflowState;

    #[test]
    fn created_workflow_can_start() {
        assert!(WorkflowState::Created.can_transition_to(WorkflowState::Running));
    }

    #[test]
    fn running_workflow_can_wait() {
        assert!(WorkflowState::Running.can_transition_to(WorkflowState::Waiting));
    }

    #[test]
    fn waiting_workflow_can_resume() {
        assert!(WorkflowState::Waiting.can_transition_to(WorkflowState::Running));
    }

    #[test]
    fn completed_workflow_is_terminal() {
        assert!(WorkflowState::Completed.is_terminal());

        assert!(!WorkflowState::Completed.can_transition_to(WorkflowState::Running));
    }
}
