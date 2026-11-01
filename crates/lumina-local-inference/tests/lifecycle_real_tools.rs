#![cfg(windows)]

use lumina_local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelStorage, ModelSettings, SamplingConfig,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn file_sha256(path: &Path) -> String {
    use std::fmt::Write;
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut hex, "{byte:02x}").unwrap();
    }
    hex
}

#[test]
#[ignore = "requires an actual rebuilt CLI, GGUF and isolated LUMINA_PATH_ROOT; performs real shell calls"]
fn actual_cli_tool_roundtrip_requires_matching_request_response_and_output() {
    let root =
        PathBuf::from(std::env::var("LUMINA_PATH_ROOT").expect("set an isolated application root"));
    let cli = PathBuf::from(
        std::env::var("LUMINA_REAL_CLI_PATH").expect("set the actual rebuilt CLI path"),
    );
    let path =
        PathBuf::from(std::env::var("LUMINA_REAL_GGUF_PATH").expect("set an actual GGUF path"));
    assert!(root.is_absolute() && cli.is_absolute() && path.is_absolute());
    let model_id = std::env::var("LUMINA_REAL_MODEL_ID").unwrap();
    let repo_id = std::env::var("LUMINA_REAL_REPO_ID").unwrap();
    let quantization = model_id.rsplit_once(':').unwrap().1.to_string();
    get_registry()
        .lock()
        .unwrap()
        .add_model(LocalModelEntry {
            id: model_id.clone(),
            repo_id,
            filename: path.file_name().unwrap().to_string_lossy().to_string(),
            quantization,
            size_bytes: std::fs::metadata(&path).unwrap().len(),
            local_path: path.clone(),
            source_url: String::new(),
            backend_id: Some("llamacpp".to_string()),
            storage: LocalModelStorage::ManualPath,
            settings: ModelSettings {
                context_size: Some(8192),
                max_output_tokens: Some(512),
                n_gpu_layers: Some(0),
                n_threads: Some(4),
                sampling: SamplingConfig::Greedy,
                enable_thinking: false,
                ..ModelSettings::default()
            },
            mmproj_path: None,
            mmproj_source_url: None,
            mmproj_size_bytes: 0,
            mmproj_checked: false,
            shard_files: vec![],
        })
        .unwrap();
    let marker = format!("lumina-tool-{}", uuid::Uuid::new_v4().simple());
    let command = format!(
        "powershell.exe -NoProfile -Command \"Write-Output '{marker}'; Write-Output (41 + 17)\""
    );
    let prompt = format!(
        "The developer shell runs cmd.exe on Windows. Call its shell tool exactly once with this exact command: {command}\n\
         Read the actual tool response, then report its marker and numeric result. Do not edit files, delegate, create worktrees or run other commands."
    );
    let binary_digest = file_sha256(&cli);
    let model_digest = file_sha256(&path);
    let output = std::process::Command::new(&cli)
        .args([
            "run",
            "--provider",
            "local",
            "--model",
            &model_id,
            "--no-session",
            "--no-profile",
            "--with-builtin",
            "developer",
            "--max-turns",
            "3",
            "--output-format",
            "stream-json",
            "--text",
            &prompt,
        ])
        .output()
        .unwrap();
    let evidence = root.join("evidence");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("cli-tools.stdout.jsonl"), &output.stdout).unwrap();
    std::fs::write(evidence.join("cli-tools.stderr.txt"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "actual CLI failed; inspect persisted stderr"
    );
    assert_eq!(
        file_sha256(&cli),
        binary_digest,
        "CLI changed during validation"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut requests = BTreeMap::new();
    let mut matched = Vec::new();
    let mut text = String::new();
    let mut complete = false;
    for line in stdout.lines().filter(|line| line.starts_with('{')) {
        let event: Value = serde_json::from_str(line).expect("invalid CLI structured event");
        complete |= event["type"] == "complete";
        let Some(content) = event["message"]["content"].as_array() else {
            continue;
        };
        for item in content {
            match item["type"].as_str() {
                Some("toolRequest")
                    if item["_meta"]["lumina_extension"] == "developer"
                        && item["toolCall"]["value"]["name"] == "shell" =>
                {
                    requests.insert(item["id"].as_str().unwrap().to_string(), item.clone());
                }
                Some("toolResponse") => {
                    let Some(id) = item["id"].as_str() else {
                        continue;
                    };
                    let result = &item["toolResult"]["value"];
                    if item["toolResult"]["status"] != "success"
                        || result["isError"] == true
                        || result["structuredContent"]["exit_code"] != 0
                    {
                        continue;
                    }
                    let observed = result["structuredContent"]["stdout"].as_str().unwrap_or("");
                    if let Some(request) = requests.get(id) {
                        if observed.lines().any(|line| line.trim() == marker)
                            && observed.lines().any(|line| line.trim() == "58")
                        {
                            matched.push(json!({"request":request,"response":item}));
                        }
                    }
                }
                Some("text") => text.push_str(item["text"].as_str().unwrap_or("")),
                _ => {}
            }
        }
    }
    assert!(complete, "CLI did not complete its structured stream");
    assert_eq!(
        requests.len(),
        1,
        "expected exactly the requested shell command"
    );
    assert_eq!(
        requests.values().next().unwrap()["toolCall"]["value"]["arguments"]["command"],
        command,
        "model changed the requested PowerShell command"
    );
    assert_eq!(matched.len(), 1, "expected a genuine shell response with both challenge marker and computed 58; inspect persisted JSONL");
    assert!(
        text.contains(&marker) && text.contains("58"),
        "assistant did not use the actual shell result"
    );
    std::fs::write(evidence.join("cli-tool-smoke.json"), serde_json::to_vec_pretty(&json!({
        "suite":"existing-cli-tool-smoke-v1", "modelId":model_id,
        "modelSha256":model_digest,"runtimeBinarySha256":binary_digest,
        "execution":"cpu","challengeMarker":marker,"actualToolRoundtrip":matched,
        "assistantReply":text,"result":"PASS",
        "scope":"existing CLI tool roundtrip; not lifecycle installation or complete capability profile acceptance"
    })).unwrap()).unwrap();
    println!("PASS: actual shell request, matching response, challenge marker and computed answer");
}
