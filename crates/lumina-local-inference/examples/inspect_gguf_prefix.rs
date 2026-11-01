use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{inspect_gguf, GgufReadLimits};
use std::io::BufReader;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "Usage: inspect_gguf_prefix <prefix-file> <advertised-file-bytes> <output-json>"
    );
    let prefix = PathBuf::from(&args[0]);
    let file_bytes: u64 = args[1]
        .to_str()
        .context("File length is not UTF-8")?
        .parse()?;
    let output = PathBuf::from(&args[2]);
    let file = std::fs::File::open(&prefix)?;
    ensure!(
        file.metadata()?.len() <= file_bytes,
        "Prefix exceeds advertised source length"
    );
    let inspection = inspect_gguf(BufReader::new(file), file_bytes, GgufReadLimits::default())?;
    std::fs::write(output, serde_json::to_vec_pretty(&inspection)?)?;
    println!(
        "architecture={:?}, tensors={}, header_bytes={}",
        inspection.architecture(),
        inspection.tensors.len(),
        inspection.header_bytes
    );
    Ok(())
}
