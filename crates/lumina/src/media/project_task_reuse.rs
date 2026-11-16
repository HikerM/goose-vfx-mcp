use anyhow::{bail, ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{Sqlite, SqliteConnection, Transaction};
use std::sync::Arc;

use super::task_configuration::ArchivedTaskConfiguration;
use super::{
    MediaKind, MediaResultRef, MediaResultStore, MediaSpec, NativeTaskTranscript,
    TaskConfigurationRecord, TaskConfigurationSnapshot,
};
use crate::project::{ProjectAssetRecord, ProjectAssetStore};
use crate::session::session_manager::SessionStorage;

const MAX_AUTHORIZATION_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProjectTaskReuseRequest {
    pub identity: TaskParameterIdentity,
    pub parent_asset_id: String,
    pub new_job_id: String,
    pub patch: Map<String, Value>,
    #[serde(default)]
    pub edit_scope: Option<ProjectEditScope>,
    #[serde(default)]
    pub edit_replacement_asset_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ProjectEditScope {
    ImageRect {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    TimeRange {
        #[serde(rename = "startMicros")]
        start_micros: u64,
        #[serde(rename = "durationMicros")]
        duration_micros: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "reference",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum ProjectInputReference {
    Image(GenerationMediaInput),
    Audio(AudioTaskMediaReference),
}

impl ProjectInputReference {
    fn native(&self) -> NativeTaskInputReference {
        match self {
            Self::Image(value) => NativeTaskInputReference::Image(value.clone()),
            Self::Audio(value) => NativeTaskInputReference::Audio(value.clone()),
        }
    }
}

impl From<NativeTaskInputReference> for ProjectInputReference {
    fn from(value: NativeTaskInputReference) -> Self {
        match value {
            NativeTaskInputReference::Image(value) => Self::Image(value),
            NativeTaskInputReference::Audio(value) => Self::Audio(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProjectCompilationBinding {
    child_session_id: String,
    child_job_id: String,
    parent_session_id: String,
    parent_job_id: String,
    parent_snapshot_digest: Sha256Digest,
    allowed_inputs: Vec<ProjectInputReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload_tier: Option<ProjectReusePayloadTier>,
}

impl ProjectCompilationBinding {
    pub(super) fn payload_tier(&self) -> Option<ProjectReusePayloadTier> {
        self.payload_tier
    }

    pub(super) fn domain_scope(&self) -> Result<ProjectTaskCompilationScope> {
        ensure!(
            self.allowed_inputs.len() <= 16,
            "Too many project task input bindings"
        );
        Ok(ProjectTaskCompilationScope::new(
            self.child_session_id.clone(),
            self.child_job_id.clone(),
            self.parent_session_id.clone(),
            self.parent_job_id.clone(),
            self.parent_snapshot_digest.clone(),
            self.allowed_inputs
                .iter()
                .map(ProjectInputReference::native)
                .collect(),
        )?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum ParentOutput {
    Media {
        result: MediaResultRef,
    },
    Transcript {
        session_id: String,
        job_id: String,
        configuration_sha256: String,
        bytes: u64,
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplacementAsset {
    asset_id: String,
    content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VerifiedProjectTaskAuthorization {
    project_id: String,
    parent_asset_id: String,
    parent_asset_content_sha256: String,
    parent_configuration: ArchivedTaskConfiguration,
    parent_output: ParentOutput,
    compilation: ProjectCompilationBinding,
    child_snapshot_digest: Sha256Digest,
    final_inputs: Vec<ProjectInputReference>,
    replacements: Vec<ReplacementAsset>,
    #[serde(default)]
    edit_replacement: Option<ReplacementAsset>,
    patch: Map<String, Value>,
    #[serde(default)]
    edit_scope: Option<ProjectEditScope>,
}

struct ParentTask {
    archive: ArchivedTaskConfiguration,
    record: TaskConfigurationRecord,
    output: ParentOutput,
}

#[derive(Debug)]
pub(crate) struct PreparedProjectTask {
    plan: NativeTaskPlan,
    authorization: VerifiedProjectTaskAuthorization,
    local_edit: Option<PreparedLocalEdit>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedLocalEdit {
    parent: MediaResultRef,
    replacement: MediaResultRef,
    scope: ProjectEditScope,
}

impl PreparedProjectTask {
    pub(crate) fn plan(&self) -> &NativeTaskPlan {
        &self.plan
    }
    pub(crate) fn authorization(&self) -> &VerifiedProjectTaskAuthorization {
        &self.authorization
    }

    pub(crate) fn local_edit(&self) -> Option<&PreparedLocalEdit> {
        self.local_edit.as_ref()
    }

    pub(super) fn payload_tier(&self) -> Result<ProjectReusePayloadTier> {
        self.authorization
            .payload_tier()
            .context("Project task has no prepared payload tier")
    }
}

impl PreparedLocalEdit {
    pub(crate) fn parent(&self) -> &MediaResultRef {
        &self.parent
    }

    pub(crate) fn replacement(&self) -> &MediaResultRef {
        &self.replacement
    }

    pub(crate) fn scope(&self) -> &ProjectEditScope {
        &self.scope
    }
}

impl VerifiedProjectTaskAuthorization {
    pub(super) fn payload_tier(&self) -> Option<ProjectReusePayloadTier> {
        self.compilation.payload_tier()
    }

    pub(crate) fn project_id(&self) -> &str {
        &self.project_id
    }
    pub(crate) fn child_session_id(&self) -> &str {
        &self.compilation.child_session_id
    }
    pub(crate) fn child_job_id(&self) -> &str {
        &self.compilation.child_job_id
    }
    pub(crate) fn child_snapshot_digest(&self) -> &Sha256Digest {
        &self.child_snapshot_digest
    }
    pub(crate) fn input_references(&self) -> Vec<NativeTaskInputReference> {
        self.final_inputs
            .iter()
            .map(ProjectInputReference::native)
            .collect()
    }
    pub(super) fn compilation(&self) -> &ProjectCompilationBinding {
        &self.compilation
    }

    pub(super) fn edit_scope(&self) -> Option<&ProjectEditScope> {
        self.edit_scope.as_ref()
    }

    pub(super) fn encode(&self) -> Result<String> {
        super::project_payload::canonical_json(
            self,
            self.payload_tier().map_or(
                MAX_AUTHORIZATION_BYTES,
                ProjectReusePayloadTier::record_limit,
            ),
        )
    }

    pub(super) async fn read_from(
        connection: &mut SqliteConnection,
        session: &str,
        job: &str,
    ) -> Result<Option<Self>> {
        use sqlx::Row;
        let row = sqlx::query("SELECT CASE WHEN length(CAST(payload AS BLOB))<=1048576 THEN payload END AS payload,sha256 FROM project_task_authorizations_v1 WHERE session_id=? AND job_id=?")
            .bind(session).bind(job).fetch_optional(connection).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let payload: String = row
            .try_get::<Option<String>, _>("payload")?
            .context("Stored project authorization exceeds its byte limit")?;
        let authorization: Self = serde_json::from_str(&payload)?;
        ensure!(
            authorization.encode()? == payload
                && Sha256Digest::of_bytes(payload.as_bytes()).as_str()
                    == row.try_get::<String, _>("sha256")?
                && authorization.child_session_id() == session
                && authorization.child_job_id() == job,
            "Stored project task authorization differs from its identity or digest"
        );
        Ok(Some(authorization))
    }

    pub(super) fn validate_child(&self, child: &TaskConfigurationRecord) -> Result<()> {
        if let Some(tier) = self.payload_tier() {
            super::project_payload::serialized_len(self, tier.record_limit())?;
            super::project_payload::serialized_len(
                &ArchivedTaskConfiguration::from_record(child)?,
                tier.record_limit(),
            )?;
            ensure!(
                !matches!(child.snapshot(), TaskConfigurationSnapshot::Qwen3Asr(_))
                    || tier == ProjectReusePayloadTier::Standard,
                "Transcript publication requires the standard payload tier"
            );
        }
        ensure!(
            child.session_id() == self.child_session_id()
                && child.job_id() == self.child_job_id()
                && child.generation_config_sha256() == self.child_snapshot_digest.as_str()
                && child.project_scope() == Some(&self.compilation),
            "Project authorization belongs to another child configuration"
        );
        let parent = self.parent_configuration.validate(
            &self.compilation.parent_session_id,
            &self.compilation.parent_job_id,
        )?;
        ensure!(
            parent.identity() == child.identity()
                && parent.generation_config_sha256()
                    == self.compilation.parent_snapshot_digest.as_str(),
            "Project parent model or snapshot differs"
        );
        verify_parent_output(&parent, &self.parent_output)?;
        if let Some(scope) = self.edit_scope() {
            validate_edit_scope(&self.parent_output, scope)?;
        }
        let contract = contract_for_record(&parent)?;
        ensure!(
            self.patch.len() == 1,
            "Project reuse changes exactly one task parameter"
        );
        let field = self
            .patch
            .keys()
            .next()
            .context("Missing project parameter patch")?;
        validate_patch(&contract, &self.patch)?;
        let mut allowed = validation_inputs(&parent, &contract)?;
        if let Some(kind) = contract.session_media_kind(field)? {
            push_unique(&mut allowed, parse_input(kind, self.patch[field].clone())?);
        }
        ensure!(
            allowed == self.compilation.allowed_inputs,
            "Project compile inputs differ from the parent and replacement parameter"
        );
        let scope = self.compilation.domain_scope()?;
        let mut patch = self.patch.clone();
        let generated_seed = matches!(child.snapshot(), TaskConfigurationSnapshot::Generation(_))
            && patch
                .get("seed")
                .is_some_and(|value| value["kind"] == "new");
        if generated_seed {
            let TaskConfigurationSnapshot::Generation(snapshot) = child.snapshot() else {
                unreachable!()
            };
            let seed = match &snapshot.parameters {
                EffectiveGenerationParameters::Image(p) => p.seed,
                EffectiveGenerationParameters::Video(p) => p.seed,
            };
            let TaskConfigurationSnapshot::Generation(parent_snapshot) = parent.snapshot() else {
                unreachable!()
            };
            let parent_seed = match &parent_snapshot.parameters {
                EffectiveGenerationParameters::Image(p) => p.seed,
                EffectiveGenerationParameters::Video(p) => p.seed,
            };
            ensure!(
                seed != parent_seed,
                "New project seed must differ from its parent"
            );
            patch.insert("seed".into(), json!({"kind":"value","value":seed}));
        }
        let expected = reuse_plan(
            &parent,
            &self.parent_output,
            contract,
            self.child_session_id(),
            self.child_job_id(),
            patch,
            &scope,
        )?;
        if generated_seed {
            let (
                NativeTaskPlan::Generation(expected),
                TaskConfigurationSnapshot::Generation(actual),
            ) = (&expected, child.snapshot())
            else {
                unreachable!()
            };
            let mut snapshot = expected.snapshot().clone();
            snapshot
                .origins
                .insert("seed".into(), GenerationParameterOrigin::GeneratedSeed);
            ensure!(
                snapshot == **actual,
                "Project child differs from the authenticated parent parameter change"
            );
        } else {
            ensure!(
                expected.snapshot_digest()? == self.child_snapshot_digest,
                "Project child differs from the authenticated parent parameter change"
            );
        }
        ensure!(
            expected.input_references() == self.input_references(),
            "Project execution inputs differ from the compiled task"
        );
        Ok(())
    }

    pub(super) async fn validate_in_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let tier = self
            .payload_tier()
            .unwrap_or(ProjectReusePayloadTier::Standard);
        let (parent, mut source_bytes) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
            tx,
            self.child_session_id(),
            self.child_job_id(),
            &self.parent_asset_id,
            tier.source_assets_limit(),
        )
        .await?
        .context("Parent asset is unavailable in the task project")?;
        ensure!(
            parent.project_id == self.project_id()
                && parent.content_sha256 == self.parent_asset_content_sha256,
            "Project parent asset changed"
        );
        let (configuration, output) = parent_configuration(&parent)?;
        ensure!(
            configuration == self.parent_configuration && output == self.parent_output,
            "Project parent archive differs from the prepared task"
        );
        let parent_record = configuration.validate(&parent.source_session_id, &parent.job_id)?;
        let contract = contract_for_record(&parent_record)?;
        let (field, value) = self.patch.iter().next().context("Missing project patch")?;
        let replacement_kind = contract.session_media_kind(field)?;
        ensure!(
            self.replacements.len() == usize::from(replacement_kind.is_some()),
            "Replacement asset binding count differs from the parameter"
        );
        for replacement in &self.replacements {
            let remaining = tier
                .source_assets_limit()
                .checked_sub(source_bytes)
                .context("Project source assets exceed the payload tier")?;
            let (asset, bytes) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
                tx,
                self.child_session_id(),
                self.child_job_id(),
                &replacement.asset_id,
                remaining,
            )
            .await?
            .context("Replacement asset is unavailable in the task project")?;
            source_bytes = source_bytes
                .checked_add(bytes)
                .context("Project source size overflow")?;
            ensure!(
                asset.content_sha256 == replacement.content_sha256,
                "Replacement project asset changed"
            );
            let input = input_from_result(
                &media_asset(&asset)?,
                replacement_kind.context("Unexpected replacement asset")?,
            )?;
            ensure!(
                input_value(&input)? == *value,
                "Replacement project input differs from the resolved asset"
            );
        }
        if let Some(replacement) = &self.edit_replacement {
            let remaining = tier
                .source_assets_limit()
                .checked_sub(source_bytes)
                .context("Project source assets exceed the payload tier")?;
            let (asset, _) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
                tx,
                self.child_session_id(),
                self.child_job_id(),
                &replacement.asset_id,
                remaining,
            )
            .await?
            .context("Local edit replacement asset is unavailable in this project")?;
            ensure!(
                asset.content_sha256 == replacement.content_sha256,
                "Local edit replacement project asset changed"
            );
            let result = media_asset(&asset)?;
            let scope = self
                .edit_scope
                .as_ref()
                .context("Local edit replacement requires an authenticated edit scope")?;
            let expected_kind = match scope {
                ProjectEditScope::ImageRect { .. } => SessionMediaParameterKind::Image,
                ProjectEditScope::TimeRange { .. } => SessionMediaParameterKind::Audio,
            };
            input_from_result(&result, expected_kind)?;
            if matches!(scope, ProjectEditScope::TimeRange { .. }) {
                ensure!(
                    result.mime == "audio/wav",
                    "Audio time range editing currently requires audio/wav"
                );
            }
        }
        Ok(())
    }
}

pub(super) async fn project_lineage(
    tx: &mut Transaction<'_, Sqlite>,
    record: &TaskConfigurationRecord,
) -> Result<Option<VerifiedProjectTaskAuthorization>> {
    let authorization =
        VerifiedProjectTaskAuthorization::read_from(tx, record.session_id(), record.job_id())
            .await?;
    ensure!(
        authorization
            .as_ref()
            .map(VerifiedProjectTaskAuthorization::compilation)
            == record.project_scope(),
        "Archived project scope has no matching durable authorization"
    );
    if let Some(value) = &authorization {
        value.validate_child(record)?;
    }
    Ok(authorization)
}

pub(crate) async fn authorize_project_inputs(
    storage: &SessionStorage,
    session: &str,
    plan: &NativeTaskPlan,
    authorization: &VerifiedProjectTaskAuthorization,
) -> Result<()> {
    ensure!(
        session == plan.session_id()
            && session == authorization.child_session_id()
            && plan.job_id() == authorization.child_job_id()
            && plan.snapshot_digest()? == *authorization.child_snapshot_digest()
            && plan.input_references() == authorization.input_references(),
        "Project input authorization differs from the task"
    );
    let pool = storage.pool().await?;
    let mut tx = pool.begin().await?;
    authorization.validate_in_tx(&mut tx).await?;
    tx.commit().await?;
    verify_final_inputs(
        &MediaResultStore::new(pool.clone()),
        &authorization.input_references(),
    )
    .await
}

pub(crate) async fn prepare_project_reused_task(
    installation: &InstallationService,
    storage: &SessionStorage,
    session: &str,
    request: ProjectTaskReuseRequest,
) -> Result<PreparedProjectTask> {
    super::domain::validate_id(session)?;
    super::domain::validate_id(&request.new_job_id)?;
    ensure!(
        request.patch.len() == 1,
        "Provide exactly one task parameter"
    );
    super::project_payload::serialized_len(&request.patch, MAX_AUTHORIZATION_BYTES)?;
    let pool = storage.pool().await?;
    ProjectAssetStore::new(pool.clone())
        .freeze_job(session, &request.new_job_id, None)
        .await?;
    let mut tx = pool.begin().await?;
    let (asset, _) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
        &mut tx,
        session,
        &request.new_job_id,
        &request.parent_asset_id,
        ProjectReusePayloadTier::Standard.source_assets_limit(),
    )
    .await?
    .context("Parent asset is unavailable in this project")?;
    tx.commit().await?;
    let (archive, output) = parent_configuration(&asset)?;
    let parent = archive.validate(&asset.source_session_id, &asset.job_id)?;
    if let Some(scope) = request.edit_scope.as_ref() {
        validate_edit_scope(&output, scope)?;
    }
    ensure!(
        parent.identity() == &request.identity,
        "Parent asset does not match the selected model identity"
    );
    ensure!(
        request.new_job_id != parent.job_id(),
        "Project reuse requires a new job"
    );
    installation.wait_ready().await?;
    let repository = Arc::clone(installation.repository());
    let identity = request.identity.clone();
    let registered = tokio::task::spawn_blocking(move || {
        NativeTaskCatalog::for_version(
            &repository,
            &identity.installation_id,
            &identity.version_id,
        )?
        .describe(&identity)
    })
    .await??;
    prepare_registered(
        storage,
        session,
        request,
        asset,
        ParentTask {
            archive,
            record: parent,
            output,
        },
        registered.descriptor,
    )
    .await
}

async fn prepare_registered(
    storage: &SessionStorage,
    session: &str,
    mut request: ProjectTaskReuseRequest,
    asset: ProjectAssetRecord,
    parent: ParentTask,
    contract: NativeTaskParameterContract,
) -> Result<PreparedProjectTask> {
    let ParentTask {
        archive,
        record: parent,
        output,
    } = parent;
    ensure!(
        request.patch.len() == 1 && parent.identity() == &request.identity,
        "Invalid project reuse identity or patch"
    );
    super::project_payload::serialized_len(&request.patch, MAX_AUTHORIZATION_BYTES)?;
    let mut source_bytes = super::project_payload::serialized_len(
        &asset.content,
        ProjectReusePayloadTier::Standard.source_assets_limit(),
    )?;
    validate_patch(&contract, &request.patch)?;
    let pool = storage.pool().await?;
    let mut allowed = validation_inputs(&parent, &contract)?;
    let mut replacements = Vec::new();
    let mut edit_replacement_result = None;
    for (field, value) in &mut request.patch {
        let Some(kind) = contract.session_media_kind(field)? else {
            continue;
        };
        let id = value
            .as_str()
            .context("Replacement media must be a project asset ID")?;
        let mut tx = pool.begin().await?;
        let remaining = ProjectReusePayloadTier::Standard
            .source_assets_limit()
            .checked_sub(source_bytes)
            .context("Project source size overflow")?;
        let (replacement, bytes) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
            &mut tx,
            session,
            &request.new_job_id,
            id,
            remaining,
        )
        .await?
        .context("Replacement asset is unavailable in this project")?;
        source_bytes = source_bytes
            .checked_add(bytes)
            .context("Project source size overflow")?;
        tx.commit().await?;
        let result = media_asset(&replacement)?;
        let reference = input_from_result(&result, kind)?;
        *value = input_value(&reference)?;
        push_unique(&mut allowed, reference);
        replacements.push(ReplacementAsset {
            asset_id: replacement.asset_id,
            content_sha256: replacement.content_sha256,
        });
    }
    let edit_replacement = if let Some(asset_id) = request.edit_replacement_asset_id.as_ref() {
        ensure!(
            request.edit_scope.is_some(),
            "Local edit replacement requires an edit scope"
        );
        let mut tx = pool.begin().await?;
        let remaining = ProjectReusePayloadTier::Standard
            .source_assets_limit()
            .checked_sub(source_bytes)
            .context("Project source size overflow")?;
        let (replacement, bytes) = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
            &mut tx,
            session,
            &request.new_job_id,
            asset_id,
            remaining,
        )
        .await?
        .context("Local edit replacement asset is unavailable in this project")?;
        source_bytes = source_bytes
            .checked_add(bytes)
            .context("Project source size overflow")?;
        tx.commit().await?;
        let result = media_asset(&replacement)?;
        let edit_scope = request
            .edit_scope
            .as_ref()
            .context("Local edit replacement requires an edit scope")?;
        let expected_kind = match edit_scope {
            ProjectEditScope::ImageRect { .. } => SessionMediaParameterKind::Image,
            ProjectEditScope::TimeRange { .. } => SessionMediaParameterKind::Audio,
        };
        input_from_result(&result, expected_kind)?;
        if matches!(edit_scope, ProjectEditScope::TimeRange { .. }) {
            ensure!(
                result.mime == "audio/wav",
                "Audio time range editing currently requires audio/wav"
            );
        }
        edit_replacement_result = Some(result);
        Some(ReplacementAsset {
            asset_id: replacement.asset_id,
            content_sha256: replacement.content_sha256,
        })
    } else {
        None
    };
    let compilation = ProjectCompilationBinding {
        child_session_id: session.into(),
        child_job_id: request.new_job_id.clone(),
        parent_session_id: parent.session_id().into(),
        parent_job_id: parent.job_id().into(),
        parent_snapshot_digest: Sha256Digest::parse(parent.generation_config_sha256())?,
        allowed_inputs: allowed,
        payload_tier: Some(ProjectReusePayloadTier::Standard),
    };
    let scope = compilation.domain_scope()?;
    let plan = reuse_plan(
        &parent,
        &output,
        contract,
        session,
        &request.new_job_id,
        request.patch.clone(),
        &scope,
    )?;
    let local_edit = match request.edit_scope.clone() {
        None => None,
        Some(edit_scope) => {
            let ParentOutput::Media {
                result: parent_result,
            } = &output
            else {
                bail!("Local media editing requires an archived media result")
            };
            validate_edit_scope(&output, &edit_scope)?;
            let replacement = edit_replacement_result
                .clone()
                .context("Local editing requires editReplacementAssetId")?;
            match edit_scope {
                ProjectEditScope::ImageRect { .. } => {
                    ensure!(
                        parent_result.kind == MediaKind::Image
                            && replacement.kind == MediaKind::Image,
                        "Image rectangle editing requires image parent and replacement assets"
                    );
                }
                ProjectEditScope::TimeRange { .. } => {
                    ensure!(
                        parent_result.kind == MediaKind::Audio
                            && replacement.kind == MediaKind::Audio
                            && parent_result.mime == "audio/wav"
                            && replacement.mime == "audio/wav",
                        "Audio time range editing currently requires audio/wav parent and replacement assets"
                    );
                }
            }
            Some(PreparedLocalEdit {
                parent: parent_result.clone(),
                replacement,
                scope: edit_scope,
            })
        }
    };
    let mut authorization = VerifiedProjectTaskAuthorization {
        project_id: asset.project_id,
        parent_asset_id: asset.asset_id,
        parent_asset_content_sha256: asset.content_sha256,
        parent_configuration: archive,
        parent_output: output,
        compilation,
        child_snapshot_digest: plan.snapshot_digest()?,
        final_inputs: plan
            .input_references()
            .into_iter()
            .map(Into::into)
            .collect(),
        replacements,
        edit_replacement,
        patch: request.patch,
        edit_scope: request.edit_scope,
    };
    let record = TaskConfigurationRecord::from_project_plan(&plan, &authorization.compilation)?;
    let archive = ArchivedTaskConfiguration::from_record(&record)?;
    let archive_bytes = super::project_payload::serialized_len(&archive, MAX_AUTHORIZATION_BYTES)?;
    let authorization_bytes =
        super::project_payload::serialized_len(&authorization, MAX_AUTHORIZATION_BYTES)?;
    let small = ProjectReusePayloadTier::Small;
    if source_bytes <= small.source_assets_limit()
        && archive_bytes <= small.record_limit()
        && authorization_bytes <= small.record_limit()
        && !matches!(plan, NativeTaskPlan::Qwen3Asr(_))
    {
        authorization.compilation.payload_tier = Some(small);
    }
    authorization.encode()?;
    authorize_project_inputs(storage, session, &plan, &authorization).await?;
    Ok(PreparedProjectTask {
        plan,
        authorization,
        local_edit,
    })
}

fn parent_configuration(
    asset: &ProjectAssetRecord,
) -> Result<(ArchivedTaskConfiguration, ParentOutput)> {
    super::project_payload::serialized_len(
        &asset.content["configuration"],
        MAX_AUTHORIZATION_BYTES,
    )?;
    let archive: ArchivedTaskConfiguration =
        serde_json::from_value(asset.content["configuration"].clone())
            .context("Parent project asset has no valid local task configuration")?;
    let parent = archive.validate(&asset.source_session_id, &asset.job_id)?;
    if parent.project_scope().is_some() {
        super::project_payload::serialized_len(
            &asset.content["projectLineage"],
            MAX_AUTHORIZATION_BYTES,
        )?;
        let lineage: VerifiedProjectTaskAuthorization =
            serde_json::from_value(asset.content["projectLineage"].clone())
                .context("Project parent has no authenticated lineage")?;
        lineage.validate_child(&parent)?;
    } else {
        ensure!(
            asset
                .content
                .get("projectLineage")
                .is_none_or(Value::is_null),
            "Ordinary parent has unexpected project lineage"
        );
    }
    let output = match asset.content["kind"].as_str() {
        Some("media") => ParentOutput::Media {
            result: media_asset(asset)?,
        },
        Some("transcript") => {
            let text = asset.content["transcript"]["text"]
                .as_str()
                .context("Parent transcript has no text")?;
            ensure!(
                text.len() as u64 <= super::MAX_TASK_TRANSCRIPT_BYTES,
                "Parent transcript exceeds its byte limit"
            );
            let value: NativeTaskTranscript =
                serde_json::from_value(asset.content["transcript"].clone())?;
            ensure!(
                value.bytes == value.text.len() as u64
                    && value.bytes <= super::MAX_TASK_TRANSCRIPT_BYTES
                    && Sha256Digest::of_bytes(value.text.as_bytes()).as_str() == value.sha256,
                "Parent transcript bytes differ from its digest"
            );
            ParentOutput::Transcript {
                session_id: value.session_id,
                job_id: value.job_id,
                configuration_sha256: value.generation_config_sha256,
                bytes: value.bytes,
                sha256: value.sha256,
            }
        }
        _ => bail!("Unsupported project task parent output"),
    };
    verify_parent_output(&parent, &output)?;
    Ok((archive, output))
}

fn verify_parent_output(parent: &TaskConfigurationRecord, output: &ParentOutput) -> Result<()> {
    match output {
        ParentOutput::Media { result } => {
            ensure!(
                result.session_id == parent.session_id() && result.job_id == parent.job_id(),
                "Parent media task identity differs"
            );
            parent.verify_source(&result.session_id, &result.job_id, &result.source)?;
            let expected_kind = match parent.snapshot() {
                TaskConfigurationSnapshot::Generation(snapshot) => match &snapshot.parameters {
                    EffectiveGenerationParameters::Image(_) => MediaKind::Image,
                    EffectiveGenerationParameters::Video(_) => MediaKind::Video,
                },
                TaskConfigurationSnapshot::PocketTts(_)
                | TaskConfigurationSnapshot::StableAudioMusic(_) => MediaKind::Audio,
                TaskConfigurationSnapshot::Qwen3Asr(_) => bail!("ASR parent must be a transcript"),
            };
            ensure!(
                result.kind == expected_kind,
                "Parent output kind differs from its model task"
            );
        }
        ParentOutput::Transcript {
            session_id,
            job_id,
            configuration_sha256,
            ..
        } => {
            ensure!(
                matches!(parent.snapshot(), TaskConfigurationSnapshot::Qwen3Asr(_))
                    && parent.session_id() == session_id
                    && parent.job_id() == job_id
                    && parent.generation_config_sha256() == configuration_sha256,
                "Parent transcript configuration differs"
            );
        }
    }
    Ok(())
}

fn media_asset(asset: &ProjectAssetRecord) -> Result<MediaResultRef> {
    ensure!(
        asset.content["kind"] == "media",
        "Project asset is not media"
    );
    super::project_payload::serialized_len(&asset.content["result"], MAX_AUTHORIZATION_BYTES)?;
    let result: MediaResultRef = serde_json::from_value(asset.content["result"].clone())?;
    ensure!(
        result.result_id == asset.asset_id
            && result.session_id == asset.source_session_id
            && result.job_id == asset.job_id,
        "Project media identity differs from its archive"
    );
    Ok(result)
}

fn input_from_result(
    result: &MediaResultRef,
    kind: SessionMediaParameterKind,
) -> Result<ProjectInputReference> {
    let sha256 = Sha256Digest::parse(&result.sha256)?;
    Ok(match (kind, result.kind, &result.spec) {
        (
            SessionMediaParameterKind::Image,
            MediaKind::Image,
            MediaSpec::Image { width, height },
        ) => ProjectInputReference::Image(GenerationMediaInput {
            session_id: result.session_id.clone(),
            result_id: result.result_id.clone(),
            sha256,
            mime_type: result.mime.clone(),
            size_bytes: result.bytes,
            width: *width,
            height: *height,
        }),
        (
            SessionMediaParameterKind::Audio,
            MediaKind::Audio,
            MediaSpec::Audio {
                sample_rate,
                channels,
                sample_frames,
                ..
            },
        ) => ProjectInputReference::Audio(AudioTaskMediaReference {
            session_id: result.session_id.clone(),
            result_id: result.result_id.clone(),
            sha256,
            mime_type: result.mime.clone(),
            size_bytes: result.bytes,
            sample_rate: *sample_rate,
            channels: u32::from(*channels),
            sample_frames: *sample_frames,
        }),
        _ => bail!("Project asset media kind differs from the installed parameter contract"),
    })
}

fn validate_patch(
    contract: &NativeTaskParameterContract,
    patch: &Map<String, Value>,
) -> Result<()> {
    ensure!(
        patch.len() == 1,
        "Project reuse changes exactly one parameter"
    );
    let (field, value) = patch
        .iter()
        .next()
        .context("Missing project parameter patch")?;
    contract.session_media_kind(field)?;
    if !matches!(contract, NativeTaskParameterContract::StableAudioMusic(_)) {
        ensure!(!value.is_null(), "A task parameter patch cannot be null");
    }
    Ok(())
}

fn validate_edit_scope(output: &ParentOutput, scope: &ProjectEditScope) -> Result<()> {
    let ParentOutput::Media { result } = output else {
        bail!("Local media edit scopes require an archived media result")
    };
    match (scope, result.kind, &result.spec) {
        (
            ProjectEditScope::ImageRect {
                x,
                y,
                width,
                height,
            },
            MediaKind::Image,
            MediaSpec::Image {
                width: image_width,
                height: image_height,
            },
        ) => {
            ensure!(
                *width > 0 && *height > 0,
                "Image edit rectangle must be non-empty"
            );
            ensure!(
                x.checked_add(*width).is_some_and(|end| end <= *image_width)
                    && y.checked_add(*height)
                        .is_some_and(|end| end <= *image_height),
                "Image edit rectangle exceeds the archived media bounds"
            );
        }
        (
            ProjectEditScope::TimeRange {
                start_micros,
                duration_micros,
            },
            MediaKind::Audio | MediaKind::Video,
            MediaSpec::Audio {
                duration_micros: total,
                ..
            }
            | MediaSpec::Video {
                duration_micros: total,
                ..
            },
        ) => {
            ensure!(*duration_micros > 0, "Media edit duration must be positive");
            ensure!(
                *start_micros < *total
                    && start_micros
                        .checked_add(*duration_micros)
                        .is_some_and(|end| end <= *total),
                "Media edit time range exceeds the archived media duration"
            );
        }
        (ProjectEditScope::ImageRect { .. }, _, _) => {
            bail!("Image edit scope requires an archived image result")
        }
        (ProjectEditScope::TimeRange { .. }, _, _) => {
            bail!("Time range edit scope requires archived audio or video")
        }
    }
    Ok(())
}

fn input_value(reference: &ProjectInputReference) -> Result<Value> {
    Ok(match reference {
        ProjectInputReference::Image(v) => serde_json::to_value(v)?,
        ProjectInputReference::Audio(v) => serde_json::to_value(v)?,
    })
}

fn parse_input(kind: SessionMediaParameterKind, value: Value) -> Result<ProjectInputReference> {
    Ok(match kind {
        SessionMediaParameterKind::Image => {
            ProjectInputReference::Image(serde_json::from_value(value)?)
        }
        SessionMediaParameterKind::Audio => {
            ProjectInputReference::Audio(serde_json::from_value(value)?)
        }
    })
}

fn push_unique(values: &mut Vec<ProjectInputReference>, value: ProjectInputReference) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn validation_inputs(
    parent: &TaskConfigurationRecord,
    contract: &NativeTaskParameterContract,
) -> Result<Vec<ProjectInputReference>> {
    let mut values = Vec::new();
    let (parameters, changes): (Value, Vec<(String, Value)>) = match parent.snapshot() {
        TaskConfigurationSnapshot::Generation(s) => (
            serde_json::to_value(&s.parameters)?["parameters"].clone(),
            s.changes
                .iter()
                .map(|(field, change)| (field.clone(), change.before.clone()))
                .collect(),
        ),
        TaskConfigurationSnapshot::PocketTts(s) => (
            serde_json::to_value(&s.parameters)?,
            s.metadata
                .derivation
                .as_ref()
                .and_then(|d| d.change.as_ref())
                .map(|c| vec![(c.field.clone(), c.before.clone())])
                .unwrap_or_default(),
        ),
        TaskConfigurationSnapshot::Qwen3Asr(s) => (
            serde_json::to_value(&s.parameters)?,
            s.metadata
                .derivation
                .as_ref()
                .and_then(|d| d.change.as_ref())
                .map(|c| vec![(c.field.clone(), c.before.clone())])
                .unwrap_or_default(),
        ),
        TaskConfigurationSnapshot::StableAudioMusic(s) => (
            serde_json::to_value(&s.parameters)?,
            s.metadata
                .derivation
                .as_ref()
                .and_then(|d| d.change.as_ref())
                .map(|c| vec![(c.field.clone(), c.before.clone())])
                .unwrap_or_default(),
        ),
    };
    for (field, value) in parameters
        .as_object()
        .context("Invalid parent parameters")?
    {
        // Effective parameters may contain derived fields absent from the writable descriptor.
        if let Ok(Some(kind)) = contract.session_media_kind(field) {
            if !value.is_null() {
                push_unique(&mut values, parse_input(kind, value.clone())?);
            }
        }
    }
    for (field, before) in changes {
        if let Ok(Some(kind)) = contract.session_media_kind(&field) {
            if !before.is_null() {
                push_unique(&mut values, parse_input(kind, before)?);
            }
        }
    }
    Ok(values)
}

fn contract_for_record(record: &TaskConfigurationRecord) -> Result<NativeTaskParameterContract> {
    let profile = || {
        AudioTaskProfile::for_parameter_schema(&record.identity().parameter_schema_id)
            .context("Unknown audio parameter schema")
    };
    Ok(match record.snapshot() {
        TaskConfigurationSnapshot::Generation(s) => {
            NativeTaskParameterContract::Generation(Box::new(
                GenerationParameterService::new(s.profile, s.binding.clone())?
                    .descriptor()
                    .clone(),
            ))
        }
        TaskConfigurationSnapshot::PocketTts(s) => {
            NativeTaskParameterContract::PocketTts(Box::new(
                PocketTtsParameterService::for_profile(profile()?, s.metadata.binding.clone())?
                    .descriptor()
                    .clone(),
            ))
        }
        TaskConfigurationSnapshot::Qwen3Asr(s) => NativeTaskParameterContract::Qwen3Asr(Box::new(
            Qwen3AsrParameterService::for_profile(profile()?, s.metadata.binding.clone())?
                .descriptor()
                .clone(),
        )),
        TaskConfigurationSnapshot::StableAudioMusic(s) => {
            NativeTaskParameterContract::StableAudioMusic(Box::new(
                StableAudioMusicParameterService::for_profile(
                    profile()?,
                    s.metadata.binding.clone(),
                )?
                .descriptor()
                .clone(),
            ))
        }
    })
}

fn reuse_plan(
    parent: &TaskConfigurationRecord,
    output: &ParentOutput,
    contract: NativeTaskParameterContract,
    session: &str,
    job: &str,
    patch: Map<String, Value>,
    scope: &ProjectTaskCompilationScope,
) -> Result<NativeTaskPlan> {
    let digest = Sha256Digest::parse(parent.generation_config_sha256())?;
    let profile = || {
        AudioTaskProfile::for_parameter_schema(&parent.identity().parameter_schema_id)
            .context("Unknown audio parameter schema")
    };
    match (parent.snapshot(), contract) {
        (
            TaskConfigurationSnapshot::Generation(snapshot),
            NativeTaskParameterContract::Generation(desc),
        ) => {
            let ParentOutput::Media { result } = output else {
                bail!("Generation parent requires a media result")
            };
            let task = match &snapshot.parameters {
                EffectiveGenerationParameters::Image(_) => "image",
                EffectiveGenerationParameters::Video(_) => "video",
            };
            let record = GenerationResultRecord {
                snapshot: (**snapshot).clone(),
                snapshot_digest: digest,
                outputs: vec![GenerationOutputReference {
                    session_id: result.session_id.clone(),
                    result_id: result.result_id.clone(),
                    sha256: Sha256Digest::parse(&result.sha256)?,
                }],
            };
            Ok(GenerationParameterService::new(desc.profile, desc.binding)?
                .reuse_result_with_project_scope(
                    session,
                    &record,
                    &result.result_id,
                    job,
                    serde_json::from_value(json!({"task":task,"parameters":patch}))?,
                    scope,
                )?
                .into())
        }
        (
            TaskConfigurationSnapshot::PocketTts(snapshot),
            NativeTaskParameterContract::PocketTts(desc),
        ) => Ok(
            PocketTtsParameterService::for_profile(profile()?, desc.binding)?
                .reuse_configuration_with_project_scope(
                    session,
                    snapshot,
                    &digest,
                    job,
                    serde_json::from_value(Value::Object(patch))?,
                    scope,
                )?
                .into(),
        ),
        (
            TaskConfigurationSnapshot::Qwen3Asr(snapshot),
            NativeTaskParameterContract::Qwen3Asr(desc),
        ) => Ok(
            Qwen3AsrParameterService::for_profile(profile()?, desc.binding)?
                .reuse_configuration_with_project_scope(
                    session,
                    snapshot,
                    &digest,
                    job,
                    serde_json::from_value(Value::Object(patch))?,
                    scope,
                )?
                .into(),
        ),
        (
            TaskConfigurationSnapshot::StableAudioMusic(snapshot),
            NativeTaskParameterContract::StableAudioMusic(desc),
        ) => Ok(
            StableAudioMusicParameterService::for_profile(profile()?, desc.binding)?
                .reuse_configuration_with_project_scope(
                    session,
                    snapshot,
                    &digest,
                    job,
                    serde_json::from_value(Value::Object(patch))?,
                    scope,
                )?
                .into(),
        ),
        _ => bail!("Project parent and installed task adapter differ"),
    }
}

pub(super) async fn verify_final_inputs(
    media: &MediaResultStore,
    references: &[NativeTaskInputReference],
) -> Result<()> {
    for reference in references {
        let held = media
            .open_content(reference.session_id(), reference.result_id())
            .await?;
        let kind = match reference {
            NativeTaskInputReference::Image(_) => SessionMediaParameterKind::Image,
            NativeTaskInputReference::Audio(_) => SessionMediaParameterKind::Audio,
        };
        ensure!(
            input_from_result(held.result(), kind)?.native() == *reference,
            "Project input metadata or digest differs from the verified original media"
        );
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "project_task_reuse_tests.rs"]
mod tests;
