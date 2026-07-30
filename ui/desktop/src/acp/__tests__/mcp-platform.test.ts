import { describe, expect, it, vi } from 'vitest';
import type { McpPlatformOutcome } from '@aaif/goose-sdk';
const { getAcpClient } = vi.hoisted(() => ({
  getAcpClient: vi.fn(),
}));
vi.mock('../acpConnection', () => ({ getAcpClient }));
import {
  McpPlatformServiceError,
  confirmMcpPlan,
  isMcpPhaseUnavailableError,
  mapMcpPlatformError,
  sanitizeMcpUserMessage,
  toMcpRecoveryViewModel,
  unwrapMcpOutcome,
  parseHttpsProvisionPlanReview,
  projectHttpsProvisionPlanReviewRequest,
} from '../mcp-platform';

function hasIsolatedSurrogate(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const codeUnit = value.charCodeAt(index);
    if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return true;
      index += 1;
      continue;
    }
    if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) return true;
  }
  return false;
}

function phaseUnavailableServiceError(
  {
    code = 'not_implemented_for_phase',
    details,
  }: {
    code?: 'not_implemented_for_phase' | 'operation_not_supported';
    details?: unknown;
  } = {}
) {
  return new McpPlatformServiceError(
    {
      code,
      message: 'Unavailable in this phase',
      retryable: false,
      correlationId: 'phase-ref',
      details: details as never,
    },
    {
      title: 'Unavailable',
      message: 'Unavailable in this phase',
      nextStep: 'Wait for a later phase.',
      retryable: false,
      correlationId: 'phase-ref',
      code,
      kind: 'service',
    }
  );
}

describe('MCP Platform outcome adapter', () => {
  it('normalizes raw confirm rejection to a safe retryable error', async () => {
    const sentinel = 'proof=raw-secret url=https://secret.invalid digest=raw-digest';
    getAcpClient.mockResolvedValue({
      mcpPlatform: {
        mcpInstallConfirm_unstable: vi.fn().mockRejectedValue(new Error(sentinel)),
      },
    });

    await expect(confirmMcpPlan({ planId: 'plan', planDigest: 'digest' }, 'confirm'))
      .rejects.toMatchObject({
        message: 'Unable to complete MCP operation',
        recovery: {
          message: 'Unable to complete MCP operation',
          retryable: true,
        },
      });
    try {
      await confirmMcpPlan({ planId: 'plan', planDigest: 'digest' }, 'confirm');
    } catch (error) {
      expect(JSON.stringify(error)).not.toContain(sentinel);
    }
  });

  it('preserves service confirm errors and their classification', async () => {
    const serviceError = new McpPlatformServiceError(
      {
        code: 'plan_expired',
        message: 'Plan expired',
        retryable: false,
        correlationId: 'server-correlation',
      },
      {
        title: 'Plan expired',
        message: 'Plan expired',
        nextStep: 'Create and review a new plan before confirming.',
        retryable: false,
        correlationId: 'server-correlation',
        code: 'plan_expired',
        kind: 'service',
      }
    );
    getAcpClient.mockResolvedValue({
      mcpPlatform: {
        mcpInstallConfirm_unstable: vi.fn().mockRejectedValue(serviceError),
      },
    });

    await expect(confirmMcpPlan({ planId: 'plan', planDigest: 'digest' }, 'confirm'))
      .rejects.toBe(serviceError);
  });

  it('projects only the three HTTPS provision plan request fields', () => {
    expect(projectHttpsProvisionPlanReviewRequest({
      provisionId: 'p', expectedManifestDigest: 'd', idempotencyKey: 'i',
    })).toEqual({ provisionId: 'p', expectedManifestDigest: 'd', idempotencyKey: 'i' });
    expect(() => projectHttpsProvisionPlanReviewRequest({
      provisionId: 'p', expectedManifestDigest: 'd', idempotencyKey: 'i', url: 'https://secret',
    } as never)).toThrow();
  });

  it('parses a safe HTTPS provision review and rejects sensitive extensions', () => {
    const review = {
      planId: 'plan', planDigest: 'digest', expiresAtMs: 1, trustTier: 'trusted', mcpId: 'mcp',
      name: 'Name', version: '1', selectedManifestDigest: 'manifest',
      permissions: [{ kind: 'network', required: true }],
      fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
      processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
      reversibility: { reversible: true, strategy: 'available' },
      policy: { outcome: 'allowed', reasonCount: 0 }, warnings: [],
      requiredConfirmations: [{ type: 'permission' }], defaultDisabled: false,
      recovery: { code: 'none', retryable: false },
    };
    expect(parseHttpsProvisionPlanReview(review)).toMatchObject({ planId: 'plan', operation: 'provision' });
    expect(parseHttpsProvisionPlanReview({ ...review, token: 'secret' })).toBeNull();
  });

  it.each([
    ['file boolean', { fileEffects: { writesFiles: 'false' } }],
    ['process boolean', { processEffects: { startsDuringConfirmation: 0 } }],
    ['ownedItems negative', { fileEffects: { ownedItems: -1 } }],
    ['ownedItems infinity', { fileEffects: { ownedItems: Infinity } }],
    ['reasonCount NaN', { policy: { reasonCount: Number.NaN } }],
    ['unknown confirmation enum', { requiredConfirmations: [{ type: 'admin' }] }],
    ['non-string warning', { warnings: ['ok', 1] }],
    ['non-string permission kind', { permissions: [{ kind: 1, required: true }] }],
  ])('rejects invalid HTTPS provision review field: %s', (_label, change) => {
    const review = {
      planId: 'plan', planDigest: 'digest', expiresAtMs: 1, trustTier: 'trusted', mcpId: 'mcp',
      name: 'Name', version: '1', selectedManifestDigest: 'manifest',
      permissions: [{ kind: 'network', required: true }],
      fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
      processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
      reversibility: { reversible: true, strategy: 'available' },
      policy: { outcome: 'allowed', reasonCount: 0 }, warnings: [],
      requiredConfirmations: [{ type: 'permission' }], defaultDisabled: false,
      recovery: { code: 'none', retryable: false },
    };
    const merged = { ...review, ...change };
    expect(parseHttpsProvisionPlanReview(merged)).toBeNull();
  });
  it('returns typed success values', () => {
    const outcome: McpPlatformOutcome<{ count: number }> = {
      status: 'success',
      value: { count: 3 },
    };

    expect(unwrapMcpOutcome(outcome)).toEqual({ count: 3 });
  });

  it('throws a recovery-safe service error for platform failures', () => {
    const outcome: McpPlatformOutcome<never> = {
      status: 'error',
      error: {
        code: 'credential_missing',
        message: 'A credential reference is required. Authorization: Basic dXNlcjpwYXNz',
        retryable: false,
        correlationId: 'correlation-1',
      },
    };

    expect(() => unwrapMcpOutcome(outcome)).toThrow(McpPlatformServiceError);
    try {
      unwrapMcpOutcome(outcome);
    } catch (error) {
      expect(error).toBeInstanceOf(McpPlatformServiceError);
      expect((error as McpPlatformServiceError).recovery).toEqual({
        title: 'This page only supports unauthenticated endpoints',
        message:
          'Manual Remote HTTP can only be added here when the endpoint does not require authentication.',
        nextStep:
          'Use an unauthenticated endpoint here. Endpoints that require credentials cannot be added from this page yet.',
        retryable: false,
        correlationId: 'correlation-1',
        code: 'credential_missing',
        kind: 'service',
      });
      expect((error as McpPlatformServiceError).message).toBe(
        'Manual Remote HTTP can only be added here when the endpoint does not require authentication.'
      );
      expect((error as McpPlatformServiceError).envelope.message).toBe(
        'Manual Remote HTTP can only be added here when the endpoint does not require authentication.'
      );
      expect(JSON.stringify((error as McpPlatformServiceError).recovery)).not.toContain(
        'bearer credential reference'
      );
    }
  });

  it('maps credential_missing to the current Manual Remote HTTP capability', () => {
    const recovery = mapMcpPlatformError({
      code: 'credential_missing',
      message: 'A credential reference is required.',
      retryable: false,
      correlationId: 'correlation-3',
    });

    expect(recovery).toEqual({
      title: 'This page only supports unauthenticated endpoints',
      message:
        'Manual Remote HTTP can only be added here when the endpoint does not require authentication.',
      nextStep:
        'Use an unauthenticated endpoint here. Endpoints that require credentials cannot be added from this page yet.',
      retryable: false,
      correlationId: 'correlation-3',
      code: 'credential_missing',
      kind: 'service',
    });
    expect(recovery.message).not.toBe('A credential reference is required.');
    expect(JSON.stringify(recovery)).not.toContain('bearer credential reference');
  });

  it('maps policy errors without exposing error details', () => {
    const recovery = mapMcpPlatformError({
      code: 'policy_denied',
      message: 'Denied by policy.',
      retryable: false,
      correlationId: 'correlation-2',
      details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
    });

    expect(recovery.nextStep).toContain('policy administrator');
    expect(recovery).not.toHaveProperty('details');
  });

  it('requires a fully validated envelope before degrading profiles as unavailable', () => {
    expect(isMcpPhaseUnavailableError(phaseUnavailableServiceError(), 'profiles')).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({ details: null }),
        'profiles'
      )
    ).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({
          details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
        }),
        'profiles'
      )
    ).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({
          details: {
            type: 'phase_unavailable',
            operation: 'goose.mcpProfileList_unstable',
            phase: '4B',
          },
        }),
        'modelSuggestions'
      )
    ).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({
          details: {
            type: 'phase_unavailable',
            operation: 'mcpProfileList_unstable',
            phase: '4B',
          },
        }),
        'profiles'
      )
    ).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({
          details: {
            type: 'phase_unavailable',
            operation: 'goose.mcpProfileList_unstable',
            phase: 'modelSuggestions',
          },
        }),
        'profiles'
      )
    ).toBe(false);
    expect(
      isMcpPhaseUnavailableError(
        phaseUnavailableServiceError({
          details: {
            type: 'phase_unavailable',
            operation: 'goose.mcpProfileList_unstable',
            phase: '4B',
          },
        }),
        'profiles'
      )
    ).toBe(true);
  });

  it('rejects unsupported phase-unavailable codes even when the pair looks valid', () => {
    const modelSuggestionUnavailable = phaseUnavailableServiceError({
      code: 'operation_not_supported',
      details: {
        type: 'phase_unavailable',
        operation: 'goose.mcpProfileModelRecommend_unstable',
        phase: '4B',
      },
    });

    expect(isMcpPhaseUnavailableError(modelSuggestionUnavailable, 'profiles')).toBe(false);
    expect(isMcpPhaseUnavailableError(modelSuggestionUnavailable, 'modelSuggestions')).toBe(false);
  });

  it('sanitizes service envelope messages while preserving non-sensitive error meaning', () => {
    const recovery = mapMcpPlatformError({
      code: 'invalid_request',
      message:
        'Request rejected after validation. Authorization: Basic dXNlcjpwYXNz ' +
        'Proxy-Authorization: Negotiate super-secret tail-token status=401 ' +
        '{"Authorization":"Bearer second-secret","X-Api-Key":"abc123"} env={"API_KEY":"sk-live"} ' +
        'argv=["C:\\\\secret\\\\tool.exe","--token=abc"] command="/bin/sh -lc curl ws://private.example.test/socket" ' +
        'path=/tmp /Users/example/.config/tool.json Failure while reading /tmp, then (wss://private.example.test/stream).',
      retryable: true,
      correlationId: 'correlation-4',
    });

    expect(recovery.message).toContain('Request rejected after validation.');
    expect(recovery.message).not.toContain('dXNlcjpwYXNz');
    expect(recovery.message).not.toContain('super-secret');
    expect(recovery.message).not.toContain('second-secret');
    expect(recovery.message).not.toContain('abc123');
    expect(recovery.message).not.toContain('sk-live');
    expect(recovery.message).not.toContain('private.example.test');
    expect(recovery.message).not.toContain('path=/tmp');
    expect(recovery.message).not.toContain('C:\\secret\\tool.exe');
    expect(recovery.message).not.toContain('/Users/example/.config/tool.json');
    expect(recovery.message).toContain('Authorization: Basic [redacted]');
    expect(recovery.message).toContain('Proxy-Authorization: Negotiate [redacted]');
    expect(recovery.message).not.toContain('status=401');
    expect(recovery.message).toContain('{"Authorization":"Bearer [redacted]","X-Api-Key":"[redacted]"}');
    expect(recovery.message).toContain('[environment removed]');
    expect(recovery.message).toContain('[argv removed]');
    expect(recovery.message).toContain('[command removed]');
    expect(recovery.message).toContain('path=[path removed]');
    expect(recovery.message).toContain('Failure while reading [path removed], then ([address removed]).');
  });

  it('falls back to the generic failure message when only sensitive fragments remain', () => {
    const recovery = mapMcpPlatformError({
      code: 'invalid_request',
      message:
        'Authorization: Bearer super-secret https://private.example.test ' +
        'command=/bin/sh argv=[\"C:\\\\secret\\\\tool.exe\"] env={\"TOKEN\":\"abc\"}',
      retryable: true,
      correlationId: 'correlation-5',
    });

    expect(recovery.message).toBe('The MCP Platform request could not be completed.');
  });

  it('stores only sanitized envelope messages on service errors', () => {
    const outcome: McpPlatformOutcome<never> = {
      status: 'error',
      error: {
        code: 'invalid_request',
        message:
          'Authorization: Basic dXNlcjpwYXNz command=/bin/sh env={"TOKEN":"abc"} https://private.example.test',
        retryable: true,
        correlationId: 'correlation-6',
      },
    };

    try {
      unwrapMcpOutcome(outcome);
    } catch (error) {
      const serviceError = error as McpPlatformServiceError;
      expect(serviceError.message).toBe('The MCP Platform request could not be completed.');
      expect(serviceError.envelope.message).toBe('The MCP Platform request could not be completed.');
      expect(JSON.stringify(serviceError)).not.toContain('dXNlcjpwYXNz');
      expect(JSON.stringify(serviceError)).not.toContain('private.example.test');
    }
  });

  it('does not expose transport error payloads when only sensitive fragments remain', () => {
    const recovery = toMcpRecoveryViewModel(
      new Error('Authorization: Bearer secret at https://private.example.test')
    );

    expect(recovery.message).toBe('The MCP Platform request could not be completed.');
    expect(recovery.kind).toBe('connection');
    expect(JSON.stringify(recovery)).not.toContain('secret');
    expect(JSON.stringify(recovery)).not.toContain('private.example.test');
  });

  it('redacts secrets, URLs, and local paths from task-facing messages', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization: Basic dXNlcjpwYXNz {"Authorization":"Bearer second-secret","X-Api-Key":"abc123"} ' +
          'env={"TOKEN":"abc"} argv=["C:\\\\sensitive\\\\tool.exe"] command="/bin/tool --token=abc" ' +
          'https://private.example.test path=/tmp /Users/example/.config/tool.json'
      )
    ).toBe(
      'Authorization: Basic [redacted] {"Authorization":"Bearer [redacted]","X-Api-Key":"[redacted]"} [environment removed] [argv removed] [command removed] [address removed] path=[path removed] [path removed]'
    );
  });

  it('redacts Authorization and Proxy-Authorization for any scheme without crossing line boundaries', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization: Bearer top-secret nextStep=Retry\n' +
          'Proxy-Authorization: Negotiate super-secret tail-token\n' +
          '{"Proxy-Authorization":"Custom secret token chain","status":"failed"}\n' +
          'status: failed'
      )
    ).toBe(
      'Authorization: Bearer [redacted]\n' +
        'Proxy-Authorization: Negotiate [redacted]\n' +
        '{"Proxy-Authorization":"Custom [redacted]","status":"failed"}\n' +
        'status: failed'
    );
  });

  it('does not let prefix-like fields skip later authorization headers', () => {
    expect(
      sanitizeMcpUserMessage(
        'prefix Authorization: Bearer prefix-secret status=401 ' +
          'label: Proxy-Authorization: Negotiate token=opaque nonce=qwerty correlationId=req-7'
      )
    ).toBe(
      'prefix Authorization: Bearer [redacted] Proxy-Authorization: Negotiate [redacted]'
    );
  });

  it('treats plain-text authorization metadata as untrusted until the next line', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization: Digest user=alice response=abc123 nonce=qwerty status=401 code=mcp_x\n' +
          'Proxy-Authorization: Bearer token=opaque nextStep=Retry correlationId=req-7\n' +
          'Proxy-Authorization: Negotiate user=alice response=abc123 nonce=qwerty\n' +
          'status=401\n' +
          'code=mcp_x'
      )
    ).toBe(
      'Authorization: Digest [redacted]\n' +
        'Proxy-Authorization: Bearer [redacted]\n' +
        'Proxy-Authorization: Negotiate [redacted]\n' +
        'status=401\n' +
        'code=mcp_x'
    );
  });

  it('redacts unicode presentation variants for authorization headers', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization：Bearer fullwidth-secret\r\n' +
          'Proxy-Authorization＝Negotiate fullwidth-secret\r\n' +
          'status=401'
      )
    ).toBe(
      'Authorization： Bearer [redacted]\n' +
        'Proxy-Authorization＝Negotiate [redacted]\n' +
        'status=401'
    );
  });

  it('redacts greek and cyrillic authorization confusables in json-like payloads', () => {
    expect(
      sanitizeMcpUserMessage(
        '{"Αuthorization":"Bearer greek-alpha-secret","status":"401"} ' +
          '{"Аuthorization":"Bearer cyrillic-alpha-secret","status":"402"}'
      )
    ).toBe(
      '{"Authorization":"Bearer [redacted]","status":"401"} ' +
        '{"Authorization":"Bearer [redacted]","status":"402"}'
    );
  });

  it('redacts unicode-neighbor bypasses for auth headers inside structured payloads', () => {
    expect(
      sanitizeMcpUserMessage(
        '{"Authοrization":"Bearer bypass-omicron","status":"401"}\n' +
          '{"Prοxy-Authorization":"Negotiate bypass-proxy-omicron","status":"402"}\n' +
          '{"A\u200Buthorization":"Bearer bypass-zwsp","status":"403"}\n' +
          '{"A\u202Euthorization":"Bearer bypass-bidi","status":"404"}\n' +
          '{"A\u0301uthorization":"Bearer bypass-combining","status":"405"}'
      )
    ).toBe(
      '{"Authorization":"Bearer [redacted]","status":"401"}\n' +
        '{"Proxy-Authorization":"Negotiate [redacted]","status":"402"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"403"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"404"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"405"}'
    );
  });

  it('redacts control, format, and combining-character bypasses inside credential keys', () => {
    expect(
      sanitizeMcpUserMessage(
        '{"A\\tuthorization":"Bearer tab-secret","status":"400"}\n'.replace(
          '\\t',
          '\t'
        ) +
          '{"A\\u000Buthorization":"Bearer vt-secret","status":"401"}\n'.replace(
            '\\u000B',
            '\u000B'
          ) +
          '{"A\\u0000uthorization":"Bearer nul-secret","status":"402"}\n'.replace(
            '\\u0000',
            '\u0000'
          ) +
          '{"A\u200Buthorization":"Bearer zwsp-secret","status":"403"}\n' +
          '{"A\u202Euthorization":"Bearer bidi-secret","status":"404"}\n' +
          '{"A\u0301uthorization":"Bearer combining-secret","status":"405"}'
      )
    ).toBe(
      '{"Authorization":"Bearer [redacted]","status":"400"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"401"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"402"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"403"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"404"}\n' +
        '{"Authorization":"Bearer [redacted]","status":"405"}'
    );
  });

  it('redacts internal greek and cyrillic homographs across Authorization and Proxy-Authorization', () => {
    expect(
      sanitizeMcpUserMessage(
        '{"Authοrizatiοn":"Bearer greek-omicron-secret","status":"401"} ' +
          '{"Prоxy-Authοrization":"Custom cyrillic-omicron-secret chain","status":"402"}'
      )
    ).toBe(
      '{"Authorization":"Bearer [redacted]","status":"401"} ' +
        '{"Proxy-Authorization":"Custom [redacted]","status":"402"}'
    );
  });

  it('keeps non-sensitive unicode fields while redacting nearby credential fields', () => {
    expect(
      sanitizeMcpUserMessage(
        '{"Αudit":"visible","status":"401","Authorization":"Bearer ascii-secret"}'
      )
    ).toBe('{"Αudit":"visible","status":"401","Authorization":"Bearer [redacted]"}');
  });

  it('preserves safe status lines while removing secrets across multiple unicode header lines', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization：Bearer top-secret\r\n' +
          'Proxy-Authorization: Negotiate next-secret\r\n' +
          'status: failed\r\n' +
          'nextStep=Retry'
      )
    ).toBe(
      'Authorization： Bearer [redacted]\n' +
        'Proxy-Authorization: Negotiate [redacted]\n' +
        'status: failed\n' +
        'nextStep=Retry'
    );
  });

  it('supports corner and small-form quotes for structured credential values', () => {
    expect(
      sanitizeMcpUserMessage(
        '「Authorization」：「Bearer corner-secret」,「status」：「401」\n' +
          '﹁Proxy-Authorization﹂＝﹁Negotiate small-form-secret tail-token﹂,status=failed'
      )
    ).toBe(
      '「Authorization」：「Bearer [redacted]」,「status」：「401」\n' +
        '﹁Proxy-Authorization﹂＝﹁Negotiate [redacted]﹂,status=failed'
    );
  });

  it('keeps JSON and plain-text authorization boundaries separate across same-line headers and CRLF', () => {
    expect(
      sanitizeMcpUserMessage(
        'Authorization: Bearer line-secret Proxy-Authorization: Digest user=alice response=abc123 nonce=qwerty status=401\r\n' +
          '{"Authorization":"Bearer json-secret","status":"402"}\r\n' +
          'Proxy-Authorization: Negotiate token=third-secret\r\n' +
          'status=failed'
      )
    ).toBe(
      'Authorization: Bearer [redacted] Proxy-Authorization: Digest [redacted]\n' +
        '{"Authorization":"Bearer [redacted]","status":"402"}\n' +
        'Proxy-Authorization: Negotiate [redacted]\n' +
        'status=failed'
    );
  });

  it('sanitizes unicode credential variants for mapped and stored envelope messages', () => {
    const serviceRecovery = mapMcpPlatformError({
      code: 'invalid_request',
      message:
        'Task monitor rejected the response. Authorization：Bearer fullwidth-secret ' +
        '{"Authοrization":"Bearer greek-omicron-secret","status":"401"} ' +
        '{"A\u200Buthorization":"Bearer zwsp-secret","status":"402"} ' +
        '﹁Proxy-Authorization﹂＝﹁Negotiate bidi-secret tail-token﹂',
      retryable: true,
      correlationId: 'unicode-correlation',
    });

    expect(serviceRecovery.message).toBe(
      'Task monitor rejected the response. Authorization： Bearer [redacted] ' +
        '{"Authorization":"Bearer [redacted]","status":"401"} ' +
        '{"Authorization":"Bearer [redacted]","status":"402"} ' +
        '﹁Proxy-Authorization﹂＝﹁Negotiate [redacted]﹂'
    );
    expect(serviceRecovery.message).not.toContain('fullwidth-secret');
    expect(serviceRecovery.message).not.toContain('greek-omicron-secret');
    expect(serviceRecovery.message).not.toContain('zwsp-secret');
    expect(serviceRecovery.message).not.toContain('bidi-secret');

    const outcome: McpPlatformOutcome<never> = {
      status: 'error',
      error: {
        code: 'invalid_request',
        message:
          'Authorization：Bearer fullwidth-secret {"A\u0301uthorization":"Bearer combining-secret","status":"401"}',
        retryable: true,
        correlationId: 'unicode-envelope',
      },
    };

    try {
      unwrapMcpOutcome(outcome);
    } catch (error) {
      const serviceError = error as McpPlatformServiceError;
      expect(serviceError.envelope.message).toBe(
        'Authorization： Bearer [redacted] {"Authorization":"Bearer [redacted]","status":"401"}'
      );
      expect(serviceError.envelope.message).not.toContain('fullwidth-secret');
      expect(serviceError.envelope.message).not.toContain('combining-secret');
    }
  });

  it('truncates prefix-dense structured input safely without echoing secrets', () => {
    const denseInput = Array.from({ length: 1200 }, (_value, index) => {
      return `prefix-${index}: Authorization: Bearer secret-${index} status=401 nextStep=Retry correlationId=req-${index}`;
    }).join('\n');

    const startedAt = performance.now();
    const sanitized = sanitizeMcpUserMessage(denseInput);
    const elapsedMs = performance.now() - startedAt;

    expect(sanitized).toContain(
      'prefix-0: Authorization: Bearer [redacted]'
    );
    expect(sanitized).toContain('[message truncated]');
    expect(sanitized).not.toContain('secret-0');
    expect(sanitized).not.toContain('secret-1199');
    expect(sanitized).not.toContain('status=401');
    expect(sanitized).not.toContain('nextStep=Retry');
    expect(sanitized).not.toContain('correlationId=req-0');
    expect(elapsedMs).toBeLessThan(1500);
  });

  it('truncates at Unicode-safe boundaries without leaking secrets around 32 KiB', () => {
    const limit = 32 * 1024;
    const beforeBoundary = sanitizeMcpUserMessage(
      'Authorization: Bearer before-secret status=401 ' + 'A'.repeat(limit) + ' tail-secret'
    );
    const crossingBoundary = sanitizeMcpUserMessage(
      'A'.repeat(limit - ' Authorization: Bearer '.length - 4) +
        ' Authorization: Bearer mid-secret status=401'
    );
    const boundaryCases = [
      sanitizeMcpUserMessage('A'.repeat(limit - 1) + '💥Authorization: Bearer emoji-secret'),
      sanitizeMcpUserMessage('A'.repeat(limit - 1) + 'e\u0301Authorization: Bearer combining-secret'),
      sanitizeMcpUserMessage('A'.repeat(limit - 1) + '✈\uFE0FAuthorization: Bearer variation-secret'),
      sanitizeMcpUserMessage('A'.repeat(limit - 1) + '\u200BAuthorization: Bearer format-secret'),
    ];

    expect(beforeBoundary).toContain('Authorization: Bearer [redacted]');
    expect(beforeBoundary).toContain('[message truncated]');
    expect(beforeBoundary).not.toContain('before-secret');
    expect(crossingBoundary).toContain(' Authorization: Bearer [redacted]');
    expect(crossingBoundary).toContain('[message truncated]');
    expect(crossingBoundary).not.toContain('mid-secret');

    for (const sanitized of boundaryCases) {
      expect(sanitized).toContain('[message truncated]');
      expect(sanitized).not.toContain('emoji-secret');
      expect(sanitized).not.toContain('combining-secret');
      expect(sanitized).not.toContain('variation-secret');
      expect(sanitized).not.toContain('format-secret');
      expect(hasIsolatedSurrogate(sanitized ?? '')).toBe(false);
      expect(sanitized).not.toMatch(/[\u0300-\u036f\u200b\ufe00-\ufe0f]\n\[message truncated\]$/u);
    }
  });

  it('redacts bare unix paths and scheme URLs while preserving punctuation', () => {
    expect(
      sanitizeMcpUserMessage(
        'Failure while reading /tmp, retry with (/tmp/cache). path=/tmp ' +
          'ws://private.example.test/socket wss://private.example.test/stream https://private.example.test/api ' +
          'C:\\\\sensitive\\\\tool.exe'
      )
    ).toBe(
      'Failure while reading [path removed], retry with ([path removed]). path=[path removed] ' +
        '[address removed] [address removed] [address removed] [path removed]'
    );
  });

  it('does not treat Windows drive paths as URLs', () => {
    const sanitized = sanitizeMcpUserMessage(
      'Open C:\\\\sensitive\\\\tool.exe before ws://private.example.test/socket'
    );

    expect(sanitized).toContain('[path removed]');
    expect(sanitized).toContain('[address removed]');
    expect(sanitized).not.toContain('private.example.test');
    expect(sanitized).not.toContain('C:\\sensitive\\tool.exe');
  });
});
