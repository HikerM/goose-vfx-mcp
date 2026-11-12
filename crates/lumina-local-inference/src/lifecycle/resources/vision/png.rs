use anyhow::{bail, ensure, Context, Result};
use flate2::{Decompress, FlushDecompress, Status};

pub(super) fn validate_pixels(bytes: &[u8], width: u32, height: u32, color: u8) -> Result<()> {
    let compressed = collect_image_data(bytes, color)?;
    let channels = if color == 2 { 3 } else { 4 };
    let stride = width as usize * channels + 1;
    let expected = stride * height as usize;
    // stb permits excess decoded PNG bytes and grows its buffer past the IHDR size.
    // An exact bounded expansion here prevents that allocation before native decoding.
    let mut pixels = vec![0; expected + 1];
    let mut decoder = Decompress::new(true);
    let status = decoder
        .decompress(&compressed, &mut pixels, FlushDecompress::Finish)
        .context("Invalid PNG image data")?;
    ensure!(
        status == Status::StreamEnd
            && decoder.total_out() == expected as u64
            && decoder.total_in() == compressed.len() as u64,
        "PNG expansion differs from the bounded declared pixel size"
    );
    ensure!(
        pixels[..expected]
            .chunks_exact(stride)
            .all(|row| row[0] <= 4),
        "Invalid PNG row filter"
    );
    Ok(())
}

fn collect_image_data(bytes: &[u8], color: u8) -> Result<Vec<u8>> {
    let mut offset = 8usize;
    let mut compressed = Vec::new();
    let mut saw_data = false;
    let mut data_ended = false;
    let mut saw_palette = false;
    let mut saw_transparency = false;
    while offset < bytes.len() {
        ensure!(bytes.len() - offset >= 12, "Truncated PNG chunk");
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(length)
            .and_then(|n| n.checked_add(12))
            .context("PNG chunk size overflow")?;
        ensure!(end <= bytes.len(), "Truncated PNG chunk data");
        let kind = &bytes[offset + 4..offset + 8];
        ensure!(
            kind.iter().all(u8::is_ascii_alphabetic),
            "Invalid PNG chunk name"
        );
        let crc = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
        ensure!(
            crc32fast::hash(&bytes[offset + 4..end - 4]) == crc,
            "PNG chunk checksum mismatch"
        );
        if saw_data && kind != b"IDAT" {
            data_ended = true;
        }
        match kind {
            b"IHDR" => ensure!(offset == 8 && length == 13, "Duplicate PNG image header"),
            b"IDAT" => {
                ensure!(!data_ended, "PNG image data chunks are not consecutive");
                saw_data = true;
                compressed.extend_from_slice(&bytes[offset + 8..end - 4]);
            }
            b"IEND" => {
                ensure!(
                    length == 0 && saw_data && end == bytes.len(),
                    "Invalid PNG end chunk"
                );
                return Ok(compressed);
            }
            b"PLTE" => {
                ensure!(
                    !saw_palette
                        && !saw_data
                        && length > 0
                        && length <= 768
                        && length.is_multiple_of(3),
                    "Invalid PNG palette"
                );
                saw_palette = true;
            }
            b"tRNS" => {
                ensure!(
                    !saw_transparency && !saw_data && color == 2 && length == 6,
                    "Invalid PNG transparency"
                );
                saw_transparency = true;
            }
            _ => ensure!(kind[0] & 0x20 != 0, "Unsupported critical PNG chunk"),
        }
        offset = end;
    }
    bail!("PNG end chunk is missing")
}
