export type DistributionMode = 'github' | 'portable';

export interface DistributionInfo {
  mode: DistributionMode;
}

export const PRIMARY_GITHUB_OWNER = 'HikerM';
export const PRIMARY_GITHUB_REPO = 'goose-vfx-mcp';
export const PRIMARY_REPOSITORY_URL = `https://github.com/${PRIMARY_GITHUB_OWNER}/${PRIMARY_GITHUB_REPO}`;
export const PRIMARY_RELEASES_URL = `${PRIMARY_REPOSITORY_URL}/releases`;
export const PRIMARY_ISSUES_URL = `${PRIMARY_REPOSITORY_URL}/issues`;

export const DEFAULT_DISTRIBUTION: DistributionInfo = { mode: 'portable' };

export function parseDistributionInfo(value: unknown): DistributionInfo {
  if (typeof value !== 'object' || value === null) {
    return DEFAULT_DISTRIBUTION;
  }

  return (value as { mode?: unknown }).mode === 'github'
    ? { mode: 'github' }
    : DEFAULT_DISTRIBUTION;
}
