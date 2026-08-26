use super::LuminaAcpAgent;
use crate::acp::custom_requests::*;
use crate::project::{CreateWorkItem, ProjectService, RunCompletion};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;

fn project_error(code: &'static str) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(code)
}

fn project_wire<T, U>(value: T) -> Result<U, agent_client_protocol::Error>
where
    T: Serialize,
    U: DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|_| agent_client_protocol::Error::internal_error().data("project_wire_failed"))
}

impl LuminaAcpAgent {
    fn project_service(&self) -> ProjectService {
        ProjectService::new(self.session_manager.storage().clone())
    }

    pub(super) async fn on_project_open(
        &self,
        req: ProjectOpenRequest,
    ) -> Result<ProjectOpenResponse, agent_client_protocol::Error> {
        let project = self
            .project_service()
            .open_project(Path::new(&req.root_path), req.name.as_deref())
            .await
            .map_err(|_| project_error("project_open_failed"))?;
        Ok(ProjectOpenResponse {
            project: project_wire(project)?,
        })
    }

    pub(super) async fn on_project_list(
        &self,
        req: ProjectListRequest,
    ) -> Result<ProjectListResponse, agent_client_protocol::Error> {
        let projects = self
            .project_service()
            .list_projects(req.include_archived)
            .await
            .map_err(|_| project_error("project_list_failed"))?;
        Ok(ProjectListResponse {
            projects: project_wire(projects)?,
        })
    }

    pub(super) async fn on_project_snapshot(
        &self,
        req: ProjectSnapshotRequest,
    ) -> Result<ProjectSnapshotResponse, agent_client_protocol::Error> {
        let snapshot = self
            .project_service()
            .snapshot(&req.project_id)
            .await
            .map_err(|_| project_error("project_snapshot_failed"))?;
        Ok(ProjectSnapshotResponse {
            snapshot: project_wire(snapshot)?,
        })
    }

    pub(super) async fn on_project_work_item_create(
        &self,
        req: ProjectWorkItemCreateRequest,
    ) -> Result<ProjectWorkItemResponse, agent_client_protocol::Error> {
        let work_item = self
            .project_service()
            .create_work_item(CreateWorkItem {
                project_id: req.project_id,
                title: req.title,
                objective: req.objective,
                acceptance_criteria: req.acceptance_criteria,
            })
            .await
            .map_err(|_| project_error("project_work_item_create_failed"))?;
        Ok(ProjectWorkItemResponse {
            work_item: project_wire(work_item)?,
        })
    }

    pub(super) async fn on_project_work_item_complete(
        &self,
        req: ProjectWorkItemCompleteRequest,
    ) -> Result<ProjectWorkItemResponse, agent_client_protocol::Error> {
        let work_item = self
            .project_service()
            .complete_work_item(&req.work_item_id)
            .await
            .map_err(|_| project_error("project_work_item_complete_failed"))?;
        Ok(ProjectWorkItemResponse {
            work_item: project_wire(work_item)?,
        })
    }

    pub(super) async fn on_project_run_start(
        &self,
        req: ProjectRunStartRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        let run = self
            .project_service()
            .start_run(&req.work_item_id, req.session_id.as_deref())
            .await
            .map_err(|_| project_error("project_run_start_failed"))?;
        Ok(ProjectRunResponse {
            run: project_wire(run)?,
        })
    }

    pub(super) async fn on_project_run_phase(
        &self,
        req: ProjectRunPhaseRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        let phase = project_wire(req.phase)?;
        let run = self
            .project_service()
            .update_run_phase(&req.run_id, phase)
            .await
            .map_err(|_| project_error("project_run_phase_failed"))?;
        Ok(ProjectRunResponse {
            run: project_wire(run)?,
        })
    }

    pub(super) async fn on_project_run_finish(
        &self,
        req: ProjectRunFinishRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        let run = self
            .project_service()
            .finish_run(
                &req.run_id,
                RunCompletion {
                    status: project_wire(req.status)?,
                    error_code: req.error_code,
                    error_message: req.error_message,
                },
            )
            .await
            .map_err(|_| project_error("project_run_finish_failed"))?;
        Ok(ProjectRunResponse {
            run: project_wire(run)?,
        })
    }

    pub(super) async fn on_project_checkpoint_create(
        &self,
        req: ProjectCheckpointCreateRequest,
    ) -> Result<ProjectCheckpointResponse, agent_client_protocol::Error> {
        let checkpoint = self
            .project_service()
            .create_checkpoint(
                &req.run_id,
                project_wire(req.phase)?,
                req.state,
                req.safe_to_resume,
            )
            .await
            .map_err(|_| project_error("project_checkpoint_create_failed"))?;
        Ok(ProjectCheckpointResponse {
            checkpoint: project_wire(checkpoint)?,
        })
    }

    pub(super) async fn on_project_change_set_get(
        &self,
        req: ProjectChangeSetGetRequest,
    ) -> Result<ProjectChangeSetResponse, agent_client_protocol::Error> {
        let change_set = self
            .project_service()
            .ensure_change_set(&req.run_id, req.base_revision.as_deref())
            .await
            .map_err(|_| project_error("project_change_set_get_failed"))?;
        Ok(ProjectChangeSetResponse {
            change_set: project_wire(change_set)?,
        })
    }
}
