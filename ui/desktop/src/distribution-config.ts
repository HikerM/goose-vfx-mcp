export type DistributionMode = 'github' | 'portable';

export interface DistributionInfo {
  mode: DistributionMode;
}

export const PRIMARY_GITHUB_OWNER = process.env.LUMINA_RELEASE_OWNER?.trim() || '';
export const PRIMARY_GITHUB_REPO = process.env.LUMINA_RELEASE_REPO?.trim() || '';
const configuredHomepage = process.env.LUMINA_HOMEPAGE?.trim() || '';
export const PRIMARY_HOMEPAGE_URL = configuredHomepage
  ? configuredHomepage.replace(/\/$/, '')
  : undefined;
export const PRIMARY_DOCS_URL = PRIMARY_HOMEPAGE_URL ? `${PRIMARY_HOMEPAGE_URL}/docs` : undefined;
export const PRIMARY_EXTENSIONS_URL = PRIMARY_HOMEPAGE_URL
  ? `${PRIMARY_HOMEPAGE_URL}/v1/extensions/`
  : undefined;
export const PRIMARY_QUICKSTART_URL = PRIMARY_DOCS_URL
  ? `${PRIMARY_DOCS_URL}/quickstart`
  : undefined;
export const RELEASE_CHANNEL_CONFIGURED = Boolean(PRIMARY_GITHUB_OWNER && PRIMARY_GITHUB_REPO);
export const PRIMARY_REPOSITORY_URL = RELEASE_CHANNEL_CONFIGURED
  ? `https://github.com/${PRIMARY_GITHUB_OWNER}/${PRIMARY_GITHUB_REPO}`
  : undefined;
export const PRIMARY_RELEASES_URL = PRIMARY_REPOSITORY_URL
  ? `${PRIMARY_REPOSITORY_URL}/releases`
  : undefined;
export const PRIMARY_ISSUES_URL = PRIMARY_REPOSITORY_URL
  ? `${PRIMARY_REPOSITORY_URL}/issues`
  : undefined;

export const DEFAULT_DISTRIBUTION: DistributionInfo = { mode: 'portable' };

export function parseDistributionInfo(
  value: unknown,
  releaseChannelConfigured = RELEASE_CHANNEL_CONFIGURED
): DistributionInfo {
  if (typeof value !== 'object' || value === null) {
    return DEFAULT_DISTRIBUTION;
  }

  return releaseChannelConfigured && (value as { mode?: unknown }).mode === 'github'
    ? { mode: 'github' }
    : DEFAULT_DISTRIBUTION;
}
