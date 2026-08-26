# Lumina commercial release gate

This checklist is an engineering release gate, not legal advice. A release is not approved for commercial distribution until every required item has an owner, evidence and an approval date. The word **BLOCKER** means the build must not be sold or provided to customers.

## 1. Product and trademark identity

- **BLOCKER:** Complete a professional clearance search for `Lumina`, its Chinese name, logo and confusingly similar marks in every sales market and for the relevant software, SaaS and AI service classes.
- **BLOCKER:** Record the legal distributor name, registered address, support contact and trademark owner. `HikerM` is currently a repository/package-maintainer identifier, not a verified legal-entity record.
- Confirm that installers, executables, signatures, update feeds, websites, screenshots and stores use only approved Lumina branding.
- Use `Goose`, `Block`, `AAIF` and related marks only where reasonably necessary to describe source or compatibility. Never imply sponsorship, certification or official status.
- Obtain written ownership or a commercial license for every Lumina logo, icon, font, screenshot, sound and marketing asset.

## 2. Apache-2.0 provenance

- Ship the complete [LICENSE](./LICENSE), [NOTICE](./NOTICE) and [MODIFICATIONS.md](./MODIFICATIONS.md) with source and binary distributions.
- Preserve applicable upstream copyright, patent, trademark and attribution notices.
- Make the exact corresponding source revision available for every release and retain reproducible build records.
- Review whether exported modified source files need additional file-level change notices under Apache-2.0 section 4(b).
- Do not replace the upstream Apache-2.0 grant with a closed EULA. Commercial terms may cover Lumina services and original additions but must not take away recipients' Apache-2.0 rights in covered code.

## 3. Third-party software, models and content

- **BLOCKER:** Run `cargo deny check advisories licenses` for the release commit and resolve every unapproved, unknown or missing Rust license.
- **BLOCKER:** Run `pnpm licenses list --prod --json` for each shipped JavaScript workspace and review every license, repository override and bundled binary.
- **BLOCKER:** Generate a release-specific SBOM and third-party notice bundle containing required license texts, copyright notices and source-availability information.
- Review MPL-2.0 components and make their covered source code available as required. Do not add LGPL/GPL/AGPL/SSPL components without a documented legal and distribution-compliance decision.
- Audit vendored code, native libraries, Electron/Chromium, FFmpeg codecs, installers and downloaded helper binaries separately; package-manager metadata alone is not sufficient.
- Treat model weights, tokenizers, sample prompts, recipes and remotely downloaded MCP servers as separately licensed artifacts. Record redistribution and commercial-use rights for each one.
- Verify AI provider, model-hosting, app-store and API terms for resale, user-provided keys, output use, data retention and geographic restrictions.
- Register distributor-owned OAuth clients before enabling subscription/device-login flows. Lumina ships no borrowed ChatGPT Codex, Gemini CLI, Grok CLI, Kimi CLI or GitHub Copilot client identity; configure `LUMINA_CHATGPT_CODEX_CLIENT_ID`, `LUMINA_GEMINI_OAUTH_CLIENT_ID`, `LUMINA_GEMINI_OAUTH_CLIENT_SECRET`, `LUMINA_XAI_OAUTH_CLIENT_ID`, `LUMINA_KIMI_CODE_CLIENT_ID`, `LUMINA_HUGGINGFACE_OAUTH_CLIENT_ID` and `GITHUB_COPILOT_CLIENT_ID` only after provider approval.

## 4. Privacy and data control

- Upstream analytics credentials must never ship. Telemetry is disabled unless both `LUMINA_POSTHOG_API_KEY` and `LUMINA_POSTHOG_CAPTURE_URL` are supplied at build time and the UI/disclosure is deliberately enabled.
- **BLOCKER before enabling telemetry:** publish a Lumina privacy policy naming the data controller, purposes, fields, retention, processors, cross-border transfers, deletion route and consent withdrawal mechanism.
- Inventory logs, crash reports, prompts, tool arguments, model-provider traffic, update checks and extension traffic. Confirm the UI statements match actual code and network behavior.
- Complete the privacy/security reviews required by the target markets, including personal-information and cross-border-transfer obligations where applicable.

## 5. Release infrastructure and customer terms

- Sign installers and update metadata with credentials controlled by the legal distributor; never reuse upstream certificates, API keys or release tokens.
- Keep the repository variable `LUMINA_PUBLISHING_READY` unset until repository, npm, Maven, GHCR, documentation and signing ownership evidence has been reviewed. Publish workflows fail closed while it is not exactly `true`.
- Verify `lumina://`, `io.github.hikerm.Lumina`, installer GUIDs, user-data paths and update assets do not collide with upstream or another product.
- Publish a vulnerability-reporting address, supported-version policy, dependency-update process and incident-response owner.
- Define the EULA, paid-service terms, refund rules, warranty/support scope, export/sanctions position and AI-risk disclosures for the actual sales model.
- Make clear that any paid warranty, indemnity or support is offered only by the Lumina distributor, not by upstream contributors.

## 6. Evidence record

For each commercial release, archive:

| Evidence | Required value |
| --- | --- |
| Source revision | Full Git commit SHA and clean-tree status |
| Build provenance | CI run, toolchain versions and artifact hashes |
| License review | Rust, JavaScript, native binary, model and asset reports |
| Notices/SBOM | Exact files included in every installer/archive |
| Trademark approval | Markets/classes searched, reviewer and date |
| Privacy approval | Data-flow version, policy version and reviewer |
| Security approval | Test results, unresolved risks and signing verification |
| Business approval | Legal distributor, customer terms and release approver |

Before every build, run `node scripts/check-lumina-independence.mjs`. This automated check enforces the runtime namespace boundary, but it does not prove trademark clearance or ownership of external accounts.

## Current status

The `codex/lumina-commercial` branch is an engineering migration branch, not a legal clearance certificate or a sale-ready release. Open blockers include professional clearance of the provisional `Lumina` name/logo; proof of ownership for the release repository, npm scope, Maven/GHCR namespaces and signing credentials; privacy/customer terms; and a complete third-party/native/model/asset notice and SBOM bundle. Online updates remain disabled until a verified distributor-owned repository is configured.
