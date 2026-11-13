use super::*;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskDerivation {
    pub parent_job_id: String,
    pub parent_snapshot_digest: Sha256Digest,
    /// Empty when the requested value already equals the parent's effective value.
    pub change: Option<AudioTaskParameterChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskParameterChange {
    pub field: String,
    pub before: Value,
    pub after: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum PocketTtsPatch {
    Text(String),
    VoiceReference(AudioTaskMediaReference),
    Seed(u32),
    Temperature(f32),
    MaxFrames(u32),
    TextChunkTokens(u32),
    FramesAfterEos(i32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Qwen3AsrPatch {
    Audio(AudioTaskMediaReference),
    Context(String),
    Language(Qwen3AsrLanguage),
    MaxTokens(u32),
    Chunking(Qwen3AsrChunking),
}

impl PocketTtsSnapshot {
    fn request(&self, job: &str) -> PocketTtsRequest {
        let p = &self.parameters;
        PocketTtsRequest {
            session_id: self.metadata.session_id.clone(),
            job_id: job.into(),
            text: p.text.clone(),
            voice_reference: p.voice_reference.clone(),
            seed: Some(p.seed),
            temperature: Some(p.temperature),
            max_frames: Some(p.max_frames),
            text_chunk_tokens: Some(p.text_chunk_tokens),
            frames_after_eos: Some(p.frames_after_eos),
        }
    }
}

impl Qwen3AsrSnapshot {
    fn request(&self, job: &str) -> Qwen3AsrRequest {
        let p = &self.parameters;
        Qwen3AsrRequest {
            session_id: self.metadata.session_id.clone(),
            job_id: job.into(),
            audio: p.audio.clone(),
            context: Some(p.context.clone()),
            language: Some(p.language),
            max_tokens: Some(p.max_tokens),
            chunking: Some(p.chunking.clone()),
        }
    }
}

impl PocketTtsPatch {
    fn apply(self, request: &mut PocketTtsRequest) {
        match self {
            Self::Text(value) => request.text = value,
            Self::VoiceReference(value) => request.voice_reference = value,
            Self::Seed(value) => request.seed = Some(value),
            Self::Temperature(value) => request.temperature = Some(value),
            Self::MaxFrames(value) => request.max_frames = Some(value),
            Self::TextChunkTokens(value) => request.text_chunk_tokens = Some(value),
            Self::FramesAfterEos(value) => request.frames_after_eos = Some(value),
        }
    }
}

impl Qwen3AsrPatch {
    fn apply(self, request: &mut Qwen3AsrRequest) {
        match self {
            Self::Audio(value) => request.audio = value,
            Self::Context(value) => request.context = Some(value),
            Self::Language(value) => request.language = Some(value),
            Self::MaxTokens(value) => request.max_tokens = Some(value),
            Self::Chunking(value) => request.chunking = Some(value),
        }
    }
}

fn invalid_parent() -> GenerationParameterError {
    invalid(
        "parent",
        "invalid_snapshot",
        "Parent configuration or recorded changes differ from this bound audio adapter",
    )
}

fn change_between<T: Serialize>(
    before: &T,
    after: &T,
) -> GenerationParameterResult<Option<AudioTaskParameterChange>> {
    let before = serde_json::to_value(before).map_err(|_| invalid_parent())?;
    let after = serde_json::to_value(after).map_err(|_| invalid_parent())?;
    let before = before.as_object().ok_or_else(invalid_parent)?;
    let after = after.as_object().ok_or_else(invalid_parent)?;
    if !before.keys().eq(after.keys()) {
        return Err(invalid_parent());
    }
    let mut change = None;
    for (field, value) in after {
        if &before[field] != value {
            if change.is_some() {
                return Err(invalid_parent());
            }
            change = Some(AudioTaskParameterChange {
                field: field.clone(),
                before: before[field].clone(),
                after: value.clone(),
            });
        }
    }
    Ok(change)
}

fn old_value<P: DeserializeOwned>(
    change: &AudioTaskParameterChange,
) -> GenerationParameterResult<P> {
    let object = serde_json::Map::from_iter([(change.field.clone(), change.before.clone())]);
    serde_json::from_value(Value::Object(object)).map_err(|_| invalid_parent())
}

fn validate_metadata(
    stored: &AudioTaskSnapshotMetadata,
    mut canonical: AudioTaskSnapshotMetadata,
) -> GenerationParameterResult<()> {
    if let Some(from) = &stored.derivation {
        services::identity("parentJobId", &from.parent_job_id)?;
        if from.parent_job_id == stored.job_id {
            return Err(invalid_parent());
        }
    }
    canonical.derivation.clone_from(&stored.derivation);
    if canonical != *stored {
        return Err(invalid_parent());
    }
    Ok(())
}

fn child_metadata(
    parent: &AudioTaskSnapshotMetadata,
    session_id: &str,
    job_id: &str,
    expected: &Sha256Digest,
    actual: Sha256Digest,
) -> GenerationParameterResult<AudioTaskDerivation> {
    if parent.session_id != session_id {
        return Err(invalid(
            "parent",
            "result_unavailable",
            "Parent configuration is unavailable in this session",
        ));
    }
    if job_id == parent.job_id {
        return Err(invalid(
            "jobId",
            "new_job_required",
            "Reusing a configuration requires a new job",
        ));
    }
    if expected != &actual {
        return Err(invalid(
            "parent",
            "snapshot_changed",
            "Stored parent configuration digest differs",
        ));
    }
    Ok(AudioTaskDerivation {
        parent_job_id: parent.job_id.clone(),
        parent_snapshot_digest: actual,
        change: None,
    })
}

fn compare_parent<T: Serialize>(
    child: &AudioTaskSnapshotMetadata,
    child_parameters: &T,
    parent: &AudioTaskSnapshotMetadata,
    parent_parameters: &T,
    digest: Sha256Digest,
) -> GenerationParameterResult<()> {
    let stored = child.derivation.as_ref().ok_or_else(invalid_parent)?;
    let mut expected = child_metadata(
        parent,
        &child.session_id,
        &child.job_id,
        &stored.parent_snapshot_digest,
        digest,
    )?;
    expected.change = change_between(parent_parameters, child_parameters)?;
    if &expected != stored {
        return Err(invalid_parent());
    }
    Ok(())
}

impl PocketTtsParameterService {
    pub fn validate_snapshot(&self, snapshot: &PocketTtsSnapshot) -> GenerationParameterResult<()> {
        let request = snapshot.request(&snapshot.metadata.job_id);
        let canonical = self.compile(request.clone())?.snapshot;
        validate_metadata(&snapshot.metadata, canonical.metadata)?;
        if canonical.parameters != snapshot.parameters {
            return Err(invalid_parent());
        }
        if let Some(change) = snapshot
            .metadata
            .derivation
            .as_ref()
            .and_then(|d| d.change.as_ref())
        {
            let mut previous = request;
            old_value::<PocketTtsPatch>(change)?.apply(&mut previous);
            if change_between(
                &self.compile(previous)?.snapshot.parameters,
                &snapshot.parameters,
            )?
            .as_ref()
                != Some(change)
            {
                return Err(invalid_parent());
            }
        }
        Ok(())
    }

    pub fn validate_parent(
        &self,
        child: &PocketTtsSnapshot,
        parent: &PocketTtsSnapshot,
    ) -> GenerationParameterResult<()> {
        self.validate_snapshot(parent)?;
        self.validate_snapshot(child)?;
        compare_parent(
            &child.metadata,
            &child.parameters,
            &parent.metadata,
            &parent.parameters,
            parent.digest()?,
        )
    }

    /// The caller retrieves the snapshot and digest from the authenticated session's immutable configuration row.
    pub fn reuse_configuration(
        &self,
        session_id: &str,
        parent: &PocketTtsSnapshot,
        expected_parent_digest: &Sha256Digest,
        new_job_id: &str,
        patch: PocketTtsPatch,
    ) -> GenerationParameterResult<PocketTtsPreparedTask> {
        let mut lineage = child_metadata(
            &parent.metadata,
            session_id,
            new_job_id,
            expected_parent_digest,
            parent.digest()?,
        )?;
        self.validate_snapshot(parent)?;
        let mut request = parent.request(new_job_id);
        patch.apply(&mut request);
        let mut prepared = self.compile(request)?;
        lineage.change = change_between(&parent.parameters, &prepared.snapshot.parameters)?;
        prepared.snapshot.metadata.derivation = Some(lineage);
        Ok(prepared)
    }
}

impl Qwen3AsrParameterService {
    pub fn validate_snapshot(&self, snapshot: &Qwen3AsrSnapshot) -> GenerationParameterResult<()> {
        let request = snapshot.request(&snapshot.metadata.job_id);
        let canonical = self.compile(request.clone())?.snapshot;
        validate_metadata(&snapshot.metadata, canonical.metadata)?;
        if canonical.parameters != snapshot.parameters {
            return Err(invalid_parent());
        }
        if let Some(change) = snapshot
            .metadata
            .derivation
            .as_ref()
            .and_then(|d| d.change.as_ref())
        {
            let mut previous = request;
            old_value::<Qwen3AsrPatch>(change)?.apply(&mut previous);
            if change_between(
                &self.compile(previous)?.snapshot.parameters,
                &snapshot.parameters,
            )?
            .as_ref()
                != Some(change)
            {
                return Err(invalid_parent());
            }
        }
        Ok(())
    }

    pub fn validate_parent(
        &self,
        child: &Qwen3AsrSnapshot,
        parent: &Qwen3AsrSnapshot,
    ) -> GenerationParameterResult<()> {
        self.validate_snapshot(parent)?;
        self.validate_snapshot(child)?;
        compare_parent(
            &child.metadata,
            &child.parameters,
            &parent.metadata,
            &parent.parameters,
            parent.digest()?,
        )
    }

    pub fn reuse_configuration(
        &self,
        session_id: &str,
        parent: &Qwen3AsrSnapshot,
        expected_parent_digest: &Sha256Digest,
        new_job_id: &str,
        patch: Qwen3AsrPatch,
    ) -> GenerationParameterResult<Qwen3AsrPreparedTask> {
        let mut lineage = child_metadata(
            &parent.metadata,
            session_id,
            new_job_id,
            expected_parent_digest,
            parent.digest()?,
        )?;
        self.validate_snapshot(parent)?;
        let mut request = parent.request(new_job_id);
        patch.apply(&mut request);
        let mut prepared = self.compile(request)?;
        lineage.change = change_between(&parent.parameters, &prepared.snapshot.parameters)?;
        prepared.snapshot.metadata.derivation = Some(lineage);
        Ok(prepared)
    }
}
