use super::*;

/// Selection identity is common; parameter schemas and compilation products are not.
/// An ASR/TTS/vector adapter supplies its own associated types instead of inheriting
/// diffusion flags or claiming that all models of a task family share the same options.
pub trait ModelTaskParameterAdapter {
    type Request;
    type Prepared;
    type Descriptor;

    fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity>;
    fn describe(&self) -> &Self::Descriptor;
    fn prepare(&self, request: Self::Request) -> GenerationParameterResult<Self::Prepared>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskParameterIdentity {
    pub installation_id: String,
    pub version_id: String,
    pub manifest_digest: String,
    pub runtime_digest: String,
    pub component_binding_digest: String,
    pub task_variant: String,
    pub parameter_schema_id: String,
}

/// Typed registries keep each adapter's request/response contract intact. The task
/// service dispatches by family first, then resolves an exact identity in that registry.
pub struct TaskParameterRegistry<A: ModelTaskParameterAdapter> {
    adapters: BTreeMap<TaskParameterIdentity, A>,
}

impl<A: ModelTaskParameterAdapter> Default for TaskParameterRegistry<A> {
    fn default() -> Self {
        Self {
            adapters: BTreeMap::new(),
        }
    }
}

impl<A: ModelTaskParameterAdapter> TaskParameterRegistry<A> {
    pub fn register(&mut self, adapter: A) -> GenerationParameterResult<TaskParameterIdentity> {
        let identity = adapter.identity()?;
        if self.adapters.contains_key(&identity) {
            return Err(issue(
                "binding",
                "already_registered",
                "A parameter adapter is already registered for this exact identity",
            ));
        }
        self.adapters.insert(identity.clone(), adapter);
        Ok(identity)
    }

    pub fn resolve(&self, identity: &TaskParameterIdentity) -> GenerationParameterResult<&A> {
        self.adapters.get(identity).ok_or_else(|| issue("binding", "unsupported_model_task", "No parameter adapter is registered for this exact installation, version, components, Runtime and task variant"))
    }

    pub fn prepare(
        &self,
        identity: &TaskParameterIdentity,
        request: A::Request,
    ) -> GenerationParameterResult<A::Prepared> {
        self.resolve(identity)?.prepare(request)
    }
}

impl ModelTaskParameterAdapter for GenerationParameterService {
    type Request = GenerationTaskRequest;
    type Prepared = PreparedGenerationTask;
    type Descriptor = GenerationParameterDescriptor;

    fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity> {
        let digest = |value: &Value| {
            Sha256Digest::of_serializable(value)
                .map(|digest| digest.as_str().to_owned())
                .map_err(|_| {
                    issue(
                        "binding",
                        "serialization_failed",
                        "Cannot digest model binding",
                    )
                })
        };
        Ok(TaskParameterIdentity {
            installation_id: self.binding.installation_id.clone(),
            version_id: self.binding.version_id.clone(),
            manifest_digest: self.binding.manifest_digest.as_str().into(),
            runtime_digest: digest(&serde_json::to_value(&self.binding.runtime).map_err(
                |_| {
                    issue(
                        "binding",
                        "serialization_failed",
                        "Cannot serialize Runtime binding",
                    )
                },
            )?)?,
            component_binding_digest: digest(
                &serde_json::to_value(&self.binding.assets).map_err(|_| {
                    issue(
                        "binding",
                        "serialization_failed",
                        "Cannot serialize component binding",
                    )
                })?,
            )?,
            task_variant: self.profile.task_variant().into(),
            parameter_schema_id: self.profile.parameter_schema_id().into(),
        })
    }

    fn describe(&self) -> &Self::Descriptor {
        self.descriptor()
    }
    fn prepare(&self, request: Self::Request) -> GenerationParameterResult<Self::Prepared> {
        self.compile(request)
    }
}
