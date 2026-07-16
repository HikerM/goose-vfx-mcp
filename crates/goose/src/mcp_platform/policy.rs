use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::domain::TrustTier;
use super::manifest::{Distribution, Manifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanOperation {
    Register,
    Install,
    Update,
    Repair,
    Uninstall,
    Health,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allow,
    Deny,
    NeedsConfirmation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReasonCode {
    PolicyRequirementsSatisfied,
    RequiredPermissionConfirmation,
    CommunitySourceConfirmation,
    LocalSourceConfirmation,
    GitDevReleaseDenied,
    UnknownDistributionAdapter,
}

impl PolicyReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyRequirementsSatisfied => "policy_requirements_satisfied",
            Self::RequiredPermissionConfirmation => "required_permission_confirmation",
            Self::CommunitySourceConfirmation => "community_source_confirmation",
            Self::LocalSourceConfirmation => "local_source_confirmation",
            Self::GitDevReleaseDenied => "git_dev_release_denied",
            Self::UnknownDistributionAdapter => "unknown_distribution_adapter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyReason {
    pub code: PolicyReasonCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecision {
    pub outcome: PolicyOutcome,
    pub reasons: Vec<PolicyReason>,
}

impl PolicyDecision {
    pub fn is_denied(&self) -> bool {
        self.outcome == PolicyOutcome::Deny
    }
}

#[derive(Debug, Clone)]
pub struct PolicyContext {
    pub trust_tier: TrustTier,
    pub operation: PlanOperation,
    known_adapters: BTreeSet<String>,
}

impl PolicyContext {
    pub fn new(trust_tier: TrustTier, operation: PlanOperation) -> Self {
        Self {
            trust_tier,
            operation,
            known_adapters: [
                "remote_http",
                "manual_stdio",
                "npm",
                "python_wheel",
                "binary_archive",
                "docker",
                "git_dev",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn with_known_adapters(
        mut self,
        adapters: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.known_adapters = adapters.into_iter().map(Into::into).collect();
        self
    }
}

pub fn evaluate_manifest_policy(manifest: &Manifest, context: &PolicyContext) -> PolicyDecision {
    if !context
        .known_adapters
        .contains(manifest.distribution.adapter_id())
    {
        return deny(
            PolicyReasonCode::UnknownDistributionAdapter,
            "the distribution adapter is not recognized",
        );
    }

    if matches!(
        context.trust_tier,
        TrustTier::Official | TrustTier::Community
    ) && matches!(manifest.distribution, Distribution::GitDev { .. })
    {
        return deny(
            PolicyReasonCode::GitDevReleaseDenied,
            "release catalogs cannot contain git development distributions",
        );
    }

    let mut reasons = Vec::new();
    match context.trust_tier {
        TrustTier::Official => {}
        TrustTier::Community => reasons.push(reason(
            PolicyReasonCode::CommunitySourceConfirmation,
            "community catalog sources require confirmation",
        )),
        TrustTier::Local => reasons.push(reason(
            PolicyReasonCode::LocalSourceConfirmation,
            "local manifest sources require confirmation",
        )),
    }

    if manifest
        .permissions
        .iter()
        .any(|permission| permission.required)
    {
        reasons.push(reason(
            PolicyReasonCode::RequiredPermissionConfirmation,
            "required package permissions must be confirmed",
        ));
    }

    if reasons.is_empty() {
        PolicyDecision {
            outcome: PolicyOutcome::Allow,
            reasons: vec![reason(
                PolicyReasonCode::PolicyRequirementsSatisfied,
                "manifest satisfies the current release policy",
            )],
        }
    } else {
        PolicyDecision {
            outcome: PolicyOutcome::NeedsConfirmation,
            reasons,
        }
    }
}

fn deny(code: PolicyReasonCode, message: &str) -> PolicyDecision {
    PolicyDecision {
        outcome: PolicyOutcome::Deny,
        reasons: vec![reason(code, message)],
    }
}

fn reason(code: PolicyReasonCode, message: &str) -> PolicyReason {
    PolicyReason {
        code,
        message: message.to_string(),
    }
}
