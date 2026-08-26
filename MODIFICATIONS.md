# Lumina modifications

This repository is a substantially modified distribution derived from the [goose project](https://github.com/aaif-goose/goose). The Git history is the authoritative record of file-level changes. Release source archives must identify the exact Git commit from which each binary was built.

The Lumina distribution includes, among other changes:

- independent Lumina desktop branding, icons, package metadata, installer identity and `lumina://` deep links;
- a fail-closed updater that is enabled only when a distributor-owned release repository is supplied at build time;
- isolated Lumina configuration, state, browser-partition and Windows storage namespaces;
- Windows desktop, installer, local-model download and GPU inference changes;
- MCP extension discovery, configuration, import/export and diagnostics changes;
- simplified-Chinese desktop localization;
- telemetry isolation so no analytics endpoint is active without Lumina-owned build configuration; and
- removal or replacement of selected upstream-branded assets and dependencies.

The Lumina runtime uses only Lumina package, protocol, configuration, storage and trust namespaces. Legacy names are confined to the separately packaged, one-time migration tool and provenance/legal records. Signed legacy MCP and evidence state is archived during migration and must be re-authorized or rebuilt in the Lumina trust domain.

## Distribution requirement

Every Lumina source or binary release must include `LICENSE`, `NOTICE`, this modification notice and the generated third-party notices/SBOM for that exact release. If source files are exported outside the Git repository, the export process must preserve a prominent modification notice and the corresponding source revision.
