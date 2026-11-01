use anyhow::{ensure, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use lumina_local_inference::lifecycle::*;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

#[derive(Deserialize)]
struct ComponentRecord {
    component: ResolvedGgufComponent,
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(args.len() == 6, "Usage: assess_recorded_resources component-json storage-directory context batch compute-bytes auxiliary-bytes");
    let mut bytes = Vec::new();
    std::fs::File::open(&args[0])?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Component record exceeds budget"
    );
    let record: ComponentRecord = serde_json::from_slice(&bytes)?;
    let backend = LlamaBackend::init()?;
    let hardware = probe_hardware_resources(
        &backend,
        Path::new(&args[1]),
        chrono::Utc::now().timestamp(),
    )?;
    let envelope = AdapterResourceEnvelope {
        runtime: baseline_pairing_runtime()?,
        component_digests: BTreeSet::from([record.component.observation.digest()?]),
        workload: ResourceWorkload {
            context_tokens: args[2].parse()?,
            batch_tokens: args[3].parse()?,
            parallel_sequences: 1,
            recurrent_rollback_slots: 0,
            kv_key_type: ResourceCacheType::F16,
            kv_value_type: ResourceCacheType::F16,
            prompt_cache_bytes: 0,
            context_checkpoint_slots: 0,
        },
        compute_buffer_bytes: args[4].parse()?,
        auxiliary_buffer_bytes: args[5].parse()?,
        evidence_id: "diagnostic-explicit-buffer-scenario-not-a-verified-runtime-profile".into(),
    };
    let budget = assess_resource_budget(&[record.component.observation], &envelope, &hardware)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"hardware":hardware,"envelope":envelope,"budget":budget,"scope":"Actual source headers and native hardware; explicit buffer scenario only, no measured compute envelope, installation admission, runtime acceptance or product READY"})
        )?
    );
    Ok(())
}
