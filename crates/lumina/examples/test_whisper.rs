use anyhow::{ensure, Result};
use lumina::dictation::whisper::{get_model, WhisperTranscriber};
use std::path::PathBuf;

const WHISPER_TOKENIZER_JSON: &str = include_str!("../src/dictation/whisper_data/tokens.json");

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        arguments.len() == 3,
        "Usage: test_whisper model-id model.gguf input-audio.wav"
    );
    let model_id = arguments[0]
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Whisper model ID is not UTF-8"))?;
    let model =
        get_model(model_id).ok_or_else(|| anyhow::anyhow!("Model {} not found", model_id))?;
    let model_path = PathBuf::from(&arguments[1]);
    let audio_path = PathBuf::from(&arguments[2]);
    ensure!(model_path.is_file(), "Whisper model file does not exist");
    ensure!(audio_path.is_file(), "Input audio file does not exist");
    let mut transcriber =
        WhisperTranscriber::new_with_tokenizer(model_id, &model_path, WHISPER_TOKENIZER_JSON)?;
    let audio_data = std::fs::read(audio_path)?;
    let text = transcriber.transcribe(&audio_data)?;
    ensure!(!text.trim().is_empty(), "Actual transcription is empty");
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({"model": model.id, "transcription": text}))?
    );
    Ok(())
}
