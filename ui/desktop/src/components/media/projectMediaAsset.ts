import type { MediaResultIdentity } from './types';

export interface ProjectMediaAssetReference {
  identity: MediaResultIdentity;
  access: {
    sessionId: string;
    assetId: string;
  };
  result: Record<string, unknown>;
}

const uuidPattern =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Project assets are returned by the native `project_asset` tool as JSON text.
 * Keep parsing at the renderer boundary so ordinary text output remains unchanged.
 */
export function readProjectMediaAsset(
  text: string,
  accessSessionId: string
): ProjectMediaAssetReference | null {
  if (!accessSessionId) return null;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!record(value) || value.status !== 'found' || value.projection !== true) return null;
  const asset = value.asset;
  if (!record(asset) || asset.kind !== 'media') return null;
  const assetId = asset.assetId;
  const sourceSessionId = asset.sourceSessionId;
  const result = asset.output;
  if (
    typeof assetId !== 'string' ||
    !uuidPattern.test(assetId) ||
    typeof sourceSessionId !== 'string' ||
    sourceSessionId.length === 0 ||
    !record(result) ||
    result.schemaVersion !== 1 ||
    result.resultId !== assetId ||
    result.sessionId !== sourceSessionId
  ) {
    return null;
  }
  return {
    identity: { sessionId: sourceSessionId, resultId: assetId },
    access: { sessionId: accessSessionId, assetId },
    result,
  };
}
