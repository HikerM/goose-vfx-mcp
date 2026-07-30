import assert from "node:assert/strict";
import { test } from "node:test";
import {
  McpPlatformClient,
  type McpCredentialStatus,
  type McpHealthObservation,
  type McpHealthStatus,
  type McpManagedDetail,
  type McpManagedPage,
  type McpManagedSummary,
  type McpTaskRef,
} from "../src/mcp-platform.ts";

type Assert<T extends true> = T;
type IncludesNull<T, K extends keyof T> = null extends T[K] ? true : false;
type IsRequiredKey<T, K extends keyof T> = {} extends Pick<T, K> ? false : true;
type IsOptionalKey<T, K extends keyof T> = {} extends Pick<T, K> ? true : false;

type _ManagedSummaryActiveVersionRequired = Assert<
  IsRequiredKey<McpManagedSummary, "activeVersion">
>;
type _ManagedSummaryActiveVersionNullable = Assert<
  IncludesNull<McpManagedSummary, "activeVersion">
>;
type _ManagedSummaryAvailableVersionRequired = Assert<
  IsRequiredKey<McpManagedSummary, "availableVersion">
>;
type _ManagedSummaryAvailableVersionNullable = Assert<
  IncludesNull<McpManagedSummary, "availableVersion">
>;
type _ManagedSummaryAvailableManifestDigestRequired = Assert<
  IsRequiredKey<McpManagedSummary, "availableManifestDigest">
>;
type _ManagedSummaryAvailableManifestDigestNullable = Assert<
  IncludesNull<McpManagedSummary, "availableManifestDigest">
>;
type _ManagedSummaryCurrentTaskRequired = Assert<
  IsRequiredKey<McpManagedSummary, "currentTask">
>;
type _ManagedSummaryCurrentTaskNullable = Assert<
  IncludesNull<McpManagedSummary, "currentTask">
>;
type _ManagedSummaryCredentialStatusOptional = Assert<
  IsOptionalKey<McpManagedSummary, "credentialStatus">
>;
type _ManagedSummaryExternalCapabilityOptional = Assert<
  IsOptionalKey<McpManagedSummary, "externalCapability">
>;
type _ManagedPageNextCursorRequired = Assert<
  IsRequiredKey<McpManagedPage, "nextCursor">
>;
type _ManagedPageNextCursorNullable = Assert<
  IncludesNull<McpManagedPage, "nextCursor">
>;
type _HealthObservationCapabilitiesDigestRequired = Assert<
  IsRequiredKey<McpHealthObservation, "capabilitiesDigest">
>;
type _HealthObservationCapabilitiesDigestNullable = Assert<
  IncludesNull<McpHealthObservation, "capabilitiesDigest">
>;
type _HealthObservationToolsDigestRequired = Assert<
  IsRequiredKey<McpHealthObservation, "toolsDigest">
>;
type _HealthObservationToolsDigestNullable = Assert<
  IncludesNull<McpHealthObservation, "toolsDigest">
>;
type _HealthStatusLatestRequired = Assert<
  IsRequiredKey<McpHealthStatus, "latest">
>;
type _HealthStatusLatestNullable = Assert<IncludesNull<McpHealthStatus, "latest">>;
type _HealthStatusCredentialStatusOptional = Assert<
  IsOptionalKey<McpHealthStatus, "credentialStatus">
>;
type _ManagedDetailLatestHealthRequired = Assert<
  IsRequiredKey<McpManagedDetail, "latestHealth">
>;
type _ManagedDetailLatestHealthNullable = Assert<
  IncludesNull<McpManagedDetail, "latestHealth">
>;
type _ManagedDetailRegistrationTaskRequired = Assert<
  IsRequiredKey<McpManagedDetail, "registrationTask">
>;
type _ManagedDetailRegistrationTaskNullable = Assert<
  IncludesNull<McpManagedDetail, "registrationTask">
>;
type _ManagedDetailSupplyChainOptional = Assert<
  IsOptionalKey<McpManagedDetail, "supplyChain">
>;

function managedSummaryWithPhaseCapabilities(
  phaseCapabilities: unknown,
): Record<string, unknown> {
  return {
    managedMcpId: "managed-a",
    mcpId: "managed-a",
    installationScope: "user",
    registration: "registered",
    installation: "installed",
    runtime: "running",
    health: "healthy",
    defaultEnabled: true,
    revision: 7,
    updatedAtMs: 7,
    distributionAdapter: "archive",
    activeVersion: null,
    availableVersion: null,
    availableManifestDigest: null,
    currentTask: null,
    recoveryRequired: false,
    eligibility: {
      update: true,
      repair: true,
      uninstall: true,
      reason: "eligible",
      updateReason: "eligible",
      repairReason: "eligible",
      uninstallReason: "eligible",
    },
    nextAction: "none",
    phaseCapabilities,
  };
}

function cloneJson<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function canonicalTaskRef(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  const outcome: McpTaskRef["outcome"] = {
    state: "pending",
    error: {
      code: "adapter_failed",
      retryable: true,
      correlationId: "task-managed-a",
      message: "adapter failed",
      suggestion: "retry",
    },
    rollback: "pending",
    finalization: "pending",
    remainingEffects: ["connection_projection", "managed_installation"],
    nextAction: "wait",
  };
  return {
    taskId: "task-managed-a",
    operation: "update",
    status: "running",
    progress: 12,
    cancellable: true,
    revision: 8,
    updatedAtMs: 1_720_000_000_008,
    outcome,
    ...overrides,
  };
}

function canonicalHealthObservation(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    taskId: "task-health-a",
    mode: "runtime",
    result: "healthy",
    latencyMs: 321,
    capabilitiesDigest: "sha256:capabilities",
    toolsDigest: "sha256:tools",
    checkedAtMs: 1_720_000_000_321,
    detailCode: "mcp_list_tools_succeeded",
    ...overrides,
  };
}

function canonicalHealthStatus(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    managedMcpId: "managed-a",
    state: "healthy",
    latest: canonicalHealthObservation(),
    credentialStatus: "ready",
    ...overrides,
  };
}

function canonicalManagedSummary(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    ...managedSummaryWithPhaseCapabilities({
      sessionEnablement: "available",
      toolPolicy: "not_available_in_this_phase",
      profiles: "available",
      modelSuggestions: "available",
    }),
    activeVersion: "2.0.0",
    availableVersion: "2.1.0",
    availableManifestDigest: "sha256:available",
    currentTask: canonicalTaskRef(),
    credentialStatus: "ready",
    externalCapability: "docker_daemon_verified",
    ...overrides,
  };
}

function canonicalManagedDetail(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    summary: canonicalManagedSummary(),
    distributionAdapter: "archive",
    activeManifestDigest: "sha256:active",
    activeVersion: "2.0.0",
    extensionConfigKey: "managed-a",
    projectionDigest: "projection-managed-a",
    latestHealth: canonicalHealthObservation(),
    registrationTask: canonicalTaskRef({
      taskId: "task-register-a",
      operation: "register",
      status: "queued",
      progress: 0,
      outcome: {
        state: "pending",
        rollback: "not_required",
        finalization: "pending",
        remainingEffects: [],
        nextAction: "wait",
      },
    }),
    supplyChain: {
      type: "docker",
      image: "registry.example.test/managed-a:2.0.0",
      image_digest: "sha256:image",
      adapter_version: "1.2.3",
      daemon_version: "27.1.0",
      rootless: true,
      mount_plan_digest: "sha256:mount-plan",
      created_at_ms: 1_720_000_000_456,
    },
    ...overrides,
  };
}

function requestOpaqueResponse<T>(
  raw: Record<string, unknown>,
  method = "goose.mcpOpaque_unstable",
) {
  const client = new McpPlatformClient({
    async extMethod(receivedMethod) {
      assert.equal(receivedMethod, method);
      return raw;
    },
  });
  const request = Reflect.get(client, "request") as (
    method: string,
    params: object,
  ) => Promise<{ outcome: { status: string; value?: T } }>;
  return request.call(client, method, {});
}

test("MCP Platform client sends the typed catalog request to the formal method", async () => {
  let receivedMethod = "";
  let receivedParams: Record<string, unknown> = {};
  const client = new McpPlatformClient({
    async extMethod(method, params) {
      receivedMethod = method;
      receivedParams = params;
      return {
        outcome: {
          status: "success",
          value: {
            items: [],
            cache: {
              offline: true,
              localPersistenceOnly: true,
              freshness: "empty",
              refreshState: "local_only",
              recovery: "none",
            },
          },
        },
      };
    },
  });

  const response = await client.mcpCatalogList_unstable({
    query: "database",
    trustTiers: ["official"],
    pageSize: 24,
  });

  assert.equal(receivedMethod, "goose.mcpCatalogList_unstable");
  assert.deepEqual(receivedParams, {
    query: "database",
    trustTiers: ["official"],
    pageSize: 24,
  });
  assert.equal(response.outcome.status, "success");
});

test("MCP Platform client rejects malformed outcomes before the desktop adapter", async () => {
  const client = new McpPlatformClient({
    async extMethod() {
      return { result: "unexpected" };
    },
  });

  await assert.rejects(
    () => client.mcpSourcesPolicyGet_unstable(),
    /invalid MCP Platform outcome/,
  );
});

test("unregistered MCP success methods accept deep JSON payloads and return a detached safe copy", async () => {
  const rawValue = {
    nested: [
      {
        ok: true,
        nullable: null,
      },
    ],
    inner: {
      list: [1, "two", false, null, { leaf: "kept" }],
    },
  };

  const response = await requestOpaqueResponse<typeof rawValue>({
    outcome: {
      status: "success",
      value: rawValue,
    },
  });

  assert.equal(response.outcome.status, "success");
  assert.deepEqual(
    response.outcome.status === "success" ? response.outcome.value : null,
    rawValue,
  );

  rawValue.nested[0]!.ok = false;
  (rawValue.inner.list[4] as { leaf: string }).leaf = "mutated";

  assert.deepEqual(
    response.outcome.status === "success" ? response.outcome.value : null,
    {
      nested: [{ ok: true, nullable: null }],
      inner: {
        list: [1, "two", false, null, { leaf: "kept" }],
      },
    },
  );
});

test("unregistered MCP success methods preserve valid null and primitive JSON values", async () => {
  for (const value of [null, false, 0, "opaque-value"] as const) {
    const response = await requestOpaqueResponse<typeof value>({
      outcome: {
        status: "success",
        value,
      },
    });

    assert.equal(response.outcome.status, "success");
    assert.deepEqual(
      response.outcome.status === "success" ? response.outcome.value : null,
      value,
    );
  }
});

test("MCP Platform client rejects success outcomes that omit an own value property", async () => {
  await assert.rejects(
    () =>
      requestOpaqueResponse({
        outcome: {
          status: "success",
        },
      }),
    /success without a value/,
  );
});

test("MCP Platform client rejects non-JSON nested success values without invoking getters", async () => {
  class NestedRecord {
    ok = true;
  }

  let getterCalls = 0;
  const accessorObject: Record<string, unknown> = {};
  Object.defineProperty(accessorObject, "secret", {
    enumerable: true,
    get() {
      getterCalls += 1;
      throw new Error("getter must not run");
    },
  });

  let arrayGetterCalls = 0;
  const accessorArray: unknown[] = [];
  Object.defineProperty(accessorArray, "0", {
    enumerable: true,
    get() {
      arrayGetterCalls += 1;
      throw new Error("array getter must not run");
    },
  });
  accessorArray.length = 1;

  const nonEnumerableObject = { ok: true };
  Object.defineProperty(nonEnumerableObject, "hidden", {
    enumerable: false,
    value: "secret",
  });

  const symbolObject = { ok: true };
  Object.defineProperty(symbolObject, Symbol("hidden"), {
    enumerable: true,
    value: "secret",
  });

  const cycle: Record<string, unknown> = { ok: true };
  cycle.self = cycle;

  const protoKeyObject = JSON.parse('{"ok":true,"__proto__":{}}') as Record<
    string,
    unknown
  >;

  const invalidNestedCases: Array<[string, unknown]> = [
    ["inherited object", Object.assign(Object.create({ inherited: true }), { ok: true })],
    ["null-prototype object", Object.assign(Object.create(null), { ok: true })],
    ["class instance", new NestedRecord()],
    ["accessor object", accessorObject],
    ["accessor array", accessorArray],
    ["non-enumerable field", nonEnumerableObject],
    ["symbol field", symbolObject],
    ["cycle", cycle],
    ["dangerous proto key", protoKeyObject],
  ];

  for (const [label, nested] of invalidNestedCases) {
    await assert.rejects(
      () =>
        requestOpaqueResponse({
          outcome: {
            status: "success",
            value: {
              nested,
            },
          },
        }),
      /invalid MCP Platform success payload/,
      label,
    );
  }

  assert.equal(getterCalls, 0);
  assert.equal(arrayGetterCalls, 0);
});

test("manual connection contracts never send secret-bearing or free-form process fields", async () => {
  const calls: Array<{ method: string; params: unknown }> = [];
  const client = new McpPlatformClient({
    async extMethod(method, params) {
      calls.push({ method, params });
      return {
        outcome: {
          status: "error",
          error: {
            code: "invalid_request",
            message: "not available",
            retryable: false,
            correlationId: "test",
            details: {
              type: "request_rejected",
            },
          },
        },
      };
    },
  });

  await client.mcpManualPlanCreate_unstable({
    connection: {
      type: "remote_http",
      endpoint: "https://mcp.example.test",
      auth: { type: "bearer_reference", authReference: "credential-handle" },
    },
    idempotencyKey: "remote-plan",
  });
  await client.mcpManualPlanCreate_unstable({
    connection: { type: "stdio_provider", sourceId: "approved-source" },
    idempotencyKey: "stdio-plan",
  });

  assert.deepEqual(calls[0], {
    method: "goose.mcpManualPlanCreate_unstable",
    params: {
      connection: {
        type: "remote_http",
        endpoint: "https://mcp.example.test",
        auth: { type: "bearer_reference", authReference: "credential-handle" },
      },
      idempotencyKey: "remote-plan",
    },
  });
  assert.deepEqual(calls[1], {
    method: "goose.mcpManualPlanCreate_unstable",
    params: {
      connection: { type: "stdio_provider", sourceId: "approved-source" },
      idempotencyKey: "stdio-plan",
    },
  });
  const serialized = JSON.stringify(calls);
  for (const forbidden of [
    "token",
    "password",
    "headers",
    "command",
    "args",
    "env",
    "cwd",
  ]) {
    assert.equal(serialized.includes(`\"${forbidden}\"`), false);
  }
});

test("catalog-backed plan contracts send explicit source-bound targets without free-form execution fields", async () => {
  const calls: Array<{ method: string; params: unknown }> = [];
  const client = new McpPlatformClient({
    async extMethod(method, params) {
      calls.push({ method, params });
      return {
        outcome: {
          status: "success",
          value: {
            planId: "plan-source-bound",
            planDigest: "digest-plan-source-bound",
            expiresAtMs: 1_720_000_000_999,
            sourceId: "verified_source_catalog_source-a",
            catalogTarget: {
              sourceId: "verified_source_catalog_source-a",
              mcpId: "pkg.source.bound",
              version: "1.2.3",
              manifestDigest: "a".repeat(64),
            },
            proof: { type: "local_bytes" },
            trustTier: "community",
            publisher: {
              id: "publisher-a",
              name: "Publisher A",
              signingIdentities: [],
            },
            mcpId: "pkg.source.bound",
            name: "Source Bound",
            version: "1.2.3",
            selectedManifestDigest: "a".repeat(64),
            immutableEvidence: { type: "unavailable", reason: "not_applicable" },
            permissions: [],
            networkOrigins: [],
            fileEffects: {
              writesFiles: false,
              removesFiles: false,
              ownedItems: 0,
            },
            hostEffects: { registrationIds: [] },
            processEffects: {
              processRequiredForConnection: false,
              startsDuringConfirmation: false,
            },
            reversibility: { reversible: true, strategy: { type: "unavailable" } },
            policy: { outcome: "allow", reasons: [] },
            warnings: [],
            requiredConfirmations: [],
            defaultDisabled: true,
            recovery: "none",
          },
        },
      };
    },
  });

  const response = await client.mcpPlanCreate_unstable({
    intent: {
      type: "install_catalog",
      catalog: {
        sourceId: "verified_source_catalog_source-a",
        mcpId: "pkg.source.bound",
        version: "1.2.3",
        manifestDigest: "a".repeat(64),
      },
    },
    idempotencyKey: "catalog-install-plan",
  });

  assert.deepEqual(calls[0], {
    method: "goose.mcpPlanCreate_unstable",
    params: {
      intent: {
        type: "install_catalog",
        catalog: {
          sourceId: "verified_source_catalog_source-a",
          mcpId: "pkg.source.bound",
          version: "1.2.3",
          manifestDigest: "a".repeat(64),
        },
      },
      idempotencyKey: "catalog-install-plan",
    },
  });
  assert.equal(response.outcome.status, "success");
  assert.deepEqual(
    response.outcome.status === "success"
      ? response.outcome.value.catalogTarget
      : null,
    {
      sourceId: "verified_source_catalog_source-a",
      mcpId: "pkg.source.bound",
      version: "1.2.3",
      manifestDigest: "a".repeat(64),
    },
  );

  const serialized = JSON.stringify(calls);
  for (const forbidden of ["command", "path", "env", "cwd", "shell"]) {
    assert.equal(serialized.includes(`\"${forbidden}\"`), false);
  }
});

test("managed mutation sends the caller's expected revision", async () => {
  let receivedParams: unknown;
  const client = new McpPlatformClient({
    async extMethod(_method, params) {
      receivedParams = params;
      return {
        outcome: {
          status: "error",
          error: {
            code: "revision_conflict",
            message: "changed",
            retryable: true,
            correlationId: "test",
            details: {
              type: "revision_conflict",
            },
          },
        },
      };
    },
  });

  await client.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 17,
  });

  assert.deepEqual(receivedParams, {
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 17,
  });
});

test("managed summaries accept available phase capabilities from the formal wire contract", async () => {
  const expectedPhaseCapabilities: McpManagedSummary["phaseCapabilities"] = {
    sessionEnablement: "available",
    toolPolicy: "not_available_in_this_phase",
    profiles: "available",
    modelSuggestions: "available",
  };
  const client = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "success",
          value: {
            items: [managedSummaryWithPhaseCapabilities(expectedPhaseCapabilities)],
            nextCursor: null,
          },
        },
      };
    },
  });

  const response = await client.mcpList_unstable({});

  assert.equal(response.outcome.status, "success");
  assert.deepEqual(
    response.outcome.status === "success"
      ? response.outcome.value.items[0]?.phaseCapabilities
      : null,
    expectedPhaseCapabilities,
  );
});

test("managed detail and set-default responses accept both formal phase capability values", async () => {
  const expectedPhaseCapabilities: McpManagedSummary["phaseCapabilities"] = {
    sessionEnablement: "not_available_in_this_phase",
    toolPolicy: "available",
    profiles: "available",
    modelSuggestions: "not_available_in_this_phase",
  };
  const client = new McpPlatformClient({
    async extMethod(method) {
      if (method === "goose.mcpGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: canonicalManagedDetail({
              summary: canonicalManagedSummary({
                phaseCapabilities: expectedPhaseCapabilities,
              }),
            }),
          },
        };
      }
      return {
        outcome: {
          status: "success",
          value: managedSummaryWithPhaseCapabilities(expectedPhaseCapabilities),
        },
      };
    },
  });

  const detail = await client.mcpGet_unstable({ managedMcpId: "managed-a" });
  const summary = await client.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 7,
  });

  assert.deepEqual(
    detail.outcome.status === "success"
      ? detail.outcome.value.summary.phaseCapabilities
      : null,
    expectedPhaseCapabilities,
  );
  assert.deepEqual(
    summary.outcome.status === "success"
      ? summary.outcome.value.phaseCapabilities
      : null,
    expectedPhaseCapabilities,
  );
});

test("managed parsers accept canonical managed wire payloads for list/get/set-default", async () => {
  const canonicalSummary = canonicalManagedSummary();
  const canonicalDetail = canonicalManagedDetail();
  const client = new McpPlatformClient({
    async extMethod(method) {
      if (method === "goose.mcpList_unstable") {
        return {
          outcome: {
            status: "success",
            value: {
              items: [canonicalSummary],
              nextCursor: "cursor-managed-a",
            },
          },
        };
      }
      if (method === "goose.mcpGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: canonicalDetail,
          },
        };
      }
      return {
        outcome: {
          status: "success",
          value: canonicalSummary,
        },
      };
    },
  });

  const list = await client.mcpList_unstable({});
  const detail = await client.mcpGet_unstable({ managedMcpId: "managed-a" });
  const summary = await client.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 7,
  });

  assert.deepEqual(
    list.outcome.status === "success" ? list.outcome.value : null,
    {
      items: [canonicalSummary],
      nextCursor: "cursor-managed-a",
    },
  );
  assert.deepEqual(
    detail.outcome.status === "success" ? detail.outcome.value : null,
    canonicalDetail,
  );
  assert.deepEqual(
    summary.outcome.status === "success" ? summary.outcome.value : null,
    canonicalSummary,
  );
});

test("managed and health parsers accept the constrained credential status projection", async () => {
  const expectedCredentialStatus: McpCredentialStatus = "trusted_state_conflict";
  const client = new McpPlatformClient({
    async extMethod(method) {
      if (method === "goose.mcpGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: canonicalManagedDetail({
              summary: canonicalManagedSummary({
                credentialStatus: expectedCredentialStatus,
              }),
            }),
          },
        };
      }
      if (method === "goose.mcpHealthGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: canonicalHealthStatus({
              credentialStatus: expectedCredentialStatus,
            }),
          },
        };
      }
      return {
        outcome: {
          status: "success",
          value: canonicalManagedSummary({
            credentialStatus: expectedCredentialStatus,
          }),
        },
      };
    },
  });

  const detail = await client.mcpGet_unstable({ managedMcpId: "managed-a" });
  const health = await client.mcpHealthGet_unstable({ managedMcpId: "managed-a" });
  const summary = await client.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 7,
  });

  assert.equal(
    detail.outcome.status === "success"
      ? detail.outcome.value.summary.credentialStatus
      : null,
    expectedCredentialStatus,
  );
  assert.equal(
    health.outcome.status === "success"
      ? health.outcome.value.credentialStatus
      : null,
    expectedCredentialStatus,
  );
  assert.equal(
    summary.outcome.status === "success"
      ? summary.outcome.value.credentialStatus
      : null,
    expectedCredentialStatus,
  );
});

test("managed parsers accept Rust null and omission matrices across managed and health routes", async () => {
  const nullableSummary = canonicalManagedSummary({
    activeVersion: null,
    availableVersion: null,
    availableManifestDigest: null,
    currentTask: null,
  });
  delete (nullableSummary as Record<string, unknown>).credentialStatus;
  delete (nullableSummary as Record<string, unknown>).externalCapability;

  const detailSummary = canonicalManagedSummary({
    activeVersion: null,
    availableVersion: null,
    availableManifestDigest: null,
    currentTask: canonicalTaskRef({
      outcome: {
        state: "pending",
        rollback: "not_required",
        finalization: "pending",
        remainingEffects: [],
        nextAction: "wait",
      },
    }),
  });
  delete (detailSummary as Record<string, unknown>).externalCapability;

  const nullableDetail = canonicalManagedDetail({
    summary: detailSummary,
    latestHealth: null,
    registrationTask: null,
  });
  delete (nullableDetail as Record<string, unknown>).supplyChain;

  const nullableHealthStatus = canonicalHealthStatus({
    credentialStatus: undefined,
    latest: canonicalHealthObservation({
      capabilitiesDigest: null,
      toolsDigest: null,
    }),
  });
  delete (nullableHealthStatus as Record<string, unknown>).credentialStatus;

  const client = new McpPlatformClient({
    async extMethod(method) {
      if (method === "goose.mcpList_unstable") {
        return {
          outcome: {
            status: "success",
            value: {
              items: [nullableSummary],
              nextCursor: null,
            },
          },
        };
      }
      if (method === "goose.mcpGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: nullableDetail,
          },
        };
      }
      if (method === "goose.mcpHealthGet_unstable") {
        return {
          outcome: {
            status: "success",
            value: nullableHealthStatus,
          },
        };
      }
      return {
        outcome: {
          status: "success",
          value: nullableSummary,
        },
      };
    },
  });

  const list = await client.mcpList_unstable({});
  const detail = await client.mcpGet_unstable({ managedMcpId: "managed-a" });
  const health = await client.mcpHealthGet_unstable({ managedMcpId: "managed-a" });
  const summary = await client.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 7,
  });

  assert.deepEqual(
    list.outcome.status === "success" ? list.outcome.value : null,
    {
      items: [nullableSummary],
      nextCursor: null,
    },
  );
  assert.deepEqual(
    detail.outcome.status === "success" ? detail.outcome.value : null,
    nullableDetail,
  );
  assert.deepEqual(
    health.outcome.status === "success" ? health.outcome.value : null,
    nullableHealthStatus,
  );
  assert.deepEqual(
    summary.outcome.status === "success" ? summary.outcome.value : null,
    nullableSummary,
  );
});

test("managed and health parsers reject unknown credential status values", async () => {
  for (const [method, value] of [
    [
      "goose.mcpGet_unstable",
      canonicalManagedDetail({
        summary: canonicalManagedSummary({ credentialStatus: "secret" }),
      }),
    ],
    [
      "goose.mcpHealthGet_unstable",
      canonicalHealthStatus({ credentialStatus: "secret" }),
    ],
    [
      "goose.mcpSetDefaultEnabled_unstable",
      canonicalManagedSummary({ credentialStatus: "secret" }),
    ],
  ] as const) {
    const client = new McpPlatformClient({
      async extMethod(receivedMethod) {
        assert.equal(receivedMethod, method);
        return {
          outcome: {
            status: "success",
            value,
          },
        };
      },
    });

    await assert.rejects(
      async () => {
        if (method === "goose.mcpGet_unstable") {
          await client.mcpGet_unstable({ managedMcpId: "managed-a" });
          return;
        }
        if (method === "goose.mcpHealthGet_unstable") {
          await client.mcpHealthGet_unstable({ managedMcpId: "managed-a" });
          return;
        }
        await client.mcpSetDefaultEnabled_unstable({
          managedMcpId: "managed-a",
          enabled: true,
          expectedRevision: 7,
        });
      },
      /invalid MCP Platform success payload/,
    );
  }
});

test("managed parsers reject null and omission mismatches against the Rust wire contract", async () => {
  const missingNextCursor = {
    items: [canonicalManagedSummary()],
  };

  const missingActiveVersion = canonicalManagedSummary();
  delete (missingActiveVersion as Record<string, unknown>).activeVersion;

  const missingAvailableVersion = canonicalManagedSummary();
  delete (missingAvailableVersion as Record<string, unknown>).availableVersion;

  const missingAvailableManifestDigest = canonicalManagedSummary();
  delete (
    missingAvailableManifestDigest as Record<string, unknown>
  ).availableManifestDigest;

  const missingCurrentTask = canonicalManagedSummary();
  delete (missingCurrentTask as Record<string, unknown>).currentTask;

  const nullExternalCapability = canonicalManagedSummary({
    externalCapability: null,
  });

  const nullTaskOutcomeError = canonicalManagedSummary({
    currentTask: canonicalTaskRef({
      outcome: {
        state: "failed",
        error: null,
        rollback: "pending",
        finalization: "pending",
        remainingEffects: ["managed_installation"],
        nextAction: "wait",
      },
    }),
  });

  const missingLatestHealth = canonicalManagedDetail();
  delete (missingLatestHealth as Record<string, unknown>).latestHealth;

  const missingRegistrationTask = canonicalManagedDetail();
  delete (missingRegistrationTask as Record<string, unknown>).registrationTask;

  const nullSupplyChain = canonicalManagedDetail({
    supplyChain: null,
  });

  const missingHealthLatest = {
    managedMcpId: "managed-a",
    state: "healthy",
  };

  const missingHealthCapabilitiesDigest = canonicalHealthStatus({
    latest: {
      taskId: "task-health-a",
      mode: "runtime",
      result: "healthy",
      latencyMs: 321,
      toolsDigest: null,
      checkedAtMs: 1_720_000_000_321,
      detailCode: "mcp_list_tools_succeeded",
    },
  });

  const missingHealthToolsDigest = canonicalHealthStatus({
    latest: {
      taskId: "task-health-a",
      mode: "runtime",
      result: "healthy",
      latencyMs: 321,
      capabilitiesDigest: null,
      checkedAtMs: 1_720_000_000_321,
      detailCode: "mcp_list_tools_succeeded",
    },
  });

  const invalidCases: Array<{
    label: string;
    method:
      | "goose.mcpList_unstable"
      | "goose.mcpGet_unstable"
      | "goose.mcpSetDefaultEnabled_unstable"
      | "goose.mcpHealthGet_unstable";
    value: unknown;
  }> = [
    { label: "missing nextCursor", method: "goose.mcpList_unstable", value: missingNextCursor },
    {
      label: "missing activeVersion",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: missingActiveVersion,
    },
    {
      label: "missing availableVersion",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: missingAvailableVersion,
    },
    {
      label: "missing availableManifestDigest",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: missingAvailableManifestDigest,
    },
    {
      label: "missing currentTask",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: missingCurrentTask,
    },
    {
      label: "null externalCapability",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: nullExternalCapability,
    },
    {
      label: "null task outcome error",
      method: "goose.mcpList_unstable",
      value: { items: [nullTaskOutcomeError], nextCursor: null },
    },
    {
      label: "missing latestHealth",
      method: "goose.mcpGet_unstable",
      value: missingLatestHealth,
    },
    {
      label: "missing registrationTask",
      method: "goose.mcpGet_unstable",
      value: missingRegistrationTask,
    },
    {
      label: "null supplyChain",
      method: "goose.mcpGet_unstable",
      value: nullSupplyChain,
    },
    {
      label: "missing health latest",
      method: "goose.mcpHealthGet_unstable",
      value: missingHealthLatest,
    },
    {
      label: "missing health capabilitiesDigest",
      method: "goose.mcpHealthGet_unstable",
      value: missingHealthCapabilitiesDigest,
    },
    {
      label: "missing health toolsDigest",
      method: "goose.mcpHealthGet_unstable",
      value: missingHealthToolsDigest,
    },
  ];

  for (const invalidCase of invalidCases) {
    const client = new McpPlatformClient({
      async extMethod() {
        return {
          outcome: {
            status: "success",
            value: invalidCase.value,
          },
        };
      },
    });

    const call =
      invalidCase.method === "goose.mcpList_unstable"
        ? () => client.mcpList_unstable({})
        : invalidCase.method === "goose.mcpGet_unstable"
          ? () => client.mcpGet_unstable({ managedMcpId: "managed-a" })
          : invalidCase.method === "goose.mcpHealthGet_unstable"
            ? () => client.mcpHealthGet_unstable({ managedMcpId: "managed-a" })
            : () =>
                client.mcpSetDefaultEnabled_unstable({
                  managedMcpId: "managed-a",
                  enabled: true,
                  expectedRevision: 7,
                });

    await assert.rejects(
      call,
      /invalid MCP Platform success payload/,
      invalidCase.label,
    );
  }
});

test("managed summary parsers reject missing and malformed required fields across every related success route", async () => {
  const invalidFieldCases: Array<[string, unknown]> = [
    ["managedMcpId", 7],
    ["mcpId", 7],
    ["installationScope", "system"],
    ["registration", "registering"],
    ["installation", "broken"],
    ["runtime", "paused"],
    ["health", "recovering"],
    ["defaultEnabled", "true"],
    ["revision", "7"],
    ["updatedAtMs", "7"],
    ["distributionAdapter", false],
    ["recoveryRequired", "false"],
    ["eligibility", []],
    ["nextAction", "enable_now"],
    ["phaseCapabilities", { sessionEnablement: "bogus" }],
  ];

  for (const [field, invalidValue] of invalidFieldCases) {
    for (const scenario of ["missing", "wrong"] as const) {
      const invalidSummary = cloneJson(canonicalManagedSummary());
      if (scenario === "missing") {
        delete (invalidSummary as Record<string, unknown>)[field];
      } else {
        (invalidSummary as Record<string, unknown>)[field] = invalidValue;
      }

      const client = new McpPlatformClient({
        async extMethod(method) {
          if (method === "goose.mcpList_unstable") {
            return {
              outcome: {
                status: "success",
                value: { items: [invalidSummary] },
              },
            };
          }
          if (method === "goose.mcpGet_unstable") {
            const detail = canonicalManagedDetail({ summary: invalidSummary });
            return {
              outcome: {
                status: "success",
                value: detail,
              },
            };
          }
          return {
            outcome: {
              status: "success",
              value: invalidSummary,
            },
          };
        },
      });

      await assert.rejects(
        () => client.mcpList_unstable({}),
        /invalid MCP Platform success payload/,
        `mcpList ${field} ${scenario}`,
      );
      await assert.rejects(
        () => client.mcpGet_unstable({ managedMcpId: "managed-a" }),
        /invalid MCP Platform success payload/,
        `mcpGet ${field} ${scenario}`,
      );
      await assert.rejects(
        () =>
          client.mcpSetDefaultEnabled_unstable({
            managedMcpId: "managed-a",
            enabled: true,
            expectedRevision: 7,
          }),
        /invalid MCP Platform success payload/,
        `mcpSetDefaultEnabled ${field} ${scenario}`,
      );
    }
  }
});

test("managed detail parser rejects missing and malformed required detail fields", async () => {
  const invalidFieldCases: Array<[string, unknown]> = [
    ["distributionAdapter", false],
    ["activeManifestDigest", false],
    ["activeVersion", false],
    ["extensionConfigKey", false],
    ["projectionDigest", false],
  ];

  for (const [field, invalidValue] of invalidFieldCases) {
    for (const scenario of ["missing", "wrong"] as const) {
      const invalidDetail = cloneJson(canonicalManagedDetail());
      if (scenario === "missing") {
        delete (invalidDetail as Record<string, unknown>)[field];
      } else {
        (invalidDetail as Record<string, unknown>)[field] = invalidValue;
      }

      const client = new McpPlatformClient({
        async extMethod() {
          return {
            outcome: {
              status: "success",
              value: invalidDetail,
            },
          };
        },
      });

      await assert.rejects(
        () => client.mcpGet_unstable({ managedMcpId: "managed-a" }),
        /invalid MCP Platform success payload/,
        `${field} ${scenario}`,
      );
    }
  }
});

test("managed parsers reject unexpected keys and malformed nested managed payloads", async () => {
  const invalidCases: Array<{
    label: string;
    method: "goose.mcpList_unstable" | "goose.mcpGet_unstable" | "goose.mcpSetDefaultEnabled_unstable";
    value: unknown;
  }> = [
    {
      label: "page extra key",
      method: "goose.mcpList_unstable",
      value: { items: [canonicalManagedSummary()], extra: true },
    },
    {
      label: "summary extra key",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: { ...canonicalManagedSummary(), extra: true },
    },
    {
      label: "detail extra key",
      method: "goose.mcpGet_unstable",
      value: { ...canonicalManagedDetail(), extra: true },
    },
    {
      label: "malformed currentTask",
      method: "goose.mcpList_unstable",
      value: {
        items: [
          canonicalManagedSummary({
            currentTask: canonicalTaskRef({ progress: 256 }),
          }),
        ],
      },
    },
    {
      label: "malformed eligibility",
      method: "goose.mcpSetDefaultEnabled_unstable",
      value: {
        ...canonicalManagedSummary(),
        eligibility: { update: true, repair: true, uninstall: true, reason: "eligible" },
      },
    },
    {
      label: "malformed latestHealth",
      method: "goose.mcpGet_unstable",
      value: canonicalManagedDetail({
        latestHealth: canonicalHealthObservation({ detailCode: "bogus" }),
      }),
    },
    {
      label: "malformed registrationTask",
      method: "goose.mcpGet_unstable",
      value: canonicalManagedDetail({
        registrationTask: canonicalTaskRef({
          outcome: {
            state: "pending",
            rollback: "not_required",
            finalization: "pending",
            remainingEffects: ["bogus"],
            nextAction: "wait",
          },
        }),
      }),
    },
    {
      label: "malformed supplyChain tag",
      method: "goose.mcpGet_unstable",
      value: canonicalManagedDetail({
        supplyChain: { type: "future_supply_chain" },
      }),
    },
    {
      label: "malformed supplyChain payload",
      method: "goose.mcpGet_unstable",
      value: canonicalManagedDetail({
        supplyChain: {
          type: "docker",
          image: "registry.example.test/managed-a:2.0.0",
          image_digest: "sha256:image",
          adapter_version: "1.2.3",
          daemon_version: "27.1.0",
          rootless: "true",
          mount_plan_digest: "sha256:mount-plan",
          created_at_ms: 1_720_000_000_456,
        },
      }),
    },
  ];

  for (const invalidCase of invalidCases) {
    const client = new McpPlatformClient({
      async extMethod() {
        return {
          outcome: {
            status: "success",
            value: invalidCase.value,
          },
        };
      },
    });

    const call =
      invalidCase.method === "goose.mcpList_unstable"
        ? () => client.mcpList_unstable({})
        : invalidCase.method === "goose.mcpGet_unstable"
          ? () => client.mcpGet_unstable({ managedMcpId: "managed-a" })
          : () =>
              client.mcpSetDefaultEnabled_unstable({
                managedMcpId: "managed-a",
                enabled: true,
                expectedRevision: 7,
              });

    await assert.rejects(
      call,
      /invalid MCP Platform success payload/,
      invalidCase.label,
    );
  }
});

test("runtime-control summaries reject unknown capability fields and future values", async () => {
  const invalidCapabilities = [
    { binding: "binding", canStart: true, canStop: false, extra: true },
    { binding: "binding", canStart: "true", canStop: false },
  ];

  for (const runtimeControl of invalidCapabilities) {
    const client = new McpPlatformClient({
      async extMethod() {
        return {
          outcome: {
            status: "success",
            value: canonicalManagedSummary({ runtimeControl }),
          },
        };
      },
    });
    await assert.rejects(
      () =>
        client.mcpRuntimeControl_unstable({
          managedMcpId: "managed-a",
          expectedRevision: 7,
          action: "stop",
          runtimeBinding: "binding",
        }),
      /invalid MCP Platform success payload/,
    );
  }
});

test("managed phase capabilities fail closed for malformed values across every related success parser", async () => {
  const invalidPhaseCapabilitiesCases: Array<[string, unknown]> = [
    ["bogus value", {
      sessionEnablement: "available",
      toolPolicy: "bogus",
      profiles: "available",
      modelSuggestions: "available",
    }],
    ["unknown key", {
      sessionEnablement: "available",
      toolPolicy: "available",
      profiles: "available",
      modelSuggestions: "available",
      extraKey: "available",
    }],
    ["missing key", {
      sessionEnablement: "available",
      toolPolicy: "available",
      profiles: "available",
    }],
    ["null", null],
    ["nonobject", "available"],
  ];

  for (const [label, phaseCapabilities] of invalidPhaseCapabilitiesCases) {
    const client = new McpPlatformClient({
      async extMethod(method) {
        if (method === "goose.mcpGet_unstable") {
          return {
            outcome: {
              status: "success",
              value: {
                summary: managedSummaryWithPhaseCapabilities(phaseCapabilities),
              },
            },
          };
        }
        if (method === "goose.mcpSetDefaultEnabled_unstable") {
          return {
            outcome: {
              status: "success",
              value: managedSummaryWithPhaseCapabilities(phaseCapabilities),
            },
          };
        }
        return {
          outcome: {
            status: "success",
            value: {
              items: [managedSummaryWithPhaseCapabilities(phaseCapabilities)],
            },
          },
        };
      },
    });

    await assert.rejects(
      () => client.mcpList_unstable({}),
      new RegExp(`invalid MCP Platform success payload`),
      `mcpList ${label}`,
    );
    await assert.rejects(
      () => client.mcpGet_unstable({ managedMcpId: "managed-a" }),
      new RegExp(`invalid MCP Platform success payload`),
      `mcpGet ${label}`,
    );
    await assert.rejects(
      () =>
        client.mcpSetDefaultEnabled_unstable({
          managedMcpId: "managed-a",
          enabled: true,
          expectedRevision: 7,
        }),
      new RegExp(`invalid MCP Platform success payload`),
      `mcpSetDefaultEnabled ${label}`,
    );
  }
});

test("managed parsers reject inherited and dangerous wire objects without accepting prototype data", async () => {
  const inheritedOutcomeResponse = Object.create({
    outcome: {
      status: "success",
      value: { items: [managedSummaryWithPhaseCapabilities({
        sessionEnablement: "available",
        toolPolicy: "available",
        profiles: "available",
        modelSuggestions: "available",
      })] },
    },
  });
  const nullPrototypeResponse = Object.assign(Object.create(null), {
    outcome: {
      status: "success",
      value: { items: [managedSummaryWithPhaseCapabilities({
        sessionEnablement: "available",
        toolPolicy: "available",
        profiles: "available",
        modelSuggestions: "available",
      })] },
    },
  });
  const protoKeyResponse = JSON.parse(
    '{"outcome":{"status":"success","value":{"items":[]}},"__proto__":{}}',
  ) as Record<string, unknown>;

  for (const [label, response] of [
    ["inherited outcome", inheritedOutcomeResponse],
    ["null prototype", nullPrototypeResponse],
    ["__proto__ own key", protoKeyResponse],
  ] as const) {
    const client = new McpPlatformClient({
      async extMethod() {
        return response;
      },
    });

    await assert.rejects(
      () => client.mcpList_unstable({}),
      /invalid MCP Platform outcome/,
      label,
    );
  }
});

test("managed parsers reject accessor and non-enumerable required fields without invoking getters", async () => {
  let getterCalls = 0;
  const accessorResponse: Record<string, unknown> = {};
  Object.defineProperty(accessorResponse, "outcome", {
    enumerable: true,
    get() {
      getterCalls += 1;
      throw new Error("getter must not run");
    },
  });

  const client = new McpPlatformClient({
    async extMethod() {
      return accessorResponse;
    },
  });

  await assert.rejects(
    () => client.mcpList_unstable({}),
    /invalid MCP Platform outcome/,
  );
  assert.equal(getterCalls, 0);

  const nonEnumerableSummary = managedSummaryWithPhaseCapabilities({
    sessionEnablement: "available",
    toolPolicy: "available",
    profiles: "available",
    modelSuggestions: "available",
  });
  Object.defineProperty(nonEnumerableSummary, "phaseCapabilities", {
    enumerable: false,
    value: {
      sessionEnablement: "available",
      toolPolicy: "available",
      profiles: "available",
      modelSuggestions: "available",
    },
  });

  const clientWithHiddenField = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "success",
          value: {
            items: [nonEnumerableSummary],
          },
        },
      };
    },
  });

  await assert.rejects(
    () => clientWithHiddenField.mcpList_unstable({}),
    /invalid MCP Platform success payload/,
  );
});

test("managed parsers reject class instances, dates, arrays, and inherited error details", async () => {
  class ManagedSummaryRecord {
    managedMcpId = "managed-a";
    mcpId = "managed-a";
    installationScope = "user";
    registration = "registered";
    installation = "installed";
    runtime = "running";
    health = "healthy";
    defaultEnabled = true;
    revision = 7;
    updatedAtMs = 7;
    distributionAdapter = "archive";
    recoveryRequired = false;
    eligibility = {
      update: true,
      repair: true,
      uninstall: true,
      reason: "eligible",
      updateReason: "eligible",
      repairReason: "eligible",
      uninstallReason: "eligible",
    };
    nextAction = "none";
    phaseCapabilities = {
      sessionEnablement: "available",
      toolPolicy: "available",
      profiles: "available",
      modelSuggestions: "available",
    };
  }

  const invalidSuccessValues = [
    { label: "array", value: [] },
    { label: "date", value: new Date("2026-07-23T00:00:00.000Z") },
    { label: "class instance", value: new ManagedSummaryRecord() },
    {
      label: "inherited phase capabilities",
      value: Object.assign(
        Object.create({
          phaseCapabilities: {
            sessionEnablement: "available",
            toolPolicy: "available",
            profiles: "available",
            modelSuggestions: "available",
          },
        }),
        {
          managedMcpId: "managed-a",
          mcpId: "managed-a",
          installationScope: "user",
          registration: "registered",
          installation: "installed",
          runtime: "running",
          health: "healthy",
          defaultEnabled: true,
          revision: 7,
          updatedAtMs: 7,
          distributionAdapter: "archive",
          recoveryRequired: false,
          eligibility: {
            update: true,
            repair: true,
            uninstall: true,
            reason: "eligible",
            updateReason: "eligible",
            repairReason: "eligible",
            uninstallReason: "eligible",
          },
          nextAction: "none",
        },
      ),
    },
  ];

  for (const { label, value } of invalidSuccessValues) {
    const client = new McpPlatformClient({
      async extMethod() {
        return {
          outcome: {
            status: "success",
            value: {
              items: [value],
            },
          },
        };
      },
    });

    await assert.rejects(
      () => client.mcpList_unstable({}),
      /invalid MCP Platform success payload/,
      label,
    );
  }

  const inheritedErrorDetails = Object.assign(
    Object.create({ type: "phase_unavailable" }),
    {
      operation: "goose.mcpProfileModelRecommend_unstable",
      phase: "4B",
    },
  );
  const errorClient = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "error",
          error: {
            code: "not_implemented_for_phase",
            message: "closed",
            retryable: false,
            correlationId: "phase-4b",
            details: inheritedErrorDetails,
          },
        },
      };
    },
  });

  await assert.rejects(
    () => errorClient.mcpGet_unstable({ managedMcpId: "managed-a" }),
    /invalid MCP Platform error/,
  );
});

test("plain JSON responses preserve formal error details and both phase capability values", async () => {
  const expectedPhaseCapabilities: McpManagedSummary["phaseCapabilities"] = {
    sessionEnablement: "available",
    toolPolicy: "not_available_in_this_phase",
    profiles: "available",
    modelSuggestions: "not_available_in_this_phase",
  };
  const successClient = new McpPlatformClient({
    async extMethod(method) {
      return JSON.parse(
        JSON.stringify(
          method === "goose.mcpGet_unstable"
            ? {
                outcome: {
                  status: "success",
                  value: {
                    summary: managedSummaryWithPhaseCapabilities(expectedPhaseCapabilities),
                    distributionAdapter: "archive",
                    activeManifestDigest: "sha256:test",
                    activeVersion: "2.0.0",
                    extensionConfigKey: "managed-a",
                    projectionDigest: "projection",
                    latestHealth: null,
                    registrationTask: null,
                  },
                },
              }
            : {
                outcome: {
                  status: "success",
                  value: managedSummaryWithPhaseCapabilities(expectedPhaseCapabilities),
                },
              },
        ),
      );
    },
  });

  const detail = await successClient.mcpGet_unstable({ managedMcpId: "managed-a" });
  const summary = await successClient.mcpSetDefaultEnabled_unstable({
    managedMcpId: "managed-a",
    enabled: true,
    expectedRevision: 7,
  });

  assert.deepEqual(
    detail.outcome.status === "success"
      ? detail.outcome.value.summary.phaseCapabilities
      : null,
    expectedPhaseCapabilities,
  );
  assert.deepEqual(
    summary.outcome.status === "success"
      ? summary.outcome.value.phaseCapabilities
      : null,
    expectedPhaseCapabilities,
  );

  for (const error of [
    {
      code: "invalid_request",
      details: { type: "request_rejected" },
    },
    {
      code: "repository_unavailable",
      details: { type: "repository_temporarily_unavailable" },
    },
    {
      code: "revision_conflict",
      details: { type: "revision_conflict" },
    },
    {
      code: "rollback_incomplete",
      details: { type: "witness_expired" },
    },
    {
      code: "rollback_incomplete",
      details: { type: "witness_consumed" },
    },
    {
      code: "not_implemented_for_phase",
      details: {
        type: "phase_unavailable",
        phase: "4B",
        operation: "goose.mcpProfileModelRecommend_unstable",
      },
    },
  ] as const) {
    const client = new McpPlatformClient({
      async extMethod() {
        return JSON.parse(
          JSON.stringify({
            outcome: {
              status: "error",
              error: {
                ...error,
                message: "closed",
                retryable: false,
                correlationId: "correlation",
              },
            },
          }),
        );
      },
    });

    const response = await client.mcpGet_unstable({ managedMcpId: "managed-a" });
    assert.equal(response.outcome.status, "error");
    assert.deepEqual(
      response.outcome.status === "error" ? response.outcome.error.details : null,
      error.details,
    );
  }
});

test("error envelopes accept omitted details but reject null details", async () => {
  const missingDetailsClient = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "error",
          error: {
            code: "invalid_request",
            message: "closed",
            retryable: false,
            correlationId: "correlation",
          },
        },
      };
    },
  });

  const missingDetails = await missingDetailsClient.mcpGet_unstable({
    managedMcpId: "managed-a",
  });
  assert.equal(missingDetails.outcome.status, "error");
  assert.equal(
    missingDetails.outcome.status === "error"
      ? missingDetails.outcome.error.details
      : undefined,
    undefined,
  );

  const nullDetailsClient = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "error",
          error: {
            code: "invalid_request",
            message: "closed",
            retryable: false,
            correlationId: "correlation",
            details: null,
          },
        },
      };
    },
  });

  await assert.rejects(
    () => nullDetailsClient.mcpGet_unstable({ managedMcpId: "managed-a" }),
    /invalid MCP Platform error/,
  );

  const forgedWitnessDetailsClient = new McpPlatformClient({
    async extMethod() {
      return {
        outcome: {
          status: "error",
          error: {
            code: "rollback_incomplete",
            message: "closed",
            retryable: false,
            correlationId: "correlation",
            details: { type: "witness_expired", rawWitness: "secret" },
          },
        },
      };
    },
  });

  await assert.rejects(
    () => forgedWitnessDetailsClient.mcpGet_unstable({ managedMcpId: "managed-a" }),
    /invalid MCP Platform error/,
  );
});
