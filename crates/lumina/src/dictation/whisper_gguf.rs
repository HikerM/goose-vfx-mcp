use anyhow::{bail, ensure, Context, Result};
use candle_core::quantized::gguf_file::{self, Content, Value};
use candle_core::Device;
use candle_transformers::models::whisper::Config;
use candle_transformers::quantized_var_builder::VarBuilder;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Cursor, Read};
use std::path::Path;
use tokenizers::Tokenizer;

use crate::providers::local_inference::lifecycle::{
    inspect_gguf, GgufInspection, GgufReadLimits, GgufTensor, Sha256Digest,
};

pub(super) const LEGACY_LAYOUT: &str = "whisper-candle-names-v1";
pub(super) const MODELSCOPE_LAYOUT: &str = "whisper-modelscope-base-enc-dec-v1";
const BASE_SHA256: &str = "81c069428bc8a24551a8169cf31cf09bcfd9d4cf50389ae281323c9aa9648c81";
const BASE_BYTES: u64 = 84_962_880;
const BUNDLED_TOKENIZER: &str = include_str!("whisper_data/tokens.json");

pub(super) enum WhisperWeights {
    Legacy(VarBuilder),
    ModelScopeBase {
        builder: VarBuilder,
        tokenizer: Box<Tokenizer>,
        suppress_tokens: Vec<u32>,
    },
}

fn read_limits() -> GgufReadLimits {
    GgufReadLimits {
        max_header_bytes: 8 * 1024 * 1024,
        max_metadata_entries: 512,
        max_tensors: 4096,
        max_array_elements: 100_000,
        max_string_bytes: 1024 * 1024,
        ..GgufReadLimits::default()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Layout {
    Candle,
    NativeBase,
}

fn layout(tensors: &[GgufTensor]) -> Result<Layout> {
    let has = |name| tensors.iter().any(|tensor| tensor.name == name);
    let native = tensors
        .iter()
        .any(|tensor| tensor.name.starts_with("enc.") || tensor.name.starts_with("dec."));
    let candle = tensors
        .iter()
        .any(|tensor| tensor.name.starts_with("model."));
    ensure!(
        !(native && candle),
        "Whisper GGUF mixes incompatible tensor layouts"
    );
    if candle && has("model.encoder.conv1.weight") && has("model.decoder.embed_tokens.weight") {
        Ok(Layout::Candle)
    } else if native && has("enc.conv.0.weight") && has("dec.token_embd.weight") {
        Ok(Layout::NativeBase)
    } else {
        bail!("Unsupported Whisper GGUF tensor layout")
    }
}

pub(super) fn load(
    path: &Path,
    model_id: &str,
    config: &Config,
    tokenizer_json: &str,
    device: &Device,
) -> Result<WhisperWeights> {
    let inspection = GgufInspection::inspect_file(path, read_limits())?;
    if layout(&inspection.tensors)? == Layout::Candle {
        return Ok(WhisperWeights::Legacy(VarBuilder::from_gguf(path, device)?));
    }
    ensure!(
        model_id == "base",
        "Native Whisper layout is supported only for the fixed Base profile"
    );
    ensure!(
        inspection.file_bytes == BASE_BYTES,
        "Native Whisper content size differs from the fixed Base profile"
    );
    let mut source_file = std::fs::File::open(path)?;
    let mut bytes = vec![0u8; BASE_BYTES as usize];
    source_file.read_exact(&mut bytes)?;
    ensure!(
        source_file.read(&mut [0u8; 1])? == 0
            && Sha256Digest::of_bytes(&bytes).as_str() == BASE_SHA256,
        "Native Whisper content digest differs from the fixed ModelScope Base profile"
    );

    // Validation and conversion use the same frozen bytes, never a rewritten source file.
    let inspection = inspect_gguf(Cursor::new(&bytes), BASE_BYTES, read_limits())?;
    let mappings = validate_tensors(&inspection.tensors)?;
    let mut source = Cursor::new(&bytes);
    let content = Content::read(&mut source)?;
    let suppress_tokens = validate_metadata(&content.metadata, config)?;
    let tokenizer = validate_tokenizer(&content.metadata, tokenizer_json)?;
    let remapped = {
        let tensors = mappings
            .iter()
            .map(|mapping| {
                content
                    .tensor(&mut source, &mapping.source, &Device::Cpu)
                    .map(|tensor| (mapping.target.as_str(), tensor))
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let tensor_refs: Vec<_> = tensors
            .iter()
            .map(|(name, tensor)| (*name, tensor))
            .collect();
        let mut metadata: Vec<_> = content
            .metadata
            .iter()
            .map(|(name, value)| (name.as_str(), value))
            .collect();
        metadata.sort_unstable_by_key(|(name, _)| *name);
        let mut buffer = Cursor::new(Vec::new());
        gguf_file::write(&mut buffer, &metadata, &tensor_refs)?;
        buffer.into_inner()
    };
    drop(content);
    drop(bytes);
    let builder = VarBuilder::from_gguf_buffer(&remapped, device)?;
    Ok(WhisperWeights::ModelScopeBase {
        builder,
        tokenizer: Box::new(tokenizer),
        suppress_tokens,
    })
}

#[derive(Debug)]
struct TensorMapping {
    source: String,
    target: String,
    shape: Vec<usize>,
    type_id: u32,
}

fn expected_tensors() -> Vec<TensorMapping> {
    let mut mappings = Vec::with_capacity(244);
    let mut add = |source: String, target: String, shape: &[usize], type_id| {
        mappings.push(TensorMapping {
            source,
            target,
            shape: shape.to_vec(),
            type_id,
        });
    };
    for (source, target, shape) in [
        (
            "enc.conv.0.weight",
            "model.encoder.conv1.weight",
            vec![512, 80, 3],
        ),
        ("enc.conv.0.bias", "model.encoder.conv1.bias", vec![512]),
        (
            "enc.conv.1.weight",
            "model.encoder.conv2.weight",
            vec![512, 512, 3],
        ),
        ("enc.conv.1.bias", "model.encoder.conv2.bias", vec![512]),
        (
            "enc.final_norm.weight",
            "model.encoder.layer_norm.weight",
            vec![512],
        ),
        (
            "enc.final_norm.bias",
            "model.encoder.layer_norm.bias",
            vec![512],
        ),
        (
            "dec.pos_emb.weight",
            "model.decoder.embed_positions.weight",
            vec![448, 512],
        ),
        (
            "dec.final_norm.weight",
            "model.decoder.layer_norm.weight",
            vec![512],
        ),
        (
            "dec.final_norm.bias",
            "model.decoder.layer_norm.bias",
            vec![512],
        ),
    ] {
        add(source.into(), target.into(), &shape, 0);
    }
    add(
        "dec.token_embd.weight".into(),
        "model.decoder.embed_tokens.weight".into(),
        &[51865, 512],
        8,
    );
    for (side, target_side) in [("enc", "encoder"), ("dec", "decoder")] {
        for index in 0..6 {
            let prefix = format!("{side}.blocks.{index}");
            let target = format!("model.{target_side}.layers.{index}");
            let norms: &[(&str, &str)] = if side == "enc" {
                &[
                    ("norm_attn", "self_attn_layer_norm"),
                    ("norm_ffn", "final_layer_norm"),
                ]
            } else {
                &[
                    ("norm_self", "self_attn_layer_norm"),
                    ("norm_cross", "encoder_attn_layer_norm"),
                    ("norm_ffn", "final_layer_norm"),
                ]
            };
            for (source_norm, target_norm) in norms {
                for suffix in ["weight", "bias"] {
                    add(
                        format!("{prefix}.{source_norm}.{suffix}"),
                        format!("{target}.{target_norm}.{suffix}"),
                        &[512],
                        0,
                    );
                }
            }
            let attention: &[(&str, &str)] = if side == "enc" {
                &[("attn", "self_attn")]
            } else {
                &[("self_attn", "self_attn"), ("cross_attn", "encoder_attn")]
            };
            for (source_attention, target_attention) in attention {
                for (projection, target_projection) in [
                    ("q", "q_proj"),
                    ("k", "k_proj"),
                    ("v", "v_proj"),
                    ("out", "out_proj"),
                ] {
                    add(
                        format!("{prefix}.{source_attention}.{projection}.weight"),
                        format!("{target}.{target_attention}.{target_projection}.weight"),
                        &[512, 512],
                        8,
                    );
                    if projection != "k" {
                        add(
                            format!("{prefix}.{source_attention}.{projection}.bias"),
                            format!("{target}.{target_attention}.{target_projection}.bias"),
                            &[512],
                            0,
                        );
                    }
                }
            }
            for (layer, shape, bias) in [("fc1", [2048, 512], 2048), ("fc2", [512, 2048], 512)] {
                add(
                    format!("{prefix}.ffn.{layer}.weight"),
                    format!("{target}.{layer}.weight"),
                    &shape,
                    8,
                );
                add(
                    format!("{prefix}.ffn.{layer}.bias"),
                    format!("{target}.{layer}.bias"),
                    &[bias],
                    0,
                );
            }
        }
    }
    mappings
}

fn validate_tensors(tensors: &[GgufTensor]) -> Result<Vec<TensorMapping>> {
    let mut by_name = BTreeMap::new();
    for tensor in tensors {
        ensure!(
            by_name.insert(tensor.name.as_str(), tensor).is_none(),
            "Duplicate Whisper tensor: {}",
            tensor.name
        );
    }
    let mappings = expected_tensors();
    ensure!(
        by_name.len() == 247,
        "Native Whisper Base requires exactly 247 source tensors"
    );
    let mut targets = BTreeSet::new();
    for mapping in &mappings {
        ensure!(
            targets.insert(&mapping.target),
            "Duplicate Whisper target tensor"
        );
        validate_shape(&by_name, &mapping.source, &mapping.shape, mapping.type_id)?;
    }
    for (name, shape) in [
        ("frontend.mel_filterbank", vec![80, 201]),
        ("frontend.window", vec![400]),
        ("enc.pos_emb.weight", vec![1500, 512]),
    ] {
        validate_shape(&by_name, name, &shape, 0)?;
    }
    Ok(mappings)
}

fn validate_shape(
    tensors: &BTreeMap<&str, &GgufTensor>,
    name: &str,
    shape: &[usize],
    type_id: u32,
) -> Result<()> {
    let tensor = tensors
        .get(name)
        .with_context(|| format!("Missing native Whisper tensor {name}"))?;
    ensure!(
        tensor.type_id == type_id
            && tensor
                .dimensions
                .iter()
                .rev()
                .copied()
                .eq(shape.iter().map(|value| *value as u64)),
        "Native Whisper tensor has unsupported shape or quantization: {name}"
    );
    Ok(())
}

fn metadata<'a>(values: &'a HashMap<String, Value>, key: &str) -> Result<&'a Value> {
    values
        .get(key)
        .with_context(|| format!("Missing Whisper metadata {key}"))
}

fn validate_metadata(values: &HashMap<String, Value>, config: &Config) -> Result<Vec<u32>> {
    ensure!(
        config.num_mel_bins == 80
            && config.max_source_positions == 1500
            && config.d_model == 512
            && config.encoder_attention_heads == 8
            && config.encoder_layers == 6
            && config.decoder_attention_heads == 8
            && config.decoder_layers == 6
            && config.vocab_size == 51865
            && config.max_target_positions == 448,
        "Whisper configuration differs from the fixed Base profile"
    );
    for (key, expected) in [
        ("general.architecture", "whisper"),
        ("stt.variant", "whisper-base"),
        ("stt.frontend.type", "mel"),
        ("stt.frontend.normalize", "whisper_logmel"),
        ("stt.frontend.pad_mode", "reflect"),
        ("stt.frontend.window", "hann_periodic"),
        ("stt.frontend.mel_norm", "slaney"),
        ("stt.whisper.encoder.activation", "gelu"),
        ("stt.whisper.decoder.activation", "gelu"),
        ("tokenizer.ggml.model", "gpt2"),
        ("tokenizer.ggml.pre", "gpt2"),
    ] {
        ensure!(
            matches!(metadata(values, key)?, Value::String(value) if value == expected),
            "Unsupported Whisper metadata {key}"
        );
    }
    for (key, expected) in [
        ("stt.frontend.sample_rate", 16000),
        ("stt.frontend.num_mels", 80),
        ("stt.frontend.n_fft", 400),
        ("stt.frontend.win_length", 400),
        ("stt.frontend.hop_length", 160),
        ("stt.frontend.n_samples", 480000),
        ("stt.frontend.nb_max_frames", 3000),
        ("stt.frontend.chunk_length", 30),
        ("stt.whisper.encoder.d_model", 512),
        ("stt.whisper.encoder.ffn_dim", 2048),
        ("stt.whisper.encoder.n_heads", 8),
        ("stt.whisper.encoder.n_layers", 6),
        ("stt.whisper.encoder.num_mel_bins", 80),
        ("stt.whisper.encoder.max_source_positions", 1500),
        ("stt.whisper.decoder.d_model", 512),
        ("stt.whisper.decoder.ffn_dim", 2048),
        ("stt.whisper.decoder.n_heads", 8),
        ("stt.whisper.decoder.n_layers", 6),
        ("stt.whisper.decoder.max_target_positions", 448),
        ("stt.whisper.decoder.vocab_size", 51865),
        ("stt.whisper.decoder_start_token_id", 50258),
        ("stt.whisper.sot_token_id", 50258),
        ("stt.whisper.no_timestamps_token_id", 50363),
        ("stt.whisper.prev_sot_token_id", 50361),
        ("stt.whisper.transcribe_token_id", 50359),
        ("stt.whisper.translate_token_id", 50358),
        ("tokenizer.ggml.bos_token_id", 50257),
        ("tokenizer.ggml.eos_token_id", 50257),
        ("tokenizer.ggml.padding_token_id", 50257),
    ] {
        ensure!(
            matches!(metadata(values, key)?, Value::U32(value) if *value == expected),
            "Unsupported Whisper metadata {key}"
        );
    }
    for (key, expected) in [
        ("stt.frontend.center", true),
        ("stt.whisper.decoder.scale_embedding", false),
        ("stt.whisper.decoder.tie_word_embeddings", true),
        ("tokenizer.ggml.add_bos_token", false),
    ] {
        ensure!(
            matches!(metadata(values, key)?, Value::Bool(value) if *value == expected),
            "Unsupported Whisper metadata {key}"
        );
    }
    for (key, expected) in [
        ("stt.frontend.dither", 0.0),
        ("stt.frontend.pre_emphasis", 0.0),
        ("stt.frontend.f_min", 0.0),
        ("stt.frontend.f_max", 8000.0),
    ] {
        ensure!(
            matches!(metadata(values, key)?, Value::F32(value) if *value == expected),
            "Unsupported Whisper metadata {key}"
        );
    }
    let suppress_tokens = integer_array(values, "stt.whisper.suppress_tokens")?;
    let mut expected = super::SUPPRESS_TOKENS.to_vec();
    expected.extend([50358, 50359, 50361]);
    expected.sort_unstable();
    ensure!(
        suppress_tokens == expected,
        "Whisper suppression tokens differ from the fixed Base profile"
    );
    ensure!(
        integer_array(values, "stt.whisper.begin_suppress_tokens")? == [220, 50257],
        "Unsupported Whisper initial suppression tokens"
    );
    Ok(suppress_tokens)
}

fn integer_array(values: &HashMap<String, Value>, key: &str) -> Result<Vec<u32>> {
    let Value::Array(items) = metadata(values, key)? else {
        bail!("Whisper metadata {key} is not an array")
    };
    items
        .iter()
        .map(|value| match value {
            Value::I32(value) => u32::try_from(*value).map_err(Into::into),
            _ => bail!("Whisper metadata {key} has an invalid integer type"),
        })
        .collect()
}

fn validate_tokenizer(values: &HashMap<String, Value>, json: &str) -> Result<Tokenizer> {
    ensure!(
        json.len() <= 4 * 1024 * 1024,
        "Whisper tokenizer JSON exceeds the fixed profile budget"
    );
    let candidate: serde_json::Value = serde_json::from_str(json)?;
    let canonical: serde_json::Value = serde_json::from_str(BUNDLED_TOKENIZER)?;
    ensure!(
        candidate == canonical,
        "Native Whisper requires the bundled, source-verified tokenizer settings"
    );
    let vocab = candidate["model"]["vocab"]
        .as_object()
        .context("Whisper tokenizer has no vocabulary")?;
    let mut by_id = BTreeMap::new();
    for (text, id) in vocab {
        let id = id
            .as_u64()
            .context("Whisper vocabulary ID is not an unsigned integer")?;
        ensure!(
            by_id.insert(id, text.as_str()).is_none(),
            "Duplicate Whisper vocabulary ID"
        );
    }
    let added = candidate["added_tokens"]
        .as_array()
        .context("Whisper tokenizer has no special tokens")?;
    for token in added {
        let id = token["id"]
            .as_u64()
            .context("Invalid Whisper added token ID")?;
        let text = token["content"]
            .as_str()
            .context("Invalid Whisper added token text")?;
        if let Some(existing) = by_id.insert(id, text) {
            ensure!(
                existing == text,
                "Whisper added token conflicts with the vocabulary"
            );
        }
    }
    let Value::Array(tokens) = metadata(values, "tokenizer.ggml.tokens")? else {
        bail!("Whisper GGUF has no token array")
    };
    ensure!(
        by_id.len() == 51865 && tokens.len() == 51865,
        "Whisper vocabulary size mismatch"
    );
    for (index, token) in tokens.iter().enumerate() {
        ensure!(
            matches!(token, Value::String(text) if by_id.get(&(index as u64)).copied() == Some(text.as_str())),
            "Whisper token {index} differs from the source model"
        );
    }
    let token_types = integer_array(values, "tokenizer.ggml.token_type")?;
    ensure!(
        token_types.len() == 51865
            && token_types.iter().enumerate().all(|(index, kind)| *kind
                == if (50257..50364).contains(&index) {
                    3
                } else {
                    1
                }),
        "Whisper token types differ from the source model"
    );
    let Value::Array(merges) = metadata(values, "tokenizer.ggml.merges")? else {
        bail!("Whisper GGUF has no BPE merges")
    };
    let candidate_merges = candidate["model"]["merges"]
        .as_array()
        .context("Whisper tokenizer has no BPE merges")?;
    ensure!(
        merges.len() == 50000 && candidate_merges.len() == merges.len(),
        "Whisper BPE merge count mismatch"
    );
    for (index, (native, bundled)) in merges.iter().zip(candidate_merges).enumerate() {
        ensure!(
            matches!(native, Value::String(text) if bundled.as_str() == Some(text.as_str())),
            "Whisper BPE merge {index} differs from the source model"
        );
    }
    Tokenizer::from_bytes(json.as_bytes())
        .map_err(|error| anyhow::anyhow!("Invalid Whisper tokenizer: {error}"))
}

pub(super) fn pcm_to_mel(
    layout: &str,
    config: &Config,
    samples: &[f32],
    filters: &[f32],
) -> Result<Vec<f32>> {
    if layout == LEGACY_LAYOUT {
        return Ok(candle_transformers::models::whisper::audio::pcm_to_mel(
            config, samples, filters,
        ));
    }
    ensure!(
        layout == MODELSCOPE_LAYOUT && config.num_mel_bins == 80,
        "Unsupported native Whisper frontend profile"
    );
    ensure!(
        filters.len() == 80 * 201 && filters.iter().all(|value| value.is_finite()),
        "Native Whisper requires 80 finite 201-bin mel filters"
    );
    ensure!(
        samples.iter().all(|value| value.is_finite()),
        "Whisper audio contains non-finite samples"
    );
    let chunks = samples.len().div_ceil(NATIVE_CHUNK_SAMPLES);
    let frames = chunks
        .checked_mul(NATIVE_CHUNK_FRAMES)
        .context("Whisper frame count overflow")?;
    let elements = frames
        .checked_mul(80)
        .context("Whisper mel size overflow")?;
    let mut output = vec![0.0; elements];
    let roots: Vec<_> = (0..NATIVE_FFT)
        .map(|index| {
            let phase = -std::f64::consts::TAU * index as f64 / NATIVE_FFT as f64;
            (phase.cos(), phase.sin())
        })
        .collect();
    let window: Vec<_> = (0..NATIVE_FFT)
        .map(|index| 0.5 * (1.0 - (std::f64::consts::TAU * index as f64 / NATIVE_FFT as f64).cos()))
        .collect();
    let filters: Vec<Vec<_>> = filters
        .chunks_exact(201)
        .map(|row| {
            row.iter()
                .enumerate()
                .filter_map(|(index, value)| (*value != 0.0).then_some((index, f64::from(*value))))
                .collect()
        })
        .collect();
    for (chunk_index, chunk) in samples.chunks(NATIVE_CHUNK_SAMPLES).enumerate() {
        let mel = native_chunk_mel(chunk, &filters, &window, &roots);
        for band in 0..80 {
            let destination = band * frames + chunk_index * NATIVE_CHUNK_FRAMES;
            output[destination..destination + NATIVE_CHUNK_FRAMES].copy_from_slice(
                &mel[band * NATIVE_CHUNK_FRAMES..(band + 1) * NATIVE_CHUNK_FRAMES],
            );
        }
    }
    Ok(output)
}

const NATIVE_FFT: usize = 400;
const NATIVE_CHUNK_SAMPLES: usize = 480_000;
const NATIVE_CHUNK_FRAMES: usize = 3_000;

fn native_chunk_mel(
    samples: &[f32],
    filters: &[Vec<(usize, f64)>],
    window: &[f64],
    roots: &[(f64, f64)],
) -> Vec<f32> {
    let mut mel = vec![-10.0_f64; 80 * NATIVE_CHUNK_FRAMES];
    let mut frame = [0.0_f64; NATIVE_FFT];
    for index in 0..NATIVE_CHUNK_FRAMES {
        for (offset, value) in frame.iter_mut().enumerate() {
            // Zero-pad the 30-second chunk before the centered STFT's reflect pad.
            // Reflect excludes the boundary element, matching torch's pad_mode.
            let position = (index * 160 + offset) as isize - (NATIVE_FFT / 2) as isize;
            let reflected = if position < 0 {
                -position
            } else if position >= NATIVE_CHUNK_SAMPLES as isize {
                2 * NATIVE_CHUNK_SAMPLES as isize - 2 - position
            } else {
                position
            } as usize;
            *value = f64::from(samples.get(reflected).copied().unwrap_or(0.0)) * window[offset];
        }
        if frame.iter().all(|value| *value == 0.0) {
            continue;
        }
        let spectrum = native_fft(&frame, 1, NATIVE_FFT, roots);
        let power: Vec<_> = spectrum[..201]
            .iter()
            .map(|(real, imag)| real * real + imag * imag)
            .collect();
        for (band, filter) in filters.iter().enumerate() {
            let energy: f64 = filter
                .iter()
                .map(|(frequency, weight)| power[*frequency] * weight)
                .sum();
            mel[band * NATIVE_CHUNK_FRAMES + index] = energy.max(1e-10).log10();
        }
    }
    let floor = mel.iter().copied().fold(f64::NEG_INFINITY, f64::max) - 8.0;
    mel.into_iter()
        .map(|value| ((value.max(floor) + 4.0) / 4.0) as f32)
        .collect()
}

// Candle's FFT is private and its public frontend folds both spectral halves.
// This fixed-size mixed-radix transform preserves the source's one-sided power.
fn native_fft(input: &[f64], stride: usize, size: usize, roots: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if size == 1 {
        return vec![(input[0], 0.0)];
    }
    let radix = if size.is_multiple_of(2) { 2 } else { 5 };
    let width = size / radix;
    let branches: Vec<_> = (0..radix)
        .map(|branch| native_fft(&input[branch * stride..], stride * radix, width, roots))
        .collect();
    (0..size)
        .map(|frequency| {
            let mut sum = (0.0, 0.0);
            for (branch, values) in branches.iter().enumerate() {
                let (real, imag) = values[frequency % width];
                let (cosine, sine) = roots[(branch * frequency * (NATIVE_FFT / size)) % NATIVE_FFT];
                sum.0 += real * cosine - imag * sine;
                sum.1 += real * sine + imag * cosine;
            }
            sum
        })
        .collect()
}

#[cfg(test)]
#[path = "whisper_gguf_tests.rs"]
mod tests;
