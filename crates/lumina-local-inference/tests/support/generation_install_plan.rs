use super::*;

fn selected_weights(profile: GenerationProfile) -> BTreeMap<ComponentRole, ComponentObservation> {
    let mut selected = BTreeMap::new();
    let assets = profile.recorded_assets();
    for source in profile.weight_sources() {
        let (sha, size) = &assets[&source.role];
        let snapshot = RepositorySnapshot {
            repository: ModelScopeRepository::parse(source.repository.as_str()).unwrap(),
            requested_revision: source.revision.clone(),
            resolved_revision: source.revision.clone(),
            files_digest: Sha256Digest::of_bytes(source.repository.as_bytes()),
            fetched_at: 10,
        };
        let path = RepositoryPath::parse(source.file.as_str()).unwrap();
        let snapshot_id = snapshot.content_digest().unwrap().as_str().to_owned();
        let artifact = Artifact {
            id: modelscope::source_artifact_id(&snapshot_id, &path, FilePurpose::GgufWeight)
                .unwrap(),
            snapshot_id,
            path,
            size_bytes: Some(*size),
            source_sha256: Some(sha.clone()),
            purpose: FilePurpose::GgufWeight,
        };
        let role = source.component_role();
        let header = GgufInspection {
            version: 3,
            byte_order: GgufByteOrder::LittleEndian,
            file_bytes: *size,
            header_bytes: 96,
            header_digest: Sha256Digest::of_bytes(source.file.as_bytes()),
            data_offset: 96,
            alignment: 32,
            metadata: BTreeMap::new(),
            tensors: vec![GgufTensor {
                name: "fixture.weight".into(),
                dimensions: vec![1],
                type_id: 0,
                quantization: "F32".into(),
                relative_offset: 0,
                encoded_bytes: 4,
                elements: 1,
            }],
            stored_tensor_elements: 1,
            encoded_tensor_bytes: 4,
            shard: None,
        };
        selected.insert(
            role,
            ComponentObservation {
                role,
                files: vec![SourceGgufObservation {
                    artifact,
                    snapshot,
                    header,
                }],
            },
        );
    }
    selected
}

#[test]
fn generation_plans_bind_exact_sources_roles_and_shared_h3_installation() {
    let directory = tempfile::tempdir().unwrap();
    let repository = LifecycleRepository::open(&directory.path().join("models.db")).unwrap();
    for profile in GenerationProfile::registered() {
        let selection = selected_weights(profile);
        let plan = freeze_plan(&repository, profile, selection.clone(), u64::MAX, 10).unwrap();
        assert_eq!(repository.read_plan(&plan.id).unwrap().unwrap(), plan);
        assert_eq!(plan.target_capabilities, profile.required_capabilities());
        assert_eq!(plan.artifacts.len(), profile.weight_sources().len());
        assert_eq!(plan.dependencies.components.len(), selection.len());
        let weight_bytes: u64 = plan.artifacts.values().map(|a| a.size_bytes.unwrap()).sum();
        let mut historical = plan.clone();
        historical.rules_version = LEGACY_GENERATION_INSTALL_RULES_VERSION.into();
        historical.required_disk_bytes = weight_bytes * 3 + 16 * 1024 * 1024;
        historical
            .validate_for_install(11, historical.required_disk_bytes)
            .unwrap();
        if cfg!(windows) {
            assert_eq!(
                plan.required_disk_bytes,
                weight_bytes * 2 + 16 * 1024 * 1024
            );
            historical.required_disk_bytes = plan.required_disk_bytes;
            assert!(historical.validate_for_install(11, u64::MAX).is_err());
        }
        assert!(plan
            .validate_for_install(11, plan.required_disk_bytes - 1)
            .is_err());
        let mut previous_schema = plan.clone();
        let root = previous_schema
            .dependencies
            .components
            .get_mut(&plan.dependencies.root)
            .unwrap();
        root.evidence_ids.remove(profile.evidence());
        root.evidence_ids
            .insert("another model parameter contract".into());
        assert!(previous_schema.validate_for_install(11, u64::MAX).is_err());
        let mut forged = plan.clone();
        forged.artifacts.values_mut().next().unwrap().source_sha256 =
            Some(Sha256Digest::of_bytes(b"replaced"));
        assert!(forged.validate_for_install(11, u64::MAX).is_err());
        if profile == GenerationProfile::H3CudaTextToVideoAudio {
            assert_eq!(plan.snapshots.len(), 2);
            assert_eq!(plan.dependencies.edges.len(), 3);
            let from_image = freeze_plan(
                &repository,
                GenerationProfile::H3CudaFirstFrameToVideoAudio,
                selection,
                u64::MAX,
                11,
            )
            .unwrap();
            assert_eq!(plan.model_id, from_image.model_id);
            assert_eq!(plan.dependencies, from_image.dependencies);
            let mut forged = plan;
            forged.dependencies.edges[0].requirement = DependencyRequirement::Optional;
            assert!(forged.validate_for_install(11, u64::MAX).is_err());
        }
    }
}
