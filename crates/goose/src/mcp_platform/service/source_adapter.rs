//! Discovery and intake preparation for governed MCP source adapters.
//!
//! This module intentionally does not parse raw JSON, contact a source, spawn a process,
//! persist an intake, or provision a distribution. Discovery is descriptive: SDK visibility
//! never grants approval or authorizes preparation. ACP and UI handlers are not connected to
//! this registry yet; a future privileged writer owns provisioning.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use goose_sdk_types::custom_requests::{
    McpSourceAdapterApproval, McpSourceAdapterAvailability, McpSourceAdapterBlockedReason,
    McpSourceAdapterDescriptor, McpSourceAdapterInputField, McpSourceAdapterInputFieldKind,
    McpSourceAdapterRisk, McpSourceAdapterSourceKind, McpSourceAdapterTransport,
    McpSourceAdapterTrustMode, McpSourceAdaptersListResult,
};

/// A source adapter provides a descriptor candidate. Public registration quarantines extensions;
/// implementations never receive raw user input before a later owner uses a prepared request.
pub trait McpSourceAdapter: Send + Sync {
    fn descriptor(&self) -> SourceAdapterDescriptor;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAdapterDescriptor {
    pub id: String,
    pub display_name: String,
    pub source_kind: SourceAdapterKind,
    pub trust_mode: SourceAdapterTrustMode,
    pub risks: BTreeSet<SourceAdapterRisk>,
    pub required_host_dependencies: BTreeSet<String>,
    pub allowed_transports: BTreeSet<SourceAdapterTransport>,
    pub input_fields: BTreeMap<String, SourceAdapterInputField>,
    pub required_approvals: BTreeSet<SourceAdapterApproval>,
    pub availability: SourceAdapterAvailability,
}

impl SourceAdapterDescriptor {
    fn to_sdk_descriptor(&self) -> Result<McpSourceAdapterDescriptor, SourceAdapterRegistryError> {
        self.to_sdk_descriptor_unchecked()
    }

    fn to_sdk_descriptor_unchecked(
        &self,
    ) -> Result<McpSourceAdapterDescriptor, SourceAdapterRegistryError> {
        if self.trust_mode == SourceAdapterTrustMode::Unverified {
            return self.to_quarantined_sdk_descriptor();
        }

        McpSourceAdapterDescriptor::new(
            self.display_name.clone(),
            self.source_kind.to_sdk(),
            self.trust_mode.to_sdk(),
            self.to_sdk_availability(),
            self.to_sdk_approvals(),
            self.risks
                .iter()
                .copied()
                .map(SourceAdapterRisk::to_sdk)
                .collect(),
            self.required_host_dependencies.iter().cloned().collect(),
            self.allowed_transports
                .iter()
                .copied()
                .map(SourceAdapterTransport::to_sdk)
                .collect(),
            self.input_fields
                .iter()
                .map(|(id, field)| (id.clone(), field.to_sdk()))
                .collect(),
        )
        .map_err(|_| SourceAdapterRegistryError::InvalidDescriptor)
    }

    fn to_quarantined_sdk_descriptor(
        &self,
    ) -> Result<McpSourceAdapterDescriptor, SourceAdapterRegistryError> {
        McpSourceAdapterDescriptor::new(
            "Unverified source".to_string(),
            McpSourceAdapterSourceKind::Opaque {
                kind_id: "unverified".to_string(),
            },
            McpSourceAdapterTrustMode::Unverified,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unverified,
            },
            Vec::new(),
            vec![McpSourceAdapterRisk::ProcessExecution],
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
        )
        .map_err(|_| SourceAdapterRegistryError::InvalidDescriptor)
    }

    fn to_sdk_availability(&self) -> McpSourceAdapterAvailability {
        if self.trust_mode == SourceAdapterTrustMode::Unverified {
            return McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unverified,
            };
        }

        match self.availability {
            SourceAdapterAvailability::ReadyForIntake if self.to_sdk_approvals().is_empty() => {
                McpSourceAdapterAvailability::ReadyForIntake {}
            }
            SourceAdapterAvailability::ReadyForIntake => McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::ApprovalRequired,
            },
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::UnsupportedSourceKind,
            ) => McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unsupported,
            },
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ) if !self.required_host_dependencies.is_empty() => {
                McpSourceAdapterAvailability::Blocked {
                    reason: McpSourceAdapterBlockedReason::RuntimeDependency,
                }
            }
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ) => McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Policy,
            },
        }
    }

    fn to_sdk_approvals(&self) -> Vec<McpSourceAdapterApproval> {
        if self.trust_mode == SourceAdapterTrustMode::Unverified {
            return Vec::new();
        }

        let mut approvals = BTreeSet::new();
        for approval in &self.required_approvals {
            match approval {
                // This is provenance, not an SDK approval capability. The SDK trust mode carries
                // it while the service retains this exact internal requirement for preparation.
                SourceAdapterApproval::UserManagedSource => {}
                SourceAdapterApproval::NetworkAccess => {
                    approvals.insert(McpSourceAdapterApproval::NetworkAccess);
                }
                SourceAdapterApproval::ProcessExecution => {
                    approvals.insert(McpSourceAdapterApproval::ProcessExecution);
                }
                SourceAdapterApproval::FilesystemAccess => {
                    if self.risks.contains(&SourceAdapterRisk::FilesystemRead) {
                        approvals.insert(McpSourceAdapterApproval::FilesystemRead);
                    }
                    if self.risks.contains(&SourceAdapterRisk::FilesystemWrite) {
                        approvals.insert(McpSourceAdapterApproval::FilesystemWrite);
                    }
                }
                SourceAdapterApproval::HostDependency => {
                    approvals.insert(McpSourceAdapterApproval::HostDependency);
                }
                SourceAdapterApproval::CredentialReference => {
                    approvals.insert(McpSourceAdapterApproval::CredentialReference);
                }
            }
        }
        approvals.into_iter().collect()
    }

    fn validate(&self) -> Result<(), SourceAdapterRegistryError> {
        if !is_valid_identifier(&self.id)
            || self.input_fields.len() > 32
            || self.input_fields.keys().any(|id| !is_valid_identifier(id))
            || self
                .required_host_dependencies
                .iter()
                .any(|id| !is_valid_identifier(id))
        {
            return Err(SourceAdapterRegistryError::InvalidDescriptor);
        }
        if self.required_approvals != required_approvals(self) {
            return Err(SourceAdapterRegistryError::InvalidDescriptor);
        }
        if self.trust_mode == SourceAdapterTrustMode::Unverified
            && self.availability == SourceAdapterAvailability::ReadyForIntake
        {
            return Err(SourceAdapterRegistryError::InvalidDescriptor);
        }
        if matches!(&self.source_kind, SourceAdapterKind::Opaque { .. })
            && self.availability
                != SourceAdapterAvailability::Blocked(
                    SourceAdapterBlockedReason::UnsupportedSourceKind,
                )
        {
            return Err(SourceAdapterRegistryError::InvalidDescriptor);
        }
        if self.input_fields.values().any(|field| {
            field.secret_reference_only
                && !matches!(
                    field.kind,
                    SourceAdapterInputKind::Reference | SourceAdapterInputKind::ProviderReference
                )
        }) {
            return Err(SourceAdapterRegistryError::InvalidDescriptor);
        }
        self.to_sdk_descriptor_unchecked().map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceAdapterKind {
    Catalog,
    RemoteHttp,
    ManualStdio,
    Npm,
    Uvx,
    Docker,
    Git,
    /// Unknown kinds deliberately have no transport and cannot become trusted through discovery.
    Opaque {
        kind_id: String,
    },
}

impl SourceAdapterKind {
    fn to_sdk(&self) -> McpSourceAdapterSourceKind {
        match self {
            Self::Catalog => McpSourceAdapterSourceKind::Catalog {},
            Self::RemoteHttp => McpSourceAdapterSourceKind::RemoteHttp {},
            Self::ManualStdio => McpSourceAdapterSourceKind::ManualStdio {},
            Self::Npm => McpSourceAdapterSourceKind::Npm {},
            Self::Uvx => McpSourceAdapterSourceKind::Uvx {},
            Self::Docker => McpSourceAdapterSourceKind::Docker {},
            Self::Git => McpSourceAdapterSourceKind::Git {},
            Self::Opaque { kind_id } => McpSourceAdapterSourceKind::Opaque {
                kind_id: kind_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceAdapterTrustMode {
    Verified,
    UserManaged,
    Unverified,
}

impl SourceAdapterTrustMode {
    fn to_sdk(self) -> McpSourceAdapterTrustMode {
        match self {
            Self::Verified => McpSourceAdapterTrustMode::Verified,
            Self::UserManaged => McpSourceAdapterTrustMode::UserManaged,
            Self::Unverified => McpSourceAdapterTrustMode::Unverified,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceAdapterRisk {
    NetworkAccess,
    ProcessExecution,
    FilesystemRead,
    FilesystemWrite,
    HostDependency,
    CredentialReference,
}

impl SourceAdapterRisk {
    fn to_sdk(self) -> McpSourceAdapterRisk {
        match self {
            Self::NetworkAccess => McpSourceAdapterRisk::NetworkAccess,
            Self::ProcessExecution => McpSourceAdapterRisk::ProcessExecution,
            Self::FilesystemRead => McpSourceAdapterRisk::FilesystemRead,
            Self::FilesystemWrite => McpSourceAdapterRisk::FilesystemWrite,
            Self::HostDependency => McpSourceAdapterRisk::HostDependency,
            Self::CredentialReference => McpSourceAdapterRisk::CredentialReference,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceAdapterTransport {
    Catalog,
    RemoteHttp,
    StdioProvider,
}

impl SourceAdapterTransport {
    fn to_sdk(self) -> McpSourceAdapterTransport {
        match self {
            Self::Catalog => McpSourceAdapterTransport::Catalog,
            Self::RemoteHttp => McpSourceAdapterTransport::RemoteHttp,
            Self::StdioProvider => McpSourceAdapterTransport::StdioProvider,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceAdapterApproval {
    UserManagedSource,
    NetworkAccess,
    ProcessExecution,
    FilesystemAccess,
    HostDependency,
    CredentialReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAdapterAvailability {
    ReadyForIntake,
    Blocked(SourceAdapterBlockedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAdapterBlockedReason {
    SafeProvisioningUnavailable,
    UnsupportedSourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAdapterInputField {
    pub label: String,
    pub kind: SourceAdapterInputKind,
    pub required: bool,
    pub multiline: bool,
    pub secret_reference_only: bool,
}

impl SourceAdapterInputField {
    fn to_sdk(&self) -> McpSourceAdapterInputField {
        McpSourceAdapterInputField {
            label: self.label.clone(),
            kind: match self.kind {
                SourceAdapterInputKind::Text => McpSourceAdapterInputFieldKind::Text,
                SourceAdapterInputKind::Reference => McpSourceAdapterInputFieldKind::Reference,
                SourceAdapterInputKind::Directory => McpSourceAdapterInputFieldKind::Directory,
                SourceAdapterInputKind::ProviderReference => {
                    McpSourceAdapterInputFieldKind::ProviderReference
                }
            },
            required: self.required,
            multiline: self.multiline,
            secret_reference_only: self.secret_reference_only,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAdapterInputKind {
    Text,
    Reference,
    Directory,
    ProviderReference,
}

#[derive(Clone, Default)]
pub struct SourceAdapterRegistry {
    adapters: BTreeMap<String, RegisteredSourceAdapter>,
}

#[derive(Clone)]
struct RegisteredSourceAdapter {
    descriptor: SourceAdapterDescriptor,
    _adapter: Arc<dyn McpSourceAdapter>,
}

impl fmt::Debug for SourceAdapterRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceAdapterRegistry")
            .field("adapter_ids", &self.adapters.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SourceAdapterRegistry {
    pub fn built_in() -> Self {
        let mut registry = Self::default();
        for adapter in built_in_adapters() {
            // Built-ins are compiled descriptors and invalid data is a programmer error.
            registry
                .register_built_in(adapter)
                .expect("built-in source adapter is valid");
        }
        registry
    }

    pub fn register(
        &mut self,
        adapter: Arc<dyn McpSourceAdapter>,
    ) -> Result<(), SourceAdapterRegistryError> {
        let mut descriptor = adapter.descriptor();
        descriptor.trust_mode = SourceAdapterTrustMode::Unverified;
        descriptor.availability =
            if matches!(&descriptor.source_kind, SourceAdapterKind::Opaque { .. }) {
                SourceAdapterAvailability::Blocked(
                    SourceAdapterBlockedReason::UnsupportedSourceKind,
                )
            } else {
                SourceAdapterAvailability::Blocked(
                    SourceAdapterBlockedReason::SafeProvisioningUnavailable,
                )
            };
        descriptor.required_approvals = required_approvals(&descriptor);
        self.register_descriptor(descriptor, adapter)
    }

    fn register_built_in(
        &mut self,
        adapter: Arc<dyn McpSourceAdapter>,
    ) -> Result<(), SourceAdapterRegistryError> {
        self.register_descriptor(adapter.descriptor(), adapter)
    }

    fn register_descriptor(
        &mut self,
        descriptor: SourceAdapterDescriptor,
        adapter: Arc<dyn McpSourceAdapter>,
    ) -> Result<(), SourceAdapterRegistryError> {
        descriptor.validate()?;
        if self.adapters.contains_key(&descriptor.id) {
            return Err(SourceAdapterRegistryError::DuplicateAdapterId);
        }
        self.adapters.insert(
            descriptor.id.clone(),
            RegisteredSourceAdapter {
                descriptor,
                _adapter: adapter,
            },
        );
        Ok(())
    }

    pub fn discover(&self) -> SourceAdapterDiscovery {
        SourceAdapterDiscovery {
            adapters: self
                .adapters
                .values()
                .map(|adapter| {
                    let descriptor = adapter.descriptor.clone();
                    (descriptor.id.clone(), descriptor)
                })
                .collect(),
        }
    }

    pub fn prepare_intake(
        &self,
        request: SourceAdapterIntakeRequest,
        context: &SourceAdapterIntakeContext,
    ) -> Result<PreparedSourceAdapterIntake, SourceAdapterIntakeRejection> {
        if !is_valid_identifier(&request.adapter_id) {
            return Err(SourceAdapterIntakeRejection::InvalidAdapterId);
        }
        let Some(adapter) = self.adapters.get(&request.adapter_id) else {
            return Err(SourceAdapterIntakeRejection::UnknownAdapter);
        };
        let descriptor = &adapter.descriptor;
        match descriptor.availability {
            SourceAdapterAvailability::ReadyForIntake => {}
            SourceAdapterAvailability::Blocked(_) => {
                return Err(SourceAdapterIntakeRejection::AdapterUnavailable);
            }
        }
        if !descriptor.allowed_transports.contains(&request.transport) {
            return Err(SourceAdapterIntakeRejection::TransportNotAllowed);
        }
        if !descriptor
            .required_host_dependencies
            .is_subset(&context.available_host_dependencies)
        {
            return Err(SourceAdapterIntakeRejection::HostDependencyUnavailable);
        }
        if !descriptor.risks.is_subset(&context.allowed_risks)
            || (descriptor.trust_mode == SourceAdapterTrustMode::UserManaged
                && !context.allow_user_managed_sources)
        {
            return Err(SourceAdapterIntakeRejection::PolicyDenied);
        }
        if !descriptor
            .required_approvals
            .is_subset(&context.approved_requirements)
        {
            return Err(SourceAdapterIntakeRejection::ApprovalRequired);
        }
        if request.fields.len() > 32
            || request
                .fields
                .keys()
                .any(|field| !is_valid_identifier(field))
        {
            return Err(SourceAdapterIntakeRejection::UnsupportedInput);
        }
        for (field_id, field) in &descriptor.input_fields {
            match request.fields.get(field_id) {
                Some(value) => validate_input_value(field, value)?,
                None if field.required => {
                    return Err(SourceAdapterIntakeRejection::RequiredInputMissing);
                }
                None => {}
            }
        }
        if request
            .fields
            .keys()
            .any(|field_id| !descriptor.input_fields.contains_key(field_id))
        {
            return Err(SourceAdapterIntakeRejection::UnsupportedInput);
        }

        Ok(PreparedSourceAdapterIntake {
            adapter_id: descriptor.id.clone(),
            source_kind: descriptor.source_kind.clone(),
            transport: request.transport,
            fields: request.fields,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAdapterDiscovery {
    adapters: BTreeMap<String, SourceAdapterDescriptor>,
}

impl SourceAdapterDiscovery {
    pub fn adapters(&self) -> &BTreeMap<String, SourceAdapterDescriptor> {
        &self.adapters
    }

    /// SDK discovery exposes every validated descriptor, including blocked entries, for a UI to
    /// explain. It is strictly descriptive: preparation re-evaluates the registry's internal
    /// availability, policy, host dependencies, and exact approvals independently.
    pub fn to_sdk_result(&self) -> Result<McpSourceAdaptersListResult, SourceAdapterRegistryError> {
        self.adapters
            .iter()
            .map(|(id, descriptor)| {
                if id != &descriptor.id || !is_valid_identifier(id) {
                    return Err(SourceAdapterRegistryError::InvalidDescriptor);
                }
                Ok((id.clone(), descriptor.to_sdk_descriptor()?))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(|adapters| McpSourceAdaptersListResult { adapters })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAdapterRegistryError {
    DuplicateAdapterId,
    InvalidDescriptor,
}

#[derive(Debug, Clone, Default)]
pub struct SourceAdapterIntakeContext {
    pub available_host_dependencies: BTreeSet<String>,
    pub allowed_risks: BTreeSet<SourceAdapterRisk>,
    pub approved_requirements: BTreeSet<SourceAdapterApproval>,
    pub allow_user_managed_sources: bool,
}

impl SourceAdapterIntakeContext {
    pub fn discovery_default() -> Self {
        Self {
            allowed_risks: [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::CredentialReference,
            ]
            .into_iter()
            .collect(),
            ..Self::default()
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SourceAdapterIntakeRequest {
    pub adapter_id: String,
    pub transport: SourceAdapterTransport,
    pub fields: BTreeMap<String, SourceAdapterInputValue>,
}

impl fmt::Debug for SourceAdapterIntakeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceAdapterIntakeRequest")
            .field("adapter_id", &self.adapter_id)
            .field("transport", &self.transport)
            .field("field_ids", &self.fields.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum SourceAdapterInputValue {
    Text(String),
    Reference(SourceAdapterReference),
    Directory(String),
    ProviderReference(SourceAdapterReference),
}

impl fmt::Debug for SourceAdapterInputValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Text(_) => "Text",
            Self::Reference(_) => "Reference",
            Self::Directory(_) => "Directory",
            Self::ProviderReference(_) => "ProviderReference",
        };
        formatter
            .write_str(name)
            .and_then(|_| formatter.write_str("([redacted])"))
    }
}

/// References are identifiers, never credential material. The inner value intentionally has no
/// public accessor so callers cannot accidentally treat an intake reference as a secret value.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceAdapterReference(String);

impl SourceAdapterReference {
    pub fn new(value: String) -> Result<Self, SourceAdapterIntakeRejection> {
        if is_valid_identifier(&value) {
            Ok(Self(value))
        } else {
            Err(SourceAdapterIntakeRejection::SecretReferenceRequired)
        }
    }
}

impl fmt::Debug for SourceAdapterReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceAdapterReference([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedSourceAdapterIntake {
    pub adapter_id: String,
    pub source_kind: SourceAdapterKind,
    pub transport: SourceAdapterTransport,
    pub fields: BTreeMap<String, SourceAdapterInputValue>,
}

impl fmt::Debug for PreparedSourceAdapterIntake {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSourceAdapterIntake")
            .field("adapter_id", &self.adapter_id)
            .field("source_kind", &self.source_kind)
            .field("transport", &self.transport)
            .field("field_ids", &self.fields.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAdapterIntakeRejection {
    InvalidAdapterId,
    UnknownAdapter,
    AdapterUnavailable,
    TransportNotAllowed,
    HostDependencyUnavailable,
    PolicyDenied,
    ApprovalRequired,
    UnsupportedInput,
    RequiredInputMissing,
    InvalidInput,
    SecretReferenceRequired,
}

fn validate_input_value(
    field: &SourceAdapterInputField,
    value: &SourceAdapterInputValue,
) -> Result<(), SourceAdapterIntakeRejection> {
    let kind_matches = matches!(
        (field.kind, value),
        (
            SourceAdapterInputKind::Text,
            SourceAdapterInputValue::Text(_)
        ) | (
            SourceAdapterInputKind::Reference,
            SourceAdapterInputValue::Reference(_)
        ) | (
            SourceAdapterInputKind::Directory,
            SourceAdapterInputValue::Directory(_)
        ) | (
            SourceAdapterInputKind::ProviderReference,
            SourceAdapterInputValue::ProviderReference(_)
        )
    );
    if !kind_matches {
        return Err(if field.secret_reference_only {
            SourceAdapterIntakeRejection::SecretReferenceRequired
        } else {
            SourceAdapterIntakeRejection::InvalidInput
        });
    }
    match value {
        SourceAdapterInputValue::Text(value) => {
            if !is_safe_text_input(value) {
                return Err(SourceAdapterIntakeRejection::InvalidInput);
            }
        }
        SourceAdapterInputValue::Directory(value) => {
            if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
                return Err(SourceAdapterIntakeRejection::InvalidInput);
            }
        }
        SourceAdapterInputValue::Reference(_) | SourceAdapterInputValue::ProviderReference(_) => {}
    }
    Ok(())
}

fn is_safe_text_input(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

fn required_approvals(descriptor: &SourceAdapterDescriptor) -> BTreeSet<SourceAdapterApproval> {
    let mut approvals = BTreeSet::new();
    if descriptor.trust_mode == SourceAdapterTrustMode::UserManaged {
        approvals.insert(SourceAdapterApproval::UserManagedSource);
    }
    for risk in &descriptor.risks {
        approvals.insert(match risk {
            SourceAdapterRisk::NetworkAccess => SourceAdapterApproval::NetworkAccess,
            SourceAdapterRisk::ProcessExecution => SourceAdapterApproval::ProcessExecution,
            SourceAdapterRisk::FilesystemRead | SourceAdapterRisk::FilesystemWrite => {
                SourceAdapterApproval::FilesystemAccess
            }
            SourceAdapterRisk::HostDependency => SourceAdapterApproval::HostDependency,
            SourceAdapterRisk::CredentialReference => SourceAdapterApproval::CredentialReference,
        });
    }
    approvals
}

fn is_valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 128
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        })
}

#[derive(Clone)]
struct BuiltInSourceAdapter(SourceAdapterDescriptor);

impl McpSourceAdapter for BuiltInSourceAdapter {
    fn descriptor(&self) -> SourceAdapterDescriptor {
        self.0.clone()
    }
}

fn built_in_adapters() -> Vec<Arc<dyn McpSourceAdapter>> {
    vec![
        built_in(
            "managed.catalog",
            "Managed catalog",
            SourceAdapterKind::Catalog,
            SourceAdapterTrustMode::Verified,
            [SourceAdapterRisk::NetworkAccess],
            [],
            [SourceAdapterTransport::Catalog],
            [],
            [SourceAdapterApproval::NetworkAccess],
            SourceAdapterAvailability::ReadyForIntake,
        ),
        built_in(
            "https.configuration",
            "HTTPS configuration",
            SourceAdapterKind::RemoteHttp,
            SourceAdapterTrustMode::UserManaged,
            [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::CredentialReference,
            ],
            [],
            [SourceAdapterTransport::RemoteHttp],
            [(
                "configuration",
                reference_field("Configuration reference", true),
            )],
            [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::CredentialReference,
            ],
            SourceAdapterAvailability::ReadyForIntake,
        ),
        built_in(
            "approved.stdio",
            "Approved stdio",
            SourceAdapterKind::ManualStdio,
            SourceAdapterTrustMode::UserManaged,
            [SourceAdapterRisk::ProcessExecution],
            [],
            [SourceAdapterTransport::StdioProvider],
            [],
            [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::ProcessExecution,
            ],
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        ),
        built_in(
            "registry.npm",
            "npm registry",
            SourceAdapterKind::Npm,
            SourceAdapterTrustMode::Verified,
            [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
                SourceAdapterRisk::CredentialReference,
            ],
            ["node"],
            [
                SourceAdapterTransport::Catalog,
                SourceAdapterTransport::StdioProvider,
            ],
            [
                ("package", text_field("Package", true)),
                ("credential", reference_field("Credential reference", false)),
            ],
            [
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
                SourceAdapterApproval::CredentialReference,
            ],
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        ),
        built_in(
            "registry.uvx",
            "UVX registry",
            SourceAdapterKind::Uvx,
            SourceAdapterTrustMode::UserManaged,
            [
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
            ],
            ["uv"],
            [SourceAdapterTransport::StdioProvider],
            [("package", text_field("Package", true))],
            [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
            ],
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        ),
        built_in(
            "runtime.docker",
            "Docker runtime",
            SourceAdapterKind::Docker,
            SourceAdapterTrustMode::UserManaged,
            [
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
            ],
            ["docker"],
            [SourceAdapterTransport::StdioProvider],
            [("image", text_field("Image reference", true))],
            [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
            ],
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        ),
        built_in(
            "source.git",
            "Git source",
            SourceAdapterKind::Git,
            SourceAdapterTrustMode::UserManaged,
            [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
            ],
            ["git"],
            [SourceAdapterTransport::StdioProvider],
            [("revision", text_field("Pinned revision", true))],
            [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
            ],
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        ),
    ]
}

fn built_in(
    id: &str,
    display_name: &str,
    source_kind: SourceAdapterKind,
    trust_mode: SourceAdapterTrustMode,
    risks: impl IntoIterator<Item = SourceAdapterRisk>,
    dependencies: impl IntoIterator<Item = &'static str>,
    transports: impl IntoIterator<Item = SourceAdapterTransport>,
    fields: impl IntoIterator<Item = (&'static str, SourceAdapterInputField)>,
    approvals: impl IntoIterator<Item = SourceAdapterApproval>,
    availability: SourceAdapterAvailability,
) -> Arc<dyn McpSourceAdapter> {
    Arc::new(BuiltInSourceAdapter(SourceAdapterDescriptor {
        id: id.to_string(),
        display_name: display_name.to_string(),
        source_kind,
        trust_mode,
        risks: risks.into_iter().collect(),
        required_host_dependencies: dependencies.into_iter().map(str::to_string).collect(),
        allowed_transports: transports.into_iter().collect(),
        input_fields: fields
            .into_iter()
            .map(|(id, field)| (id.to_string(), field))
            .collect(),
        required_approvals: approvals.into_iter().collect(),
        availability,
    }))
}

fn text_field(label: &str, required: bool) -> SourceAdapterInputField {
    SourceAdapterInputField {
        label: label.to_string(),
        kind: SourceAdapterInputKind::Text,
        required,
        multiline: false,
        secret_reference_only: false,
    }
}

fn reference_field(label: &str, required: bool) -> SourceAdapterInputField {
    SourceAdapterInputField {
        label: label.to_string(),
        kind: SourceAdapterInputKind::Reference,
        required,
        multiline: false,
        secret_reference_only: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_discovery_is_deterministic_and_maps_to_the_sdk_contract() {
        let registry = SourceAdapterRegistry::built_in();
        let discovery = registry.discover();
        let ids = discovery.adapters().keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "approved.stdio",
                "https.configuration",
                "managed.catalog",
                "registry.npm",
                "registry.uvx",
                "runtime.docker",
                "source.git"
            ]
        );
        let sdk = discovery.to_sdk_result().unwrap();
        assert_eq!(sdk.adapters.keys().cloned().collect::<Vec<_>>(), ids);
        assert_eq!(
            sdk.adapters["managed.catalog"].availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::ApprovalRequired,
            }
        );
        assert_eq!(
            sdk.adapters["https.configuration"].availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::ApprovalRequired,
            }
        );
        assert_eq!(
            sdk.adapters["approved.stdio"].availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Policy,
            }
        );
        for id in [
            "registry.npm",
            "registry.uvx",
            "runtime.docker",
            "source.git",
        ] {
            assert_eq!(
                sdk.adapters[id].availability,
                McpSourceAdapterAvailability::Blocked {
                    reason: McpSourceAdapterBlockedReason::RuntimeDependency,
                },
                "{id} must preserve its internal runtime block over future approvals"
            );
        }
        assert_eq!(
            sdk.adapters["approved.stdio"].required_approvals,
            vec![McpSourceAdapterApproval::ProcessExecution]
        );
        assert_eq!(
            sdk.adapters["https.configuration"].required_approvals,
            vec![
                McpSourceAdapterApproval::NetworkAccess,
                McpSourceAdapterApproval::CredentialReference,
            ]
        );
        assert_eq!(
            sdk.adapters["managed.catalog"].required_approvals,
            vec![McpSourceAdapterApproval::NetworkAccess]
        );
        assert_eq!(
            sdk.adapters["registry.npm"].required_approvals,
            vec![
                McpSourceAdapterApproval::NetworkAccess,
                McpSourceAdapterApproval::ProcessExecution,
                McpSourceAdapterApproval::HostDependency,
                McpSourceAdapterApproval::CredentialReference,
            ]
        );
        for id in ["registry.uvx", "runtime.docker"] {
            assert_eq!(
                sdk.adapters[id].required_approvals,
                vec![
                    McpSourceAdapterApproval::ProcessExecution,
                    McpSourceAdapterApproval::HostDependency,
                ]
            );
        }
        assert_eq!(
            sdk.adapters["source.git"].required_approvals,
            vec![
                McpSourceAdapterApproval::NetworkAccess,
                McpSourceAdapterApproval::ProcessExecution,
                McpSourceAdapterApproval::HostDependency,
            ]
        );
        let round_trip: McpSourceAdaptersListResult =
            serde_json::from_value(serde_json::to_value(&sdk).unwrap()).unwrap();
        assert_eq!(round_trip.adapters.keys().cloned().collect::<Vec<_>>(), ids);
    }

    #[test]
    fn sdk_conversion_keeps_visibility_separate_from_intake_authorization() {
        let safe = sdk_safe_descriptor("safe.catalog");
        let safe_sdk = safe.to_sdk_descriptor().unwrap();
        assert_eq!(
            safe_sdk.availability,
            McpSourceAdapterAvailability::ReadyForIntake {}
        );
        assert!(safe_sdk.required_approvals.is_empty());

        let mut blocked = safe.clone();
        blocked.id = "blocked.catalog".to_string();
        blocked.required_approvals = [SourceAdapterApproval::NetworkAccess].into_iter().collect();
        blocked.availability = SourceAdapterAvailability::Blocked(
            SourceAdapterBlockedReason::SafeProvisioningUnavailable,
        );
        let blocked_sdk = blocked.to_sdk_descriptor().unwrap();
        assert_eq!(
            blocked_sdk.availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Policy,
            }
        );
        assert_eq!(
            blocked_sdk.required_approvals,
            vec![McpSourceAdapterApproval::NetworkAccess]
        );

        let runtime_unavailable = SourceAdapterDescriptor {
            id: "runtime.npm".to_string(),
            display_name: "Runtime npm".to_string(),
            source_kind: SourceAdapterKind::Npm,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: [
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
            ]
            .into_iter()
            .collect(),
            required_host_dependencies: ["node".to_string()].into_iter().collect(),
            allowed_transports: [SourceAdapterTransport::StdioProvider]
                .into_iter()
                .collect(),
            input_fields: BTreeMap::new(),
            required_approvals: [
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
            ]
            .into_iter()
            .collect(),
            availability: SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            ),
        };
        let runtime_sdk = runtime_unavailable.to_sdk_descriptor().unwrap();
        assert_eq!(
            runtime_sdk.availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::RuntimeDependency,
            }
        );
        assert_eq!(
            runtime_sdk.required_approvals,
            vec![
                McpSourceAdapterApproval::ProcessExecution,
                McpSourceAdapterApproval::HostDependency,
            ]
        );

        let mut unverified = safe.clone();
        unverified.id = "unverified.catalog".to_string();
        unverified.trust_mode = SourceAdapterTrustMode::Unverified;
        unverified.availability = SourceAdapterAvailability::Blocked(
            SourceAdapterBlockedReason::SafeProvisioningUnavailable,
        );
        let unverified_sdk = unverified.to_sdk_descriptor().unwrap();
        assert!(matches!(
            unverified_sdk.source_kind,
            McpSourceAdapterSourceKind::Opaque { ref kind_id } if kind_id == "unverified"
        ));
        assert_eq!(
            unverified_sdk.availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unverified,
            }
        );
        assert!(unverified_sdk.required_approvals.is_empty());

        let round_trip: McpSourceAdaptersListResult = serde_json::from_value(
            serde_json::to_value(McpSourceAdaptersListResult {
                adapters: [("safe.catalog".to_string(), safe_sdk)]
                    .into_iter()
                    .collect(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            round_trip.adapters["safe.catalog"].availability,
            McpSourceAdapterAvailability::ReadyForIntake {}
        ));

        let registry = SourceAdapterRegistry::built_in();
        let sdk = registry.discover().to_sdk_result().unwrap();
        assert!(sdk.adapters.contains_key("approved.stdio"));
        let mut fully_approved_context = ready_context();
        fully_approved_context
            .approved_requirements
            .insert(SourceAdapterApproval::UserManagedSource);
        assert_eq!(
            registry.prepare_intake(
                SourceAdapterIntakeRequest {
                    adapter_id: "approved.stdio".to_string(),
                    transport: SourceAdapterTransport::StdioProvider,
                    fields: BTreeMap::new(),
                },
                &fully_approved_context,
            ),
            Err(SourceAdapterIntakeRejection::AdapterUnavailable)
        );
    }

    #[test]
    fn sdk_discovery_maps_valid_descriptors_and_rejects_sdk_semantic_invalidity() {
        let descriptor = sdk_safe_descriptor("safe.catalog");
        let sdk = SourceAdapterDiscovery {
            adapters: [(descriptor.id.clone(), descriptor.clone())]
                .into_iter()
                .collect(),
        }
        .to_sdk_result()
        .unwrap();
        assert_eq!(
            sdk.adapters["safe.catalog"].display_name,
            descriptor.display_name
        );
        assert_eq!(
            sdk.adapters["safe.catalog"].source_kind,
            McpSourceAdapterSourceKind::Catalog {}
        );

        let mut semantically_invalid = descriptor;
        semantically_invalid.risks.clear();
        assert!(matches!(
            SourceAdapterDiscovery {
                adapters: [(semantically_invalid.id.clone(), semantically_invalid,)]
                    .into_iter()
                    .collect(),
            }
            .to_sdk_result(),
            Err(SourceAdapterRegistryError::InvalidDescriptor)
        ));
    }

    #[test]
    fn duplicate_and_invalid_extensions_are_rejected() {
        let mut registry = SourceAdapterRegistry::built_in();
        let duplicate = registry.adapters["managed.catalog"]._adapter.clone();
        assert_eq!(
            registry.register(duplicate),
            Err(SourceAdapterRegistryError::DuplicateAdapterId)
        );
        assert_eq!(
            registry.register(Arc::new(TestAdapter(invalid_descriptor()))),
            Err(SourceAdapterRegistryError::InvalidDescriptor)
        );
    }

    #[test]
    fn opaque_extensions_stay_fail_closed() {
        let descriptor = SourceAdapterDescriptor {
            id: "future.registry".to_string(),
            display_name: "Extension secret label".to_string(),
            source_kind: SourceAdapterKind::Opaque {
                kind_id: "future.registry".to_string(),
            },
            trust_mode: SourceAdapterTrustMode::Unverified,
            risks: [
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
                SourceAdapterRisk::CredentialReference,
            ]
            .into_iter()
            .collect(),
            required_host_dependencies: ["attacker-host".to_string()].into_iter().collect(),
            allowed_transports: [SourceAdapterTransport::StdioProvider]
                .into_iter()
                .collect(),
            input_fields: [(
                "credential".to_string(),
                reference_field("Secret reference", true),
            )]
            .into_iter()
            .collect(),
            required_approvals: [
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
                SourceAdapterApproval::CredentialReference,
            ]
            .into_iter()
            .collect(),
            availability: SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::UnsupportedSourceKind,
            ),
        };
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register(Arc::new(TestAdapter(descriptor)))
            .unwrap();
        let discovery = registry.discover();
        assert!(matches!(
            discovery.adapters()["future.registry"].source_kind,
            SourceAdapterKind::Opaque { ref kind_id } if kind_id == "future.registry"
        ));
        assert_eq!(
            discovery.adapters()["future.registry"].availability,
            SourceAdapterAvailability::Blocked(SourceAdapterBlockedReason::UnsupportedSourceKind,)
        );
        let sdk = discovery.to_sdk_result().unwrap();
        let sdk_descriptor = &sdk.adapters["future.registry"];
        assert_eq!(sdk_descriptor.display_name, "Unverified source");
        assert!(matches!(
            sdk_descriptor.source_kind,
            McpSourceAdapterSourceKind::Opaque { ref kind_id } if kind_id == "unverified"
        ));
        assert_eq!(
            sdk_descriptor.trust_mode,
            McpSourceAdapterTrustMode::Unverified
        );
        assert_eq!(
            sdk_descriptor.availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unverified,
            }
        );
        assert!(sdk_descriptor.required_approvals.is_empty());
        assert_eq!(
            sdk_descriptor.risks,
            vec![McpSourceAdapterRisk::ProcessExecution]
        );
        assert!(sdk_descriptor.required_host_dependencies.is_empty());
        assert!(sdk_descriptor.allowed_transports.is_empty());
        assert!(sdk_descriptor.input_fields.is_empty());
        let public_json = serde_json::to_string(sdk_descriptor).unwrap();
        for leaked_value in [
            "Extension secret label",
            "Secret reference",
            "attacker-host",
        ] {
            assert!(
                !public_json.contains(leaked_value),
                "quarantine leaked {leaked_value}"
            );
        }
        let round_trip: McpSourceAdapterDescriptor = serde_json::from_str(&public_json).unwrap();
        assert_eq!(round_trip.display_name, "Unverified source");
        assert_eq!(
            registry.prepare_intake(request("future.registry"), &ready_context()),
            Err(SourceAdapterIntakeRejection::AdapterUnavailable)
        );
    }

    #[test]
    fn extensions_cannot_self_attest_verified_trust() {
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register(Arc::new(TestAdapter(ready_npm_descriptor())))
            .unwrap();

        let discovery = registry.discover();
        let descriptor = &discovery.adapters()["test.npm"];
        assert_eq!(descriptor.trust_mode, SourceAdapterTrustMode::Unverified);
        assert_eq!(
            descriptor.availability,
            SourceAdapterAvailability::Blocked(
                SourceAdapterBlockedReason::SafeProvisioningUnavailable,
            )
        );
        let sdk = discovery.to_sdk_result().unwrap();
        let sdk_descriptor = &sdk.adapters["test.npm"];
        assert!(matches!(
            sdk_descriptor.source_kind,
            McpSourceAdapterSourceKind::Opaque { ref kind_id } if kind_id == "unverified"
        ));
        assert_eq!(
            sdk_descriptor.availability,
            McpSourceAdapterAvailability::Blocked {
                reason: McpSourceAdapterBlockedReason::Unverified,
            }
        );
        assert_eq!(
            registry.prepare_intake(
                SourceAdapterIntakeRequest {
                    adapter_id: "test.npm".to_string(),
                    transport: SourceAdapterTransport::StdioProvider,
                    fields: [(
                        "package".to_string(),
                        SourceAdapterInputValue::Text("example".to_string())
                    )]
                    .into_iter()
                    .collect(),
                },
                &ready_context(),
            ),
            Err(SourceAdapterIntakeRejection::AdapterUnavailable)
        );
    }

    #[test]
    fn intake_requires_exact_approvals_and_preserves_policy_checks() {
        let descriptor = ready_npm_descriptor();
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register_built_in(Arc::new(TestAdapter(descriptor)))
            .unwrap();
        let request = SourceAdapterIntakeRequest {
            adapter_id: "test.npm".to_string(),
            transport: SourceAdapterTransport::StdioProvider,
            fields: [(
                "package".to_string(),
                SourceAdapterInputValue::Text("example".to_string()),
            )]
            .into_iter()
            .collect(),
        };
        let mut context = SourceAdapterIntakeContext::discovery_default();
        context
            .available_host_dependencies
            .insert("node".to_string());
        assert_eq!(
            registry.prepare_intake(request.clone(), &context),
            Err(SourceAdapterIntakeRejection::PolicyDenied)
        );
        context.allowed_risks.extend([
            SourceAdapterRisk::ProcessExecution,
            SourceAdapterRisk::HostDependency,
        ]);
        assert_eq!(
            registry.prepare_intake(request.clone(), &context),
            Err(SourceAdapterIntakeRejection::ApprovalRequired)
        );
        context
            .approved_requirements
            .insert(SourceAdapterApproval::FilesystemAccess);
        assert_eq!(
            registry.prepare_intake(request.clone(), &context),
            Err(SourceAdapterIntakeRejection::ApprovalRequired)
        );
        context.approved_requirements.extend([
            SourceAdapterApproval::NetworkAccess,
            SourceAdapterApproval::ProcessExecution,
            SourceAdapterApproval::HostDependency,
            SourceAdapterApproval::CredentialReference,
        ]);
        assert!(registry.prepare_intake(request.clone(), &context).is_ok());
        context.available_host_dependencies.clear();
        assert_eq!(
            registry.prepare_intake(request, &context),
            Err(SourceAdapterIntakeRejection::HostDependencyUnavailable)
        );
    }

    #[test]
    fn process_execution_cannot_be_bound_to_an_unrelated_approval() {
        let mut descriptor = ready_npm_descriptor();
        descriptor.required_approvals = [SourceAdapterApproval::FilesystemAccess]
            .into_iter()
            .collect();
        assert_eq!(
            SourceAdapterRegistry::default().register_built_in(Arc::new(TestAdapter(descriptor))),
            Err(SourceAdapterRegistryError::InvalidDescriptor)
        );
    }

    #[test]
    fn https_configuration_requires_a_reference_not_raw_credential_text() {
        let registry = SourceAdapterRegistry::built_in();
        let context = SourceAdapterIntakeContext {
            allowed_risks: [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::CredentialReference,
            ]
            .into_iter()
            .collect(),
            approved_requirements: [
                SourceAdapterApproval::UserManagedSource,
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::CredentialReference,
            ]
            .into_iter()
            .collect(),
            allow_user_managed_sources: true,
            ..SourceAdapterIntakeContext::default()
        };
        let raw_credential = SourceAdapterIntakeRequest {
            adapter_id: "https.configuration".to_string(),
            transport: SourceAdapterTransport::RemoteHttp,
            fields: [(
                "configuration".to_string(),
                SourceAdapterInputValue::Text("any-raw-credential-value".to_string()),
            )]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            registry.prepare_intake(raw_credential, &context),
            Err(SourceAdapterIntakeRejection::SecretReferenceRequired)
        );

        let reference = SourceAdapterIntakeRequest {
            adapter_id: "https.configuration".to_string(),
            transport: SourceAdapterTransport::RemoteHttp,
            fields: [(
                "configuration".to_string(),
                SourceAdapterInputValue::Reference(
                    SourceAdapterReference::new("credential-ref".to_string()).unwrap(),
                ),
            )]
            .into_iter()
            .collect(),
        };
        assert!(registry.prepare_intake(reference, &context).is_ok());
    }

    #[test]
    fn directory_and_text_inputs_reject_control_characters() {
        let descriptor = SourceAdapterDescriptor {
            id: "safe.directory".to_string(),
            display_name: "Safe directory".to_string(),
            source_kind: SourceAdapterKind::Catalog,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: BTreeSet::new(),
            required_host_dependencies: BTreeSet::new(),
            allowed_transports: [SourceAdapterTransport::Catalog].into_iter().collect(),
            input_fields: [
                (
                    "directory".to_string(),
                    SourceAdapterInputField {
                        label: "Directory".to_string(),
                        kind: SourceAdapterInputKind::Directory,
                        required: true,
                        multiline: false,
                        secret_reference_only: false,
                    },
                ),
                ("label".to_string(), text_field("Label", true)),
            ]
            .into_iter()
            .collect(),
            required_approvals: BTreeSet::new(),
            availability: SourceAdapterAvailability::ReadyForIntake,
        };
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register_built_in(Arc::new(TestAdapter(descriptor)))
            .unwrap();
        let request = SourceAdapterIntakeRequest {
            adapter_id: "safe.directory".to_string(),
            transport: SourceAdapterTransport::Catalog,
            fields: [
                (
                    "directory".to_string(),
                    SourceAdapterInputValue::Directory("C:\\safe\r\nnext".to_string()),
                ),
                (
                    "label".to_string(),
                    SourceAdapterInputValue::Text("safe".to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            registry.prepare_intake(request, &SourceAdapterIntakeContext::default()),
            Err(SourceAdapterIntakeRejection::InvalidInput)
        );

        let text_with_newline = SourceAdapterIntakeRequest {
            adapter_id: "safe.directory".to_string(),
            transport: SourceAdapterTransport::Catalog,
            fields: [
                (
                    "directory".to_string(),
                    SourceAdapterInputValue::Directory("C:\\safe".to_string()),
                ),
                (
                    "label".to_string(),
                    SourceAdapterInputValue::Text("safe\r\nnext".to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        };
        assert_eq!(
            registry.prepare_intake(text_with_newline, &SourceAdapterIntakeContext::default()),
            Err(SourceAdapterIntakeRejection::InvalidInput)
        );
    }

    #[test]
    fn preparation_has_no_adapter_effects() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let descriptor_calls = Arc::new(AtomicUsize::new(0));
        let descriptor = SourceAdapterDescriptor {
            id: "safe.remote".to_string(),
            display_name: "Safe remote".to_string(),
            source_kind: SourceAdapterKind::RemoteHttp,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: [SourceAdapterRisk::NetworkAccess].into_iter().collect(),
            required_host_dependencies: BTreeSet::new(),
            allowed_transports: [SourceAdapterTransport::RemoteHttp].into_iter().collect(),
            input_fields: BTreeMap::new(),
            required_approvals: [SourceAdapterApproval::NetworkAccess].into_iter().collect(),
            availability: SourceAdapterAvailability::ReadyForIntake,
        };
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register_built_in(Arc::new(CountingAdapter {
                descriptor,
                descriptor_calls: descriptor_calls.clone(),
            }))
            .unwrap();
        descriptor_calls.store(0, Ordering::SeqCst);
        let result = registry.prepare_intake(request("safe.remote"), &ready_context());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().adapter_id, "safe.remote");
        assert_eq!(descriptor_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn sdk_discovery_has_no_adapter_effects() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let descriptor_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = SourceAdapterRegistry::default();
        registry
            .register_built_in(Arc::new(CountingAdapter {
                descriptor: ready_npm_descriptor(),
                descriptor_calls: descriptor_calls.clone(),
            }))
            .unwrap();
        descriptor_calls.store(0, Ordering::SeqCst);

        let sdk = registry.discover().to_sdk_result().unwrap();

        assert!(sdk.adapters.contains_key("test.npm"));
        assert_eq!(descriptor_calls.load(Ordering::SeqCst), 0);
    }

    #[derive(Clone)]
    struct TestAdapter(SourceAdapterDescriptor);

    impl McpSourceAdapter for TestAdapter {
        fn descriptor(&self) -> SourceAdapterDescriptor {
            self.0.clone()
        }
    }

    struct CountingAdapter {
        descriptor: SourceAdapterDescriptor,
        descriptor_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl McpSourceAdapter for CountingAdapter {
        fn descriptor(&self) -> SourceAdapterDescriptor {
            self.descriptor_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.descriptor.clone()
        }
    }

    fn request(adapter_id: &str) -> SourceAdapterIntakeRequest {
        SourceAdapterIntakeRequest {
            adapter_id: adapter_id.to_string(),
            transport: SourceAdapterTransport::RemoteHttp,
            fields: BTreeMap::new(),
        }
    }

    fn ready_context() -> SourceAdapterIntakeContext {
        SourceAdapterIntakeContext {
            available_host_dependencies: ["node".to_string()].into_iter().collect(),
            allowed_risks: [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
                SourceAdapterRisk::CredentialReference,
            ]
            .into_iter()
            .collect(),
            approved_requirements: [
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
                SourceAdapterApproval::CredentialReference,
            ]
            .into_iter()
            .collect(),
            allow_user_managed_sources: true,
        }
    }

    fn ready_npm_descriptor() -> SourceAdapterDescriptor {
        SourceAdapterDescriptor {
            id: "test.npm".to_string(),
            display_name: "Test npm".to_string(),
            source_kind: SourceAdapterKind::Npm,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: [
                SourceAdapterRisk::NetworkAccess,
                SourceAdapterRisk::ProcessExecution,
                SourceAdapterRisk::HostDependency,
                SourceAdapterRisk::CredentialReference,
            ]
            .into_iter()
            .collect(),
            required_host_dependencies: ["node".to_string()].into_iter().collect(),
            allowed_transports: [SourceAdapterTransport::StdioProvider]
                .into_iter()
                .collect(),
            input_fields: [
                ("package".to_string(), text_field("Package", true)),
                (
                    "credential".to_string(),
                    reference_field("Credential reference", false),
                ),
            ]
            .into_iter()
            .collect(),
            required_approvals: [
                SourceAdapterApproval::NetworkAccess,
                SourceAdapterApproval::ProcessExecution,
                SourceAdapterApproval::HostDependency,
                SourceAdapterApproval::CredentialReference,
            ]
            .into_iter()
            .collect(),
            availability: SourceAdapterAvailability::ReadyForIntake,
        }
    }

    fn sdk_safe_descriptor(id: &str) -> SourceAdapterDescriptor {
        SourceAdapterDescriptor {
            id: id.to_string(),
            display_name: "Safe catalog".to_string(),
            source_kind: SourceAdapterKind::Catalog,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: [SourceAdapterRisk::NetworkAccess].into_iter().collect(),
            required_host_dependencies: BTreeSet::new(),
            allowed_transports: [SourceAdapterTransport::Catalog].into_iter().collect(),
            input_fields: BTreeMap::new(),
            required_approvals: BTreeSet::new(),
            availability: SourceAdapterAvailability::ReadyForIntake,
        }
    }

    fn invalid_descriptor() -> SourceAdapterDescriptor {
        SourceAdapterDescriptor {
            id: "Bad id".to_string(),
            display_name: "Bad".to_string(),
            source_kind: SourceAdapterKind::Catalog,
            trust_mode: SourceAdapterTrustMode::Verified,
            risks: [SourceAdapterRisk::NetworkAccess].into_iter().collect(),
            required_host_dependencies: BTreeSet::new(),
            allowed_transports: [SourceAdapterTransport::Catalog].into_iter().collect(),
            input_fields: BTreeMap::new(),
            required_approvals: BTreeSet::new(),
            availability: SourceAdapterAvailability::ReadyForIntake,
        }
    }
}
