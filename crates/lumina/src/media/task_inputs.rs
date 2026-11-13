use anyhow::{ensure, Result};
use lumina_providers::local_inference::lifecycle::{
    NativeTaskInput, NativeTaskInputReference, NativeTaskPlan,
};

use super::{MediaKind, MediaResultRef, MediaResultStore, MediaSpec};

impl MediaResultStore {
    /// The service has already authenticated `session_id` on the active connection.
    /// Each returned input owns the store's verified content lease through execution.
    pub async fn open_native_task_inputs(
        &self,
        session_id: &str,
        plan: &NativeTaskPlan,
    ) -> Result<Vec<NativeTaskInput>> {
        ensure!(
            plan.session_id() == session_id,
            "Task belongs to a different session"
        );
        let mut inputs = Vec::new();
        for reference in plan.input_references() {
            ensure!(
                reference.session_id() == session_id,
                "Task input belongs to a different session"
            );
            let content = self.open_content(session_id, reference.result_id()).await?;
            verify_input_metadata(&reference, content.result())?;
            let path = content.path().to_path_buf();
            inputs.push(NativeTaskInput::from_verified_media(
                reference, path, content,
            ));
        }
        Ok(inputs)
    }
}

fn verify_input_metadata(
    expected: &NativeTaskInputReference,
    result: &MediaResultRef,
) -> Result<()> {
    ensure!(
        expected.session_id() == result.session_id
            && expected.result_id() == result.result_id
            && expected.sha256().as_str() == result.sha256
            && expected.size_bytes() == result.bytes,
        "Task input identity, digest or length differs from the verified result"
    );
    let matches = match (expected, result.kind, &result.spec) {
        (
            NativeTaskInputReference::Image(input),
            MediaKind::Image,
            MediaSpec::Image { width, height },
        ) => input.mime_type == result.mime && input.width == *width && input.height == *height,
        (
            NativeTaskInputReference::Audio(input),
            MediaKind::Audio,
            MediaSpec::Audio {
                sample_rate,
                channels,
                sample_frames,
                ..
            },
        ) => {
            input.mime_type == result.mime
                && input.sample_rate == *sample_rate
                && input.channels == u32::from(*channels)
                && input.sample_frames == *sample_frames
        }
        _ => false,
    };
    ensure!(
        matches,
        "Task input kind, MIME type or decoded media dimensions differ from the verified result"
    );
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "task_inputs_tests.rs"]
mod tests;
