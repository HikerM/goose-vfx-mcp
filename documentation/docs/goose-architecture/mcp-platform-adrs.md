---
title: MCP Platform ADRs
sidebar_position: 4
---

# MCP Platform Architecture Decision Records

Status for every ADR below is **Accepted for Phase 1**. Reversal requires a superseding ADR, compatibility/migration plan, and security review.

## ADR-001: Rust Core is the authority

**Decision:** Manifest parsing, catalog trust, policy, plans, tasks, checksums, owned files, installation state, activation, rollback, and recovery live in Rust Core. The renderer uses typed ACP requests and events.

**Consequences:** Renderer compromise cannot introduce an installation command through the supported API. Core can serve CLI or future clients consistently. UI drafts are non-authoritative and must be refreshed on revision conflicts.

## ADR-002: Keep seven state dimensions independent

**Decision:** Connection/registration, installation, runtime, health, default enablement, per-session enablement, and per-tool policy are separately stored and returned.

**Consequences:** Installation success never implies enablement or health. A runtime crash does not corrupt inventory. APIs and UI must not expose a single ambiguous `enabled/installed` status.

## ADR-003: New MCPs start disabled

**Decision:** A new registration or installation projects an `ExtensionConfig` with default enablement `false`. Health verification and an explicit user action precede default/session enablement.

**Consequences:** Catalog or model input cannot silently add tools to sessions. Migration from any prototype that auto-enabled packages is not behavior-compatible and must require review.

## ADR-004: Manifest v1 is closed and declarative

**Decision:** JSON Schema 2020-12 with `schema_version: 1`, closed objects, exact versions, typed distribution/transport/auth/health/host actions, declared permissions, and owned-file uninstall. Arbitrary pre/post lifecycle commands are forbidden.

**Consequences:** New fields or executable semantics require a new schema version. Entrypoints are direct executable/argument vectors interpreted by trusted adapters, never shell text. Policy enforces provenance rules that JSON Schema cannot express.

## ADR-005: Use six orthogonal adapter families

**Decision:** Distribution, HostIntegration, Transport, Auth, HealthCheck, and Policy are independent traits. Initial distribution coverage is `remote_http` and `manual_stdio`; package/archive/container and DCC adapters follow.

**Consequences:** A DCC integration cannot become a special installer architecture. Adapter compatibility is resolved during planning and adapter versions are journaled for recovery.

## ADR-006: Catalog trust is tiered and cryptographically bound

**Decision:** Sources are `official`, `community`, or `local`. Release catalogs pin index versions and hashes and bind exact manifest/artifact digests to verified publishers. `git_dev` is only valid in explicit dev/local mode.

**Consequences:** `latest`, ranges, mutable tags, unpinned Git, revoked publishers, or signature/hash mismatches block new execution. Last-known-good verified cache remains available with explicit stale/offline status.

## ADR-007: Persistent tasks use staged atomic activation

**Decision:** Install/update/repair/uninstall run as journaled tasks with immutable plans, idempotency keys, typed steps, cancellation boundaries, compensations, and startup recovery. Acquisition occurs in staging, verification precedes activation, and activation is atomic on the same filesystem.

**Consequences:** Update preserves the old active version until commit. Cancellation can remain `cancelling`. Incomplete rollback becomes `recovery_required`, not generic failure or silent cleanup.

## ADR-008: MCP platform persistence is separate from sessions

**Decision:** Use a versioned SQLite database below `Paths::data_dir()/mcp-platform`, separate from `sessions.db`. Session storage only holds the exact profile/extension resolution snapshot needed by that session.

**Consequences:** Inventory survives session deletion/export and has independent migrations. Cross-database changes use durable reconciliation/outbox-style events rather than pretending to be one SQLite transaction.

## ADR-009: ExtensionConfig is a one-way runtime projection

**Decision:** A verified managed object generates the existing `stdio` or `streamable_http` connection configuration. Install/catalog/task/ownership fields never enter `ExtensionConfig`.

**Consequences:** Existing agent/runtime code remains the connection seam. Removing a legacy config does not uninstall managed files; managed projections are linked and reconciled by Core.

## ADR-010: Profiles resolve to immutable session snapshots

**Decision:** Named profile templates reference stable MCP IDs and a version policy. Session creation stores exact version, manifest digest, connection revision, enablement, and tool-policy resolution.

**Consequences:** Updating a template affects future resolutions only. Re-resolving an old session is an explicit, reviewable action with a new snapshot revision.

## ADR-011: Natural language produces plans, never effects

**Decision:** Model output may draft `InstallationPlan` or `ProfilePlan`. Core resolves sources/artifacts, policy validates it, UI shows effects, and a human confirmation with matching plan digest starts the task.

**Consequences:** Prompt injection cannot call the execution boundary through prose. A stale or changed plan must be regenerated and reviewed. Credential values are excluded from model-visible and audit-visible plan data.

## ADR-012: Typed ACP is the only desktop service boundary

**Decision:** Catalog, planning, confirmation, task control, lifecycle, health, profiles, enablement, tool policy, and event subscription each have typed operations and closed errors/events. No generic `execute`, lifecycle command, or renderer-provided arbitrary path exists.

**Consequences:** API additions require wire-contract tests. Renderer production components consume real typed states and never infer authoritative success optimistically.

## ADR-013: Uninstall removes only proven ownership

**Decision:** Core removes only files/directories and host registrations recorded from a verified manifest and committed task steps. Path expansion must remain inside approved roots; user data is preserved by default.

**Consequences:** Unknown files are reported and retained. Ownership collisions block planning or require a specific conflict policy. Prototype uninstall scripts are not migrated.

## ADR-014: Cross-platform runtimes are app-managed

**Decision:** Package adapters use per-user goose-managed runtimes/stores and platform-specific effect adapters. No global npm/pip install, system Python mutation, hard-coded OS data path, or implicit elevation is part of this architecture.

**Consequences:** Artifacts and activation behavior are reproducible per platform/architecture. Docker remains an external, detected dependency with digest and mount policy.
