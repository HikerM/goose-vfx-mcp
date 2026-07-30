//! Receipt-gated projection effects for a future recovery runtime.
//!
//! This module intentionally has no TaskRunner, ACP, or legacy `ProjectionSink`
//! integration. A caller can only execute through `FencedProjectionSink`; an
//! unfenced sink has no conversion path and therefore fails closed by type.

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use std::sync::{Arc, Condvar, Mutex};

use super::effect_fence::EffectFenceGuard;
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::repository::{
    ConsumingProjectionEffectGrant, FinishProjectionEffectGrant, IssuedProjectionEffectGrant,
    SqliteMcpPlatformRepository,
};
use super::runtime_fenced_sink_registry::RuntimeFencedSinkBootstrapAuthority;

#[cfg(test)]
use super::repository::IssueProjectionEffectGrant;

const FENCED_EFFECT_COMMAND_DOMAIN: &[u8] = b"goose.mcp-platform.fenced-projection-command-v43";

/// An externally safe effect request. Its payload is represented only by the
/// target digest; V43 will bind it to a concrete projection runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FencedProjectionEffectRequest {
    effect_id: String,
    target_digest: String,
}

/// The only data the repository accepts when a fenced effect is granted. Its
/// fields are private so a crate-local caller cannot swap an arbitrary sink
/// into a durable lifecycle by manufacturing a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FencedSinkIssueBinding {
    sink_identity: String,
    sink_version: String,
    authority_id: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
}

impl FencedSinkIssueBinding {
    pub(crate) fn from_trusted(sink: &TrustedFencedProjectionSink<'_>) -> McpPlatformResult<Self> {
        sink.ensure_live()?;
        validate_sink_identity(sink.adapter_id(), sink.adapter_version())?;
        validate_authority_id(&sink.authority_id)?;
        validate_authority_key(sink.authority_key_epoch, &sink.authority_key_fingerprint)?;
        Ok(Self {
            sink_identity: sink.adapter_id().to_string(),
            sink_version: sink.adapter_version().to_string(),
            authority_id: sink.authority_id.clone(),
            authority_key_epoch: sink.authority_key_epoch,
            authority_key_fingerprint: sink.authority_key_fingerprint.clone(),
        })
    }

    pub(crate) fn sink_identity(&self) -> &str {
        &self.sink_identity
    }
    pub(crate) fn sink_version(&self) -> &str {
        &self.sink_version
    }
    pub(crate) fn authority_id(&self) -> &str {
        &self.authority_id
    }
    pub(crate) const fn authority_key_epoch(&self) -> u64 {
        self.authority_key_epoch
    }
    pub(crate) fn authority_key_fingerprint(&self) -> &str {
        &self.authority_key_fingerprint
    }

    pub(crate) fn exactly_matches(
        &self,
        sink_identity: &str,
        sink_version: &str,
        authority_id: &str,
        authority_key_epoch: u64,
        authority_key_fingerprint: &str,
    ) -> bool {
        self.sink_identity == sink_identity
            && self.sink_version == sink_version
            && self.authority_id == authority_id
            && self.authority_key_epoch == authority_key_epoch
            && self.authority_key_fingerprint == authority_key_fingerprint
    }
}

/// Repository input used only by the fenced executor. It has no public or
/// crate-visible constructor; legacy direct grant construction is test-only.
pub(crate) struct BoundFencedProjectionEffectGrant<'a> {
    pub(crate) effect_id: &'a str,
    pub(crate) target_digest: &'a str,
    pub(crate) sink: FencedSinkIssueBinding,
    pub(crate) now_ms: i64,
}

/// Completion evidence accepted after command/receipt verification. This is a
/// linear, non-Clone capability with private fields. Repository code can read
/// it but no other module can construct it.
pub(crate) struct AcceptedFencedReceipt {
    receipt_id: String,
    sink_identity: String,
    sink_version: String,
    sink_authority: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    grant_id: String,
    effect_id: String,
    fence_epoch: i64,
    target_digest: String,
    command_binding: String,
    receipt_binding: String,
}

impl AcceptedFencedReceipt {
    pub(crate) fn receipt_id(&self) -> &str {
        &self.receipt_id
    }
    pub(crate) fn sink_identity(&self) -> &str {
        &self.sink_identity
    }
    pub(crate) fn sink_version(&self) -> &str {
        &self.sink_version
    }
    pub(crate) fn sink_authority(&self) -> &str {
        &self.sink_authority
    }
    pub(crate) const fn authority_key_epoch(&self) -> u64 {
        self.authority_key_epoch
    }
    pub(crate) fn authority_key_fingerprint(&self) -> &str {
        &self.authority_key_fingerprint
    }
    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }
    pub(crate) fn effect_id(&self) -> &str {
        &self.effect_id
    }
    pub(crate) const fn fence_epoch(&self) -> i64 {
        self.fence_epoch
    }
    pub(crate) fn target_digest(&self) -> &str {
        &self.target_digest
    }
    pub(crate) fn command_binding(&self) -> &str {
        &self.command_binding
    }
    pub(crate) fn receipt_binding(&self) -> &str {
        &self.receipt_binding
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        grant_id: String,
        effect_id: String,
        sink_identity: String,
        sink_version: String,
        sink_authority: String,
        authority_key_epoch: u64,
        authority_key_fingerprint: String,
        command_binding: String,
        fence_epoch: i64,
        target_digest: String,
    ) -> Self {
        let receipt_id = uuid::Uuid::new_v4().to_string();
        let receipt_binding = fenced_receipt_binding(
            &receipt_id,
            &sink_identity,
            &sink_version,
            &sink_authority,
            authority_key_epoch,
            &authority_key_fingerprint,
            &grant_id,
            &effect_id,
            fence_epoch,
            &target_digest,
            &command_binding,
        );
        Self {
            receipt_id,
            sink_identity,
            sink_version,
            sink_authority,
            authority_key_epoch,
            authority_key_fingerprint,
            grant_id,
            effect_id,
            fence_epoch,
            target_digest,
            command_binding,
            receipt_binding,
        }
    }
}

impl FencedProjectionEffectRequest {
    pub(crate) fn new(effect_id: String, target_digest: String) -> McpPlatformResult<Self> {
        validate_uuid(&effect_id)?;
        validate_digest(&target_digest)?;
        Ok(Self {
            effect_id,
            target_digest,
        })
    }
}

/// Opaque, repository-issued conditional request for an external projection sink.
/// It contains no raw grant nonce or other secret material.
pub(crate) struct FencedEffectCommand {
    repository_instance_id: String,
    repository_path_binding: String,
    repository_key_epoch: u64,
    sink_id: String,
    sink_version: String,
    sink_authority: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    fence_scope: String,
    grant_id: String,
    effect_id: String,
    fence_epoch: i64,
    target_digest: String,
    canonical_digest: String,
    grant_binding: String,
}

impl FencedEffectCommand {
    fn from_grant(grant: GrantFields<'_>) -> McpPlatformResult<Self> {
        validate_sink_identity(grant.sink_identity, grant.sink_version)?;
        validate_authority_id(grant.sink_authority)?;
        validate_authority_key(
            u64::try_from(grant.authority_key_epoch).map_err(|_| integrity_error())?,
            grant.authority_key_fingerprint,
        )?;
        validate_uuid(grant.grant_id)?;
        validate_uuid(grant.effect_id)?;
        validate_digest(grant.target_digest)?;
        validate_digest(grant.canonical_digest)?;
        if grant.fence_scope != "global" || grant.fence_epoch < 1 {
            return Err(integrity_error());
        }
        if grant.repository_key_epoch < 1 || grant.command_binding.len() != 64 {
            return Err(integrity_error());
        }
        let expected_binding = fenced_command_binding(
            grant.repository_instance_id,
            grant.repository_path_binding,
            grant.repository_key_epoch,
            grant.sink_identity,
            grant.sink_version,
            grant.sink_authority,
            grant.authority_key_epoch,
            grant.authority_key_fingerprint,
            grant.grant_id,
            grant.effect_id,
            grant.fence_epoch,
            grant.target_digest,
            grant.canonical_digest,
        );
        if grant.command_binding != expected_binding {
            return Err(integrity_error());
        }
        Ok(Self {
            repository_instance_id: grant.repository_instance_id.to_string(),
            repository_path_binding: grant.repository_path_binding.to_string(),
            repository_key_epoch: u64::try_from(grant.repository_key_epoch)
                .map_err(|_| integrity_error())?,
            sink_id: grant.sink_identity.to_string(),
            sink_version: grant.sink_version.to_string(),
            sink_authority: grant.sink_authority.to_string(),
            authority_key_epoch: u64::try_from(grant.authority_key_epoch)
                .map_err(|_| integrity_error())?,
            authority_key_fingerprint: grant.authority_key_fingerprint.to_string(),
            fence_scope: grant.fence_scope.to_string(),
            grant_id: grant.grant_id.to_string(),
            effect_id: grant.effect_id.to_string(),
            fence_epoch: grant.fence_epoch,
            target_digest: grant.target_digest.to_string(),
            canonical_digest: grant.canonical_digest.to_string(),
            grant_binding: grant.command_binding.to_string(),
        })
    }

    pub(crate) fn effect_id(&self) -> &str {
        &self.effect_id
    }

    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }

    pub(crate) const fn fence_epoch(&self) -> i64 {
        self.fence_epoch
    }

    pub(crate) fn target_digest(&self) -> &str {
        &self.target_digest
    }

    pub(crate) fn canonical_digest(&self) -> &str {
        &self.canonical_digest
    }

    pub(crate) fn grant_binding(&self) -> &str {
        &self.grant_binding
    }

    fn validates_for_sink(&self, sink: &TrustedFencedProjectionSink<'_>) -> bool {
        self.sink_id == sink.adapter_id()
            && self.sink_version == sink.adapter_version()
            && self.sink_authority == sink.authority_id
            && self.authority_key_epoch == sink.authority_key_epoch
            && self.authority_key_fingerprint == sink.authority_key_fingerprint
            && self.fence_scope == "global"
            && self.fence_epoch >= 1
            && !self.repository_instance_id.is_empty()
            && self.repository_key_epoch > 0
            && self.repository_path_binding.len() == 64
            && self.grant_binding.len() == 64
            && i64::try_from(self.repository_key_epoch).is_ok_and(|repository_key_epoch| {
                self.grant_binding
                    == fenced_command_binding(
                        &self.repository_instance_id,
                        &self.repository_path_binding,
                        repository_key_epoch,
                        &self.sink_id,
                        &self.sink_version,
                        &self.sink_authority,
                        i64::try_from(self.authority_key_epoch).unwrap_or_default(),
                        &self.authority_key_fingerprint,
                        &self.grant_id,
                        &self.effect_id,
                        self.fence_epoch,
                        &self.target_digest,
                        &self.canonical_digest,
                    )
            })
    }
}

/// A durable sink receipt. A sink may return the same receipt for a duplicate
/// `effect_id`, but it must never perform the external effect twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FencedEffectReceipt {
    receipt_id: String,
    sink_id: String,
    sink_version: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    grant_id: String,
    effect_id: String,
    fence_epoch: i64,
    target_digest: String,
    grant_binding: String,
}

impl FencedEffectReceipt {
    #[cfg(test)]
    pub(crate) fn for_command(
        command: &FencedEffectCommand,
        receipt_id: String,
    ) -> McpPlatformResult<Self> {
        Self::from_command(command, receipt_id)
    }

    fn validates_for(
        &self,
        command: &FencedEffectCommand,
        sink: &TrustedFencedProjectionSink<'_>,
    ) -> bool {
        validate_uuid(&self.receipt_id).is_ok()
            && self.sink_id == sink.adapter_id()
            && self.sink_version == sink.adapter_version()
            && self.authority_key_epoch == sink.authority_key_epoch
            && self.authority_key_fingerprint == sink.authority_key_fingerprint
            && self.grant_id == command.grant_id
            && self.effect_id == command.effect_id
            && self.fence_epoch == command.fence_epoch
            && self.target_digest == command.target_digest
            && self.grant_binding == command.grant_binding
    }
}

/// A sink implementation receives this issuer only from the reviewed registry
/// path. It makes receipts constructible for a future trusted adapter without
/// exposing a general crate-local receipt constructor.
pub(crate) struct FencedReceiptIssuer {
    authority_id: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    runtime_gate: Arc<RuntimeCallGate>,
}

impl FencedReceiptIssuer {
    pub(crate) fn issue(
        &self,
        command: &FencedEffectCommand,
        execution: &FencedEffectExecution,
        receipt_id: String,
    ) -> McpPlatformResult<FencedEffectReceipt> {
        validate_authority_id(&self.authority_id)?;
        validate_authority_key(self.authority_key_epoch, &self.authority_key_fingerprint)?;
        if command.sink_authority != self.authority_id
            || command.authority_key_epoch != self.authority_key_epoch
            || command.authority_key_fingerprint != self.authority_key_fingerprint
            || !execution.is_for_runtime_gate(&self.runtime_gate)
        {
            return Err(integrity_error());
        }
        FencedEffectReceipt::from_command(command, receipt_id)
    }
}

impl FencedEffectReceipt {
    fn from_command(command: &FencedEffectCommand, receipt_id: String) -> McpPlatformResult<Self> {
        validate_uuid(&receipt_id)?;
        Ok(Self {
            receipt_id,
            sink_id: command.sink_id.clone(),
            sink_version: command.sink_version.clone(),
            authority_key_epoch: command.authority_key_epoch,
            authority_key_fingerprint: command.authority_key_fingerprint.clone(),
            grant_id: command.grant_id.clone(),
            effect_id: command.effect_id.clone(),
            fence_epoch: command.fence_epoch,
            target_digest: command.target_digest.clone(),
            grant_binding: command.grant_binding.clone(),
        })
    }
}

impl FencedEffectReceipt {
    fn accept(
        self,
        command: &FencedEffectCommand,
        sink: &TrustedFencedProjectionSink<'_>,
        execution: &FencedEffectExecution,
    ) -> McpPlatformResult<AcceptedFencedReceipt> {
        if !self.validates_for(command, sink)
            || !command.validates_for_sink(sink)
            || !execution.is_for_sink(sink)
        {
            return Err(integrity_error());
        }
        let receipt_binding = fenced_receipt_binding(
            &self.receipt_id,
            &self.sink_id,
            &self.sink_version,
            &sink.authority_id,
            sink.authority_key_epoch,
            &sink.authority_key_fingerprint,
            &self.grant_id,
            &self.effect_id,
            self.fence_epoch,
            &self.target_digest,
            &self.grant_binding,
        );
        Ok(AcceptedFencedReceipt {
            receipt_id: self.receipt_id,
            sink_identity: self.sink_id,
            sink_version: self.sink_version,
            sink_authority: sink.authority_id.clone(),
            authority_key_epoch: sink.authority_key_epoch,
            authority_key_fingerprint: sink.authority_key_fingerprint.clone(),
            grant_id: self.grant_id,
            effect_id: self.effect_id,
            fence_epoch: self.fence_epoch,
            target_digest: self.target_digest,
            command_binding: self.grant_binding,
            receipt_binding,
        })
    }
}

/// A sink that can atomically condition an effect on the supplied fence and
/// durably look up the resulting receipt. `ProjectionSink` does not satisfy
/// this trait and is intentionally not adapted here. For a remote sink, the
/// registry must additionally require a receipt-authority signature before it
/// can issue `TrustedFencedProjectionSink`; V42 persists an authority identity
/// but does not claim to implement remote receipt signatures.
#[async_trait]
pub(crate) trait FencedProjectionSink: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn adapter_version(&self) -> &'static str;

    async fn apply_conditional(
        &self,
        command: &FencedEffectCommand,
        execution: &FencedEffectExecution,
    ) -> McpPlatformResult<FencedEffectReceipt>;

    async fn lookup_receipt(
        &self,
        command: &FencedEffectCommand,
        execution: &FencedEffectExecution,
    ) -> McpPlatformResult<Option<FencedEffectReceipt>>;
}

/// A trusted registration is a capability, not a name match. Only the runtime
/// registry may create the owned variant; tests retain a borrow-only helper.
/// A same-crate module is not a hostile-code boundary, but public plugins and
/// UI/request paths cannot name either construction path.
#[derive(Clone)]
pub(crate) struct TrustedFencedProjectionSink<'a> {
    sink: FencedProjectionSinkReference<'a>,
    authority_id: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    runtime_gate: Option<Arc<RuntimeCallGate>>,
}

#[derive(Clone)]
enum FencedProjectionSinkReference<'a> {
    Borrowed(&'a dyn FencedProjectionSink),
    Registered(Arc<dyn FencedProjectionSink>),
}

impl TrustedFencedProjectionSink<'_> {
    fn sink(&self) -> &dyn FencedProjectionSink {
        match &self.sink {
            FencedProjectionSinkReference::Borrowed(sink) => *sink,
            FencedProjectionSinkReference::Registered(sink) => sink.as_ref(),
        }
    }

    fn adapter_id(&self) -> &'static str {
        self.sink().adapter_id()
    }

    fn adapter_version(&self) -> &'static str {
        self.sink().adapter_version()
    }

    pub(crate) fn ensure_live(&self) -> McpPlatformResult<()> {
        if self.runtime_gate.as_ref().is_none_or(|gate| gate.is_open()) {
            Ok(())
        } else {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::NotImplementedForPhase,
                "fenced projection sink runtime registration is unavailable",
            ))
        }
    }

    fn begin_execution(&self) -> McpPlatformResult<FencedEffectExecution> {
        Ok(FencedEffectExecution {
            call_lease: self
                .runtime_gate
                .as_ref()
                .map(|gate| gate.acquire())
                .transpose()?,
        })
    }
}

/// A non-cloneable, stack-scoped proof that a runtime call entered before the
/// gate closed. Commands deliberately do not contain this proof: retaining a
/// command cannot keep shutdown draining indefinitely.
pub(crate) struct FencedEffectExecution {
    call_lease: Option<RuntimeCallLease>,
}

impl FencedEffectExecution {
    fn is_for_runtime_gate(&self, gate: &Arc<RuntimeCallGate>) -> bool {
        self.call_lease
            .as_ref()
            .is_some_and(|lease| lease.is_for(gate))
    }

    fn is_for_sink(&self, sink: &TrustedFencedProjectionSink<'_>) -> bool {
        match &sink.runtime_gate {
            Some(gate) => self.is_for_runtime_gate(gate),
            None => self.call_lease.is_none(),
        }
    }
}

pub(crate) struct RuntimeCallGate {
    state: Mutex<RuntimeCallGateState>,
    drained: Condvar,
}

struct RuntimeCallGateState {
    open: bool,
    inflight: usize,
}

impl RuntimeCallGate {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RuntimeCallGateState {
                open: true,
                inflight: 0,
            }),
            drained: Condvar::new(),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state.lock().expect("runtime gate mutex poisoned").open
    }

    fn acquire(self: &Arc<Self>) -> McpPlatformResult<RuntimeCallLease> {
        let mut state = self.state.lock().expect("runtime gate mutex poisoned");
        if !state.open {
            return Err(runtime_unavailable());
        }
        state.inflight += 1;
        drop(state);
        Ok(RuntimeCallLease {
            state: RuntimeCallLeaseState { gate: self.clone() },
        })
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("runtime gate mutex poisoned");
        state.open = false;
        if state.inflight == 0 {
            self.drained.notify_all();
        }
    }

    pub(crate) fn wait_for_drain(&self) {
        let mut state = self.state.lock().expect("runtime gate mutex poisoned");
        while state.inflight != 0 {
            state = self
                .drained
                .wait(state)
                .expect("runtime gate mutex poisoned");
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("runtime gate mutex poisoned");
        debug_assert!(state.inflight > 0);
        state.inflight -= 1;
        if state.inflight == 0 && !state.open {
            self.drained.notify_all();
        }
    }

    #[cfg(test)]
    fn inflight(&self) -> usize {
        self.state
            .lock()
            .expect("runtime gate mutex poisoned")
            .inflight
    }
}

struct RuntimeCallLeaseState {
    gate: Arc<RuntimeCallGate>,
}

impl Drop for RuntimeCallLeaseState {
    fn drop(&mut self) {
        self.gate.release();
    }
}

struct RuntimeCallLease {
    state: RuntimeCallLeaseState,
}

impl RuntimeCallLease {
    fn is_for(&self, gate: &Arc<RuntimeCallGate>) -> bool {
        Arc::ptr_eq(&self.state.gate, gate)
    }
}

/// Creates the owned capability issued exclusively by the V43 runtime registry.
/// The registry owns `liveness` and invalidates every capability it issued when
/// it is dropped, so an old runtime capability cannot be replayed after restart.
pub(crate) fn trusted_runtime_fenced_sink(
    _bootstrap: &RuntimeFencedSinkBootstrapAuthority,
    sink: Arc<dyn FencedProjectionSink>,
    authority_id: String,
    authority_key_epoch: u64,
    authority_key_fingerprint: String,
    runtime_gate: Arc<RuntimeCallGate>,
) -> McpPlatformResult<(TrustedFencedProjectionSink<'static>, FencedReceiptIssuer)> {
    validate_sink_identity(sink.adapter_id(), sink.adapter_version())?;
    validate_authority_id(&authority_id)?;
    validate_authority_key(authority_key_epoch, &authority_key_fingerprint)?;
    if !runtime_gate.is_open() {
        return Err(runtime_unavailable());
    }
    Ok((
        TrustedFencedProjectionSink {
            sink: FencedProjectionSinkReference::Registered(sink),
            authority_id: authority_id.clone(),
            authority_key_epoch,
            authority_key_fingerprint: authority_key_fingerprint.clone(),
            runtime_gate: Some(runtime_gate.clone()),
        },
        FencedReceiptIssuer {
            authority_id,
            authority_key_epoch,
            authority_key_fingerprint,
            runtime_gate,
        },
    ))
}

#[cfg(test)]
fn trusted_test_sink(sink: &dyn FencedProjectionSink) -> TrustedFencedProjectionSink<'_> {
    trusted_test_sink_with_authority(sink, "in-memory-test-authority-v42")
}

#[cfg(test)]
fn trusted_test_sink_with_authority<'a>(
    sink: &'a dyn FencedProjectionSink,
    authority_id: &str,
) -> TrustedFencedProjectionSink<'a> {
    TrustedFencedProjectionSink {
        sink: FencedProjectionSinkReference::Borrowed(sink),
        authority_id: authority_id.to_string(),
        authority_key_epoch: 1,
        authority_key_fingerprint: "0".repeat(64),
        runtime_gate: None,
    }
}

#[cfg(test)]
fn trusted_test_runtime_sink(
    sink: Arc<dyn FencedProjectionSink>,
    runtime_gate: Arc<RuntimeCallGate>,
) -> TrustedFencedProjectionSink<'static> {
    TrustedFencedProjectionSink {
        sink: FencedProjectionSinkReference::Registered(sink),
        authority_id: "in-memory-test-authority-v42".to_string(),
        authority_key_epoch: 1,
        authority_key_fingerprint: "0".repeat(64),
        runtime_gate: Some(runtime_gate),
    }
}

#[cfg(test)]
pub(crate) fn test_sink_issue_binding() -> McpPlatformResult<FencedSinkIssueBinding> {
    let sink = TestOnlyBindingSink;
    FencedSinkIssueBinding::from_trusted(&trusted_test_sink(&sink))
}

#[cfg(test)]
struct TestOnlyBindingSink;

#[cfg(test)]
#[async_trait]
impl FencedProjectionSink for TestOnlyBindingSink {
    fn adapter_id(&self) -> &'static str {
        "fenced_test_sink"
    }
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    async fn apply_conditional(
        &self,
        _: &FencedEffectCommand,
        _: &FencedEffectExecution,
    ) -> McpPlatformResult<FencedEffectReceipt> {
        Err(integrity_error())
    }
    async fn lookup_receipt(
        &self,
        _: &FencedEffectCommand,
        _: &FencedEffectExecution,
    ) -> McpPlatformResult<Option<FencedEffectReceipt>> {
        Err(integrity_error())
    }
}

/// A runtime must explicitly present conditional receipt capability. V42 has no
/// adapter from the legacy projection sink, so an unavailable capability is a
/// terminal, fail-closed outcome rather than a best-effort fallback.
pub(crate) enum FencedProjectionSinkCapability<'a> {
    Available(TrustedFencedProjectionSink<'a>),
    Unavailable,
}

/// The only V42 execution path. Both SQLite updates are short transactions;
/// `apply_conditional` and receipt lookup are awaited after the transaction has
/// committed. Sink errors leave the grant in `consuming`, never re-apply it.
#[derive(Clone)]
pub(crate) struct FencedProjectionEffectExecutor {
    repository: SqliteMcpPlatformRepository,
    #[cfg(test)]
    before_issue_bound_grant: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FencedProjectionEffectExecutor {
    pub(crate) fn new(repository: SqliteMcpPlatformRepository) -> Self {
        Self {
            repository,
            #[cfg(test)]
            before_issue_bound_grant: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_before_issue_bound_grant(
        mut self,
        hook: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.before_issue_bound_grant = Some(hook);
        self
    }

    pub(crate) async fn execute(
        &self,
        guard: &EffectFenceGuard,
        sink: TrustedFencedProjectionSink<'_>,
        request: FencedProjectionEffectRequest,
        now_ms: i64,
    ) -> McpPlatformResult<FencedEffectReceipt> {
        if now_ms < 0 {
            return Err(integrity_error());
        }
        sink.ensure_live()?;
        let binding = FencedSinkIssueBinding::from_trusted(&sink)?;
        #[cfg(test)]
        if let Some(hook) = &self.before_issue_bound_grant {
            hook();
        }
        let grant = self
            .repository
            .issue_bound_fenced_projection_effect_grant(
                guard,
                BoundFencedProjectionEffectGrant {
                    effect_id: &request.effect_id,
                    target_digest: &request.target_digest,
                    sink: binding,
                    now_ms,
                },
            )
            .await?;
        self.repository
            .consume_projection_effect_grant(guard, &grant, now_ms)
            .await?;
        let command = command_from_issued(&grant)?;
        let (receipt, accepted) = {
            let execution = sink.begin_execution()?;
            let receipt = sink.sink().apply_conditional(&command, &execution).await?;
            let accepted = receipt.clone().accept(&command, &sink, &execution)?;
            (receipt, accepted)
        };
        self.finish_accepted_receipt(guard, &command, accepted, now_ms)
            .await?;
        Ok(receipt)
    }

    pub(crate) async fn execute_with_capability(
        &self,
        guard: &EffectFenceGuard,
        capability: FencedProjectionSinkCapability<'_>,
        request: FencedProjectionEffectRequest,
        now_ms: i64,
    ) -> McpPlatformResult<FencedEffectReceipt> {
        let FencedProjectionSinkCapability::Available(sink) = capability else {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::NotImplementedForPhase,
                "projection recovery requires a fenced sink with durable receipts",
            ));
        };
        self.execute(guard, sink, request, now_ms).await
    }

    /// Reconciliation is lookup-only: it never calls `apply_conditional`.
    pub(crate) async fn reconcile_consuming(
        &self,
        guard: &EffectFenceGuard,
        sink: TrustedFencedProjectionSink<'_>,
        effect_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Option<FencedEffectReceipt>> {
        if now_ms < 0 {
            return Err(integrity_error());
        }
        sink.ensure_live()?;
        let grant = self
            .repository
            .load_consuming_projection_effect_grant(effect_id)
            .await?;
        let command = command_from_consuming(&grant)?;
        if !command.validates_for_sink(&sink) {
            return Err(integrity_error());
        }
        let Some((receipt, accepted)) = ({
            let execution = sink.begin_execution()?;
            let receipt = sink.sink().lookup_receipt(&command, &execution).await?;
            receipt
                .map(|receipt| {
                    receipt
                        .clone()
                        .accept(&command, &sink, &execution)
                        .map(|accepted| (receipt, accepted))
                })
                .transpose()?
        }) else {
            return Ok(None);
        };
        self.finish_accepted_receipt(guard, &command, accepted, now_ms)
            .await?;
        Ok(Some(receipt))
    }

    async fn finish_accepted_receipt(
        &self,
        guard: &EffectFenceGuard,
        command: &FencedEffectCommand,
        accepted: AcceptedFencedReceipt,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.repository
            .finish_projection_effect_grant_with_conditional_receipt(
                guard,
                FinishProjectionEffectGrant {
                    grant_id: command.grant_id(),
                    now_ms,
                },
                accepted,
            )
            .await?;
        Ok(())
    }
}

struct GrantFields<'a> {
    grant_id: &'a str,
    effect_id: &'a str,
    fence_scope: &'a str,
    fence_epoch: i64,
    target_digest: &'a str,
    canonical_digest: &'a str,
    sink_identity: &'a str,
    sink_version: &'a str,
    sink_authority: &'a str,
    authority_key_epoch: i64,
    authority_key_fingerprint: &'a str,
    command_binding: &'a str,
    repository_instance_id: &'a str,
    repository_path_binding: &'a str,
    repository_key_epoch: i64,
}

fn command_from_issued(
    grant: &IssuedProjectionEffectGrant,
) -> McpPlatformResult<FencedEffectCommand> {
    FencedEffectCommand::from_grant(GrantFields {
        grant_id: grant.grant_id(),
        effect_id: grant.effect_id(),
        fence_scope: "global",
        fence_epoch: grant.fence_epoch(),
        target_digest: grant.target_digest(),
        canonical_digest: grant.canonical_digest(),
        sink_identity: &grant.sink_identity,
        sink_version: &grant.sink_version,
        sink_authority: &grant.sink_authority,
        authority_key_epoch: grant.authority_key_epoch,
        authority_key_fingerprint: &grant.authority_key_fingerprint,
        command_binding: &grant.command_binding,
        repository_instance_id: &grant.repository_instance_id,
        repository_path_binding: &grant.repository_path_binding,
        repository_key_epoch: grant.repository_key_epoch,
    })
}

fn command_from_consuming(
    grant: &ConsumingProjectionEffectGrant,
) -> McpPlatformResult<FencedEffectCommand> {
    FencedEffectCommand::from_grant(GrantFields {
        grant_id: &grant.grant_id,
        effect_id: &grant.effect_id,
        fence_scope: &grant.fence_scope,
        fence_epoch: grant.fence_epoch,
        target_digest: &grant.target_digest,
        canonical_digest: &grant.canonical_digest,
        sink_identity: &grant.sink_identity,
        sink_version: &grant.sink_version,
        sink_authority: &grant.sink_authority,
        authority_key_epoch: grant.authority_key_epoch,
        authority_key_fingerprint: &grant.authority_key_fingerprint,
        command_binding: &grant.command_binding,
        repository_instance_id: &grant.repository_instance_id,
        repository_path_binding: &grant.repository_path_binding,
        repository_key_epoch: grant.repository_key_epoch,
    })
}

fn binding_digest(fields: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    crate::utils::bytes_to_hex(hasher.finalize())
}

pub(crate) fn fenced_command_binding(
    repository_instance_id: &str,
    repository_path_binding: &str,
    repository_key_epoch: i64,
    sink_identity: &str,
    sink_version: &str,
    sink_authority: &str,
    authority_key_epoch: i64,
    authority_key_fingerprint: &str,
    grant_id: &str,
    effect_id: &str,
    fence_epoch: i64,
    target_digest: &str,
    canonical_digest: &str,
) -> String {
    binding_digest(&[
        FENCED_EFFECT_COMMAND_DOMAIN,
        repository_instance_id.as_bytes(),
        repository_path_binding.as_bytes(),
        repository_key_epoch.to_string().as_bytes(),
        sink_identity.as_bytes(),
        sink_version.as_bytes(),
        sink_authority.as_bytes(),
        authority_key_epoch.to_string().as_bytes(),
        authority_key_fingerprint.as_bytes(),
        b"global",
        grant_id.as_bytes(),
        effect_id.as_bytes(),
        fence_epoch.to_string().as_bytes(),
        target_digest.as_bytes(),
        canonical_digest.as_bytes(),
    ])
}

pub(crate) fn fenced_receipt_binding(
    receipt_id: &str,
    sink_identity: &str,
    sink_version: &str,
    sink_authority: &str,
    authority_key_epoch: u64,
    authority_key_fingerprint: &str,
    grant_id: &str,
    effect_id: &str,
    fence_epoch: i64,
    target_digest: &str,
    command_binding: &str,
) -> String {
    binding_digest(&[
        b"goose.mcp-platform.fenced-projection-receipt-v43",
        receipt_id.as_bytes(),
        sink_identity.as_bytes(),
        sink_version.as_bytes(),
        sink_authority.as_bytes(),
        authority_key_epoch.to_string().as_bytes(),
        authority_key_fingerprint.as_bytes(),
        grant_id.as_bytes(),
        effect_id.as_bytes(),
        fence_epoch.to_string().as_bytes(),
        target_digest.as_bytes(),
        command_binding.as_bytes(),
    ])
}

fn validate_sink_identity(id: &str, version: &str) -> McpPlatformResult<()> {
    if id.is_empty()
        || version.is_empty()
        || id.len() > 128
        || version.len() > 128
        || id.contains('\0')
        || version.contains('\0')
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn validate_authority_id(value: &str) -> McpPlatformResult<()> {
    if value.is_empty() || value.len() > 128 || value.contains('\0') {
        return Err(integrity_error());
    }
    Ok(())
}

fn validate_authority_key(epoch: u64, fingerprint: &str) -> McpPlatformResult<()> {
    if epoch == 0 || validate_digest(fingerprint).is_err() {
        return Err(integrity_error());
    }
    Ok(())
}

fn runtime_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::NotImplementedForPhase,
        "fenced projection sink runtime registration is unavailable",
    )
}

fn validate_uuid(value: &str) -> McpPlatformResult<()> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| integrity_error())?;
    if parsed.hyphenated().to_string() != value {
        return Err(integrity_error());
    }
    Ok(())
}

fn validate_digest(value: &str) -> McpPlatformResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "fenced projection sink verification failed",
    )
}

#[cfg(all(test, windows))]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use sqlx::{Connection, Executor, SqliteConnection};

    use super::*;
    use crate::mcp_platform::repository::{InMemoryIntegritySigner, IntegritySigner};

    struct ReceiptStore {
        _directory: tempfile::TempDir,
        path: PathBuf,
    }

    impl ReceiptStore {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipts");
            std::fs::File::create(&path).unwrap();
            Self {
                _directory: directory,
                path,
            }
        }
    }

    struct FakeSink {
        receipt_store_path: PathBuf,
        apply_count: Mutex<u64>,
        lookup_count: Mutex<u64>,
        required_epoch: Mutex<Option<i64>>,
        fail_apply: bool,
        fail_after_apply: bool,
        wrong_receipt: bool,
        repository: Option<SqliteMcpPlatformRepository>,
        identity: (&'static str, &'static str),
        apply_started: Option<mpsc::SyncSender<()>>,
        release_apply: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    }

    fn new_fake_sink(
        receipt_store_path: PathBuf,
        repository: Option<SqliteMcpPlatformRepository>,
    ) -> FakeSink {
        FakeSink {
            receipt_store_path,
            apply_count: Mutex::new(0),
            lookup_count: Mutex::new(0),
            required_epoch: Mutex::new(None),
            fail_apply: false,
            fail_after_apply: false,
            wrong_receipt: false,
            repository,
            identity: ("fenced_test_sink", "1"),
            apply_started: None,
            release_apply: None,
        }
    }

    impl FakeSink {
        fn applies(&self) -> u64 {
            *self.apply_count.lock().unwrap()
        }

        fn lookups(&self) -> u64 {
            *self.lookup_count.lock().unwrap()
        }

        fn load_receipt(&self, effect_id: &str) -> McpPlatformResult<Option<FencedEffectReceipt>> {
            let contents =
                std::fs::read_to_string(&self.receipt_store_path).map_err(|_| integrity_error())?;
            for line in contents.lines() {
                let fields = line.split('\t').collect::<Vec<_>>();
                if fields.len() != 10 || fields[6] != effect_id {
                    continue;
                }
                return Ok(Some(FencedEffectReceipt {
                    receipt_id: fields[0].to_string(),
                    sink_id: fields[1].to_string(),
                    sink_version: fields[2].to_string(),
                    authority_key_epoch: fields[3].parse().map_err(|_| integrity_error())?,
                    authority_key_fingerprint: fields[4].to_string(),
                    grant_id: fields[5].to_string(),
                    effect_id: fields[6].to_string(),
                    fence_epoch: fields[7].parse().map_err(|_| integrity_error())?,
                    target_digest: fields[8].to_string(),
                    grant_binding: fields[9].to_string(),
                }));
            }
            Ok(None)
        }

        fn persist_receipt(&self, receipt: &FencedEffectReceipt) -> McpPlatformResult<()> {
            let record = format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                receipt.receipt_id,
                receipt.sink_id,
                receipt.sink_version,
                receipt.authority_key_epoch,
                receipt.authority_key_fingerprint,
                receipt.grant_id,
                receipt.effect_id,
                receipt.fence_epoch,
                receipt.target_digest,
                receipt.grant_binding,
            );
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.receipt_store_path)
                .map_err(|_| integrity_error())?;
            file.write_all(record.as_bytes())
                .map_err(|_| integrity_error())?;
            file.sync_all().map_err(|_| integrity_error())
        }
    }

    #[async_trait::async_trait]
    impl FencedProjectionSink for FakeSink {
        fn adapter_id(&self) -> &'static str {
            self.identity.0
        }

        fn adapter_version(&self) -> &'static str {
            self.identity.1
        }

        async fn apply_conditional(
            &self,
            command: &FencedEffectCommand,
            _: &FencedEffectExecution,
        ) -> McpPlatformResult<FencedEffectReceipt> {
            let trusted = trusted_test_sink(self);
            if !command.validates_for_sink(&trusted)
                || self
                    .required_epoch
                    .lock()
                    .unwrap()
                    .is_some_and(|epoch| epoch != command.fence_epoch())
            {
                return Err(integrity_error());
            }
            if let Some(repository) = &self.repository {
                repository
                    .load_consuming_projection_effect_grant(command.effect_id())
                    .await?;
            }
            if let Some(started) = &self.apply_started {
                started.send(()).map_err(|_| integrity_error())?;
            }
            if let Some(release) = &self.release_apply {
                release
                    .lock()
                    .map_err(|_| integrity_error())?
                    .recv()
                    .map_err(|_| integrity_error())?;
            }
            if self.fail_apply {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::RepositoryUnavailable,
                    "simulated sink timeout after consume",
                ));
            }
            if let Some(receipt) = self.load_receipt(command.effect_id())? {
                return Ok(receipt);
            }
            *self.apply_count.lock().unwrap() += 1;
            let mut receipt =
                FencedEffectReceipt::for_command(&command, uuid::Uuid::new_v4().to_string())?;
            if self.wrong_receipt {
                receipt.target_digest = "0".repeat(64);
            }
            self.persist_receipt(&receipt)?;
            if self.fail_after_apply {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::RepositoryUnavailable,
                    "simulated crash after external success before finish",
                ));
            }
            Ok(receipt)
        }

        async fn lookup_receipt(
            &self,
            command: &FencedEffectCommand,
            _: &FencedEffectExecution,
        ) -> McpPlatformResult<Option<FencedEffectReceipt>> {
            *self.lookup_count.lock().unwrap() += 1;
            let trusted = trusted_test_sink(self);
            if !command.validates_for_sink(&trusted) {
                return Err(integrity_error());
            }
            if self.wrong_receipt {
                let mut receipt =
                    FencedEffectReceipt::for_command(command, uuid::Uuid::new_v4().to_string())?;
                receipt.target_digest = "0".repeat(64);
                return Ok(Some(receipt));
            }
            self.load_receipt(command.effect_id())
        }
    }

    async fn repository() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Arc<dyn IntegritySigner>,
        SqliteMcpPlatformRepository,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fenced-projection.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x42; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        (directory, path, signer, repository)
    }

    fn request() -> FencedProjectionEffectRequest {
        FencedProjectionEffectRequest::new(uuid::Uuid::new_v4().to_string(), "a".repeat(64))
            .unwrap()
    }

    fn runtime_command() -> FencedEffectCommand {
        FencedEffectCommand {
            repository_instance_id: "test-repository".to_string(),
            repository_path_binding: "a".repeat(64),
            repository_key_epoch: 1,
            sink_id: "fenced_test_sink".to_string(),
            sink_version: "1".to_string(),
            sink_authority: "runtime-test-authority".to_string(),
            authority_key_epoch: 1,
            authority_key_fingerprint: "b".repeat(64),
            fence_scope: "global".to_string(),
            grant_id: uuid::Uuid::new_v4().to_string(),
            effect_id: uuid::Uuid::new_v4().to_string(),
            fence_epoch: 1,
            target_digest: "c".repeat(64),
            canonical_digest: "d".repeat(64),
            grant_binding: "e".repeat(64),
        }
    }

    #[test]
    fn close_rejects_new_issuer_calls_but_allows_the_call_that_already_started() {
        let gate = Arc::new(RuntimeCallGate::new());
        let issuer = FencedReceiptIssuer {
            authority_id: "runtime-test-authority".to_string(),
            authority_key_epoch: 1,
            authority_key_fingerprint: "b".repeat(64),
            runtime_gate: gate.clone(),
        };
        let command = runtime_command();
        let execution = FencedEffectExecution {
            call_lease: Some(gate.acquire().unwrap()),
        };

        gate.close();
        assert!(issuer
            .issue(&command, &execution, uuid::Uuid::new_v4().to_string())
            .is_ok());
        assert!(issuer
            .issue(
                &command,
                &FencedEffectExecution { call_lease: None },
                uuid::Uuid::new_v4().to_string(),
            )
            .is_err());
        drop(execution);
        gate.wait_for_drain();
    }

    #[test]
    fn retaining_a_command_never_holds_the_runtime_drain() {
        let gate = Arc::new(RuntimeCallGate::new());
        let command = runtime_command();

        gate.close();
        gate.wait_for_drain();

        assert_eq!(command.effect_id().len(), 36);
        assert_eq!(gate.inflight(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_and_drain_tracks_external_sink_leases_not_slow_execute_database_work() {
        let (_directory, path, _signer, first_repository) = repository().await;
        let guard = Arc::new(
            first_repository
                .acquire_projection_effect_fence()
                .await
                .unwrap(),
        );
        let runtime_gate = Arc::new(RuntimeCallGate::new());
        let receipt_store = ReceiptStore::new();
        let sink = Arc::new(new_fake_sink(receipt_store.path.clone(), None));
        let (issue_entered, issue_entered_receiver) = mpsc::sync_channel(1);
        let executor = FencedProjectionEffectExecutor::new(first_repository.clone())
            .with_before_issue_bound_grant(Arc::new(move || {
                issue_entered.send(()).unwrap();
            }));
        let write_lock_options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let mut write_lock = SqliteConnection::connect_with(&write_lock_options)
            .await
            .unwrap();
        write_lock.execute("BEGIN IMMEDIATE").await.unwrap();

        let execute = tokio::spawn({
            let guard = guard.clone();
            let sink = trusted_test_runtime_sink(sink.clone(), runtime_gate.clone());
            async move { executor.execute(&guard, sink, request(), 1).await }
        });
        tokio::task::spawn_blocking(move || {
            issue_entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("execute must reach its production repository await")
        })
        .await
        .unwrap();

        let (drained, drained_receiver) = mpsc::sync_channel(0);
        let closing_gate = runtime_gate.clone();
        let closer = std::thread::spawn(move || {
            closing_gate.close();
            closing_gate.wait_for_drain();
            drained.send(()).unwrap();
        });
        drained_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("close must return while execute is awaiting the SQLite write lock");
        assert_eq!(runtime_gate.inflight(), 0);
        assert_eq!(sink.applies(), 0);

        write_lock.execute("ROLLBACK").await.unwrap();
        assert_eq!(
            execute.await.unwrap().unwrap_err().code(),
            McpPlatformErrorCode::NotImplementedForPhase
        );
        closer.join().unwrap();
        first_repository.close().await;

        let (_directory, _path, _signer, repository) = repository().await;
        let guard = Arc::new(repository.acquire_projection_effect_fence().await.unwrap());
        let runtime_gate = Arc::new(RuntimeCallGate::new());
        let receipt_store = ReceiptStore::new();
        let (apply_started, apply_started_receiver) = mpsc::sync_channel(0);
        let (release_apply, release_apply_receiver) = mpsc::sync_channel(0);
        let sink = Arc::new(FakeSink {
            apply_started: Some(apply_started),
            release_apply: Some(Arc::new(Mutex::new(release_apply_receiver))),
            ..new_fake_sink(receipt_store.path.clone(), None)
        });
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let execute = tokio::spawn({
            let guard = guard.clone();
            let sink = trusted_test_runtime_sink(sink.clone(), runtime_gate.clone());
            async move { executor.execute(&guard, sink, request(), 1).await }
        });
        tokio::task::spawn_blocking(move || {
            apply_started_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("execute must acquire the external sink lease before apply")
        })
        .await
        .unwrap();
        assert_eq!(runtime_gate.inflight(), 1);

        let (closed, closed_receiver) = mpsc::sync_channel(0);
        let closing_gate = runtime_gate.clone();
        let closer = std::thread::spawn(move || {
            closing_gate.close();
            closed.send(()).unwrap();
            closing_gate.wait_for_drain();
        });
        closed_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("close must reject new calls before waiting for an active sink lease");
        assert!(matches!(
            closed_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release_apply.send(()).unwrap();
        execute.await.unwrap().unwrap();
        closer.join().unwrap();
        assert_eq!(runtime_gate.inflight(), 0);
        repository.close().await;
    }

    #[test]
    fn close_and_last_lease_return_race_always_drains() {
        for _ in 0..256 {
            let gate = Arc::new(RuntimeCallGate::new());
            let execution = gate.acquire().unwrap();
            let (drained, drained_receiver) = mpsc::sync_channel(0);
            let closing_gate = gate.clone();
            let closer = std::thread::spawn(move || {
                closing_gate.close();
                closing_gate.wait_for_drain();
                drained.send(()).unwrap();
            });

            drop(execution);
            drained_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("close must observe the final lease return");
            closer.join().unwrap();
        }
    }

    #[tokio::test]
    async fn conditional_receipt_is_idempotent_and_apply_holds_no_sqlite_transaction() {
        let (_directory, _path, _signer, repository) = repository().await;
        let receipt_store = ReceiptStore::new();
        let sink = new_fake_sink(receipt_store.path.clone(), Some(repository.clone()));
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();

        let receipt = executor
            .execute(&guard, trusted_test_sink(&sink), request(), 1)
            .await
            .unwrap();

        assert_eq!(sink.applies(), 1);
        assert_eq!(receipt.sink_id, sink.adapter_id());
        assert!(repository
            .load_consuming_projection_effect_grant(receipt.effect_id.as_str())
            .await
            .is_err());
        repository.close().await;
    }

    #[tokio::test]
    async fn duplicate_effect_id_returns_the_same_receipt_without_a_second_effect() {
        let (_directory, _path, _signer, repository) = repository().await;
        let receipt_store = ReceiptStore::new();
        let sink = new_fake_sink(receipt_store.path.clone(), None);
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &request.effect_id,
                    target_digest: &request.target_digest,
                    now_ms: 1,
                },
            )
            .await
            .unwrap();
        repository
            .consume_projection_effect_grant(&guard, &grant, 1)
            .await
            .unwrap();
        let command = command_from_issued(&grant).unwrap();

        let trusted = trusted_test_sink(&sink);
        let execution = trusted.begin_execution().unwrap();
        let first = sink.apply_conditional(&command, &execution).await.unwrap();
        let second = sink.apply_conditional(&command, &execution).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(sink.applies(), 1);
        repository.close().await;
    }

    #[tokio::test]
    async fn stale_epoch_and_wrong_receipt_fail_closed_without_finishing_grant() {
        let (_directory, _path, _signer, repository) = repository().await;
        let stale_receipt_store = ReceiptStore::new();
        let stale = new_fake_sink(stale_receipt_store.path.clone(), Some(repository.clone()));
        *stale.required_epoch.lock().unwrap() = Some(99);
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let stale_request = request();
        assert!(executor
            .execute(&guard, trusted_test_sink(&stale), stale_request.clone(), 1)
            .await
            .is_err());
        assert!(repository
            .load_consuming_projection_effect_grant(&stale_request.effect_id)
            .await
            .is_ok());
        drop(guard);

        let wrong_receipt_store = ReceiptStore::new();
        let wrong = FakeSink {
            wrong_receipt: true,
            ..new_fake_sink(wrong_receipt_store.path.clone(), Some(repository.clone()))
        };
        let second_guard = repository.acquire_projection_effect_fence().await.unwrap();
        assert!(executor
            .reconcile_consuming(
                &second_guard,
                trusted_test_sink(&wrong),
                &stale_request.effect_id,
                2
            )
            .await
            .is_err());
        assert!(repository
            .load_consuming_projection_effect_grant(&stale_request.effect_id)
            .await
            .is_ok());
        repository.close().await;
    }

    #[tokio::test]
    async fn crash_after_consume_reopens_for_lookup_only_without_replay() {
        let (_directory, path, signer, repository) = repository().await;
        let receipt_store = ReceiptStore::new();
        let durable_store = receipt_store.path.clone();
        let failing_sink = FakeSink {
            fail_apply: true,
            ..new_fake_sink(durable_store.clone(), Some(repository.clone()))
        };
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        assert!(executor
            .execute(&guard, trusted_test_sink(&failing_sink), request.clone(), 1)
            .await
            .is_err());
        assert_eq!(failing_sink.applies(), 0);
        let consuming_grant = repository
            .load_consuming_projection_effect_grant(&request.effect_id)
            .await
            .unwrap();
        let receipt_store_before_reopen = std::fs::read_to_string(&durable_store).unwrap();
        drop(guard);
        repository.close().await;
        drop(failing_sink);

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        let lookup_only_sink = new_fake_sink(durable_store.clone(), Some(reopened.clone()));
        let reopened_executor = FencedProjectionEffectExecutor::new(reopened.clone());
        let reopened_guard = reopened.acquire_projection_effect_fence().await.unwrap();
        assert!(reopened_executor
            .reconcile_consuming(
                &reopened_guard,
                trusted_test_sink(&lookup_only_sink),
                &request.effect_id,
                2
            )
            .await
            .unwrap()
            .is_none());
        assert_eq!(lookup_only_sink.lookups(), 1);
        assert_eq!(lookup_only_sink.applies(), 0);
        assert_eq!(
            reopened
                .load_consuming_projection_effect_grant(&request.effect_id)
                .await
                .unwrap(),
            consuming_grant
        );
        assert_eq!(
            std::fs::read_to_string(&durable_store).unwrap(),
            receipt_store_before_reopen
        );
        reopened.close().await;
    }

    #[tokio::test]
    async fn durable_receipt_lookup_after_restart_finishes_once_without_reapply() {
        let (_directory, path, signer, repository) = repository().await;
        let receipt_store = ReceiptStore::new();
        let durable_store = receipt_store.path.clone();
        let initial_sink = FakeSink {
            fail_after_apply: true,
            ..new_fake_sink(durable_store.clone(), Some(repository.clone()))
        };
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        assert!(executor
            .execute(&guard, trusted_test_sink(&initial_sink), request.clone(), 1)
            .await
            .is_err());
        assert_eq!(initial_sink.applies(), 1);
        drop(guard);
        repository.close().await;
        drop(initial_sink);

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        let lookup_only_sink = new_fake_sink(durable_store, Some(reopened.clone()));
        let reopened_executor = FencedProjectionEffectExecutor::new(reopened.clone());
        let reopened_guard = reopened.acquire_projection_effect_fence().await.unwrap();
        assert!(reopened_executor
            .reconcile_consuming(
                &reopened_guard,
                trusted_test_sink(&lookup_only_sink),
                &request.effect_id,
                2,
            )
            .await
            .unwrap()
            .is_some());
        assert_eq!(lookup_only_sink.lookups(), 1);
        assert_eq!(lookup_only_sink.applies(), 0);
        assert!(reopened
            .load_consuming_projection_effect_grant(&request.effect_id)
            .await
            .is_err());
        reopened.close().await;
    }

    #[tokio::test]
    async fn same_display_different_stable_sink_identity_is_rejected() {
        let (_directory, _path, _signer, repository) = repository().await;
        let primary_receipt_store = ReceiptStore::new();
        let primary = new_fake_sink(primary_receipt_store.path.clone(), Some(repository.clone()));
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        let grant = repository
            .issue_bound_fenced_projection_effect_grant(
                &guard,
                BoundFencedProjectionEffectGrant {
                    effect_id: &request.effect_id,
                    target_digest: &request.target_digest,
                    sink: FencedSinkIssueBinding::from_trusted(&trusted_test_sink(&primary))
                        .unwrap(),
                    now_ms: 1,
                },
            )
            .await
            .unwrap();
        repository
            .consume_projection_effect_grant(&guard, &grant, 1)
            .await
            .unwrap();
        let wrong_identity_receipt_store = ReceiptStore::new();
        let wrong_identity = FakeSink {
            identity: ("fenced_test_sink_other_tenant", "1"),
            ..new_fake_sink(
                wrong_identity_receipt_store.path.clone(),
                Some(repository.clone()),
            )
        };
        assert!(executor
            .reconcile_consuming(
                &guard,
                trusted_test_sink(&wrong_identity),
                &request.effect_id,
                2,
            )
            .await
            .is_err());
        assert_eq!(wrong_identity.applies(), 0);
        repository.close().await;
    }

    #[tokio::test]
    async fn same_identity_and_version_with_wrong_authority_never_looks_up_the_sink() {
        let (_directory, _path, _signer, repository) = repository().await;
        let primary_receipt_store = ReceiptStore::new();
        let primary = new_fake_sink(primary_receipt_store.path.clone(), Some(repository.clone()));
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        let grant = repository
            .issue_bound_fenced_projection_effect_grant(
                &guard,
                BoundFencedProjectionEffectGrant {
                    effect_id: &request.effect_id,
                    target_digest: &request.target_digest,
                    sink: FencedSinkIssueBinding::from_trusted(&trusted_test_sink(&primary))
                        .unwrap(),
                    now_ms: 1,
                },
            )
            .await
            .unwrap();
        repository
            .consume_projection_effect_grant(&guard, &grant, 1)
            .await
            .unwrap();
        let wrong_authority_receipt_store = ReceiptStore::new();
        let wrong_authority = new_fake_sink(
            wrong_authority_receipt_store.path.clone(),
            Some(repository.clone()),
        );

        assert!(executor
            .reconcile_consuming(
                &guard,
                trusted_test_sink_with_authority(&wrong_authority, "other-authority-v42"),
                &request.effect_id,
                2,
            )
            .await
            .is_err());
        assert_eq!(wrong_authority.lookups(), 0);
        assert_eq!(wrong_authority.applies(), 0);
        assert!(repository
            .load_consuming_projection_effect_grant(&request.effect_id)
            .await
            .is_ok());
        repository.close().await;
    }

    #[tokio::test]
    async fn command_key_epoch_change_invalidates_the_persisted_command_binding() {
        let (_directory, _path, _signer, repository) = repository().await;
        let receipt_store = ReceiptStore::new();
        let sink = new_fake_sink(receipt_store.path.clone(), Some(repository.clone()));
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let request = request();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &request.effect_id,
                    target_digest: &request.target_digest,
                    now_ms: 1,
                },
            )
            .await
            .unwrap();
        let mut command = command_from_issued(&grant).unwrap();
        command.repository_key_epoch += 1;

        assert!(!command.validates_for_sink(&trusted_test_sink(&sink)));
        assert_eq!(sink.applies(), 0);
        assert_eq!(sink.lookups(), 0);
        repository.close().await;
    }

    #[tokio::test]
    async fn absent_conditional_capability_is_explicitly_rejected() {
        let (_directory, _path, _signer, repository) = repository().await;
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let guard = repository.acquire_projection_effect_fence().await.unwrap();

        let error = executor
            .execute_with_capability(
                &guard,
                FencedProjectionSinkCapability::Unavailable,
                request(),
                1,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::NotImplementedForPhase);
        repository.close().await;
    }
}
