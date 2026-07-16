use goose::agents::ExtensionConfig;
use goose::mcp_platform::adapters::plan_for_manifest;
use goose::mcp_platform::manifest::{Architecture, Platform};
use goose::mcp_platform::{
    evaluate_manifest_policy, parse_manifest, CatalogCompatibility, CatalogFilter,
    CatalogInsertOutcome, CompatibilityTarget, ManifestProof, McpPlatformErrorCode, PlanOperation,
    PolicyContext, PolicyOutcome, PolicyReasonCode, TrustTier, VerifiedManifestCollection,
};
use serde_json::{json, Value};

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");
const NPM: &str = include_str!("../../../documentation/static/schemas/examples/npm-package.json");
const BINARY: &str =
    include_str!("../../../documentation/static/schemas/examples/binary-archive-houdini.json");

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn manual_with_distribution(distribution: Value) -> Value {
    let mut manifest: Value = serde_json::from_str(MANUAL).unwrap();
    manifest["distribution"] = distribution;
    manifest
}

fn phase_2a_context(trust_tier: TrustTier) -> PolicyContext {
    PolicyContext::new(trust_tier, PlanOperation::Register)
}

#[test]
fn phase_1_examples_parse_and_validate_against_embedded_schema() {
    for example in [REMOTE, MANUAL, NPM, BINARY] {
        parse_manifest(example.as_bytes()).unwrap();
    }
}

#[test]
fn all_distribution_variants_deserialize() {
    let python = manual_with_distribution(json!({
        "type": "python_wheel",
        "package": "example-mcp",
        "package_version": "1.0.0",
        "python": ">=3.11",
        "artifacts": [{
            "platform": "any",
            "arch": "any",
            "url": "https://packages.example.com/example_mcp-1.0.0.whl",
            "digest": {"algorithm": "sha256", "value": "5555555555555555555555555555555555555555555555555555555555555555"}
        }],
        "entrypoint": {"executable": "${installation.bin}/example-mcp"}
    }));
    let docker = manual_with_distribution(json!({
        "type": "docker",
        "image": "registry.example.com/example/mcp",
        "digest": {"algorithm": "sha256", "value": "6666666666666666666666666666666666666666666666666666666666666666"},
        "entrypoint": {"executable": "example-mcp", "args": ["--stdio"]}
    }));
    let git = manual_with_distribution(json!({
        "type": "git_dev",
        "repository": "https://github.com/example/mcp.git",
        "commit": "7777777777777777777777777777777777777777",
        "adapter": "npm",
        "entrypoint": {"executable": "${installation.bin}/example-mcp"}
    }));

    let fixtures = [
        (serde_json::from_str(REMOTE).unwrap(), "remote_http"),
        (serde_json::from_str(MANUAL).unwrap(), "manual_stdio"),
        (serde_json::from_str(NPM).unwrap(), "npm"),
        (python, "python_wheel"),
        (serde_json::from_str(BINARY).unwrap(), "binary_archive"),
        (docker, "docker"),
        (git, "git_dev"),
    ];

    for (fixture, expected) in fixtures {
        let verified = parse_manifest(&bytes(&fixture)).unwrap();
        assert_eq!(verified.manifest().distribution.adapter_id(), expected);
    }
}

#[test]
fn only_remote_and_manual_generate_phase_2a_plans() {
    let remote = parse_manifest(REMOTE.as_bytes()).unwrap();
    let manual = parse_manifest(MANUAL.as_bytes()).unwrap();
    assert_eq!(
        plan_for_manifest(&remote, &phase_2a_context(TrustTier::Official))
            .unwrap()
            .adapter()
            .id,
        "remote_http"
    );
    assert_eq!(
        plan_for_manifest(&manual, &phase_2a_context(TrustTier::Local))
            .unwrap()
            .adapter()
            .id,
        "manual_stdio"
    );

    let python = manual_with_distribution(json!({
        "type": "python_wheel",
        "package": "example-mcp",
        "package_version": "1.0.0",
        "python": ">=3.11",
        "artifacts": [{
            "platform": "any", "arch": "any",
            "url": "https://packages.example.com/example.whl",
            "digest": {"algorithm": "sha256", "value": "8888888888888888888888888888888888888888888888888888888888888888"}
        }],
        "entrypoint": {"executable": "${installation.bin}/example-mcp"}
    }));
    let docker = manual_with_distribution(json!({
        "type": "docker",
        "image": "registry.example.com/example/mcp",
        "digest": {"algorithm": "sha256", "value": "9999999999999999999999999999999999999999999999999999999999999999"},
        "entrypoint": {"executable": "example-mcp"}
    }));
    let git = manual_with_distribution(json!({
        "type": "git_dev",
        "repository": "https://github.com/example/mcp.git",
        "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "adapter": "npm",
        "entrypoint": {"executable": "${installation.bin}/example-mcp"}
    }));
    let unsupported = [
        serde_json::from_str(NPM).unwrap(),
        python,
        serde_json::from_str(BINARY).unwrap(),
        docker,
        git,
    ];
    for fixture in unsupported {
        let verified = parse_manifest(&bytes(&fixture)).unwrap();
        let error = plan_for_manifest(&verified, &phase_2a_context(TrustTier::Local)).unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::NotImplementedForPhase);
    }
}

#[test]
fn closed_schema_rejects_unknown_top_level_and_variant_fields() {
    let mut top: Value = serde_json::from_str(REMOTE).unwrap();
    top["unexpected"] = json!(true);
    assert_eq!(
        parse_manifest(&bytes(&top)).unwrap_err().code(),
        McpPlatformErrorCode::InvalidManifest
    );

    let mut variant: Value = serde_json::from_str(MANUAL).unwrap();
    variant["distribution"]["shell"] = json!("forbidden");
    assert_eq!(
        parse_manifest(&bytes(&variant)).unwrap_err().code(),
        McpPlatformErrorCode::InvalidManifest
    );
}

#[test]
fn invalid_versions_urls_digests_and_references_have_stable_codes() {
    for invalid in ["latest", "^1.2.3", "1.2"] {
        let mut manifest: Value = serde_json::from_str(REMOTE).unwrap();
        manifest["version"] = json!(invalid);
        assert_eq!(
            parse_manifest(&bytes(&manifest)).unwrap_err().code(),
            McpPlatformErrorCode::VersionNotExact
        );
    }

    let mut http_artifact: Value = serde_json::from_str(NPM).unwrap();
    http_artifact["distribution"]["artifacts"][0]["url"] =
        json!("http://packages.example.com/archive.tgz");
    assert_eq!(
        parse_manifest(&bytes(&http_artifact)).unwrap_err().code(),
        McpPlatformErrorCode::UnsafeUrl
    );

    let mut bad_digest: Value = serde_json::from_str(NPM).unwrap();
    bad_digest["distribution"]["artifacts"][0]["digest"]["value"] = json!("abcd");
    assert_eq!(
        parse_manifest(&bytes(&bad_digest)).unwrap_err().code(),
        McpPlatformErrorCode::InvalidDigest
    );

    let mut missing_digest: Value = serde_json::from_str(NPM).unwrap();
    missing_digest["distribution"]["artifacts"][0]["digest"]
        .as_object_mut()
        .unwrap()
        .remove("value");
    assert_eq!(
        parse_manifest(&bytes(&missing_digest)).unwrap_err().code(),
        McpPlatformErrorCode::InvalidDigest
    );

    let git = manual_with_distribution(json!({
        "type": "git_dev",
        "repository": "https://github.com/example/mcp.git",
        "commit": "main",
        "adapter": "npm",
        "entrypoint": {"executable": "example-mcp"}
    }));
    assert_eq!(
        parse_manifest(&bytes(&git)).unwrap_err().code(),
        McpPlatformErrorCode::ImmutableReferenceRequired
    );
}

#[test]
fn duplicate_selectors_transport_mismatch_traversal_and_templates_have_stable_codes() {
    let mut duplicate: Value = serde_json::from_str(NPM).unwrap();
    let artifact = duplicate["distribution"]["artifacts"][0].clone();
    duplicate["distribution"]["artifacts"]
        .as_array_mut()
        .unwrap()
        .push(artifact);
    assert_eq!(
        parse_manifest(&bytes(&duplicate)).unwrap_err().code(),
        McpPlatformErrorCode::DuplicateSelector
    );

    let mut mismatch: Value = serde_json::from_str(REMOTE).unwrap();
    mismatch["transport"] = json!({"type": "stdio"});
    assert_eq!(
        parse_manifest(&bytes(&mismatch)).unwrap_err().code(),
        McpPlatformErrorCode::TransportMismatch
    );

    let mut traversal: Value = serde_json::from_str(MANUAL).unwrap();
    traversal["distribution"]["entrypoint"]["cwd"] = json!("safe/../secret");
    assert_eq!(
        parse_manifest(&bytes(&traversal)).unwrap_err().code(),
        McpPlatformErrorCode::PathTraversal
    );

    let mut template: Value = serde_json::from_str(MANUAL).unwrap();
    template["distribution"]["entrypoint"]["args"][1] = json!("${shell.command}");
    assert_eq!(
        parse_manifest(&bytes(&template)).unwrap_err().code(),
        McpPlatformErrorCode::UnknownTemplateVariable
    );
}

#[test]
fn official_git_dev_is_denied_by_release_policy() {
    let git = manual_with_distribution(json!({
        "type": "git_dev",
        "repository": "https://github.com/example/mcp.git",
        "commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "adapter": "npm",
        "entrypoint": {"executable": "example-mcp"}
    }));
    let verified = parse_manifest(&bytes(&git)).unwrap();
    let context = phase_2a_context(TrustTier::Official);
    let decision = evaluate_manifest_policy(verified.manifest(), &context);
    assert_eq!(decision.outcome, PolicyOutcome::Deny);
    assert_eq!(
        decision.reasons[0].code,
        PolicyReasonCode::GitDevReleaseDenied
    );
    assert_eq!(
        plan_for_manifest(&verified, &context).unwrap_err().code(),
        McpPlatformErrorCode::PolicyDenied
    );
}

#[test]
fn unknown_distribution_adapter_is_denied_with_stable_reason() {
    let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
    let context =
        phase_2a_context(TrustTier::Official).with_known_adapters(std::iter::empty::<String>());
    let decision = evaluate_manifest_policy(verified.manifest(), &context);
    assert_eq!(decision.outcome, PolicyOutcome::Deny);
    assert_eq!(
        decision.reasons[0].code,
        PolicyReasonCode::UnknownDistributionAdapter
    );
}

#[test]
fn registration_plans_are_disabled_and_have_no_download_or_file_effects() {
    for (fixture, trust_tier) in [(REMOTE, TrustTier::Official), (MANUAL, TrustTier::Local)] {
        let verified = parse_manifest(fixture.as_bytes()).unwrap();
        let plan = plan_for_manifest(&verified, &phase_2a_context(trust_tier)).unwrap();
        assert!(!plan.default_enabled());
        assert!(!plan.effects().downloads_artifacts);
        assert!(!plan.effects().writes_files);
        assert!(!plan.effects().removes_files);
        assert_eq!(plan.manifest_digest(), verified.digest());
        assert_eq!(plan.plan_digest().len(), 64);
    }
}

#[test]
fn connection_projection_is_one_way_and_typed() {
    let remote = parse_manifest(REMOTE.as_bytes()).unwrap();
    let remote_plan = plan_for_manifest(&remote, &phase_2a_context(TrustTier::Official)).unwrap();
    assert!(matches!(
        remote_plan
            .connection_projection()
            .to_extension_config(),
        ExtensionConfig::StreamableHttp { ref uri, .. }
            if uri == "https://mcp.example.com/v1"
    ));

    let manual = parse_manifest(MANUAL.as_bytes()).unwrap();
    let manual_plan = plan_for_manifest(&manual, &phase_2a_context(TrustTier::Local)).unwrap();
    assert!(matches!(
        manual_plan
            .connection_projection()
            .to_extension_config(),
        ExtensionConfig::Stdio { ref cmd, ref args, .. }
            if cmd == "mcp-filesystem" && args == &["--root", "${user.workspace}"]
    ));
}

#[test]
fn api_key_header_projection_uses_braced_environment_placeholder() {
    let mut manifest: Value = serde_json::from_str(REMOTE).unwrap();
    manifest["auth"] = json!({
        "type": "api_key_header",
        "header_name": "Authorization",
        "prefix": "Bearer",
        "credential_name": "api-token"
    });
    let verified = parse_manifest(&bytes(&manifest)).unwrap();
    let plan = plan_for_manifest(&verified, &phase_2a_context(TrustTier::Official)).unwrap();
    let ExtensionConfig::StreamableHttp {
        envs,
        env_keys,
        headers,
        ..
    } = plan.connection_projection().to_extension_config()
    else {
        panic!("expected streamable HTTP projection");
    };

    assert!(envs.get_env().is_empty());
    assert_eq!(env_keys, vec!["API_TOKEN"]);
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer ${API_TOKEN}")
    );
}

#[test]
fn catalog_replay_is_idempotent_and_conflicting_content_is_rejected() {
    let mut catalog = VerifiedManifestCollection::default();
    assert_eq!(
        catalog
            .insert(
                "local-source",
                TrustTier::Local,
                ManifestProof::LocalBytes,
                REMOTE.as_bytes()
            )
            .unwrap(),
        CatalogInsertOutcome::Inserted
    );
    assert_eq!(
        catalog
            .insert(
                "local-source",
                TrustTier::Local,
                ManifestProof::LocalBytes,
                REMOTE.as_bytes()
            )
            .unwrap(),
        CatalogInsertOutcome::IdempotentReplay
    );
    assert_eq!(catalog.len(), 1);

    let mut changed: Value = serde_json::from_str(REMOTE).unwrap();
    changed["description"] = json!("Different content for the same identity.");
    assert_eq!(
        catalog
            .insert(
                "local-source",
                TrustTier::Local,
                ManifestProof::LocalBytes,
                &bytes(&changed)
            )
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::ManifestConflict
    );

    let target = CompatibilityTarget {
        platform: Platform::Windows,
        arch: Architecture::X86_64,
    };
    let entries = catalog.list(
        &CatalogFilter {
            query: Some("knowledge".to_string()),
            trust_tiers: vec![TrustTier::Local],
            compatibility: Some(CatalogCompatibility::Compatible),
        },
        target,
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id(), "com.example.knowledge-search");
    assert_eq!(entries[0].version(), "1.4.2");
    assert_eq!(entries[0].trust_tier(), TrustTier::Local);
}

#[test]
fn canonical_manifest_digest_ignores_json_formatting_and_changes_with_fields() {
    let compact: Value = serde_json::from_str(REMOTE).unwrap();
    let compact_bytes = serde_json::to_vec(&compact).unwrap();
    let pretty_bytes = serde_json::to_vec_pretty(&compact).unwrap();
    let first = parse_manifest(&compact_bytes).unwrap();
    let second = parse_manifest(&pretty_bytes).unwrap();
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.canonical_json(), second.canonical_json());

    let mut omitted_default = compact.clone();
    omitted_default["license"]
        .as_object_mut()
        .unwrap()
        .remove("notice_required");
    let omitted_default = parse_manifest(&bytes(&omitted_default)).unwrap();
    assert_eq!(first.digest(), omitted_default.digest());

    let mut changed = compact;
    changed["description"] = json!("A changed field changes the canonical digest.");
    let third = parse_manifest(&bytes(&changed)).unwrap();
    assert_ne!(first.digest(), third.digest());
}

#[test]
fn all_domain_state_dimensions_default_independently() {
    let state = goose::mcp_platform::ManagedMcpState::default();
    assert!(!state.default_enabled);
    assert!(state.session_enabled.is_empty());
    assert!(state.tool_policies.is_empty());
    assert_ne!(
        serde_json::to_value(state.registration).unwrap(),
        serde_json::to_value(state.installation).unwrap()
    );
}
