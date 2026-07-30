use std::fmt;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

use super::super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::{PlatformLocator, PlatformRootState, PlatformStorageContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PlatformStoragePreflightClassification {
    CommittedFamily,
    RootAbsent,
    RootPhysicallyEmpty,
    RootNonempty,
    RootInaccessible,
    RootReparse,
    RootAdsPresent,
    RootHardlinkAlias,
    RootHiddenOrSystemPresent,
    RootIdentityMismatch,
    PartialFamily,
    ResidualPresent,
    ReleasedSidecar,
    Locked,
}

impl PlatformStoragePreflightClassification {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::CommittedFamily => "committed_family",
            Self::RootAbsent => "root_absent",
            Self::RootPhysicallyEmpty => "root_physically_empty",
            Self::RootNonempty => "root_nonempty",
            Self::RootInaccessible => "root_inaccessible",
            Self::RootReparse => "root_reparse",
            Self::RootAdsPresent => "root_ads_present",
            Self::RootHardlinkAlias => "root_hardlink_alias",
            Self::RootHiddenOrSystemPresent => "root_hidden_or_system_present",
            Self::RootIdentityMismatch => "root_identity_mismatch",
            Self::PartialFamily => "partial_family",
            Self::ResidualPresent => "residual_present",
            Self::ReleasedSidecar => "released_sidecar",
            Self::Locked => "locked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ObservedRootSurface {
    Absent,
    PhysicallyEmpty,
    Nonempty,
    Inaccessible,
    Reparse,
    AdsPresent,
    HardlinkAlias,
    HiddenOrSystemPresent,
    IdentityMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct RequiredFamilyArtifacts {
    main_db: bool,
    provider_binding: bool,
    keyring_reference: bool,
    anchor: bool,
}

impl RequiredFamilyArtifacts {
    pub(super) const fn none() -> Self {
        Self {
            main_db: false,
            provider_binding: false,
            keyring_reference: false,
            anchor: false,
        }
    }

    pub(super) fn with_main_db(mut self) -> Self {
        self.main_db = true;
        self
    }

    pub(super) fn with_provider_binding(mut self) -> Self {
        self.provider_binding = true;
        self
    }

    pub(super) fn with_keyring_reference(mut self) -> Self {
        self.keyring_reference = true;
        self
    }

    pub(super) fn with_anchor(mut self) -> Self {
        self.anchor = true;
        self
    }

    fn all_present(self) -> bool {
        self.main_db && self.provider_binding && self.keyring_reference && self.anchor
    }

    fn any_present(self) -> bool {
        self.main_db || self.provider_binding || self.keyring_reference || self.anchor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct OptionalFamilyArtifacts {
    sqlite_wal: bool,
    sqlite_shm: bool,
    sqlite_journal: bool,
}

impl OptionalFamilyArtifacts {
    pub(super) const fn none() -> Self {
        Self {
            sqlite_wal: false,
            sqlite_shm: false,
            sqlite_journal: false,
        }
    }

    pub(super) fn with_sqlite_wal(mut self) -> Self {
        self.sqlite_wal = true;
        self
    }

    pub(super) fn with_sqlite_shm(mut self) -> Self {
        self.sqlite_shm = true;
        self
    }

    pub(super) fn with_sqlite_journal(mut self) -> Self {
        self.sqlite_journal = true;
        self
    }

    fn any_present(self) -> bool {
        self.sqlite_wal || self.sqlite_shm || self.sqlite_journal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct OrphanFamilyArtifacts {
    provider_binding: bool,
    keyring_reference: bool,
    anchor: bool,
    sqlite_wal: bool,
    sqlite_shm: bool,
    sqlite_journal: bool,
}

impl OrphanFamilyArtifacts {
    pub(super) const fn none() -> Self {
        Self {
            provider_binding: false,
            keyring_reference: false,
            anchor: false,
            sqlite_wal: false,
            sqlite_shm: false,
            sqlite_journal: false,
        }
    }

    pub(super) fn with_provider_binding(mut self) -> Self {
        self.provider_binding = true;
        self
    }

    pub(super) fn with_keyring_reference(mut self) -> Self {
        self.keyring_reference = true;
        self
    }

    pub(super) fn with_anchor(mut self) -> Self {
        self.anchor = true;
        self
    }

    pub(super) fn with_sqlite_wal(mut self) -> Self {
        self.sqlite_wal = true;
        self
    }

    pub(super) fn with_sqlite_shm(mut self) -> Self {
        self.sqlite_shm = true;
        self
    }

    pub(super) fn with_sqlite_journal(mut self) -> Self {
        self.sqlite_journal = true;
        self
    }

    fn any_present(self) -> bool {
        self.provider_binding
            || self.keyring_reference
            || self.anchor
            || self.sqlite_wal
            || self.sqlite_shm
            || self.sqlite_journal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct ForbiddenFamilyArtifacts {
    selector_artifact_in_root: bool,
    cache_artifact: bool,
    installations_artifact: bool,
    staging_artifact: bool,
    quarantine_artifact: bool,
    temp_artifact: bool,
    unknown_sidecar: bool,
    unknown_artifact: bool,
}

impl ForbiddenFamilyArtifacts {
    pub(super) const fn none() -> Self {
        Self {
            selector_artifact_in_root: false,
            cache_artifact: false,
            installations_artifact: false,
            staging_artifact: false,
            quarantine_artifact: false,
            temp_artifact: false,
            unknown_sidecar: false,
            unknown_artifact: false,
        }
    }

    pub(super) fn with_selector_artifact_in_root(mut self) -> Self {
        self.selector_artifact_in_root = true;
        self
    }

    pub(super) fn with_cache_artifact(mut self) -> Self {
        self.cache_artifact = true;
        self
    }

    pub(super) fn with_installations_artifact(mut self) -> Self {
        self.installations_artifact = true;
        self
    }

    pub(super) fn with_staging_artifact(mut self) -> Self {
        self.staging_artifact = true;
        self
    }

    pub(super) fn with_quarantine_artifact(mut self) -> Self {
        self.quarantine_artifact = true;
        self
    }

    pub(super) fn with_temp_artifact(mut self) -> Self {
        self.temp_artifact = true;
        self
    }

    pub(super) fn with_unknown_sidecar(mut self) -> Self {
        self.unknown_sidecar = true;
        self
    }

    pub(super) fn with_unknown_artifact(mut self) -> Self {
        self.unknown_artifact = true;
        self
    }

    fn any_present(self) -> bool {
        self.selector_artifact_in_root
            || self.cache_artifact
            || self.installations_artifact
            || self.staging_artifact
            || self.quarantine_artifact
            || self.temp_artifact
            || self.unknown_sidecar
            || self.unknown_artifact
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct PlatformStorageFamilyObservation {
    required: RequiredFamilyArtifacts,
    optional: OptionalFamilyArtifacts,
    orphaned: OrphanFamilyArtifacts,
    forbidden: ForbiddenFamilyArtifacts,
    interrupted_bootstrap: bool,
    released_sidecar: bool,
    live_exclusive_lock: bool,
}

impl PlatformStorageFamilyObservation {
    pub(super) const fn empty() -> Self {
        Self {
            required: RequiredFamilyArtifacts::none(),
            optional: OptionalFamilyArtifacts::none(),
            orphaned: OrphanFamilyArtifacts::none(),
            forbidden: ForbiddenFamilyArtifacts::none(),
            interrupted_bootstrap: false,
            released_sidecar: false,
            live_exclusive_lock: false,
        }
    }

    pub(super) fn with_required(mut self, required: RequiredFamilyArtifacts) -> Self {
        self.required = required;
        self
    }

    pub(super) fn with_optional(mut self, optional: OptionalFamilyArtifacts) -> Self {
        self.optional = optional;
        self
    }

    pub(super) fn with_orphaned(mut self, orphaned: OrphanFamilyArtifacts) -> Self {
        self.orphaned = orphaned;
        self
    }

    pub(super) fn with_forbidden(mut self, forbidden: ForbiddenFamilyArtifacts) -> Self {
        self.forbidden = forbidden;
        self
    }

    pub(super) fn with_interrupted_bootstrap(mut self) -> Self {
        self.interrupted_bootstrap = true;
        self
    }

    pub(super) fn with_released_sidecar(mut self) -> Self {
        self.released_sidecar = true;
        self
    }

    pub(super) fn with_live_exclusive_lock(mut self) -> Self {
        self.live_exclusive_lock = true;
        self
    }

    fn classify_nonempty(self) -> PlatformStoragePreflightClassification {
        if self.live_exclusive_lock {
            return PlatformStoragePreflightClassification::Locked;
        }
        if self.forbidden.any_present() {
            return PlatformStoragePreflightClassification::ResidualPresent;
        }
        if self.is_partial_family() {
            return PlatformStoragePreflightClassification::PartialFamily;
        }
        if self.released_sidecar {
            return PlatformStoragePreflightClassification::ReleasedSidecar;
        }
        if self.is_committed_family() {
            return PlatformStoragePreflightClassification::CommittedFamily;
        }
        PlatformStoragePreflightClassification::RootNonempty
    }

    fn is_committed_family(self) -> bool {
        self.required.all_present() && !self.interrupted_bootstrap && !self.orphaned.any_present()
    }

    fn is_partial_family(self) -> bool {
        self.interrupted_bootstrap
            || self.orphaned.any_present()
            || (self.required.any_present() && !self.required.all_present())
            || (self.optional.any_present() && !self.required.all_present())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReadOnlyStoragePreflightFacts {
    root_surface: ObservedRootSurface,
    family: PlatformStorageFamilyObservation,
}

impl ReadOnlyStoragePreflightFacts {
    pub(super) const fn new(
        root_surface: ObservedRootSurface,
        family: PlatformStorageFamilyObservation,
    ) -> Self {
        Self {
            root_surface,
            family,
        }
    }
}

mod observation_seal {
    #[derive(Clone, PartialEq, Eq)]
    pub(super) struct PlatformStoragePreflightObservationSeal(());

    impl PlatformStoragePreflightObservationSeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PlatformStoragePreflightObservation {
    seal: observation_seal::PlatformStoragePreflightObservationSeal,
    locator: PlatformLocator,
    context: PlatformStorageContext,
    root_surface: ObservedRootSurface,
    family: PlatformStorageFamilyObservation,
}

impl PlatformStoragePreflightObservation {
    fn new(
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        root_surface: ObservedRootSurface,
        family: PlatformStorageFamilyObservation,
    ) -> McpPlatformResult<Self> {
        if !context.matches_locator(locator) {
            return Err(invalid_preflight_observation_error(
                "preflight observation locator/context mismatch",
            ));
        }
        if !root_surface_matches_context(context, root_surface) {
            return Err(invalid_preflight_observation_error(
                "preflight observation root surface does not match context state",
            ));
        }
        Ok(Self {
            seal: observation_seal::PlatformStoragePreflightObservationSeal::new(),
            locator: locator.clone(),
            context: context.clone(),
            root_surface,
            family,
        })
    }

    fn classify(&self) -> PlatformStoragePreflightClassification {
        match self.root_surface {
            ObservedRootSurface::Absent => PlatformStoragePreflightClassification::RootAbsent,
            ObservedRootSurface::PhysicallyEmpty => {
                PlatformStoragePreflightClassification::RootPhysicallyEmpty
            }
            ObservedRootSurface::Nonempty => self.family.classify_nonempty(),
            ObservedRootSurface::Inaccessible => {
                PlatformStoragePreflightClassification::RootInaccessible
            }
            ObservedRootSurface::Reparse => PlatformStoragePreflightClassification::RootReparse,
            ObservedRootSurface::AdsPresent => {
                PlatformStoragePreflightClassification::RootAdsPresent
            }
            ObservedRootSurface::HardlinkAlias => {
                PlatformStoragePreflightClassification::RootHardlinkAlias
            }
            ObservedRootSurface::HiddenOrSystemPresent => {
                PlatformStoragePreflightClassification::RootHiddenOrSystemPresent
            }
            ObservedRootSurface::IdentityMismatch => {
                PlatformStoragePreflightClassification::RootIdentityMismatch
            }
        }
    }
}

fn invalid_preflight_observation_error(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityError, message)
}

fn read_only_preflight_replay_error(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityError, message)
}

fn read_only_preflight_evidence_unavailable(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityUnavailable, message)
}

fn root_surface_matches_context(
    context: &PlatformStorageContext,
    root_surface: ObservedRootSurface,
) -> bool {
    match (context.root_state(), root_surface) {
        (PlatformRootState::Absent, ObservedRootSurface::Absent) => true,
        (PlatformRootState::Absent, _) => false,
        (PlatformRootState::Present, ObservedRootSurface::Absent) => false,
        (PlatformRootState::Present, _) => true,
    }
}

impl fmt::Debug for PlatformStoragePreflightObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformStoragePreflightObservation")
            .field("locator", &self.locator)
            .field("context", &self.context)
            .field("root_surface", &self.root_surface)
            .field("family", &self.family)
            .finish()
    }
}

pub(super) trait ReadOnlyStoragePreflightObserver: Send + Sync {
    fn observe_read_only_preflight(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
    ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts>;
}

static NEXT_PREFLIGHT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn next_preflight_session_id() -> u64 {
    let next = NEXT_PREFLIGHT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    if next == 0 {
        1
    } else {
        next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreflightBindingSnapshot {
    session_id: u64,
    selector_revision: u64,
    observation_epoch: u64,
    root_state: PlatformRootState,
}

mod preflight_capability_seal {
    #[derive(PartialEq, Eq)]
    pub(super) struct PreflightSessionSeal(());

    impl PreflightSessionSeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }

    #[derive(PartialEq, Eq)]
    pub(super) struct ReadOnlyPreflightRequestSeal(());

    impl ReadOnlyPreflightRequestSeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }
}

#[derive(Debug, Default)]
struct PreflightSessionState {
    request_issued: bool,
}

#[derive(Debug, Default)]
struct ReadOnlyPreflightRequestState {
    consumed: bool,
}

pub(super) struct PreflightSession {
    seal: preflight_capability_seal::PreflightSessionSeal,
    binding: PreflightBindingSnapshot,
    locator: PlatformLocator,
    context: PlatformStorageContext,
    observer: Arc<dyn ReadOnlyStoragePreflightObserver>,
    state: Mutex<PreflightSessionState>,
}

impl PreflightSession {
    pub(super) fn new(
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        observer: Arc<dyn ReadOnlyStoragePreflightObserver>,
    ) -> McpPlatformResult<Self> {
        if !context.matches_locator(locator) {
            return Err(read_only_preflight_replay_error(
                "read-only preflight session locator binding drifted",
            ));
        }
        Ok(Self {
            seal: preflight_capability_seal::PreflightSessionSeal::new(),
            binding: PreflightBindingSnapshot {
                session_id: next_preflight_session_id(),
                selector_revision: locator.selector().selector_revision(),
                observation_epoch: context.observation_epoch().get(),
                root_state: context.root_state(),
            },
            locator: locator.clone(),
            context: context.clone(),
            observer,
            state: Mutex::new(PreflightSessionState::default()),
        })
    }

    pub(super) fn issue_read_only_request(&self) -> McpPlatformResult<ReadOnlyPreflightRequest> {
        let mut state = self.state.lock().map_err(|_| {
            read_only_preflight_evidence_unavailable(
                "read-only preflight session state is unavailable",
            )
        })?;
        if state.request_issued {
            return Err(read_only_preflight_replay_error(
                "read-only preflight session cannot be reused",
            ));
        }
        state.request_issued = true;
        Ok(ReadOnlyPreflightRequest {
            seal: preflight_capability_seal::ReadOnlyPreflightRequestSeal::new(),
            binding: self.binding,
            locator: self.locator.clone(),
            context: self.context.clone(),
            observer: Arc::clone(&self.observer),
            state: Mutex::new(ReadOnlyPreflightRequestState::default()),
        })
    }
}

impl fmt::Debug for PreflightSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreflightSession")
            .field("binding", &self.binding)
            .field("locator", &self.locator)
            .field("context", &self.context)
            .finish()
    }
}

pub(super) struct ReadOnlyPreflightRequest {
    seal: preflight_capability_seal::ReadOnlyPreflightRequestSeal,
    binding: PreflightBindingSnapshot,
    locator: PlatformLocator,
    context: PlatformStorageContext,
    observer: Arc<dyn ReadOnlyStoragePreflightObserver>,
    state: Mutex<ReadOnlyPreflightRequestState>,
}

impl ReadOnlyPreflightRequest {
    pub(super) fn consume(&self) -> McpPlatformResult<PlatformStoragePreflightClassification> {
        let mut state = self.state.lock().map_err(|_| {
            read_only_preflight_evidence_unavailable(
                "read-only preflight request state is unavailable",
            )
        })?;
        if state.consumed {
            return Err(read_only_preflight_replay_error(
                "read-only preflight request already consumed",
            ));
        }
        state.consumed = true;
        drop(state);

        self.validate_binding()?;
        assemble_read_only_preflight_classification(
            self.observer.as_ref(),
            &self.locator,
            &self.context,
        )
    }

    fn validate_binding(&self) -> McpPlatformResult<()> {
        if !self.context.matches_locator(&self.locator) {
            return Err(read_only_preflight_replay_error(
                "read-only preflight locator binding drifted",
            ));
        }
        if self.binding.selector_revision != self.locator.selector().selector_revision() {
            return Err(read_only_preflight_replay_error(
                "read-only preflight selector revision drifted",
            ));
        }
        if self.binding.observation_epoch != self.context.observation_epoch().get() {
            return Err(read_only_preflight_replay_error(
                "read-only preflight observation epoch drifted",
            ));
        }
        if self.binding.root_state != self.context.root_state() {
            return Err(read_only_preflight_replay_error(
                "read-only preflight root state drifted",
            ));
        }
        if self.binding.session_id == 0 {
            return Err(read_only_preflight_replay_error(
                "read-only preflight session binding drifted",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for ReadOnlyPreflightRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadOnlyPreflightRequest")
            .field("binding", &self.binding)
            .field("locator", &self.locator)
            .field("context", &self.context)
            .finish()
    }
}

fn assemble_read_only_preflight_classification(
    observer: &dyn ReadOnlyStoragePreflightObserver,
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
) -> McpPlatformResult<PlatformStoragePreflightClassification> {
    let facts = observer.observe_read_only_preflight(locator, context)?;
    let observation = PlatformStoragePreflightObservation::new(
        locator,
        context,
        facts.root_surface,
        facts.family,
    )?;
    Ok(observation.classify())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use crate::mcp_platform::storage_domain::{
        DSelectorRecord, LiveAncestorIdentity, LiveObservationEpoch, LiveRootIdentity,
        LiveVolumeIdentity,
    };

    const MODULE_SOURCE: &str = include_str!("storage_preflight.rs");

    fn selector(root: &str) -> DSelectorRecord {
        DSelectorRecord::new("selector-1", 1, 7, PathBuf::from(root)).unwrap()
    }

    fn absent_locator_and_context() -> (PlatformLocator, PlatformStorageContext) {
        let locator =
            PlatformLocator::from_selector(selector(r"D:\anchor-keyring-binding\platform.db\root"));
        let context = PlatformStorageContext::for_absent_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveAncestorIdentity::new("ancestor-secret").unwrap(),
            LiveObservationEpoch::new(11).unwrap(),
        );
        (locator, context)
    }

    fn present_locator_and_context() -> (PlatformLocator, PlatformStorageContext) {
        let locator =
            PlatformLocator::from_selector(selector(r"D:\anchor-keyring-binding\platform.db\root"));
        let context = PlatformStorageContext::for_present_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveRootIdentity::new("root-secret").unwrap(),
            LiveObservationEpoch::new(17).unwrap(),
        );
        (locator, context)
    }

    fn committed_required() -> RequiredFamilyArtifacts {
        RequiredFamilyArtifacts::none()
            .with_main_db()
            .with_provider_binding()
            .with_keyring_reference()
            .with_anchor()
    }

    fn assert_source_does_not_declare_clone_or_copy(struct_name: &str) {
        let struct_line = MODULE_SOURCE
            .lines()
            .position(|line| line.contains(&format!("struct {struct_name}")))
            .unwrap_or_else(|| panic!("missing struct declaration for {struct_name}"));
        let lines: Vec<_> = MODULE_SOURCE.lines().collect();

        for attribute_line in lines[..struct_line].iter().rev() {
            let attribute_line = attribute_line.trim();
            if attribute_line.is_empty() || !attribute_line.starts_with("#[") {
                break;
            }
            assert!(
                !attribute_line.contains("Clone"),
                "{struct_name} source regression: derive(Clone) reappeared"
            );
            assert!(
                !attribute_line.contains("Copy"),
                "{struct_name} source regression: derive(Copy) reappeared"
            );
        }

        assert!(
            !MODULE_SOURCE.contains(&format!("impl Clone for {struct_name}")),
            "{struct_name} source regression: manual Clone impl reappeared"
        );
        assert!(
            !MODULE_SOURCE.contains(&format!("impl Copy for {struct_name}")),
            "{struct_name} source regression: manual Copy impl reappeared"
        );
    }

    #[test]
    fn family_matrix_accepts_required_and_optional_committed_shapes() {
        let (locator, context) = present_locator_and_context();
        let observation = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty()
                .with_required(committed_required())
                .with_optional(
                    OptionalFamilyArtifacts::none()
                        .with_sqlite_wal()
                        .with_sqlite_shm()
                        .with_sqlite_journal(),
                ),
        )
        .unwrap();

        assert_eq!(
            observation.classify(),
            PlatformStoragePreflightClassification::CommittedFamily
        );
        assert_eq!(
            PlatformStoragePreflightClassification::CommittedFamily.as_str(),
            "committed_family"
        );
    }

    #[test]
    fn released_sidecar_requires_live_lock_to_report_locked() {
        let (locator, context) = present_locator_and_context();
        let released_sidecar = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty().with_released_sidecar(),
        )
        .unwrap();
        let locked = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty()
                .with_released_sidecar()
                .with_live_exclusive_lock(),
        )
        .unwrap();

        assert_eq!(
            released_sidecar.classify(),
            PlatformStoragePreflightClassification::ReleasedSidecar
        );
        assert_eq!(
            locked.classify(),
            PlatformStoragePreflightClassification::Locked
        );
    }

    #[test]
    fn partial_orphan_and_residual_shapes_do_not_fall_back_to_root_nonempty() {
        let (locator, context) = present_locator_and_context();
        let partial = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty().with_required(
                RequiredFamilyArtifacts::none()
                    .with_main_db()
                    .with_provider_binding(),
            ),
        )
        .unwrap();
        let orphan = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty().with_orphaned(
                OrphanFamilyArtifacts::none()
                    .with_anchor()
                    .with_sqlite_wal(),
            ),
        )
        .unwrap();
        let residual = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty().with_forbidden(
                ForbiddenFamilyArtifacts::none()
                    .with_selector_artifact_in_root()
                    .with_unknown_sidecar(),
            ),
        )
        .unwrap();

        assert_eq!(
            partial.classify(),
            PlatformStoragePreflightClassification::PartialFamily
        );
        assert_eq!(
            orphan.classify(),
            PlatformStoragePreflightClassification::PartialFamily
        );
        assert_eq!(
            residual.classify(),
            PlatformStoragePreflightClassification::ResidualPresent
        );
    }

    #[test]
    fn interrupted_bootstrap_and_optional_without_required_are_partial_family() {
        let (locator, context) = present_locator_and_context();
        let interrupted = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty().with_interrupted_bootstrap(),
        )
        .unwrap();
        let orphan_optional = PlatformStoragePreflightObservation::new(
            &locator,
            &context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty()
                .with_optional(OptionalFamilyArtifacts::none().with_sqlite_journal()),
        )
        .unwrap();

        assert_eq!(
            interrupted.classify(),
            PlatformStoragePreflightClassification::PartialFamily
        );
        assert_eq!(
            orphan_optional.classify(),
            PlatformStoragePreflightClassification::PartialFamily
        );
    }

    #[test]
    fn root_surface_classifications_remain_explicit() {
        let (present_locator, present_context) = present_locator_and_context();
        let (absent_locator, absent_context) = absent_locator_and_context();

        let cases = [
            (
                PlatformStoragePreflightObservation::new(
                    &absent_locator,
                    &absent_context,
                    ObservedRootSurface::Absent,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootAbsent,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::PhysicallyEmpty,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootPhysicallyEmpty,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::Inaccessible,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootInaccessible,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::Reparse,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootReparse,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::AdsPresent,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootAdsPresent,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::HardlinkAlias,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootHardlinkAlias,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::HiddenOrSystemPresent,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootHiddenOrSystemPresent,
            ),
            (
                PlatformStoragePreflightObservation::new(
                    &present_locator,
                    &present_context,
                    ObservedRootSurface::IdentityMismatch,
                    PlatformStorageFamilyObservation::empty(),
                )
                .unwrap(),
                PlatformStoragePreflightClassification::RootIdentityMismatch,
            ),
        ];

        for (observation, expected) in cases {
            assert_eq!(observation.classify(), expected);
            assert_eq!(observation.classify().as_str(), expected.as_str());
        }
    }

    #[test]
    fn unmatched_selector_context_pair_cannot_construct_preflight_observation() {
        let (locator, _) = present_locator_and_context();
        let (_, absent_context) = absent_locator_and_context();

        let error = PlatformStoragePreflightObservation::new(
            &locator,
            &absent_context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty(),
        )
        .unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[test]
    fn preflight_observation_rejects_invalid_root_state_surface_pairs() {
        let (absent_locator, absent_context) = absent_locator_and_context();
        let (present_locator, present_context) = present_locator_and_context();

        let absent_context_error = PlatformStoragePreflightObservation::new(
            &absent_locator,
            &absent_context,
            ObservedRootSurface::Nonempty,
            PlatformStorageFamilyObservation::empty(),
        )
        .unwrap_err();
        let present_context_error = PlatformStoragePreflightObservation::new(
            &present_locator,
            &present_context,
            ObservedRootSurface::Absent,
            PlatformStorageFamilyObservation::empty(),
        )
        .unwrap_err();

        assert_eq!(
            absent_context_error.code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            present_context_error.code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    struct FakeReadOnlyObserver {
        facts: ReadOnlyStoragePreflightFacts,
        read_calls: AtomicU32,
        write_calls: AtomicU32,
    }

    impl ReadOnlyStoragePreflightObserver for FakeReadOnlyObserver {
        fn observe_read_only_preflight(
            &self,
            _locator: &PlatformLocator,
            _context: &PlatformStorageContext,
        ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
            self.read_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.facts)
        }
    }

    #[test]
    fn session_request_rejects_observer_surface_outside_context_boundary() {
        let (absent_locator, absent_context) = absent_locator_and_context();
        let (present_locator, present_context) = present_locator_and_context();
        let absent_observer = Arc::new(FakeReadOnlyObserver {
            facts: ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty(),
            ),
            read_calls: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
        });
        let present_observer = Arc::new(FakeReadOnlyObserver {
            facts: ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Absent,
                PlatformStorageFamilyObservation::empty(),
            ),
            read_calls: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
        });
        let absent_session =
            PreflightSession::new(&absent_locator, &absent_context, absent_observer.clone())
                .unwrap();
        let present_session =
            PreflightSession::new(&present_locator, &present_context, present_observer.clone())
                .unwrap();

        let absent_request = absent_session.issue_read_only_request().unwrap();
        let present_request = present_session.issue_read_only_request().unwrap();
        let absent_error = absent_request.consume().unwrap_err();
        let present_error = present_request.consume().unwrap_err();

        assert_eq!(absent_error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(present_error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(absent_observer.read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(present_observer.read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(absent_observer.write_calls.load(Ordering::SeqCst), 0);
        assert_eq!(present_observer.write_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn observer_facts_are_classified_only_via_session_request_consume() {
        let (locator, context) = present_locator_and_context();
        let observer = Arc::new(FakeReadOnlyObserver {
            facts: ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty()
                    .with_forbidden(ForbiddenFamilyArtifacts::none().with_cache_artifact()),
            ),
            read_calls: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
        });
        let session = PreflightSession::new(&locator, &context, observer.clone()).unwrap();
        let request = session.issue_read_only_request().unwrap();
        let classification = request.consume().unwrap();

        assert_eq!(
            classification,
            PlatformStoragePreflightClassification::ResidualPresent
        );
        assert_eq!(observer.read_calls.load(Ordering::SeqCst), 1);
        assert_eq!(observer.write_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn validation_failure_consumes_request_before_observer_work() {
        let (locator, context) = present_locator_and_context();
        let observer = Arc::new(FakeReadOnlyObserver {
            facts: ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty(),
            ),
            read_calls: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
        });
        let session = PreflightSession::new(&locator, &context, observer.clone()).unwrap();
        let mut request = session.issue_read_only_request().unwrap();
        request.binding.observation_epoch += 1;

        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(observer.read_calls.load(Ordering::SeqCst), 0);
        assert_eq!(observer.write_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn observer_failure_consumes_request_fail_closed() {
        struct FailingObserver {
            read_calls: AtomicU32,
        }

        impl ReadOnlyStoragePreflightObserver for FailingObserver {
            fn observe_read_only_preflight(
                &self,
                _locator: &PlatformLocator,
                _context: &PlatformStorageContext,
            ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
                self.read_calls.fetch_add(1, Ordering::SeqCst);
                Err(McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityUnavailable,
                    "read-only preflight evidence is unavailable",
                ))
            }
        }

        let (locator, context) = present_locator_and_context();
        let observer = Arc::new(FailingObserver {
            read_calls: AtomicU32::new(0),
        });
        let session = PreflightSession::new(&locator, &context, observer.clone()).unwrap();
        let request = session.issue_read_only_request().unwrap();

        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );
        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(observer.read_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn observer_bypass_entrypoints_are_not_exported() {
        let production_source = MODULE_SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("storage_preflight.rs must contain production source");
        assert!(!production_source
            .contains("pub(in crate::mcp_platform) fn classify_read_only_preflight"));
        assert!(!production_source
            .contains("pub(in crate::mcp_platform) struct PlatformStoragePreflightObservation"));
    }

    #[test]
    fn preflight_session_and_request_are_single_use_and_fail_closed_on_drift() {
        let (locator, context) = present_locator_and_context();
        let observer: Arc<dyn ReadOnlyStoragePreflightObserver> = Arc::new(FakeReadOnlyObserver {
            facts: ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty()
                    .with_forbidden(ForbiddenFamilyArtifacts::none().with_cache_artifact()),
            ),
            read_calls: AtomicU32::new(0),
            write_calls: AtomicU32::new(0),
        });

        let session = PreflightSession::new(&locator, &context, Arc::clone(&observer)).unwrap();
        let request = session.issue_read_only_request().unwrap();

        assert_eq!(
            request.consume().unwrap(),
            PlatformStoragePreflightClassification::ResidualPresent
        );
        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            session.issue_read_only_request().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );

        let drifted_session = PreflightSession::new(&locator, &context, observer).unwrap();
        let mut drifted_request = drifted_session.issue_read_only_request().unwrap();
        drifted_request.binding.observation_epoch += 1;
        assert_eq!(
            drifted_request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn preflight_capability_source_does_not_declare_clone_or_copy() {
        assert_source_does_not_declare_clone_or_copy("PreflightSession");
        assert_source_does_not_declare_clone_or_copy("ReadOnlyPreflightRequest");
    }
}
