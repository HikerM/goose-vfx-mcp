use super::intake_inspection::{
    classify_failure, ClassifiedInspectionFailure, InspectionBindingTuple, InspectionConflictCode,
    InspectionFactCode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteObservation {
    pub(crate) failure: ClassifiedInspectionFailure,
    pub(crate) conflict_codes: Vec<InspectionConflictCode>,
    pub(crate) fact_codes: Vec<InspectionFactCode>,
}

pub(crate) trait InspectionNetworkOwner {
    fn observe_remote_boundary(&self, binding: &InspectionBindingTuple) -> RemoteObservation;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NoNetworkInspectionOwner;

impl InspectionNetworkOwner for NoNetworkInspectionOwner {
    fn observe_remote_boundary(&self, _binding: &InspectionBindingTuple) -> RemoteObservation {
        let conflict_codes = vec![InspectionConflictCode::NetworkExecutionUnavailable];
        RemoteObservation {
            failure: classify_failure(&conflict_codes),
            conflict_codes,
            fact_codes: vec![
                InspectionFactCode::CandidateMetadataObserved,
                InspectionFactCode::RemoteExecutionUnavailable,
            ],
        }
    }
}

pub(crate) fn no_network_remote_observation(binding: &InspectionBindingTuple) -> RemoteObservation {
    NoNetworkInspectionOwner.observe_remote_boundary(binding)
}
