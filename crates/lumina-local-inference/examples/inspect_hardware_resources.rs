use anyhow::{ensure, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use lumina_local_inference::lifecycle::probe_hardware_resources;
use std::path::Path;

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 1,
        "Usage: inspect_hardware_resources existing-storage-directory"
    );
    let backend = LlamaBackend::init()?;
    let snapshot = probe_hardware_resources(
        &backend,
        Path::new(&arguments[0]),
        chrono::Utc::now().timestamp(),
    )?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
