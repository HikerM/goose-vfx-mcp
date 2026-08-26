import type { InitializeResponse } from '@agentclientprotocol/sdk';
import { describe, expect, it } from 'vitest';
import { hasLocalInferenceCapability } from '../capabilities';

function initializeResponseWithMeta(meta?: unknown): Pick<InitializeResponse, 'agentCapabilities'> {
  return {
    agentCapabilities: {
      _meta: meta,
    },
  } as Pick<InitializeResponse, 'agentCapabilities'>;
}

describe('ACP capabilities', () => {
  it('detects local inference support from Lumina metadata', () => {
    expect(
      hasLocalInferenceCapability(
        initializeResponseWithMeta({
          lumina: {
            localInference: {},
          },
        })
      )
    ).toBe(true);
  });

  it('treats missing local inference metadata as unsupported', () => {
    expect(hasLocalInferenceCapability(initializeResponseWithMeta())).toBe(false);
    expect(hasLocalInferenceCapability(initializeResponseWithMeta({}))).toBe(false);
    expect(hasLocalInferenceCapability(initializeResponseWithMeta({ lumina: {} }))).toBe(false);
  });

  it('ignores malformed Lumina metadata', () => {
    expect(hasLocalInferenceCapability(initializeResponseWithMeta({ lumina: true }))).toBe(false);
    expect(hasLocalInferenceCapability(initializeResponseWithMeta({ lumina: null }))).toBe(false);
  });
});
