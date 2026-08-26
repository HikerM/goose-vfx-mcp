use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

const KEY_BYTES: usize = 32;
#[cfg(feature = "system-keyring")]
const KEYRING_SERVICE: &str = "lumina-mcp-platform-integrity-v2";
const ENVELOPE_DOMAIN: &[u8] = b"lumina.mcp-platform.integrity";

#[path = "../../anchor/mod.rs"]
mod anchor;

pub(crate) use anchor::SchemaCompatibilityFence;
pub(crate) use anchor::{parse_persisted_provider_id, PersistedProviderId};
pub use anchor::{AnchorCheckpoint, AnchorIdentity};
pub(crate) use anchor::{AnchorProvider, AnchorProviderAssurance, AnchorProviderHealth};

pub(crate) trait IntegritySigner: AnchorProvider {}

impl<T> IntegritySigner for T where T: AnchorProvider + ?Sized {}

pub(crate) struct InMemoryIntegritySigner;

impl InMemoryIntegritySigner {
    pub(crate) fn new_for_testing(key: [u8; KEY_BYTES]) -> Arc<dyn IntegritySigner> {
        Self::new_for_testing_with_path_binding(key, anchor::DEFAULT_TEST_PATH_BINDING)
    }

    pub(crate) fn new_for_testing_with_path_binding(
        key: [u8; KEY_BYTES],
        path_binding: impl Into<String>,
    ) -> Arc<dyn IntegritySigner> {
        anchor::AnchorProviderRegistry::in_memory_for_testing(key, path_binding)
    }
}

/// Returns the existing system-keyring signer for another root-bound durable
/// commitment. Callers must use a distinct, domain-separated path binding.
pub(crate) fn system_signer(path_binding: &str, allow_create: bool) -> Arc<dyn IntegritySigner> {
    let signer = anchor::AnchorProviderRegistry::production_default(path_binding, allow_create);
    #[cfg(test)]
    if let Some(failure) = consume_test_system_signer_failure(path_binding) {
        return Arc::new(TestSystemSignerFailureWrapper {
            inner: signer,
            failure,
        });
    }
    signer
}

pub(super) fn unavailable_signer() -> Arc<dyn IntegritySigner> {
    anchor::AnchorProviderRegistry::unavailable()
}

pub(super) fn unsupported_signer(provider_id: impl Into<String>) -> Arc<dyn IntegritySigner> {
    anchor::AnchorProviderRegistry::unsupported(provider_id)
}

pub(super) fn clear_system_anchor_for_trusted_reenrollment(
    path_binding: &str,
) -> McpPlatformResult<()> {
    #[cfg(test)]
    if consume_test_clear_anchor_failure(path_binding) {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::IntegrityUnavailable,
            "managed MCP integrity anchor clear failed for testing",
        ));
    }
    anchor::AnchorProviderRegistry::clear_system_anchor_for_trusted_reenrollment(path_binding)
}

#[cfg(all(test, feature = "system-keyring"))]
pub(super) fn rewrite_system_anchor_as_legacy_for_testing(
    path_binding: &str,
) -> McpPlatformResult<()> {
    anchor::AnchorProviderRegistry::rewrite_system_anchor_as_legacy_for_testing(path_binding)
}

#[cfg(all(test, feature = "system-keyring"))]
pub(super) fn write_raw_system_anchor_for_testing(
    path_binding: &str,
    payload: &str,
) -> McpPlatformResult<()> {
    anchor::AnchorProviderRegistry::write_raw_system_anchor_for_testing(path_binding, payload)
}

#[cfg(all(test, feature = "system-keyring"))]
pub(super) fn read_raw_system_anchor_for_testing(path_binding: &str) -> McpPlatformResult<String> {
    anchor::AnchorProviderRegistry::read_raw_system_anchor_for_testing(path_binding)
}

#[cfg(all(test, feature = "system-keyring"))]
pub(super) fn overwrite_system_anchor_provider_id_for_testing(
    path_binding: &str,
    provider_id: &str,
) -> McpPlatformResult<()> {
    anchor::AnchorProviderRegistry::overwrite_system_anchor_provider_id_for_testing(
        path_binding,
        provider_id,
    )
}

pub(super) fn preflight_open_error(signer: &dyn IntegritySigner) -> Option<McpPlatformError> {
    signer.health().availability_error()
}

pub(super) fn matches_expected_path_binding(
    signer: &dyn IntegritySigner,
    expected: &str,
    actual: &str,
) -> bool {
    signer.matches_expected_path_binding(expected, actual)
}

pub(super) fn canonical_fields(fields: &[&[u8]]) -> Vec<u8> {
    let mut payload = Vec::new();
    for field in fields {
        payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
        payload.extend_from_slice(field);
    }
    payload
}

fn integrity_envelope(domain: &str, canonical_payload: &[u8]) -> Vec<u8> {
    canonical_fields(&[
        ENVELOPE_DOMAIN,
        b"schema-v2",
        domain.as_bytes(),
        canonical_payload,
    ])
}

pub(super) fn digest(fields: &[&[u8]]) -> String {
    crate::utils::bytes_to_hex(Sha256::digest(canonical_fields(fields)))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut normalized = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "managed MCP integrity validation failed",
    )
}

pub(super) fn integrity_recovery_required() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "managed MCP integrity recovery or trusted re-enrollment is required",
    )
}

fn integrity_unavailable_with_reason(reason: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityUnavailable, reason)
}

fn integrity_unsupported(reason: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityUnavailable, reason)
}

#[cfg(test)]
struct QueuedStringScopedRegistration<T> {
    id: u64,
    value: T,
}

#[cfg(test)]
struct StringScopedTestRegistrations<T> {
    entries: std::collections::HashMap<
        String,
        std::collections::VecDeque<QueuedStringScopedRegistration<T>>,
    >,
}

#[cfg(test)]
impl<T> Default for StringScopedTestRegistrations<T> {
    fn default() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
impl<T> StringScopedTestRegistrations<T> {
    fn insert(&mut self, key: String, value: T) -> u64 {
        let id = next_test_injection_id();
        self.entries
            .entry(key)
            .or_default()
            .push_back(QueuedStringScopedRegistration { id, value });
        id
    }

    fn pop_front(&mut self, key: &str) -> Option<T> {
        let (registration, remove_entry) = {
            let registrations = self.entries.get_mut(key)?;
            let registration = registrations.pop_front()?;
            let remove_entry = registrations.is_empty();
            (registration, remove_entry)
        };
        if remove_entry {
            self.entries.remove(key);
        }
        Some(registration.value)
    }

    fn remove(&mut self, key: &str, id: u64) -> Option<T> {
        let (registration, remove_entry) = {
            let registrations = self.entries.get_mut(key)?;
            let index = registrations
                .iter()
                .position(|registration| registration.id == id)?;
            let registration = registrations.remove(index)?;
            let remove_entry = registrations.is_empty();
            (registration, remove_entry)
        };
        if remove_entry {
            self.entries.remove(key);
        }
        Some(registration.value)
    }
}

#[cfg(test)]
fn next_test_injection_id() -> u64 {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
fn test_clear_anchor_failure_state() -> &'static std::sync::Mutex<StringScopedTestRegistrations<()>>
{
    static FAIL_NEXT_CLEAR: std::sync::OnceLock<
        std::sync::Mutex<StringScopedTestRegistrations<()>>,
    > = std::sync::OnceLock::new();
    FAIL_NEXT_CLEAR.get_or_init(|| std::sync::Mutex::new(StringScopedTestRegistrations::default()))
}

#[cfg(test)]
fn consume_test_clear_anchor_failure(path_binding: &str) -> bool {
    test_clear_anchor_failure_state()
        .lock()
        .unwrap()
        .pop_front(path_binding)
        .is_some()
}

#[cfg(test)]
pub(super) fn fail_next_clear_system_anchor_for_trusted_reenrollment(
    path_binding: impl Into<String>,
) -> TestClearAnchorFailureGuard {
    let path_binding = path_binding.into();
    let id = test_clear_anchor_failure_state()
        .lock()
        .unwrap()
        .insert(path_binding.clone(), ());
    TestClearAnchorFailureGuard { path_binding, id }
}

#[cfg(test)]
pub(super) struct TestClearAnchorFailureGuard {
    path_binding: String,
    id: u64,
}

#[cfg(test)]
impl Drop for TestClearAnchorFailureGuard {
    fn drop(&mut self) {
        test_clear_anchor_failure_state()
            .lock()
            .unwrap()
            .remove(&self.path_binding, self.id);
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestSystemSignerFailure {
    Identity,
    Publish,
    PublishThenError,
    EnsureSchemaCompatibilityFence,
    EnsureSchemaCompatibilityFenceThenError,
}

#[cfg(test)]
struct TestSystemSignerFailureWrapper {
    inner: Arc<dyn IntegritySigner>,
    failure: TestSystemSignerFailure,
}

#[cfg(test)]
impl AnchorProvider for TestSystemSignerFailureWrapper {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn assurance(&self) -> AnchorProviderAssurance {
        self.inner.assurance()
    }

    fn health(&self) -> AnchorProviderHealth {
        self.inner.health()
    }

    fn identity(&self) -> McpPlatformResult<AnchorIdentity> {
        if matches!(self.failure, TestSystemSignerFailure::Identity) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP integrity identity failed for testing",
            ));
        }
        self.inner.identity()
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<AnchorCheckpoint>> {
        self.inner.checkpoint()
    }

    fn publish(
        &self,
        expected: Option<&AnchorCheckpoint>,
        next: &AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        match self.failure {
            TestSystemSignerFailure::Identity => self.inner.publish(expected, next),
            TestSystemSignerFailure::Publish => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP integrity publish failed for testing",
            )),
            TestSystemSignerFailure::PublishThenError => {
                self.inner.publish(expected, next)?;
                Err(McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityUnavailable,
                    "managed MCP integrity publish succeeded before failing for testing",
                ))
            }
            TestSystemSignerFailure::EnsureSchemaCompatibilityFence
            | TestSystemSignerFailure::EnsureSchemaCompatibilityFenceThenError => {
                self.inner.publish(expected, next)
            }
        }
    }

    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
        self.inner.sign(domain, canonical_payload)
    }

    fn ensure_schema_compatibility_fence(
        &self,
        expected: &AnchorCheckpoint,
        fence: &SchemaCompatibilityFence,
    ) -> McpPlatformResult<()> {
        match self.failure {
            TestSystemSignerFailure::EnsureSchemaCompatibilityFence => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP schema compatibility fence publish failed for testing",
            )),
            TestSystemSignerFailure::EnsureSchemaCompatibilityFenceThenError => {
                self.inner
                    .ensure_schema_compatibility_fence(expected, fence)?;
                Err(McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityUnavailable,
                    "managed MCP schema compatibility fence publish succeeded before failing for testing",
                ))
            }
            _ => self
                .inner
                .ensure_schema_compatibility_fence(expected, fence),
        }
    }

    fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
        self.inner.matches_expected_path_binding(expected, actual)
    }
}

#[cfg(test)]
fn test_system_signer_failure_state(
) -> &'static std::sync::Mutex<StringScopedTestRegistrations<TestSystemSignerFailure>> {
    static FAILURES: std::sync::OnceLock<
        std::sync::Mutex<StringScopedTestRegistrations<TestSystemSignerFailure>>,
    > = std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(StringScopedTestRegistrations::default()))
}

#[cfg(test)]
fn consume_test_system_signer_failure(path_binding: &str) -> Option<TestSystemSignerFailure> {
    test_system_signer_failure_state()
        .lock()
        .unwrap()
        .pop_front(path_binding)
}

#[cfg(test)]
fn install_test_system_signer_failure(
    path_binding: impl Into<String>,
    failure: TestSystemSignerFailure,
) -> TestSystemSignerFailureGuard {
    let path_binding = path_binding.into();
    let id = test_system_signer_failure_state()
        .lock()
        .unwrap()
        .insert(path_binding.clone(), failure);
    TestSystemSignerFailureGuard { path_binding, id }
}

#[cfg(test)]
pub(super) fn fail_next_system_signer_identity(
    path_binding: impl Into<String>,
) -> TestSystemSignerFailureGuard {
    install_test_system_signer_failure(path_binding, TestSystemSignerFailure::Identity)
}

#[cfg(test)]
pub(super) fn fail_next_system_signer_publish(
    path_binding: impl Into<String>,
) -> TestSystemSignerFailureGuard {
    install_test_system_signer_failure(path_binding, TestSystemSignerFailure::Publish)
}

#[cfg(test)]
pub(super) fn fail_next_system_signer_publish_then_error(
    path_binding: impl Into<String>,
) -> TestSystemSignerFailureGuard {
    install_test_system_signer_failure(path_binding, TestSystemSignerFailure::PublishThenError)
}

#[cfg(test)]
pub(super) fn fail_next_system_signer_schema_fence_publish(
    path_binding: impl Into<String>,
) -> TestSystemSignerFailureGuard {
    install_test_system_signer_failure(
        path_binding,
        TestSystemSignerFailure::EnsureSchemaCompatibilityFence,
    )
}

#[cfg(test)]
pub(super) fn fail_next_system_signer_schema_fence_publish_then_error(
    path_binding: impl Into<String>,
) -> TestSystemSignerFailureGuard {
    install_test_system_signer_failure(
        path_binding,
        TestSystemSignerFailure::EnsureSchemaCompatibilityFenceThenError,
    )
}

#[cfg(test)]
pub(super) struct TestSystemSignerFailureGuard {
    path_binding: String,
    id: u64,
}

#[cfg(test)]
impl Drop for TestSystemSignerFailureGuard {
    fn drop(&mut self) {
        test_system_signer_failure_state()
            .lock()
            .unwrap()
            .remove(&self.path_binding, self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_rfc_4231_vector() {
        let key = [0x0b; 20];
        assert_eq!(
            crate::utils::bytes_to_hex(hmac_sha256(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn in_memory_anchor_compare_and_set_rejects_replay() {
        let signer = InMemoryIntegritySigner::new_for_testing([0x41; 32]);
        let identity = signer.identity().unwrap();
        let first = AnchorCheckpoint {
            identity,
            sequence: 0,
            root: "first".to_string(),
        };
        signer.publish(None, &first).unwrap();
        assert!(signer.publish(None, &first).is_err());
    }

    #[test]
    fn production_selection_never_downgrades_to_in_memory_provider() {
        let signer = system_signer(&"a".repeat(64), false);
        assert_eq!(signer.provider_id(), anchor::SYSTEM_KEYRING_PROVIDER_ID);
        assert_eq!(signer.assurance(), AnchorProviderAssurance::SystemKeyring);
        assert_ne!(signer.provider_id(), anchor::IN_MEMORY_TEST_PROVIDER_ID);
    }

    #[test]
    fn in_memory_provider_is_explicitly_test_only() {
        let signer = InMemoryIntegritySigner::new_for_testing([0x42; 32]);
        assert_eq!(signer.provider_id(), anchor::IN_MEMORY_TEST_PROVIDER_ID);
        assert_eq!(signer.assurance(), AnchorProviderAssurance::TestInMemory);
        assert!(matches!(signer.health(), AnchorProviderHealth::Ready));
        assert!(matches_expected_path_binding(
            signer.as_ref(),
            "different-binding",
            anchor::DEFAULT_TEST_PATH_BINDING,
        ));
        assert!(!matches_expected_path_binding(
            signer.as_ref(),
            "different-binding",
            "another-path-binding",
        ));
    }

    #[test]
    fn clear_anchor_failure_is_path_binding_scoped_and_drop_safe() {
        let _guard = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-a");
        assert!(consume_test_clear_anchor_failure("binding-a"));
        assert!(!consume_test_clear_anchor_failure("binding-a"));

        let guard = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-b");
        drop(guard);
        assert!(!consume_test_clear_anchor_failure("binding-b"));
    }

    #[test]
    fn clear_anchor_failure_supports_same_path_dual_registration() {
        let first = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-c");
        let second = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-c");
        assert!(consume_test_clear_anchor_failure("binding-c"));
        assert!(consume_test_clear_anchor_failure("binding-c"));
        assert!(!consume_test_clear_anchor_failure("binding-c"));
        drop(first);
        drop(second);

        let dropped = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-d");
        let kept = fail_next_clear_system_anchor_for_trusted_reenrollment("binding-d");
        drop(dropped);
        assert!(consume_test_clear_anchor_failure("binding-d"));
        assert!(!consume_test_clear_anchor_failure("binding-d"));
        drop(kept);
    }
}
