---
title: MCP Platform Acceptance
sidebar_position: 5
---

# MCP Platform Acceptance

This document is the executable acceptance contract for implementing [MCP Platform Architecture](./mcp-platform-architecture.md). A phase passes only when its required evidence is automated or explicitly marked as a manual platform/accessibility check. Happy-path screenshots are not acceptance evidence.

## 1. Architecture acceptance criteria

| ID | Criterion | Evidence |
|---|---|---|
| ARC-01 | Renderer cannot provide or execute an installation/lifecycle command; all effects originate from typed Core plans. | ACP schema negative tests and renderer source scan |
| ARC-02 | Registration, installation, runtime, health, default enablement, session enablement, and tool policy are independently persisted and returned. | Domain transition and serialization tests |
| ARC-03 | New install/register produces a disabled connection; health success does not enable it. | End-to-end lifecycle test |
| ARC-04 | Six adapter traits are independent, and host integration composes with rather than subclasses distribution. | Trait/module dependency test or architecture lint |
| ARC-05 | Managed objects project only connection fields into `ExtensionConfig`; install fields never serialize there. | Projection golden tests |
| ARC-06 | MCP platform DB/install/cache paths derive from `Paths`, live under data storage, and are separate from sessions. | Path tests with `LUMINA_PATH_ROOT`; DB inventory check |
| ARC-07 | Profile update does not change an existing session resolution snapshot. | Repository/service integration test |
| ARC-08 | Natural-language endpoints can create draft plans but cannot confirm/start tasks. | Authorization/capability and API tests |

## 2. Manifest and catalog acceptance criteria

| ID | Criterion | Evidence |
|---|---|---|
| MAN-01 | Schema is Draft 2020-12, fixes `schema_version` to `1`, and rejects unknown top-level/variant fields. | Schema validator tests |
| MAN-02 | All seven distribution variants have distinct required structures. | One positive and at least one negative fixture per variant |
| MAN-03 | `remote_http` requires HTTP transport and `manual_stdio` requires stdio. | `if/then` negative fixtures |
| MAN-04 | Release versions are exact; Policy rejects `latest`, ranges, mutable image tags, `git_dev`, and unpinned Git in release catalogs. | Policy table tests |
| MAN-05 | Downloaded artifacts select deterministically by platform/arch and verify SHA-256 before extraction/activation. | Resolver property tests and corrupted artifact tests |
| MAN-06 | Publisher, license, capabilities, permissions, transport/auth/health, host integration, ownership, and uninstall are visible in the verified model. | Manifest round-trip tests |
| MAN-07 | No arbitrary lifecycle field is accepted; entrypoint uses direct spawn and never a shell. | Schema negative fixtures and process-adapter tests |
| CAT-01 | Official/community index, manifest, publisher proof, and artifact digests form a verified chain; revocation is enforced. | Trust-chain/revocation integration tests |
| CAT-02 | Failed refresh preserves last-known-good cache. Offline install requires complete, policy-fresh verified blobs. | Offline/cache fault-injection tests |
| CAT-03 | Audit/errors/events redact credential values, authorization headers, OAuth codes, and sensitive environment values. | Redaction property tests with canary values |

## 3. Lifecycle and persistence acceptance criteria

| ID | Criterion | Evidence |
|---|---|---|
| JOB-01 | Install/update/repair/uninstall use immutable plan digests and idempotency keys; duplicate confirm returns the same task/result. | Concurrent idempotency tests |
| JOB-02 | Install stages, verifies, then atomically activates. Update keeps the old version active until commit. | Filesystem integration tests on each OS family |
| JOB-03 | Every effect records start/commit and compensation evidence; restart recovers or rolls back without repeating committed effects. | Kill/restart fault-injection suite |
| JOB-04 | Cancel is cooperative and transitions through `cancelling`; unsafe atomic steps are not interrupted. | Cancellation-at-each-step tests |
| JOB-05 | Failed compensation becomes `recovery_required` with remaining effects; it is never reported as a clean failure. | Rollback fault tests |
| JOB-06 | Repair restores only declared missing/corrupt owned content. Uninstall removes only proven ownership and preserves requested user data. | Ownership collision, traversal, symlink, and unknown-file tests |
| DB-01 | DB migrations are sequential, transactional, forward-only, concurrency safe, and refuse unknown newer versions. | Migration matrix and two-process initialization test |
| DB-02 | Platform inventory remains after session deletion/export; session DB migration does not mutate platform inventory. | Cross-store integration tests |

## 4. ACP contract acceptance criteria

| ID | Criterion | Evidence |
|---|---|---|
| API-01 | Catalog list/detail are paged, source-aware, compatibility-aware, and return cache/offline metadata. | Wire contract tests |
| API-02 | Plan responses include exact source/version/digests, permissions, file/host/network effects, rollback, policy, expiry, and digest. | Golden response tests |
| API-03 | Confirm requires matching plan ID/digest and explicit user decision; stale plans return `plan_stale`. | Service integration tests |
| API-04 | Cancel/retry, update/repair/uninstall, health, profiles, enablement, and tool-policy operations accept typed IDs/revisions only. | Request schema negative tests |
| API-05 | Events have monotonic sequence and support resume; reconnect does not lose final task state. | Stream disconnect/reconnect tests |
| API-06 | Closed errors include code, retryability, correlation ID, and redacted typed details. | Error mapping tests |

## 5. MCP Center acceptance criteria

The implementation must use the existing desktop design system and semantic tokens. Production components consume ACP data/adapters only: no embedded catalog items, fake progress, placeholder actions, `TODO` panels, or renderer-created success states.

| ID | Criterion | Evidence |
|---|---|---|
| UI-01 | Discover, detail, plan review, My MCPs, managed detail, add HTTP/stdio, Profiles, Activity, and Sources/policy have real routes/entry/exit paths. | Route and component tests |
| UI-02 | Every meaningful action maps to the typed ACP operation documented in architecture; destructive operations require effect review and confirmation. | Service mock contract tests |
| UI-03 | Independent install/runtime/health/default/session/tool-policy states are not collapsed or color-only. | State matrix visual/component tests |
| UI-04 | Loading, distinct empty states, partial/error, stale/offline, permission/policy, disabled, long-running, cancelling, failed, interrupted, rollback, and recovery-required states are implemented. | Story/component state suite backed by typed fixtures outside production components |
| UI-05 | At `720x600`, `1440x900`, and `2560x1440`, primary navigation/actions remain reachable; there is no uncontrolled page overflow or clipped dialog. | Playwright screenshots plus layout assertions |
| UI-06 | `<760px` uses one routed column; medium uses drawer detail; wide uses bounded split panels; ultra-wide content is capped. | Responsive browser tests |
| UI-07 | Search/filter, lists, paths, publisher names, localized text, and errors handle long content; large lists paginate/virtualize without losing focus. | Long-content and 200% zoom tests |
| UI-08 | Full keyboard flow works; focus-visible, modal trap/restore, live announcements, accessible names, text/icon state cues, and reduced motion are present. | axe plus manual screen-reader/keyboard checklist |
| UI-09 | Long tasks are non-blocking and survive renderer reconnect through event resume and state reload. | Electron/ACP reconnect integration test |
| UI-10 | Plan review displays source/publisher/version, permissions, files, host effects, network origins/bytes, reversibility, and warnings before confirm. | Component contract test |

## 6. Platform matrix gates

| Gate | Windows | macOS | Linux |
|---|---|---|---|
| Remote HTTP | TLS/auth/redirect policy and health pass | Same | Same |
| Manual stdio | Direct spawn `.exe`, argument fidelity, no shell | Direct spawn, executable/code-sign policy | Direct spawn, executable-bit policy |
| Paths | Test-root and app data roots only | Same | Same |
| Atomic activation | Same-volume replacement and recovery | Same | Same |
| Archive safety (when implemented) | Traversal, drive/UNC, link checks | Traversal, quarantine/link checks | Traversal/link/mode checks |
| Managed runtimes (when implemented) | No global npm/pip mutation | Same | Same |

Docker is accepted only when image digest, declared mounts, daemon availability, cancellation, and cleanup pass. A DCC host adapter is accepted only when distribution tests pass without the host adapter and host registration/unregistration tests use declarative actions with owned-file evidence.

## 7. Test pyramid

1. **Domain/unit (largest):** schema and policy tables, version/platform resolution, state transitions, plan canonicalization/digest, permission and redaction properties, path containment, profile snapshots, error mapping.
2. **Adapter contract:** every adapter runs the same idempotency/cancel/rollback contract; fake filesystem/network/process clocks enable deterministic failure points.
3. **Repository/integration:** migrations from every supported version, `BEGIN IMMEDIATE` concurrency, journal recovery, cache/revocation, ExtensionConfig projection, separate session/platform databases.
4. **ACP contract:** request/response snapshots, negative field tests, event ordering/resume, stale revision/digest, no generic command field.
5. **Desktop component/accessibility:** complete typed state matrix, service mapping, keyboard, axe, long content, reduced motion.
6. **End to end (smallest):** register remote HTTP, register manual stdio, install/update/rollback a local signed fixture for later adapters, profile resolution, crash/restart recovery, uninstall ownership, and renderer reconnect on each supported OS.

No network-dependent public package is required for deterministic CI. Catalogs, signatures, artifacts, HTTP MCP servers, and process MCP servers use repository-owned test fixtures in the implementation phase.

## 8. Threat model and required mitigations

| Threat | Boundary | Required mitigation | Verification |
|---|---|---|---|
| Malicious manifest introduces code | Catalog -> Core | Closed schema, no lifecycle commands, trusted typed adapters | Negative schema/policy tests |
| Supply-chain substitution/replay | Network/cache -> Core | Signed pinned index, exact manifest/artifact digest, expiry/revocation | Tamper/replay tests |
| Prompt injection starts installation | Model -> plan service | Draft-only NL API; human confirm + plan digest; policy cannot be lowered | Capability/authorization tests |
| Renderer compromise runs shell | Renderer -> ACP | No command/generic path fields; Core chooses effects | ACP schema/source scan |
| Path traversal/symlink escape | Artifact/manifest -> filesystem | Canonical containment, reject links/drive/UNC escapes, owned roots | Property/fuzz tests |
| Credential disclosure | Auth -> logs/UI/model | Opaque handles, late resolution, structured redaction, no literal values in plans | Canary redaction tests |
| SSRF/redirect abuse | Manifest HTTP -> network | HTTPS, origin policy, redirect allowlist, local/private-range policy | HTTP adversarial tests |
| TOCTOU during install/update | Staging -> activation | Verify staged bytes, same-volume atomic activation, immutable digest | Mutation/race tests |
| Crash or concurrent duplicate corrupts state | Task/DB | Journal, idempotency, immediate transactions, recovery lease/heartbeat | Kill/concurrency tests |
| Uninstall deletes user/other-package files | Ownership -> filesystem | Exact committed ownership, collision checks, preserve unknown/user data | Ownership tests |
| Host adapter mutates arbitrary config | Core -> DCC host | Declarative allowlisted actions and approved roots; backup/compensation | Host adapter contract tests |
| Stale profile silently changes sessions | Profile -> session | Exact resolution snapshot and optimistic revisions | Snapshot regression test |

Residual risks requiring implementation review are platform code-signing differences, external Docker daemon policy, publisher key compromise before revocation propagation, and non-atomic host-application config formats. These must remain visible as policy/health/audit state rather than being hidden by UI success.

## 9. Traceability matrix

| Requirement | Component/decision | ACP API | Primary tests |
|---|---|---|---|
| Core authority; renderer no shell | Application services, Policy, ADR-001/012 | all typed plan/confirm/task APIs | ARC-01, API-04, renderer scan |
| Separate connection/install/runtime/health/enables/tools | Managed aggregate, ADR-002 | list, health, set default/session, tool policy | ARC-02, UI-03 |
| New MCP disabled | Projection service, ADR-003 | install confirm then set enable | ARC-03 |
| Declarative Manifest v1, no arbitrary lifecycle | Manifest verifier, ADR-004 | catalog detail, plan create | MAN-01/02/03/07 |
| Exact versions and artifact digest/platform | Catalog/Policy/Distribution | catalog detail, plan create/update | MAN-04/05, CAT-01 |
| Publisher/license/capabilities/permissions/auth/health/host/ownership | Manifest domain | catalog detail, plan response | MAN-06, UI-10 |
| Six adapter families | Adapter traits, ADR-005 | plan and health operations | ARC-04, adapter contracts |
| Catalog tiers, signatures, revocation, offline | Catalog service, ADR-006 | catalog list/detail/events | CAT-01/02/03 |
| Crash recovery/cancel/failure/rollback, atomic activation | Task runner, ADR-007 | confirm, cancel, retry, events | JOB-01..05, API-05 |
| SQLite boundary and migrations under Paths | Repositories, ADR-008 | indirect through all services | ARC-06, DB-01/02 |
| One-way ExtensionConfig seam | Projection service, ADR-009 | enable/session resolution | ARC-05 |
| Profile template plus session snapshot | Profile resolver, ADR-010 | profile plan/confirm/resolve | ARC-07, snapshot tests |
| Natural language plan safety | Planning service, ADR-011 | plan from NL, profile plan | ARC-08, threat tests |
| Remote HTTP/manual stdio first | Initial distribution/transport adapters | plan, confirm, health | initial E2E suite |
| DCC only later host examples | HostIntegrationAdapter | plan effects | host contract tests |
| Ordinary-user adaptive MCP Center and complete states | MCP Center UI contract | all list/detail/action/events | UI-01..10 |
| Windows/macOS/Linux and package boundary | Platform effect adapters, ADR-014 | compatible plans/errors | platform matrix gates |
| Orphan VFX migration rules | Architecture section 15 | not applicable | source scan/migration review |

## 10. Phase gates

- **Gate 0 — contracts:** all manifest examples parse and validate; ADRs accepted; ACP contains no arbitrary execution field; database/repository and adapter traits are reviewable.
- **Gate 1 — safe connections:** remote HTTP and manual stdio lifecycle, disabled default, health/auth separation, projection, audit/redaction, cancellation/recovery pass.
- **Gate 2 — MCP Center:** UI-01 through UI-10 pass at required sizes with real service adapters; no production placeholder data.
- **Gate 3 — profiles/plans:** immutable session snapshots and draft-only natural-language plans pass security tests.
- **Gate 4 — managed distributions:** npm, wheel, and archive pass artifact, staging, atomic activation, repair, update, rollback, and platform tests.
- **Gate 5 — Docker/hosts:** digest/mount policy and at least one independent DCC host adapter pass; other hosts require only a new adapter and manifest data.

## 11. Phase 1 document validation

The architecture phase itself is accepted when:

- only the requested architecture, schema, and example paths changed;
- all JSON files parse with PowerShell `ConvertFrom-Json`;
- all examples validate against the Draft 2020-12 schema;
- schema/examples contain no lifecycle command fields or literal credentials;
- documentation explicitly assigns `git_dev` and floating-version rejection to Policy;
- `git diff --check` passes; and
- the result is committed once with `docs: define extensible MCP platform architecture`, without push or phase 2 implementation.
