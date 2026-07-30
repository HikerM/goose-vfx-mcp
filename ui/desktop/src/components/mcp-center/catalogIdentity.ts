import type { McpCatalogDetail, McpCatalogPlanTarget, McpCatalogSummary, McpPlanReview } from '@aaif/goose-sdk';

export function catalogPlanTargetFromItem(
  item: Pick<McpCatalogSummary, 'sourceId' | 'mcpId' | 'version' | 'manifestDigest'>
): McpCatalogPlanTarget {
  return {
    sourceId: item.sourceId,
    mcpId: item.mcpId,
    version: item.version,
    manifestDigest: item.manifestDigest,
  };
}

export function catalogTargetKey(item: McpCatalogPlanTarget): string {
  return JSON.stringify([item.sourceId, item.mcpId, item.version, item.manifestDigest]);
}

export function catalogDetailMatchesTarget(
  target: McpCatalogPlanTarget,
  detail: McpCatalogDetail
): boolean {
  return (
    target.sourceId === detail.sourceId &&
    target.mcpId === detail.mcpId &&
    target.version === detail.version &&
    target.manifestDigest === detail.manifestDigest
  );
}

export function readCatalogTarget(review: McpPlanReview): McpCatalogPlanTarget | null {
  const record = review as unknown as Record<string, unknown>;
  const value = record.catalogTarget;
  return isCatalogPlanTarget(value) ? value : null;
}

export function catalogReviewMatchesTarget(
  target: McpCatalogPlanTarget,
  review: McpPlanReview
): boolean {
  const catalogTarget = readCatalogTarget(review);
  return (
    !!catalogTarget &&
    catalogTarget.sourceId === target.sourceId &&
    catalogTarget.mcpId === target.mcpId &&
    catalogTarget.version === target.version &&
    catalogTarget.manifestDigest === target.manifestDigest
  );
}

function isCatalogPlanTarget(value: unknown): value is McpCatalogPlanTarget {
  if (!value || typeof value !== 'object') {
    return false;
  }

  const record = value as Record<string, unknown>;
  return (
    typeof record.sourceId === 'string' &&
    typeof record.mcpId === 'string' &&
    typeof record.version === 'string' &&
    typeof record.manifestDigest === 'string'
  );
}
