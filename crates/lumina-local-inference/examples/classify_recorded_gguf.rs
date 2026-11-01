use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::{RemoteGgufInspection, RepositoryInspection};
use serde::{de::DeserializeOwned, Deserialize};
use std::io::Read;

#[derive(Deserialize)]
struct SourceRecord {
    inspection: RepositoryInspection,
}

#[derive(Deserialize)]
struct HeaderRecord {
    remote: RemoteGgufInspection,
}

fn read_record<T: DeserializeOwned>(path: &str) -> Result<T> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Recorded GGUF observation exceeds byte budget"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 2,
        "Usage: classify_recorded_gguf provider-inspection-json remote-gguf-json"
    );
    let source: SourceRecord = read_record(&arguments[0])?;
    source.inspection.validate()?;
    let header: HeaderRecord = read_record(&arguments[1])?;
    let result = source
        .inspection
        .inventory
        .classify_observation(&header.remote)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
