use sha2::{Digest, Sha256};

pub type DigestHex = String;

const FRAME_DOMAIN: &[u8] = b"goose-verified-sources";
const FRAME_VERSION: &[u8] = b"v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest32([u8; 32]);

impl Digest32 {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex_encode(&self.0)
    }

    pub fn from_lower_hex(value: &str) -> Option<Self> {
        if value.len() != 64 || !value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        if value
            .as_bytes()
            .iter()
            .any(|byte| byte.is_ascii_uppercase())
        {
            return None;
        }
        let mut out = [0u8; 32];
        let bytes = value.as_bytes();
        let mut index = 0usize;
        while index < out.len() {
            let high = decode_hex_nibble(bytes[index * 2])?;
            let low = decode_hex_nibble(bytes[index * 2 + 1])?;
            out[index] = (high << 4) | low;
            index += 1;
        }
        Some(Self(out))
    }
}

impl std::fmt::Display for Digest32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

pub fn frame(label: &str, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        FRAME_DOMAIN.len() + FRAME_VERSION.len() + label.len() + bytes.len() + 11,
    );
    out.extend_from_slice(FRAME_DOMAIN);
    out.push(0);
    out.extend_from_slice(FRAME_VERSION);
    out.push(0);
    out.extend_from_slice(label.as_bytes());
    out.push(0);
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

pub(crate) fn digest_frame(label: &str, bytes: &[u8]) -> Digest32 {
    let mut hasher = Sha256::new();
    hasher.update(frame(label, bytes));
    Digest32::new(hasher.finalize().into())
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    out
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::frame;

    #[test]
    fn frame_layout_is_exact() {
        let expected = [
            b"".as_slice(),
            b"goose-verified-sources",
            &[0],
            b"v2",
            &[0],
            b"document",
            &[0],
            &[0, 0, 0, 0, 0, 0, 0, 3],
            b"abc",
        ]
        .concat();
        assert_eq!(frame("document", b"abc"), expected);
    }
}
