import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DISTRIBUTION,
  parseDistributionInfo,
  PRIMARY_GITHUB_OWNER,
  PRIMARY_GITHUB_REPO,
  PRIMARY_RELEASES_URL,
} from './distribution-config';

describe('distribution config', () => {
  it('fails closed to portable mode when the config is absent or invalid', () => {
    expect(parseDistributionInfo(undefined)).toEqual(DEFAULT_DISTRIBUTION);
    expect(parseDistributionInfo({})).toEqual(DEFAULT_DISTRIBUTION);
    expect(parseDistributionInfo({ mode: 'official' })).toEqual(DEFAULT_DISTRIBUTION);
  });

  it('enables GitHub updates only for the explicit HikerM mode', () => {
    expect(parseDistributionInfo({ mode: 'github' })).toEqual({ mode: 'github' });
    expect(PRIMARY_GITHUB_OWNER).toBe('HikerM');
    expect(PRIMARY_GITHUB_REPO).toBe('goose-vfx-mcp');
    expect(PRIMARY_RELEASES_URL).toBe('https://github.com/HikerM/goose-vfx-mcp/releases');
  });
});
