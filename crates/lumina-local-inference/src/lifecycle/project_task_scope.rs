use super::{
    GenerationParameterError, GenerationParameterIssue, GenerationParameterResult,
    NativeTaskInputReference, Sha256Digest,
};

/// Explicit compilation context supplied after the caller authenticates the project,
/// parent snapshot and every referenced asset. This value does not grant execution
/// permission or replace project-store authorization.
#[derive(Debug, Clone)]
pub struct ProjectTaskCompilationScope {
    child_session_id: String,
    child_job_id: String,
    parent_session_id: String,
    parent_job_id: String,
    parent_snapshot_digest: Sha256Digest,
    allowed_inputs: Vec<NativeTaskInputReference>,
}

impl ProjectTaskCompilationScope {
    pub fn new(
        child_session_id: String,
        child_job_id: String,
        parent_session_id: String,
        parent_job_id: String,
        parent_snapshot_digest: Sha256Digest,
        allowed_inputs: Vec<NativeTaskInputReference>,
    ) -> GenerationParameterResult<Self> {
        for identity in [
            &child_session_id,
            &child_job_id,
            &parent_session_id,
            &parent_job_id,
        ] {
            if identity.is_empty() || identity.len() > 256 || identity.chars().any(char::is_control)
            {
                return Err(Self::invalid());
            }
        }
        if child_job_id == parent_job_id {
            return Err(Self::invalid());
        }
        for input in &allowed_inputs {
            if input.session_id().is_empty()
                || input.session_id().len() > 256
                || input.session_id().chars().any(char::is_control)
                || !uuid::Uuid::parse_str(input.result_id())
                    .is_ok_and(|id| id.to_string() == input.result_id())
                || input.size_bytes() == 0
                || allowed_inputs.iter().any(|other| {
                    input.session_id() == other.session_id()
                        && input.result_id() == other.result_id()
                        && input != other
                })
            {
                return Err(Self::invalid());
            }
        }
        Ok(Self {
            child_session_id,
            child_job_id,
            parent_session_id,
            parent_job_id,
            parent_snapshot_digest,
            allowed_inputs,
        })
    }

    pub fn child_session_id(&self) -> &str {
        &self.child_session_id
    }
    pub fn child_job_id(&self) -> &str {
        &self.child_job_id
    }
    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }
    pub fn parent_job_id(&self) -> &str {
        &self.parent_job_id
    }
    pub fn parent_snapshot_digest(&self) -> &Sha256Digest {
        &self.parent_snapshot_digest
    }
    pub fn allowed_inputs(&self) -> &[NativeTaskInputReference] {
        &self.allowed_inputs
    }

    pub(super) fn validate_child(&self, session: &str, job: &str) -> GenerationParameterResult<()> {
        if session != self.child_session_id || job != self.child_job_id {
            return Err(Self::invalid());
        }
        Ok(())
    }

    pub(super) fn validate_identity(
        &self,
        session: &str,
        job: &str,
    ) -> GenerationParameterResult<()> {
        if (session == self.child_session_id && job == self.child_job_id)
            || (session == self.parent_session_id && job == self.parent_job_id)
        {
            Ok(())
        } else {
            Err(Self::invalid())
        }
    }

    pub(super) fn validate_parent(
        &self,
        session: &str,
        job: &str,
        digest: &Sha256Digest,
    ) -> GenerationParameterResult<()> {
        if session != self.parent_session_id
            || job != self.parent_job_id
            || digest != &self.parent_snapshot_digest
        {
            return Err(Self::invalid());
        }
        Ok(())
    }

    pub(super) fn validate_input(
        &self,
        input: &NativeTaskInputReference,
    ) -> GenerationParameterResult<()> {
        if !self.allowed_inputs.contains(input) {
            return Err(Self::invalid());
        }
        Ok(())
    }

    fn invalid() -> GenerationParameterError {
        GenerationParameterError { issues: vec![GenerationParameterIssue {
            field: "projectScope".into(), code: "project_scope_mismatch".into(),
            message: "Task identity, parent digest or complete input reference differs from the explicit project compilation scope".into(),
        }] }
    }
}
