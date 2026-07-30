#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::mcp_platform::credential_authority::{
    CredentialAuthority, CredentialHandle, CredentialReadinessBinding, EnrollmentAuthorityReader,
};
use crate::mcp_platform::credential_runtime_gate::{
    credential_reference_digest, enrollment_backed_runtime_snapshot, CredentialRuntimeSnapshot,
    CredentialRuntimeStatus, EnrollmentBindingSource, EnrollmentFactRuntimeBinding,
};
use crate::mcp_platform::error::McpPlatformResult;
use crate::mcp_platform::manifest::{trusted_credential_enrollment_schema, Auth};
use crate::mcp_platform::repository::{
    ManagedCredentialEnrollmentRecord, SqliteMcpPlatformRepository,
};

pub(crate) enum ResolvedEnrollmentRuntimeBinding {
    Missing,
    Unavailable,
    Bound(EnrollmentFactRuntimeBinding),
}

impl ResolvedEnrollmentRuntimeBinding {
    pub(crate) fn as_binding_source(&self) -> EnrollmentBindingSource<'_> {
        match self {
            Self::Missing => EnrollmentBindingSource::Missing,
            Self::Unavailable => EnrollmentBindingSource::Unavailable,
            Self::Bound(binding) => EnrollmentBindingSource::Bound(binding),
        }
    }
}

#[async_trait]
pub(crate) trait EnrollmentRuntimeBindingResolver: Send + Sync {
    fn authority_reader(&self) -> &EnrollmentAuthorityReader;

    async fn resolve(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> McpPlatformResult<ResolvedEnrollmentRuntimeBinding>;

    async fn evaluate_managed_credential_status(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> CredentialRuntimeStatus {
        self.safe_runtime_snapshot(
            managed_mcp_id,
            current_managed_revision,
            current_manifest_digest,
            auth,
        )
        .await
        .status()
    }

    async fn safe_runtime_snapshot(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> CredentialRuntimeSnapshot {
        let resolution = self
            .resolve(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                auth,
            )
            .await
            .unwrap_or(ResolvedEnrollmentRuntimeBinding::Unavailable);
        enrollment_backed_runtime_snapshot(
            self.authority_reader(),
            auth,
            resolution.as_binding_source(),
        )
    }
}

pub(crate) struct SharedManagedCredentialStatusEvaluator {
    resolver: Mutex<Arc<dyn EnrollmentRuntimeBindingResolver>>,
}

impl SharedManagedCredentialStatusEvaluator {
    pub(crate) fn new(
        resolver: Arc<dyn EnrollmentRuntimeBindingResolver>,
    ) -> Arc<SharedManagedCredentialStatusEvaluator> {
        Arc::new(Self {
            resolver: Mutex::new(resolver),
        })
    }

    pub(crate) fn set_resolver(&self, resolver: Arc<dyn EnrollmentRuntimeBindingResolver>) {
        *self
            .resolver
            .lock()
            .expect("managed credential status evaluator resolver lock") = resolver;
    }

    pub(crate) fn resolver(&self) -> Arc<dyn EnrollmentRuntimeBindingResolver> {
        self.resolver
            .lock()
            .expect("managed credential status evaluator resolver lock")
            .clone()
    }

    pub(crate) async fn evaluate(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> CredentialRuntimeStatus {
        self.resolver()
            .evaluate_managed_credential_status(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                auth,
            )
            .await
    }

    pub(crate) async fn safe_runtime_snapshot(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> CredentialRuntimeSnapshot {
        self.resolver()
            .safe_runtime_snapshot(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                auth,
            )
            .await
    }
}

#[async_trait]
trait ManagedEnrollmentFactReader: Send + Sync {
    async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>>;
}

#[async_trait]
impl ManagedEnrollmentFactReader for SqliteMcpPlatformRepository {
    async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>> {
        SqliteMcpPlatformRepository::get_managed_credential_enrollment(self, managed_mcp_id).await
    }
}

pub(crate) struct RepositoryEnrollmentRuntimeBindingResolver {
    repository: Arc<dyn ManagedEnrollmentFactReader>,
    authority: EnrollmentAuthorityReader,
}

impl RepositoryEnrollmentRuntimeBindingResolver {
    pub(crate) fn production(
        repository: Arc<SqliteMcpPlatformRepository>,
        authority: EnrollmentAuthorityReader,
    ) -> Arc<dyn EnrollmentRuntimeBindingResolver> {
        Arc::new(Self {
            repository,
            authority,
        })
    }
}

#[async_trait]
impl EnrollmentRuntimeBindingResolver for RepositoryEnrollmentRuntimeBindingResolver {
    fn authority_reader(&self) -> &EnrollmentAuthorityReader {
        &self.authority
    }

    async fn resolve(
        &self,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> McpPlatformResult<ResolvedEnrollmentRuntimeBinding> {
        let trusted_schema_id = match auth {
            Auth::None => return Ok(ResolvedEnrollmentRuntimeBinding::Missing),
            _ => match trusted_credential_enrollment_schema(auth) {
                Ok(schema) => schema.schema_id,
                Err(_) => return Ok(ResolvedEnrollmentRuntimeBinding::Unavailable),
            },
        };
        let record = self
            .repository
            .get_managed_credential_enrollment(managed_mcp_id)
            .await?;
        Ok(record
            .as_ref()
            .map(|record| {
                ResolvedEnrollmentRuntimeBinding::Bound(EnrollmentFactRuntimeBinding::new(
                    managed_mcp_id,
                    current_managed_revision,
                    current_manifest_digest,
                    trusted_schema_id,
                    &record.manifest_digest,
                    &record.auth_schema_id,
                    &record.credential_reference,
                    &record.reference_digest,
                    &record.authority.provider_id,
                    &record.authority.writer_mode,
                    record.authority_evidence_digest.clone().unwrap_or_default(),
                    record.revision,
                ))
            })
            .unwrap_or(ResolvedEnrollmentRuntimeBinding::Missing))
    }
}

pub(crate) struct FailClosedEnrollmentRuntimeBindingResolver {
    authority: EnrollmentAuthorityReader,
}

impl FailClosedEnrollmentRuntimeBindingResolver {
    pub(crate) fn new() -> Self {
        Self {
            authority: CredentialAuthority::production_default().enrollment_reader(),
        }
    }
}

#[async_trait]
impl EnrollmentRuntimeBindingResolver for FailClosedEnrollmentRuntimeBindingResolver {
    fn authority_reader(&self) -> &EnrollmentAuthorityReader {
        &self.authority
    }

    async fn resolve(
        &self,
        _managed_mcp_id: &str,
        _current_managed_revision: i64,
        _current_manifest_digest: &str,
        _auth: &Auth,
    ) -> McpPlatformResult<ResolvedEnrollmentRuntimeBinding> {
        Ok(ResolvedEnrollmentRuntimeBinding::Unavailable)
    }
}

#[cfg(test)]
pub(crate) struct StaticEnrollmentRuntimeBindingResolver {
    authority: EnrollmentAuthorityReader,
    resolution: ResolvedEnrollmentRuntimeBinding,
}

#[cfg(test)]
impl StaticEnrollmentRuntimeBindingResolver {
    pub(crate) fn missing() -> Arc<dyn EnrollmentRuntimeBindingResolver> {
        Arc::new(Self {
            authority: CredentialAuthority::in_memory_for_testing_default_time()
                .enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Missing,
        })
    }

    pub(crate) fn unavailable() -> Arc<dyn EnrollmentRuntimeBindingResolver> {
        Arc::new(Self {
            authority: CredentialAuthority::in_memory_for_testing_default_time()
                .enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Unavailable,
        })
    }

    pub(crate) fn ready(
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> Arc<dyn EnrollmentRuntimeBindingResolver> {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("enr.ready").expect("test handle");
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle, b"secret")
            .expect("seed authority secret");
        let authority_evidence_digest = authority
            .readiness_snapshot(
                &handle,
                &CredentialReadinessBinding::auth_requirement_probe(
                    "managed_enrollment_runtime",
                    managed_mcp_id,
                ),
            )
            .expect("seed authority snapshot")
            .evidence_digest()
            .to_string();
        let schema = trusted_credential_enrollment_schema(auth).expect("supported auth");
        let authority_summary = authority.enrollment_reader().authority_summary();
        Arc::new(Self {
            authority: authority.enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Bound(EnrollmentFactRuntimeBinding::new(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                schema.schema_id,
                current_manifest_digest,
                schema.schema_id,
                handle.as_str(),
                credential_reference_digest(handle.as_str()),
                authority.provider_id(),
                authority_summary.writer_mode,
                authority_evidence_digest,
                current_managed_revision,
            )),
        })
    }
}

#[cfg(test)]
pub(crate) struct RecordingEnrollmentRuntimeBindingResolver {
    authority: EnrollmentAuthorityReader,
    resolution: ResolvedEnrollmentRuntimeBinding,
    calls: AtomicUsize,
}

#[cfg(test)]
impl RecordingEnrollmentRuntimeBindingResolver {
    pub(crate) fn missing() -> Arc<Self> {
        Arc::new(Self {
            authority: CredentialAuthority::in_memory_for_testing_default_time()
                .enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Missing,
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            authority: CredentialAuthority::in_memory_for_testing_default_time()
                .enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Unavailable,
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn ready(
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        auth: &Auth,
    ) -> Arc<Self> {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("enr.recording").expect("test handle");
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle, b"secret")
            .expect("seed authority secret");
        let authority_evidence_digest = authority
            .readiness_snapshot(
                &handle,
                &CredentialReadinessBinding::auth_requirement_probe(
                    "managed_enrollment_runtime",
                    managed_mcp_id,
                ),
            )
            .expect("seed authority snapshot")
            .evidence_digest()
            .to_string();
        let schema = trusted_credential_enrollment_schema(auth).expect("supported auth");
        let authority_summary = authority.enrollment_reader().authority_summary();
        Arc::new(Self {
            authority: authority.enrollment_reader(),
            resolution: ResolvedEnrollmentRuntimeBinding::Bound(EnrollmentFactRuntimeBinding::new(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                schema.schema_id,
                current_manifest_digest,
                schema.schema_id,
                handle.as_str(),
                credential_reference_digest(handle.as_str()),
                authority.provider_id(),
                authority_summary.writer_mode,
                authority_evidence_digest,
                current_managed_revision,
            )),
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[async_trait]
impl EnrollmentRuntimeBindingResolver for RecordingEnrollmentRuntimeBindingResolver {
    fn authority_reader(&self) -> &EnrollmentAuthorityReader {
        &self.authority
    }

    async fn resolve(
        &self,
        _managed_mcp_id: &str,
        _current_managed_revision: i64,
        _current_manifest_digest: &str,
        _auth: &Auth,
    ) -> McpPlatformResult<ResolvedEnrollmentRuntimeBinding> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(match &self.resolution {
            ResolvedEnrollmentRuntimeBinding::Missing => ResolvedEnrollmentRuntimeBinding::Missing,
            ResolvedEnrollmentRuntimeBinding::Unavailable => {
                ResolvedEnrollmentRuntimeBinding::Unavailable
            }
            ResolvedEnrollmentRuntimeBinding::Bound(binding) => {
                ResolvedEnrollmentRuntimeBinding::Bound(binding.clone())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::credential_runtime_gate::{
        enrollment_backed_runtime_snapshot, CredentialRuntimeStatus,
    };
    use crate::mcp_platform::repository::ManagedCredentialEnrollmentAuthoritySummary;

    struct ReaderOnlyRepository(Option<ManagedCredentialEnrollmentRecord>);

    #[async_trait]
    impl ManagedEnrollmentFactReader for ReaderOnlyRepository {
        async fn get_managed_credential_enrollment(
            &self,
            _managed_mcp_id: &str,
        ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>> {
            Ok(self.0.clone())
        }
    }

    fn auth() -> Auth {
        Auth::Environment {
            environment_key: "AUTH_TOKEN".to_string(),
            credential_name: "enr.status".to_string(),
        }
    }

    fn ready_components() -> (
        CredentialAuthority,
        String,
        String,
        ManagedCredentialEnrollmentAuthoritySummary,
    ) {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("enr.status").unwrap();
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle, b"secret")
            .unwrap();
        let snapshot = authority
            .readiness_snapshot(
                &handle,
                &CredentialReadinessBinding::auth_requirement_probe(
                    "managed_enrollment_runtime",
                    "managed_status",
                ),
            )
            .unwrap();
        (
            authority,
            handle.as_str().to_string(),
            snapshot.evidence_digest().to_string(),
            ManagedCredentialEnrollmentAuthoritySummary {
                provider_id: snapshot.provider_id().to_string(),
                writer_mode: "best_effort_single_process".to_string(),
            },
        )
    }

    fn binding_with(
        current_manifest_digest: &str,
        enrolled_manifest_digest: &str,
        trusted_schema_id: &str,
        enrolled_schema_id: &str,
        authority_provider_id: &str,
        authority_writer_mode: &str,
        authority_evidence_digest: &str,
        current_managed_revision: i64,
        enrollment_revision: i64,
    ) -> EnrollmentFactRuntimeBinding {
        EnrollmentFactRuntimeBinding::new(
            "managed_status",
            current_managed_revision,
            current_manifest_digest,
            trusted_schema_id,
            enrolled_manifest_digest,
            enrolled_schema_id,
            "enr.status",
            credential_reference_digest("enr.status"),
            authority_provider_id,
            authority_writer_mode,
            authority_evidence_digest,
            enrollment_revision,
        )
    }

    #[tokio::test]
    async fn repository_resolver_uses_current_trusted_schema_from_auth() {
        let (authority, handle, evidence_digest, authority_summary) = ready_components();
        let resolver = RepositoryEnrollmentRuntimeBindingResolver {
            repository: Arc::new(ReaderOnlyRepository(Some(
                ManagedCredentialEnrollmentRecord {
                    managed_mcp_id: "managed_status".to_string(),
                    manifest_digest: "m".repeat(64),
                    auth_schema_id: "static_env_secret".to_string(),
                    credential_reference: handle.clone(),
                    reference_digest: credential_reference_digest(&handle),
                    authority: authority_summary,
                    authority_evidence_digest: Some(evidence_digest),
                    revision: 11,
                    updated_at_ms: 10,
                },
            ))),
            authority: authority.enrollment_reader(),
        };
        let resolved = resolver
            .resolve("managed_status", 11, &"m".repeat(64), &auth())
            .await
            .unwrap();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth(),
                resolved.as_binding_source(),
            )
            .status(),
            CredentialRuntimeStatus::Ready
        );
    }

    #[tokio::test]
    async fn repository_resolver_fails_closed_for_none_and_unsupported_auth() {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let resolver = RepositoryEnrollmentRuntimeBindingResolver {
            repository: Arc::new(ReaderOnlyRepository(None)),
            authority: authority.enrollment_reader(),
        };

        assert!(matches!(
            resolver
                .resolve("managed_status", 11, &"m".repeat(64), &Auth::None)
                .await
                .unwrap(),
            ResolvedEnrollmentRuntimeBinding::Missing
        ));
        assert!(matches!(
            resolver
                .resolve(
                    "managed_status",
                    11,
                    &"m".repeat(64),
                    &Auth::Oauth2 {
                        authorization_url: "https://example.com/authorize".to_string(),
                        token_url: "https://example.com/token".to_string(),
                        client_registration:
                            crate::mcp_platform::manifest::OAuthClientRegistration::Dynamic,
                        scopes: vec!["scope".to_string()],
                    },
                )
                .await
                .unwrap(),
            ResolvedEnrollmentRuntimeBinding::Unavailable
        ));
    }

    #[test]
    fn ready_binding_evaluates_ready() {
        let (authority, _handle, evidence_digest, authority_summary) = ready_components();
        let schema = trusted_credential_enrollment_schema(&auth()).unwrap();
        let binding = binding_with(
            &"m".repeat(64),
            &"m".repeat(64),
            schema.schema_id,
            schema.schema_id,
            &authority_summary.provider_id,
            &authority_summary.writer_mode,
            &evidence_digest,
            4,
            4,
        );
        let status = enrollment_backed_runtime_snapshot(
            &authority.enrollment_reader(),
            &auth(),
            EnrollmentBindingSource::Bound(&binding),
        )
        .status();
        assert_eq!(status, CredentialRuntimeStatus::Ready);
    }

    #[test]
    fn revision_drift_requires_reregistration() {
        let (authority, _handle, evidence_digest, authority_summary) = ready_components();
        let schema = trusted_credential_enrollment_schema(&auth()).unwrap();
        let binding = binding_with(
            &"m".repeat(64),
            &"m".repeat(64),
            schema.schema_id,
            schema.schema_id,
            &authority_summary.provider_id,
            &authority_summary.writer_mode,
            &evidence_digest,
            5,
            4,
        );
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth(),
                EnrollmentBindingSource::Bound(&binding),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );
    }

    #[test]
    fn schema_drift_requires_reregistration() {
        let (authority, _handle, evidence_digest, authority_summary) = ready_components();
        let schema = trusted_credential_enrollment_schema(&auth()).unwrap();
        let binding = binding_with(
            &"m".repeat(64),
            &"m".repeat(64),
            schema.schema_id,
            "wrong_schema",
            &authority_summary.provider_id,
            &authority_summary.writer_mode,
            &evidence_digest,
            4,
            4,
        );
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth(),
                EnrollmentBindingSource::Bound(&binding),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );
    }

    #[test]
    fn provider_mismatch_is_trusted_state_conflict() {
        let (authority, _handle, evidence_digest, authority_summary) = ready_components();
        let schema = trusted_credential_enrollment_schema(&auth()).unwrap();
        let binding = binding_with(
            &"m".repeat(64),
            &"m".repeat(64),
            schema.schema_id,
            schema.schema_id,
            "other-provider",
            &authority_summary.writer_mode,
            &evidence_digest,
            4,
            4,
        );
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth(),
                EnrollmentBindingSource::Bound(&binding),
            )
            .status(),
            CredentialRuntimeStatus::TrustedStateConflict
        );
    }

    #[test]
    fn unavailable_authority_fails_closed() {
        let authority = CredentialAuthority::unavailable_for_testing_default_time();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth(),
                EnrollmentBindingSource::Missing,
            )
            .status(),
            CredentialRuntimeStatus::TemporarilyUnavailable
        );
    }

    #[tokio::test]
    async fn fail_closed_resolver_returns_unavailable_without_leaking_legacy_fallback() {
        let resolver = FailClosedEnrollmentRuntimeBindingResolver::new();
        assert!(matches!(
            resolver
                .resolve("managed_status", 1, &"m".repeat(64), &auth())
                .await
                .unwrap(),
            ResolvedEnrollmentRuntimeBinding::Unavailable
        ));
    }
}

#[cfg(test)]
#[async_trait]
impl EnrollmentRuntimeBindingResolver for StaticEnrollmentRuntimeBindingResolver {
    fn authority_reader(&self) -> &EnrollmentAuthorityReader {
        &self.authority
    }

    async fn resolve(
        &self,
        _managed_mcp_id: &str,
        _current_managed_revision: i64,
        _current_manifest_digest: &str,
        _auth: &Auth,
    ) -> McpPlatformResult<ResolvedEnrollmentRuntimeBinding> {
        Ok(match &self.resolution {
            ResolvedEnrollmentRuntimeBinding::Missing => ResolvedEnrollmentRuntimeBinding::Missing,
            ResolvedEnrollmentRuntimeBinding::Unavailable => {
                ResolvedEnrollmentRuntimeBinding::Unavailable
            }
            ResolvedEnrollmentRuntimeBinding::Bound(binding) => {
                ResolvedEnrollmentRuntimeBinding::Bound(binding.clone())
            }
        })
    }
}
