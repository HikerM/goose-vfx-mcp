import { describe, expect, it, vi } from 'vitest';
import {
  mediaProjectAccess,
  parseMediaProjectAccessResponse,
} from './media';

const response = {
  ticket: 'ticket-1',
  expiresInMs: 300000,
  sourceSessionId: 'source-session',
  resultId: '12345678-1234-4234-8234-123456789abc',
};

describe('project media access', () => {
  it('validates and preserves the source media identity', () => {
    expect(parseMediaProjectAccessResponse(response)).toEqual(response);
  });

  it.each([
    undefined,
    { ...response, ticket: '' },
    { ...response, expiresInMs: -1 },
    { ...response, expiresInMs: 1.5 },
    { ...response, sourceSessionId: '' },
    { ...response, resultId: '' },
  ])('rejects malformed responses', (value) => {
    expect(() => parseMediaProjectAccessResponse(value)).toThrow(
      'Invalid project media access response.'
    );
  });

  it('uses the raw ACP extension request without depending on generated SDK methods', async () => {
    const extMethod = vi.fn(async () => response);
    const client = { extMethod } as never;
    const request = {
      sessionId: 'current-session',
      assetId: response.resultId,
      purpose: 'preview' as const,
    };
    await expect(mediaProjectAccess(client, request)).resolves.toEqual(response);
    expect(extMethod).toHaveBeenCalledWith('_lumina/unstable/media/project_access', request);
  });
});
