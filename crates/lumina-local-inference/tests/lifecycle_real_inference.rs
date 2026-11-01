use futures::StreamExt;
use lumina_local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelStorage, ModelSettings, SamplingConfig,
};
use lumina_local_inference::LocalInferenceProvider;
use lumina_provider_types::base::Provider;
use lumina_provider_types::conversation::message::Message;
use lumina_provider_types::model::ModelConfig;
use std::path::PathBuf;

#[tokio::test]
#[ignore = "requires an actual GGUF model and an isolated LUMINA_PATH_ROOT"]
async fn installed_gguf_generates_actual_text() {
    let root =
        PathBuf::from(std::env::var("LUMINA_PATH_ROOT").expect("set an isolated application root"));
    assert!(root.is_absolute());
    let path =
        PathBuf::from(std::env::var("LUMINA_REAL_GGUF_PATH").expect("set an actual GGUF file"));
    let model_id = std::env::var("LUMINA_REAL_MODEL_ID").expect("set the actual model ID");
    let quantization = model_id
        .rsplit_once(':')
        .expect("model ID must include actual quantization")
        .1
        .to_string();
    let repo_id =
        std::env::var("LUMINA_REAL_REPO_ID").expect("set the actual ModelScope repository");
    let metadata = std::fs::metadata(&path).unwrap();
    assert!(metadata.is_file() && metadata.len() > 0);
    get_registry()
        .lock()
        .unwrap()
        .add_model(LocalModelEntry {
            id: model_id.clone(),
            repo_id,
            filename: path.file_name().unwrap().to_string_lossy().to_string(),
            quantization,
            local_path: path,
            source_url: String::new(),
            backend_id: Some("llamacpp".to_string()),
            storage: LocalModelStorage::ManualPath,
            settings: ModelSettings {
                context_size: Some(1024),
                max_output_tokens: Some(32),
                n_gpu_layers: Some(0),
                n_threads: Some(4),
                sampling: SamplingConfig::Greedy,
                enable_thinking: false,
                ..ModelSettings::default()
            },
            size_bytes: metadata.len(),
            mmproj_path: None,
            mmproj_source_url: None,
            mmproj_size_bytes: 0,
            mmproj_checked: false,
            shard_files: vec![],
        })
        .unwrap();
    let provider = LocalInferenceProvider::from_env().await.unwrap();
    let config = ModelConfig::new(model_id).with_max_tokens(Some(32));
    let messages = [Message::user().with_text("Compute 2 + 2. Respond only with the numeral.")];
    let mut stream = provider
        .stream(&config, "Be accurate and concise.", &messages, &[])
        .await
        .unwrap();
    let mut output = String::new();
    let mut generated_tokens = 0;
    while let Some(item) = stream.next().await {
        let (message, usage) = item.unwrap();
        if let Some(message) = message {
            output.push_str(&message.as_concat_text());
        }
        if let Some(usage) = usage {
            generated_tokens += usage.usage.output_tokens.unwrap_or(0);
        }
    }
    assert_eq!(output.trim(), "4", "actual inference result: {output}");
    assert!(generated_tokens > 0);
    std::fs::create_dir_all(root.join("evidence")).unwrap();
    std::fs::write(
        root.join("evidence/text-smoke.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "test": "installed_gguf_generates_actual_text",
            "input": messages[0].as_concat_text(),
            "output": output,
            "outputTokens": generated_tokens,
            "execution": "cpu",
            "scope": "existing runtime text smoke, not lifecycle installation acceptance"
        }))
        .unwrap(),
    )
    .unwrap();
}
