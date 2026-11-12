use anyhow::{ensure, Context, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use lumina_local_inference::lifecycle::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

#[derive(Deserialize)]
struct ComponentRecord {
    component: ResolvedGgufComponent,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Recorded input exceeds its byte budget"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn check_runtime(binary: &Path, runtime: &RuntimeBuild) -> Result<()> {
    let mut file = std::fs::File::open(binary)?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "Runtime verification budget exceeded"
    );
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    ensure!(
        Sha256Digest::parse(
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )? == runtime.binary_sha256,
        "Runtime binary differs from the measured vision build"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 5 || (args.len() == 6 && args[5] == "--reuse-database"),
        "Usage: prepare_recorded_vision_plan main-json projector-json database native-binary installation-input [--reuse-database]"
    );
    let database = Path::new(&args[2]);
    ensure!(
        !database.exists() || args.len() == 6,
        "Existing database requires --reuse-database"
    );
    let mut main: ComponentObservation =
        read_json::<ComponentRecord>(Path::new(&args[0]), 16 * 1024 * 1024)?
            .component
            .observation;
    let mut projector: ComponentObservation =
        read_json::<ComponentRecord>(Path::new(&args[1]), 16 * 1024 * 1024)?
            .component
            .observation;
    main.role = ComponentRole::Main;
    projector.role = ComponentRole::VisionProjector;
    main.validate()?;
    projector.validate()?;
    let selected = [main, projector];
    let envelope = baseline_vision_resource_envelope(&selected)?;
    check_runtime(Path::new(&args[3]), &envelope.runtime)?;
    let pairing_profile = baseline_pairing_profiles()?
        .into_iter()
        .find(|profile| profile.id == "qwen35-08b-vision-content-baseline")
        .context("Missing registered vision pairing profile")?;
    let pairing = assess_component_pairing(
        &selected[0],
        &selected[1],
        &envelope.runtime,
        &pairing_profile,
    )?;
    ensure!(
        pairing.compatibility == ComponentCompatibility::Verified,
        "Vision pairing is not verified"
    );
    let ids = selected
        .iter()
        .map(|component| {
            Ok(format!(
                "observed-component:{}",
                component.digest()?.as_str()
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let identity = Sha256Digest::of_serializable(&ids)?;
    let model_id = format!("observed-vision-model:{}", identity.as_str());
    let variant_id = format!("observed-vision-variant:{}", identity.as_str());
    let mut artifacts = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut components = BTreeMap::new();
    for (index, observation) in selected.iter().enumerate() {
        for file in &observation.files {
            artifacts.insert(file.artifact.id.clone(), file.artifact.clone());
            snapshots.insert(file.artifact.snapshot_id.clone(), file.snapshot.clone());
        }
        components.insert(
            ids[index].clone(),
            Component {
                id: ids[index].clone(),
                role: observation.role,
                artifact_ids: observation
                    .files
                    .iter()
                    .map(|file| file.artifact.id.clone())
                    .collect(),
                compatibility: pairing.compatibility,
                evidence_ids: BTreeSet::from([
                    format!("component-pairing:{}", pairing.digest()?.as_str()),
                    VISION_RESOURCE_EVIDENCE.into(),
                ]),
            },
        );
    }
    let primary = &selected[0].files[0];
    let classification = classify_gguf(
        &primary.artifact,
        &primary.artifact.snapshot_id,
        &primary.snapshot,
        &primary.header,
        &[],
    )?;
    let catalog = ModelCatalogSnapshot {
        model: ModelDefinition {
            id: model_id.clone(),
            display_name: format!("{} (vision)", primary.snapshot.repository.as_str()),
            snapshot_ids: snapshots.keys().cloned().collect(),
            architecture: classification.architecture,
            structure: classification.structure,
            task_families: classification.task_candidates,
            declared_capabilities: BTreeSet::new(),
        },
        snapshots,
        artifacts,
        variants: BTreeMap::from([(
            variant_id.clone(),
            ModelVariant {
                id: variant_id.clone(),
                model_id: model_id.clone(),
                artifact_ids: selected[0]
                    .files
                    .iter()
                    .map(|file| file.artifact.id.clone())
                    .collect(),
                quantization_label: "Q8_0 main with F16 projector".into(),
                tensor_types: primary
                    .header
                    .tensors
                    .iter()
                    .map(|tensor| tensor.type_id)
                    .collect(),
                evidence_ids: BTreeSet::from([format!(
                    "component-pairing:{}",
                    pairing.digest()?.as_str()
                )]),
            },
        )]),
        components,
    };
    let profile = RuntimeInstallProfile {
        id: "measured-qwen35-08b-vision-cpu".into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: envelope.runtime.clone(),
        architectures: BTreeSet::from(["qwen35".into()]),
        capabilities: BTreeSet::from([ModelCapability::Text, ModelCapability::Vision]),
        required_roles: BTreeSet::from([ComponentRole::Main, ComponentRole::VisionProjector]),
        evidence_ids: BTreeSet::from([VISION_RESOURCE_EVIDENCE.into()]),
    };
    let request = ObservedPlanRequest {
        id: format!("vision-plan:{}", uuid::Uuid::new_v4()),
        model_id,
        variant_id,
        target_capabilities: profile.capabilities.clone(),
        dependencies: DependencyGraph {
            root: ids[0].clone(),
            components: catalog.components.clone(),
            edges: vec![DependencyEdge {
                from: ids[0].clone(),
                to: ids[1].clone(),
                requirement: DependencyRequirement::Required,
            }],
        },
    };
    let input: InstallationInput = read_json(Path::new(&args[4]), 64 * 1024)?;
    input.validate()?;
    let store = InstallationStore::open(&input.store_root)?;
    let input = InstallationInput::new(
        &store,
        input.installation_id,
        input.version_id,
        input.source,
    )?;
    let repository = LifecycleRepository::open(database)?;
    repository.save_catalog(&catalog)?;
    for component in &selected {
        for file in &component.files {
            repository.save_gguf_observation(file, chrono::Utc::now().timestamp())?;
        }
    }
    let backend = LlamaBackend::init()?;
    let now = chrono::Utc::now().timestamp();
    let hardware = probe_hardware_resources(&backend, &input.store_root, now)?;
    let assessment =
        repository.prepare_observed_plan(&request, &profile, &envelope, &hardware, now)?;
    ensure!(
        assessment.blocking_reasons.is_empty(),
        "Vision plan blocked: {:?}",
        assessment.blocking_reasons
    );
    let plan = assessment
        .plan
        .as_ref()
        .context("Vision plan was not prepared")?;
    let acceptance = JobAcceptance {
        id: format!("vision-job:{}", uuid::Uuid::new_v4()),
        idempotency_key: format!("vision-install:{}", plan.id),
        plan_id: plan.id.clone(),
        plan_digest: plan.digest()?,
    };
    let job =
        repository.enqueue_installation(&acceptance, &input, now, hardware.disk_free_bytes)?;
    ensure!(
        job.status == JobStatus::Queued,
        "Vision task was not queued"
    );
    drop(repository);
    let reopened = LifecycleRepository::open(database)?;
    ensure!(
        reopened.read_plan(&plan.id)?.as_ref() == Some(plan),
        "Reopened vision plan differs"
    );
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "assessment":assessment,"queued_job":job,"pairing":pairing,"profile":profile,
            "installation_input":input,
            "scope":"Source/header pairing, bounded measured CPU workload and frozen installation admission. This does not create Runtime or capability READY evidence."
        }))?
    );
    Ok(())
}
