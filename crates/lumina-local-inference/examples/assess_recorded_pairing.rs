use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::*;
use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize)]
struct SourceRecord {
    inspection: RepositoryInspection,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Recorded pairing input exceeds byte budget"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn observed(
    source: &str,
    path: &str,
    header: &str,
    role: ComponentRole,
) -> Result<ComponentObservation> {
    let source: SourceRecord = read_json(source)?;
    source.inspection.validate()?;
    let header: GgufInspection = read_json(header)?;
    let artifact = source
        .inspection
        .inventory
        .artifact(&RepositoryPath::parse(path)?, FilePurpose::GgufWeight)?;
    Ok(ComponentObservation {
        role,
        files: vec![SourceGgufObservation {
            artifact,
            snapshot: source.inspection.inventory.snapshot,
            header,
        }],
    })
}

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(arguments.len()==7,"Usage: assess_recorded_pairing profile-id main-source main-path main-header dependency-source dependency-path dependency-header");
    let profile = baseline_pairing_profiles()?
        .into_iter()
        .find(|profile| profile.id == arguments[0])
        .ok_or_else(|| anyhow::anyhow!("Unknown registered pairing profile"))?;
    let main = observed(
        &arguments[1],
        &arguments[2],
        &arguments[3],
        ComponentRole::Main,
    )?;
    let dependency = observed(
        &arguments[4],
        &arguments[5],
        &arguments[6],
        profile.dependency_role,
    )?;
    let decision =
        assess_component_pairing(&main, &dependency, &baseline_pairing_runtime()?, &profile)?;
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"profile":profile,"decision":decision,"scope":"Pinned source/header pairing and recorded provenance only; no product installation or new runtime acceptance"})
        )?
    );
    Ok(())
}
