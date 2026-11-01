use anyhow::{ensure, Result};
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

fn prepare_native_text_plan(
    repository: &LifecycleRepository,
    catalog: &ModelCatalogSnapshot,
    observation: &ComponentObservation,
    binary: &Path,
    database: &Path,
) -> Result<serde_json::Value> {
    ensure!(
        observation.role == ComponentRole::Main
            && observation.files.len() == 1
            && observation.files[0]
                .artifact
                .source_sha256
                .as_ref()
                .is_some_and(|sha| sha.as_str()
                    == "c54f8b67069c70085b98440de696b44da8250250ac69a961b41133def876e262"),
        "This diagnostic profile is limited to the actually measured 0.8B baseline content"
    );
    let runtime = baseline_pairing_runtime()?;
    let mut file = std::fs::File::open(binary)?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "Runtime binary exceeds verification budget"
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    ensure!(
        Sha256Digest::parse(
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )? == runtime.binary_sha256,
        "Runtime binary differs from measured baseline"
    );
    let profile = RuntimeInstallProfile {
        id: "measured-qwen35-08b-text-cpu".into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: runtime.clone(),
        architectures: BTreeSet::from(["qwen35".into()]),
        capabilities: BTreeSet::from([ModelCapability::Text]),
        required_roles: BTreeSet::from([ComponentRole::Main]),
        evidence_ids: BTreeSet::from([
            "m1:2026-11-01-m1-native-resource-measurements.json:bounded-exact-workload".into(),
        ]),
    };
    let envelope = AdapterResourceEnvelope {runtime,component_digests:BTreeSet::from([observation.digest()?]),workload:ResourceWorkload {context_tokens:2048,batch_tokens:512,parallel_sequences:1,recurrent_rollback_slots:0,kv_key_type:ResourceCacheType::F16,kv_value_type:ResourceCacheType::F16,prompt_cache_bytes:0,context_checkpoint_slots:0},compute_buffer_bytes:64*1024*1024,auxiliary_buffer_bytes:256*1024*1024,evidence_id:"m1:native-resource-measurements:r2:37.02MiB-compute-0.95MiB-output-with-explicit-headroom".into()};
    let mut dependencies = DependencyGraph {
        root: catalog.components.keys().next().unwrap().clone(),
        components: catalog.components.clone(),
        edges: vec![],
    };
    for component in dependencies.components.values_mut() {
        component.compatibility = ComponentCompatibility::Verified;
        component
            .evidence_ids
            .extend(profile.evidence_ids.iter().cloned());
    }
    let request = ObservedPlanRequest {
        id: format!("plan:{}", uuid::Uuid::new_v4()),
        model_id: catalog.model.id.clone(),
        variant_id: catalog.variants.keys().next().unwrap().clone(),
        target_capabilities: BTreeSet::from([ModelCapability::Text]),
        dependencies,
    };
    let backend = LlamaBackend::init()?;
    let now = chrono::Utc::now().timestamp();
    let hardware = probe_hardware_resources(&backend, database.parent().unwrap(), now)?;
    let assessment =
        repository.prepare_observed_plan(&request, &profile, &envelope, &hardware, now)?;
    ensure!(
        assessment.blocking_reasons.is_empty(),
        "Actual baseline plan was blocked: {:?}",
        assessment.blocking_reasons
    );
    let plan = assessment
        .plan
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Baseline plan was not created"))?;
    let job = repository.enqueue(
        &format!("job:{}", uuid::Uuid::new_v4()),
        &format!("baseline-install:{}", plan.id),
        &plan.id,
        &plan.digest()?,
        now,
        hardware.disk_free_bytes,
    )?;
    ensure!(
        job.status == JobStatus::Queued && repository.read_plan(&plan.id)?.as_ref() == Some(plan),
        "Plan acceptance did not persist"
    );
    Ok(
        serde_json::json!({"profile":profile,"envelope":envelope,"hardware":hardware,"assessment":assessment,"queued_job":job,"scope":"Exact measured baseline content only; one immutable installation plan and queued job in a fresh diagnostic database. No download worker, installation, runtime adapter or product READY was created."}),
    )
}

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 2 || (arguments.len() == 4 && arguments[2] == "--prepare-native-text-plan"),
        "Usage: cache_recorded_component component-json new-database-path [--prepare-native-text-plan native-binary]"
    );
    ensure!(
        !Path::new(&arguments[1]).exists(),
        "Existing model database is preserved; use a fresh path"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&arguments[0])?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Component observation exceeds input budget"
    );
    let record: ComponentRecord = serde_json::from_slice(&bytes)?;
    let observation = record.component.observation;
    observation.validate()?;
    let primary = observation
        .files
        .iter()
        .find(|file| {
            file.header
                .shard
                .as_ref()
                .is_none_or(|shard| shard.index == 0)
        })
        .ok_or_else(|| anyhow::anyhow!("Missing primary GGUF header"))?;
    let classification = classify_gguf(
        &primary.artifact,
        &primary.artifact.snapshot_id,
        &primary.snapshot,
        &primary.header,
        &[],
    )?;
    let digest = observation.digest()?;
    let model_id = format!("observed-model:{}", digest.as_str());
    let variant_id = format!("observed-variant:{}", digest.as_str());
    let component_id = format!("observed-component:{}", digest.as_str());
    let artifacts = observation
        .files
        .iter()
        .map(|file| (file.artifact.id.clone(), file.artifact.clone()))
        .collect::<BTreeMap<_, _>>();
    let artifact_ids = artifacts.keys().cloned().collect::<BTreeSet<_>>();
    let snapshots = observation
        .files
        .iter()
        .map(|file| (file.artifact.snapshot_id.clone(), file.snapshot.clone()))
        .collect::<BTreeMap<_, _>>();
    let tensor_types = observation
        .files
        .iter()
        .flat_map(|file| file.header.tensors.iter().map(|tensor| tensor.type_id))
        .collect();
    let quantization_label = observation
        .files
        .iter()
        .flat_map(|file| {
            file.header
                .tensors
                .iter()
                .map(|tensor| tensor.quantization.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" + ");
    let catalog = ModelCatalogSnapshot {
        model: ModelDefinition {
            id: model_id.clone(),
            display_name: primary.snapshot.repository.as_str().into(),
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
                id: variant_id,
                model_id,
                artifact_ids: artifact_ids.clone(),
                quantization_label,
                tensor_types,
                evidence_ids: BTreeSet::from(["actual-header-observation".into()]),
            },
        )]),
        components: BTreeMap::from([(
            component_id.clone(),
            Component {
                id: component_id,
                role: observation.role,
                artifact_ids,
                compatibility: ComponentCompatibility::Unverified,
                evidence_ids: BTreeSet::new(),
            },
        )]),
    };
    let repository = LifecycleRepository::open(Path::new(&arguments[1]))?;
    repository.save_catalog(&catalog)?;
    let mut stored = Vec::new();
    for file in &observation.files {
        stored.push(repository.save_gguf_observation(file, chrono::Utc::now().timestamp())?);
    }
    drop(repository);
    let reopened = LifecycleRepository::open(Path::new(&arguments[1]))?;
    for record in &stored {
        ensure!(
            reopened.read_gguf_observation(&record.id)?.as_ref() == Some(record),
            "Reopened observation differs"
        );
    }
    let plan_preparation = if arguments.len() == 4 {
        Some(prepare_native_text_plan(
            &reopened,
            &catalog,
            &observation,
            Path::new(&arguments[3]),
            Path::new(&arguments[1]),
        )?)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"database_schema_version":MODEL_DATABASE_SCHEMA_VERSION,"observation_ids":stored.iter().map(|record|&record.id).collect::<Vec<_>>(),"records_reopened":stored.len(),"plan_preparation":plan_preparation,"scope":"Actual source/header persistence; optional exact-baseline plan acceptance is reported separately. No installed version or runtime capability was created."})
        )?
    );
    Ok(())
}
