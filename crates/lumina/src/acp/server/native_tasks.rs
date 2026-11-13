use super::*;

#[cfg(feature = "local-inference")]
use crate::providers::local_inference::lifecycle as domain;

#[cfg(feature = "local-inference")]
mod mapping;

#[cfg(feature = "local-inference")]
impl LuminaAcpAgent {
    async fn catalog_session(
        &self,
        session_id: &str,
    ) -> Result<Arc<Agent>, agent_client_protocol::Error> {
        let sessions = self.sessions.lock().await;
        let closed = self.closed_session_ids.lock().await;
        sessions
            .get(session_id)
            .filter(|_| !closed.contains(session_id))
            .map(|s| s.agent.clone())
            .ok_or_else(|| {
                agent_client_protocol::Error::resource_not_found(None)
                    .data("native_task_session_not_active")
            })
    }

    async fn catalog_for_session<T, F>(
        &self,
        session_id: &str,
        load: F,
    ) -> Result<T, agent_client_protocol::Error>
    where
        T: Send + 'static,
        F: FnOnce(&domain::LifecycleRepository) -> anyhow::Result<T> + Send + 'static,
    {
        let expected = self.catalog_session(session_id).await?;
        let service = self.local_installations.service().await.internal_err()?;
        service.wait_ready().await.internal_err()?;
        let repo = Arc::clone(service.repository());
        let result = tokio::task::spawn_blocking(move || load(&repo))
            .await
            .internal_err()?;
        let current = self.catalog_session(session_id).await?;
        if !Arc::ptr_eq(&current, &expected) {
            return Err(agent_client_protocol::Error::resource_not_found(None)
                .data("native_task_session_not_active"));
        }
        result.internal_err()
    }
}

impl LuminaAcpAgent {
    pub(super) async fn on_native_task_list(
        &self,
        req: NativeTaskListRequest,
    ) -> Result<NativeTaskListResponse, agent_client_protocol::Error> {
        #[cfg(feature = "local-inference")]
        {
            self.catalog_for_session(&req.session_id, move |repo| {
                let catalog = domain::NativeTaskCatalog::active(repo)?;
                let tasks = catalog
                    .list(mapping::core_intent(req.intent))
                    .into_iter()
                    .map(mapping::description)
                    .collect::<anyhow::Result<_>>()?;
                Ok(NativeTaskListResponse { tasks })
            })
            .await
        }
        #[cfg(not(feature = "local-inference"))]
        {
            let _ = req;
            Err(super::local_inference::local_inference_unavailable())
        }
    }

    pub(super) async fn on_native_task_describe(
        &self,
        req: NativeTaskDescribeRequest,
    ) -> Result<NativeTaskDescribeResponse, agent_client_protocol::Error> {
        #[cfg(feature = "local-inference")]
        {
            self.catalog_for_session(&req.session_id, move |repo| {
                let identity = mapping::core_identity(req.identity);
                let Some(installed) = repo.installed_version(&identity.version_id)? else {
                    return Ok(NativeTaskDescribeResponse::NotFound);
                };
                if installed.manifest.installation_id != identity.installation_id
                    || installed.manifest_digest.as_str() != identity.manifest_digest
                {
                    return Ok(NativeTaskDescribeResponse::NotFound);
                }
                let catalog = domain::NativeTaskCatalog::for_version(
                    repo,
                    &identity.installation_id,
                    &identity.version_id,
                )?;
                match catalog.describe(&identity) {
                    Ok(task) => Ok(NativeTaskDescribeResponse::Found {
                        task: Box::new(mapping::description(task)?),
                    }),
                    Err(_) => Ok(NativeTaskDescribeResponse::NotFound),
                }
            })
            .await
        }
        #[cfg(not(feature = "local-inference"))]
        {
            let _ = req;
            Err(super::local_inference::local_inference_unavailable())
        }
    }
}

#[cfg(test)]
mod tests;
