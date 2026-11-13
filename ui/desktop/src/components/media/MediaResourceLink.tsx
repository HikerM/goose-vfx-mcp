import type { ContentBlock } from '../../types/message';
import { useIntl } from '../../i18n';
import ConnectedMediaResultCard from './ConnectedMediaResultCard';
import type { MediaResultIdentity } from './types';

type ResourceLink = Extract<ContentBlock, { type: 'resource_link' }>;

export function mediaResourceIdentity(
  resource: ResourceLink,
  sessionId: string | undefined
): MediaResultIdentity | null {
  const reference = resource._meta?.['lumina.mediaResult'];
  if (!sessionId || !reference || typeof reference !== 'object' || Array.isArray(reference)) {
    return null;
  }
  const record = reference as Record<string, unknown>;
  if (
    record.schemaVersion !== 1 ||
    record.sessionId !== sessionId ||
    typeof record.resultId !== 'string' ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(record.resultId) ||
    resource.uri !== `lumina-media://result/${record.resultId}`
  ) {
    return null;
  }
  return { sessionId, resultId: record.resultId };
}

export default function MediaResourceLink({
  resource,
  sessionId,
  isActive = true,
}: {
  resource: ResourceLink;
  sessionId?: string;
  isActive?: boolean;
}) {
  const intl = useIntl();
  const identity = mediaResourceIdentity(resource, sessionId);
  if (identity) {
    return <ConnectedMediaResultCard {...identity} isActive={isActive} />;
  }
  return (
    <div role="group" aria-label={resource.name} className="flex min-w-0 flex-col gap-1">
      <p className="font-sans text-sm font-medium break-all">{resource.name}</p>
      {resource.title?.trim() && resource.title.trim() !== resource.name.trim() && (
        <p className="text-sm break-words">{resource.title}</p>
      )}
      {resource.description?.trim() && (
        <p className="text-sm whitespace-pre-wrap break-words">{resource.description}</p>
      )}
      <div className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-text-secondary">
        {resource.mimeType?.trim() && <span className="break-all">{resource.mimeType}</span>}
        {resource.size !== undefined &&
          Number.isSafeInteger(resource.size) &&
          resource.size >= 0 && <span>{intl.formatNumber(resource.size)} B</span>}
      </div>
      <p className="font-mono text-xs text-text-secondary break-all">{resource.uri}</p>
    </div>
  );
}
