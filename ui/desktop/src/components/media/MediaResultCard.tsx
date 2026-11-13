import { useEffect, useId, useRef, useState } from 'react';
import { defineMessages, useIntl } from '../../i18n';
import { Button } from '../ui/button';
import type {
  MediaAvailability,
  MediaFileAction,
  MediaResultCardProps,
  MediaResultPresentation,
} from './types';

const i18n = defineMessages({
  previewUnavailable: {
    id: 'mediaResult.previewUnavailable',
    defaultMessage: 'A preview is not available for this result.',
  },
  loading: { id: 'mediaResult.loading', defaultMessage: 'Loading preview…' },
  previewError: {
    id: 'mediaResult.previewError',
    defaultMessage: 'Unable to preview this file. Its format may be unsupported or damaged.',
  },
  publishing: {
    id: 'mediaResult.publishing',
    defaultMessage: 'The file is still being published.',
  },
  missing: { id: 'mediaResult.missing', defaultMessage: 'The result file is missing.' },
  accessDenied: {
    id: 'mediaResult.accessDenied',
    defaultMessage: 'Access to the result file was denied.',
  },
  tampered: {
    id: 'mediaResult.tampered',
    defaultMessage: 'The result file has changed or is damaged.',
  },
  unsafePath: {
    id: 'mediaResult.unsafePath',
    defaultMessage: 'The result location could not be verified.',
  },
  unsupportedFormat: {
    id: 'mediaResult.unsupportedFormat',
    defaultMessage: 'This media format is not supported.',
  },
  ioUnavailable: {
    id: 'mediaResult.ioUnavailable',
    defaultMessage: 'The result file is temporarily unavailable.',
  },
  open: { id: 'mediaResult.open', defaultMessage: 'Open file' },
  reveal: { id: 'mediaResult.reveal', defaultMessage: 'Show in folder' },
  copyPath: { id: 'mediaResult.copyPath', defaultMessage: 'Copy path' },
  saveAs: { id: 'mediaResult.saveAs', defaultMessage: 'Save as' },
  actionBusy: { id: 'mediaResult.actionBusy', defaultMessage: '{action}…' },
  actionCompleted: { id: 'mediaResult.actionCompleted', defaultMessage: '{action}: completed.' },
  actionCancelled: { id: 'mediaResult.actionCancelled', defaultMessage: '{action}: cancelled.' },
  actionFailed: { id: 'mediaResult.actionFailed', defaultMessage: '{action}: failed.' },
  channels: {
    id: 'mediaResult.channels',
    defaultMessage: '{count, plural, one {# channel} other {# channels}}',
  },
});

const availabilityMessages = {
  publishing: i18n.publishing,
  missing: i18n.missing,
  access_denied: i18n.accessDenied,
  tampered: i18n.tampered,
  unsafe_path: i18n.unsafePath,
  unsupported_format: i18n.unsupportedFormat,
  io_unavailable: i18n.ioUnavailable,
} satisfies Record<Exclude<MediaAvailability, 'available'>, (typeof i18n)['missing']>;

const previewMimeTypes = {
  image: new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/avif']),
  audio: new Set([
    'audio/wav',
    'audio/wave',
    'audio/x-wav',
    'audio/mpeg',
    'audio/mp3',
    'audio/ogg',
    'audio/webm',
    'audio/mp4',
    'audio/aac',
    'audio/flac',
    'audio/x-flac',
  ]),
  video: new Set(['video/mp4', 'video/webm', 'video/ogg']),
};

function supportsPreview(result: MediaResultPresentation): boolean {
  const mime = result.mime.trim().toLowerCase();
  return (
    mime.length <= 256 &&
    /^[a-z]+\/[a-z0-9.+-]+(?:\s*;\s*codecs\s*=\s*(?:"[a-z0-9., _-]+"|[a-z0-9.,_-]+))?$/.test(
      mime
    ) &&
    previewMimeTypes[result.kind].has(mime.split(';', 1)[0].trim())
  );
}

function ResultPreview({
  result,
  url,
  isActive,
}: {
  result: MediaResultPresentation;
  url: string;
  isActive: boolean;
}) {
  const intl = useIntl();
  const [loaded, setLoaded] = useState(false);
  const [failed, setFailed] = useState(false);
  const mediaRef = useRef<globalThis.HTMLMediaElement | null>(null);
  const inViewport = useRef(true);

  useEffect(() => {
    const media = mediaRef.current;
    if (!media) return;
    const pause = () => media.pause();
    const onVisibility = () => {
      if (document.visibilityState !== 'visible') pause();
    };
    const observer =
      typeof window.IntersectionObserver === 'function'
        ? new window.IntersectionObserver((entries) => {
            const entry = entries.find((item) => item.target === media);
            if (!entry) return;
            inViewport.current = entry.isIntersecting;
            if (!entry.isIntersecting) pause();
          })
        : null;
    observer?.observe(media);
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('pagehide', pause);
    return () => {
      pause();
      observer?.disconnect();
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('pagehide', pause);
    };
  }, []);

  useEffect(() => {
    if (!isActive) mediaRef.current?.pause();
  }, [isActive]);

  const mediaProps = {
    src: url,
    controls: true,
    autoPlay: false,
    preload: 'metadata',
    'aria-label': result.fileName,
    onLoadedMetadata: () => setLoaded(true),
    onError: () => {
      mediaRef.current?.pause();
      setFailed(true);
    },
    onPlay: () => {
      if (!isActive || !inViewport.current || document.visibilityState !== 'visible') {
        mediaRef.current?.pause();
      }
    },
  };

  return (
    <div className="flex min-w-0 flex-col gap-2">
      {result.kind === 'image' ? (
        <img
          src={url}
          alt={result.fileName}
          width={result.spec.width}
          height={result.spec.height}
          className="block h-auto max-h-96 w-full max-w-full rounded-md object-contain"
          hidden={failed}
          onLoad={() => setLoaded(true)}
          onError={() => setFailed(true)}
        />
      ) : result.kind === 'audio' ? (
        <audio
          {...mediaProps}
          ref={(element) => {
            mediaRef.current = element;
          }}
          hidden={failed}
          className="block w-full min-w-0 max-w-full"
        />
      ) : (
        <video
          {...mediaProps}
          ref={(element) => {
            mediaRef.current = element;
          }}
          hidden={failed}
          playsInline
          width={result.spec.width}
          height={result.spec.height}
          className="block h-auto max-h-[min(24rem,50vh)] w-full max-w-full rounded-md object-contain"
        />
      )}
      {failed ? (
        <p role="alert" className="text-sm text-text-danger">
          {intl.formatMessage(i18n.previewError)}
        </p>
      ) : !loaded ? (
        <p role="status" className="text-sm text-text-secondary">
          {intl.formatMessage(i18n.loading)}
        </p>
      ) : null}
    </div>
  );
}

type ActionFeedback = {
  action: MediaFileAction;
  phase: 'busy' | 'completed' | 'cancelled' | 'failed';
  message?: string;
};
const actionOrder: MediaFileAction[] = ['open', 'reveal', 'copyPath', 'saveAs'];
const actionMessages = {
  busy: i18n.actionBusy,
  completed: i18n.actionCompleted,
  cancelled: i18n.actionCancelled,
  failed: i18n.actionFailed,
};

function MediaResultCardContent({
  result,
  availability,
  previewUrl,
  isActive,
  actions,
}: MediaResultCardProps) {
  const intl = useIntl();
  const titleId = useId();
  const [feedback, setFeedback] = useState<ActionFeedback | null>(null);
  const pending = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const runAction = async (action: MediaFileAction) => {
    const callback = actions[action];
    if (!callback || pending.current || availability !== 'available' || !isActive) return;
    pending.current = true;
    setFeedback({ action, phase: 'busy' });
    try {
      const outcome = await callback({ resultId: result.resultId, sessionId: result.sessionId });
      if (mounted.current) {
        setFeedback({
          action,
          phase: outcome.status,
          ...('message' in outcome ? { message: outcome.message } : {}),
        });
      }
    } catch {
      if (mounted.current) setFeedback({ action, phase: 'failed' });
    } finally {
      pending.current = false;
    }
  };

  const spec = result.spec;
  const specifications: string[] = [];
  if ('width' in spec)
    specifications.push(`${intl.formatNumber(spec.width)} × ${intl.formatNumber(spec.height)} px`);
  if ('durationMicros' in spec)
    specifications.push(
      `${intl.formatNumber(spec.durationMicros / 1_000_000, { maximumFractionDigits: 3 })} s`
    );
  if (spec.kind === 'audio') {
    specifications.push(
      `${intl.formatNumber(spec.sampleRate)} Hz`,
      intl.formatMessage(i18n.channels, { count: spec.channels })
    );
  }
  if (spec.kind === 'video')
    specifications.push(
      `${intl.formatNumber(spec.fpsNumerator / spec.fpsDenominator, { maximumFractionDigits: 3 })} FPS`
    );
  const availableActions = actionOrder.filter((action) => actions[action]);

  return (
    <article
      aria-labelledby={titleId}
      className="flex w-full min-w-0 max-w-full flex-col gap-3 rounded-lg border border-border-primary bg-background-primary p-3 text-text-primary"
    >
      <div className="flex min-w-0 flex-col gap-1">
        <h3 id={titleId} className="break-all text-sm font-medium">
          {result.fileName}
        </h3>
        <p className="flex flex-wrap gap-x-3 gap-y-1 break-all text-xs text-text-secondary">
          <span>{result.mime}</span>
          <span>{intl.formatNumber(result.bytes)} B</span>
        </p>
        <p className="break-words text-xs text-text-secondary">{specifications.join(' · ')}</p>
      </div>
      {availability !== 'available' ? (
        <p role="status" className="text-sm text-text-secondary">
          {intl.formatMessage(availabilityMessages[availability])}
        </p>
      ) : !supportsPreview(result) ? (
        <p role="status" className="text-sm text-text-secondary">
          {intl.formatMessage(i18n.unsupportedFormat)}
        </p>
      ) : previewUrl ? (
        <ResultPreview
          key={JSON.stringify([result.kind, result.mime, previewUrl])}
          result={result}
          url={previewUrl}
          isActive={isActive}
        />
      ) : (
        <p role="status" className="text-sm text-text-secondary">
          {intl.formatMessage(i18n.previewUnavailable)}
        </p>
      )}
      {availableActions.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {availableActions.map((action) => (
            <Button
              key={action}
              type="button"
              variant="outline"
              size="sm"
              className="h-auto min-h-8 max-w-full whitespace-normal break-words py-2"
              disabled={feedback?.phase === 'busy' || availability !== 'available' || !isActive}
              aria-busy={feedback?.action === action && feedback.phase === 'busy'}
              onClick={() => {
                void runAction(action);
              }}
            >
              {intl.formatMessage(i18n[action])}
            </Button>
          ))}
        </div>
      )}
      {feedback && (
        <p
          role={feedback.phase === 'failed' ? 'alert' : 'status'}
          className="break-words text-sm text-text-secondary"
        >
          {intl.formatMessage(actionMessages[feedback.phase], {
            action: intl.formatMessage(i18n[feedback.action]),
          })}
          {feedback.phase === 'failed' && feedback.message && <> {feedback.message}</>}
        </p>
      )}
    </article>
  );
}

export default function MediaResultCard(props: MediaResultCardProps) {
  return (
    <MediaResultCardContent
      key={JSON.stringify([props.result.sessionId, props.result.resultId])}
      {...props}
    />
  );
}
