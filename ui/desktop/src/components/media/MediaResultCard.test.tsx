import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../i18n/test-utils';
import MediaResultCard from './MediaResultCard';
import type { MediaActionOutcome, MediaResultCardProps, MediaResultPresentation } from './types';

const audio: MediaResultPresentation = {
  sessionId: 'session-a',
  resultId: 'result-a',
  fileName: 'registered-speech.wav',
  mime: 'audio/wav',
  bytes: 115244,
  kind: 'audio',
  spec: {
    kind: 'audio',
    durationMicros: 2400000,
    sampleRate: 24000,
    channels: 1,
    sampleFrames: 57600,
  },
};
const image: MediaResultPresentation = {
  ...audio,
  fileName: 'registered-image.png',
  mime: 'image/png',
  kind: 'image',
  spec: { kind: 'image', width: 1280, height: 720 },
};
const video: MediaResultPresentation = {
  ...audio,
  fileName: 'registered-video.mp4',
  mime: 'video/mp4',
  kind: 'video',
  spec: {
    kind: 'video',
    width: 1280,
    height: 720,
    durationMicros: 2002000,
    frameCount: 60,
    fpsNumerator: 30000,
    fpsDenominator: 1001,
  },
};

const observers: {
  notify: (target: globalThis.Element, visible: boolean) => void;
  disconnect: ReturnType<typeof vi.fn>;
}[] = [];
const pause = vi.fn();
const play = vi.fn();

beforeEach(() => {
  observers.length = 0;
  pause.mockReset();
  play.mockReset().mockResolvedValue(undefined);
  vi.spyOn(window.HTMLMediaElement.prototype, 'pause').mockImplementation(pause);
  vi.spyOn(window.HTMLMediaElement.prototype, 'play').mockImplementation(play);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  vi.stubGlobal(
    'IntersectionObserver',
    class {
      disconnect = vi.fn();
      observe = vi.fn();
      constructor(callback: globalThis.IntersectionObserverCallback) {
        observers.push({
          disconnect: this.disconnect,
          notify: (target, visible) =>
            callback(
              [{ target, isIntersecting: visible } as globalThis.IntersectionObserverEntry],
              this as unknown as globalThis.IntersectionObserver
            ),
        });
      }
    }
  );
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function props(overrides: Partial<MediaResultCardProps> = {}): MediaResultCardProps {
  return {
    result: audio,
    availability: 'available',
    previewUrl: 'blob:controlled-result-a',
    isActive: true,
    actions: {},
    ...overrides,
  };
}

function renderCard(overrides: Partial<MediaResultCardProps> = {}) {
  return render(<MediaResultCard {...props(overrides)} />, { wrapper: IntlTestWrapper });
}

function deferred() {
  let resolve!: (outcome: MediaActionOutcome) => void;
  const promise = new Promise<MediaActionOutcome>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe('MediaResultCard', () => {
  it('renders registered audio metadata and native controls without autoplay or invented actions', () => {
    renderCard();
    const player = screen.getByLabelText(audio.fileName, { selector: 'audio' });
    expect(player.tagName).toBe('AUDIO');
    expect(player).toHaveAttribute('src', 'blob:controlled-result-a');
    expect(player).toHaveAttribute('controls');
    expect(player).toHaveAttribute('preload', 'metadata');
    expect(player).not.toHaveAttribute('autoplay');
    expect(screen.getByRole('heading', { name: audio.fileName })).toBeInTheDocument();
    expect(screen.getByText('audio/wav')).toBeInTheDocument();
    expect(screen.getByText('115,244 B')).toBeInTheDocument();
    expect(screen.getByText('2.4 s · 24,000 Hz · 1 channel')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Loading preview');
    fireEvent.loadedMetadata(player);
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
    expect(play).not.toHaveBeenCalled();
  });

  it('preserves an image aspect ratio and marks loading complete only on the image event', () => {
    renderCard({ result: image });
    const preview = screen.getByRole('img', { name: image.fileName });
    expect(preview).toHaveAttribute('width', '1280');
    expect(preview).toHaveAttribute('height', '720');
    expect(preview).toHaveClass('object-contain', 'h-auto', 'max-w-full');
    expect(screen.getByText('1,280 × 720 px')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Loading preview');
    fireEvent.load(preview);
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('renders video with native controls, intrinsic dimensions and actual rational frame rate', () => {
    renderCard({ result: video });
    const player = screen.getByLabelText(video.fileName, { selector: 'video' });
    expect(player.tagName).toBe('VIDEO');
    expect(player).toHaveAttribute('controls');
    expect(player).toHaveAttribute('playsinline');
    expect(player).toHaveAttribute('width', '1280');
    expect(player).toHaveAttribute('height', '720');
    expect(player).not.toHaveAttribute('autoplay');
    expect(screen.getByText('1,280 × 720 px · 2.002 s · 29.97 FPS')).toBeInTheDocument();
  });

  it.each([
    ['publishing', 'still being published'],
    ['missing', 'file is missing'],
    ['access_denied', 'Access to the result file was denied'],
    ['tampered', 'has changed or is damaged'],
    ['unsafe_path', 'location could not be verified'],
    ['unsupported_format', 'format is not supported'],
    ['io_unavailable', 'temporarily unavailable'],
  ] as const)(
    'keeps metadata and blocks preview/actions for backend availability %s',
    (availability, message) => {
      const open = vi.fn();
      renderCard({ availability, actions: { open } });
      expect(screen.getByRole('heading', { name: audio.fileName })).toBeInTheDocument();
      expect(screen.getByText('115,244 B')).toBeInTheDocument();
      expect(screen.getByRole('status')).toHaveTextContent(message);
      expect(
        screen.queryByLabelText(audio.fileName, { selector: 'audio' })
      ).not.toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Open file' })).toBeDisabled();
      fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
      expect(open).not.toHaveBeenCalled();
    }
  );

  it('does not invent a preview URL while independently exposing an available real action', async () => {
    const open = vi.fn().mockResolvedValue({ status: 'completed' });
    renderCard({ previewUrl: null, actions: { open } });
    expect(screen.getByRole('status')).toHaveTextContent('A preview is not available');
    expect(screen.queryByLabelText(audio.fileName, { selector: 'audio' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    await screen.findByText('Open file: completed.');
    expect(open).toHaveBeenCalledExactlyOnceWith({ sessionId: 'session-a', resultId: 'result-a' });
    expect(screen.queryByRole('button', { name: 'Show in folder' })).not.toBeInTheDocument();
  });

  it('runs only provided actions with identity and prevents duplicate or overlapping clicks', async () => {
    const operation = deferred();
    const copyPath = vi.fn().mockReturnValue(operation.promise);
    const reveal = vi.fn().mockResolvedValue({ status: 'completed' });
    renderCard({ actions: { copyPath, reveal } });
    fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));
    fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));
    fireEvent.click(screen.getByRole('button', { name: 'Show in folder' }));
    expect(copyPath).toHaveBeenCalledExactlyOnceWith({
      sessionId: 'session-a',
      resultId: 'result-a',
    });
    expect(reveal).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Copy path' })).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByRole('button', { name: 'Show in folder' })).toBeDisabled();
    expect(screen.getByText('Copy path…')).toBeInTheDocument();
    expect(screen.queryByText('Copy path: completed.')).not.toBeInTheDocument();
    await act(async () => operation.resolve({ status: 'completed' }));
    expect(screen.getByText('Copy path: completed.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy path' })).toBeEnabled();
  });

  it('reports a cancelled save without claiming the file was saved', async () => {
    const saveAs = vi.fn().mockResolvedValue({ status: 'cancelled' });
    renderCard({ actions: { saveAs } });
    fireEvent.click(screen.getByRole('button', { name: 'Save as' }));
    await screen.findByText('Save as: cancelled.');
    expect(screen.queryByText(/completed/)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as' })).toBeEnabled();
  });

  it('reports action failure as text, without interpreting a callback message as markup', async () => {
    const detail = '<img src=x onerror=alert(1)> permission denied';
    const reveal = vi.fn().mockResolvedValue({ status: 'failed', message: detail });
    renderCard({ actions: { reveal } });
    fireEvent.click(screen.getByRole('button', { name: 'Show in folder' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(`Show in folder: failed. ${detail}`);
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('handles a rejected action and allows a subsequent retry', async () => {
    const open = vi
      .fn()
      .mockRejectedValueOnce(new Error('internal transport failure'))
      .mockResolvedValueOnce({ status: 'completed' });
    renderCard({ actions: { open } });
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Open file: failed.');
    expect(screen.queryByText(/internal transport/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    await screen.findByText('Open file: completed.');
    expect(open).toHaveBeenCalledTimes(2);
  });

  it('ignores an old action completion after switching result identity', async () => {
    const operation = deferred();
    const view = renderCard({ actions: { open: () => operation.promise } });
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    view.rerender(
      <MediaResultCard
        {...props({ result: { ...audio, resultId: 'result-b', fileName: 'next.wav' } })}
      />
    );
    await act(async () => operation.resolve({ status: 'completed' }));
    expect(screen.getByRole('heading', { name: 'next.wav' })).toBeInTheDocument();
    expect(screen.queryByText('Open file: completed.')).not.toBeInTheDocument();
  });

  it('pauses when offscreen and never resumes automatically on reentry', () => {
    renderCard();
    const player = screen.getByLabelText(audio.fileName, { selector: 'audio' });
    act(() => observers[0].notify(player, false));
    expect(pause).toHaveBeenCalledTimes(1);
    fireEvent.play(player);
    expect(pause).toHaveBeenCalledTimes(2);
    act(() => observers[0].notify(player, true));
    expect(play).not.toHaveBeenCalled();
    fireEvent.play(player);
    expect(pause).toHaveBeenCalledTimes(2);
  });

  it('pauses on document hiding, pagehide, leaving the session and unmounting', () => {
    const view = renderCard();
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    fireEvent(document, new Event('visibilitychange'));
    expect(pause).toHaveBeenCalledTimes(1);
    fireEvent(window, new Event('pagehide'));
    expect(pause).toHaveBeenCalledTimes(2);
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    view.rerender(<MediaResultCard {...props({ isActive: false })} />);
    expect(pause).toHaveBeenCalledTimes(3);
    fireEvent.play(screen.getByLabelText(audio.fileName, { selector: 'audio' }));
    expect(pause).toHaveBeenCalledTimes(4);
    view.unmount();
    expect(pause).toHaveBeenCalledTimes(5);
    expect(observers[0].disconnect).toHaveBeenCalledExactlyOnceWith();
    fireEvent(window, new Event('pagehide'));
    expect(pause).toHaveBeenCalledTimes(5);
  });

  it('pauses replaced media and resets only preview state for a changed controlled URL', () => {
    const view = renderCard();
    const originalPlayer = screen.getByLabelText(audio.fileName, { selector: 'audio' });
    fireEvent.loadedMetadata(originalPlayer);
    view.rerender(<MediaResultCard {...props({ previewUrl: 'blob:renewed-grant' })} />);
    expect(pause).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText(audio.fileName, { selector: 'audio' })).not.toBe(originalPlayer);
    expect(screen.getByLabelText(audio.fileName, { selector: 'audio' })).toHaveAttribute(
      'src',
      'blob:renewed-grant'
    );
    expect(screen.getByRole('status')).toHaveTextContent('Loading preview');
    expect(play).not.toHaveBeenCalled();
  });

  it('pauses on a session identity change even when result ID and URL happen to match', () => {
    const view = renderCard();
    view.rerender(<MediaResultCard {...props({ result: { ...audio, sessionId: 'session-b' } })} />);
    expect(pause).toHaveBeenCalledTimes(1);
    expect(observers[0].disconnect).toHaveBeenCalledExactlyOnceWith();
  });

  it('keeps file actions and metadata after a decoder error without declaring generation failure', async () => {
    const open = vi.fn().mockResolvedValue({ status: 'completed' });
    renderCard({ actions: { open } });
    fireEvent.error(screen.getByLabelText(audio.fileName, { selector: 'audio' }));
    expect(screen.getByRole('alert')).toHaveTextContent('Unable to preview this file');
    expect(screen.getByRole('heading', { name: audio.fileName })).toBeInTheDocument();
    expect(screen.getByText('115,244 B')).toBeInTheDocument();
    expect(screen.queryByText(/generat.*fail/i)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Open file' })).toBeEnabled();
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    await screen.findByText('Open file: completed.');
  });

  it('retains image metadata when the image cannot be decoded', () => {
    renderCard({ result: image });
    fireEvent.error(screen.getByRole('img', { name: image.fileName }));
    expect(screen.getByRole('alert')).toHaveTextContent('Unable to preview this file');
    expect(screen.getByText('1,280 × 720 px')).toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it.each(['text/html', 'image/svg+xml', 'image/png', 'audio/wav\ntext/html'])(
    'does not load an unsupported or mismatched MIME %s into the audio element',
    (mime) => {
      renderCard({ result: { ...audio, mime } });
      expect(
        screen.queryByLabelText(audio.fileName, { selector: 'audio' })
      ).not.toBeInTheDocument();
      expect(screen.getByRole('status')).toHaveTextContent('format is not supported');
      expect(screen.getByRole('heading', { name: audio.fileName })).toBeInTheDocument();
    }
  );

  it('does not run actions from an inactive conversation', async () => {
    const open = vi.fn();
    renderCard({ isActive: false, actions: { open } });
    fireEvent.click(screen.getByRole('button', { name: 'Open file' }));
    await waitFor(() => expect(open).not.toHaveBeenCalled());
    expect(screen.getByRole('button', { name: 'Open file' })).toBeDisabled();
  });
});
