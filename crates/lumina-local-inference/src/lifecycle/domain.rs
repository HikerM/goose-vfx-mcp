use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

pub const MODEL_LIFECYCLE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelScopeRepository(String);

impl ModelScopeRepository {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let parts: Vec<_> = value.split('/').collect();
        ensure!(
            parts.len() == 2,
            "ModelScope repository must be owner/model"
        );
        ensure!(
            parts.iter().all(|part| !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || "-_.".contains(c))),
            "Invalid ModelScope repository ID"
        );
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelScopeRepository {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<ModelScopeRepository> for String {
    fn from(value: ModelScopeRepository) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        ensure!(
            value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid SHA256 digest"
        );
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self::from_sha256(Sha256::new().chain_update(bytes))
    }

    pub(crate) fn from_sha256(hasher: Sha256) -> Self {
        let mut hex = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(&mut hex, "{byte:02x}").expect("writing a digest to a String cannot fail");
        }
        Self(hex)
    }

    pub fn of_serializable(value: &impl Serialize) -> Result<Self> {
        Ok(Self::of_bytes(&serde_json::to_vec(value)?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<Sha256Digest> for String {
    fn from(value: Sha256Digest) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        ensure!(
            !value.is_empty()
                && !value.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
                && !value.chars().any(char::is_control)
                && value.split('/').all(portable_path_component),
            "Invalid repository file path"
        );
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn portable_path_component(part: &str) -> bool {
    if part.is_empty() || part.ends_with(['.', ' ']) {
        return false;
    }
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

impl TryFrom<String> for RepositoryPath {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<RepositoryPath> for String {
    fn from(value: RepositoryPath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub repository: ModelScopeRepository,
    pub requested_revision: String,
    pub resolved_revision: String,
    pub files_digest: Sha256Digest,
    pub fetched_at: i64,
}

impl RepositorySnapshot {
    pub fn content_digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(&(
            &self.repository,
            self.resolved_revision.to_ascii_lowercase(),
            &self.files_digest,
        ))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.resolved_revision.len(), 40 | 64)
                && self
                    .resolved_revision
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "Repository revision is not an immutable commit"
        );
        ensure!(
            !self.requested_revision.trim().is_empty(),
            "Missing requested revision"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Text,
    Tools,
    Vision,
    Embedding,
    Reranking,
    Transcription,
    SpeechSynthesis,
    AudioUnderstanding,
    AudioGeneration,
    ImageGeneration,
    VideoGeneration,
    Mtp,
    Dflash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStructure {
    Dense,
    Moe,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTaskFamily {
    Language,
    VisionLanguage,
    Asr,
    Tts,
    Audio,
    Embedding,
    Reranking,
    ImageDiffusion,
    VideoDiffusion,
    Specialized,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ArchitectureKnowledge {
    Unknown,
    Observed {
        architecture: String,
        evidence_ids: BTreeSet<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDefinition {
    pub id: String,
    pub display_name: String,
    pub snapshot_ids: BTreeSet<String>,
    pub architecture: ArchitectureKnowledge,
    pub structure: ModelStructure,
    pub task_families: BTreeSet<ModelTaskFamily>,
    pub declared_capabilities: BTreeSet<ModelCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelVariant {
    pub id: String,
    pub model_id: String,
    pub artifact_ids: BTreeSet<String>,
    pub quantization_label: String,
    pub tensor_types: BTreeSet<u32>,
    pub evidence_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentRole {
    Main,
    VisionProjector,
    Mtp,
    Dflash,
    Vae,
    TextEncoder,
    AudioEncoder,
    AudioDecoder,
    Vocoder,
    Codec,
    Metadata,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePurpose {
    GgufWeight,
    Tokenizer,
    RuntimeConfig,
    ChatTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub snapshot_id: String,
    pub path: RepositoryPath,
    pub purpose: FilePurpose,
    pub size_bytes: Option<u64>,
    pub source_sha256: Option<Sha256Digest>,
}

impl Artifact {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.id.is_empty(), "Missing artifact ID");
        let filename = self
            .path
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let allowed = match self.purpose {
            FilePurpose::GgufWeight => filename.ends_with(".gguf"),
            FilePurpose::Tokenizer => matches!(
                filename.as_str(),
                "tokenizer.json"
                    | "tokenizer_config.json"
                    | "special_tokens_map.json"
                    | "added_tokens.json"
                    | "vocab.json"
                    | "vocab.txt"
                    | "merges.txt"
            ),
            FilePurpose::RuntimeConfig => matches!(
                filename.as_str(),
                "config.json"
                    | "configuration.json"
                    | "generation_config.json"
                    | "scheduler_config.json"
                    | "preprocessor_config.json"
                    | "processor_config.json"
            ),
            FilePurpose::ChatTemplate => matches!(
                filename.as_str(),
                "chat_template.jinja" | "chat_template.json"
            ),
        };
        ensure!(
            allowed,
            "File is not allowed for its declared purpose: {}",
            self.path.as_str()
        );
        ensure!(self.size_bytes != Some(0), "Artifact is empty");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub role: ComponentRole,
    pub artifact_ids: BTreeSet<String>,
    pub compatibility: ComponentCompatibility,
    pub evidence_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentCompatibility {
    Verified,
    Incompatible,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyRequirement {
    Required,
    Optional,
    Conditional {
        condition: String,
        resolution: ConditionResolution,
    },
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionResolution {
    Unknown,
    Satisfied,
    Unsatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub requirement: DependencyRequirement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentCompleteness {
    Complete,
    Incomplete,
    Incompatible,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyGraph {
    pub root: String,
    pub components: BTreeMap<String, Component>,
    pub edges: Vec<DependencyEdge>,
}

impl DependencyGraph {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.components.contains_key(&self.root),
            "Missing root component"
        );
        for (id, component) in &self.components {
            ensure!(id == &component.id, "Component key does not match its ID");
        }
        for edge in &self.edges {
            ensure!(
                self.components.contains_key(&edge.from),
                "Missing dependency source"
            );
        }
        let mut visited = BTreeSet::new();
        let mut active = BTreeSet::new();
        for id in self.components.keys() {
            self.visit(id, &mut visited, &mut active)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        id: &'a str,
        visited: &mut BTreeSet<&'a str>,
        active: &mut BTreeSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        ensure!(active.insert(id), "Dependency graph contains a cycle");
        for edge in self.edges.iter().filter(|edge| edge.from == id) {
            if self.components.contains_key(&edge.to) {
                self.visit(&edge.to, visited, active)?;
            }
        }
        active.remove(id);
        visited.insert(id);
        Ok(())
    }

    pub fn completeness(&self) -> Result<ComponentCompleteness> {
        self.validate()?;
        let mut selected = BTreeSet::from([self.root.as_str()]);
        let mut pending = vec![self.root.as_str()];
        let mut unknown = false;
        while let Some(id) = pending.pop() {
            let component = &self.components[id];
            if component.compatibility == ComponentCompatibility::Incompatible {
                return Ok(ComponentCompleteness::Incompatible);
            }
            if component.artifact_ids.is_empty() {
                return Ok(ComponentCompleteness::Incomplete);
            }
            unknown |= component.compatibility == ComponentCompatibility::Unverified
                || component.evidence_ids.is_empty();
            for edge in self.edges.iter().filter(|edge| edge.from == id) {
                let required = match &edge.requirement {
                    DependencyRequirement::Required => true,
                    DependencyRequirement::Optional => self.components.contains_key(&edge.to),
                    DependencyRequirement::Unsupported => {
                        return Ok(ComponentCompleteness::Incompatible)
                    }
                    DependencyRequirement::Conditional {
                        resolution: ConditionResolution::Satisfied,
                        ..
                    } => true,
                    DependencyRequirement::Conditional {
                        resolution: ConditionResolution::Unsatisfied,
                        ..
                    } => false,
                    DependencyRequirement::Conditional {
                        resolution: ConditionResolution::Unknown,
                        ..
                    } => {
                        unknown = true;
                        false
                    }
                };
                if required {
                    if !self.components.contains_key(&edge.to) {
                        return Ok(ComponentCompleteness::Incomplete);
                    }
                    if selected.insert(edge.to.as_str()) {
                        pending.push(edge.to.as_str());
                    }
                }
            }
        }
        ensure!(
            selected.len() == self.components.len(),
            "Dependency graph includes unselected components"
        );
        Ok(if unknown {
            ComponentCompleteness::Unverified
        } else {
            ComponentCompleteness::Complete
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBuild {
    pub adapter_id: String,
    pub engine_revision: String,
    pub binding_version: String,
    pub platform: String,
    pub features: BTreeSet<String>,
    pub binary_sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeExecutionMode {
    InProcess,
    Helper { port: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchChatTemplate {
    Embedded,
    Builtin {
        name: String,
    },
    Inline {
        template: String,
        digest: Sha256Digest,
    },
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchConfig {
    pub installation_id: String,
    pub version_id: String,
    pub manifest_digest: Sha256Digest,
    pub runtime: RuntimeBuild,
    pub capabilities: BTreeSet<ModelCapability>,
    pub component_files: BTreeMap<ComponentRole, Vec<RepositoryPath>>,
    pub parameters: RuntimeParameters,
    pub mode: RuntimeExecutionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "adapter", rename_all = "snake_case")]
pub enum RuntimeParameters {
    LlamaCpp {
        chat_template: LaunchChatTemplate,
        context_length: u32,
        gpu_layers: u32,
        cpu_threads: u32,
        kv_key_type: String,
        kv_value_type: String,
        batch_size: u32,
        flash_attention: bool,
    },
    Whisper {
        cpu_threads: u32,
        use_gpu: bool,
    },
    Audio {
        cpu_threads: u32,
        use_gpu: bool,
    },
    Diffusion {
        cpu_threads: u32,
        use_gpu: bool,
        max_width: u32,
        max_height: u32,
        max_frames: u32,
    },
    Specialized {
        schema_id: String,
        settings: BTreeMap<String, String>,
    },
}

impl LaunchConfig {
    pub fn validate_binding(&self, manifest: &ModelManifest) -> Result<()> {
        manifest.validate()?;
        ensure!(
            self.installation_id == manifest.installation_id
                && self.version_id == manifest.version_id
                && self.manifest_digest == manifest.digest()?
                && self.runtime == manifest.plan.runtime,
            "Launch configuration does not match its installed version and runtime"
        );
        ensure!(
            !self.capabilities.is_empty()
                && self
                    .capabilities
                    .is_subset(&manifest.plan.target_capabilities),
            "Launch capabilities exceed the installation profile"
        );
        let mut expected: BTreeMap<ComponentRole, BTreeSet<&str>> = BTreeMap::new();
        for component in manifest.plan.dependencies.components.values() {
            let files = expected.entry(component.role).or_default();
            for id in &component.artifact_ids {
                files.insert(manifest.installed_files[id].relative_path.as_str());
            }
        }
        let mut actual = BTreeMap::new();
        for (role, paths) in &self.component_files {
            let files: BTreeSet<_> = paths.iter().map(RepositoryPath::as_str).collect();
            ensure!(
                files.len() == paths.len(),
                "Launch component files contain duplicates"
            );
            actual.insert(*role, files);
        }
        ensure!(
            actual == expected,
            "Launch component files differ from the installation"
        );
        Ok(())
    }

    pub fn digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledArtifact {
    pub artifact_id: String,
    pub relative_path: RepositoryPath,
    pub computed_sha256: Sha256Digest,
    pub size_bytes: u64,
    pub verification_evidence: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub installation_id: String,
    pub version_id: String,
    pub plan: InstallPlan,
    pub installed_files: BTreeMap<String, InstalledArtifact>,
    pub plan_accepted_at: i64,
    pub installed_at: i64,
}

impl ModelManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == MODEL_LIFECYCLE_SCHEMA_VERSION,
            "Unsupported model manifest schema"
        );
        ensure!(
            !self.installation_id.trim().is_empty(),
            "Missing installation ID"
        );
        ensure!(
            !self.version_id.trim().is_empty(),
            "Missing installation version ID"
        );
        ensure!(
            self.plan_accepted_at <= self.installed_at,
            "Installation predates plan acceptance"
        );
        self.plan
            .validate_for_install(self.plan_accepted_at, u64::MAX)?;
        ensure!(
            self.installed_files.len() == self.plan.artifacts.len(),
            "Installed file set does not match the plan"
        );
        let mut paths = BTreeSet::new();
        for (id, file) in &self.installed_files {
            let artifact = self
                .plan
                .artifacts
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("Installed file is not in the plan"))?;
            ensure!(
                id == &file.artifact_id,
                "Installed artifact identity mismatch"
            );
            ensure!(
                file.size_bytes > 0
                    && artifact
                        .size_bytes
                        .is_none_or(|size| size == file.size_bytes),
                "Installed artifact size mismatch"
            );
            ensure!(
                artifact
                    .source_sha256
                    .as_ref()
                    .is_none_or(|hash| hash == &file.computed_sha256),
                "Installed artifact SHA256 mismatch"
            );
            ensure!(
                paths.insert(file.relative_path.as_str().to_lowercase()),
                "Installed artifact paths collide"
            );
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareCompatibility {
    Runnable,
    Limited,
    NotRecommended,
    Incompatible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub schema_version: u32,
    pub id: String,
    pub model_id: String,
    pub variant_id: String,
    pub target_capabilities: BTreeSet<ModelCapability>,
    pub snapshots: BTreeMap<String, RepositorySnapshot>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub dependencies: DependencyGraph,
    pub runtime: RuntimeBuild,
    pub runtime_verified: bool,
    pub hardware: HardwareCompatibility,
    pub required_disk_bytes: u64,
    pub rules_version: String,
    pub expires_at: i64,
}

impl InstallPlan {
    pub fn digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }

    pub fn validate_for_install(&self, now: i64, available_disk_bytes: u64) -> Result<()> {
        ensure!(
            self.schema_version == MODEL_LIFECYCLE_SCHEMA_VERSION,
            "Unsupported installation plan schema"
        );
        ensure!(!self.id.is_empty(), "Missing installation plan ID");
        ensure!(
            !self.model_id.trim().is_empty() && !self.variant_id.trim().is_empty(),
            "Missing model or variant identity"
        );
        ensure!(
            !self.target_capabilities.is_empty(),
            "Missing target capabilities"
        );
        ensure!(self.expires_at > now, "Installation plan has expired");
        ensure!(!self.rules_version.is_empty(), "Missing rules version");
        ensure!(self.runtime_verified, "Runtime support is unverified");
        ensure!(
            !self.runtime.adapter_id.is_empty() && !self.runtime.engine_revision.is_empty(),
            "Missing runtime identity"
        );
        ensure!(
            matches!(
                self.hardware,
                HardwareCompatibility::Runnable | HardwareCompatibility::Limited
            ),
            "Hardware does not satisfy the installation profile"
        );
        ensure!(
            self.required_disk_bytes > 0 && self.required_disk_bytes <= available_disk_bytes,
            "Insufficient disk space"
        );
        ensure!(
            self.dependencies.completeness()? == ComponentCompleteness::Complete,
            "Model components are not complete and verified"
        );
        ensure!(
            self.dependencies.components[&self.dependencies.root].role == ComponentRole::Main,
            "Model root must be a main component"
        );
        for snapshot in self.snapshots.values() {
            snapshot.validate()?;
        }
        let mut source_paths = BTreeSet::new();
        for (id, artifact) in &self.artifacts {
            ensure!(id == &artifact.id, "Artifact key does not match its ID");
            artifact.validate()?;
            ensure!(
                artifact.size_bytes.is_some(),
                "Artifact size is unknown; installation budget cannot be verified"
            );
            ensure!(
                self.snapshots.contains_key(&artifact.snapshot_id),
                "Artifact has no repository snapshot"
            );
            ensure!(
                source_paths.insert((&artifact.snapshot_id, artifact.path.as_str())),
                "Duplicate artifact source path"
            );
        }
        let mut seen_files = BTreeSet::new();
        for component in self.dependencies.components.values() {
            for id in &component.artifact_ids {
                let Some(artifact) = self.artifacts.get(id) else {
                    bail!("Component references a missing artifact");
                };
                let role_matches = match component.role {
                    ComponentRole::Metadata => artifact.purpose != FilePurpose::GgufWeight,
                    _ => artifact.purpose == FilePurpose::GgufWeight,
                };
                ensure!(
                    role_matches,
                    "Component role does not match the artifact purpose"
                );
                seen_files.insert(id);
            }
        }
        ensure!(
            seen_files.len() == self.artifacts.len(),
            "Plan includes unassigned artifacts"
        );
        Ok(())
    }
}
