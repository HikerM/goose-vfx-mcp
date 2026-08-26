//! Crate-private in-memory lookup for untrusted adapter capability contract syntax.
//!
//! The registry has no external-input population path, and each result remains untrusted
//! syntax data. Do not public re-export or wire it to HTTP, ACP, UI, CLI, persistence,
//! managed MCP, installation, download, signature or authorization decisions, process,
//! or network code; a future executor must independently establish every trust boundary.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use super::v3_contract::{valid_digest, valid_id, AdapterCapabilityContract};

#[derive(Debug, Default)]
pub(crate) struct AdapterCapabilityRegistry {
    contracts: BTreeMap<(String, String, String), AdapterCapabilityContract>,
}

impl AdapterCapabilityRegistry {
    /// Adds a test fixture to this process-local syntax registry only.
    ///
    /// This is intentionally private so external JSON cannot be presented as a
    /// pre-registered or trusted adapter contract.
    fn register_untrusted_fixture(&mut self, contract: AdapterCapabilityContract) -> Result<()> {
        contract.validate()?;
        let key = (
            contract.capability_id.clone(),
            contract.contract_version.clone(),
            contract.contract_digest.clone(),
        );
        if self.contracts.contains_key(&key) {
            bail!("adapter capability contract is already registered");
        }
        self.contracts.insert(key, contract);
        Ok(())
    }

    /// Returns the sole compatible in-memory candidate, if it is unambiguous.
    pub(crate) fn select_range_candidate(
        &self,
        capability_id: &str,
        version_range: &str,
    ) -> Result<&AdapterCapabilityContract> {
        if !valid_id(capability_id) {
            bail!("invalid capability id");
        }
        let requirement = semver::VersionReq::parse(version_range)?;
        let mut candidates = self.contracts.values().filter(|contract| {
            contract.capability_id == capability_id
                && semver::Version::parse(&contract.contract_version)
                    .is_ok_and(|version| requirement.matches(&version))
        });
        let Some(candidate) = candidates.next() else {
            bail!("no registered adapter capability contract matches the range");
        };
        if candidates.next().is_some() {
            bail!("adapter capability contract range is ambiguous");
        }
        Ok(candidate)
    }

    /// Fail-closed exact lookup for a future executor. This method itself performs no
    /// execution and the returned contract remains an untrusted syntax object.
    pub(crate) fn require_exact_digest(
        &self,
        capability_id: &str,
        contract_version: &str,
        contract_digest: &str,
    ) -> Result<&AdapterCapabilityContract> {
        if !valid_id(capability_id)
            || semver::Version::parse(contract_version).is_err()
            || !valid_digest(contract_digest)
        {
            bail!("invalid exact adapter capability contract selector");
        }
        self.contracts
            .get(&(
                capability_id.to_string(),
                contract_version.to_string(),
                contract_digest.to_string(),
            ))
            .ok_or_else(|| {
                anyhow::anyhow!("exact registered adapter capability contract not found")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::v3_contract::{parse_untrusted_document, UntrustedDocument};

    fn digest(value: char) -> String {
        value.to_string().repeat(64)
    }

    fn contract(version: &str, value: char) -> AdapterCapabilityContract {
        let json = format!(
            r#"{{"document_type":"lumina.mcp.adapter-capability-contract","schema_version":1,"capability_id":"demo.adapter","contract_version":"{version}","contract_digest":"{}","supported_manifest_schema_versions":[3],"operations":[{{"id":"inspect","request_kind":"manifest","response_kind":"result"}}],"entrypoint_kinds":["module"],"transport_protocols":[{{"transport":"stdio","protocol_range":"^1.0"}}],"platform_requirements":[{{"selector":"linux-x64","runtime":"node"}}],"permission_kinds":["network"],"config_kinds":["json"],"dependency_resolution":"closed_lock_only","trusted_launcher":{{"identity":"launcher","digest":"{}"}},"recovery":{{"rollback_supported":true,"maximum_attempts":1}}}}"#,
            digest(value),
            digest(value)
        );
        match parse_untrusted_document(&json).unwrap() {
            UntrustedDocument::AdapterCapabilityContract(contract) => contract,
            _ => unreachable!(),
        }
    }

    #[test]
    fn registry_accepts_only_registered_contracts_and_fails_closed_on_digest_mismatch() {
        let mut registry = AdapterCapabilityRegistry::default();
        registry
            .register_untrusted_fixture(contract("1.0.0", 'a'))
            .unwrap();
        assert!(registry
            .select_range_candidate("unknown.adapter", "^1")
            .is_err());
        assert!(registry
            .require_exact_digest("demo.adapter", "1.0.0", &digest('b'))
            .is_err());
        assert!(registry
            .require_exact_digest("demo.adapter", "1.0.0", &digest('a'))
            .is_ok());
    }

    #[test]
    fn range_selection_rejects_ambiguity() {
        let mut registry = AdapterCapabilityRegistry::default();
        registry
            .register_untrusted_fixture(contract("1.0.0", 'a'))
            .unwrap();
        registry
            .register_untrusted_fixture(contract("1.1.0", 'b'))
            .unwrap();
        assert!(registry
            .select_range_candidate("demo.adapter", "^1")
            .is_err());
    }
}
