use sqlx::Row;

use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::{
    ManagedCredentialEnrollmentAuthoritySummary, ManagedCredentialEnrollmentRecord,
    SaveManagedCredentialEnrollment,
};

use super::{decode, encode, map_sqlx, not_found, SqliteMcpPlatformRepository};

impl SqliteMcpPlatformRepository {
    pub(crate) async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            r#"SELECT managed_mcp_id,manifest_digest,auth_schema_id,credential_reference,
                      reference_digest,authority_json,authority_evidence_digest,revision,updated_at_ms
               FROM managed_credential_enrollments WHERE managed_mcp_id=?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.as_ref().map(decode_enrollment_row).transpose()
    }

    pub(crate) async fn save_managed_credential_enrollment(
        &self,
        input: SaveManagedCredentialEnrollment<'_>,
    ) -> McpPlatformResult<ManagedCredentialEnrollmentRecord> {
        let mut tx = self.begin_immediate().await?;
        sqlx::query_scalar::<_, String>(
            "SELECT managed_mcp_id FROM managed_mcps WHERE managed_mcp_id=?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        sqlx::query_scalar::<_, String>(
            "SELECT manifest_digest FROM manifest_blobs WHERE manifest_digest=?",
        )
        .bind(input.manifest_digest)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;

        let authority_json = encode(input.authority)?;
        let existing = sqlx::query(
            r#"SELECT managed_mcp_id,manifest_digest,auth_schema_id,credential_reference,
                      reference_digest,authority_json,authority_evidence_digest,revision,updated_at_ms
               FROM managed_credential_enrollments WHERE managed_mcp_id=?"#,
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        match existing {
            Some(row) => {
                let current = decode_enrollment_row(&row)?;
                if current.manifest_digest == input.manifest_digest
                    && current.auth_schema_id == input.auth_schema_id
                    && current.credential_reference == input.credential_reference
                    && current.reference_digest == input.reference_digest
                    && current.authority == *input.authority
                    && current.authority_evidence_digest.as_deref()
                        == Some(input.authority_evidence_digest)
                {
                    tx.commit().await.map_err(map_sqlx)?;
                    return Ok(current);
                }
                sqlx::query(
                    r#"UPDATE managed_credential_enrollments
                       SET manifest_digest=?,auth_schema_id=?,credential_reference=?,
                           reference_digest=?,authority_json=?,authority_evidence_digest=?,
                           revision=revision+1,updated_at_ms=?
                       WHERE managed_mcp_id=?"#,
                )
                .bind(input.manifest_digest)
                .bind(input.auth_schema_id)
                .bind(input.credential_reference)
                .bind(input.reference_digest)
                .bind(&authority_json)
                .bind(input.authority_evidence_digest)
                .bind(input.now_ms)
                .bind(input.managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            }
            None => {
                sqlx::query(
                    r#"INSERT INTO managed_credential_enrollments(
                           managed_mcp_id,manifest_digest,auth_schema_id,credential_reference,
                           reference_digest,authority_json,authority_evidence_digest,
                           revision,updated_at_ms
                       ) VALUES (?,?,?,?,?,?,?,1,?)"#,
                )
                .bind(input.managed_mcp_id)
                .bind(input.manifest_digest)
                .bind(input.auth_schema_id)
                .bind(input.credential_reference)
                .bind(input.reference_digest)
                .bind(&authority_json)
                .bind(input.authority_evidence_digest)
                .bind(input.now_ms)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            }
        }
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_credential_enrollment(input.managed_mcp_id)
            .await?
            .ok_or_else(|| {
                crate::mcp_platform::McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityError,
                    "managed credential enrollment persistence is inconsistent",
                )
            })
    }
}

fn decode_enrollment_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ManagedCredentialEnrollmentRecord> {
    Ok(ManagedCredentialEnrollmentRecord {
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        manifest_digest: row.try_get("manifest_digest").map_err(map_sqlx)?,
        auth_schema_id: row.try_get("auth_schema_id").map_err(map_sqlx)?,
        credential_reference: row.try_get("credential_reference").map_err(map_sqlx)?,
        reference_digest: row.try_get("reference_digest").map_err(map_sqlx)?,
        authority: decode::<ManagedCredentialEnrollmentAuthoritySummary>(
            &row.try_get::<String, _>("authority_json")
                .map_err(map_sqlx)?,
        )?,
        authority_evidence_digest: row.try_get("authority_evidence_digest").map_err(map_sqlx)?,
        revision: row.try_get("revision").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::mcp_platform::repository::{
        InMemoryIntegritySigner, ManifestRecord, RegisterManagedMcp,
    };
    use crate::mcp_platform::task::AdapterEvidence;
    use crate::mcp_platform::{parse_manifest, ManifestProof, TrustTier};

    async fn repository() -> SqliteMcpPlatformRepository {
        SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x72; 32])),
        )
        .await
        .unwrap()
    }

    async fn managed_fixture(repository: &SqliteMcpPlatformRepository) -> String {
        let verified = parse_manifest(
            &serde_json::to_vec(&json!({
                "schema_version": 1,
                "id": "managed-enrollment-fixture",
                "version": "1.0.0",
                "name": "Fixture",
                "description": "fixture",
                "publisher": {"id": "local", "name": "Local"},
                "license": {"spdx": "MIT"},
                "capabilities": ["tools"],
                "permissions": [],
                "distribution": {"type": "remote_http"},
                "transport": {"type": "streamable_http", "url": "https://example.com/mcp"},
                "auth": {
                    "type": "environment",
                    "environment_key": "AUTH_TOKEN",
                    "credential_name": "legacy-token"
                },
                "health_check": {"type": "mcp_initialize", "timeout_seconds": 30},
                "owned_files": [],
                "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let adapter_evidence = AdapterEvidence {
            adapter_id: "test-adapter".to_string(),
            adapter_version: "1.0.0".to_string(),
            compatible_for_recovery: true,
            resume_safe: true,
        };
        repository
            .register_managed_mcp(RegisterManagedMcp {
                managed_mcp_id: "managed_enrollment",
                mcp_id: "managed-enrollment-fixture",
                installation_scope: "user",
                distribution_adapter: "remote_http",
                manifest_digest: &digest,
                version: "1.0.0",
                task_id: "task_fixture",
                adapter_evidence: &adapter_evidence,
                now_ms: 1,
            })
            .await
            .unwrap();
        digest
    }

    #[tokio::test]
    async fn persistence_stores_only_safe_facts_and_rotation_updates_reference() {
        let repository = repository().await;
        let digest = managed_fixture(&repository).await;
        let secret = "super-secret-value";
        let first_digest = "1".repeat(64);
        let first_authority_evidence_digest = "a".repeat(64);
        let first = repository
            .save_managed_credential_enrollment(SaveManagedCredentialEnrollment {
                managed_mcp_id: "managed_enrollment",
                manifest_digest: &digest,
                auth_schema_id: "static_env_secret",
                credential_reference: "enr.first",
                reference_digest: &first_digest,
                authority: &ManagedCredentialEnrollmentAuthoritySummary {
                    provider_id: "in-memory-test".to_string(),
                    writer_mode: "best_effort_single_process".to_string(),
                },
                authority_evidence_digest: &first_authority_evidence_digest,
                now_ms: 10,
            })
            .await
            .unwrap();
        assert_eq!(first.revision, 1);
        let raw_row: String = sqlx::query_scalar(
            r#"SELECT managed_mcp_id || '|' || manifest_digest || '|' || auth_schema_id || '|' ||
                      credential_reference || '|' || reference_digest || '|' || authority_json ||
                      '|' || authority_evidence_digest
               FROM managed_credential_enrollments WHERE managed_mcp_id='managed_enrollment'"#,
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert!(!raw_row.contains(secret));
        assert!(!raw_row.contains("payload"));
        assert!(!raw_row.contains("witness"));
        assert!(!raw_row.contains("anchor"));
        assert!(!raw_row.contains("keyring"));

        let second_digest = "2".repeat(64);
        let second_authority_evidence_digest = "b".repeat(64);
        let second = repository
            .save_managed_credential_enrollment(SaveManagedCredentialEnrollment {
                managed_mcp_id: "managed_enrollment",
                manifest_digest: &digest,
                auth_schema_id: "static_env_secret",
                credential_reference: "enr.second",
                reference_digest: &second_digest,
                authority: &ManagedCredentialEnrollmentAuthoritySummary {
                    provider_id: "in-memory-test".to_string(),
                    writer_mode: "best_effort_single_process".to_string(),
                },
                authority_evidence_digest: &second_authority_evidence_digest,
                now_ms: 20,
            })
            .await
            .unwrap();
        assert_eq!(second.revision, 2);
        assert_ne!(first.credential_reference, second.credential_reference);
        assert_ne!(first.reference_digest, second.reference_digest);
        let summary_json = serde_json::to_value(second.redacted_summary()).unwrap();
        assert!(summary_json.get("credential_reference").is_none());
        assert!(summary_json.get("reference_digest").is_none());
        assert!(summary_json.get("manifest_digest").is_none());
        assert!(summary_json.get("auth_schema_id").is_none());
        assert!(summary_json.get("authority").is_none());
        let current = repository
            .get_managed_credential_enrollment("managed_enrollment")
            .await
            .unwrap()
            .unwrap();
        let current_summary = current.redacted_summary();
        assert_eq!(current.credential_reference, "enr.second");
        assert_eq!(current.authority.writer_mode, "best_effort_single_process");
        assert_eq!(
            current.authority_evidence_digest.as_deref(),
            Some(second_authority_evidence_digest.as_str())
        );
        assert_eq!(current_summary.enrollment_revision, current.revision);
        assert!(!current.authority.writer_mode.contains("compare_and_swap"));
        let authority_debug = format!("{:?}", current.authority);
        let summary_debug = format!("{current_summary:?}");
        let record_debug = format!("{current:?}");
        assert!(!authority_debug.contains("in-memory-test"));
        assert!(!authority_debug.contains("best_effort_single_process"));
        assert!(!summary_debug.contains(&digest));
        assert!(!summary_debug.contains("static_env_secret"));
        assert!(!summary_debug.contains("in-memory-test"));
        assert!(!summary_debug.contains("best_effort_single_process"));
        assert!(!record_debug.contains("enr.second"));
        assert!(!record_debug.contains(&second_digest));
        assert!(!record_debug.contains(second_authority_evidence_digest.as_str()));
    }

    #[tokio::test]
    async fn legacy_rows_without_authority_evidence_digest_remain_readable_as_missing_fact() {
        let repository = repository().await;
        let digest = managed_fixture(&repository).await;
        let authority_evidence_digest = "a".repeat(64);
        repository
            .save_managed_credential_enrollment(SaveManagedCredentialEnrollment {
                managed_mcp_id: "managed_enrollment",
                manifest_digest: &digest,
                auth_schema_id: "static_env_secret",
                credential_reference: "enr.first",
                reference_digest: &"1".repeat(64),
                authority: &ManagedCredentialEnrollmentAuthoritySummary {
                    provider_id: "in-memory-test".to_string(),
                    writer_mode: "best_effort_single_process".to_string(),
                },
                authority_evidence_digest: &authority_evidence_digest,
                now_ms: 10,
            })
            .await
            .unwrap();

        sqlx::query(
            "UPDATE managed_credential_enrollments SET authority_evidence_digest = NULL WHERE managed_mcp_id = ?",
        )
        .bind("managed_enrollment")
        .execute(&repository.pool)
        .await
        .unwrap();

        let current = repository
            .get_managed_credential_enrollment("managed_enrollment")
            .await
            .unwrap()
            .unwrap();
        assert!(current.authority_evidence_digest.is_none());
    }
}
