use super::*;
use candle_core::quantized::{GgmlDType, QTensor};
use candle_core::Tensor;
use serde_json::Value as Json;

fn scalar(value: &Json) -> Value {
    match value["type"].as_str().unwrap() {
        "string" => Value::String(value["value"].as_str().unwrap().into()),
        "unsigned" => Value::U32(value["value"].as_u64().unwrap().try_into().unwrap()),
        "signed" => Value::I32(value["value"].as_i64().unwrap().try_into().unwrap()),
        "bool" => Value::Bool(value["value"].as_bool().unwrap()),
        "float32_bits" => Value::F32(f32::from_bits(value["value"].as_u64().unwrap() as u32)),
        kind => panic!("Unexpected fixture scalar type {kind}"),
    }
}

fn fixture() -> (HashMap<String, Value>, Vec<GgufTensor>) {
    let data: Json = serde_json::from_str(include_str!("whisper_gguf_test_data.json")).unwrap();
    assert_eq!(data["source"]["sha256"], BASE_SHA256);
    assert_eq!(data["source"]["sizeBytes"], BASE_BYTES);
    let metadata = data["metadata"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| {
            let parsed = if value["kind"] == "scalar" {
                scalar(&value["value"])
            } else {
                let retained = value["retained_values"].as_array().unwrap();
                assert_eq!(value["count"].as_u64().unwrap() as usize, retained.len());
                Value::Array(retained.iter().map(scalar).collect())
            };
            (key.clone(), parsed)
        })
        .collect();
    (
        metadata,
        serde_json::from_value(data["tensors"].clone()).unwrap(),
    )
}

fn tokenizer_metadata() -> HashMap<String, Value> {
    let json: Json = serde_json::from_str(BUNDLED_TOKENIZER).unwrap();
    let mut tokens = vec![String::new(); 51865];
    for (token, index) in json["model"]["vocab"].as_object().unwrap() {
        tokens[index.as_u64().unwrap() as usize] = token.clone();
    }
    for added in json["added_tokens"].as_array().unwrap() {
        tokens[added["id"].as_u64().unwrap() as usize] = added["content"].as_str().unwrap().into();
    }
    let merges: Vec<_> = json["model"]["merges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    // Independent fingerprints were computed from every string in the source GGUF arrays.
    assert_eq!(
        string_digest(&tokens),
        "6e42e488f922e3ddabfc990084323b4cc3937d19c39fb99d57310bc70e53ff97"
    );
    assert_eq!(
        string_digest(&merges),
        "6a52830ebc4e73859a00cf84cb3b4844b14c3cc5ae70f8f635495b96e25fb7bb"
    );
    HashMap::from([
        (
            "tokenizer.ggml.tokens".into(),
            Value::Array(tokens.into_iter().map(Value::String).collect()),
        ),
        (
            "tokenizer.ggml.merges".into(),
            Value::Array(merges.into_iter().map(Value::String).collect()),
        ),
        (
            "tokenizer.ggml.token_type".into(),
            Value::Array(
                (0..51865)
                    .map(|index| {
                        Value::I32(if (50257..50364).contains(&index) {
                            3
                        } else {
                            1
                        })
                    })
                    .collect(),
            ),
        ),
    ])
}

fn string_digest(values: &[String]) -> String {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }
    Sha256Digest::of_bytes(&bytes).as_str().into()
}

#[test]
fn observed_native_headers_map_every_required_candle_tensor() {
    let (_, tensors) = fixture();
    assert_eq!(layout(&tensors).unwrap(), Layout::NativeBase);
    let mapped = validate_tensors(&tensors).unwrap();
    assert_eq!(mapped.len(), 244);
    let targets: BTreeSet<_> = mapped.iter().map(|item| item.target.as_str()).collect();
    assert_eq!(targets.len(), 244);
    for (source, target, shape) in [
        (
            "enc.conv.0.weight",
            "model.encoder.conv1.weight",
            vec![512, 80, 3],
        ),
        (
            "dec.token_embd.weight",
            "model.decoder.embed_tokens.weight",
            vec![51865, 512],
        ),
        (
            "dec.pos_emb.weight",
            "model.decoder.embed_positions.weight",
            vec![448, 512],
        ),
        (
            "enc.blocks.5.ffn.fc2.weight",
            "model.encoder.layers.5.fc2.weight",
            vec![512, 2048],
        ),
        (
            "dec.blocks.5.cross_attn.k.weight",
            "model.decoder.layers.5.encoder_attn.k_proj.weight",
            vec![512, 512],
        ),
    ] {
        let item = mapped.iter().find(|item| item.source == source).unwrap();
        assert_eq!(item.target, target);
        assert_eq!(item.shape, shape);
    }
    assert!(!targets.iter().any(|name| name.ends_with("k_proj.bias")));
}

#[test]
fn missing_duplicate_foreign_shape_and_quantization_headers_are_rejected() {
    for change in 0..6 {
        let (_, mut tensors) = fixture();
        match change {
            0 => {
                tensors.pop();
            }
            1 => {
                tensors.push(tensors[0].clone());
            }
            2 => {
                tensors[0].name = tensors[1].name.clone();
            }
            3 => {
                tensors[0].dimensions[0] += 1;
            }
            4 => {
                tensors[0].type_id = 1;
            }
            5 => {
                tensors[0].name = "enc.blocks.99.attn.q.weight".into();
            }
            _ => unreachable!(),
        }
        assert!(validate_tensors(&tensors).is_err(), "change {change}");
    }
}

#[test]
fn legacy_mixed_and_unknown_layouts_are_distinguished() {
    let (_, tensors) = fixture();
    let mut legacy = tensors[..2].to_vec();
    legacy[0].name = "model.encoder.conv1.weight".into();
    legacy[1].name = "model.decoder.embed_tokens.weight".into();
    assert_eq!(layout(&legacy).unwrap(), Layout::Candle);
    legacy.push(
        tensors
            .iter()
            .find(|item| item.name == "enc.conv.0.weight")
            .unwrap()
            .clone(),
    );
    assert!(layout(&legacy).is_err());
    assert!(layout(&tensors[..2]).is_err());
    assert!(layout(&[]).is_err());
}

#[test]
fn source_metadata_provides_the_complete_suppression_set() {
    let (metadata, _) = fixture();
    let config = super::super::get_model("base").unwrap().config();
    let suppressed = validate_metadata(&metadata, &config).unwrap();
    assert_eq!(suppressed.len(), 88);
    assert!(
        suppressed.contains(&50358) && suppressed.contains(&50359) && suppressed.contains(&50361)
    );
    assert!(validate_metadata(
        &metadata,
        &super::super::get_model("tiny").unwrap().config()
    )
    .is_err());
}

#[test]
fn altered_metadata_and_suppression_rules_are_rejected() {
    let (original, _) = fixture();
    let config = super::super::get_model("base").unwrap().config();
    for key in [
        "general.architecture",
        "stt.variant",
        "stt.frontend.sample_rate",
        "stt.frontend.num_mels",
        "stt.frontend.center",
        "stt.frontend.f_max",
        "stt.whisper.encoder.n_heads",
        "stt.whisper.decoder.n_layers",
        "stt.whisper.decoder.vocab_size",
        "stt.whisper.decoder.tie_word_embeddings",
        "stt.whisper.transcribe_token_id",
        "tokenizer.ggml.eos_token_id",
    ] {
        let mut values = original.clone();
        values.insert(key.into(), Value::String("incorrect".into()));
        assert!(validate_metadata(&values, &config).is_err(), "key {key}");
    }
    for key in [
        "stt.whisper.suppress_tokens",
        "stt.whisper.begin_suppress_tokens",
    ] {
        let mut values = original.clone();
        let Value::Array(items) = values.get_mut(key).unwrap() else {
            unreachable!()
        };
        items.pop();
        assert!(validate_metadata(&values, &config).is_err());
    }
}

#[test]
fn corrected_tokenizer_matches_independent_full_source_fingerprints() {
    let metadata = tokenizer_metadata();
    let tokenizer = validate_tokenizer(&metadata, BUNDLED_TOKENIZER).unwrap();
    assert_eq!(tokenizer.id_to_token(24717).as_deref(), Some("Ġgoose"));
    assert_eq!(tokenizer.id_to_token(48305).as_deref(), Some("Ġgoosebumps"));
    assert_eq!(tokenizer.id_to_token(50259).as_deref(), Some("<|en|>"));
    assert_eq!(tokenizer.id_to_token(50364).as_deref(), Some("<|0.00|>"));
}

#[test]
fn changed_source_tokens_merges_types_and_truncated_arrays_are_rejected() {
    let original = tokenizer_metadata();
    for (key, index) in [
        ("tokenizer.ggml.tokens", 24717),
        ("tokenizer.ggml.merges", 48049),
        ("tokenizer.ggml.token_type", 50257),
    ] {
        let mut metadata = original.clone();
        let Value::Array(items) = metadata.get_mut(key).unwrap() else {
            unreachable!()
        };
        items[index] = if key.ends_with("token_type") {
            Value::I32(1)
        } else {
            Value::String("polluted".into())
        };
        assert!(validate_tokenizer(&metadata, BUNDLED_TOKENIZER).is_err());
        let mut metadata = original.clone();
        let Value::Array(items) = metadata.get_mut(key).unwrap() else {
            unreachable!()
        };
        items.pop();
        assert!(validate_tokenizer(&metadata, BUNDLED_TOKENIZER).is_err());
    }
}

#[test]
fn polluted_sidecar_content_and_altered_decoder_settings_are_rejected_in_memory() {
    let metadata = tokenizer_metadata();
    let polluted = BUNDLED_TOKENIZER
        .replace("\"Ġgoose\": 24717", "\"Ġlumina\": 24717")
        .replace("\"Ġgoosebumps\": 48305", "\"Ġluminabumps\": 48305")
        .replace("\"Ġgoose bumps\"", "\"Ġlumina bumps\"");
    assert_ne!(polluted, BUNDLED_TOKENIZER);
    assert!(validate_tokenizer(&metadata, &polluted).is_err());
    let mut altered: Json = serde_json::from_str(BUNDLED_TOKENIZER).unwrap();
    altered["decoder"] = Json::Null;
    assert!(validate_tokenizer(&metadata, &serde_json::to_string(&altered).unwrap()).is_err());
}

#[test]
fn legacy_quantized_builder_remains_loadable_without_rewriting_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.gguf");
    let tensor = Tensor::from_vec(
        (0..32).map(|index| index as f32 / 32.0).collect(),
        32,
        &Device::Cpu,
    )
    .unwrap();
    let quantized = QTensor::quantize(&tensor, GgmlDType::Q8_0).unwrap();
    let mut bytes = Cursor::new(Vec::new());
    gguf_file::write(
        &mut bytes,
        &[],
        &[
            ("model.encoder.conv1.weight", &quantized),
            ("model.decoder.embed_tokens.weight", &quantized),
        ],
    )
    .unwrap();
    std::fs::write(&path, bytes.get_ref()).unwrap();
    let loaded = load(
        &path,
        "base",
        &super::super::get_model("base").unwrap().config(),
        BUNDLED_TOKENIZER,
        &Device::Cpu,
    )
    .unwrap();
    let WhisperWeights::Legacy(builder) = loaded else {
        panic!("Legacy input took the native branch")
    };
    let result = builder.get(32, "model.encoder.conv1.weight").unwrap();
    assert_eq!(result.dtype(), GgmlDType::Q8_0);
    assert_eq!(
        result.data().unwrap().as_ref(),
        quantized.data().unwrap().as_ref()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes.into_inner());
    assert!(!dir.path().join("tokenizer.json").exists());
}

#[test]
fn arbitrary_native_named_gguf_cannot_enter_the_fixed_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.gguf");
    let tensor = Tensor::zeros(32, candle_core::DType::F32, &Device::Cpu).unwrap();
    let quantized = QTensor::quantize(&tensor, GgmlDType::F32).unwrap();
    let mut bytes = Cursor::new(Vec::new());
    gguf_file::write(
        &mut bytes,
        &[],
        &[
            ("enc.conv.0.weight", &quantized),
            ("dec.token_embd.weight", &quantized),
        ],
    )
    .unwrap();
    std::fs::write(&path, bytes.get_ref()).unwrap();
    for model in ["base", "tiny"] {
        assert!(load(
            &path,
            model,
            &super::super::get_model(model).unwrap().config(),
            BUNDLED_TOKENIZER,
            &Device::Cpu
        )
        .is_err());
    }
    assert_eq!(std::fs::read(&path).unwrap(), bytes.into_inner());
}

fn frontend_filters() -> Vec<f32> {
    include_bytes!("whisper_data/melfilters.bytes")
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

fn frontend_signal(name: &str) -> Vec<f32> {
    if name == "singleton" {
        return vec![1.0];
    }
    if let Some(length) = name.strip_prefix("hop_") {
        return (0..length.parse::<usize>().unwrap())
            .map(|index| ((index * 37 % 257) as f32 - 128.0) / 512.0)
            .collect();
    }
    match name {
        "starting_impulse" => {
            let mut samples = vec![0.0; 640];
            samples[0] = 1.0;
            samples[159] = -0.5;
            samples[160] = 0.25;
            samples
        }
        "ordinary" => (0..2001)
            .map(|index| ((index * 37 % 257) as f32 - 128.0) / 512.0)
            .collect(),
        "chunk_boundary" => {
            let mut samples = vec![0.0; 480001];
            samples[479999] = 0.75;
            samples[480000] = -0.5;
            samples
        }
        _ => panic!("Unknown frontend signal"),
    }
}

#[test]
fn native_frontend_matches_numpy_centered_stft_reference() {
    // Independent NumPy 2.2.6 reference: pad each chunk to 480000, np.pad(200,
    // mode="reflect"), periodic Hann, np.fft.rfft, one-sided abs()**2, source
    // mel matrix, log10(max(energy,1e-10)), max-8 clamp, then (x+4)/4.
    // The final centered STFT frame is discarded. These constants are not
    // computed with the production FFT or frontend during the test.
    let cases: &[(&str, usize, &[(usize, usize, f32)])] = &[
        (
            "singleton",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f1950a2)),
                (1, 0, f32::from_bits(0x3f1950a2)),
                (5, 0, f32::from_bits(0x3f1950a2)),
                (10, 0, f32::from_bits(0x3f1950a2)),
                (20, 0, f32::from_bits(0x3f1950a2)),
                (40, 0, f32::from_bits(0x3f1b4297)),
                (60, 0, f32::from_bits(0x3f1994b1)),
                (79, 0, f32::from_bits(0x3f196291)),
                (0, 1, f32::from_bits(0x3db600f0)),
                (1, 1, f32::from_bits(0x3db600f0)),
                (5, 1, f32::from_bits(0x3db600ef)),
                (10, 1, f32::from_bits(0x3db600ef)),
                (20, 1, f32::from_bits(0x3db600f0)),
                (40, 1, f32::from_bits(0x3dc59098)),
                (60, 1, f32::from_bits(0x3db82167)),
                (79, 1, f32::from_bits(0x3db6906a)),
                (0, 2, f32::from_bits(0xbfb233ea)),
                (1, 2, f32::from_bits(0xbfb233ea)),
                (5, 2, f32::from_bits(0xbfb233ea)),
                (10, 2, f32::from_bits(0xbfb233ea)),
                (20, 2, f32::from_bits(0xbfb233ea)),
                (40, 2, f32::from_bits(0xbfb233ea)),
                (60, 2, f32::from_bits(0xbfb233ea)),
                (79, 2, f32::from_bits(0xbfb233ea)),
                (0, 2999, f32::from_bits(0xbfb233ea)),
                (1, 2999, f32::from_bits(0xbfb233ea)),
                (5, 2999, f32::from_bits(0xbfb233ea)),
                (10, 2999, f32::from_bits(0xbfb233ea)),
                (20, 2999, f32::from_bits(0xbfb233ea)),
                (40, 2999, f32::from_bits(0xbfb233ea)),
                (60, 2999, f32::from_bits(0xbfb233ea)),
                (79, 2999, f32::from_bits(0xbfb233ea)),
            ],
        ),
        (
            "hop_159",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f445c84)),
                (1, 0, f32::from_bits(0x3f50b015)),
                (5, 0, f32::from_bits(0x3f0b45af)),
                (10, 0, f32::from_bits(0x3e937085)),
                (20, 0, f32::from_bits(0x3eb4aa7a)),
                (40, 0, f32::from_bits(0x3f13d84e)),
                (60, 0, f32::from_bits(0x3e1ce890)),
                (79, 0, f32::from_bits(0x3e9c3639)),
                (0, 1, f32::from_bits(0x3eb078db)),
                (1, 1, f32::from_bits(0x3f11e4ea)),
                (5, 1, f32::from_bits(0x3f16e6d9)),
                (10, 1, f32::from_bits(0x3eeb2f8d)),
                (20, 1, f32::from_bits(0x3ec2c642)),
                (40, 1, f32::from_bits(0x3ebc1bea)),
                (60, 1, f32::from_bits(0x3eaf66b4)),
                (79, 1, f32::from_bits(0x3ecb2bdb)),
                (0, 2, f32::from_bits(0xbe84a3fd)),
                (1, 2, f32::from_bits(0xbe846755)),
                (5, 2, f32::from_bits(0xbe850b95)),
                (10, 2, f32::from_bits(0xbe95ecf9)),
                (20, 2, f32::from_bits(0xbeac0cc3)),
                (40, 2, f32::from_bits(0xbe10ad7f)),
                (60, 2, f32::from_bits(0xbec6c80c)),
                (79, 2, f32::from_bits(0xbe8d4c90)),
                (0, 2999, f32::from_bits(0xbf68e7e5)),
                (1, 2999, f32::from_bits(0xbf68e7e5)),
                (5, 2999, f32::from_bits(0xbf68e7e5)),
                (10, 2999, f32::from_bits(0xbf68e7e5)),
                (20, 2999, f32::from_bits(0xbf68e7e5)),
                (40, 2999, f32::from_bits(0xbf68e7e5)),
                (60, 2999, f32::from_bits(0xbf68e7e5)),
                (79, 2999, f32::from_bits(0xbf68e7e5)),
            ],
        ),
        (
            "hop_160",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f452a54)),
                (1, 0, f32::from_bits(0x3f5084aa)),
                (5, 0, f32::from_bits(0x3f0bb266)),
                (10, 0, f32::from_bits(0x3e91b362)),
                (20, 0, f32::from_bits(0x3eb4522f)),
                (40, 0, f32::from_bits(0x3f1340c5)),
                (60, 0, f32::from_bits(0x3e29b2ec)),
                (79, 0, f32::from_bits(0x3e99fbcf)),
                (0, 1, f32::from_bits(0x3edb0040)),
                (1, 1, f32::from_bits(0x3f0c69e1)),
                (5, 1, f32::from_bits(0x3f15496a)),
                (10, 1, f32::from_bits(0x3f071b76)),
                (20, 1, f32::from_bits(0x3edb1e28)),
                (40, 1, f32::from_bits(0x3eca9389)),
                (60, 1, f32::from_bits(0x3ebe4b0b)),
                (79, 1, f32::from_bits(0x3eaa500b)),
                (0, 2, f32::from_bits(0xbf10e0ef)),
                (1, 2, f32::from_bits(0xbedc94fa)),
                (5, 2, f32::from_bits(0xbe690392)),
                (10, 2, f32::from_bits(0xbe2a0290)),
                (20, 2, f32::from_bits(0xbe2e0490)),
                (40, 2, f32::from_bits(0xbd471cc3)),
                (60, 2, f32::from_bits(0xbec27502)),
                (79, 2, f32::from_bits(0xbf02223f)),
                (0, 2999, f32::from_bits(0xbf68de0e)),
                (1, 2999, f32::from_bits(0xbf68de0e)),
                (5, 2999, f32::from_bits(0xbf68de0e)),
                (10, 2999, f32::from_bits(0xbf68de0e)),
                (20, 2999, f32::from_bits(0xbf68de0e)),
                (40, 2999, f32::from_bits(0xbf68de0e)),
                (60, 2999, f32::from_bits(0xbf68de0e)),
                (79, 2999, f32::from_bits(0xbf68de0e)),
            ],
        ),
        (
            "hop_161",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f444034)),
                (1, 0, f32::from_bits(0x3f50bbd9)),
                (5, 0, f32::from_bits(0x3f0b1c27)),
                (10, 0, f32::from_bits(0x3e930e13)),
                (20, 0, f32::from_bits(0x3eb629fd)),
                (40, 0, f32::from_bits(0x3f137189)),
                (60, 0, f32::from_bits(0x3e058fa6)),
                (79, 0, f32::from_bits(0x3e994ca7)),
                (0, 1, f32::from_bits(0x3eaa942f)),
                (1, 1, f32::from_bits(0x3f136876)),
                (5, 1, f32::from_bits(0x3f164c8f)),
                (10, 1, f32::from_bits(0x3ee36d55)),
                (20, 1, f32::from_bits(0x3ea9c123)),
                (40, 1, f32::from_bits(0x3e8c7c15)),
                (60, 1, f32::from_bits(0x3e8e0632)),
                (79, 1, f32::from_bits(0x3eb99590)),
                (0, 2, f32::from_bits(0xbe5e7257)),
                (1, 2, f32::from_bits(0xbe627b59)),
                (5, 2, f32::from_bits(0xbe85cc86)),
                (10, 2, f32::from_bits(0xbebc8eab)),
                (20, 2, f32::from_bits(0xbef72f91)),
                (40, 2, f32::from_bits(0xbe1dc094)),
                (60, 2, f32::from_bits(0xbe8cd5dc)),
                (79, 2, f32::from_bits(0xbe5071e6)),
                (0, 2999, f32::from_bits(0xbf68eb26)),
                (1, 2999, f32::from_bits(0xbf68eb26)),
                (5, 2999, f32::from_bits(0xbf68eb26)),
                (10, 2999, f32::from_bits(0xbf68eb26)),
                (20, 2999, f32::from_bits(0xbf68eb26)),
                (40, 2999, f32::from_bits(0xbf68eb26)),
                (60, 2999, f32::from_bits(0xbf68eb26)),
                (79, 2999, f32::from_bits(0xbf68eb26)),
            ],
        ),
        (
            "starting_impulse",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f1b92d9)),
                (1, 0, f32::from_bits(0x3f18d443)),
                (5, 0, f32::from_bits(0x3f196c9f)),
                (10, 0, f32::from_bits(0x3f1760fc)),
                (20, 0, f32::from_bits(0x3f194a48)),
                (40, 0, f32::from_bits(0x3f1aa112)),
                (60, 0, f32::from_bits(0x3f199cd7)),
                (79, 0, f32::from_bits(0x3f19a255)),
                (0, 1, f32::from_bits(0x3eb79b46)),
                (1, 1, f32::from_bits(0x3e99a721)),
                (5, 1, f32::from_bits(0x3ea16a33)),
                (10, 1, f32::from_bits(0x3e83cd51)),
                (20, 1, f32::from_bits(0x3ea7a63d)),
                (40, 1, f32::from_bits(0x3ebdd60c)),
                (60, 1, f32::from_bits(0x3eee9a35)),
                (79, 1, f32::from_bits(0x3f09b511)),
                (0, 2, f32::from_bits(0xbe6f8ac3)),
                (1, 2, f32::from_bits(0xbe6f5db4)),
                (5, 2, f32::from_bits(0xbe6d9b6e)),
                (10, 2, f32::from_bits(0xbe6927fe)),
                (20, 2, f32::from_bits(0xbe59a9b0)),
                (40, 2, f32::from_bits(0xbe1966e4)),
                (60, 2, f32::from_bits(0xbd62a84c)),
                (79, 2, f32::from_bits(0x3c9dacd6)),
                (0, 2999, f32::from_bits(0xbfb15db3)),
                (1, 2999, f32::from_bits(0xbfb15db3)),
                (5, 2999, f32::from_bits(0xbfb15db3)),
                (10, 2999, f32::from_bits(0xbfb15db3)),
                (20, 2999, f32::from_bits(0xbfb15db3)),
                (40, 2999, f32::from_bits(0xbfb15db3)),
                (60, 2999, f32::from_bits(0xbfb15db3)),
                (79, 2999, f32::from_bits(0xbfb15db3)),
            ],
        ),
        (
            "ordinary",
            3000,
            &[
                (0, 0, f32::from_bits(0x3f434cc7)),
                (1, 0, f32::from_bits(0x3f510b0c)),
                (5, 0, f32::from_bits(0x3f0a7401)),
                (10, 0, f32::from_bits(0x3e946cdc)),
                (20, 0, f32::from_bits(0x3eb7e776)),
                (40, 0, f32::from_bits(0x3f13f848)),
                (60, 0, f32::from_bits(0x3e146a2b)),
                (79, 0, f32::from_bits(0x3e992b0a)),
                (0, 1, f32::from_bits(0x3e91435b)),
                (1, 1, f32::from_bits(0x3f108ead)),
                (5, 1, f32::from_bits(0x3f11de4e)),
                (10, 1, f32::from_bits(0x3ef71759)),
                (20, 1, f32::from_bits(0x3ec0bb98)),
                (40, 1, f32::from_bits(0x3ebdd7d5)),
                (60, 1, f32::from_bits(0x3ead4079)),
                (79, 1, f32::from_bits(0x3eaf806f)),
                (0, 2, f32::from_bits(0x3e210de4)),
                (1, 2, f32::from_bits(0x3f1daba8)),
                (5, 2, f32::from_bits(0x3f16c4b9)),
                (10, 2, f32::from_bits(0x3ed40001)),
                (20, 2, f32::from_bits(0x3e8a18b0)),
                (40, 2, f32::from_bits(0x3ecd375a)),
                (60, 2, f32::from_bits(0x3ea95360)),
                (79, 2, f32::from_bits(0x3eab8510)),
                (0, 2999, f32::from_bits(0xbf6751b5)),
                (1, 2999, f32::from_bits(0xbf6751b5)),
                (5, 2999, f32::from_bits(0xbf6751b5)),
                (10, 2999, f32::from_bits(0xbf6751b5)),
                (20, 2999, f32::from_bits(0xbf6751b5)),
                (40, 2999, f32::from_bits(0xbf6751b5)),
                (60, 2999, f32::from_bits(0xbf6751b5)),
                (79, 2999, f32::from_bits(0xbf6751b5)),
            ],
        ),
        (
            "chunk_boundary",
            6000,
            &[
                (0, 0, f32::from_bits(0xbfc00000)),
                (1, 0, f32::from_bits(0xbfc00000)),
                (5, 0, f32::from_bits(0xbfc00000)),
                (10, 0, f32::from_bits(0xbfc00000)),
                (20, 0, f32::from_bits(0xbfc00000)),
                (40, 0, f32::from_bits(0xbfc00000)),
                (60, 0, f32::from_bits(0xbfc00000)),
                (79, 0, f32::from_bits(0xbfc00000)),
                (0, 2998, f32::from_bits(0xbfc00000)),
                (1, 2998, f32::from_bits(0xbfc00000)),
                (5, 2998, f32::from_bits(0xbfc00000)),
                (10, 2998, f32::from_bits(0xbfc00000)),
                (20, 2998, f32::from_bits(0xbfc00000)),
                (40, 2998, f32::from_bits(0xbfc00000)),
                (60, 2998, f32::from_bits(0xbfc00000)),
                (79, 2998, f32::from_bits(0xbfc00000)),
                (0, 2999, f32::from_bits(0x3d1690fa)),
                (1, 2999, f32::from_bits(0x3d1690f8)),
                (5, 2999, f32::from_bits(0x3d1690f8)),
                (10, 2999, f32::from_bits(0x3d1690f7)),
                (20, 2999, f32::from_bits(0x3d1690f9)),
                (40, 2999, f32::from_bits(0x3d35b049)),
                (60, 2999, f32::from_bits(0x3d1ad1e8)),
                (79, 2999, f32::from_bits(0x3d17afed)),
                (0, 3000, f32::from_bits(0x3ee590f7)),
                (1, 3000, f32::from_bits(0x3ee590f7)),
                (5, 3000, f32::from_bits(0x3ee590f7)),
                (10, 3000, f32::from_bits(0x3ee590f7)),
                (20, 3000, f32::from_bits(0x3ee590f7)),
                (40, 3000, f32::from_bits(0x3ee974e1)),
                (60, 3000, f32::from_bits(0x3ee61915)),
                (79, 3000, f32::from_bits(0x3ee5b4d5)),
                (0, 3001, f32::from_bits(0xbd7c808a)),
                (1, 3001, f32::from_bits(0xbd7c808b)),
                (5, 3001, f32::from_bits(0xbd7c808c)),
                (10, 3001, f32::from_bits(0xbd7c808c)),
                (20, 3001, f32::from_bits(0xbd7c808b)),
                (40, 3001, f32::from_bits(0xbd5d613b)),
                (60, 3001, f32::from_bits(0xbd783f9b)),
                (79, 3001, f32::from_bits(0xbd7b6196)),
                (0, 5999, f32::from_bits(0xbfc00000)),
                (1, 5999, f32::from_bits(0xbfc00000)),
                (5, 5999, f32::from_bits(0xbfc00000)),
                (10, 5999, f32::from_bits(0xbfc00000)),
                (20, 5999, f32::from_bits(0xbfc00000)),
                (40, 5999, f32::from_bits(0xbfc00000)),
                (60, 5999, f32::from_bits(0xbfc00000)),
                (79, 5999, f32::from_bits(0xbfc00000)),
            ],
        ),
    ];
    let config = super::super::get_model("base").unwrap().config();
    let filters = frontend_filters();
    for &(name, frame_count, expected) in cases {
        let samples = frontend_signal(name);
        let actual = pcm_to_mel(MODELSCOPE_LAYOUT, &config, &samples, &filters).unwrap();
        assert_eq!(actual.len(), 80 * frame_count, "{name}");
        assert!(actual.iter().all(|value| value.is_finite()), "{name}");
        for &(band, frame, value) in expected {
            let error = (actual[band * frame_count + frame] - value).abs();
            assert!(
                error < 2e-5,
                "{name} band {band} frame {frame}: got {} expected {value}",
                actual[band * frame_count + frame]
            );
        }
    }
}

#[test]
fn native_frontend_empty_silence_and_nonfinite_inputs() {
    let config = super::super::get_model("base").unwrap().config();
    let filters = frontend_filters();
    assert!(pcm_to_mel(MODELSCOPE_LAYOUT, &config, &[], &filters)
        .unwrap()
        .is_empty());
    let silence = pcm_to_mel(MODELSCOPE_LAYOUT, &config, &[0.0; 400], &filters).unwrap();
    assert_eq!(silence.len(), 80 * 3000);
    assert!(silence.iter().all(|value| *value == -1.5));
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(pcm_to_mel(MODELSCOPE_LAYOUT, &config, &[invalid], &filters).is_err());
    }
    assert!(pcm_to_mel(MODELSCOPE_LAYOUT, &config, &[1.0], &filters[..10]).is_err());
    assert!(pcm_to_mel("unknown", &config, &[1.0], &filters).is_err());
}

#[test]
fn legacy_frontend_remains_bit_identical_to_candle() {
    let config = super::super::get_model("base").unwrap().config();
    let filters = frontend_filters();
    let samples = frontend_signal("ordinary");
    let expected =
        candle_transformers::models::whisper::audio::pcm_to_mel(&config, &samples, &filters);
    let actual = pcm_to_mel(LEGACY_LAYOUT, &config, &samples, &filters).unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
}

#[test]
fn mixed_radix_power_matches_numpy_rfft_all_201_bins() {
    let roots: Vec<_> = (0..400)
        .map(|index| {
            let phase = -std::f64::consts::TAU * index as f64 / 400.0;
            (phase.cos(), phase.sin())
        })
        .collect();
    let input: Vec<_> = (0..400)
        .map(|index| {
            let sample = ((index * 37 % 257) as f64 - 128.0) / 512.0;
            sample * 0.5 * (1.0 - (std::f64::consts::TAU * index as f64 / 400.0).cos())
        })
        .collect();
    let spectrum = native_fft(&input, 1, 400, &roots);
    // NumPy 2.2.6 np.abs(np.fft.rfft(input))**2, independent of native_fft.
    let expected: [f64; 201] = [
        f64::from_bits(0x3edcbdf7d5799dc9),
        f64::from_bits(0x3fa360c92fef82d8),
        f64::from_bits(0x3fe3b73c6593144a),
        f64::from_bits(0x40140cfc9f54e5d9),
        f64::from_bits(0x40023a52226cb4eb),
        f64::from_bits(0x3f9edebc62895459),
        f64::from_bits(0x3ff2677c529c1a4b),
        f64::from_bits(0x3feae841e0570065),
        f64::from_bits(0x3f7bd2716bb00ec5),
        f64::from_bits(0x3fdbbf57a07f27eb),
        f64::from_bits(0x3fdfe4293eca4ffc),
        f64::from_bits(0x3f9cdbc4ae6d144c),
        f64::from_bits(0x3fc81160596bafb3),
        f64::from_bits(0x3fd625a5d278d857),
        f64::from_bits(0x3fab822ccc66a885),
        f64::from_bits(0x3fb4b07c1dac799c),
        f64::from_bits(0x3fd06c3401e1856e),
        f64::from_bits(0x3fb3c6987f0b17d5),
        f64::from_bits(0x3f9e7d645a0fdd6a),
        f64::from_bits(0x3fc8f2ca44dc3628),
        f64::from_bits(0x3fb90cf62c8ed125),
        f64::from_bits(0x3f7c98eaf3fe5202),
        f64::from_bits(0x3fc2ec5fb7fd5551),
        f64::from_bits(0x3fbd9be79d7daf9a),
        f64::from_bits(0x3f52545b6baf2bd2),
        f64::from_bits(0x3fbc0c500d3fd51f),
        f64::from_bits(0x3fc0c2ff8240085a),
        f64::from_bits(0x3f7dd44471bab707),
        f64::from_bits(0x3fb3c9597c6a9981),
        f64::from_bits(0x3fc26e66e4dac967),
        f64::from_bits(0x3f98394a1b1f9168),
        f64::from_bits(0x3fa97efdb5a5c876),
        f64::from_bits(0x3fc3d78de953e028),
        f64::from_bits(0x3fa9d2f9155e106d),
        f64::from_bits(0x3f9b988f20d571b9),
        f64::from_bits(0x3fc504f0152cd5b8),
        f64::from_bits(0x3fb7011ddd0662f4),
        f64::from_bits(0x3f841a8a774695e9),
        f64::from_bits(0x3fc5fd611cf24cc5),
        f64::from_bits(0x3fc2daab35da412c),
        f64::from_bits(0x3f589d831a5feb5e),
        f64::from_bits(0x3fc6cace541aeaaf),
        f64::from_bits(0x3fce0762c656802c),
        f64::from_bits(0x3f83ee00f689c45e),
        f64::from_bits(0x3fc781b59703e02a),
        f64::from_bits(0x3fd855b60561e8c2),
        f64::from_bits(0x3fac41c6100b5f2a),
        f64::from_bits(0x3fc85a7a7ceaa8c2),
        f64::from_bits(0x3fe559b8d02c32a0),
        f64::from_bits(0x3fca2d88ed1784f4),
        f64::from_bits(0x3fca2339115287b8),
        f64::from_bits(0x3ff71aab59fe94ef),
        f64::from_bits(0x3fe81f2c63f26468),
        f64::from_bits(0x3fd0b3ce1b174e4c),
        f64::from_bits(0x40193e43db477246),
        f64::from_bits(0x400ab2601671ef8b),
        f64::from_bits(0x4013df54c1b58e6e),
        f64::from_bits(0x4063b127ea2bc2b7),
        f64::from_bits(0x406a00cf0870c887),
        f64::from_bits(0x4028b5db44b97dd3),
        f64::from_bits(0x40072d0125ff60f7),
        f64::from_bits(0x400ae3ced9b317a2),
        f64::from_bits(0x3fe301300ed2f8a5),
        f64::from_bits(0x3fd821c469d03ea3),
        f64::from_bits(0x3ff1f527af9afb95),
        f64::from_bits(0x3fd5226bb985a4de),
        f64::from_bits(0x3fb3fee722cd5528),
        f64::from_bits(0x3fe16d5cf2d6d5d1),
        f64::from_bits(0x3fd0af76bfa2839e),
        f64::from_bits(0x3f871d3e8d4b3cee),
        f64::from_bits(0x3fd36d8e1ef9aae9),
        f64::from_bits(0x3fccfdda311118be),
        f64::from_bits(0x3f5c5ae4e18cdfb6),
        f64::from_bits(0x3fc6a6d3165e68c6),
        f64::from_bits(0x3fc9ff2f10d2b35c),
        f64::from_bits(0x3f88414af3fc8840),
        f64::from_bits(0x3fba05bea452c1f5),
        f64::from_bits(0x3fc780a3865dd4ce),
        f64::from_bits(0x3f9ef85b01c768fc),
        f64::from_bits(0x3fabb028077785ac),
        f64::from_bits(0x3fc52bb1443670bd),
        f64::from_bits(0x3fab1093a64708f9),
        f64::from_bits(0x3f98a1ba45157774),
        f64::from_bits(0x3fc2d8155826760d),
        f64::from_bits(0x3fb405b5132bb621),
        f64::from_bits(0x3f7c7dde6b371b22),
        f64::from_bits(0x3fc0701e3b36e103),
        f64::from_bits(0x3fbb219300748999),
        f64::from_bits(0x3f517943336a6efa),
        f64::from_bits(0x3fbbcda78ada447d),
        f64::from_bits(0x3fc17c060110bb2d),
        f64::from_bits(0x3f7bb165d522841e),
        f64::from_bits(0x3fb66944592c00a4),
        f64::from_bits(0x3fc5ec69ba4900a3),
        f64::from_bits(0x3f9b653445213797),
        f64::from_bits(0x3fb0b05b1e344e56),
        f64::from_bits(0x3fcb333208f8b9c7),
        f64::from_bits(0x3fb1527f177e3efb),
        f64::from_bits(0x3fa577a3d54f9461),
        f64::from_bits(0x3fd0fbc8b497c2f3),
        f64::from_bits(0x3fc284e2c0e88932),
        f64::from_bits(0x3f93dd8d288b2e4a),
        f64::from_bits(0x3fd5dcfb42ac3236),
        f64::from_bits(0x3fd2f573ee5fb9c0),
        f64::from_bits(0x3f6e8cc1131aa797),
        f64::from_bits(0x3fde59592e2bb927),
        f64::from_bits(0x3fe48cf64861f31f),
        f64::from_bits(0x3f954c98c5c9eea7),
        f64::from_bits(0x3fe97ad78f7ecde8),
        f64::from_bits(0x3ffbc1e0a760f9f7),
        f64::from_bits(0x3fccfee2b081fac7),
        f64::from_bits(0x4004b44c0871f55e),
        f64::from_bits(0x4025038850b45593),
        f64::from_bits(0x4000d8335910a48a),
        f64::from_bits(0x40201331e2a85f81),
        f64::from_bits(0x404e42abcb929d3e),
        f64::from_bits(0x403b562552c76651),
        f64::from_bits(0x3fa186faa50f0f20),
        f64::from_bits(0x4006b7ca3878d7ce),
        f64::from_bits(0x3fff26fa62f801a6),
        f64::from_bits(0x3f9489c3f1097f75),
        f64::from_bits(0x3fe7540cf69bb139),
        f64::from_bits(0x3fea4fc5742a04b6),
        f64::from_bits(0x3fa846e471bbb7a4),
        f64::from_bits(0x3fd0f154bd0f1b09),
        f64::from_bits(0x3fdf242ab7781111),
        f64::from_bits(0x3fb31243cd6f1094),
        f64::from_bits(0x3fba148e2496443c),
        f64::from_bits(0x3fd4eb8f69585c57),
        f64::from_bits(0x3fb8bc3c8bb0011c),
        f64::from_bits(0x3fa1a6e495acb3a2),
        f64::from_bits(0x3fcd9b4e30bbd31e),
        f64::from_bits(0x3fbd3578919485f7),
        f64::from_bits(0x3f7e4c2abcece9b2),
        f64::from_bits(0x3fc53a0dd8f992a1),
        f64::from_bits(0x3fc0572b82a6a5ae),
        f64::from_bits(0x3f534fba5054d3ba),
        f64::from_bits(0x3fbdf9543ecc2291),
        f64::from_bits(0x3fc1a640ddd8c814),
        f64::from_bits(0x3f801814d1448199),
        f64::from_bits(0x3fb438e4a9ee2709),
        f64::from_bits(0x3fc2945c341854c9),
        f64::from_bits(0x3f98adbe68d43ceb),
        f64::from_bits(0x3fa8f0655909ba79),
        f64::from_bits(0x3fc3289d5a06727d),
        f64::from_bits(0x3fa90cb9e277881f),
        f64::from_bits(0x3f99c467d5c3ff15),
        f64::from_bits(0x3fc365d947ed4fcb),
        f64::from_bits(0x3fb542fe40c505cf),
        f64::from_bits(0x3f81c47d5495dfb4),
        f64::from_bits(0x3fc34af405342108),
        f64::from_bits(0x3fc08798576078e4),
        f64::from_bits(0x3f5520653ee47888),
        f64::from_bits(0x3fc2d29aa2c7cfe7),
        f64::from_bits(0x3fc8b6a9c4943b2f),
        f64::from_bits(0x3f80d2b6fd57cc3e),
        f64::from_bits(0x3fc1f27511f6bff2),
        f64::from_bits(0x3fd26b14915dc05f),
        f64::from_bits(0x3fa59a17b31c51b0),
        f64::from_bits(0x3fc09aacd5f6a033),
        f64::from_bits(0x3fdc7ad32557c6ec),
        f64::from_bits(0x3fc1b0308e39fe99),
        f64::from_bits(0x3fbd784d4f66dc94),
        f64::from_bits(0x3fe8613624d969fe),
        f64::from_bits(0x3fdab49481625e92),
        f64::from_bits(0x3fb913b56a7fbeab),
        f64::from_bits(0x3ffb103cee042952),
        f64::from_bits(0x3ff876ee3ead965f),
        f64::from_bits(0x3fbc81db46e13dd3),
        f64::from_bits(0x402458f804288cb9),
        f64::from_bits(0x40294e2f7cede0cc),
        f64::from_bits(0x3ff446bf714664ee),
        f64::from_bits(0x4026c7a794e189f8),
        f64::from_bits(0x403b3ef8fe8af889),
        f64::from_bits(0x400c41809b2ba757),
        f64::from_bits(0x3feae6ba2c469399),
        f64::from_bits(0x4003f13bea157a8c),
        f64::from_bits(0x3fe67d551f3793f2),
        f64::from_bits(0x3fbfab69ede0b410),
        f64::from_bits(0x3fec221259648fbb),
        f64::from_bits(0x3fda9a76a4642498),
        f64::from_bits(0x3f8ec22e5bb7f089),
        f64::from_bits(0x3fdacf9361913b98),
        f64::from_bits(0x3fd3fd2500a43ff6),
        f64::from_bits(0x3f637e37c3430294),
        f64::from_bits(0x3fcc53d030a68657),
        f64::from_bits(0x3fd0566669da6102),
        f64::from_bits(0x3f8dece99116023b),
        f64::from_bits(0x3fbe48b721556f58),
        f64::from_bits(0x3fcb99f0310051e4),
        f64::from_bits(0x3fa1c89916e105ce),
        f64::from_bits(0x3fae5e5b1bb5b347),
        f64::from_bits(0x3fc78a51f4b11c0a),
        f64::from_bits(0x3fad78474351344d),
        f64::from_bits(0x3f998e4f1fc31ff4),
        f64::from_bits(0x3fc3fbb26fd72e1d),
        f64::from_bits(0x3fb4d2cd35f8ba67),
        f64::from_bits(0x3f7bbbf488005f1c),
        f64::from_bits(0x3fc0af0e94b5d662),
        f64::from_bits(0x3fbb0738c32aae1f),
        f64::from_bits(0x3f51602580b62c07),
    ];
    for (index, ((real, imaginary), expected)) in spectrum.iter().zip(expected).enumerate() {
        let actual = real * real + imaginary * imaginary;
        assert!(
            (actual - expected).abs() <= 1e-9 * (1.0 + expected.abs()),
            "FFT bin {index}: got {actual} expected {expected}"
        );
    }
}
