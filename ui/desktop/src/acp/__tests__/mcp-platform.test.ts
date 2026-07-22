import { describe, expect, it } from 'vitest';
import type { McpPlatformOutcome } from '@aaif/goose-sdk';
import {
  McpPlatformServiceError,
  mapMcpPlatformError,
  toMcpRecoveryViewModel,
  unwrapMcpOutcome,
} from '../mcp-platform';

describe('MCP Platform outcome adapter', () => {
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
        message: 'A credential reference is required.',
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
        title: 'Credential reference required',
        message: 'A credential reference is required.',
        nextStep: 'Choose an existing bearer credential reference, then create a new plan.',
        retryable: false,
        correlationId: 'correlation-1',
        code: 'credential_missing',
        kind: 'service',
      });
    }
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

  it('does not expose transport error payloads', () => {
    const recovery = toMcpRecoveryViewModel(
      new Error('Authorization: Bearer secret at https://private.example.test')
    );

    expect(recovery.message).toBe('The MCP Platform request could not be completed.');
    expect(recovery.kind).toBe('connection');
    expect(JSON.stringify(recovery)).not.toContain('secret');
    expect(JSON.stringify(recovery)).not.toContain('private.example.test');
  });
});
