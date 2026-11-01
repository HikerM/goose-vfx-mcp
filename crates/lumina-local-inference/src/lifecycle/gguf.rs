use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read};
use std::path::Path;

use super::Sha256Digest;

#[derive(Debug, Clone, Copy)]
pub struct GgufReadLimits {
    pub max_header_bytes: u64,
    pub max_metadata_entries: u64,
    pub max_tensors: u64,
    pub max_array_elements: u64,
    pub max_string_bytes: u64,
    pub retained_array_elements: u64,
    pub max_tensor_name_bytes: u64,
    pub max_tensor_dimensions: u32,
}

impl Default for GgufReadLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 64 * 1024 * 1024,
            max_metadata_entries: 100_000,
            max_tensors: 200_000,
            max_array_elements: 64 * 1024 * 1024,
            max_string_bytes: 8 * 1024 * 1024,
            retained_array_elements: 256,
            max_tensor_name_bytes: 1024,
            max_tensor_dimensions: 8,
        }
    }
}

impl GgufReadLimits {
    pub fn for_llama_cpp() -> Self {
        Self {
            max_tensor_name_bytes: 63,
            max_tensor_dimensions: 4,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GgufByteOrder {
    LittleEndian,
    BigEndian,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum GgufScalar {
    Unsigned(u64),
    Signed(i64),
    Float32Bits(u32),
    Float64Bits(u64),
    Bool(bool),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GgufValue {
    Scalar {
        type_id: u32,
        value: GgufScalar,
    },
    Array {
        element_type: u32,
        count: u64,
        retained_values: Vec<GgufScalar>,
        encoded_digest: Sha256Digest,
    },
}

impl GgufValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Scalar {
                value: GgufScalar::String(value),
                ..
            } => Some(value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Scalar {
                value: GgufScalar::Unsigned(value),
                ..
            } => Some(*value),
            Self::Scalar {
                value: GgufScalar::Signed(value),
                ..
            } => u64::try_from(*value).ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufTensor {
    pub name: String,
    pub dimensions: Vec<u64>,
    pub type_id: u32,
    pub quantization: String,
    pub relative_offset: u64,
    pub encoded_bytes: u64,
    pub elements: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufShard {
    pub index: u64,
    pub count: u64,
    pub total_tensors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufInspection {
    pub version: u32,
    pub byte_order: GgufByteOrder,
    pub file_bytes: u64,
    pub header_bytes: u64,
    pub header_digest: Sha256Digest,
    pub data_offset: u64,
    pub alignment: u64,
    pub metadata: BTreeMap<String, GgufValue>,
    pub tensors: Vec<GgufTensor>,
    pub stored_tensor_elements: u64,
    pub encoded_tensor_bytes: u64,
    pub shard: Option<GgufShard>,
}

impl GgufInspection {
    pub fn architecture(&self) -> Option<&str> {
        self.metadata
            .get("general.architecture")
            .and_then(GgufValue::as_str)
    }

    pub fn inspect_file(path: &Path, limits: GgufReadLimits) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "GGUF input is not a regular file");
        inspect_gguf(BufReader::new(file), metadata.len(), limits)
    }
}

pub fn validate_gguf_shard_set(shards: &[GgufInspection]) -> Result<()> {
    let first = shards.first().context("Missing GGUF weights")?;
    let Some(expected) = &first.shard else {
        ensure!(
            shards.len() == 1,
            "Multiple GGUF weights lack shard metadata"
        );
        return Ok(());
    };
    let first = shards
        .iter()
        .find(|inspection| {
            inspection
                .shard
                .as_ref()
                .is_some_and(|shard| shard.index == 0)
        })
        .context("Missing primary GGUF shard")?;
    ensure!(
        expected.count == shards.len() as u64,
        "Incomplete GGUF shard set"
    );
    let mut indices = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut count = 0u64;
    let mut common_metadata = BTreeMap::new();
    for inspection in shards {
        let shard = inspection
            .shard
            .as_ref()
            .context("Mixed split and unsplit GGUF weights")?;
        ensure!(
            inspection.version == first.version && inspection.byte_order == first.byte_order,
            "GGUF shard encoding mismatch"
        );
        ensure!(
            shard.count == expected.count && shard.total_tensors == expected.total_tensors,
            "Inconsistent GGUF shard totals"
        );
        ensure!(indices.insert(shard.index), "Duplicate GGUF shard index");
        if let Some(architecture) = inspection.architecture() {
            ensure!(
                Some(architecture) == first.architecture(),
                "GGUF shard architecture mismatch"
            );
        }
        for (key, value) in &inspection.metadata {
            if key == "split.no" {
                continue;
            }
            if let Some(previous) = common_metadata.insert(key, value) {
                ensure!(previous == value, "GGUF shard metadata mismatch: {key}");
            }
        }
        for tensor in &inspection.tensors {
            ensure!(
                names.insert(&tensor.name),
                "Repeated tensor across GGUF shards: {}",
                tensor.name
            );
        }
        count = count
            .checked_add(inspection.tensors.len() as u64)
            .context("GGUF shard tensor count overflow")?;
    }
    ensure!(
        indices.iter().copied().eq(0..expected.count),
        "Missing GGUF shard index"
    );
    ensure!(
        count == expected.total_tensors,
        "GGUF shard tensor total mismatch"
    );
    Ok(())
}

struct HeaderReader<R> {
    reader: R,
    order: GgufByteOrder,
    position: u64,
    file_bytes: u64,
    limits: GgufReadLimits,
    digest: Sha256,
    array_digest: Option<Sha256>,
}

impl<R: Read> HeaderReader<R> {
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        let end = self
            .position
            .checked_add(bytes.len() as u64)
            .context("GGUF header size overflow")?;
        ensure!(
            end <= self.limits.max_header_bytes,
            "GGUF header exceeds read budget"
        );
        ensure!(end <= self.file_bytes, "GGUF header exceeds file length");
        self.reader
            .read_exact(bytes)
            .context("Truncated GGUF header")?;
        self.digest.update(&*bytes);
        if let Some(digest) = &mut self.array_digest {
            digest.update(&*bytes);
        }
        self.position = end;
        Ok(())
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.fixed()?;
        Ok(match self.order {
            GgufByteOrder::LittleEndian => u32::from_le_bytes(bytes),
            GgufByteOrder::BigEndian => u32::from_be_bytes(bytes),
        })
    }

    fn u64(&mut self) -> Result<u64> {
        let bytes = self.fixed()?;
        Ok(match self.order {
            GgufByteOrder::LittleEndian => u64::from_le_bytes(bytes),
            GgufByteOrder::BigEndian => u64::from_be_bytes(bytes),
        })
    }

    fn string(&mut self, max_bytes: u64) -> Result<String> {
        let len = self.u64()?;
        ensure!(
            len <= max_bytes.min(self.limits.max_string_bytes),
            "GGUF string exceeds limit"
        );
        ensure!(
            len <= self.limits.max_header_bytes.saturating_sub(self.position),
            "GGUF string exceeds header budget"
        );
        ensure!(
            len <= self.file_bytes.saturating_sub(self.position),
            "Truncated GGUF string"
        );
        let mut bytes = vec![0; usize::try_from(len)?];
        self.read_exact(&mut bytes)?;
        String::from_utf8(bytes).context("GGUF string is not UTF-8")
    }

    fn scalar(&mut self, type_id: u32) -> Result<GgufScalar> {
        Ok(match type_id {
            0 => GgufScalar::Unsigned(self.fixed::<1>()?[0] as u64),
            1 => GgufScalar::Signed(self.fixed::<1>()?[0] as i8 as i64),
            2 | 3 => {
                let bytes = self.fixed()?;
                let value = match self.order {
                    GgufByteOrder::LittleEndian => u16::from_le_bytes(bytes),
                    GgufByteOrder::BigEndian => u16::from_be_bytes(bytes),
                };
                if type_id == 2 {
                    GgufScalar::Unsigned(value as u64)
                } else {
                    GgufScalar::Signed(value as i16 as i64)
                }
            }
            4 => GgufScalar::Unsigned(self.u32()? as u64),
            5 => GgufScalar::Signed(self.u32()? as i32 as i64),
            6 => GgufScalar::Float32Bits(self.u32()?),
            7 => {
                let byte = self.fixed::<1>()?[0];
                ensure!(byte <= 1, "Invalid GGUF boolean");
                GgufScalar::Bool(byte == 1)
            }
            8 => GgufScalar::String(self.string(self.limits.max_string_bytes)?),
            10 => GgufScalar::Unsigned(self.u64()?),
            11 => GgufScalar::Signed(self.u64()? as i64),
            12 => GgufScalar::Float64Bits(self.u64()?),
            _ => bail!("Unsupported GGUF scalar type {type_id}"),
        })
    }

    fn value(&mut self, type_id: u32) -> Result<GgufValue> {
        if type_id != 9 {
            return Ok(GgufValue::Scalar {
                type_id,
                value: self.scalar(type_id)?,
            });
        }
        let element_type = self.u32()?;
        ensure!(
            matches!(element_type, 0..=8 | 10..=12),
            "Invalid or nested GGUF array type"
        );
        let count = self.u64()?;
        ensure!(
            count <= self.limits.max_array_elements,
            "GGUF array exceeds element limit"
        );
        self.array_digest = Some(Sha256::new());
        let mut retained_values = Vec::new();
        let retained_count = count.min(self.limits.retained_array_elements);
        for _ in 0..retained_count {
            let value = self.scalar(element_type)?;
            retained_values.push(value);
        }
        let remaining = count - retained_count;
        if let Some(width) = scalar_width(element_type) {
            let mut bytes = remaining
                .checked_mul(width)
                .context("GGUF array byte count overflow")?;
            ensure!(
                bytes <= self.limits.max_header_bytes.saturating_sub(self.position),
                "GGUF array exceeds header budget"
            );
            let mut buffer = [0; 8192];
            while bytes > 0 {
                let size = bytes.min(buffer.len() as u64) as usize;
                self.read_exact(&mut buffer[..size])?;
                if element_type == 7 {
                    ensure!(
                        buffer[..size].iter().all(|byte| *byte <= 1),
                        "Invalid GGUF boolean array"
                    );
                }
                bytes -= size as u64;
            }
        } else {
            ensure!(
                remaining <= self.limits.max_header_bytes.saturating_sub(self.position) / 8,
                "GGUF string array exceeds header budget"
            );
            for _ in 0..remaining {
                self.scalar(element_type)?;
            }
        }
        let encoded_digest =
            finish_digest(self.array_digest.take().expect("array digest initialized"))?;
        Ok(GgufValue::Array {
            element_type,
            count,
            retained_values,
            encoded_digest,
        })
    }
}

fn scalar_width(type_id: u32) -> Option<u64> {
    match type_id {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

pub fn inspect_gguf(
    reader: impl Read,
    file_bytes: u64,
    limits: GgufReadLimits,
) -> Result<GgufInspection> {
    let mut reader = HeaderReader {
        reader,
        order: GgufByteOrder::LittleEndian,
        position: 0,
        file_bytes,
        limits,
        digest: Sha256::new(),
        array_digest: None,
    };
    ensure!(&reader.fixed::<4>()? == b"GGUF", "File is not GGUF");
    let version_bytes = reader.fixed::<4>()?;
    let version = match u32::from_le_bytes(version_bytes) {
        version @ (2 | 3) => version,
        _ if u32::from_be_bytes(version_bytes) == 3 => {
            reader.order = GgufByteOrder::BigEndian;
            3
        }
        _ => bail!("Unsupported GGUF version"),
    };
    let tensor_count = reader.u64()?;
    let kv_count = reader.u64()?;
    ensure!(
        tensor_count <= limits.max_tensors,
        "GGUF tensor count exceeds limit"
    );
    ensure!(
        kv_count <= limits.max_metadata_entries,
        "GGUF metadata count exceeds limit"
    );
    let mut metadata = BTreeMap::new();
    for _ in 0..kv_count {
        let key = reader.string(65_535)?;
        ensure!(
            !key.is_empty() && key.is_ascii() && !key.bytes().any(|b| b.is_ascii_control()),
            "Invalid GGUF metadata key"
        );
        ensure!(
            !metadata.contains_key(&key),
            "Duplicate GGUF metadata key {key}"
        );
        let type_id = reader.u32()?;
        metadata.insert(key, reader.value(type_id)?);
    }
    let alignment = match metadata.get("general.alignment") {
        Some(GgufValue::Scalar {
            type_id: 4,
            value: GgufScalar::Unsigned(value),
        }) => *value,
        Some(_) => bail!("Invalid GGUF alignment type"),
        None => 32,
    };
    ensure!(
        alignment.is_power_of_two() && alignment <= 1024 * 1024,
        "Invalid GGUF alignment"
    );
    let mut tensors = Vec::new();
    let mut names = BTreeSet::new();
    let mut stored_tensor_elements = 0u64;
    let mut encoded_tensor_bytes = 0u64;
    for _ in 0..tensor_count {
        let name = reader.string(limits.max_tensor_name_bytes)?;
        ensure!(
            !name.is_empty() && !name.contains('\0'),
            "Invalid GGUF tensor name"
        );
        ensure!(names.insert(name.clone()), "Duplicate GGUF tensor {name}");
        let dimensions_count = reader.u32()?;
        ensure!(
            dimensions_count <= limits.max_tensor_dimensions,
            "GGUF tensor rank exceeds the selected reader profile"
        );
        let mut dimensions = Vec::new();
        let mut elements = 1u64;
        for _ in 0..dimensions_count {
            let dimension = reader.u64()?;
            ensure!(
                dimension > 0 && dimension <= i64::MAX as u64,
                "Invalid GGUF tensor dimension"
            );
            elements = elements
                .checked_mul(dimension)
                .context("GGUF tensor element count overflow")?;
            ensure!(
                elements <= i64::MAX as u64,
                "GGUF tensor exceeds engine element limit"
            );
            dimensions.push(dimension);
        }
        let type_id = reader.u32()?;
        let (quantization, block_elements, block_bytes) =
            tensor_encoding(type_id).context("Unsupported or removed GGUF tensor type")?;
        ensure!(
            dimensions.first().copied().unwrap_or(1) % block_elements == 0,
            "GGUF tensor row is not divisible by quantization block"
        );
        let encoded_bytes = (elements / block_elements)
            .checked_mul(block_bytes)
            .context("GGUF tensor byte count overflow")?;
        let relative_offset = reader.u64()?;
        ensure!(
            relative_offset % alignment == 0,
            "Unaligned GGUF tensor offset"
        );
        stored_tensor_elements = stored_tensor_elements
            .checked_add(elements)
            .context("GGUF total element count overflow")?;
        encoded_tensor_bytes = encoded_tensor_bytes
            .checked_add(encoded_bytes)
            .context("GGUF total tensor bytes overflow")?;
        tensors.push(GgufTensor {
            name,
            dimensions,
            type_id,
            quantization: quantization.to_string(),
            relative_offset,
            encoded_bytes,
            elements,
        });
    }
    let header_bytes = reader.position;
    let data_offset = header_bytes
        .checked_add(alignment - 1)
        .context("GGUF data offset overflow")?
        / alignment
        * alignment;
    if tensor_count > 0 {
        ensure!(
            data_offset <= file_bytes,
            "GGUF data starts beyond file length"
        );
    }
    let mut ranges = Vec::new();
    for tensor in &tensors {
        let start = data_offset
            .checked_add(tensor.relative_offset)
            .context("GGUF tensor offset overflow")?;
        let end = start
            .checked_add(tensor.encoded_bytes)
            .context("GGUF tensor end overflow")?;
        ensure!(
            end <= file_bytes,
            "GGUF tensor {} exceeds file length",
            tensor.name
        );
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    ensure!(
        ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "Overlapping GGUF tensors"
    );
    let shard = parse_shard(&metadata, tensor_count)?;
    Ok(GgufInspection {
        version,
        byte_order: reader.order,
        file_bytes,
        header_bytes,
        header_digest: finish_digest(reader.digest)?,
        data_offset,
        alignment,
        metadata,
        tensors,
        stored_tensor_elements,
        encoded_tensor_bytes,
        shard,
    })
}

fn parse_shard(
    metadata: &BTreeMap<String, GgufValue>,
    tensor_count: u64,
) -> Result<Option<GgufShard>> {
    let keys = ["split.no", "split.count", "split.tensors.count"];
    if keys.iter().all(|key| !metadata.contains_key(*key)) {
        return Ok(None);
    }
    let values = keys.map(|key| {
        metadata
            .get(key)
            .and_then(GgufValue::as_u64)
            .with_context(|| format!("Missing or invalid GGUF {key}"))
    });
    let [index, count, total_tensors] = values;
    let (index, count, total_tensors) = (index?, count?, total_tensors?);
    ensure!(
        count > 0 && index < count && total_tensors >= tensor_count,
        "Invalid GGUF shard metadata"
    );
    Ok(Some(GgufShard {
        index,
        count,
        total_tensors,
    }))
}

fn finish_digest(digest: Sha256) -> Result<Sha256Digest> {
    use std::fmt::Write;
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(&mut hex, "{byte:02x}")?;
    }
    Sha256Digest::parse(hex)
}

// Wire IDs and block layouts are tied to the locked ggml runtime, not model filenames.
fn tensor_encoding(type_id: u32) -> Option<(&'static str, u64, u64)> {
    Some(match type_id {
        0 => ("f32", 1, 4),
        1 => ("f16", 1, 2),
        2 => ("q4_0", 32, 18),
        3 => ("q4_1", 32, 20),
        6 => ("q5_0", 32, 22),
        7 => ("q5_1", 32, 24),
        8 => ("q8_0", 32, 34),
        9 => ("q8_1", 32, 36),
        10 => ("q2_k", 256, 84),
        11 => ("q3_k", 256, 110),
        12 => ("q4_k", 256, 144),
        13 => ("q5_k", 256, 176),
        14 => ("q6_k", 256, 210),
        15 => ("q8_k", 256, 292),
        16 => ("iq2_xxs", 256, 66),
        17 => ("iq2_xs", 256, 74),
        18 => ("iq3_xxs", 256, 98),
        19 => ("iq1_s", 256, 50),
        20 => ("iq4_nl", 32, 18),
        21 => ("iq3_s", 256, 110),
        22 => ("iq2_s", 256, 82),
        23 => ("iq4_xs", 256, 136),
        24 => ("i8", 1, 1),
        25 => ("i16", 1, 2),
        26 => ("i32", 1, 4),
        27 => ("i64", 1, 8),
        28 => ("f64", 1, 8),
        29 => ("iq1_m", 256, 56),
        30 => ("bf16", 1, 2),
        34 => ("tq1_0", 256, 54),
        35 => ("tq2_0", 256, 66),
        39 => ("mxfp4", 32, 17),
        40 => ("nvfp4", 64, 36),
        41 => ("q1_0", 128, 18),
        _ => return None,
    })
}
