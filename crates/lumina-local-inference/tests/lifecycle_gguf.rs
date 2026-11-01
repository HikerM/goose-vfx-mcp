use lumina_local_inference::lifecycle::*;
use std::io::Cursor;

struct Wire {
    bytes: Vec<u8>,
    big: bool,
}

impl Wire {
    fn u32(&mut self, value: u32) {
        self.bytes.extend(if self.big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        });
    }
    fn u64(&mut self, value: u64) {
        self.bytes.extend(if self.big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        });
    }
    fn string(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.bytes.extend(value.as_bytes());
    }
}

#[derive(Clone)]
struct Tensor<'a> {
    name: &'a str,
    dimensions: Vec<u64>,
    type_id: u32,
    offset: u64,
}

fn tensor() -> Tensor<'static> {
    Tensor {
        name: "test.weight",
        dimensions: vec![32, 2],
        type_id: 2,
        offset: 0,
    }
}

fn document(
    big: bool,
    metadata: &[(&str, u32, Vec<u8>)],
    tensors: &[Tensor],
    data_bytes: usize,
) -> Vec<u8> {
    let mut wire = Wire {
        bytes: b"GGUF".to_vec(),
        big,
    };
    wire.u32(3);
    wire.u64(tensors.len() as u64);
    wire.u64(metadata.len() as u64);
    for (key, type_id, bytes) in metadata {
        wire.string(key);
        wire.u32(*type_id);
        wire.bytes.extend(bytes);
    }
    for tensor in tensors {
        wire.string(tensor.name);
        wire.u32(tensor.dimensions.len() as u32);
        for dimension in &tensor.dimensions {
            wire.u64(*dimension);
        }
        wire.u32(tensor.type_id);
        wire.u64(tensor.offset);
    }
    if !tensors.is_empty() {
        wire.bytes
            .resize(wire.bytes.len().div_ceil(32) * 32 + data_bytes, 0);
    }
    wire.bytes
}

fn string_value(value: &str, big: bool) -> Vec<u8> {
    let mut wire = Wire {
        bytes: Vec::new(),
        big,
    };
    wire.string(value);
    wire.bytes
}

fn inspect(bytes: &[u8]) -> anyhow::Result<GgufInspection> {
    inspect_gguf(
        Cursor::new(bytes),
        bytes.len() as u64,
        GgufReadLimits::default(),
    )
}

#[test]
fn parses_endian_metadata_quantization_and_tensor_parameters() {
    for big in [false, true] {
        let bytes = document(
            big,
            &[("general.architecture", 8, string_value("qwen2", big))],
            &[tensor()],
            36,
        );
        let parsed = inspect(&bytes).unwrap();
        assert_eq!(parsed.architecture(), Some("qwen2"));
        assert_eq!(
            parsed.byte_order,
            if big {
                GgufByteOrder::BigEndian
            } else {
                GgufByteOrder::LittleEndian
            }
        );
        assert_eq!(parsed.stored_tensor_elements, 64);
        assert_eq!(parsed.encoded_tensor_bytes, 36);
        assert_eq!(parsed.tensors[0].quantization, "q4_0");
        assert_eq!(parsed.data_offset % 32, 0);
        assert_eq!(
            parsed.header_digest,
            Sha256Digest::of_bytes(&bytes[..parsed.header_bytes as usize])
        );
    }
}

#[test]
fn bounds_array_storage_but_hashes_every_token() {
    let mut array = Wire {
        bytes: Vec::new(),
        big: false,
    };
    array.u32(8);
    array.u64(300);
    for index in 0..300 {
        array.string(&format!("token-{index}"));
    }
    let bytes = document(false, &[("tokenizer.ggml.tokens", 9, array.bytes)], &[], 0);
    let parsed = inspect(&bytes).unwrap();
    let GgufValue::Array {
        count,
        retained_values,
        encoded_digest,
        ..
    } = &parsed.metadata["tokenizer.ggml.tokens"]
    else {
        panic!("not array")
    };
    assert_eq!(*count, 300);
    assert_eq!(retained_values.len(), 256);
    let mut changed = bytes.clone();
    *changed.last_mut().unwrap() = b'X';
    let modified = inspect(&changed).unwrap();
    let GgufValue::Array {
        encoded_digest: modified_digest,
        ..
    } = &modified.metadata["tokenizer.ggml.tokens"]
    else {
        panic!("not array")
    };
    assert_ne!(encoded_digest, modified_digest);
}

#[test]
fn rejects_truncation_invalid_versions_and_forged_counts() {
    let bytes = document(
        false,
        &[("general.architecture", 8, string_value("qwen2", false))],
        &[tensor()],
        36,
    );
    let header_bytes = inspect(&bytes).unwrap().header_bytes as usize;
    for end in 0..header_bytes {
        assert!(
            inspect_gguf(
                Cursor::new(&bytes[..end]),
                bytes.len() as u64,
                GgufReadLimits::default()
            )
            .is_err(),
            "accepted truncated header {end}"
        );
    }
    let mut changed = bytes.clone();
    changed[4..8].copy_from_slice(&99u32.to_le_bytes());
    assert!(inspect(&changed).is_err());
    changed = bytes.clone();
    changed[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(inspect(&changed).is_err());
    changed = bytes.clone();
    changed[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(inspect(&changed).is_err());
    changed = bytes.clone();
    changed[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(inspect(&changed).is_err());
    assert!(inspect(&bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn rejects_tensor_overlaps_offsets_invalid_blocks_and_overflow() {
    let cases = [
        Tensor {
            offset: 1,
            ..tensor()
        },
        Tensor {
            offset: u64::MAX - 31,
            ..tensor()
        },
        Tensor {
            dimensions: vec![33, 2],
            ..tensor()
        },
        Tensor {
            dimensions: vec![32, 0],
            ..tensor()
        },
        Tensor {
            dimensions: vec![32, u64::MAX],
            ..tensor()
        },
        Tensor {
            dimensions: vec![32, i64::MAX as u64],
            ..tensor()
        },
        Tensor {
            dimensions: vec![1, 1, 1, 1, 1],
            ..tensor()
        },
        Tensor {
            type_id: 4,
            ..tensor()
        },
        Tensor {
            type_id: 42,
            ..tensor()
        },
    ];
    for tensor in cases {
        assert!(inspect(&document(false, &[], &[tensor], 36)).is_err());
    }
    let overlapping = Tensor {
        name: "second",
        ..tensor()
    };
    assert!(inspect(&document(false, &[], &[tensor(), overlapping], 72)).is_err());
    assert!(inspect(&document(false, &[], &[tensor(), tensor()], 72)).is_err());
    let separate = Tensor {
        name: "second",
        offset: 64,
        ..tensor()
    };
    assert!(inspect(&document(false, &[], &[tensor(), separate], 100)).is_ok());
}

#[test]
fn rejects_duplicate_keys_nested_arrays_invalid_bool_and_budget_exhaustion() {
    let key = ("general.architecture", 8, string_value("qwen2", false));
    assert!(inspect(&document(false, &[key.clone(), key.clone()], &[], 0)).is_err());
    assert!(inspect(&document(false, &[("bad", 7, vec![2])], &[], 0)).is_err());
    let mut array = Wire {
        bytes: Vec::new(),
        big: false,
    };
    array.u32(9);
    array.u64(1);
    assert!(inspect(&document(false, &[("nested", 9, array.bytes)], &[], 0)).is_err());
    let bytes = document(false, &[key], &[tensor()], 36);
    let limits = GgufReadLimits {
        max_header_bytes: 32,
        ..GgufReadLimits::default()
    };
    assert!(inspect_gguf(Cursor::new(&bytes), bytes.len() as u64, limits).is_err());
    let mut array = Wire {
        bytes: Vec::new(),
        big: false,
    };
    array.u32(0);
    array.u64(u64::MAX);
    assert!(inspect(&document(false, &[("huge", 9, array.bytes)], &[], 0)).is_err());
}

#[test]
fn validates_split_metadata_without_guessing_from_filename() {
    let metadata = [
        ("split.no", 2, 0u16.to_le_bytes().to_vec()),
        ("split.count", 2, 2u16.to_le_bytes().to_vec()),
        ("split.tensors.count", 5, 10i32.to_le_bytes().to_vec()),
    ];
    let parsed = inspect(&document(false, &metadata, &[tensor()], 36)).unwrap();
    assert_eq!(
        parsed.shard,
        Some(GgufShard {
            index: 0,
            count: 2,
            total_tensors: 10
        })
    );
    assert!(inspect(&document(false, &metadata[..2], &[tensor()], 36)).is_err());
    let mut invalid = metadata.clone();
    invalid[0].2 = 2u16.to_le_bytes().to_vec();
    assert!(inspect(&document(false, &invalid, &[tensor()], 36)).is_err());
}

#[test]
fn requires_complete_consistent_shards_and_disjoint_tensor_names() {
    let shard = |index: u16, name: &str, architecture: &str| {
        let metadata = [
            ("split.no", 2, index.to_le_bytes().to_vec()),
            ("split.count", 2, 2u16.to_le_bytes().to_vec()),
            ("split.tensors.count", 5, 2i32.to_le_bytes().to_vec()),
            ("general.architecture", 8, string_value(architecture, false)),
        ];
        inspect(&document(
            false,
            &metadata,
            &[Tensor { name, ..tensor() }],
            36,
        ))
        .unwrap()
    };
    let first = shard(0, "first", "qwen2");
    let second = shard(1, "second", "qwen2");
    assert!(validate_gguf_shard_set(&[first.clone(), second.clone()]).is_ok());
    let mut secondary_without_model_metadata = second.clone();
    secondary_without_model_metadata
        .metadata
        .remove("general.architecture");
    assert!(validate_gguf_shard_set(&[secondary_without_model_metadata, first.clone()]).is_ok());
    assert!(validate_gguf_shard_set(std::slice::from_ref(&first)).is_err());
    assert!(validate_gguf_shard_set(&[first.clone(), first.clone()]).is_err());
    assert!(validate_gguf_shard_set(&[first.clone(), shard(1, "first", "qwen2")]).is_err());
    assert!(validate_gguf_shard_set(&[first.clone(), shard(1, "second", "llama")]).is_err());
    let mut wrong_total = second;
    wrong_total.shard.as_mut().unwrap().total_tensors = 3;
    assert!(validate_gguf_shard_set(&[first.clone(), wrong_total]).is_err());
    let unsplit = inspect(&document(false, &[], &[tensor()], 36)).unwrap();
    assert!(validate_gguf_shard_set(std::slice::from_ref(&unsplit)).is_ok());
    assert!(validate_gguf_shard_set(&[unsplit.clone(), unsplit.clone()]).is_err());
    assert!(validate_gguf_shard_set(&[first, unsplit]).is_err());
}

#[test]
fn parses_all_locked_quantization_layouts_against_native_sizes() {
    for type_id in 0u32..42 {
        if matches!(type_id, 4 | 5 | 31..=33 | 36..=38) {
            continue;
        }
        // Only valid, locked ggml enum values reach the native table accessors.
        let (block, size) = unsafe {
            (
                llama_cpp_sys_2::ggml_blck_size(type_id as i32),
                llama_cpp_sys_2::ggml_type_size(type_id as i32),
            )
        };
        let tensor = Tensor {
            type_id,
            dimensions: vec![block as u64],
            ..tensor()
        };
        let parsed = inspect(&document(false, &[], &[tensor], size)).unwrap();
        assert_eq!(
            parsed.encoded_tensor_bytes, size as u64,
            "wire type {type_id}"
        );
    }
}

#[test]
fn bounded_container_profiles_distinguish_diffusion_shapes_from_llama_limits() {
    let long_name = "cond_stage_model.transformer.text_model.embeddings.position_embedding.weight";
    let extended = Tensor {
        name: long_name,
        dimensions: vec![2, 3, 1, 1, 1],
        type_id: 0,
        offset: 0,
    };
    let bytes = document(false, &[], &[extended], 24);
    let parsed = inspect(&bytes).unwrap();
    assert_eq!(parsed.tensors[0].dimensions.len(), 5);
    assert_eq!(parsed.stored_tensor_elements, 6);
    assert!(inspect_gguf(
        Cursor::new(&bytes),
        bytes.len() as u64,
        GgufReadLimits::for_llama_cpp()
    )
    .is_err());
    let five_dimensional = Tensor {
        dimensions: vec![2, 3, 1, 1, 1],
        type_id: 0,
        ..tensor()
    };
    let bytes = document(false, &[], &[five_dimensional], 24);
    assert!(inspect(&bytes).is_ok());
    assert!(inspect_gguf(
        Cursor::new(&bytes),
        bytes.len() as u64,
        GgufReadLimits::for_llama_cpp()
    )
    .is_err());
    let scalar = Tensor {
        dimensions: vec![],
        type_id: 0,
        ..tensor()
    };
    assert_eq!(
        inspect(&document(false, &[], &[scalar], 4))
            .unwrap()
            .stored_tensor_elements,
        1
    );
    let too_many = Tensor {
        dimensions: vec![1; 9],
        type_id: 0,
        ..tensor()
    };
    assert!(inspect(&document(false, &[], &[too_many], 4)).is_err());
}

#[test]
fn hashes_large_numeric_metadata_without_retaining_it_and_validates_bool_tails() {
    let count = 3_000_000u64;
    let mut array = Wire {
        bytes: Vec::new(),
        big: false,
    };
    array.u32(0);
    array.u64(count);
    array.bytes.resize(array.bytes.len() + count as usize, 0x41);
    let bytes = document(false, &[("embedded.data", 9, array.bytes)], &[], 0);
    let parsed = inspect(&bytes).unwrap();
    let GgufValue::Array {
        retained_values,
        count: actual_count,
        encoded_digest,
        ..
    } = &parsed.metadata["embedded.data"]
    else {
        panic!("not array")
    };
    assert_eq!(*actual_count, count);
    assert_eq!(retained_values.len(), 256);
    assert_eq!(
        *encoded_digest,
        Sha256Digest::of_bytes(&vec![0x41; count as usize])
    );
    let limits = GgufReadLimits {
        max_header_bytes: 1024 * 1024,
        ..GgufReadLimits::default()
    };
    assert!(inspect_gguf(Cursor::new(&bytes), bytes.len() as u64, limits).is_err());
    let mut booleans = Wire {
        bytes: Vec::new(),
        big: false,
    };
    booleans.u32(7);
    booleans.u64(300);
    booleans.bytes.resize(312, 0);
    *booleans.bytes.last_mut().unwrap() = 2;
    assert!(inspect(&document(
        false,
        &[("bad.bool.tail", 9, booleans.bytes)],
        &[],
        0
    ))
    .is_err());
}

#[test]
#[ignore = "requires LUMINA_REAL_GGUF_PATH; reads actual model metadata without copying weights"]
fn inspects_actual_gguf_and_writes_evidence() {
    let path = std::path::PathBuf::from(std::env::var("LUMINA_REAL_GGUF_PATH").unwrap());
    let parsed = GgufInspection::inspect_file(&path, GgufReadLimits::default()).unwrap();
    assert!(parsed.architecture().is_some());
    assert!(!parsed.tensors.is_empty());
    assert!(parsed.stored_tensor_elements > 0);
    let root = std::path::PathBuf::from(std::env::var("LUMINA_PATH_ROOT").unwrap());
    assert!(root.is_absolute());
    std::fs::create_dir_all(root.join("evidence")).unwrap();
    std::fs::write(
        root.join("evidence/gguf-inspection.json"),
        serde_json::to_vec_pretty(&parsed).unwrap(),
    )
    .unwrap();
    println!(
        "architecture={:?}, tensors={}, stored elements={}, encoded bytes={}, header bytes={}",
        parsed.architecture(),
        parsed.tensors.len(),
        parsed.stored_tensor_elements,
        parsed.encoded_tensor_bytes,
        parsed.header_bytes
    );
}
