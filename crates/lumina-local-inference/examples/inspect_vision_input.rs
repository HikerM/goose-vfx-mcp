use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::InstallPlan;
use std::io::Read;

fn read_bounded(path: &str, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "Input exceeds its byte limit");
    Ok(bytes)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4,
        "Usage: inspect_vision_input plan-json image-png input-tokens output-tokens"
    );
    let plan: InstallPlan = serde_json::from_slice(&read_bounded(&args[0], 16 * 1024 * 1024)?)?;
    let admission = plan
        .admission
        .as_ref()
        .context("Plan has no frozen admission")?;
    admission.validate(&plan, admission.hardware.observed_at)?;
    let workload = &admission.envelope.workload;
    let vision = workload
        .vision
        .as_ref()
        .context("Plan has no vision envelope")?;
    vision.validate()?;
    let bytes = read_bounded(&args[1], vision.max_input_bytes)?;
    let images = vision.inspect_images(&[&bytes])?;
    workload.validate_vision_prompt(args[2].parse()?, args[3].parse()?)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "plan_digest": plan.digest()?,
            "image_sha256": images[0].sha256(),
            "image_bytes": images[0].bytes().len(),
            "allocation": images[0].allocation(),
            "scope": "Recorded input boundary diagnostic; supplied token counts are not authenticated Runtime preflight."
        }))?
    );
    Ok(())
}
