use super::*;

/// Prepared values come only from the model-specific compilers, never raw argv JSON.
#[derive(Debug, Clone)]
pub enum NativeTaskPlan {
    Generation(Box<PreparedGenerationTask>),
    PocketTts(Box<PocketTtsPreparedTask>),
    Qwen3Asr(Box<Qwen3AsrPreparedTask>),
}

impl From<PreparedGenerationTask> for NativeTaskPlan {
    fn from(value: PreparedGenerationTask) -> Self {
        Self::Generation(Box::new(value))
    }
}
impl From<PocketTtsPreparedTask> for NativeTaskPlan {
    fn from(value: PocketTtsPreparedTask) -> Self {
        Self::PocketTts(Box::new(value))
    }
}
impl From<Qwen3AsrPreparedTask> for NativeTaskPlan {
    fn from(value: Qwen3AsrPreparedTask) -> Self {
        Self::Qwen3Asr(Box::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTaskInputReference {
    Image(GenerationMediaInput),
    Audio(AudioTaskMediaReference),
}

impl NativeTaskInputReference {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Image(v) => &v.session_id,
            Self::Audio(v) => &v.session_id,
        }
    }
    pub fn result_id(&self) -> &str {
        match self {
            Self::Image(v) => &v.result_id,
            Self::Audio(v) => &v.result_id,
        }
    }
    pub fn sha256(&self) -> &Sha256Digest {
        match self {
            Self::Image(v) => &v.sha256,
            Self::Audio(v) => &v.sha256,
        }
    }
    pub fn size_bytes(&self) -> u64 {
        match self {
            Self::Image(v) => v.size_bytes,
            Self::Audio(v) => v.size_bytes,
        }
    }
}

impl NativeTaskPlan {
    pub fn snapshot_digest(&self) -> Result<Sha256Digest> {
        match self {
            Self::Generation(v) => v.snapshot().digest(),
            Self::PocketTts(v) => Ok(v.snapshot().digest()?),
            Self::Qwen3Asr(v) => Ok(v.snapshot().digest()?),
        }
    }
    pub fn installation_id(&self) -> &str {
        match self {
            Self::Generation(v) => &v.snapshot().binding.installation_id,
            Self::PocketTts(v) => &v.snapshot().metadata.binding.installation_id,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.binding.installation_id,
        }
    }
    pub fn version_id(&self) -> &str {
        match self {
            Self::Generation(v) => &v.snapshot().binding.version_id,
            Self::PocketTts(v) => &v.snapshot().metadata.binding.version_id,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.binding.version_id,
        }
    }
    pub fn manifest_digest(&self) -> &Sha256Digest {
        match self {
            Self::Generation(v) => &v.snapshot().binding.manifest_digest,
            Self::PocketTts(v) => &v.snapshot().metadata.binding.manifest_digest,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.binding.manifest_digest,
        }
    }
    pub fn runtime(&self) -> &RuntimeBuild {
        match self {
            Self::Generation(v) => &v.snapshot().binding.runtime,
            Self::PocketTts(v) => &v.snapshot().metadata.binding.runtime,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.binding.runtime,
        }
    }
    pub fn session_id(&self) -> &str {
        match self {
            Self::Generation(v) => &v.snapshot().session_id,
            Self::PocketTts(v) => &v.snapshot().metadata.session_id,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.session_id,
        }
    }
    pub fn job_id(&self) -> &str {
        match self {
            Self::Generation(v) => &v.snapshot().job_id,
            Self::PocketTts(v) => &v.snapshot().metadata.job_id,
            Self::Qwen3Asr(v) => &v.snapshot().metadata.job_id,
        }
    }

    pub fn input_references(&self) -> Vec<NativeTaskInputReference> {
        match self {
            Self::Generation(value) => match &value.snapshot().parameters {
                EffectiveGenerationParameters::Image(_) => Vec::new(),
                EffectiveGenerationParameters::Video(v) => v
                    .first_frame
                    .iter()
                    .cloned()
                    .map(NativeTaskInputReference::Image)
                    .collect(),
            },
            Self::PocketTts(v) => vec![NativeTaskInputReference::Audio(
                v.snapshot().parameters.voice_reference.clone(),
            )],
            Self::Qwen3Asr(v) => vec![NativeTaskInputReference::Audio(
                v.snapshot().parameters.audio.clone(),
            )],
        }
    }

    /// Verify the prepared task's component, runtime and model identities against the registered version.
    pub fn validate_manifest(&self, installed: &InstalledVersion) -> Result<()> {
        ensure!(
            &installed.manifest_digest == self.manifest_digest(),
            "Task manifest digest differs from registered version"
        );
        match self {
            Self::Generation(v) => ensure!(
                GenerationModelBinding::from_manifest(&installed.manifest)? == v.snapshot().binding,
                "Task generation binding differs from registered manifest"
            ),
            Self::PocketTts(v) => ensure!(
                AudioTaskModelBinding::from_manifest(&installed.manifest)?
                    == v.snapshot().metadata.binding,
                "Task TTS binding differs from registered manifest"
            ),
            Self::Qwen3Asr(v) => ensure!(
                AudioTaskModelBinding::from_manifest(&installed.manifest)?
                    == v.snapshot().metadata.binding,
                "Task ASR binding differs from registered manifest"
            ),
        }
        Ok(())
    }

    fn output_extension(&self) -> &'static str {
        match self {
            Self::Generation(v) => v.snapshot().engine.output_format.extension(),
            Self::PocketTts(_) => "wav",
            Self::Qwen3Asr(_) => "txt",
        }
    }

    pub(super) fn output_name(&self) -> String {
        format!("output.{}", self.output_extension())
    }

    pub fn expected_output_names(&self, max_bytes: u64) -> Result<Vec<String>> {
        if let Self::Generation(v) = self {
            if let EffectiveGenerationParameters::Image(image) = &v.snapshot().parameters {
                // Allocation is bounded before collecting the native batch's filenames.
                ensure!(
                    u64::from(image.batch_count) <= max_bytes / 8 && image.batch_count <= 1024,
                    "Image batch exceeds task output enumeration bound"
                );
                if image.batch_count > 1 {
                    return Ok((0..image.batch_count)
                        .map(|index| format!("output_{index}.{}", self.output_extension()))
                        .collect());
                }
            }
        }
        Ok(vec![self.output_name()])
    }

    pub(super) fn arguments(
        &self,
        installed: &InstalledVersion,
        directory: &Path,
        inputs: &[NativeTaskInput],
        output: &Path,
    ) -> Result<Vec<OsString>> {
        let artifact = |id: &str| -> Result<OsString> {
            let file = installed
                .manifest
                .installed_files
                .get(id)
                .context("Task component is absent from installation")?;
            Ok(checked_local_path(directory, &file.relative_path)?.into_os_string())
        };
        let input = |expected: &NativeTaskInputReference| -> Result<OsString> {
            let found = inputs
                .iter()
                .find(|value| &value.reference == expected)
                .context("Task input does not match verified session media")?;
            Ok(found.path.clone().into_os_string())
        };
        match self {
            Self::Generation(v) => v
                .arguments()
                .iter()
                .map(|argument| match argument {
                    NativeGenerationArgument::Literal(v) => Ok(OsString::from(v)),
                    NativeGenerationArgument::Component(role) => {
                        artifact(&v.snapshot().binding.assets[role].artifact_id)
                    }
                    NativeGenerationArgument::InputResult(id) => {
                        let references = self.input_references();
                        input(
                            references
                                .iter()
                                .find(|r| r.result_id() == id)
                                .context("Unbound task image input")?,
                        )
                    }
                    NativeGenerationArgument::Output => Ok(output.as_os_str().to_owned()),
                })
                .collect(),
            Self::PocketTts(v) => audio_arguments(
                v.arguments(),
                &v.snapshot().metadata.binding,
                artifact,
                input,
                output,
            ),
            Self::Qwen3Asr(v) => audio_arguments(
                v.arguments(),
                &v.snapshot().metadata.binding,
                artifact,
                input,
                output,
            ),
        }
    }
}

fn audio_arguments(
    args: &[NativeAudioArgument],
    binding: &AudioTaskModelBinding,
    artifact: impl Fn(&str) -> Result<OsString>,
    input: impl Fn(&NativeTaskInputReference) -> Result<OsString>,
    output: &Path,
) -> Result<Vec<OsString>> {
    args.iter()
        .map(|arg| {
            let path = match arg {
                NativeAudioArgument::Literal(v) => return Ok(OsString::from(v)),
                NativeAudioArgument::Model => artifact(&binding.model.artifact_id)?,
                NativeAudioArgument::ModelSpec => artifact(&binding.model_spec.artifact_id)?,
                NativeAudioArgument::Input(v) => input(&NativeTaskInputReference::Audio(v.clone()))?,
                NativeAudioArgument::Output => output.as_os_str().to_owned(),
            };
            #[cfg(windows)]
            if !binding.runtime.features.contains("windows-long-path-aware") {
                ensure!(legacy_audio_path_fits(&path),
                    "The installed audio runtime cannot reopen long Windows paths. Install the long-path-aware model version.");
            }
            Ok(path)
        })
        .collect()
}

#[cfg(windows)]
fn legacy_audio_path_fits(path: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let units = path.encode_wide().collect::<Vec<_>>();
    let device_prefix = [92, 92, 63, 92];
    let normalized_length = if units.starts_with(&device_prefix) {
        let rest = &units[4..];
        if rest.starts_with(&[85, 78, 67, 92]) {
            rest.len() - 2
        } else {
            rest.len()
        }
    } else {
        units.len()
    };
    normalized_length < 260
}

#[cfg(all(test, windows))]
#[test]
fn legacy_audio_uses_utf16_length_after_windows_prefix_normalization() {
    use std::ffi::OsStr;
    let suffix = "x".repeat(254);
    for prefix in [r"D:\", r"\\?\D:\"] {
        let path = format!("{prefix}{suffix}🟢");
        assert!(legacy_audio_path_fits(OsStr::new(&path)));
        assert!(!legacy_audio_path_fits(OsStr::new(&format!("{path}y"))));
    }
    let server = format!(r"server\share\{}", "x".repeat(244));
    assert!(legacy_audio_path_fits(OsStr::new(&format!(
        r"\\?\UNC\{server}"
    ))));
    assert!(!legacy_audio_path_fits(OsStr::new(&format!(
        r"\\?\UNC\{server}x"
    ))));
}
