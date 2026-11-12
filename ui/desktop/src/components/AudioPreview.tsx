import { useEffect, useState } from 'react';
import { defineMessages, useIntl } from '../i18n';
import type { AudioContent } from '../types/message';

const i18n = defineMessages({
  label: { id: 'audioPreview.label', defaultMessage: 'Audio message' },
  loading: { id: 'audioPreview.loading', defaultMessage: 'Loading audio…' },
  unsupported: {
    id: 'audioPreview.unsupported',
    defaultMessage: 'This audio format is not supported.',
  },
  invalid: { id: 'audioPreview.invalid', defaultMessage: 'The audio data is empty or invalid.' },
  tooLarge: {
    id: 'audioPreview.tooLarge',
    defaultMessage: 'This audio exceeds the 32 MiB playback limit.',
  },
  unableToPlay: {
    id: 'audioPreview.unableToPlay',
    defaultMessage: 'Unable to play this audio. The file may be damaged or its codec unsupported.',
  },
});

const MAX_AUDIO_BYTES = 32 * 1024 * 1024;
const MAX_BASE64_LENGTH = Math.ceil(MAX_AUDIO_BYTES / 3) * 4;
const AUDIO_MIME_TYPES = new Set([
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
]);

type AudioError = 'unsupported' | 'invalid' | 'tooLarge' | 'unableToPlay';
type PreparedAudio = Pick<AudioContent, 'data' | 'mimeType'> &
  ({ src: string; error?: never } | { src?: never; error: AudioError });

function audioBlob(data: string, mimeType: string): Blob | AudioError {
  if (mimeType.length > 256) return 'unsupported';
  const normalizedMime = mimeType.trim().toLowerCase();
  const essence = normalizedMime.split(';', 1)[0].trim();
  if (
    !AUDIO_MIME_TYPES.has(essence) ||
    !/^audio\/[a-z0-9.+-]+(?:\s*;\s*codecs\s*=\s*(?:"[a-z0-9., _-]+"|[a-z0-9.,_-]+))?$/.test(
      normalizedMime
    )
  ) {
    return 'unsupported';
  }
  if (data.length > MAX_BASE64_LENGTH) return 'tooLarge';
  if (!data || !/^[A-Za-z0-9+/]+={0,2}$/.test(data)) return 'invalid';

  try {
    const decoded = window.atob(data);
    if (decoded.length > MAX_AUDIO_BYTES) return 'tooLarge';
    const bytes = new Uint8Array(decoded.length);
    for (let index = 0; index < decoded.length; index++) {
      bytes[index] = decoded.charCodeAt(index);
    }
    return new Blob([bytes], { type: normalizedMime });
  } catch {
    return 'invalid';
  }
}

function AudioPlayer({ src, onError }: { src: string; onError: () => void }) {
  const intl = useIntl();
  const [loaded, setLoaded] = useState(false);

  return (
    <>
      <audio
        src={src}
        controls
        preload="metadata"
        aria-label={intl.formatMessage(i18n.label)}
        className="block w-full min-w-0 max-w-full rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-border-primary"
        onLoadedMetadata={() => setLoaded(true)}
        onError={onError}
      />
      {!loaded && (
        <p role="status" className="text-xs text-text-secondary">
          {intl.formatMessage(i18n.loading)}
        </p>
      )}
    </>
  );
}

export default function AudioPreview({ data, mimeType }: Pick<AudioContent, 'data' | 'mimeType'>) {
  const intl = useIntl();
  const [prepared, setPrepared] = useState<PreparedAudio>();

  useEffect(() => {
    const blob = audioBlob(data, mimeType);
    if (typeof blob === 'string') {
      setPrepared({ data, mimeType, error: blob });
      return;
    }

    try {
      const src = URL.createObjectURL(blob);
      setPrepared({ data, mimeType, src });
      return () => URL.revokeObjectURL(src);
    } catch {
      setPrepared({ data, mimeType, error: 'unableToPlay' });
      return;
    }
  }, [data, mimeType]);

  const current = prepared?.data === data && prepared.mimeType === mimeType ? prepared : undefined;
  return (
    <div className="audio-preview my-2 flex w-full min-w-0 max-w-full flex-col gap-1">
      {current?.error ? (
        <p role="alert" className="text-sm text-text-secondary">
          {intl.formatMessage(i18n[current.error])}
        </p>
      ) : current?.src ? (
        <AudioPlayer
          key={current.src}
          src={current.src}
          onError={() => {
            URL.revokeObjectURL(current.src);
            setPrepared({ data, mimeType, error: 'unableToPlay' });
          }}
        />
      ) : (
        <p role="status" className="text-xs text-text-secondary">
          {intl.formatMessage(i18n.loading)}
        </p>
      )}
    </div>
  );
}
