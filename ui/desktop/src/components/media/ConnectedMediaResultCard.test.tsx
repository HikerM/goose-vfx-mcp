import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../i18n/test-utils';
import { getAcpClient } from '../../acp/acpConnection';
import ConnectedMediaResultCard from './ConnectedMediaResultCard';
import { readMediaPresentation } from './mediaResultPresentation';
import type { DesktopMediaAPI } from '../../types/desktopMedia';

vi.mock('../../acp/acpConnection', () => ({ getAcpClient: vi.fn() }));
const identity = { sessionId: 'session-1', resultId: '12345678-1234-4234-8234-123456789abc' };
const audio = {
  schemaVersion: 1,
  ...identity,
  fileName: 'registered-output.wav',
  mime: 'audio/wav',
  bytes: 115244,
  sha256: '1'.repeat(64),
  kind: 'audio',
  relativePath: 'private/result.wav',
  rootId: 'output-root',
  spec: {
    kind: 'audio',
    durationMicros: 2400000,
    sampleRate: 24000,
    channels: 1,
    sampleFrames: 57600,
  },
};
let controller: AbortController;
let methods: {
  mediaRead_unstable: ReturnType<typeof vi.fn>;
  mediaAccess_unstable: ReturnType<typeof vi.fn>;
  mediaRelease_unstable: ReturnType<typeof vi.fn>;
};
let api: DesktopMediaAPI;
let sequence: number;

beforeEach(() => {
  sequence = 0;
  controller = new AbortController();
  methods = {
    mediaRead_unstable: vi.fn(async () => ({
      status: 'found',
      result: audio,
      availability: 'available',
    })),
    mediaAccess_unstable: vi.fn(async () => ({
      ticket: `ticket-${++sequence}`,
      expiresInMs: 300000,
    })),
    mediaRelease_unstable: vi.fn(async () => ({})),
  };
  vi.mocked(getAcpClient).mockResolvedValue({
    lumina: methods,
    signal: controller.signal,
  } as unknown as Awaited<ReturnType<typeof getAcpClient>>);
  api = {
    mediaOpen: vi.fn(async () => ({ handleId: 'handle-1', nativeActions: true })),
    mediaPreview: vi.fn(async () => ({ url: 'lumina-media://content/opaque' })),
    mediaAction: vi.fn(async () => ({ status: 'completed' as const })),
    mediaClose: vi.fn(async () => undefined),
  };
  Object.assign(window.electron, api);
  vi.spyOn(window.HTMLMediaElement.prototype, 'pause').mockImplementation(() => undefined);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});
const show = (isActive = true) =>
  render(
    <IntlTestWrapper>
      <ConnectedMediaResultCard {...identity} isActive={isActive} />
    </IntlTestWrapper>
  );

describe('registered media connection', () => {
  it('reads the registered result and uses only main-issued preview URLs without autoplay', async () => {
    const view = show();
    await waitFor(() =>
      expect(view.container.querySelector('audio')).toHaveAttribute(
        'src',
        'lumina-media://content/opaque'
      )
    );
    expect(view.container.querySelector('audio')).toHaveProperty('autoplay', false);
    expect(methods.mediaRead_unstable).toHaveBeenCalledWith(identity);
    expect(api.mediaOpen).toHaveBeenCalledWith(identity);
    expect(methods.mediaAccess_unstable).toHaveBeenCalledWith({ ...identity, purpose: 'preview' });
    expect(api.mediaPreview).toHaveBeenCalledWith({
      handleId: 'handle-1',
      ticket: 'ticket-1',
      expiresInMs: 300000,
      metadata: {
        fileName: audio.fileName,
        mime: audio.mime,
        bytes: audio.bytes,
        sha256: audio.sha256,
      },
    });
    expect(view.container.textContent).not.toContain(audio.relativePath);
    expect(view.container.textContent).not.toContain('ticket-');
  });
  it('performs real-purpose IPC actions and separately protects the local save source', async () => {
    show();
    fireEvent.click(await screen.findByRole('button', { name: 'Save as' }));
    await screen.findByText('Save as: completed.');
    expect(methods.mediaAccess_unstable).toHaveBeenCalledWith({ ...identity, purpose: 'save_as' });
    expect(methods.mediaAccess_unstable).toHaveBeenCalledWith({
      ...identity,
      purpose: 'copy_path',
    });
    expect(api.mediaAction).toHaveBeenCalledWith(
      expect.objectContaining({
        purpose: 'save_as',
        ticket: 'ticket-2',
        sourcePathTicket: 'ticket-3',
        handleId: 'handle-1',
      })
    );
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'ticket-2' });
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'ticket-3' });
  });
  it('offers only preview/save-as for external backends', async () => {
    vi.mocked(api.mediaOpen).mockResolvedValue({ handleId: 'external', nativeActions: false });
    show();
    const save = await screen.findByRole('button', { name: 'Save as' });
    expect(screen.queryByRole('button', { name: 'Open file' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Show in folder' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Copy path' })).not.toBeInTheDocument();
    fireEvent.click(save);
    await screen.findByText('Save as: completed.');
    expect(api.mediaAction).toHaveBeenCalledWith(
      expect.not.objectContaining({ sourcePathTicket: expect.anything() })
    );
  });
  it('releases original-owner tickets and closes the mapping when inactive or unmounted', async () => {
    const view = show();
    await waitFor(() => expect(api.mediaPreview).toHaveBeenCalledOnce());
    const replacementRelease = vi.fn();
    vi.mocked(getAcpClient).mockResolvedValue({
      lumina: { ...methods, mediaRelease_unstable: replacementRelease },
      signal: new AbortController().signal,
    } as unknown as Awaited<ReturnType<typeof getAcpClient>>);
    view.rerender(
      <IntlTestWrapper>
        <ConnectedMediaResultCard {...identity} isActive={false} />
      </IntlTestWrapper>
    );
    await waitFor(() => expect(api.mediaClose).toHaveBeenCalledWith('handle-1'));
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'ticket-1' });
    expect(replacementRelease).not.toHaveBeenCalled();
    expect(view.container.querySelector('audio')).toBeNull();
    expect(screen.getByText(audio.fileName)).toBeInTheDocument();
  });
  it('releases a ticket arriving after unmount without opening a preview', async () => {
    let resolve!: (access: { ticket: string; expiresInMs: number }) => void;
    methods.mediaAccess_unstable.mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        })
    );
    const view = show();
    await waitFor(() => expect(methods.mediaAccess_unstable).toHaveBeenCalledOnce());
    view.unmount();
    await act(async () => resolve({ ticket: 'late-ticket', expiresInMs: 300000 }));
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'late-ticket' });
    expect(api.mediaPreview).not.toHaveBeenCalled();
  });
  it('renews preview access with the same opaque URL and releases the old ticket', async () => {
    methods.mediaAccess_unstable.mockResolvedValueOnce({ ticket: 'first', expiresInMs: 100 });
    const view = show();
    await waitFor(() => expect(api.mediaPreview).toHaveBeenCalledTimes(2));
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'first' });
    expect(view.container.querySelector('audio')).toHaveAttribute(
      'src',
      'lumina-media://content/opaque'
    );
  });
  it('keeps metadata when preview access fails and allows retry', async () => {
    vi.mocked(api.mediaPreview).mockRejectedValueOnce(new Error('private network detail'));
    show();
    const retry = await screen.findByRole('button', { name: 'Retry' });
    expect(screen.getByText(audio.fileName)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save as' })).toBeEnabled();
    expect(screen.queryByText('private network detail')).not.toBeInTheDocument();
    fireEvent.click(retry);
    await waitFor(() => expect(api.mediaPreview).toHaveBeenCalledTimes(2));
  });
  it('keeps missing-file metadata and never mints a ticket for unavailable results', async () => {
    methods.mediaRead_unstable.mockResolvedValue({
      status: 'found',
      result: audio,
      availability: 'missing',
    });
    show();
    await screen.findByText('The result file is missing.');
    expect(screen.getByText(audio.fileName)).toBeInTheDocument();
    expect(api.mediaOpen).not.toHaveBeenCalled();
    expect(methods.mediaAccess_unstable).not.toHaveBeenCalled();
  });
  it('shows a precise missing-registration state and retries through the real read method', async () => {
    methods.mediaRead_unstable.mockResolvedValueOnce({ status: 'notFound' });
    show();
    await screen.findByText('This media result is no longer registered.');
    expect(api.mediaOpen).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await screen.findByText(audio.fileName);
    expect(methods.mediaRead_unstable).toHaveBeenCalledTimes(2);
  });
  it('revokes the preview and reports connection loss without erasing result metadata', async () => {
    const view = show();
    await waitFor(() => expect(api.mediaPreview).toHaveBeenCalledOnce());
    act(() => controller.abort());
    await screen.findByText('Unable to load this media result.');
    expect(view.container.querySelector('audio')).toBeNull();
    expect(screen.getByText(audio.fileName)).toBeInTheDocument();
    expect(api.mediaClose).toHaveBeenCalledWith('handle-1');
    expect(methods.mediaRelease_unstable).toHaveBeenCalledWith({ ticket: 'ticket-1' });
  });
  it('does not contact the backend for inactive cards', () => {
    show(false);
    expect(getAcpClient).not.toHaveBeenCalled();
    expect(api.mediaOpen).not.toHaveBeenCalled();
  });
});

describe('registered result projection', () => {
  it.each([
    { schemaVersion: 2 },
    { resultId: 'another-id' },
    { sessionId: 'another-session' },
    { bytes: Number.MAX_SAFE_INTEGER + 1 },
    { sha256: 'unknown' },
    { fileName: '../unsafe' },
    { kind: 'video' },
    { spec: { ...audio.spec, durationMicros: NaN } },
  ])('rejects invalid or mismatched registration %j', (override) => {
    expect(() =>
      readMediaPresentation(
        { status: 'found', result: { ...audio, ...override }, availability: 'available' },
        identity
      )
    ).toThrow();
  });
  it('accepts actual image/video discriminants and omits source storage fields', () => {
    for (const spec of [
      { kind: 'image', width: 1280, height: 720 },
      {
        kind: 'video',
        width: 1280,
        height: 720,
        durationMicros: 1000000,
        frameCount: 24,
        fpsNumerator: 24,
        fpsDenominator: 1,
      },
    ]) {
      const projected = readMediaPresentation(
        { status: 'found', result: { ...audio, kind: spec.kind, spec }, availability: 'available' },
        identity
      );
      expect(projected?.result.kind).toBe(spec.kind);
      expect(projected?.result).not.toHaveProperty('relativePath');
      expect(projected?.result).not.toHaveProperty('rootId');
    }
  });
});
