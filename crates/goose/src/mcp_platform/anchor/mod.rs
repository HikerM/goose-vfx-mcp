use std::sync::{Arc, Mutex};

#[cfg(feature = "system-keyring")]
use base64::Engine;
#[cfg(feature = "system-keyring")]
use rand::RngExt;
#[cfg(feature = "system-keyring")]
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mcp_platform::error::McpPlatformResult;

use super::{
    constant_time_eq, hmac_sha256, integrity_envelope, integrity_recovery_required,
    integrity_unavailable_with_reason, integrity_unsupported, KEY_BYTES,
};

pub(crate) const SYSTEM_KEYRING_PROVIDER_ID: &str = "system-keyring";
pub(crate) const IN_MEMORY_TEST_PROVIDER_ID: &str = "in-memory-test";
pub(crate) const DEFAULT_TEST_PATH_BINDING: &str = "in-memory-test-database";
const UNSUPPORTED_PROVIDER_ID: &str = "unsupported";
#[cfg(feature = "system-keyring")]
const STORED_ANCHOR_PROVIDER_BOUND_V2_FORMAT_VERSION: u8 = 2;
#[cfg(feature = "system-keyring")]
const STORED_ANCHOR_SCHEMA_FENCED_V3_FORMAT_VERSION: u8 = 3;
const STORED_SCHEMA_FENCE_PROJECTION_MUTATIONS_WITNESS_V2_V15: &str =
    "projection_mutations_witness_v2_v15";

const SYSTEM_KEYRING_UNAVAILABLE_REASON: &str =
    "managed MCP system keyring integrity anchor is unavailable";
const PATHLESS_PROVIDER_UNAVAILABLE_REASON: &str =
    "managed MCP integrity anchor is unavailable for pathless SQLite repositories";
const UNSUPPORTED_PROVIDER_REASON: &str =
    "configured managed MCP integrity anchor provider is unsupported";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorIdentity {
    pub instance_id: String,
    pub path_binding: String,
    pub key_epoch: u64,
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorCheckpoint {
    pub identity: AnchorIdentity,
    pub sequence: u64,
    pub root: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnchorProviderAssurance {
    SystemKeyring,
    TestInMemory,
    Unavailable,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistedProviderId {
    SystemKeyring,
    InMemoryTest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchemaCompatibilityFencePhase {
    Pending,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SchemaCompatibilityFence {
    pub minimum_schema_version: i64,
    pub projection_mutations_shape: String,
    pub phase: SchemaCompatibilityFencePhase,
}

impl SchemaCompatibilityFence {
    pub(crate) fn projection_witness_v2_v15() -> Self {
        Self::projection_witness_v2_v15_active()
    }

    pub(crate) fn projection_witness_v2_v15_pending() -> Self {
        Self {
            minimum_schema_version: 15,
            projection_mutations_shape: STORED_SCHEMA_FENCE_PROJECTION_MUTATIONS_WITNESS_V2_V15
                .to_string(),
            phase: SchemaCompatibilityFencePhase::Pending,
        }
    }

    pub(crate) fn projection_witness_v2_v15_active() -> Self {
        Self {
            minimum_schema_version: 15,
            projection_mutations_shape: STORED_SCHEMA_FENCE_PROJECTION_MUTATIONS_WITNESS_V2_V15
                .to_string(),
            phase: SchemaCompatibilityFencePhase::Active,
        }
    }
}

impl PersistedProviderId {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SystemKeyring => SYSTEM_KEYRING_PROVIDER_ID,
            Self::InMemoryTest => IN_MEMORY_TEST_PROVIDER_ID,
        }
    }
}

pub(crate) fn parse_persisted_provider_id(provider_id: &str) -> Option<PersistedProviderId> {
    match provider_id {
        SYSTEM_KEYRING_PROVIDER_ID => Some(PersistedProviderId::SystemKeyring),
        IN_MEMORY_TEST_PROVIDER_ID => Some(PersistedProviderId::InMemoryTest),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnchorProviderHealth {
    Ready,
    Unavailable { reason: &'static str },
    Unsupported { reason: &'static str },
}

impl AnchorProviderHealth {
    pub(crate) fn availability_error(
        &self,
    ) -> Option<crate::mcp_platform::error::McpPlatformError> {
        match self {
            Self::Ready => None,
            Self::Unavailable { reason } => Some(integrity_unavailable_with_reason(reason)),
            Self::Unsupported { reason } => Some(integrity_unsupported(reason)),
        }
    }
}

pub(crate) trait AnchorProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn assurance(&self) -> AnchorProviderAssurance;
    fn health(&self) -> AnchorProviderHealth;
    fn identity(&self) -> McpPlatformResult<AnchorIdentity>;
    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>>;
    fn publish(
        &self,
        expected: Option<&AnchorCheckpoint>,
        next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()>;
    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String>;
    fn needs_persisted_provider_migration(&self) -> McpPlatformResult<bool> {
        Ok(false)
    }
    fn migrate_persisted_provider_binding(
        &self,
        expected: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        let _ = expected;
        Ok(())
    }
    fn stored_schema_compatibility_fence(
        &self,
    ) -> McpPlatformResult<Option<SchemaCompatibilityFence>> {
        Ok(None)
    }
    fn ensure_schema_compatibility_fence(
        &self,
        expected: &AnchorCheckpoint,
        fence: &SchemaCompatibilityFence,
    ) -> McpPlatformResult<()> {
        let _ = (expected, fence);
        Ok(())
    }

    fn verify(
        &self,
        domain: &str,
        canonical_payload: &[u8],
        expected_mac: &str,
    ) -> McpPlatformResult<()> {
        let actual = self.sign(domain, canonical_payload)?;
        if !constant_time_eq(actual.as_bytes(), expected_mac.as_bytes()) {
            return Err(super::integrity_error());
        }
        Ok(())
    }

    fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
        actual == expected
    }
}

impl<T> AnchorProvider for Arc<T>
where
    T: AnchorProvider + ?Sized,
{
    fn provider_id(&self) -> &str {
        self.as_ref().provider_id()
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        self.as_ref().assurance()
    }

    fn health(&self) -> AnchorProviderHealth {
        self.as_ref().health()
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        self.as_ref().identity()
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        self.as_ref().checkpoint()
    }

    fn publish(
        &self,
        expected: Option<&AnchorCheckpoint>,
        next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        self.as_ref().publish(expected, next)
    }

    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
        self.as_ref().sign(domain, canonical_payload)
    }

    fn needs_persisted_provider_migration(&self) -> McpPlatformResult<bool> {
        self.as_ref().needs_persisted_provider_migration()
    }

    fn migrate_persisted_provider_binding(
        &self,
        expected: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        self.as_ref().migrate_persisted_provider_binding(expected)
    }

    fn stored_schema_compatibility_fence(
        &self,
    ) -> McpPlatformResult<Option<SchemaCompatibilityFence>> {
        self.as_ref().stored_schema_compatibility_fence()
    }

    fn ensure_schema_compatibility_fence(
        &self,
        expected: &AnchorCheckpoint,
        fence: &SchemaCompatibilityFence,
    ) -> McpPlatformResult<()> {
        self.as_ref()
            .ensure_schema_compatibility_fence(expected, fence)
    }

    fn verify(
        &self,
        domain: &str,
        canonical_payload: &[u8],
        expected_mac: &str,
    ) -> McpPlatformResult<()> {
        self.as_ref()
            .verify(domain, canonical_payload, expected_mac)
    }

    fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
        self.as_ref()
            .matches_expected_path_binding(expected, actual)
    }
}

pub(crate) struct AnchorProviderRegistry;

impl AnchorProviderRegistry {
    pub(crate) fn production_default(
        path_binding: &str,
        allow_create: bool,
    ) -> Arc<dyn super::IntegritySigner> {
        #[cfg(feature = "system-keyring")]
        match SystemKeyringAnchorProvider::new(path_binding, allow_create) {
            Ok(provider) => Arc::new(provider),
            Err(SystemKeyringProviderInitError::Unavailable) => {
                Arc::new(FailClosedAnchorProvider::unavailable(
                    SYSTEM_KEYRING_PROVIDER_ID,
                    AnchorProviderAssurance::SystemKeyring,
                    SYSTEM_KEYRING_UNAVAILABLE_REASON,
                ))
            }
            Err(SystemKeyringProviderInitError::Corrupt) => {
                Arc::new(CorruptPersistedAnchorProvider)
            }
        }

        #[cfg(not(feature = "system-keyring"))]
        {
            let _ = (path_binding, allow_create);
            Arc::new(FailClosedAnchorProvider::unavailable(
                SYSTEM_KEYRING_PROVIDER_ID,
                AnchorProviderAssurance::SystemKeyring,
                SYSTEM_KEYRING_UNAVAILABLE_REASON,
            ))
        }
    }

    pub(crate) fn in_memory_for_testing(
        key: [u8; KEY_BYTES],
        path_binding: impl Into<String>,
    ) -> Arc<dyn super::IntegritySigner> {
        Arc::new(InMemoryAnchorProvider {
            key,
            identity: AnchorIdentity {
                instance_id: Uuid::new_v4().to_string(),
                path_binding: path_binding.into(),
                key_epoch: 1,
                provider_id: IN_MEMORY_TEST_PROVIDER_ID.to_string(),
            },
            checkpoint: Mutex::new(None),
        })
    }

    pub(crate) fn unavailable() -> Arc<dyn super::IntegritySigner> {
        Arc::new(FailClosedAnchorProvider::unavailable(
            "unavailable",
            AnchorProviderAssurance::Unavailable,
            PATHLESS_PROVIDER_UNAVAILABLE_REASON,
        ))
    }

    pub(crate) fn unsupported(provider_id: impl Into<String>) -> Arc<dyn super::IntegritySigner> {
        let _ = provider_id.into();
        Arc::new(FailClosedAnchorProvider::unsupported())
    }

    #[cfg(feature = "system-keyring")]
    pub(crate) fn clear_system_anchor_for_trusted_reenrollment(
        path_binding: &str,
    ) -> McpPlatformResult<()> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(integrity_unavailable_with_reason(
                SYSTEM_KEYRING_UNAVAILABLE_REASON,
            )),
        }
    }

    #[cfg(not(feature = "system-keyring"))]
    pub(crate) fn clear_system_anchor_for_trusted_reenrollment(
        _path_binding: &str,
    ) -> McpPlatformResult<()> {
        Err(integrity_unavailable_with_reason(
            SYSTEM_KEYRING_UNAVAILABLE_REASON,
        ))
    }

    #[cfg(all(test, feature = "system-keyring"))]
    pub(crate) fn rewrite_system_anchor_as_legacy_for_testing(
        path_binding: &str,
    ) -> McpPlatformResult<()> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let encoded = entry
            .get_password()
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let stored = decode_stored_anchor(&encoded).map_err(|_| integrity_recovery_required())?;
        entry
            .set_password(&encode_stored_anchor_legacy(&stored).map_err(|_| {
                integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON)
            })?)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))
    }

    #[cfg(all(test, feature = "system-keyring"))]
    pub(crate) fn write_raw_system_anchor_for_testing(
        path_binding: &str,
        payload: &str,
    ) -> McpPlatformResult<()> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        entry
            .set_password(payload)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))
    }

    #[cfg(all(test, feature = "system-keyring"))]
    pub(crate) fn read_raw_system_anchor_for_testing(
        path_binding: &str,
    ) -> McpPlatformResult<String> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        entry
            .get_password()
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))
    }

    #[cfg(all(test, feature = "system-keyring"))]
    pub(crate) fn overwrite_system_anchor_provider_id_for_testing(
        path_binding: &str,
        provider_id: &str,
    ) -> McpPlatformResult<()> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let encoded = entry
            .get_password()
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let mut stored =
            decode_stored_anchor(&encoded).map_err(|_| integrity_recovery_required())?;
        stored.checkpoint.identity.provider_id = provider_id.to_string();
        entry
            .set_password(&encode_stored_anchor_provider_bound(&stored).map_err(|_| {
                integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON)
            })?)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))
    }
}

struct InMemoryAnchorProvider {
    key: [u8; KEY_BYTES],
    identity: AnchorIdentity,
    checkpoint: Mutex<Option<AnchorCheckpoint>>,
}

impl AnchorProvider for InMemoryAnchorProvider {
    fn provider_id(&self) -> &str {
        IN_MEMORY_TEST_PROVIDER_ID
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        AnchorProviderAssurance::TestInMemory
    }

    fn health(&self) -> AnchorProviderHealth {
        AnchorProviderHealth::Ready
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        Ok(self.identity.clone())
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        self.checkpoint
            .lock()
            .map(|checkpoint| checkpoint.clone())
            .map_err(|_| integrity_unavailable_with_reason(PATHLESS_PROVIDER_UNAVAILABLE_REASON))
    }

    fn publish(
        &self,
        expected: Option<&AnchorCheckpoint>,
        next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        let mut checkpoint = self
            .checkpoint
            .lock()
            .map_err(|_| integrity_unavailable_with_reason(PATHLESS_PROVIDER_UNAVAILABLE_REASON))?;
        if checkpoint.as_ref() != expected || !is_monotonic_publish(expected, next, &self.identity)
        {
            return Err(integrity_recovery_required());
        }
        *checkpoint = Some(next.clone());
        Ok(())
    }

    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
        Ok(crate::utils::bytes_to_hex(hmac_sha256(
            &self.key,
            &integrity_envelope(domain, canonical_payload),
        )))
    }

    fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
        actual == expected || actual == DEFAULT_TEST_PATH_BINDING
    }
}

struct FailClosedAnchorProvider {
    provider_id: String,
    assurance: AnchorProviderAssurance,
    health: AnchorProviderHealth,
}

impl FailClosedAnchorProvider {
    fn unavailable(
        provider_id: impl Into<String>,
        assurance: AnchorProviderAssurance,
        reason: &'static str,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            assurance,
            health: AnchorProviderHealth::Unavailable { reason },
        }
    }

    fn unsupported() -> Self {
        Self {
            provider_id: UNSUPPORTED_PROVIDER_ID.to_string(),
            assurance: AnchorProviderAssurance::Unsupported,
            health: AnchorProviderHealth::Unsupported {
                reason: UNSUPPORTED_PROVIDER_REASON,
            },
        }
    }

    fn error(&self) -> crate::mcp_platform::error::McpPlatformError {
        self.health
            .availability_error()
            .expect("fail-closed providers are never healthy")
    }
}

struct CorruptPersistedAnchorProvider;

impl CorruptPersistedAnchorProvider {
    fn error(&self) -> crate::mcp_platform::error::McpPlatformError {
        integrity_recovery_required()
    }
}

#[cfg(feature = "system-keyring")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoredAnchorEncoding {
    Legacy,
    ProviderBoundV2,
    SchemaFencedV3,
}

#[cfg(feature = "system-keyring")]
#[derive(Clone)]
struct StoredAnchor {
    checkpoint: AnchorCheckpoint,
    key: String,
    encoding: StoredAnchorEncoding,
    schema_fence: Option<SchemaCompatibilityFence>,
}

#[cfg(feature = "system-keyring")]
impl StoredAnchor {
    fn provider_bound(checkpoint: AnchorCheckpoint, key: String) -> Self {
        Self {
            checkpoint,
            key,
            encoding: StoredAnchorEncoding::ProviderBoundV2,
            schema_fence: None,
        }
    }

    fn schema_fenced(
        checkpoint: AnchorCheckpoint,
        key: String,
        schema_fence: SchemaCompatibilityFence,
    ) -> Self {
        Self {
            checkpoint,
            key,
            encoding: StoredAnchorEncoding::SchemaFencedV3,
            schema_fence: Some(schema_fence),
        }
    }

    fn needs_provider_migration(&self) -> bool {
        matches!(self.encoding, StoredAnchorEncoding::Legacy)
    }
}

impl AnchorProvider for FailClosedAnchorProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        self.assurance
    }

    fn health(&self) -> AnchorProviderHealth {
        self.health.clone()
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        Err(self.error())
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        Err(self.error())
    }

    fn publish(
        &self,
        _expected: Option<&AnchorCheckpoint>,
        _next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        Err(self.error())
    }

    fn sign(&self, _domain: &str, _canonical_payload: &[u8]) -> McpPlatformResult<String> {
        Err(self.error())
    }
}

impl AnchorProvider for CorruptPersistedAnchorProvider {
    fn provider_id(&self) -> &str {
        SYSTEM_KEYRING_PROVIDER_ID
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        AnchorProviderAssurance::SystemKeyring
    }

    fn health(&self) -> AnchorProviderHealth {
        AnchorProviderHealth::Ready
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        Err(self.error())
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        Err(self.error())
    }

    fn publish(
        &self,
        _expected: Option<&AnchorCheckpoint>,
        _next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        Err(self.error())
    }

    fn sign(&self, _domain: &str, _canonical_payload: &[u8]) -> McpPlatformResult<String> {
        Err(self.error())
    }

    fn needs_persisted_provider_migration(&self) -> McpPlatformResult<bool> {
        Err(self.error())
    }

    fn migrate_persisted_provider_binding(
        &self,
        _expected: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        Err(self.error())
    }

    fn stored_schema_compatibility_fence(
        &self,
    ) -> McpPlatformResult<Option<SchemaCompatibilityFence>> {
        Err(self.error())
    }

    fn ensure_schema_compatibility_fence(
        &self,
        _expected: &AnchorCheckpoint,
        _fence: &SchemaCompatibilityFence,
    ) -> McpPlatformResult<()> {
        Err(self.error())
    }
}

#[cfg(feature = "system-keyring")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAnchorLegacyRecord {
    checkpoint: LegacyAnchorCheckpoint,
    key: String,
}

#[cfg(feature = "system-keyring")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAnchorCheckpoint {
    identity: LegacyAnchorIdentity,
    sequence: u64,
    root: String,
}

#[cfg(feature = "system-keyring")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAnchorIdentity {
    instance_id: String,
    path_binding: String,
    key_epoch: u64,
}

#[cfg(feature = "system-keyring")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAnchorProviderBoundRecord {
    format_version: u8,
    checkpoint: StoredAnchorProviderBoundCheckpoint,
    key: String,
}

#[cfg(feature = "system-keyring")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAnchorProviderBoundCheckpoint {
    identity: StoredAnchorProviderBoundIdentity,
    sequence: u64,
    root: String,
}

#[cfg(feature = "system-keyring")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAnchorProviderBoundIdentity {
    instance_id: String,
    path_binding: String,
    key_epoch: u64,
    provider_id: String,
}

#[cfg(feature = "system-keyring")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAnchorSchemaFencedRecord {
    format_version: u8,
    checkpoint: StoredAnchorProviderBoundCheckpoint,
    key: String,
    schema_fence: StoredSchemaCompatibilityFenceRecord,
}

#[cfg(feature = "system-keyring")]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredSchemaCompatibilityFenceRecord {
    minimum_schema_version: i64,
    projection_mutations_shape: String,
    phase: SchemaCompatibilityFencePhase,
}

#[cfg(feature = "system-keyring")]
#[derive(Clone, Debug)]
enum StrictJsonValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<StrictJsonValue>),
    Object(Vec<(String, StrictJsonValue)>),
}

#[cfg(feature = "system-keyring")]
impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrictJsonValueVisitor;

        impl<'de> Visitor<'de> for StrictJsonValueVisitor {
            type Value = StrictJsonValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("valid JSON without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictJsonValue::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictJsonValue::Number(serde_json::Number::from(value)))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictJsonValue::Number(serde_json::Number::from(value)))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let number = serde_json::Number::from_f64(value)
                    .ok_or_else(|| E::custom("invalid JSON number"))?;
                Ok(StrictJsonValue::Number(number))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictJsonValue::String(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictJsonValue::String(value))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictJsonValue::Null)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictJsonValue::Null)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictJsonValue>()? {
                    values.push(value);
                }
                Ok(StrictJsonValue::Array(values))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = std::collections::HashSet::new();
                let mut values = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, StrictJsonValue>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom(format!(
                            "duplicate JSON object key `{key}`"
                        )));
                    }
                    values.push((key, value));
                }
                Ok(StrictJsonValue::Object(values))
            }
        }

        deserializer.deserialize_any(StrictJsonValueVisitor)
    }
}

#[cfg(feature = "system-keyring")]
impl From<StrictJsonValue> for serde_json::Value {
    fn from(value: StrictJsonValue) -> Self {
        match value {
            StrictJsonValue::Null => serde_json::Value::Null,
            StrictJsonValue::Bool(value) => serde_json::Value::Bool(value),
            StrictJsonValue::Number(value) => serde_json::Value::Number(value),
            StrictJsonValue::String(value) => serde_json::Value::String(value),
            StrictJsonValue::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(serde_json::Value::from).collect())
            }
            StrictJsonValue::Object(entries) => {
                let mut object = serde_json::Map::with_capacity(entries.len());
                for (key, value) in entries {
                    object.insert(key, serde_json::Value::from(value));
                }
                serde_json::Value::Object(object)
            }
        }
    }
}

#[cfg(all(test, feature = "system-keyring"))]
#[derive(Serialize)]
struct StoredAnchorLegacyWriteRecord {
    checkpoint: LegacyAnchorWriteCheckpoint,
    key: String,
}

#[cfg(all(test, feature = "system-keyring"))]
#[derive(Serialize)]
struct LegacyAnchorWriteCheckpoint {
    identity: LegacyAnchorWriteIdentity,
    sequence: u64,
    root: String,
}

#[cfg(all(test, feature = "system-keyring"))]
#[derive(Serialize)]
struct LegacyAnchorWriteIdentity {
    instance_id: String,
    path_binding: String,
    key_epoch: u64,
}

#[cfg(feature = "system-keyring")]
#[derive(Clone, Copy)]
enum SystemKeyringProviderInitError {
    Unavailable,
    Corrupt,
}

#[cfg(feature = "system-keyring")]
struct SystemKeyringAnchorProvider {
    account: String,
    candidate: StoredAnchor,
}

#[cfg(feature = "system-keyring")]
impl SystemKeyringAnchorProvider {
    fn new(path_binding: &str, allow_create: bool) -> Result<Self, SystemKeyringProviderInitError> {
        let account = system_account(path_binding);
        let entry = keyring::Entry::new(super::KEYRING_SERVICE, &account)
            .map_err(|_| SystemKeyringProviderInitError::Unavailable)?;
        let candidate = match entry.get_password() {
            Ok(encoded) => decode_stored_anchor(&encoded)
                .map_err(|_| SystemKeyringProviderInitError::Corrupt)?,
            Err(keyring::Error::NoEntry) if allow_create => {
                let mut key = [0_u8; KEY_BYTES];
                rand::rng().fill(&mut key);
                StoredAnchor::provider_bound(
                    AnchorCheckpoint {
                        identity: AnchorIdentity {
                            instance_id: Uuid::new_v4().to_string(),
                            path_binding: path_binding.to_string(),
                            key_epoch: 1,
                            provider_id: SYSTEM_KEYRING_PROVIDER_ID.to_string(),
                        },
                        sequence: 0,
                        root: String::new(),
                    },
                    base64::engine::general_purpose::STANDARD_NO_PAD.encode(key),
                )
            }
            Err(_) => return Err(SystemKeyringProviderInitError::Unavailable),
        };
        if candidate.checkpoint.identity.provider_id != SYSTEM_KEYRING_PROVIDER_ID {
            return Err(SystemKeyringProviderInitError::Corrupt);
        }
        if candidate.checkpoint.identity.path_binding != path_binding {
            return Err(SystemKeyringProviderInitError::Corrupt);
        }
        decode_key(&candidate.key).map_err(|_| SystemKeyringProviderInitError::Corrupt)?;
        Ok(Self { account, candidate })
    }

    fn entry(&self) -> McpPlatformResult<keyring::Entry> {
        keyring::Entry::new(super::KEYRING_SERVICE, &self.account)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))
    }

    fn load(&self) -> McpPlatformResult<Option<StoredAnchor>> {
        match self.entry()?.get_password() {
            Ok(encoded) => decode_stored_anchor(&encoded)
                .map(Some)
                .map_err(|_| integrity_recovery_required()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(integrity_unavailable_with_reason(
                SYSTEM_KEYRING_UNAVAILABLE_REASON,
            )),
        }
    }
}

#[cfg(feature = "system-keyring")]
impl AnchorProvider for SystemKeyringAnchorProvider {
    fn provider_id(&self) -> &str {
        SYSTEM_KEYRING_PROVIDER_ID
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        AnchorProviderAssurance::SystemKeyring
    }

    fn health(&self) -> AnchorProviderHealth {
        AnchorProviderHealth::Ready
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        Ok(self
            .load()?
            .unwrap_or_else(|| self.candidate.clone())
            .checkpoint
            .identity)
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        Ok(self.load()?.map(|stored| stored.checkpoint))
    }

    fn publish(
        &self,
        expected: Option<&AnchorCheckpoint>,
        next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        let current = self.load()?;
        if current.as_ref().map(|stored| &stored.checkpoint) != expected
            || !is_monotonic_publish(expected, next, &self.candidate.checkpoint.identity)
        {
            return Err(integrity_recovery_required());
        }
        let encoding = current
            .as_ref()
            .map_or(StoredAnchorEncoding::ProviderBoundV2, |stored| {
                stored.encoding
            });
        let schema_fence = current
            .as_ref()
            .and_then(|stored| stored.schema_fence.clone());
        let stored = StoredAnchor {
            checkpoint: next.clone(),
            key: current
                .map(|stored| stored.key)
                .unwrap_or_else(|| self.candidate.key.clone()),
            encoding,
            schema_fence,
        };
        let encoded = encode_stored_anchor(&stored)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        self.entry()?
            .set_password(&encoded)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let persisted = self
            .load()?
            .ok_or_else(|| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        if persisted.checkpoint != *next {
            return Err(integrity_recovery_required());
        }
        Ok(())
    }

    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
        let stored = self.load()?.unwrap_or_else(|| self.candidate.clone());
        let key = decode_key(&stored.key).map_err(|_| integrity_recovery_required())?;
        Ok(crate::utils::bytes_to_hex(hmac_sha256(
            &key,
            &integrity_envelope(domain, canonical_payload),
        )))
    }

    fn needs_persisted_provider_migration(&self) -> McpPlatformResult<bool> {
        Ok(self
            .load()?
            .unwrap_or_else(|| self.candidate.clone())
            .needs_provider_migration())
    }

    fn migrate_persisted_provider_binding(
        &self,
        expected: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        let current = self.load()?.ok_or_else(integrity_recovery_required)?;
        if current.checkpoint != *expected {
            return Err(integrity_recovery_required());
        }
        if !current.needs_provider_migration() {
            return Ok(());
        }
        let encoded = encode_stored_anchor_provider_bound(&current)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        self.entry()?
            .set_password(&encoded)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let migrated = self
            .load()?
            .ok_or_else(|| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        if migrated.checkpoint != *expected || migrated.needs_provider_migration() {
            return Err(integrity_recovery_required());
        }
        Ok(())
    }

    fn stored_schema_compatibility_fence(
        &self,
    ) -> McpPlatformResult<Option<SchemaCompatibilityFence>> {
        Ok(self.load()?.and_then(|stored| stored.schema_fence))
    }

    fn ensure_schema_compatibility_fence(
        &self,
        expected: &AnchorCheckpoint,
        fence: &SchemaCompatibilityFence,
    ) -> McpPlatformResult<()> {
        let current = self.load()?.ok_or_else(integrity_recovery_required)?;
        if current.checkpoint != *expected {
            return Err(integrity_recovery_required());
        }
        if current.schema_fence.as_ref() == Some(fence) {
            return Ok(());
        }
        let encoded = encode_stored_anchor(&StoredAnchor::schema_fenced(
            current.checkpoint.clone(),
            current.key.clone(),
            fence.clone(),
        ))
        .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        self.entry()?
            .set_password(&encoded)
            .map_err(|_| integrity_unavailable_with_reason(SYSTEM_KEYRING_UNAVAILABLE_REASON))?;
        let migrated = self.load()?.ok_or_else(integrity_recovery_required)?;
        if migrated.checkpoint != *expected || migrated.schema_fence.as_ref() != Some(fence) {
            return Err(integrity_recovery_required());
        }
        Ok(())
    }
}

#[cfg(feature = "system-keyring")]
fn system_account(path_binding: &str) -> String {
    format!("database-{}", &path_binding[..32])
}

fn is_monotonic_publish(
    expected: Option<&AnchorCheckpoint>,
    next: &AnchorCheckpoint,
    identity: &AnchorIdentity,
) -> bool {
    if next.identity != *identity {
        return false;
    }
    match expected {
        Some(current) => next.sequence == current.sequence + 1 && next.root != current.root,
        None => next.sequence == 0,
    }
}

#[cfg(feature = "system-keyring")]
fn decode_stored_anchor(encoded: &str) -> Result<StoredAnchor, ()> {
    let value = parse_strict_json(encoded)?;
    if is_exact_schema_fenced_v3_shape(&value) {
        let record = serde_json::from_value::<StoredAnchorSchemaFencedRecord>(
            serde_json::Value::from(value),
        )
        .map_err(|_| ())?;
        if record.format_version != STORED_ANCHOR_SCHEMA_FENCED_V3_FORMAT_VERSION {
            return Err(());
        }
        let provider_id = parse_persisted_provider_id(&record.checkpoint.identity.provider_id)
            .ok_or(())?
            .as_str()
            .to_string();
        let schema_fence = parse_schema_compatibility_fence(&record.schema_fence)?;
        return Ok(StoredAnchor::schema_fenced(
            AnchorCheckpoint {
                identity: AnchorIdentity {
                    instance_id: record.checkpoint.identity.instance_id,
                    path_binding: record.checkpoint.identity.path_binding,
                    key_epoch: record.checkpoint.identity.key_epoch,
                    provider_id,
                },
                sequence: record.checkpoint.sequence,
                root: record.checkpoint.root,
            },
            record.key,
            schema_fence,
        ));
    }
    if is_exact_provider_bound_v2_shape(&value) {
        let record = serde_json::from_value::<StoredAnchorProviderBoundRecord>(
            serde_json::Value::from(value),
        )
        .map_err(|_| ())?;
        if record.format_version != STORED_ANCHOR_PROVIDER_BOUND_V2_FORMAT_VERSION {
            return Err(());
        }
        let provider_id = parse_persisted_provider_id(&record.checkpoint.identity.provider_id)
            .ok_or(())?
            .as_str()
            .to_string();
        return Ok(StoredAnchor::provider_bound(
            AnchorCheckpoint {
                identity: AnchorIdentity {
                    instance_id: record.checkpoint.identity.instance_id,
                    path_binding: record.checkpoint.identity.path_binding,
                    key_epoch: record.checkpoint.identity.key_epoch,
                    provider_id,
                },
                sequence: record.checkpoint.sequence,
                root: record.checkpoint.root,
            },
            record.key,
        ));
    }
    if is_exact_legacy_shape(&value) {
        let legacy =
            serde_json::from_value::<StoredAnchorLegacyRecord>(serde_json::Value::from(value))
                .map_err(|_| ())?;
        return Ok(StoredAnchor {
            checkpoint: AnchorCheckpoint {
                identity: AnchorIdentity {
                    instance_id: legacy.checkpoint.identity.instance_id,
                    path_binding: legacy.checkpoint.identity.path_binding,
                    key_epoch: legacy.checkpoint.identity.key_epoch,
                    provider_id: SYSTEM_KEYRING_PROVIDER_ID.to_string(),
                },
                sequence: legacy.checkpoint.sequence,
                root: legacy.checkpoint.root,
            },
            key: legacy.key,
            encoding: StoredAnchorEncoding::Legacy,
            schema_fence: None,
        });
    }
    Err(())
}

#[cfg(feature = "system-keyring")]
fn parse_strict_json(encoded: &str) -> Result<StrictJsonValue, ()> {
    let mut deserializer = serde_json::Deserializer::from_str(encoded);
    let value = StrictJsonValue::deserialize(&mut deserializer).map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    Ok(value)
}

#[cfg(feature = "system-keyring")]
fn is_exact_legacy_shape(value: &StrictJsonValue) -> bool {
    let Some(root) = as_object(value) else {
        return false;
    };
    if !has_exact_keys(root, &["checkpoint", "key"]) {
        return false;
    }
    let Some(checkpoint) = root_field(root, "checkpoint").and_then(as_object) else {
        return false;
    };
    if !has_exact_keys(checkpoint, &["identity", "sequence", "root"]) {
        return false;
    }
    let Some(identity) = root_field(checkpoint, "identity").and_then(as_object) else {
        return false;
    };
    has_exact_keys(identity, &["instance_id", "path_binding", "key_epoch"])
}

#[cfg(feature = "system-keyring")]
fn is_exact_provider_bound_v2_shape(value: &StrictJsonValue) -> bool {
    let Some(root) = as_object(value) else {
        return false;
    };
    if !has_exact_keys(root, &["format_version", "checkpoint", "key"]) {
        return false;
    }
    let Some(checkpoint) = root_field(root, "checkpoint").and_then(as_object) else {
        return false;
    };
    if !has_exact_keys(checkpoint, &["identity", "sequence", "root"]) {
        return false;
    }
    let Some(identity) = root_field(checkpoint, "identity").and_then(as_object) else {
        return false;
    };
    has_exact_keys(
        identity,
        &["instance_id", "path_binding", "key_epoch", "provider_id"],
    )
}

#[cfg(feature = "system-keyring")]
fn is_exact_schema_fenced_v3_shape(value: &StrictJsonValue) -> bool {
    let Some(root) = as_object(value) else {
        return false;
    };
    if !has_exact_keys(
        root,
        &["format_version", "checkpoint", "key", "schema_fence"],
    ) {
        return false;
    }
    let Some(checkpoint) = root_field(root, "checkpoint").and_then(as_object) else {
        return false;
    };
    if !has_exact_keys(checkpoint, &["identity", "sequence", "root"]) {
        return false;
    }
    let Some(identity) = root_field(checkpoint, "identity").and_then(as_object) else {
        return false;
    };
    if !has_exact_keys(
        identity,
        &["instance_id", "path_binding", "key_epoch", "provider_id"],
    ) {
        return false;
    }
    let Some(schema_fence) = root_field(root, "schema_fence").and_then(as_object) else {
        return false;
    };
    has_exact_keys(
        schema_fence,
        &[
            "minimum_schema_version",
            "projection_mutations_shape",
            "phase",
        ],
    )
}

#[cfg(feature = "system-keyring")]
fn as_object(value: &StrictJsonValue) -> Option<&[(String, StrictJsonValue)]> {
    match value {
        StrictJsonValue::Object(entries) => Some(entries),
        _ => None,
    }
}

#[cfg(feature = "system-keyring")]
fn has_exact_keys(entries: &[(String, StrictJsonValue)], expected: &[&str]) -> bool {
    entries.len() == expected.len()
        && expected
            .iter()
            .all(|expected_key| root_field(entries, expected_key).is_some())
}

#[cfg(feature = "system-keyring")]
fn root_field<'a>(
    entries: &'a [(String, StrictJsonValue)],
    expected: &str,
) -> Option<&'a StrictJsonValue> {
    entries
        .iter()
        .find_map(|(key, value)| (key == expected).then_some(value))
}

#[cfg(feature = "system-keyring")]
fn decode_key(encoded: &str) -> Result<[u8; KEY_BYTES], ()> {
    let decoded = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| ())?;
    decoded.try_into().map_err(|_| ())
}

#[cfg(feature = "system-keyring")]
fn parse_schema_compatibility_fence(
    record: &StoredSchemaCompatibilityFenceRecord,
) -> Result<SchemaCompatibilityFence, ()> {
    if record.minimum_schema_version != 15
        || record.projection_mutations_shape
            != STORED_SCHEMA_FENCE_PROJECTION_MUTATIONS_WITNESS_V2_V15
    {
        return Err(());
    }
    Ok(SchemaCompatibilityFence {
        minimum_schema_version: record.minimum_schema_version,
        projection_mutations_shape: record.projection_mutations_shape.clone(),
        phase: record.phase,
    })
}

#[cfg(feature = "system-keyring")]
fn encode_stored_anchor(stored: &StoredAnchor) -> Result<String, ()> {
    match (&stored.encoding, &stored.schema_fence) {
        (StoredAnchorEncoding::Legacy, _) => encode_stored_anchor_provider_bound(stored),
        (StoredAnchorEncoding::ProviderBoundV2, None) => {
            encode_stored_anchor_provider_bound(stored)
        }
        (StoredAnchorEncoding::SchemaFencedV3, Some(schema_fence)) => {
            encode_stored_anchor_schema_fenced(stored, schema_fence)
        }
        _ => Err(()),
    }
}

#[cfg(feature = "system-keyring")]
fn encode_stored_anchor_provider_bound(stored: &StoredAnchor) -> Result<String, ()> {
    serde_json::to_string(&StoredAnchorProviderBoundRecord {
        format_version: STORED_ANCHOR_PROVIDER_BOUND_V2_FORMAT_VERSION,
        checkpoint: StoredAnchorProviderBoundCheckpoint {
            identity: StoredAnchorProviderBoundIdentity {
                instance_id: stored.checkpoint.identity.instance_id.clone(),
                path_binding: stored.checkpoint.identity.path_binding.clone(),
                key_epoch: stored.checkpoint.identity.key_epoch,
                provider_id: stored.checkpoint.identity.provider_id.clone(),
            },
            sequence: stored.checkpoint.sequence,
            root: stored.checkpoint.root.clone(),
        },
        key: stored.key.clone(),
    })
    .map_err(|_| ())
}

#[cfg(feature = "system-keyring")]
fn encode_stored_anchor_schema_fenced(
    stored: &StoredAnchor,
    schema_fence: &SchemaCompatibilityFence,
) -> Result<String, ()> {
    serde_json::to_string(&StoredAnchorSchemaFencedRecord {
        format_version: STORED_ANCHOR_SCHEMA_FENCED_V3_FORMAT_VERSION,
        checkpoint: StoredAnchorProviderBoundCheckpoint {
            identity: StoredAnchorProviderBoundIdentity {
                instance_id: stored.checkpoint.identity.instance_id.clone(),
                path_binding: stored.checkpoint.identity.path_binding.clone(),
                key_epoch: stored.checkpoint.identity.key_epoch,
                provider_id: stored.checkpoint.identity.provider_id.clone(),
            },
            sequence: stored.checkpoint.sequence,
            root: stored.checkpoint.root.clone(),
        },
        key: stored.key.clone(),
        schema_fence: StoredSchemaCompatibilityFenceRecord {
            minimum_schema_version: schema_fence.minimum_schema_version,
            projection_mutations_shape: schema_fence.projection_mutations_shape.clone(),
            phase: schema_fence.phase,
        },
    })
    .map_err(|_| ())
}

#[cfg(all(test, feature = "system-keyring"))]
fn encode_stored_anchor_legacy(stored: &StoredAnchor) -> Result<String, ()> {
    serde_json::to_string(&StoredAnchorLegacyWriteRecord {
        checkpoint: LegacyAnchorWriteCheckpoint {
            identity: LegacyAnchorWriteIdentity {
                instance_id: stored.checkpoint.identity.instance_id.clone(),
                path_binding: stored.checkpoint.identity.path_binding.clone(),
                key_epoch: stored.checkpoint.identity.key_epoch,
            },
            sequence: stored.checkpoint.sequence,
            root: stored.checkpoint.root.clone(),
        },
        key: stored.key.clone(),
    })
    .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "system-keyring")]
    fn decode_provider_bound_v2_strict(
        encoded: &str,
    ) -> Result<StoredAnchorProviderBoundRecord, ()> {
        let value = parse_strict_json(encoded)?;
        if !is_exact_provider_bound_v2_shape(&value) {
            return Err(());
        }
        let record = serde_json::from_value::<StoredAnchorProviderBoundRecord>(
            serde_json::Value::from(value),
        )
        .map_err(|_| ())?;
        if record.format_version != STORED_ANCHOR_PROVIDER_BOUND_V2_FORMAT_VERSION {
            return Err(());
        }
        Ok(record)
    }

    #[cfg(feature = "system-keyring")]
    fn system_anchor(checkpoint_sequence: u64) -> StoredAnchor {
        StoredAnchor::provider_bound(
            AnchorCheckpoint {
                identity: AnchorIdentity {
                    instance_id: "instance".to_string(),
                    path_binding: "binding".to_string(),
                    key_epoch: 1,
                    provider_id: SYSTEM_KEYRING_PROVIDER_ID.to_string(),
                },
                sequence: checkpoint_sequence,
                root: "a".repeat(64),
            },
            base64::engine::general_purpose::STANDARD_NO_PAD.encode([0x11; KEY_BYTES]),
        )
    }

    #[cfg(feature = "system-keyring")]
    fn system_schema_fenced_anchor(
        checkpoint_sequence: u64,
        fence: SchemaCompatibilityFence,
    ) -> StoredAnchor {
        let stored = system_anchor(checkpoint_sequence);
        StoredAnchor::schema_fenced(stored.checkpoint, stored.key, fence)
    }

    #[cfg(feature = "system-keyring")]
    #[test]
    fn strict_v3_decoder_accepts_only_exact_pending_and_active_payloads() {
        for fence in [
            SchemaCompatibilityFence::projection_witness_v2_v15_pending(),
            SchemaCompatibilityFence::projection_witness_v2_v15_active(),
        ] {
            let encoded =
                encode_stored_anchor(&system_schema_fenced_anchor(5, fence.clone())).unwrap();
            let decoded = decode_stored_anchor(&encoded).unwrap();
            assert_eq!(decoded.encoding, StoredAnchorEncoding::SchemaFencedV3);
            assert_eq!(decoded.schema_fence, Some(fence));
            assert_eq!(
                decoded.checkpoint.identity.provider_id,
                SYSTEM_KEYRING_PROVIDER_ID
            );
        }
    }

    #[cfg(feature = "system-keyring")]
    #[test]
    fn strict_v3_decoder_rejects_phase_less_unknown_and_extra_schema_payloads() {
        let encoded = encode_stored_anchor(&system_schema_fenced_anchor(
            6,
            SchemaCompatibilityFence::projection_witness_v2_v15_active(),
        ))
        .unwrap();

        let mut missing_phase = serde_json::from_str::<serde_json::Value>(&encoded).unwrap();
        missing_phase["schema_fence"]
            .as_object_mut()
            .unwrap()
            .remove("phase");
        assert!(decode_stored_anchor(&serde_json::to_string(&missing_phase).unwrap()).is_err());

        let mut null_phase = serde_json::from_str::<serde_json::Value>(&encoded).unwrap();
        null_phase["schema_fence"]["phase"] = serde_json::Value::Null;
        assert!(decode_stored_anchor(&serde_json::to_string(&null_phase).unwrap()).is_err());

        let mut unknown_phase = serde_json::from_str::<serde_json::Value>(&encoded).unwrap();
        unknown_phase["schema_fence"]["phase"] = serde_json::json!("active_v16");
        assert!(decode_stored_anchor(&serde_json::to_string(&unknown_phase).unwrap()).is_err());

        let mut mistyped_phase = serde_json::from_str::<serde_json::Value>(&encoded).unwrap();
        mistyped_phase["schema_fence"]["phase"] = serde_json::json!(15);
        assert!(decode_stored_anchor(&serde_json::to_string(&mistyped_phase).unwrap()).is_err());

        let mut extra_schema_field = serde_json::from_str::<serde_json::Value>(&encoded).unwrap();
        extra_schema_field["schema_fence"]["unexpected"] = serde_json::json!(true);
        assert!(
            decode_stored_anchor(&serde_json::to_string(&extra_schema_field).unwrap()).is_err()
        );
    }

    #[cfg(feature = "system-keyring")]
    #[test]
    fn strict_v2_decoder_accepts_only_exact_provider_bound_v2_payloads() {
        let encoded = encode_stored_anchor_provider_bound(&system_anchor(2)).unwrap();
        let decoded = decode_provider_bound_v2_strict(&encoded).unwrap();
        assert_eq!(
            decoded.format_version,
            STORED_ANCHOR_PROVIDER_BOUND_V2_FORMAT_VERSION
        );
        assert_eq!(
            decoded.checkpoint.identity.provider_id,
            SYSTEM_KEYRING_PROVIDER_ID
        );
    }

    #[cfg(feature = "system-keyring")]
    #[test]
    fn strict_v2_decoder_rejects_v3_unknown_and_partial_payloads() {
        let v3_active = encode_stored_anchor_schema_fenced(
            &StoredAnchor::schema_fenced(
                system_anchor(3).checkpoint.clone(),
                system_anchor(3).key.clone(),
                SchemaCompatibilityFence::projection_witness_v2_v15_active(),
            ),
            &SchemaCompatibilityFence::projection_witness_v2_v15_active(),
        )
        .unwrap();
        assert!(decode_provider_bound_v2_strict(&v3_active).is_err());

        let v3_pending = encode_stored_anchor_schema_fenced(
            &StoredAnchor::schema_fenced(
                system_anchor(3).checkpoint.clone(),
                system_anchor(3).key.clone(),
                SchemaCompatibilityFence::projection_witness_v2_v15_pending(),
            ),
            &SchemaCompatibilityFence::projection_witness_v2_v15_pending(),
        )
        .unwrap();
        assert!(decode_provider_bound_v2_strict(&v3_pending).is_err());

        let mut legacy_v3_active = serde_json::from_str::<serde_json::Value>(&v3_active).unwrap();
        legacy_v3_active["schema_fence"]
            .as_object_mut()
            .unwrap()
            .remove("phase");
        assert!(decode_provider_bound_v2_strict(
            &serde_json::to_string(&legacy_v3_active).unwrap()
        )
        .is_err());

        let exact_v2 = encode_stored_anchor_provider_bound(&system_anchor(4)).unwrap();
        let mut unknown = serde_json::from_str::<serde_json::Value>(&exact_v2).unwrap();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(
            decode_provider_bound_v2_strict(&serde_json::to_string(&unknown).unwrap()).is_err()
        );

        let mut partial = serde_json::from_str::<serde_json::Value>(&exact_v2).unwrap();
        partial["checkpoint"]["identity"]
            .as_object_mut()
            .unwrap()
            .remove("provider_id");
        assert!(
            decode_provider_bound_v2_strict(&serde_json::to_string(&partial).unwrap()).is_err()
        );
    }
}
