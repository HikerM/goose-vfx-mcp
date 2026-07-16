use std::path::{Path, PathBuf};

use goose::mcp_platform::adapters::plan_for_manifest;
use goose::mcp_platform::{
    parse_manifest, AdapterEvidence, AuditEventType, CompensationDescriptor, ConfirmationEvidence,
    ConnectionProjection, CreateTask, HealthState, InsertOutcome, InstallationPlan,
    InstallationState, ManagedVersionRecord, ManifestProof, ManifestRecord, McpPlatformErrorCode,
    NewManagedMcp, PlanOperation, PlanTarget, PolicyContext, RecoveryDecision, RedactedError,
    RedactedErrorCode, RegistrationState, RollbackEvidence, RollbackStatus, RuntimeState, SavePlan,
    SqliteMcpPlatformRepository, StepEvidence, StepTransition, TaskOperation, TaskRecord,
    TaskStatus, TaskStepStatus, TaskTransition, ToolPolicy, ToolPolicyDecision, TrustTier,
};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");
const NPM: &str = include_str!("../../../documentation/static/schemas/examples/npm-package.json");
const BINARY: &str =
    include_str!("../../../documentation/static/schemas/examples/binary-archive-houdini.json");

async fn test_repository() -> (TempDir, PathBuf, SqliteMcpPlatformRepository) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp-platform").join("platform.db");
    let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    (directory, path, repository)
}

fn plan(
    fixture: &str,
    trust_tier: TrustTier,
) -> (goose::mcp_platform::VerifiedManifest, InstallationPlan) {
    let manifest = parse_manifest(fixture.as_bytes()).unwrap();
    let context = PolicyContext::new(trust_tier, PlanOperation::Register);
    let plan = plan_for_manifest(&manifest, &context).unwrap();
    (manifest, plan)
}

async fn save_manifest_and_plan(
    repository: &SqliteMcpPlatformRepository,
    fixture: &str,
    trust_tier: TrustTier,
    plan_id: &str,
    idempotency_key: &str,
) -> InstallationPlan {
    let (manifest, plan) = plan(fixture, trust_tier);
    repository
        .save_manifest(&ManifestRecord {
            verified: manifest,
            proof: ManifestProof::LocalBytes,
            trust_tier,
            created_at_ms: 10,
        })
        .await
        .unwrap();
    let target = PlanTarget {
        managed_mcp_id: None,
        mcp_id: plan.manifest_id().to_string(),
        version: plan.manifest_version().to_string(),
    };
    repository
        .save_plan(SavePlan {
            plan_id,
            idempotency_key,
            plan: &plan,
            target: &target,
            policy_evidence: plan.policy(),
            confirmation_evidence: &ConfirmationEvidence::Pending,
            expires_at_ms: 10_000,
            created_at_ms: 20,
            actor: "tester",
        })
        .await
        .unwrap();
    plan
}

async fn create_task(
    repository: &SqliteMcpPlatformRepository,
    plan_id: &str,
    plan: &InstallationPlan,
    task_id: &str,
    key: &str,
    adapter_evidence: Option<&AdapterEvidence>,
    rollback_evidence: Option<&RollbackEvidence>,
) -> TaskRecord {
    repository
        .create_task(CreateTask {
            task_id,
            plan_id,
            plan_digest: plan.plan_digest(),
            operation: TaskOperation::Register,
            idempotency_key: key,
            actor: "tester",
            now_ms: 30,
            adapter_evidence,
            rollback_evidence,
        })
        .await
        .unwrap()
}

async fn transition(
    repository: &SqliteMcpPlatformRepository,
    task: &TaskRecord,
    next_status: TaskStatus,
    now_ms: i64,
) -> TaskRecord {
    repository
        .transition_task(TaskTransition {
            task_id: &task.task_id,
            expected_revision: task.revision,
            next_status,
            actor: "tester",
            now_ms,
            heartbeat_at_ms: Some(now_ms),
            progress: task.progress,
            redacted_error: task.redacted_error.as_ref(),
            rollback_status: task.rollback_status,
            rollback_evidence: task.rollback_evidence.as_ref(),
        })
        .await
        .unwrap()
}

async fn advance_to_running(
    repository: &SqliteMcpPlatformRepository,
    mut task: TaskRecord,
    now_ms: i64,
) -> TaskRecord {
    task = transition(repository, &task, TaskStatus::AwaitingConfirmation, now_ms).await;
    let expected_revision = task.revision;
    task = repository
        .confirm_task(&task.task_id, expected_revision, "tester", now_ms + 1)
        .await
        .unwrap();
    let audit_after_confirmation = repository.list_audit_events(&task.task_id).await.unwrap();
    assert_eq!(
        audit_after_confirmation.last().unwrap().event_type,
        AuditEventType::ConfirmationRecorded
    );
    let confirmation_replay = repository
        .confirm_task(&task.task_id, expected_revision, "tester", now_ms + 1)
        .await
        .unwrap();
    assert_eq!(confirmation_replay, task);
    assert_eq!(
        repository
            .list_audit_events(&task.task_id)
            .await
            .unwrap()
            .len(),
        audit_after_confirmation.len()
    );
    transition(repository, &task, TaskStatus::Running, now_ms + 2).await
}

async fn raw_pool(path: &Path) -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_is_idempotent_and_configures_an_independent_database() {
    let (_directory, path, repository) = test_repository().await;
    let diagnostics = repository.diagnostics().await.unwrap();
    assert!(diagnostics.foreign_keys);
    assert_eq!(diagnostics.journal_mode, "wal");
    assert_eq!(diagnostics.busy_timeout_ms, 30_000);
    for table in [
        "schema_version",
        "manifest_blobs",
        "managed_mcps",
        "managed_versions",
        "connection_projections",
        "install_plans",
        "tasks",
        "task_steps",
        "audit_events",
    ] {
        assert!(diagnostics
            .tables
            .iter()
            .any(|candidate| candidate == table));
    }
    for index in [
        "managed_versions_one_active",
        "tasks_status_heartbeat",
        "audit_events_task_sequence",
    ] {
        assert!(diagnostics
            .indexes
            .iter()
            .any(|candidate| candidate == index));
    }
    assert!(!diagnostics.tables.iter().any(|table| table == "sessions"));
    repository.close().await;

    let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    assert_eq!(
        reopened.diagnostics().await.unwrap().tables,
        diagnostics.tables
    );
    reopened.close().await;

    let memory = SqliteMcpPlatformRepository::open_url("sqlite::memory:")
        .await
        .unwrap();
    assert!(memory.diagnostics().await.unwrap().foreign_keys);
}

#[tokio::test]
async fn newer_schema_is_rejected_without_migration_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("future.db");
    let pool = raw_pool(&path).await;
    sqlx::query(
        "CREATE TABLE schema_version(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO schema_version VALUES (99, 123)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let error = SqliteMcpPlatformRepository::open_path(&path)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::SchemaTooNew);
    assert!(!error.to_string().contains(path.to_string_lossy().as_ref()));

    let pool = raw_pool(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT version FROM schema_version")
            .fetch_one(&pool)
            .await
            .unwrap(),
        99
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn manifests_round_trip_replay_conflict_and_detect_tampering() {
    let (_directory, path, repository) = test_repository().await;
    let mut digests = Vec::new();
    for (index, fixture) in [REMOTE, MANUAL, NPM, BINARY].into_iter().enumerate() {
        let verified = parse_manifest(fixture.as_bytes()).unwrap();
        let record = ManifestRecord {
            verified: verified.clone(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            created_at_ms: index as i64,
        };
        assert_eq!(
            repository.save_manifest(&record).await.unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            repository.save_manifest(&record).await.unwrap(),
            InsertOutcome::IdempotentReplay
        );
        assert_eq!(
            repository
                .get_manifest(verified.digest())
                .await
                .unwrap()
                .verified
                .canonical_json(),
            verified.canonical_json()
        );
        digests.push(verified.digest().to_string());
    }

    let mut changed: Value = serde_json::from_str(REMOTE).unwrap();
    changed["description"] = json!("changed immutable identity");
    let conflict = ManifestRecord {
        verified: parse_manifest(&serde_json::to_vec(&changed).unwrap()).unwrap(),
        proof: ManifestProof::LocalBytes,
        trust_tier: TrustTier::Local,
        created_at_ms: 99,
    };
    assert_eq!(
        repository
            .save_manifest(&conflict)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::ManifestConflict
    );

    let pool = raw_pool(&path).await;
    sqlx::query("UPDATE manifest_blobs SET canonical_bytes = ? WHERE manifest_digest = ?")
        .bind(b"{}".as_slice())
        .bind(&digests[3])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_manifest(&digests[3])
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    sqlx::query("UPDATE manifest_blobs SET manifest_digest = ? WHERE manifest_digest = ?")
        .bind("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .bind(&digests[2])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_manifest("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
}

#[tokio::test]
async fn inventory_versions_and_projection_round_trip_with_revision_control() {
    let (_directory, path, repository) = test_repository().await;
    let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
    repository
        .save_manifest(&ManifestRecord {
            verified: verified.clone(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let managed = repository
        .create_managed_mcp(
            &NewManagedMcp {
                managed_mcp_id: "managed-1".to_string(),
                mcp_id: verified.manifest().id.clone(),
                installation_scope: "user".to_string(),
            },
            2,
        )
        .await
        .unwrap();
    assert!(!managed.state.default_enabled);
    assert_eq!(managed.revision, 0);

    let mut state = managed.state;
    state.registration = RegistrationState::Registered;
    state.installation = InstallationState::Installed;
    state.runtime = RuntimeState::Running;
    state.health = HealthState::Healthy;
    state.default_enabled = true;
    state.session_enabled.insert("session-1".to_string(), true);
    state.tool_policies.insert(
        "search".to_string(),
        ToolPolicy {
            decision: ToolPolicyDecision::Ask,
            scope: Some("workspace".to_string()),
        },
    );
    let updated = repository
        .update_managed_state("managed-1", 0, &state, 3)
        .await
        .unwrap();
    assert_eq!(updated.state, state);
    assert_eq!(updated.revision, 1);
    assert_eq!(
        repository
            .update_managed_state("managed-1", 0, &state, 4)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RevisionConflict
    );

    let version = ManagedVersionRecord {
        version: verified.manifest().version.as_str().to_string(),
        manifest_digest: verified.digest().to_string(),
        installation_root: None,
        verified: true,
        active: true,
        adapter_evidence: None,
        created_at_ms: 5,
    };
    repository
        .save_managed_version("managed-1", &version)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_managed_mcp("managed-1")
            .await
            .unwrap()
            .versions,
        vec![version]
    );

    let (_, remote_plan) = plan(REMOTE, TrustTier::Official);
    let projection = remote_plan.connection_projection().clone();
    let stored = repository
        .put_connection_projection("managed-1", "mcp-managed-1", &projection, None, 6)
        .await
        .unwrap();
    assert_eq!(stored.projection, projection);
    let json = serde_json::to_string(&stored).unwrap();
    assert!(!json.contains("manifest_digest"));
    assert!(!json.contains("installation_root"));
    assert!(matches!(
        stored.projection,
        ConnectionProjection::RemoteHttp { .. }
    ));
    repository.close().await;
    let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    let reopened_managed = reopened.get_managed_mcp("managed-1").await.unwrap();
    assert_eq!(reopened_managed.state, state);
    assert_eq!(reopened_managed.versions.len(), 1);
    assert_eq!(
        reopened
            .get_connection_projection("managed-1")
            .await
            .unwrap()
            .projection,
        projection
    );
}

#[tokio::test]
async fn remote_and_manual_plans_are_immutable_idempotent_and_tamper_evident() {
    let (_directory, path, repository) = test_repository().await;
    let remote = save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-remote",
        "plan-key",
    )
    .await;
    let replay = repository
        .save_plan(SavePlan {
            plan_id: "plan-remote",
            idempotency_key: "plan-key",
            plan: &remote,
            target: &PlanTarget {
                managed_mcp_id: None,
                mcp_id: remote.manifest_id().to_string(),
                version: remote.manifest_version().to_string(),
            },
            policy_evidence: remote.policy(),
            confirmation_evidence: &ConfirmationEvidence::Pending,
            expires_at_ms: 10_000,
            created_at_ms: 20,
            actor: "tester",
        })
        .await
        .unwrap();
    assert_eq!(replay.plan_id, "plan-remote");
    assert_eq!(replay.plan, remote);
    assert_eq!(
        repository
            .save_plan(SavePlan {
                plan_id: "plan-remote",
                idempotency_key: "plan-key",
                plan: &remote,
                target: &PlanTarget {
                    managed_mcp_id: Some("different-managed-target".to_string()),
                    mcp_id: remote.manifest_id().to_string(),
                    version: remote.manifest_version().to_string(),
                },
                policy_evidence: remote.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: 10_000,
                created_at_ms: 20,
                actor: "tester",
            })
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );

    let manual_manifest = parse_manifest(MANUAL.as_bytes()).unwrap();
    repository
        .save_manifest(&ManifestRecord {
            verified: manual_manifest.clone(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            created_at_ms: 30,
        })
        .await
        .unwrap();
    let (_, manual) = plan(MANUAL, TrustTier::Local);
    assert_eq!(
        repository
            .save_plan(SavePlan {
                plan_id: "plan-manual",
                idempotency_key: "plan-key",
                plan: &manual,
                target: &PlanTarget {
                    managed_mcp_id: None,
                    mcp_id: manual.manifest_id().to_string(),
                    version: manual.manifest_version().to_string(),
                },
                policy_evidence: manual.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: 10_000,
                created_at_ms: 31,
                actor: "tester",
            })
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    save_manifest_and_plan(
        &repository,
        MANUAL,
        TrustTier::Local,
        "plan-tamper-json",
        "plan-tamper-json-key",
    )
    .await;
    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-tamper-digest",
        "plan-tamper-digest-key",
    )
    .await;
    for (plan_id, key) in [
        ("plan-tamper-target", "plan-tamper-target-key"),
        ("plan-tamper-confirmation", "plan-tamper-confirmation-key"),
        ("plan-tamper-expiry", "plan-tamper-expiry-key"),
        ("plan-tamper-envelope", "plan-tamper-envelope-key"),
    ] {
        save_manifest_and_plan(&repository, REMOTE, TrustTier::Official, plan_id, key).await;
    }

    let pool = raw_pool(&path).await;
    sqlx::query("UPDATE install_plans SET plan_json = '{}' WHERE plan_id = 'plan-tamper-json'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_plan("plan-tamper-json")
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    sqlx::query("UPDATE install_plans SET plan_digest = ? WHERE plan_id = 'plan-tamper-digest'")
        .bind("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_plan("plan-tamper-digest")
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );

    let tampered_target = PlanTarget {
        managed_mcp_id: Some("attacker-controlled-managed-id".to_string()),
        mcp_id: remote.manifest_id().to_string(),
        version: remote.manifest_version().to_string(),
    };
    sqlx::query("UPDATE install_plans SET target_json = ? WHERE plan_id = 'plan-tamper-target'")
        .bind(serde_json::to_string(&tampered_target).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE install_plans SET confirmation_evidence_json = ? \
         WHERE plan_id = 'plan-tamper-confirmation'",
    )
    .bind(
        serde_json::to_string(&ConfirmationEvidence::Confirmed {
            actor: "attacker".to_string(),
            confirmed_at_ms: 21,
        })
        .unwrap(),
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE install_plans SET expires_at_ms = expires_at_ms + 1 \
         WHERE plan_id = 'plan-tamper-expiry'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE install_plans SET envelope_digest = ? WHERE plan_id = 'plan-tamper-envelope'",
    )
    .bind("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
    .execute(&pool)
    .await
    .unwrap();
    for plan_id in [
        "plan-tamper-target",
        "plan-tamper-confirmation",
        "plan-tamper-expiry",
        "plan-tamper-envelope",
    ] {
        assert_eq!(
            repository.get_plan(plan_id).await.unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError,
            "{plan_id} must fail immutable envelope verification"
        );
    }

    pool.close().await;
    repository.close().await;
    let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    assert_eq!(reopened.get_plan("plan-remote").await.unwrap(), replay);
}

#[tokio::test]
async fn task_creation_validates_the_plan_in_the_write_transaction() {
    let (_directory, path, repository) = test_repository().await;
    let plan = save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-create-tamper",
        "plan-create-tamper-key",
    )
    .await;
    let pool = raw_pool(&path).await;
    sqlx::query("UPDATE install_plans SET plan_json = '{}' WHERE plan_id = 'plan-create-tamper'")
        .execute(&pool)
        .await
        .unwrap();

    let error = repository
        .create_task(CreateTask {
            task_id: "task-from-tampered-plan",
            plan_id: "plan-create-tamper",
            plan_digest: plan.plan_digest(),
            operation: TaskOperation::Register,
            idempotency_key: "task-from-tampered-plan-key",
            actor: "tester",
            now_ms: 30,
            adapter_evidence: None,
            rollback_evidence: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    let task_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE task_id = 'task-from-tampered-plan'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE task_id = 'task-from-tampered-plan'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((task_count, audit_count), (0, 0));

    let valid_plan = plan.clone();
    repository
        .save_plan(SavePlan {
            plan_id: "plan-expired",
            idempotency_key: "plan-expired-key",
            plan: &valid_plan,
            target: &PlanTarget {
                managed_mcp_id: None,
                mcp_id: valid_plan.manifest_id().to_string(),
                version: valid_plan.manifest_version().to_string(),
            },
            policy_evidence: valid_plan.policy(),
            confirmation_evidence: &ConfirmationEvidence::Pending,
            expires_at_ms: 29,
            created_at_ms: 20,
            actor: "tester",
        })
        .await
        .unwrap();
    let expired = repository
        .create_task(CreateTask {
            task_id: "task-from-expiry-tamper",
            plan_id: "plan-expired",
            plan_digest: valid_plan.plan_digest(),
            operation: TaskOperation::Register,
            idempotency_key: "task-from-expiry-tamper-key",
            actor: "tester",
            now_ms: 30,
            adapter_evidence: None,
            rollback_evidence: None,
        })
        .await
        .unwrap_err();
    assert_eq!(expired.code(), McpPlatformErrorCode::PlanExpired);
    let task_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE task_id = 'task-from-expiry-tamper'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(task_count, 0);
}

#[tokio::test]
async fn task_replay_survives_plan_expiry_but_expiry_blocks_new_tasks() {
    let (_directory, path, repository) = test_repository().await;
    let plan = save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-expiry-replay",
        "plan-expiry-replay-key",
    )
    .await;
    let create = |now_ms| CreateTask {
        task_id: "expiry-replay-task",
        plan_id: "plan-expiry-replay",
        plan_digest: plan.plan_digest(),
        operation: TaskOperation::Register,
        idempotency_key: "expiry-replay-task-key",
        actor: "tester",
        now_ms,
        adapter_evidence: None,
        rollback_evidence: None,
    };
    let created = repository.create_task(create(9_999)).await.unwrap();
    let initial_audit = repository
        .list_audit_events(&created.task_id)
        .await
        .unwrap();
    assert_eq!(
        repository.create_task(create(10_000)).await.unwrap(),
        created
    );
    assert_eq!(
        repository.create_task(create(10_001)).await.unwrap(),
        created
    );
    assert_eq!(
        repository
            .list_audit_events(&created.task_id)
            .await
            .unwrap(),
        initial_audit
    );

    let pool = raw_pool(&path).await;
    let task_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE idempotency_key = 'expiry-replay-task-key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(task_count, 1);
    let error = repository
        .create_task(CreateTask {
            task_id: "new-task-at-expiry",
            plan_id: "plan-expiry-replay",
            plan_digest: plan.plan_digest(),
            operation: TaskOperation::Register,
            idempotency_key: "new-task-at-expiry-key",
            actor: "tester",
            now_ms: 10_000,
            adapter_evidence: None,
            rollback_evidence: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::PlanExpired);
    let rejected_task_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE task_id = 'new-task-at-expiry'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let rejected_audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events WHERE task_id = 'new-task-at-expiry'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((rejected_task_count, rejected_audit_count), (0, 0));
}

#[tokio::test]
async fn task_idempotency_transitions_concurrency_steps_and_audit_are_durable() {
    let (_directory, path, repository) = test_repository().await;
    let plan = save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-task",
        "plan-task-key",
    )
    .await;
    let task = create_task(
        &repository,
        "plan-task",
        &plan,
        "task-1",
        "task-key",
        None,
        None,
    )
    .await;
    let replay = create_task(
        &repository,
        "plan-task",
        &plan,
        "ignored-task",
        "task-key",
        None,
        None,
    )
    .await;
    assert_eq!(replay.task_id, task.task_id);
    assert_eq!(
        repository
            .transition_task(TaskTransition {
                task_id: &task.task_id,
                expected_revision: task.revision,
                next_status: TaskStatus::Succeeded,
                actor: "tester",
                now_ms: 31,
                heartbeat_at_ms: None,
                progress: 100,
                redacted_error: None,
                rollback_status: RollbackStatus::NotRequired,
                rollback_evidence: None,
            })
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::InvalidTransition
    );

    let task = advance_to_running(&repository, task, 40).await;
    repository
        .add_task_step(
            &task.task_id,
            0,
            "step-token",
            &CompensationDescriptor::RemoveConnectionProjection {
                link_key: "managed-link".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .add_task_step(
                &task.task_id,
                1,
                "step-token",
                &CompensationDescriptor::RemoveConnectionProjection {
                    link_key: "managed-link".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IdempotencyConflict
    );
    repository
        .transition_task_step(StepTransition {
            task_id: &task.task_id,
            ordinal: 0,
            expected_status: TaskStepStatus::NotStarted,
            next_status: TaskStepStatus::Started,
            evidence: None,
            actor: "tester",
            now_ms: 50,
        })
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id: &task.task_id,
            ordinal: 0,
            expected_status: TaskStepStatus::Started,
            next_status: TaskStepStatus::Committed,
            evidence: Some(&StepEvidence::ConnectionProjected {
                link_key: "managed-link".to_string(),
                revision: 0,
            }),
            actor: "tester",
            now_ms: 51,
        })
        .await
        .unwrap();
    let task = repository.get_task(&task.task_id).await.unwrap();
    let task = transition(&repository, &task, TaskStatus::Verifying, 52).await;
    let task = transition(&repository, &task, TaskStatus::Activating, 53).await;
    let task = transition(&repository, &task, TaskStatus::Succeeded, 54).await;
    assert!(task.status.is_terminal());
    let audit = repository.list_audit_events(&task.task_id).await.unwrap();
    assert_eq!(audit.last().unwrap().sequence, task.event_sequence);
    assert!(audit
        .windows(2)
        .all(|pair| pair[0].sequence + 1 == pair[1].sequence));
    assert!(audit
        .iter()
        .any(|event| event.event_type == AuditEventType::ConfirmationRecorded));
    assert!(audit
        .iter()
        .any(|event| event.event_type == AuditEventType::StepCommitted));

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-race",
        "plan-race-key",
    )
    .await;
    let first = repository.clone();
    let second = repository.clone();
    let digest = plan.plan_digest().to_string();
    let (left, right) = tokio::join!(
        first.create_task(CreateTask {
            task_id: "race-left",
            plan_id: "plan-race",
            plan_digest: &digest,
            operation: TaskOperation::Register,
            idempotency_key: "race-key",
            actor: "tester",
            now_ms: 60,
            adapter_evidence: None,
            rollback_evidence: None,
        }),
        second.create_task(CreateTask {
            task_id: "race-right",
            plan_id: "plan-race",
            plan_digest: &digest,
            operation: TaskOperation::Register,
            idempotency_key: "race-key",
            actor: "tester",
            now_ms: 60,
            adapter_evidence: None,
            rollback_evidence: None,
        })
    );
    assert_eq!(left.unwrap().task_id, right.unwrap().task_id);
    let pool = raw_pool(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM tasks WHERE idempotency_key = 'race-key'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-cancel",
        "plan-cancel-key",
    )
    .await;
    let cancel_task = create_task(
        &repository,
        "plan-cancel",
        &plan,
        "cancel-task",
        "cancel-task-key",
        None,
        None,
    )
    .await;
    let cancel_task = transition(
        &repository,
        &cancel_task,
        TaskStatus::AwaitingConfirmation,
        70,
    )
    .await;
    let cancellation_expected_revision = cancel_task.revision;
    let cancelled = repository
        .request_cancel(
            &cancel_task.task_id,
            cancellation_expected_revision,
            "tester",
            71,
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    let cancellation_audit = repository
        .list_audit_events(&cancelled.task_id)
        .await
        .unwrap();
    assert_eq!(
        cancellation_audit.last().unwrap().event_type,
        AuditEventType::CancellationRequested
    );
    assert_eq!(
        repository
            .request_cancel(
                &cancelled.task_id,
                cancellation_expected_revision,
                "tester",
                71,
            )
            .await
            .unwrap(),
        cancelled
    );
    assert_eq!(
        repository
            .list_audit_events(&cancelled.task_id)
            .await
            .unwrap()
            .len(),
        cancellation_audit.len()
    );
    assert!(cancellation_audit
        .windows(2)
        .all(|pair| pair[0].sequence + 1 == pair[1].sequence));

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-retry",
        "plan-retry-key",
    )
    .await;
    let retry_task = create_task(
        &repository,
        "plan-retry",
        &plan,
        "retry-task",
        "retry-task-key",
        None,
        None,
    )
    .await;
    let retry_task = advance_to_running(&repository, retry_task, 80).await;
    let retry_task = transition(&repository, &retry_task, TaskStatus::Failed, 83).await;
    let retried = repository
        .retry_task(
            &retry_task.task_id,
            retry_task.revision,
            "retry-request-key",
            "tester",
            84,
        )
        .await
        .unwrap();
    assert_eq!(retried.status, TaskStatus::Queued);
    assert_eq!(
        repository
            .retry_task(
                &retried.task_id,
                retried.revision,
                "retry-request-key",
                "tester",
                84,
            )
            .await
            .unwrap(),
        retried
    );
}

#[tokio::test]
async fn startup_recovery_is_pure_idempotent_redacted_and_survives_reopen() {
    let (_directory, path, repository) = test_repository().await;
    let plan = save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-recovery",
        "plan-recovery-key",
    )
    .await;
    let adapter = AdapterEvidence {
        adapter_id: "remote_http".to_string(),
        adapter_version: "1".to_string(),
        compatible_for_recovery: true,
        resume_safe: true,
    };
    let rollback = RollbackEvidence {
        compensation_available: true,
        remaining_compensations: vec![CompensationDescriptor::RemoveConnectionProjection {
            link_key: "link".to_string(),
        }],
    };

    let resumable = create_task(
        &repository,
        "plan-recovery",
        &plan,
        "resumable",
        "resumable-key",
        Some(&adapter),
        Some(&rollback),
    )
    .await;
    let resumable = advance_to_running(&repository, resumable, 10).await;
    repository
        .add_task_step(
            &resumable.task_id,
            0,
            "recovery-token",
            &CompensationDescriptor::RemoveConnectionProjection {
                link_key: "link".to_string(),
            },
        )
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id: &resumable.task_id,
            ordinal: 0,
            expected_status: TaskStepStatus::NotStarted,
            next_status: TaskStepStatus::Started,
            evidence: None,
            actor: "tester",
            now_ms: 13,
        })
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id: &resumable.task_id,
            ordinal: 0,
            expected_status: TaskStepStatus::Started,
            next_status: TaskStepStatus::Committed,
            evidence: Some(&StepEvidence::ConnectionProjected {
                link_key: "link".to_string(),
                revision: 0,
            }),
            actor: "tester",
            now_ms: 14,
        })
        .await
        .unwrap();

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-rollback",
        "plan-rollback-key",
    )
    .await;
    let rolling = create_task(
        &repository,
        "plan-rollback",
        &plan,
        "rolling",
        "rolling-key",
        Some(&adapter),
        Some(&rollback),
    )
    .await;
    let rolling = advance_to_running(&repository, rolling, 10).await;
    repository
        .add_task_step(
            &rolling.task_id,
            2,
            "rollback-token",
            &CompensationDescriptor::RemoveConnectionProjection {
                link_key: "link-2".to_string(),
            },
        )
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id: &rolling.task_id,
            ordinal: 2,
            expected_status: TaskStepStatus::NotStarted,
            next_status: TaskStepStatus::Started,
            evidence: None,
            actor: "tester",
            now_ms: 13,
        })
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id: &rolling.task_id,
            ordinal: 2,
            expected_status: TaskStepStatus::Started,
            next_status: TaskStepStatus::Committed,
            evidence: None,
            actor: "tester",
            now_ms: 14,
        })
        .await
        .unwrap();
    let rolling = repository.get_task(&rolling.task_id).await.unwrap();
    let _rolling = transition(&repository, &rolling, TaskStatus::RollingBack, 15).await;

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-manual-recovery",
        "plan-manual-recovery-key",
    )
    .await;
    let incompatible_adapter = AdapterEvidence {
        adapter_id: "remote_http".to_string(),
        adapter_version: "0".to_string(),
        compatible_for_recovery: false,
        resume_safe: false,
    };
    let manual = create_task(
        &repository,
        "plan-manual-recovery",
        &plan,
        "manual-recovery",
        "manual-recovery-key",
        Some(&incompatible_adapter),
        None,
    )
    .await;
    let _manual = advance_to_running(&repository, manual, 10).await;

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-terminal",
        "plan-terminal-key",
    )
    .await;
    let terminal = create_task(
        &repository,
        "plan-terminal",
        &plan,
        "terminal",
        "terminal-key",
        None,
        None,
    )
    .await;
    let terminal = advance_to_running(&repository, terminal, 10).await;
    let terminal = transition(&repository, &terminal, TaskStatus::Verifying, 13).await;
    let terminal = transition(&repository, &terminal, TaskStatus::Activating, 14).await;
    let _terminal = transition(&repository, &terminal, TaskStatus::Succeeded, 15).await;

    let recovered = repository
        .recover_stale_tasks(100, "startup-recovery", 200)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 3);
    assert!(recovered
        .iter()
        .all(|record| record.task.status == TaskStatus::Interrupted));
    assert!(recovered.iter().any(|record| {
        record.task.task_id == "resumable"
            && record.decision == RecoveryDecision::ResumeFromStep { ordinal: 1 }
    }));
    assert!(recovered.iter().any(|record| {
        record.task.task_id == "rolling"
            && record.decision == RecoveryDecision::RollbackFromStep { ordinal: 2 }
    }));
    assert!(recovered.iter().any(|record| {
        record.task.task_id == "manual-recovery"
            && record.decision == RecoveryDecision::RequiresManualRecovery
    }));
    assert!(repository
        .recover_stale_tasks(100, "startup-recovery", 201)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        repository.get_task("terminal").await.unwrap().status,
        TaskStatus::Succeeded
    );

    save_manifest_and_plan(
        &repository,
        REMOTE,
        TrustTier::Official,
        "plan-secret",
        "plan-secret-key",
    )
    .await;
    let secret_task = create_task(
        &repository,
        "plan-secret",
        &plan,
        "secret-task",
        "secret-task-key",
        None,
        None,
    )
    .await;
    let secret_task = advance_to_running(&repository, secret_task, 300).await;
    let secret = "canary-super-secret";
    let redacted = RedactedError::new(
        RedactedErrorCode::AdapterFailed,
        "Authorization: Bearer bearer-token https://callback.invalid/?code=oauth-code env=canary-super-secret",
        [secret],
    );
    assert!(!redacted.message().contains(secret));
    assert!(!redacted.message().contains("bearer-token"));
    assert!(!redacted.message().contains("oauth-code"));
    repository
        .transition_task(TaskTransition {
            task_id: &secret_task.task_id,
            expected_revision: secret_task.revision,
            next_status: TaskStatus::Failed,
            actor: "tester",
            now_ms: 303,
            heartbeat_at_ms: Some(303),
            progress: 10,
            redacted_error: Some(&redacted),
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        })
        .await
        .unwrap();
    let audit_json =
        serde_json::to_string(&repository.list_audit_events("secret-task").await.unwrap()).unwrap();
    assert!(!audit_json.contains(secret));
    assert!(!audit_json.contains("bearer-token"));
    assert!(!audit_json.contains("oauth-code"));

    repository.close().await;
    let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    assert_eq!(
        reopened.get_task("resumable").await.unwrap().status,
        TaskStatus::Interrupted
    );
    assert_eq!(
        reopened.list_task_steps("resumable").await.unwrap()[0].status,
        TaskStepStatus::Committed
    );
    assert_eq!(reopened.get_plan("plan-recovery").await.unwrap().plan, plan);
    reopened.close().await;
    let mut database_bytes = std::fs::read(&path).unwrap();
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        if let Ok(bytes) = std::fs::read(sidecar) {
            database_bytes.extend(bytes);
        }
    }
    let database_text = String::from_utf8_lossy(&database_bytes);
    assert!(!database_text.contains(secret));
    assert!(!database_text.contains("bearer-token"));
    assert!(!database_text.contains("oauth-code"));
}
