import assert from "node:assert/strict";
import { test } from "node:test";
import { McpPlatformClient } from "../src/mcp-platform.ts";

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

test("manual connection contracts never send secret-bearing or free-form process fields", async () => {
  const calls: Array<{ method: string; params: unknown }> = [];
  const client = new McpPlatformClient({
    async extMethod(method, params) {
      calls.push({ method, params });
      return {
        outcome: {
          status: "error",
          error: {
            code: "operation_not_supported",
            message: "not available",
            retryable: false,
            correlationId: "test",
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
