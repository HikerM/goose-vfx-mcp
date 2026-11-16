import type { LuminaClient } from '@hikerm/lumina-sdk';

export type MediaProjectAccessPurpose = 'preview' | 'save_as' | 'open' | 'reveal' | 'copy_path';

export interface MediaProjectAccessRequest {
  sessionId: string;
  assetId: string;
  purpose: MediaProjectAccessPurpose;
}

export interface MediaProjectAccessResponse {
  ticket: string;
  expiresInMs: number;
  sourceSessionId: string;
  resultId: string;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function parseMediaProjectAccessResponse(value: unknown): MediaProjectAccessResponse {
  const expiresInMs = record(value) ? value.expiresInMs : undefined;
  if (
    !record(value) ||
    typeof value.ticket !== 'string' ||
    value.ticket.length === 0 ||
    !Number.isSafeInteger(expiresInMs) ||
    (expiresInMs as number) < 0 ||
    typeof value.sourceSessionId !== 'string' ||
    value.sourceSessionId.length === 0 ||
    typeof value.resultId !== 'string' ||
    value.resultId.length === 0
  ) {
    throw new Error('Invalid project media access response.');
  }
  return {
    ticket: value.ticket,
    expiresInMs: expiresInMs as number,
    sourceSessionId: value.sourceSessionId,
    resultId: value.resultId,
  };
}

export async function mediaProjectAccess(
  client: LuminaClient,
  request: MediaProjectAccessRequest
): Promise<MediaProjectAccessResponse> {
  const response = await client.extMethod('_lumina/unstable/media/project_access', {
    ...request,
  });
  return parseMediaProjectAccessResponse(response);
}
