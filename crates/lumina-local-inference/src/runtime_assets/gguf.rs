use anyhow::{bail, ensure, Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub(super) fn native_split_count(path: &Path) -> Result<Option<u16>> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut reader = MetadataReader {
        reader: BufReader::new(file),
        length,
        position: 0,
    };
    ensure!(&reader.fixed::<4>()? == b"GGUF", "File is not GGUF");
    ensure!(
        matches!(reader.u32()?, 2 | 3),
        "Unsupported native GGUF version"
    );
    ensure!(
        reader.u64()? <= i64::MAX as u64,
        "Invalid GGUF tensor count"
    );
    let entries = reader.u64()?;
    ensure!(
        entries <= reader.remaining() / 13,
        "Truncated GGUF metadata entries"
    );
    let mut count = None;
    let mut index = None;
    for _ in 0..entries {
        let key = reader.split_key()?;
        let kind = reader.u32()?;
        if let Some(key) = key {
            let destination = match key {
                SplitKey::Count => &mut count,
                SplitKey::Index => &mut index,
            };
            ensure!(destination.is_none(), "Duplicate GGUF split metadata key");
            *destination = Some(if kind == 2 {
                SplitValue::U16(u16::from_le_bytes(reader.fixed()?))
            } else {
                reader.skip_value(kind)?;
                SplitValue::Other
            });
        } else {
            reader.skip_value(kind)?;
        }
    }
    let count = match count {
        None => return Ok(None),
        Some(SplitValue::U16(count)) if count <= 1 => return Ok(None),
        Some(SplitValue::U16(count)) => count,
        Some(SplitValue::Other) => bail!("GGUF split.count must have uint16 type"),
    };
    match index {
        Some(SplitValue::U16(0)) => Ok(Some(count)),
        Some(SplitValue::U16(_)) => bail!("Native loading requires the first GGUF shard"),
        Some(SplitValue::Other) => bail!("GGUF split.no must have uint16 type"),
        None => bail!("Missing GGUF split.no"),
    }
}

enum SplitKey {
    Count,
    Index,
}

enum SplitValue {
    U16(u16),
    Other,
}

struct MetadataReader {
    reader: BufReader<File>,
    length: u64,
    position: u64,
}

impl MetadataReader {
    fn remaining(&self) -> u64 {
        self.length - self.position
    }

    fn end_position(&self, bytes: u64) -> Result<u64> {
        let end = self
            .position
            .checked_add(bytes)
            .context("GGUF metadata length overflow")?;
        ensure!(end <= self.length, "Truncated GGUF metadata value");
        Ok(end)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.end_position(N as u64)?;
        let mut bytes = [0; N];
        self.reader.read_exact(&mut bytes)?;
        self.position = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    fn skip(&mut self, bytes: u64) -> Result<()> {
        let end = self.end_position(bytes)?;
        self.reader.seek(SeekFrom::Start(end))?;
        self.position = end;
        Ok(())
    }

    fn skip_string(&mut self) -> Result<()> {
        let bytes = self.u64()?;
        self.skip(bytes)
    }

    fn split_key(&mut self) -> Result<Option<SplitKey>> {
        let length = self.u64()?;
        let prefix_length = length.min(12) as usize;
        let mut prefix = [0; 12];
        for byte in prefix.iter_mut().take(prefix_length) {
            *byte = self.fixed::<1>()?[0];
        }
        self.skip(length - prefix_length as u64)?;
        // gguf_find_key compares C strings, including keys containing embedded NULs.
        let key = prefix[..prefix_length]
            .split(|byte| *byte == 0)
            .next()
            .unwrap();
        match key {
            b"split.count" => Ok(Some(SplitKey::Count)),
            b"split.no" => Ok(Some(SplitKey::Index)),
            _ => Ok(None),
        }
    }

    fn skip_value(&mut self, kind: u32) -> Result<()> {
        if kind == 8 {
            return self.skip_string();
        }
        if kind != 9 {
            return self.skip(scalar_width(kind)?);
        }
        let element = self.u32()?;
        let count = self.u64()?;
        if element == 8 {
            ensure!(count <= self.remaining() / 8, "Truncated GGUF string array");
            for _ in 0..count {
                self.skip_string()?;
            }
            Ok(())
        } else {
            let bytes = count
                .checked_mul(scalar_width(element)?)
                .context("GGUF array byte count overflow")?;
            self.skip(bytes)
        }
    }
}

fn scalar_width(kind: u32) -> Result<u64> {
    match kind {
        0 | 1 | 7 => Ok(1),
        2 | 3 => Ok(2),
        4..=6 => Ok(4),
        10..=12 => Ok(8),
        _ => bail!("Unsupported or nested GGUF metadata type {kind}"),
    }
}
