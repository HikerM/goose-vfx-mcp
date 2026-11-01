use anyhow::Result;
use std::collections::BTreeSet;

use super::{ComponentPairingProfile, PairingConstraint, PairingSide, TokenizerIdentityPolicy};
use crate::lifecycle::{ComponentRole, RuntimeBuild, Sha256Digest};

pub fn baseline_pairing_runtime() -> Result<RuntimeBuild> {
    Ok(RuntimeBuild {
        adapter_id: "llama-helper".into(),
        engine_revision: "dde879bc4f954b809892fd0d660515f42536394a".into(),
        binding_version: "native-http-v1".into(),
        platform: "x86_64-pc-windows-msvc".into(),
        features: ["cpu", "mtmd", "embedding", "reranking", "mtp", "dflash"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        binary_sha256: Sha256Digest::parse(
            "5031f8cc21505b931581b96e37270e1cc8c6c378d38b3dfc9b1c254db8f9c58e",
        )?,
    })
}

fn text(side: PairingSide, key: &str, value: &str) -> PairingConstraint {
    PairingConstraint::Text {
        side,
        key: key.into(),
        value: value.into(),
    }
}

fn equal(left_key: &str, right_key: &str) -> PairingConstraint {
    PairingConstraint::UnsignedEqual {
        left_side: PairingSide::Main,
        left_key: left_key.into(),
        right_side: PairingSide::Dependency,
        right_key: right_key.into(),
    }
}

fn one_hash(value: &str) -> Result<BTreeSet<Sha256Digest>> {
    Ok(BTreeSet::from([Sha256Digest::parse(value)?]))
}

pub fn baseline_pairing_profiles() -> Result<Vec<ComponentPairingProfile>> {
    let runtime = baseline_pairing_runtime()?;
    let small = "c54f8b67069c70085b98440de696b44da8250250ac69a961b41133def876e262";
    let projector = ComponentPairingProfile {
        id: "qwen35-08b-vision-content-baseline".into(),
        version: "1".into(),
        runtime: runtime.clone(),
        dependency_role: ComponentRole::VisionProjector,
        main_content: one_hash(small)?,
        dependency_content: one_hash(
            "ea8519d0c6240e465a0265d6912f73d750a17ca7d42150281b778b8b59f05798",
        )?,
        constraints: vec![
            text(PairingSide::Main, "general.architecture", "qwen35"),
            text(PairingSide::Dependency, "general.architecture", "clip"),
            text(
                PairingSide::Dependency,
                "clip.projector_type",
                "qwen3vl_merger",
            ),
            PairingConstraint::Boolean {
                side: PairingSide::Dependency,
                key: "clip.has_vision_encoder".into(),
                value: true,
            },
            equal("qwen35.embedding_length", "clip.vision.projection_dim"),
            PairingConstraint::TensorAxisEqual {
                side: PairingSide::Dependency,
                tensor: "mm.2.weight".into(),
                axis: 1,
                field_side: PairingSide::Main,
                field_key: "qwen35.embedding_length".into(),
            },
        ],
        evidence_ids: BTreeSet::from(["m0:2026-11-01-native-helper-vision.json".into()]),
    };
    let mtp = ComponentPairingProfile {
        id: "qwen35-08b-embedded-mtp-content-baseline".into(),
        version: "1".into(),
        runtime: runtime.clone(),
        dependency_role: ComponentRole::Mtp,
        main_content: one_hash(small)?,
        dependency_content: one_hash(small)?,
        constraints: vec![
            text(PairingSide::Main, "general.architecture", "qwen35"),
            text(PairingSide::Dependency, "general.architecture", "qwen35"),
            PairingConstraint::UnsignedMinimum {
                side: PairingSide::Main,
                key: "qwen35.nextn_predict_layers".into(),
                minimum: 1,
            },
            PairingConstraint::UnsignedMinimum {
                side: PairingSide::Dependency,
                key: "qwen35.nextn_predict_layers".into(),
                minimum: 1,
            },
            equal("qwen35.embedding_length", "qwen35.embedding_length"),
            PairingConstraint::TokenizerIdentity {
                policy: TokenizerIdentityPolicy::Exact,
            },
        ],
        evidence_ids: BTreeSet::from(["m0:2026-11-01-native-mtp-feasibility.json".into()]),
    };
    let dflash = ComponentPairingProfile {
        id: "qwen35-4b-dflash-content-baseline".into(),
        version: "1".into(),
        runtime,
        dependency_role: ComponentRole::Dflash,
        main_content: one_hash("c1c33fb44b3cea1d296174d58dc9f1449a0150cbc353679b7ad86955c240d49a")?,
        dependency_content: one_hash(
            "f0689eadab0d46468bc2629bb2706f4aba7f794b63e1c9f6eb291269d572a52b",
        )?,
        constraints: vec![
            text(PairingSide::Main, "general.architecture", "qwen35"),
            text(PairingSide::Dependency, "general.architecture", "dflash"),
            equal("qwen35.embedding_length", "dflash.embedding_length"),
            PairingConstraint::UnsignedMinimum {
                side: PairingSide::Dependency,
                key: "dflash.block_size".into(),
                minimum: 1,
            },
            PairingConstraint::LayerIndices {
                side: PairingSide::Dependency,
                key: "dflash.target_layers".into(),
                target_side: PairingSide::Main,
                target_block_count_key: "qwen35.block_count".into(),
            },
            PairingConstraint::TokenizerIdentity {
                policy: TokenizerIdentityPolicy::DflashMask,
            },
        ],
        evidence_ids: BTreeSet::from(["m0:2026-11-01-native-dflash-feasibility.json".into()]),
    };
    Ok(vec![projector, mtp, dflash])
}
