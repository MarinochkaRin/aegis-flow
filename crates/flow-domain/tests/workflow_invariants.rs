use flow_domain::{Workflow, WorkflowId, WorkflowState};
use proptest::prelude::*;

const STATES: [WorkflowState; 6] = [
    WorkflowState::Created,
    WorkflowState::Running,
    WorkflowState::Waiting,
    WorkflowState::Completed,
    WorkflowState::Failed,
    WorkflowState::Cancelled,
];

proptest! {
    #[test]
    fn arbitrary_transition_sequences_never_break_terminal_invariant(
        requested in proptest::collection::vec(0usize..6, 0..100)
    ) {
        let mut workflow = Workflow::new(WorkflowId::new());

        for index in requested {
            let before = workflow.state();
            let next = STATES[index];
            let result = workflow.transition_to(next);

            if before.is_terminal() {
                prop_assert!(result.is_err());
                prop_assert_eq!(workflow.state(), before);
            } else if before.can_transition_to(next) {
                prop_assert!(result.is_ok());
                prop_assert_eq!(workflow.state(), next);
            } else {
                prop_assert!(result.is_err());
                prop_assert_eq!(workflow.state(), before);
            }
        }
    }
}
