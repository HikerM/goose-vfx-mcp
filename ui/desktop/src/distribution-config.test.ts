import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DISTRIBUTION,
  parseDistributionInfo,
  PRIMARY_GITHUB_OWNER,
  PRIMARY_GITHUB_REPO,
  PRIMARY_RELEASES_URL,
  RELEASE_CHANNEL_CONFIGURED,
} from './distribution-config';

describe('distribution config', () => {
  it('fails closed to portable mode when the config is absent or invalid', () => {
    expect(parseDistributionInfo(undefined)).toEqual(DEFAULT_DISTRIBUTION);
    expect(parseDistributionInfo({})).toEqual(DEFAULT_DISTRIBUTION);
    expect(parseDistributionInfo({ mode: 'official' })).toEqual(DEFAULT_DISTRIBUTION);
  });

  it('enables GitHub updates only when an owned release channel is configured', () => {
    expect(parseDistributionInfo({ mode: 'github' }, false)).toEqual(DEFAULT_DISTRIBUTION);
    expect(parseDistributionInfo({ mode: 'github' }, true)).toEqual({ mode: 'github' });
    expect(RELEASE_CHANNEL_CONFIGURED).toBe(Boolean(PRIMARY_GITHUB_OWNER && PRIMARY_GITHUB_REPO));
    expect(PRIMARY_RELEASES_URL).toBe(
      RELEASE_CHANNEL_CONFIGURED
        ? `https://github.com/${PRIMARY_GITHUB_OWNER}/${PRIMARY_GITHUB_REPO}/releases`
        : undefined
    );
  });
});
