use sha2::{Digest, Sha256};

use super::error::{V2Error, V2Result};

pub(crate) const DIGEST_LEN: usize = 32;

pub(crate) fn checked_u32_len(len: usize) -> V2Result<u32> {
    u32::try_from(len).map_err(|_| V2Error::NumericOverflow)
}

pub(crate) fn checked_usize_to_u64(value: usize) -> V2Result<u64> {
    u64::try_from(value).map_err(|_| V2Error::NumericOverflow)
}

pub(crate) fn sha256_domain(label: &str, parts: &[&[u8]]) -> V2Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(checked_u32_len(label.len())?.to_be_bytes());
    hasher.update(label.as_bytes());
    for part in parts {
        hasher.update(checked_u32_len(part.len())?.to_be_bytes());
        hasher.update(part);
    }
    Ok(hasher.finalize().into())
}

pub(crate) fn write_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> V2Result<()> {
    out.extend_from_slice(&checked_u32_len(bytes.len())?.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

pub(crate) fn decode_fixed_32(
    bytes: &[u8],
    context: &'static str,
    field: &'static str,
) -> V2Result<[u8; 32]> {
    if bytes.len() != DIGEST_LEN {
        return Err(V2Error::InvalidCanonicalLength {
            context,
            expected: DIGEST_LEN,
            actual: bytes.len(),
        });
    }
    let mut out = [0u8; DIGEST_LEN];
    out.copy_from_slice(bytes);
    if field.is_empty() {
        return Ok(out);
    }
    Ok(out)
}

pub(crate) fn decode_u8_field(
    bytes: &[u8],
    context: &'static str,
    field: &'static str,
) -> V2Result<u8> {
    if bytes.len() != 1 {
        return Err(V2Error::InvalidCanonicalLength {
            context,
            expected: 1,
            actual: bytes.len(),
        });
    }
    if field.is_empty() {
        return Ok(bytes[0]);
    }
    Ok(bytes[0])
}

pub(crate) fn decode_u64_field(
    bytes: &[u8],
    context: &'static str,
    field: &'static str,
) -> V2Result<u64> {
    if bytes.len() != 8 {
        return Err(V2Error::InvalidCanonicalLength {
            context,
            expected: 8,
            actual: bytes.len(),
        });
    }
    if field.is_empty() {
        return Ok(u64::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| V2Error::InvalidCanonicalField { context, field })?,
        ));
    }
    Ok(u64::from_be_bytes(bytes.try_into().map_err(|_| {
        V2Error::InvalidCanonicalField { context, field }
    })?))
}

pub(crate) fn decode_optional_fixed_32(
    bytes: &[u8],
    context: &'static str,
    field: &'static str,
) -> V2Result<Option<[u8; 32]>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(decode_fixed_32(bytes, context, field)?))
}

pub(crate) fn decode_optional_u64_field(
    bytes: &[u8],
    context: &'static str,
    field: &'static str,
) -> V2Result<Option<u64>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(decode_u64_field(bytes, context, field)?))
}

pub(crate) struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn field_bytes(&mut self, tag: u8, bytes: &[u8]) -> V2Result<()> {
        self.bytes.push(tag);
        self.bytes
            .extend_from_slice(&checked_u32_len(bytes.len())?.to_be_bytes());
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn field_u8(&mut self, tag: u8, value: u8) -> V2Result<()> {
        self.field_bytes(tag, &[value])
    }

    pub(crate) fn field_u64(&mut self, tag: u8, value: u64) -> V2Result<()> {
        self.field_bytes(tag, &value.to_be_bytes())
    }

    pub(crate) fn field_optional_u64(&mut self, tag: u8, value: Option<u64>) -> V2Result<()> {
        match value {
            Some(value) => self.field_u64(tag, value),
            None => self.field_bytes(tag, &[]),
        }
    }

    pub(crate) fn field_optional_bytes(&mut self, tag: u8, value: Option<&[u8]>) -> V2Result<()> {
        match value {
            Some(value) => self.field_bytes(tag, value),
            None => self.field_bytes(tag, &[]),
        }
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct CanonicalReader<'a> {
    context: &'static str,
    bytes: &'a [u8],
    offset: usize,
}

pub(crate) struct CanonicalField<'a> {
    pub(crate) value: &'a [u8],
    pub(crate) value_offset: usize,
    pub(crate) value_len: usize,
}

impl<'a> CanonicalReader<'a> {
    pub(crate) fn new(context: &'static str, bytes: &'a [u8]) -> Self {
        Self {
            context,
            bytes,
            offset: 0,
        }
    }

    pub(crate) fn field(&mut self, expected_tag: u8, field: &'static str) -> V2Result<&'a [u8]> {
        let field_value = self.field_with_range(expected_tag, field)?;
        Ok(field_value.value)
    }

    pub(crate) fn field_with_range(
        &mut self,
        expected_tag: u8,
        field: &'static str,
    ) -> V2Result<CanonicalField<'a>> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if remaining < 5 {
            return Err(V2Error::InvalidCanonicalField {
                context: self.context,
                field,
            });
        }
        let tag = self.bytes[self.offset];
        if tag != expected_tag {
            return Err(V2Error::InvalidCanonicalField {
                context: self.context,
                field,
            });
        }
        self.offset += 1;
        let len_bytes = &self.bytes[self.offset..self.offset + 4];
        let len = u32::from_be_bytes(len_bytes.try_into().map_err(|_| {
            V2Error::InvalidCanonicalField {
                context: self.context,
                field,
            }
        })?);
        self.offset += 4;
        let field_len = usize::try_from(len).map_err(|_| V2Error::NumericOverflow)?;
        let end = self
            .offset
            .checked_add(field_len)
            .ok_or(V2Error::NumericOverflow)?;
        if end > self.bytes.len() {
            return Err(V2Error::InvalidCanonicalField {
                context: self.context,
                field,
            });
        }
        let value_offset = self.offset;
        let value = &self.bytes[value_offset..end];
        self.offset = end;
        Ok(CanonicalField {
            value,
            value_offset,
            value_len: field_len,
        })
    }

    pub(crate) fn finish(self) -> V2Result<()> {
        if self.offset == self.bytes.len() {
            return Ok(());
        }
        Err(V2Error::InvalidCanonicalField {
            context: self.context,
            field: "trailing_bytes",
        })
    }
}
