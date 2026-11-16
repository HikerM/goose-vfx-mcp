import { describe, expect, it } from 'vitest';
import { readProjectMediaAsset } from './projectMediaAsset';

const resultId = '12345678-1234-4234-8234-123456789abc';
const result = {
  schemaVersion: 1,
  sessionId: 'source-session',
  resultId,
  kind: 'image',
  fileName: 'frame.png',
  mime: 'image/png',
  bytes: 128,
  sha256: '1'.repeat(64),
  spec: { kind: 'image', width: 32, height: 32 },
};

describe('project media asset projection', () => {
  it('binds an archived result to the current project session for access', () => {
    expect(
      readProjectMediaAsset(
        JSON.stringify({
          status: 'found',
          projection: true,
          asset: {
            assetId: resultId,
            sourceSessionId: 'source-session',
            kind: 'media',
            output: result,
          },
        }),
        'current-session'
      )
    ).toEqual({
      identity: { sessionId: 'source-session', resultId },
      access: { sessionId: 'current-session', assetId: resultId },
      result,
    });
  });

  it.each([
    'plain text',
    JSON.stringify({ status: 'not_found' }),
    JSON.stringify({ status: 'found', projection: true, asset: { kind: 'transcript' } }),
    JSON.stringify({
      status: 'found',
      projection: true,
      asset: {
        assetId: resultId,
        sourceSessionId: 'source-session',
        kind: 'media',
        output: { ...result, resultId: 'other' },
      },
    }),
  ])('ignores non-media or inconsistent projections', (text) => {
    expect(readProjectMediaAsset(text, 'current-session')).toBeNull();
  });
});
