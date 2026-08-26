use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use lumina::session::ExtensionState;

struct StaticProvenanceVerifier {
    provenance: lumina::session::ManagedExtensionProvenance,
    available: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl lumina::session::SessionExtensionProvenanceVerifier for StaticProvenanceVerifier {
    async fn verified_provenance(&self) -> Result<lumina::session::ManagedExtensionProvenance> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.available
            .then(|| self.provenance.clone())
            .ok_or_else(|| anyhow!("provenance unavailable"))
    }
}

fn frontend_extension(name: &str, instructions: &str) -> lumina::agents::ExtensionConfig {
    lumina::agents::ExtensionConfig::Frontend {
        name: name.to_string(),
        description: name.to_string(),
        tools: Vec::new(),
        instructions: Some(instructions.to_string()),
        bundled: None,
        available_tools: Vec::new(),
    }
}

#[tokio::test]
async fn agent_direct_extension_path_rejects_managed_extension_name_before_live_sink() {
    let temp_dir = tempfile::tempdir().unwrap();
    let managed = frontend_extension("managed-canonical", "managed instructions");
    let ordinary = frontend_extension("ordinary", "ordinary instructions");
    let mut provenance = lumina::session::ManagedExtensionProvenance::default();
    provenance.names = HashSet::from([managed.name()]);
    let session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            Arc::new(StaticProvenanceVerifier {
                provenance,
                available: true,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ),
    );
    let session = session_manager
        .create_session(
            temp_dir.path().to_path_buf(),
            "direct extension guard".to_string(),
            lumina::session::SessionType::User,
            lumina::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    let agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                temp_dir.path().to_path_buf(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));

    assert!(agent.add_extension(managed, &session.id).await.is_err());
    assert!(agent.get_extension_configs().await.is_empty());

    agent.add_extension(ordinary, &session.id).await.unwrap();
    assert_eq!(agent.get_extension_configs().await.len(), 1);
}

#[test]
fn public_profile_views_remain_constructible_without_internal_hash_fields() {
    let profile = lumina::mcp_platform::McpProfile {
        profile_id: "profile".to_string(),
        name: "Example".to_string(),
        description: "public view".to_string(),
        revision: 1,
        archived: false,
        entries: vec![lumina::mcp_platform::ProfileEntry {
            managed_mcp_id: "managed".to_string(),
            ordinal: 0,
        }],
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let confirmation = lumina::mcp_platform::ProfileApplyConfirmationView {
        confirmation_token: "profile_apply_confirmation_token".to_string(),
    };
    let token = lumina::mcp_platform::ProfileApplicationTokenView {
        token: "profile_application_token".to_string(),
        plan_id: "plan".to_string(),
        profile_id: profile.profile_id.clone(),
        profile_revision: profile.revision,
        expires_at_ms: 1,
    };

    assert_eq!(profile.entries.len(), 1);
    assert_eq!(
        confirmation.confirmation_token,
        "profile_apply_confirmation_token"
    );
    assert_eq!(token.plan_id, "plan");
}

#[test]
fn public_managed_extension_provenance_debug_surface_omits_sensitive_evidence() {
    let mut provenance = lumina::session::ManagedExtensionProvenance::default();
    provenance.names.insert("managed-canonical".to_string());

    let debug = format!("{provenance:?}");

    assert!(debug.contains("managed-canonical"));
    assert!(!debug.contains("source_fingerprints"));
    assert!(!debug.contains("bindings"));
}

#[tokio::test]
async fn unavailable_provenance_keeps_plain_session_sink_free_and_fails_closed_on_add() {
    let temp_dir = tempfile::tempdir().unwrap();
    let session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            Arc::new(StaticProvenanceVerifier {
                provenance: lumina::session::ManagedExtensionProvenance::default(),
                available: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ),
    );
    let session = session_manager
        .create_session(
            temp_dir.path().to_path_buf(),
            "plain unavailable".to_string(),
            lumina::session::SessionType::User,
            lumina::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    let agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                temp_dir.path().to_path_buf(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));

    assert!(agent
        .load_extensions_from_session(&session)
        .await
        .unwrap()
        .is_empty());
    assert!(agent
        .add_extension(frontend_extension("ordinary", "ordinary"), &session.id)
        .await
        .is_err());
    assert!(agent.get_extension_configs().await.is_empty());
}

#[tokio::test]
async fn legacy_plain_load_does_not_authorize_add_or_tampered_persisted_activation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let session_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("20240101_120000.jsonl"), "{}\n").unwrap();
    fs::write(
        session_dir.join("tampered.jsonl"),
        r#"{"extension_data":{"enabled_extensions.v0":{"extensions":[{"Frontend":{"name":"tampered","description":"tampered","tools":[],"instructions":"tampered","bundled":null,"available_tools":[]}}]}}}
"#,
    )
    .unwrap();

    let session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            Arc::new(StaticProvenanceVerifier {
                provenance: lumina::session::ManagedExtensionProvenance::default(),
                available: false,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ),
    );
    let plain = session_manager
        .get_session("20240101_120000", false)
        .await
        .unwrap();
    assert!(plain.extension_data.extension_states.is_empty());
    assert!(session_manager
        .get_session("tampered", false)
        .await
        .is_err());

    let agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                temp_dir.path().to_path_buf(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));
    assert!(agent
        .add_extension(frontend_extension("ordinary", "ordinary"), &plain.id)
        .await
        .is_err());
    assert!(agent.get_extension_configs().await.is_empty());
}

#[tokio::test]
async fn public_session_load_rejects_persisted_extension_without_managed_provenance() {
    let temp_dir = tempfile::tempdir().unwrap();
    let session_dir = temp_dir.path().join("sessions");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("managed.jsonl"),
        r#"{"extension_data":{"enabled_extensions.v0":{"extensions":[{"Frontend":{"name":"managed","description":"managed","tools":[],"instructions":"managed","bundled":null,"available_tools":[]}}]}}}
"#,
    )
    .unwrap();

    let session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            Arc::new(StaticProvenanceVerifier {
                provenance: lumina::session::ManagedExtensionProvenance::default(),
                available: true,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ),
    );
    let session = session_manager.get_session("managed", false).await.unwrap();
    let agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                temp_dir.path().to_path_buf(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));

    assert!(agent
        .load_extensions_from_session(&session)
        .await
        .unwrap()
        .is_empty());
    assert!(agent.get_extension_configs().await.is_empty());
}

fn extension_data_with_profile_marker(
    extension: &lumina::agents::ExtensionConfig,
    marker_name: &str,
) -> lumina::session::ExtensionData {
    let mut extension_data = lumina::session::ExtensionData::new();
    lumina::session::EnabledExtensionsState::new(vec![extension.clone()])
        .to_extension_data(&mut extension_data)
        .unwrap();
    extension_data.set_extension_state(
        "mcp_profile_application",
        "v1",
        serde_json::json!({
            "application_id": "application-id",
            "profile_id": "profile-id",
            "profile_revision": 1,
            "merge_policy": "replace_managed_only",
            "original_session_id": "original-session",
            "managed_extension_names": [marker_name]
        }),
    );
    extension_data
}

#[tokio::test]
async fn public_session_load_verifies_real_persisted_managed_record_and_rejects_tampering() {
    let temp_dir = tempfile::tempdir().unwrap();
    let managed = frontend_extension("managed", "managed instructions");

    let control_root = temp_dir.path().join("control");
    let tampered_root = temp_dir.path().join("tampered");
    let control_seed_manager = lumina::session::SessionManager::new(control_root.clone());
    let control_seed = control_seed_manager
        .create_session(
            control_root.clone(),
            "provenance fixture seed".to_string(),
            lumina::session::SessionType::User,
            lumina::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    let tampered_seed_manager = lumina::session::SessionManager::new(tampered_root.clone());
    let tampered_seed = tampered_seed_manager
        .create_session(
            tampered_root.clone(),
            "tampered provenance fixture seed".to_string(),
            lumina::session::SessionType::User,
            lumina::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    let mut control_json = serde_json::to_value(control_seed).unwrap();
    control_json["extension_data"] =
        serde_json::to_value(extension_data_with_profile_marker(&managed, "managed")).unwrap();
    let mut tampered_json = serde_json::to_value(tampered_seed).unwrap();
    tampered_json["extension_data"] =
        serde_json::to_value(extension_data_with_profile_marker(&managed, "other-name")).unwrap();

    let control_calls = Arc::new(AtomicUsize::new(0));
    let control_session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            control_root.clone(),
            Arc::new(StaticProvenanceVerifier {
                provenance: {
                    let mut p = lumina::session::ManagedExtensionProvenance::default();
                    p.names.insert("managed".to_string());
                    p
                },
                available: true,
                calls: control_calls.clone(),
            }),
        ),
    );
    let control = control_session_manager
        .import_session_with_managed_provenance(
            &serde_json::to_string(&control_json).unwrap(),
            None,
            &[],
        )
        .await
        .unwrap();
    let control_agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            control_session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                control_root.clone(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));
    assert!(
        control_agent
            .load_extensions_from_session(&control)
            .await
            .unwrap()
            .len()
            == 1
    );
    assert!(control_calls.load(Ordering::SeqCst) > 0);
    assert_eq!(control_agent.get_extension_configs().await.len(), 1);

    let tampered_calls = Arc::new(AtomicUsize::new(0));
    let tampered_session_manager = Arc::new(
        lumina::session::SessionManager::new_with_provenance_verifier(
            tampered_root.clone(),
            Arc::new(StaticProvenanceVerifier {
                provenance: {
                    let mut p = lumina::session::ManagedExtensionProvenance::default();
                    p.names.insert("managed".to_string());
                    p
                },
                available: true,
                calls: tampered_calls.clone(),
            }),
        ),
    );
    let tampered = tampered_session_manager
        .import_session_with_managed_provenance(
            &serde_json::to_string(&tampered_json).unwrap(),
            None,
            &[],
        )
        .await
        .unwrap();
    let tampered_agent = Arc::new(lumina::agents::Agent::with_config(
        lumina::agents::AgentConfig::new(
            tampered_session_manager,
            Arc::new(lumina::config::permission::PermissionManager::new(
                tampered_root.clone(),
            )),
            None,
            lumina::config::LuminaMode::default(),
            false,
            lumina::agents::LuminaPlatform::LuminaCli,
        ),
    ));

    assert!(tampered_agent
        .load_extensions_from_session(&tampered)
        .await
        .unwrap()
        .is_empty());
    assert!(tampered_calls.load(Ordering::SeqCst) > 0);
    assert_eq!(tampered_agent.get_extension_configs().await.len(), 0);
}
