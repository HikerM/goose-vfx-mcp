use super::*;
use serde_json::json;
use uuid::Uuid;

impl GenerationParameterService {
    pub fn compile_json(
        &self,
        request: Value,
    ) -> GenerationParameterResult<PreparedGenerationTask> {
        if let Some(parameters) = request["parameters"]["parameters"].as_object() {
            for (name, value) in parameters {
                match self.descriptor.fields.get(name) {
                    None => {
                        return Err(issue(
                            name,
                            "unknown_parameter",
                            "Parameter is not declared by this model/task schema",
                        ))
                    }
                    Some(field)
                        if field.support == GenerationParameterSupport::Unsupported
                            && !value.is_null() =>
                    {
                        return Err(issue(
                            name,
                            "unsupported_parameter",
                            &field.constraints.join("; "),
                        ));
                    }
                    _ => {}
                }
            }
        }
        let request = serde_json::from_value(request)
            .map_err(|error| issue("request", "invalid_schema", &error.to_string()))?;
        self.compile(request)
    }

    pub fn compile(
        &self,
        request: GenerationTaskRequest,
    ) -> GenerationParameterResult<PreparedGenerationTask> {
        identity("sessionId", &request.session_id)?;
        identity("jobId", &request.job_id)?;
        validate_float_fields(&request.parameters)?;
        let supplied = parameter_values(&request.parameters)?;
        let parameters = match request.parameters {
            GenerationParameters::Image(value) if self.profile.is_image() => {
                EffectiveGenerationParameters::Image(image(self.profile.defaults(), value)?)
            }
            GenerationParameters::Video(value) if !self.profile.is_image() => {
                EffectiveGenerationParameters::Video(video(
                    self.profile,
                    &request.session_id,
                    value,
                )?)
            }
            _ => {
                return Err(issue(
                    "parameters.task",
                    "wrong_model_schema",
                    "Parameters do not belong to this model and task variant",
                ))
            }
        };
        let effective = parameter_values(&parameters)?;
        let origins = effective
            .keys()
            .map(|name| {
                let origin = if matches!(name.as_str(), "aspectRatio" | "duration")
                    || (name == "frames"
                        && supplied.get("frames").is_none_or(Value::is_null)
                        && supplied
                            .get("durationSeconds")
                            .is_some_and(|value| !value.is_null()))
                {
                    GenerationParameterOrigin::Derived
                } else if name == "seed"
                    && supplied
                        .get(name)
                        .is_some_and(|value| value["kind"] == "new")
                {
                    GenerationParameterOrigin::GeneratedSeed
                } else if supplied.get(name).is_some_and(|value| !value.is_null()) {
                    GenerationParameterOrigin::Request
                } else {
                    GenerationParameterOrigin::RecordedDefault {
                        evidence: self.profile.evidence().into(),
                    }
                };
                (name.clone(), origin)
            })
            .collect();
        let snapshot = GenerationParameterSnapshot {
            schema_version: GENERATION_PARAMETER_SCHEMA,
            descriptor_digest: Sha256Digest::of_serializable(&self.descriptor).map_err(internal)?,
            profile: self.profile,
            binding: self.binding.clone(),
            session_id: request.session_id,
            job_id: request.job_id,
            observation: observation(self.profile, &parameters),
            parameters,
            engine: self.profile.engine(),
            origins,
            parent: None,
            changes: BTreeMap::new(),
            execution_status: GenerationExecutionStatus::NeedsResourceAdmission,
        };
        let arguments = arguments(&snapshot);
        Ok(PreparedGenerationTask {
            snapshot,
            arguments,
        })
    }

    /// `record` must come from the authenticated session's store; matching a string
    /// is not session authorization. Ambiguous targets must be resolved before this API.
    pub fn reuse_result(
        &self,
        session_id: &str,
        record: &GenerationResultRecord,
        selected_result_id: &str,
        new_job_id: &str,
        patch: GenerationParameters,
    ) -> GenerationParameterResult<PreparedGenerationTask> {
        canonical_result_id(selected_result_id)?;
        let previous = &record.snapshot;
        if previous.session_id != session_id
            || !record.outputs.iter().any(|output| {
                output.session_id == session_id && output.result_id == selected_result_id
            })
        {
            return Err(issue(
                "parentResultId",
                "result_unavailable",
                "The selected result is not available in this session",
            ));
        }
        if record.outputs.iter().any(|output| {
            output.session_id != session_id || canonical_result_id(&output.result_id).is_err()
        }) {
            return Err(issue(
                "parentResultId",
                "invalid_record",
                "Result record has invalid output bindings",
            ));
        }
        if previous.binding != self.binding
            || previous.profile != self.profile
            || previous.schema_version != GENERATION_PARAMETER_SCHEMA
            || !self
                .matches_descriptor_digest(&previous.descriptor_digest)
                .map_err(internal)?
        {
            return Err(issue(
                "binding",
                "parent_binding_mismatch",
                "Reuse requires the same model version, components, Runtime and task schema",
            ));
        }
        if previous.digest().map_err(internal)? != record.snapshot_digest {
            return Err(issue(
                "parentResultId",
                "snapshot_changed",
                "Stored parameter snapshot digest does not match",
            ));
        }
        let mut stored_parameters = parameter_values(&previous.parameters)?;
        stored_parameters.remove("duration");
        stored_parameters.insert(
            "seed".into(),
            json!({"kind":"value","value":stored_parameters["seed"]}),
        );
        let task_name = if self.profile.is_image() {
            "image"
        } else {
            "video"
        };
        let stored_request =
            serde_json::from_value(json!({"task":task_name,"parameters":stored_parameters}))
                .map_err(|_| {
                    issue(
                        "parentResultId",
                        "invalid_snapshot",
                        "Stored parameters do not match the bound schema",
                    )
                })?;
        let canonical = self.compile(GenerationTaskRequest {
            session_id: session_id.into(),
            job_id: previous.job_id.clone(),
            parameters: stored_request,
        })?;
        if canonical.snapshot.parameters != previous.parameters
            || canonical.snapshot.engine != previous.engine
            || canonical.snapshot.observation != previous.observation
        {
            return Err(issue(
                "parentResultId",
                "invalid_snapshot",
                "Stored effective parameters differ from the bound compiler",
            ));
        }
        if previous.job_id == new_job_id {
            return Err(issue(
                "jobId",
                "new_job_required",
                "Regeneration creates a new job and does not overwrite the parent",
            ));
        }
        let same_schema = matches!(
            (&previous.parameters, &patch),
            (
                EffectiveGenerationParameters::Image(_),
                GenerationParameters::Image(_)
            ) | (
                EffectiveGenerationParameters::Video(_),
                GenerationParameters::Video(_)
            )
        );
        if !same_schema {
            return Err(issue(
                "parameters.task",
                "wrong_model_schema",
                "Patch schema differs from the selected result",
            ));
        }
        validate_float_fields(&patch)?;
        let old = parameter_values(&previous.parameters)?;
        let explicit = parameter_values(&patch)?;
        let mut merged = old.clone();
        merged.remove("aspectRatio");
        merged.remove("duration");
        merged.insert("seed".into(), json!({"kind":"value","value":old["seed"]}));
        for (key, value) in &explicit {
            if !value.is_null() {
                merged.insert(key.clone(), value.clone());
            }
        }
        if explicit
            .get("durationSeconds")
            .is_some_and(|value| !value.is_null())
            && explicit.get("frames").is_none_or(Value::is_null)
        {
            merged.remove("frames");
        }
        let task = if self.profile.is_image() {
            "image"
        } else {
            "video"
        };
        let parameters = serde_json::from_value(json!({"task":task,"parameters":merged}))
            .map_err(|error| issue("parentResultId", "invalid_snapshot", &error.to_string()))?;
        let mut prepared = self.compile(GenerationTaskRequest {
            session_id: session_id.into(),
            job_id: new_job_id.into(),
            parameters,
        })?;
        // A new seed is chosen once before persistence, never left to the native -1 sentinel.
        if explicit
            .get("seed")
            .is_some_and(|value| value["kind"] == "new")
        {
            while parameter_values(&prepared.snapshot.parameters)?["seed"] == old["seed"] {
                match &mut prepared.snapshot.parameters {
                    EffectiveGenerationParameters::Image(value) => value.seed = fresh_seed(),
                    EffectiveGenerationParameters::Video(value) => value.seed = fresh_seed(),
                }
            }
        }
        let new = parameter_values(&prepared.snapshot.parameters)?;
        prepared.snapshot.origins = new
            .keys()
            .map(|name| {
                let origin = if matches!(name.as_str(), "aspectRatio" | "duration") {
                    GenerationParameterOrigin::Derived
                } else if name == "seed"
                    && explicit
                        .get(name)
                        .is_some_and(|value| value["kind"] == "new")
                {
                    GenerationParameterOrigin::GeneratedSeed
                } else if explicit.get(name).is_some_and(|value| !value.is_null()) {
                    GenerationParameterOrigin::Request
                } else if name == "frames"
                    && explicit
                        .get("durationSeconds")
                        .is_some_and(|value| !value.is_null())
                {
                    GenerationParameterOrigin::Derived
                } else {
                    GenerationParameterOrigin::Parent {
                        result_id: selected_result_id.into(),
                    }
                };
                (name.clone(), origin)
            })
            .collect();
        prepared.snapshot.changes = new
            .iter()
            .filter(|(name, after)| old.get(*name) != Some(*after))
            .map(|(name, after)| {
                let before = old.get(name).cloned().unwrap_or(Value::Null);
                (
                    name.clone(),
                    GenerationParameterChange {
                        before,
                        after: after.clone(),
                    },
                )
            })
            .collect();
        prepared.snapshot.parent = Some(GenerationParent {
            result_id: selected_result_id.into(),
            snapshot_digest: record.snapshot_digest.clone(),
        });
        prepared.snapshot.observation = observation(self.profile, &prepared.snapshot.parameters);
        prepared.arguments = arguments(&prepared.snapshot);
        Ok(prepared)
    }
}

fn internal(_: anyhow::Error) -> GenerationParameterError {
    issue(
        "parameters",
        "serialization_failed",
        "Unable to serialize parameter data",
    )
}

fn validate_float_fields(parameters: &GenerationParameters) -> GenerationParameterResult<()> {
    // serde_json turns non-finite floats into null, which would erase an explicit patch.
    let (cfg_scale, duration_seconds) = match parameters {
        GenerationParameters::Image(value) => (value.cfg_scale, None),
        GenerationParameters::Video(value) => (value.cfg_scale, value.duration_seconds),
    };
    if let Some(value) = cfg_scale {
        cfg(value)?;
    }
    if duration_seconds.is_some_and(|value| !value.is_finite()) {
        return Err(issue(
            "durationSeconds",
            "out_of_range",
            "Duration must be finite",
        ));
    }
    Ok(())
}

fn parameter_values(value: &impl Serialize) -> GenerationParameterResult<BTreeMap<String, Value>> {
    let value = serde_json::to_value(value).map_err(|_| {
        issue(
            "parameters",
            "invalid_value",
            "Parameters contain a value that cannot be serialized",
        )
    })?;
    serde_json::from_value(value["parameters"].clone()).map_err(|_| {
        issue(
            "parameters",
            "invalid_schema",
            "Expected typed parameter object",
        )
    })
}

fn identity(field: &str, value: &str) -> GenerationParameterResult<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(issue(
            field,
            "invalid_identity",
            "Identity must contain 1..256 bytes without control characters",
        ));
    }
    Ok(())
}

fn canonical_result_id(value: &str) -> GenerationParameterResult<()> {
    if !Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        return Err(issue(
            "resultId",
            "invalid_reference",
            "Expected a canonical result UUID",
        ));
    }
    Ok(())
}

pub(super) fn text(field: &str, value: &str, required: bool) -> GenerationParameterResult<()> {
    if (required && value.trim().is_empty()) || value.len() > 16_384 || value.contains('\0') {
        return Err(issue(
            field,
            "invalid_text",
            "Expected bounded text without NUL (maximum 16384 UTF-8 bytes)",
        ));
    }
    if value.to_ascii_lowercase().contains("<lora:") {
        return Err(issue(
            field,
            "unbound_asset",
            "Implicit LoRA loading is not supported by this bound profile",
        ));
    }
    Ok(())
}

pub(super) fn integer(
    field: &str,
    value: u32,
    minimum: u32,
    step: u32,
    offset: u32,
) -> GenerationParameterResult<()> {
    if value < minimum || value > i32::MAX as u32 || value % step != offset {
        return Err(issue(
            field,
            "out_of_range",
            "Value violates the native integer range or model alignment",
        ));
    }
    Ok(())
}

pub(super) fn geometry(
    width: u32,
    height: u32,
    multiple: u32,
    requested: Option<GenerationAspectRatio>,
) -> GenerationParameterResult<GenerationAspectRatio> {
    integer("width", width, multiple, multiple, 0)?;
    integer("height", height, multiple, multiple, 0)?;
    if u64::from(width) * u64::from(height) * 3 > i32::MAX as u64 {
        return Err(issue(
            "width,height",
            "native_shape_overflow",
            "RGB geometry exceeds the native signed integer representation",
        ));
    }
    if let Some(ratio) = requested {
        if ratio.width == 0
            || ratio.height == 0
            || u64::from(width) * u64::from(ratio.height)
                != u64::from(height) * u64::from(ratio.width)
        {
            return Err(issue(
                "aspectRatio",
                "conflicting_geometry",
                "Aspect ratio must agree with explicit/effective width and height",
            ));
        }
    }
    let (mut a, mut b) = (width, height);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ok(GenerationAspectRatio {
        width: width / a,
        height: height / a,
    })
}

pub(super) fn cfg(value: f32) -> GenerationParameterResult<()> {
    if !value.is_finite() || value < 0. {
        return Err(issue(
            "cfgScale",
            "out_of_range",
            "CFG must be finite and non-negative",
        ));
    }
    Ok(())
}
fn fresh_seed() -> u32 {
    u32::from_le_bytes(
        Uuid::new_v4().as_bytes()[0..4]
            .try_into()
            .expect("UUID prefix"),
    )
}
fn seed(value: Option<GenerationSeed>, default: u32) -> u32 {
    match value {
        None => default,
        Some(GenerationSeed::Value { value }) => value,
        Some(GenerationSeed::New) => fresh_seed(),
    }
}

fn select_option<T: Copy + PartialEq>(
    field: &str,
    allowed: &[T],
    requested: Option<T>,
) -> GenerationParameterResult<T> {
    let value = requested.unwrap_or(allowed[0]);
    if allowed.contains(&value) {
        Ok(value)
    } else {
        Err(issue(
            field,
            "unsupported_by_profile",
            "Value is not supported by the selected model profile",
        ))
    }
}

fn image(
    defaults: &super::inventory::Defaults,
    value: DiffusionImageParameters,
) -> GenerationParameterResult<EffectiveImageParameters> {
    let prompt = value.prompt.unwrap_or_default();
    let negative_prompt = value
        .negative_prompt
        .unwrap_or_else(|| defaults.negative_prompt.clone());
    text("prompt", &prompt, true)?;
    text("negativePrompt", &negative_prompt, false)?;
    let (width, height) = (
        value.width.unwrap_or(defaults.width),
        value.height.unwrap_or(defaults.height),
    );
    let aspect_ratio = geometry(
        width,
        height,
        defaults.dimension_multiple,
        value.aspect_ratio,
    )?;
    let steps = value.steps.unwrap_or(defaults.steps);
    integer("steps", steps, 1, 1, 0)?;
    let batch_count = value.batch_count.unwrap_or(1);
    integer("batchCount", batch_count, 1, 1, 0)?;
    if u64::from(width) * u64::from(height) * u64::from(batch_count) > i64::MAX as u64 / 16 {
        return Err(issue(
            "batchCount",
            "native_shape_overflow",
            "Batch geometry exceeds native tensor representation",
        ));
    }
    let cfg_scale = value.cfg_scale.unwrap_or(defaults.cfg_scale);
    cfg(cfg_scale)?;
    Ok(EffectiveImageParameters {
        prompt,
        negative_prompt,
        width,
        height,
        aspect_ratio,
        seed: seed(value.seed, defaults.seed),
        steps,
        cfg_scale,
        sampler: select_option("sampler", &defaults.samplers, value.sampler)?,
        scheduler: select_option("scheduler", &defaults.schedulers, value.scheduler)?,
        batch_count,
    })
}

fn video(
    profile: GenerationProfile,
    session_id: &str,
    value: DiffusionVideoParameters,
) -> GenerationParameterResult<EffectiveVideoParameters> {
    let prompt = value.prompt.unwrap_or_default();
    let defaults = profile.defaults();
    let negative_prompt = value
        .negative_prompt
        .unwrap_or_else(|| defaults.negative_prompt.clone());
    text("prompt", &prompt, true)?;
    text("negativePrompt", &negative_prompt, false)?;
    let (width, height) = (
        value.width.unwrap_or(defaults.width),
        value.height.unwrap_or(defaults.height),
    );
    let aspect_ratio = geometry(
        width,
        height,
        defaults.dimension_multiple,
        value.aspect_ratio,
    )?;
    let steps = value.steps.unwrap_or(defaults.steps);
    integer("steps", steps, 1, 1, 0)?;
    let cfg_scale = value.cfg_scale.unwrap_or(defaults.cfg_scale);
    cfg(cfg_scale)?;
    let timing = defaults.video.as_ref().expect("video timing");
    let fps = value.fps.unwrap_or(timing.fps);
    if fps != timing.fps && !timing.supported_fps.contains(&fps) {
        return Err(issue("fps", "unsupported_by_model", &timing.fps_constraint));
    }
    let duration_frames = if let Some(seconds) = value.duration_seconds {
        let frames = seconds * f64::from(fps);
        if !seconds.is_finite()
            || frames < f64::from(timing.minimum_frames)
            || frames > f64::from(i32::MAX)
            || (frames - frames.round()).abs() > 1e-9
        {
            return Err(issue(
                "durationSeconds",
                "non_integral_frames",
                "Duration must map exactly to an integral frame count",
            ));
        }
        Some(frames.round() as u32)
    } else {
        None
    };
    let frames = value.frames.or(duration_frames).unwrap_or(timing.frames);
    if duration_frames.is_some_and(|duration| duration != frames) {
        return Err(issue(
            "durationSeconds,frames,fps",
            "conflicting_duration",
            "Duration, frames and FPS disagree",
        ));
    }
    integer(
        "frames",
        frames,
        timing.minimum_frames,
        timing.frame_step,
        timing.minimum_frames % timing.frame_step,
    )?;
    if u64::from(width) * u64::from(height) * u64::from(frames) > i64::MAX as u64 / 24 {
        return Err(issue(
            "frames",
            "native_shape_overflow",
            "Video geometry exceeds native tensor representation",
        ));
    }
    let sampler = select_option("sampler", &defaults.samplers, value.sampler)?;
    match &value.first_frame {
        Some(input) if profile.requires_first_frame() => {
            canonical_result_id(&input.result_id)?;
            if input.session_id != session_id {
                return Err(issue(
                    "firstFrame",
                    "result_unavailable",
                    "First frame must belong to the same session",
                ));
            }
            if !matches!(input.mime_type.as_str(), "image/png" | "image/jpeg")
                || input.size_bytes == 0
                || input.size_bytes > 32 * 1024 * 1024
                || input.width == 0
                || input.height == 0
                || u64::from(input.width) * u64::from(input.height) > 32 * 1024 * 1024
            {
                return Err(issue("firstFrame", "invalid_media", "Expected bounded PNG/JPEG metadata; owner must verify actual bytes before execution"));
            }
        }
        None if !profile.requires_first_frame() => {}
        _ => {
            return Err(issue(
                "firstFrame",
                "wrong_task_variant",
                "First-frame input must match the selected task variant",
            ))
        }
    }
    let audio_mode = value.audio_mode.unwrap_or(timing.audio_mode);
    if audio_mode != timing.audio_mode {
        return Err(issue(
            "audioMode",
            "unsupported_by_profile",
            "The installed model cannot produce this audio mode",
        ));
    }
    Ok(EffectiveVideoParameters {
        prompt,
        negative_prompt,
        first_frame: value.first_frame,
        width,
        height,
        aspect_ratio,
        seed: seed(value.seed, defaults.seed),
        steps,
        cfg_scale,
        sampler,
        scheduler: select_option("scheduler", &defaults.schedulers, value.scheduler)?,
        frames,
        fps,
        duration: GenerationDuration {
            numerator: frames,
            denominator: fps,
        },
        audio_mode,
    })
}

fn observation(
    profile: GenerationProfile,
    parameters: &EffectiveGenerationParameters,
) -> GenerationObservation {
    let mut normalized = parameters.clone();
    if let EffectiveGenerationParameters::Video(video) = &mut normalized {
        if let Some(reference) = &mut video.first_frame {
            reference.session_id.clear();
            reference.result_id.clear();
        }
    }
    let matches_observed_request = profile.observed_parameters() == Some(&normalized);
    GenerationObservation {
        evidence: profile.evidence().into(),
        matches_observed_request,
        quality_acceptance_complete: false,
    }
}

fn arguments(snapshot: &GenerationParameterSnapshot) -> Vec<NativeGenerationArgument> {
    use NativeGenerationArgument::{Component, InputResult, Literal, Output};
    let mut args = Vec::new();
    let mut pair = |flag: &str, value: String| {
        args.push(Literal(flag.into()));
        args.push(Literal(value));
    };
    pair(
        "-M",
        if snapshot.profile.is_image() {
            "img_gen"
        } else {
            "vid_gen"
        }
        .into(),
    );
    let (prompt, negative, width, height, seed, steps, cfg, sampler, scheduler) =
        match &snapshot.parameters {
            EffectiveGenerationParameters::Image(value) => {
                pair("--batch-count", value.batch_count.to_string());
                (
                    &value.prompt,
                    &value.negative_prompt,
                    value.width,
                    value.height,
                    value.seed,
                    value.steps,
                    value.cfg_scale,
                    value.sampler,
                    value.scheduler,
                )
            }
            EffectiveGenerationParameters::Video(value) => {
                pair("--fps", value.fps.to_string());
                pair("--video-frames", value.frames.to_string());
                (
                    &value.prompt,
                    &value.negative_prompt,
                    value.width,
                    value.height,
                    value.seed,
                    value.steps,
                    value.cfg_scale,
                    value.sampler,
                    value.scheduler,
                )
            }
        };
    for (flag, value) in [
        ("-p", prompt.clone()),
        ("-n", negative.clone()),
        ("-W", width.to_string()),
        ("-H", height.to_string()),
        ("-s", seed.to_string()),
        ("--steps", steps.to_string()),
        ("--cfg-scale", cfg.to_string()),
        ("--sampling-method", sampler.native_name().into()),
        ("--scheduler", scheduler.native_name().into()),
        ("--threads", snapshot.engine.cpu_threads.to_string()),
        ("--backend", snapshot.engine.backend.clone()),
        (
            "--params-backend",
            snapshot.engine.parameters_backend.clone(),
        ),
        ("--rng", snapshot.engine.rng.clone()),
        ("--sampler-rng", snapshot.engine.sampler_rng.clone()),
        ("--flash-attn", snapshot.engine.flash_attention.clone()),
    ] {
        pair(flag, value);
    }
    // sd-cli disables caching by default. Its flag accepts only enabled algorithms.
    if snapshot.engine.cache_mode != "disabled" {
        pair("--cache-mode", snapshot.engine.cache_mode.clone());
    }
    if let Some(shift) = snapshot.engine.flow_shift {
        pair("--flow-shift", shift.to_string());
    }
    if let Some(gib) = snapshot.engine.max_vram_gib {
        pair("--max-vram", format!("cuda0={gib}"));
    }
    if snapshot.engine.disable_prefetch {
        args.push(Literal("--disable-prefetch".into()));
    }
    for asset in snapshot.profile.weight_sources() {
        args.extend([
            Literal(snapshot.profile.component_argument(asset.role).into()),
            Component(asset.role),
        ]);
    }
    if let EffectiveGenerationParameters::Video(value) = &snapshot.parameters {
        if let Some(input) = &value.first_frame {
            args.extend([
                Literal("--init-img".into()),
                InputResult(input.result_id.clone()),
            ]);
        }
    }
    args.extend([Literal("-o".into()), Output]);
    args
}

#[cfg(test)]
mod model_defaults_tests {
    use super::*;

    #[test]
    fn two_image_models_compile_different_geometry_defaults_and_samplers() {
        let first = GenerationProfile::Sd15CpuTextToImage.defaults();
        let second: inventory::Defaults = serde_json::from_value(json!({
            "width": 480, "height": 320, "dimensionMultiple": 32,
            "steps": 4, "cfgScale": 2.5, "negativePrompt": "", "seed": 17,
            "samplers": ["euler"], "schedulers": ["karras"]
        }))
        .unwrap();
        let prompt = DiffusionImageParameters {
            prompt: Some("a blue cup".into()),
            ..Default::default()
        };
        let a = image(first, prompt.clone()).unwrap();
        let b = image(&second, prompt.clone()).unwrap();
        assert_eq!(
            (a.width, a.height, a.steps, a.seed, a.sampler),
            (512, 512, 20, 42, GenerationSampler::EulerA)
        );
        assert_eq!(
            (b.width, b.height, b.steps, b.seed, b.sampler),
            (480, 320, 4, 17, GenerationSampler::Euler)
        );
        assert_eq!(b.cfg_scale, 2.5);
        assert_eq!(a.scheduler, GenerationScheduler::Discrete);
        assert_eq!(b.scheduler, GenerationScheduler::Karras);
        let mut custom_scheduler = prompt.clone();
        custom_scheduler.scheduler = Some(GenerationScheduler::Karras);
        assert!(image(first, custom_scheduler.clone()).is_err());
        assert!(image(&second, custom_scheduler).is_ok());
        let mut custom = prompt.clone();
        custom.width = Some(480);
        assert!(image(first, custom.clone()).is_err());
        assert!(image(&second, custom).is_ok());
        let mut exclusive = prompt;
        exclusive.sampler = Some(GenerationSampler::EulerA);
        assert!(image(first, exclusive.clone()).is_ok());
        assert!(image(&second, exclusive).is_err());
    }
}
