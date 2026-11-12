use flate2::{write::ZlibEncoder, Compression};
use lumina_local_inference::lifecycle::*;
use serde_json::Value;
use std::io::Write;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const MAIN_BYTES: u64 = 833_592_736;
const PROJECTOR_BYTES: u64 = 204_987_104;
const FROZEN_TEXT_PLAN: &str = include_str!("fixtures/lifecycle-vision/frozen-text-v2-plan.json");

const RGB_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 96, 96, 96, 0, 0, 0, 4, 0,
    1, 246, 23, 56, 85, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const RGBA_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2, 0, 0, 5, 0, 1,
    122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn png_header(width: u32, height: u32, color_type: u8) -> [u8; 13] {
    let mut header = [0; 13];
    header[..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    header[8] = 8;
    header[9] = color_type;
    header
}

fn png_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in chunks {
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(*kind);
        bytes.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(*kind);
        crc.update(data);
        bytes.extend_from_slice(&crc.finalize().to_be_bytes());
    }
    bytes
}

fn zlib_bytes(decoded: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(decoded).unwrap();
    encoder.finish().unwrap()
}

fn png_with_idat(width: u32, height: u32, color_type: u8, compressed: &[u8]) -> Vec<u8> {
    png_chunks(&[
        (b"IHDR", &png_header(width, height, color_type)),
        (b"IDAT", compressed),
        (b"IEND", &[]),
    ])
}

fn recorded_component(json: &str) -> ComponentObservation {
    let fixture: Value = serde_json::from_str(json).unwrap();
    serde_json::from_value(fixture["observation"].clone()).unwrap()
}

fn components() -> Vec<ComponentObservation> {
    let main = recorded_component(include_str!(
        "fixtures/lifecycle-vision/main-observation.json"
    ));
    let mut projector = recorded_component(include_str!(
        "fixtures/lifecycle-vision/projector-observation.json"
    ));
    // The source diagnostic defaults to Main; role selection leaves its recorded content intact.
    projector.role = ComponentRole::VisionProjector;
    vec![main, projector]
}

fn hardware() -> HardwareResourceSnapshot {
    HardwareResourceSnapshot {
        observed_at: 100,
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "CPU".into(),
            name: "synthetic-test-cpu".into(),
            description: "Deterministic resource boundaries, not a hardware measurement".into(),
            kind: "cpu".into(),
            total_bytes: 16 * GIB,
            free_bytes: 8 * GIB,
        }],
        storage_volume: "synthetic-test-volume".into(),
        disk_free_bytes: 16 * GIB,
    }
}

fn bind_components(envelope: &mut AdapterResourceEnvelope, components: &[ComponentObservation]) {
    envelope.component_digests = components
        .iter()
        .map(|component| component.digest().unwrap())
        .collect();
}

fn assert_unknown(components: &[ComponentObservation], envelope: &AdapterResourceEnvelope) {
    let budget = assess_resource_budget(components, envelope, &hardware()).unwrap();
    assert_eq!(budget.compatibility, HardwareCompatibility::Unknown);
    assert!(budget.host_memory_required_bytes.is_none());
    assert!(!budget.blocking_reasons.is_empty());
}

#[test]
fn native_resize_can_exceed_the_1024_cli_token_hint() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    for (width, height, resized_width, resized_height, image_tokens) in [
        (256, 256, 1024, 1024, 1024),
        (1024, 1024, 1024, 1024, 1024),
        (128, 1024, 384, 2912, 1092),
        (1024, 128, 2912, 384, 1092),
        (1, 1023, 64, 32768, 2048),
        (1023, 1, 32768, 64, 2048),
    ] {
        let allocations = workload
            .assess_inputs(&[VisionImageInput {
                width,
                height,
                encoded_bytes: 69,
            }])
            .unwrap();
        assert_eq!(
            allocations,
            vec![VisionImageAllocation {
                resized_width,
                resized_height,
                image_tokens,
            }],
            "native allocation for {width}x{height}"
        );
    }
}

#[test]
fn input_bounds_include_the_entire_conversation() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let valid = VisionImageInput {
        width: 1024,
        height: 1024,
        encoded_bytes: 8 * MIB,
    };
    assert!(workload.assess_inputs(&[valid]).is_ok());
    assert!(workload.assess_inputs(&[]).is_err());
    assert!(workload.assess_inputs(&[valid, valid]).is_err());
    for invalid in [
        VisionImageInput { width: 0, ..valid },
        VisionImageInput { height: 0, ..valid },
        VisionImageInput {
            width: 1025,
            ..valid
        },
        VisionImageInput {
            height: 1025,
            ..valid
        },
        VisionImageInput {
            width: u32::MAX,
            ..valid
        },
        VisionImageInput {
            encoded_bytes: 0,
            ..valid
        },
        VisionImageInput {
            encoded_bytes: 8 * MIB + 1,
            ..valid
        },
    ] {
        assert!(workload.assess_inputs(&[invalid]).is_err(), "{invalid:?}");
    }
}

#[test]
fn inspected_png_bytes_and_digest_are_frozen() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    for original in [RGB_PNG, RGBA_PNG] {
        let mut submitted = original.to_vec();
        let inspected = workload.inspect_images(&[&submitted]).unwrap();
        submitted.fill(0);
        assert_eq!(inspected[0].bytes(), original);
        assert_eq!(inspected[0].sha256(), &Sha256Digest::of_bytes(original));
        assert_eq!(
            inspected[0].allocation(),
            VisionImageAllocation {
                resized_width: 1024,
                resized_height: 1024,
                image_tokens: 1024,
            }
        );
    }
    assert!(workload.inspect_images(&[]).is_err());
    assert!(workload.inspect_images(&[RGB_PNG, RGBA_PNG]).is_err());
}

#[test]
fn png_header_gate_rejects_unbounded_or_unmeasured_formats() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    for (offset, replacement) in [
        (0, 0),
        (11, 12),
        (12, b'J'),
        (24, 16),
        (25, 0),
        (25, 3),
        (26, 1),
        (27, 1),
        (28, 1),
    ] {
        let mut bytes = RGB_PNG.to_vec();
        bytes[offset] = replacement;
        if offset >= 24 {
            let crc = crc32fast::hash(&bytes[12..29]);
            bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        }
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "offset {offset}"
        );
    }
    for (width, height) in [(0, 1), (1, 0), (1025, 1), (1, 1025), (u32::MAX, u32::MAX)] {
        let mut bytes = RGB_PNG.to_vec();
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        let crc = crc32fast::hash(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "{width}x{height}"
        );
    }
    for length in [0, 8, 24, 32] {
        assert!(workload.inspect_images(&[&RGB_PNG[..length]]).is_err());
    }
    assert!(workload.inspect_images(&[b"not a PNG"]).is_err());
    let mut oversized = RGB_PNG.to_vec();
    oversized.resize((8 * MIB + 1) as usize, 0);
    assert!(workload.inspect_images(&[&oversized]).is_err());
}

#[test]
fn png_inflate_must_match_the_declared_pixel_budget() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let valid = png_with_idat(1, 1, 2, &zlib_bytes(&[0; 4]));
    assert!(workload.inspect_images(&[&valid]).is_ok());
    for decoded_len in [0, 3, 5, 64 * 1024] {
        let compressed = zlib_bytes(&vec![0; decoded_len]);
        let bytes = png_with_idat(1, 1, 2, &compressed);
        assert!(bytes.len() < 1024);
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "1x1 RGB declares four decoded bytes, but IDAT expands to {decoded_len}"
        );
    }
}

#[test]
fn png_zlib_stream_must_end_exactly_at_the_idat_boundary() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let compressed = zlib_bytes(&[0; 4]);
    let mut trailing = compressed.clone();
    trailing.push(0);
    let mut concatenated = compressed.clone();
    concatenated.extend_from_slice(&compressed);
    let mut invalid_adler = compressed.clone();
    *invalid_adler.last_mut().unwrap() ^= 1;
    for (name, idat) in [
        ("truncated zlib header", &compressed[..1]),
        (
            "truncated zlib checksum",
            &compressed[..compressed.len() - 1],
        ),
        ("trailing byte", trailing.as_slice()),
        ("concatenated zlib streams", concatenated.as_slice()),
        ("invalid zlib checksum", invalid_adler.as_slice()),
    ] {
        let bytes = png_with_idat(1, 1, 2, idat);
        assert!(workload.inspect_images(&[&bytes]).is_err(), "{name}");
    }
}

#[test]
fn png_chunk_crcs_are_checked_for_header_image_and_end() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let compressed = zlib_bytes(&[0; 4]);
    let valid = png_with_idat(1, 1, 2, &compressed);
    for crc_offset in [29, 33 + 8 + compressed.len(), valid.len() - 4] {
        let mut bytes = valid.clone();
        bytes[crc_offset] ^= 1;
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "CRC at {crc_offset}"
        );
    }
}

#[test]
fn png_requires_ordered_complete_chunks_and_known_critical_types() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let header = png_header(1, 1, 2);
    let compressed = zlib_bytes(&[0; 4]);
    let split = compressed.len() / 2;
    let cases = [
        (
            "missing IEND",
            png_chunks(&[(b"IHDR", &header), (b"IDAT", &compressed)]),
        ),
        (
            "missing IDAT",
            png_chunks(&[(b"IHDR", &header), (b"IEND", &[])]),
        ),
        (
            "duplicate IHDR",
            png_chunks(&[
                (b"IHDR", &header),
                (b"IHDR", &header),
                (b"IDAT", &compressed),
                (b"IEND", &[]),
            ]),
        ),
        (
            "IHDR after IDAT",
            png_chunks(&[
                (b"IHDR", &header),
                (b"IDAT", &compressed),
                (b"IHDR", &header),
                (b"IEND", &[]),
            ]),
        ),
        (
            "IDAT before IHDR",
            png_chunks(&[(b"IDAT", &compressed), (b"IHDR", &header), (b"IEND", &[])]),
        ),
        (
            "nonempty IEND",
            png_chunks(&[(b"IHDR", &header), (b"IDAT", &compressed), (b"IEND", &[0])]),
        ),
        (
            "duplicate IEND",
            png_chunks(&[
                (b"IHDR", &header),
                (b"IDAT", &compressed),
                (b"IEND", &[]),
                (b"IEND", &[]),
            ]),
        ),
        (
            "unknown critical chunk",
            png_chunks(&[
                (b"IHDR", &header),
                (b"ABCD", &[]),
                (b"IDAT", &compressed),
                (b"IEND", &[]),
            ]),
        ),
        (
            "noncontiguous IDAT",
            png_chunks(&[
                (b"IHDR", &header),
                (b"IDAT", &compressed[..split]),
                (b"tEXt", b"key\0value"),
                (b"IDAT", &compressed[split..]),
                (b"IEND", &[]),
            ]),
        ),
    ];
    for (name, bytes) in cases {
        assert!(workload.inspect_images(&[&bytes]).is_err(), "{name}");
    }
    let mut trailing = png_with_idat(1, 1, 2, &compressed);
    trailing.push(0);
    assert!(workload.inspect_images(&[&trailing]).is_err());
}

#[test]
fn png_accepts_contiguous_idat_chunks_with_ancillary_metadata() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let header = png_header(1, 1, 2);
    let compressed = zlib_bytes(&[0; 4]);
    for split in 1..compressed.len() {
        let bytes = png_chunks(&[
            (b"IHDR", &header),
            (b"tEXt", b"key\0value"),
            (b"IDAT", &compressed[..split]),
            (b"IDAT", &compressed[split..]),
            (b"tEXt", b"other\0value"),
            (b"IEND", &[]),
        ]);
        let images = workload.inspect_images(&[&bytes]).unwrap();
        assert_eq!(images[0].bytes(), bytes.as_slice());
        assert_eq!(images[0].sha256(), &Sha256Digest::of_bytes(&bytes));
    }
}

#[test]
fn png_checks_filter_bytes_on_every_scanline() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    for filter in 0..=4 {
        let pixels = [filter, 0, 0, 0, filter, 0, 0, 0];
        let bytes = png_with_idat(1, 2, 2, &zlib_bytes(&pixels));
        assert!(
            workload.inspect_images(&[&bytes]).is_ok(),
            "valid filter {filter}"
        );
    }
    for (first, second) in [(5, 0), (0, 5), (255, 0), (0, 255)] {
        let pixels = [first, 0, 0, 0, second, 0, 0, 0];
        let bytes = png_with_idat(1, 2, 2, &zlib_bytes(&pixels));
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "filters {first}, {second}"
        );
    }
}

#[test]
fn png_palette_and_transparency_chunks_match_the_color_type() {
    let workload = VisionResourceWorkload::measured_qwen35_cpu();
    let rgb_header = png_header(1, 1, 2);
    let rgb_data = zlib_bytes(&[0; 4]);
    for palette_len in [3, 768] {
        let bytes = png_chunks(&[
            (b"IHDR", &rgb_header),
            (b"PLTE", &vec![0; palette_len]),
            (b"tRNS", &[0; 6]),
            (b"IDAT", &rgb_data),
            (b"IEND", &[]),
        ]);
        assert!(workload.inspect_images(&[&bytes]).is_ok());
    }
    for palette_len in [4, 771] {
        let bytes = png_chunks(&[
            (b"IHDR", &rgb_header),
            (b"PLTE", &vec![0; palette_len]),
            (b"IDAT", &rgb_data),
            (b"IEND", &[]),
        ]);
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "PLTE length {palette_len}"
        );
    }
    for (color_type, decoded_len, transparency_len) in [(2, 4, 5), (2, 4, 7), (6, 5, 0), (6, 5, 6)]
    {
        let bytes = png_chunks(&[
            (b"IHDR", &png_header(1, 1, color_type)),
            (b"tRNS", &vec![0; transparency_len]),
            (b"IDAT", &zlib_bytes(&vec![0; decoded_len])),
            (b"IEND", &[]),
        ]);
        assert!(
            workload.inspect_images(&[&bytes]).is_err(),
            "color {color_type}, tRNS length {transparency_len}"
        );
    }
}

#[test]
fn prompt_capacity_includes_complete_multimodal_input_and_output() {
    let mut workload = baseline_vision_resource_envelope(&components())
        .unwrap()
        .workload;
    assert!(workload.validate_vision_prompt(3072, 1024).is_ok());
    for (input, output) in [(3073, 1024), (4096, 1), (u64::MAX, 1), (0, 1), (1, 0)] {
        assert!(workload.validate_vision_prompt(input, output).is_err());
    }
    workload.vision = None;
    assert!(workload.validate_vision_prompt(1, 1).is_err());
}

#[test]
fn paired_weights_are_charged_without_projector_decoder_cache() {
    let components = components();
    let envelope = baseline_vision_resource_envelope(&components).unwrap();
    let budget = assess_resource_budget(&components, &envelope, &hardware()).unwrap();
    assert_eq!(budget.compatibility, HardwareCompatibility::Runnable);
    assert_eq!(budget.rules_version, VISION_RESOURCE_RULES_VERSION);
    assert!(budget.blocking_reasons.is_empty());
    assert_eq!(budget.weight_bytes, MAIN_BYTES + PROJECTOR_BYTES);
    assert_eq!(
        budget.peak_incremental_disk_bytes,
        3 * (MAIN_BYTES + PROJECTOR_BYTES) + 16 * MIB
    );
    assert_eq!(budget.kv_cache_bytes, Some(58_720_256));
    assert_eq!(budget.recurrent_cache_bytes, Some(20_201_472));
    assert_eq!(budget.dynamic_cache_bytes, Some(0));
    assert_eq!(budget.compute_buffer_bytes, 256 * MIB);
    assert_eq!(budget.auxiliary_buffer_bytes, 512 * MIB);
    assert_eq!(
        budget.host_memory_required_bytes,
        Some(MAIN_BYTES + PROJECTOR_BYTES + 58_720_256 + 20_201_472 + 768 * MIB)
    );

    let mut text_envelope = envelope.clone();
    text_envelope.workload.vision = None;
    bind_components(&mut text_envelope, &components[..1]);
    let text = assess_resource_budget(&components[..1], &text_envelope, &hardware()).unwrap();
    assert_eq!(text.compatibility, HardwareCompatibility::Runnable);
    assert_eq!(text.kv_cache_bytes, budget.kv_cache_bytes);
    assert_eq!(text.recurrent_cache_bytes, budget.recurrent_cache_bytes);
    assert_eq!(
        budget.host_memory_required_bytes.unwrap() - text.host_memory_required_bytes.unwrap(),
        PROJECTOR_BYTES
    );
}

#[test]
fn measured_profile_requires_memory_and_staging_space() {
    let components = components();
    let envelope = baseline_vision_resource_envelope(&components).unwrap();
    let mut available = hardware();
    let baseline = assess_resource_budget(&components, &envelope, &available).unwrap();
    available.devices[0].free_bytes = baseline.host_memory_required_bytes.unwrap();
    available.disk_free_bytes = baseline.peak_incremental_disk_bytes;
    assert_eq!(
        assess_resource_budget(&components, &envelope, &available)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Runnable
    );
    available.devices[0].free_bytes -= 1;
    assert_eq!(
        assess_resource_budget(&components, &envelope, &available)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Incompatible
    );
    available.devices[0].free_bytes += 1;
    available.disk_free_bytes -= 1;
    assert_eq!(
        assess_resource_budget(&components, &envelope, &available)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Incompatible
    );
}

#[test]
fn unmeasured_runtime_workload_and_buffer_changes_are_unknown() {
    let components = components();
    let baseline = baseline_vision_resource_envelope(&components).unwrap();
    let mutations: &[fn(&mut AdapterResourceEnvelope)] = &[
        |e| e.runtime.binary_sha256 = Sha256Digest::of_bytes(b"different runtime binary"),
        |e| e.runtime.engine_revision = "unmeasured".into(),
        |e| e.runtime.binding_version = "unmeasured".into(),
        |e| e.evidence_id = "unmeasured".into(),
        |e| e.compute_buffer_bytes -= 1,
        |e| e.auxiliary_buffer_bytes -= 1,
        |e| e.workload.context_tokens *= 2,
        |e| e.workload.batch_tokens *= 2,
        |e| e.workload.parallel_sequences = 2,
        |e| e.workload.recurrent_rollback_slots = 1,
        |e| e.workload.kv_key_type = ResourceCacheType::F32,
        |e| e.workload.kv_value_type = ResourceCacheType::F32,
        |e| e.workload.prompt_cache_bytes = MIB,
        |e| e.workload.context_checkpoint_slots = 1,
        |e| e.workload.vision.as_mut().unwrap().max_images = 2,
        |e| e.workload.vision.as_mut().unwrap().max_input_width = 1025,
        |e| e.workload.vision.as_mut().unwrap().max_input_height = 1025,
        |e| e.workload.vision.as_mut().unwrap().max_input_bytes += 1,
        |e| e.workload.vision.as_mut().unwrap().native_min_image_tokens = 512,
        |e| e.workload.vision.as_mut().unwrap().native_max_image_tokens = 2048,
        |e| e.workload.vision.as_mut().unwrap().max_encoded_image_tokens = 2049,
        |e| e.workload.vision.as_mut().unwrap().microbatch_tokens = 1024,
        |e| e.workload.vision.as_mut().unwrap().image_batch_tokens = 2048,
        |e| e.workload.vision.as_mut().unwrap().cpu_threads = 8,
    ];
    for mutate in mutations {
        let mut envelope = baseline.clone();
        mutate(&mut envelope);
        assert_unknown(&components, &envelope);
    }
}

#[test]
fn missing_components_and_changed_content_cannot_use_measured_profile() {
    let baseline_components = components();
    let baseline = baseline_vision_resource_envelope(&baseline_components).unwrap();
    for missing in 0..2 {
        let mut selected = baseline_components.clone();
        selected.remove(missing);
        assert!(baseline_vision_resource_envelope(&selected).is_err());
        let mut envelope = baseline.clone();
        bind_components(&mut envelope, &selected);
        assert_unknown(&selected, &envelope);
    }
    let mutations: &[fn(&mut SourceGgufObservation)] = &[
        |file| file.artifact.source_sha256 = Some(Sha256Digest::of_bytes(b"different content")),
        |file| file.artifact.source_sha256 = None,
        |file| {
            file.header.file_bytes += 32;
            file.artifact.size_bytes = Some(file.header.file_bytes);
        },
        |file| {
            file.header.metadata.insert(
                "general.architecture".into(),
                GgufValue::Scalar {
                    type_id: 8,
                    value: GgufScalar::String("unmeasured".into()),
                },
            );
        },
    ];
    for index in 0..2 {
        for mutate in mutations {
            let mut selected = baseline_components.clone();
            mutate(&mut selected[index].files[0]);
            assert!(baseline_vision_resource_envelope(&selected).is_err());
            let mut envelope = baseline.clone();
            bind_components(&mut envelope, &selected);
            assert_unknown(&selected, &envelope);
        }
    }
}

#[test]
fn projector_without_vision_envelope_remains_unknown() {
    let components = components();
    let mut envelope = baseline_vision_resource_envelope(&components).unwrap();
    envelope.workload.vision = None;
    assert_unknown(&components, &envelope);
    let budget = assess_resource_budget(&components, &envelope, &hardware()).unwrap();
    assert_eq!(budget.rules_version, RESOURCE_RULES_VERSION);
    assert_eq!(budget.weight_bytes, MAIN_BYTES + PROJECTOR_BYTES);
}

#[test]
fn vision_limits_are_part_of_resource_budget_binding() {
    let components = components();
    let mut envelope = baseline_vision_resource_envelope(&components).unwrap();
    let initial = assess_resource_budget(&components, &envelope, &hardware()).unwrap();
    envelope.workload.vision.as_mut().unwrap().max_input_width = 512;
    let bounded = assess_resource_budget(&components, &envelope, &hardware()).unwrap();
    assert_eq!(bounded.compatibility, HardwareCompatibility::Runnable);
    assert_ne!(bounded.binding_digest, initial.binding_digest);
    assert_eq!(
        bounded.host_memory_required_bytes,
        initial.host_memory_required_bytes
    );
}

#[test]
fn recorded_v2_plan_preserves_its_original_digest_and_admission() {
    let fixture: Value = serde_json::from_str(FROZEN_TEXT_PLAN).unwrap();
    let plan: InstallPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    let admission = plan.admission.as_ref().unwrap();
    let accepted_at = fixture["accepted_at"].as_i64().unwrap();
    assert_eq!(admission.budget.rules_version, "local-resource-budget-v2");
    assert!(admission.envelope.workload.vision.is_none());
    let serialized = serde_json::to_value(&plan).unwrap();
    assert_eq!(serialized, fixture["plan"]);
    assert!(serialized["admission"]["envelope"]["workload"]
        .get("vision")
        .is_none());
    assert_eq!(
        plan.digest().unwrap().as_str(),
        fixture["expected_plan_digest"].as_str().unwrap()
    );
    admission.validate(&plan, accepted_at).unwrap();
    plan.validate_for_install(accepted_at, admission.hardware.disk_free_bytes)
        .unwrap();
    assert_eq!(
        assess_resource_budget(&components()[..1], &admission.envelope, &admission.hardware)
            .unwrap(),
        admission.budget
    );
}

#[test]
fn old_plan_cannot_gain_vision_without_rebinding_admission() {
    let fixture: Value = serde_json::from_str(FROZEN_TEXT_PLAN).unwrap();
    let mut plan: InstallPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    let original_digest = plan.digest().unwrap();
    plan.admission.as_mut().unwrap().envelope.workload.vision =
        Some(VisionResourceWorkload::measured_qwen35_cpu());
    assert_ne!(plan.digest().unwrap(), original_digest);
    let admission = plan.admission.as_ref().unwrap();
    assert!(admission
        .validate(&plan, fixture["accepted_at"].as_i64().unwrap())
        .is_err());
}
