use anyhow::{ensure, Context, Result};
use image::{imageops, DynamicImage, ImageFormat};
use sha2::{Digest, Sha256};
use std::io::Cursor;

use super::{MediaKind, MediaResultRef, MediaSpec, ProjectEditScope, MAX_MEDIA_BYTES};

pub(crate) fn image_edit_result_id(
    session_id: &str,
    job_id: &str,
    parent_result_id: &str,
    scope: &ProjectEditScope,
) -> String {
    let mut hash = Sha256::new();
    for value in [
        "lumina-local-image-edit-v1",
        session_id,
        job_id,
        parent_result_id,
        &serde_json::to_string(scope).expect("ProjectEditScope is serializable"),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Applies an image replacement to one bounded rectangle while retaining every
/// pixel outside that rectangle from the archived parent image.
pub(crate) fn apply_image_rect_overlay(
    parent: &[u8],
    replacement: &[u8],
    parent_result: &MediaResultRef,
    scope: &ProjectEditScope,
) -> Result<Vec<u8>> {
    let ProjectEditScope::ImageRect {
        x,
        y,
        width,
        height,
    } = scope
    else {
        anyhow::bail!("Image editor requires an image rectangle")
    };
    ensure!(
        parent_result.kind == MediaKind::Image,
        "Parent result is not an image"
    );
    let MediaSpec::Image {
        width: expected_width,
        height: expected_height,
    } = parent_result.spec
    else {
        anyhow::bail!("Parent result has no image dimensions")
    };
    ensure!(
        parent.len() as u64 <= MAX_MEDIA_BYTES,
        "Parent image exceeds the byte limit"
    );
    ensure!(
        replacement.len() as u64 <= MAX_MEDIA_BYTES,
        "Replacement image exceeds the byte limit"
    );
    ensure!(
        *width > 0 && *height > 0,
        "Image edit rectangle must be non-empty"
    );
    ensure!(
        x.checked_add(*width)
            .is_some_and(|end| end <= expected_width)
            && y.checked_add(*height)
                .is_some_and(|end| end <= expected_height),
        "Image edit rectangle exceeds the archived media bounds"
    );

    let mut base = image::load_from_memory(parent).context("Failed to decode parent image")?;
    ensure!(
        base.width() == expected_width && base.height() == expected_height,
        "Decoded parent dimensions differ from the archived result"
    );
    let replacement = image::load_from_memory(replacement)
        .context("Failed to decode replacement image")?
        .resize_exact(*width, *height, imageops::FilterType::Triangle);
    imageops::overlay(&mut base, &replacement, i64::from(*x), i64::from(*y));

    let mut output = Cursor::new(Vec::new());
    DynamicImage::write_to(&base, &mut output, ImageFormat::Png)
        .context("Failed to encode locally edited image")?;
    let bytes = output.into_inner();
    ensure!(
        bytes.len() as u64 <= MAX_MEDIA_BYTES,
        "Edited image exceeds the byte limit"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png(image: RgbaImage) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn result() -> MediaResultRef {
        MediaResultRef {
            schema_version: 1,
            result_id: "00000000-0000-0000-0000-000000000001".into(),
            session_id: "session".into(),
            association: super::super::MediaAssociation::Message {
                message_id: "message".into(),
            },
            job_id: "job".into(),
            kind: MediaKind::Image,
            mime: "image/png".into(),
            file_name: "parent.png".into(),
            bytes: 0,
            sha256: "0".repeat(64),
            created_at: 0,
            root_id: "root".into(),
            relative_path: "session/job/parent.png".into(),
            spec: MediaSpec::Image {
                width: 4,
                height: 4,
            },
            ownership: super::super::MediaOwnership::ApplicationGenerated,
            source: super::super::MediaSource::LocalModel {
                installation_id: "installation".into(),
                version_id: "version".into(),
                runtime: super::super::MediaRuntimeBuild {
                    adapter_id: "adapter".into(),
                    engine_revision: "revision".into(),
                    binary_sha256: "0".repeat(64),
                },
                generation_config_sha256: "0".repeat(64),
                input_refs: Vec::new(),
            },
        }
    }

    #[test]
    fn overlay_replaces_only_the_selected_rectangle() {
        let parent = png(RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])));
        let replacement = png(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 255, 255])));
        let output = apply_image_rect_overlay(
            &parent,
            &replacement,
            &result(),
            &ProjectEditScope::ImageRect {
                x: 1,
                y: 2,
                width: 2,
                height: 1,
            },
        )
        .unwrap();
        let decoded = image::load_from_memory(&output).unwrap().to_rgba8();
        for y in 0..4 {
            for x in 0..4 {
                let expected = if (1..3).contains(&x) && y == 2 {
                    Rgba([0, 0, 255, 255])
                } else {
                    Rgba([255, 0, 0, 255])
                };
                assert_eq!(*decoded.get_pixel(x, y), expected);
            }
        }
    }

    #[test]
    fn overlay_rejects_wrong_kind_and_out_of_bounds_scope() {
        let parent = png(RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])));
        let replacement = png(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 255, 255])));
        let error = apply_image_rect_overlay(
            &parent,
            &replacement,
            &result(),
            &ProjectEditScope::ImageRect {
                x: 4,
                y: 0,
                width: 1,
                height: 1,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        let error = apply_image_rect_overlay(
            &parent,
            &replacement,
            &result(),
            &ProjectEditScope::TimeRange {
                start_micros: 0,
                duration_micros: 1,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires an image rectangle"));
    }
}
