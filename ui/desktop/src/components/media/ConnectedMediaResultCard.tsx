import { useEffect, useState } from 'react';
import { getAcpClient } from '../../acp/acpConnection';
import { defineMessages, useIntl } from '../../i18n';
import { Button } from '../ui/button';
import MediaResultCard from './MediaResultCard';
import { readMediaPresentation } from './mediaResultPresentation';
import type { MediaResultActions, MediaResultIdentity } from './types';
import type {
  DesktopMediaGrant,
  DesktopMediaHandle,
  DesktopMediaPurpose,
} from '../../types/desktopMedia';

const messages = defineMessages({
  readError: { id: 'mediaResult.readError', defaultMessage: 'Unable to load this media result.' },
  notFound: {
    id: 'mediaResult.notFound',
    defaultMessage: 'This media result is no longer registered.',
  },
  retry: { id: 'mediaResult.retry', defaultMessage: 'Retry' },
});
type Props = MediaResultIdentity & { isActive: boolean };
type LoadedResult = NonNullable<ReturnType<typeof readMediaPresentation>>;

function ConnectedMediaResultContent({ sessionId, resultId, isActive }: Props) {
  const intl = useIntl();
  const [attempt, setAttempt] = useState(0);
  const [loaded, setLoaded] = useState<LoadedResult | null>(null);
  const [status, setStatus] = useState<'loading' | 'loaded' | 'notFound' | 'error'>('loading');
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [previewFailed, setPreviewFailed] = useState(false);
  const [actions, setActions] = useState<MediaResultActions>({});

  useEffect(() => {
    if (!isActive) {
      setPreviewUrl(null);
      setActions({});
      return;
    }
    let active = true;
    let client: Awaited<ReturnType<typeof getAcpClient>> | undefined;
    let handle: DesktopMediaHandle | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let previewTicket: string | undefined;
    const tickets = new Set<string>();
    const api = window.electron;
    const identity = { sessionId, resultId };
    const release = async (ticket: string) => {
      if (!tickets.delete(ticket)) return;
      await client?.lumina.mediaRelease_unstable({ ticket }).catch(() => undefined);
    };
    const stop = () => {
      active = false;
      clearTimeout(timer);
      if (handle) void api.mediaClose(handle.handleId).catch(() => undefined);
      for (const ticket of tickets) void release(ticket);
    };
    const disconnected = () => {
      stop();
      setPreviewUrl(null);
      setActions({});
      setStatus('error');
    };
    const grant = async (
      purpose: DesktopMediaPurpose,
      row: LoadedResult
    ): Promise<DesktopMediaGrant> => {
      if (!active || !client || client.signal.aborted) throw new Error('Media connection closed.');
      const access = await client.lumina.mediaAccess_unstable({ ...identity, purpose });
      tickets.add(access.ticket);
      if (!active) {
        await release(access.ticket);
        throw new Error('Media connection closed.');
      }
      return { ...access, metadata: row.metadata };
    };
    const refreshPreview = async (row: LoadedResult): Promise<void> => {
      let next: DesktopMediaGrant | undefined;
      try {
        next = await grant('preview', row);
        const result = await api.mediaPreview({ ...next, handleId: handle!.handleId });
        if (!active) {
          await release(next.ticket);
          return;
        }
        const previous = previewTicket;
        previewTicket = next.ticket;
        setPreviewUrl(result.url);
        setPreviewFailed(false);
        if (previous) await release(previous);
        if (active)
          timer = setTimeout(
            () => void refreshPreview(row),
            Math.max(1, Math.min(next.expiresInMs * 0.8, next.expiresInMs - 1000))
          );
      } catch {
        if (next) await release(next.ticket);
        if (previewTicket) await release(previewTicket);
        if (active) {
          setPreviewUrl(null);
          setPreviewFailed(true);
        }
      }
    };
    setStatus('loading');
    setPreviewFailed(false);
    setActions({});
    void (async () => {
      try {
        client = await getAcpClient();
        if (!active) return;
        client.signal.addEventListener('abort', disconnected, { once: true });
        if (client.signal.aborted) throw new Error('Media connection closed.');
        const row = readMediaPresentation(
          await client.lumina.mediaRead_unstable(identity),
          identity
        );
        if (!active) return;
        setLoaded(row);
        setStatus(row ? 'loaded' : 'notFound');
        if (!row || row.availability !== 'available') return;
        const opened = await api.mediaOpen(identity);
        if (!active) {
          await api.mediaClose(opened.handleId);
          return;
        }
        handle = opened;
        const action =
          (
            purpose: Exclude<DesktopMediaPurpose, 'preview'>
          ): NonNullable<MediaResultActions['open']> =>
          async (requested) => {
            if (!active || requested.sessionId !== sessionId || requested.resultId !== resultId)
              return { status: 'failed' };
            let access: DesktopMediaGrant | undefined;
            let source: DesktopMediaGrant | undefined;
            try {
              access = await grant(purpose, row);
              if (purpose === 'save_as' && opened.nativeActions)
                source = await grant('copy_path', row);
              if (!active) return { status: 'cancelled' };
              return await api.mediaAction({
                ...access,
                handleId: opened.handleId,
                purpose,
                ...(source ? { sourcePathTicket: source.ticket } : {}),
              });
            } catch {
              return { status: 'failed' };
            } finally {
              if (access) await release(access.ticket);
              if (source) await release(source.ticket);
            }
          };
        setActions({
          saveAs: action('save_as'),
          ...(opened.nativeActions
            ? { open: action('open'), reveal: action('reveal'), copyPath: action('copy_path') }
            : {}),
        });
        await refreshPreview(row);
      } catch {
        if (active) {
          setStatus('error');
          setPreviewUrl(null);
          setActions({});
        }
      }
    })();
    return () => {
      client?.signal.removeEventListener('abort', disconnected);
      stop();
    };
  }, [sessionId, resultId, isActive, attempt]);

  return (
    <div className="min-w-0 space-y-2">
      {loaded && (
        <MediaResultCard
          result={loaded.result}
          availability={loaded.availability}
          previewUrl={previewUrl}
          isActive={isActive}
          actions={actions}
        />
      )}
      {status === 'loading' && !loaded && (
        <p role="status" className="text-sm text-text-secondary">
          {intl.formatMessage({ id: 'mediaResult.loading', defaultMessage: 'Loading preview…' })}
        </p>
      )}
      {(status === 'error' || status === 'notFound') && (
        <p role="alert" className="text-sm text-text-danger">
          {intl.formatMessage(status === 'notFound' ? messages.notFound : messages.readError)}
        </p>
      )}
      {isActive &&
        (status === 'error' ||
          status === 'notFound' ||
          previewFailed ||
          (loaded && loaded.availability !== 'available')) && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAttempt((value) => value + 1)}
          >
            {intl.formatMessage(messages.retry)}
          </Button>
        )}
    </div>
  );
}

export default function ConnectedMediaResultCard(props: Props) {
  return <ConnectedMediaResultContent key={`${props.sessionId}:${props.resultId}`} {...props} />;
}
