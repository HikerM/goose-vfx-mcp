use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    EffectiveGenerationParameters, GenerationOutputFormat, NativeTaskOutput, NativeTaskPlan,
    Sha256Digest,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use super::{
    domain, files, formats, BeginMediaPublish, MediaAssociation, MediaInputRef, MediaKind,
    MediaResultRef, MediaResultStore, MediaRuntimeBuild, MediaSource, MediaSpec,
    PublishMediaRequest, StagedMedia, TaskConfigurationStore, MAX_MEDIA_BYTES,
};

pub const MAX_TASK_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTaskTranscript {
    pub session_id: String,
    pub job_id: String,
    pub association: MediaAssociation,
    pub generation_config_sha256: String,
    pub bytes: u64,
    pub sha256: String,
    /// Exact UTF-8 produced by the recognizer, including the CLI line ending.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedTaskOutput {
    Media(Vec<MediaResultRef>),
    Transcript(NativeTaskTranscript),
}

#[derive(Clone, Copy)]
enum ExpectedOutput {
    Image {
        format: GenerationOutputFormat,
        width: u32,
        height: u32,
    },
    Video {
        format: GenerationOutputFormat,
        width: u32,
        height: u32,
        frames: u32,
        with_audio: bool,
        fps: u32,
    },
    Speech,
    Transcript,
}

impl ExpectedOutput {
    fn from_plan(plan: &NativeTaskPlan) -> Self {
        match plan {
            NativeTaskPlan::Generation(task) => match &task.snapshot().parameters {
                EffectiveGenerationParameters::Image(p) => Self::Image {
                    format: task.snapshot().engine.output_format,
                    width: p.width,
                    height: p.height,
                },
                EffectiveGenerationParameters::Video(p) => Self::Video {
                    format: task.snapshot().engine.output_format,
                    width: p.width,
                    height: p.height,
                    frames: p.frames,
                    with_audio: p.audio_mode == lumina_providers::local_inference::lifecycle::GenerationAudioMode::JointAudioVideo,
                    fps: p.fps,
                },
            },
            NativeTaskPlan::PocketTts(_) => Self::Speech,
            NativeTaskPlan::Qwen3Asr(_) => Self::Transcript,
        }
    }

    fn inspect(self, file: &mut File) -> Result<formats::ValidatedMedia> {
        let media = match self {
            Self::Video {
                with_audio: true, ..
            } => formats::inspect_video_audio(file)?,
            Self::Video {
                with_audio: false, ..
            } => formats::inspect_video_only(file)?,
            _ => formats::inspect(file)?,
        };
        let conforms = match (self, &media.spec) {
            (
                Self::Image {
                    format,
                    width,
                    height,
                },
                MediaSpec::Image {
                    width: w,
                    height: h,
                },
            ) => media.mime == format.mime() && (width, height) == (*w, *h),
            (
                Self::Video {
                    format,
                    width,
                    height,
                    frames,
                    fps,
                    ..
                },
                MediaSpec::Video {
                    width: w,
                    height: h,
                    frame_count,
                    fps_numerator,
                    fps_denominator,
                    ..
                },
            ) => {
                media.mime == format.mime()
                    && (width, height) == (*w, *h)
                    && u64::from(frames) == *frame_count
                    && *fps_denominator > 0
                    && u64::from(fps) * u64::from(*fps_denominator) == u64::from(*fps_numerator)
            }
            (
                Self::Speech,
                MediaSpec::Audio {
                    sample_rate: 24_000,
                    channels: 1,
                    ..
                },
            ) => media.mime == "audio/wav" && is_pcm16_wav(file)?,
            _ => false,
        };
        ensure!(
            conforms,
            "Decoded native output differs from the task's effective configuration"
        );
        Ok(media)
    }
}

// The shared WAV validator checks complete samples and accepts multiple encodings.
// The frozen PocketTTS CLI specifically requested PCM16, which MediaSpec omits.
fn is_pcm16_wav(file: &mut File) -> Result<bool> {
    file.seek(SeekFrom::Start(12))?;
    let end = file.metadata()?.len();
    while file.stream_position()? < end {
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_le_bytes(header[4..].try_into()?);
        if &header[..4] == b"fmt " {
            let mut fmt = [0; 16];
            file.read_exact(&mut fmt)?;
            return Ok(u16::from_le_bytes(fmt[..2].try_into()?) == 1
                && u16::from_le_bytes(fmt[14..].try_into()?) == 16);
        }
        file.seek(SeekFrom::Current(i64::from(size) + i64::from(size % 2)))?;
    }
    Ok(false)
}

struct OutputReader {
    file: File,
    name: String,
    bytes: u64,
    sha256: String,
}

struct CheckedMedia {
    input: OutputReader,
    decoded: formats::ValidatedMedia,
}

enum CheckedOutput {
    Media(Vec<CheckedMedia>),
    Text { input: OutputReader, text: String },
}

fn prepare_readers(
    expected: ExpectedOutput,
    names: &[String],
    readers: Vec<OutputReader>,
) -> Result<CheckedOutput> {
    ensure!(
        !readers.is_empty()
            && readers.len() == names.len()
            && readers
                .iter()
                .zip(names)
                .all(|(file, name)| &file.name == name),
        "Native output layout differs from its task plan"
    );
    let mut checked = Vec::new();
    for mut input in readers {
        domain::validate_digest(&input.sha256)?;
        ensure!(
            input.file.metadata()?.len() == input.bytes,
            "Native output length changed after collection"
        );
        if matches!(expected, ExpectedOutput::Transcript) {
            ensure!(
                names.len() == 1 && input.bytes > 0 && input.bytes <= MAX_TASK_TRANSCRIPT_BYTES,
                "ASR transcript exceeds its byte bounds"
            );
            let mut bytes = Vec::with_capacity(input.bytes as usize);
            copy_verified(&mut input, &mut bytes)?;
            let text = String::from_utf8(bytes).context("ASR transcript is not UTF-8")?;
            ensure!(
                !text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
                "ASR transcript contains non-text control characters"
            );
            return Ok(CheckedOutput::Text { input, text });
        }
        ensure!(
            input.bytes > 0 && input.bytes <= MAX_MEDIA_BYTES,
            "Native media exceeds the publication byte limit"
        );
        let decoded = expected.inspect(&mut input.file)?;
        ensure!(
            files::digest(&mut input.file)? == input.sha256,
            "Native output digest changed after collection"
        );
        checked.push(CheckedMedia { input, decoded });
    }
    Ok(CheckedOutput::Media(checked))
}

fn copy_verified(input: &mut OutputReader, destination: &mut impl Write) -> Result<()> {
    input.file.rewind()?;
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while total < input.bytes {
        let length = usize::try_from((input.bytes - total).min(buffer.len() as u64))?;
        input.file.read_exact(&mut buffer[..length])?;
        destination.write_all(&buffer[..length])?;
        hash.update(&buffer[..length]);
        total += length as u64;
    }
    ensure!(
        input.file.read(&mut buffer[..1])? == 0,
        "Native output grew during import"
    );
    ensure!(
        files::hex_digest(&hash.finalize()) == input.sha256,
        "Copied native output digest differs from the collected artifact"
    );
    Ok(())
}

pub(super) fn native_job_result_id(
    session_id: &str,
    job_id: &str,
    snapshot_digest: &Sha256Digest,
    index: usize,
) -> String {
    let mut hash = Sha256::new();
    for part in [
        "lumina-native-task-media-v1",
        session_id,
        job_id,
        snapshot_digest.as_str(),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.update((index as u64).to_le_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn output_request(
    plan: &NativeTaskPlan,
    snapshot_digest: &Sha256Digest,
    root_id: &str,
    association: &MediaAssociation,
    index: usize,
    kind: MediaKind,
) -> PublishMediaRequest {
    let input_refs = plan
        .input_references()
        .iter()
        .map(|input| MediaInputRef::MediaResult {
            result_id: input.result_id().into(),
        })
        .collect();
    PublishMediaRequest {
        result_id: native_job_result_id(plan.session_id(), plan.job_id(), snapshot_digest, index),
        session_id: plan.session_id().into(),
        job_id: plan.job_id().into(),
        root_id: root_id.into(),
        kind,
        association: association.clone(),
        source: MediaSource::LocalModel {
            installation_id: plan.installation_id().into(),
            version_id: plan.version_id().into(),
            runtime: MediaRuntimeBuild {
                adapter_id: plan.runtime().adapter_id.clone(),
                engine_revision: plan.runtime().engine_revision.clone(),
                binary_sha256: plan.runtime().binary_sha256.as_str().into(),
            },
            generation_config_sha256: snapshot_digest.as_str().into(),
            input_refs,
        },
    }
}

fn verify_media(result: &MediaResultRef, input: &CheckedMedia) -> Result<()> {
    ensure!(
        result.kind == input.decoded.kind
            && result.mime == input.decoded.mime
            && result.spec == input.decoded.spec
            && result.bytes == input.input.bytes
            && result.sha256 == input.input.sha256,
        "Published result differs from the verified native output"
    );
    Ok(())
}

async fn copy_stage(
    store: &MediaResultStore,
    mut stage: StagedMedia,
    mut media: CheckedMedia,
) -> Result<(StagedMedia, CheckedMedia)> {
    let (stage, media, copied) = tokio::task::spawn_blocking(move || {
        let copied = copy_verified(&mut media.input, &mut stage);
        (stage, media, copied)
    })
    .await
    .context("Native media copy worker failed")?;
    if let Err(error) = copied {
        store
            .abort_staged(stage)
            .await
            .context("Failed to discard incomplete native output")?;
        return Err(error);
    }
    Ok((stage, media))
}

/// The authenticated task owner supplies the same prepared plan it executed and a registered output root.
/// Result IDs are stable across a retry, including after a previously committed batch prefix.
pub async fn import_native_task_output(
    configurations: &TaskConfigurationStore,
    media: &MediaResultStore,
    plan: &NativeTaskPlan,
    output: NativeTaskOutput,
    root_id: &str,
    association: MediaAssociation,
) -> Result<ImportedTaskOutput> {
    let digest = plan.snapshot_digest()?;
    ensure!(
        output.snapshot_digest() == &digest,
        "Native output belongs to a different executed task"
    );
    let plan = plan.clone();
    let configurations = configurations.clone();
    let media = media.clone();
    let root_id = root_id.to_owned();
    // Once handed off, the worker keeps the native directory and publication journal
    // alive through completion even if the calling connection drops its await.
    tokio::spawn(async move {
        let readers = output
            .files()
            .iter()
            .map(|file| {
                Ok(OutputReader {
                    file: file.try_clone_reader()?,
                    name: file.name().into(),
                    bytes: file.size_bytes(),
                    sha256: file.sha256().as_str().into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let result = import_readers(
            &configurations,
            &media,
            &plan,
            &digest,
            readers,
            &root_id,
            association,
        )
        .await;
        drop(output);
        result
    })
    .await
    .context("Native output import worker failed")?
}

async fn import_readers(
    configurations: &TaskConfigurationStore,
    media: &MediaResultStore,
    plan: &NativeTaskPlan,
    digest: &Sha256Digest,
    readers: Vec<OutputReader>,
    root_id: &str,
    association: MediaAssociation,
) -> Result<ImportedTaskOutput> {
    let config = configurations
        .read(plan.session_id(), plan.job_id())
        .await?
        .context("Native task has no persisted configuration")?;
    ensure!(
        config.generation_config_sha256() == digest.as_str()
            && config.identity().installation_id == plan.installation_id()
            && config.identity().version_id == plan.version_id()
            && config.identity().manifest_digest == plan.manifest_digest().as_str(),
        "Native task plan differs from its persisted configuration"
    );
    let expected = ExpectedOutput::from_plan(plan);
    let total = readers.iter().try_fold(0_u64, |sum, file| {
        sum.checked_add(file.bytes)
            .context("Native task output length overflow")
    })?;
    let layout = plan.expected_output_names(total)?;
    let validated =
        tokio::task::spawn_blocking(move || prepare_readers(expected, &layout, readers))
            .await
            .context("Native output validation worker failed")??;
    let imported = match validated {
        CheckedOutput::Text { input, text } => {
            let association_id = match &association {
                MediaAssociation::Message { message_id } => message_id,
                MediaAssociation::ToolCall { tool_call_id } => tool_call_id,
            };
            ensure!(
                !association_id.trim().is_empty()
                    && association_id.len() <= 1024
                    && !association_id.chars().any(char::is_control),
                "Invalid transcript association"
            );
            let transcript = NativeTaskTranscript {
                session_id: plan.session_id().into(),
                job_id: plan.job_id().into(),
                association,
                generation_config_sha256: digest.as_str().into(),
                bytes: input.bytes,
                sha256: input.sha256,
                text,
            };
            configurations.save_transcript(&transcript).await?;
            ImportedTaskOutput::Transcript(transcript)
        }
        CheckedOutput::Media(outputs) => {
            let mut results = Vec::with_capacity(outputs.len());
            for (index, item) in outputs.into_iter().enumerate() {
                let request = output_request(
                    plan,
                    digest,
                    root_id,
                    &association,
                    index,
                    item.decoded.kind,
                );
                configurations.verify_publish(&request).await?;
                let (result, item) = match media.begin_publish(request).await? {
                    BeginMediaPublish::Existing(existing) => (existing, item),
                    BeginMediaPublish::Staging(stage) => {
                        let (stage, item) = copy_stage(media, stage, item).await?;
                        (media.publish(stage).await?, item)
                    }
                };
                verify_media(&result, &item)?;
                configurations.verify_result(&result).await?;
                results.push(result);
            }
            ImportedTaskOutput::Media(results)
        }
    };
    Ok(imported)
}

#[cfg(test)]
#[path = "task_output_tests.rs"]
mod tests;
