import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import AudioPreview from './AudioPreview';
import { IntlTestWrapper } from '../i18n/test-utils';

const createObjectURL = vi.fn();
const revokeObjectURL = vi.fn();

beforeEach(() => {
  createObjectURL.mockReset().mockReturnValue('blob:audio-test');
  revokeObjectURL.mockReset();
  vi.stubGlobal(
    'URL',
    class extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    }
  );
});

afterEach(() => vi.unstubAllGlobals());

function renderAudio(data = 'AQID', mimeType = 'audio/wav') {
  return render(<AudioPreview data={data} mimeType={mimeType} />, { wrapper: IntlTestWrapper });
}

describe('AudioPreview', () => {
  it('uses a bounded local blob with native controls and no autoplay', async () => {
    renderAudio();
    const player = screen.getByLabelText('Audio message');
    expect(player.tagName).toBe('AUDIO');
    expect(player).toHaveAttribute('src', 'blob:audio-test');
    expect(player).toHaveAttribute('controls');
    expect(player).toHaveAttribute('preload', 'metadata');
    expect(player).not.toHaveAttribute('autoplay');
    const blob = createObjectURL.mock.calls[0][0] as Blob;
    expect(blob.type).toBe('audio/wav');
    expect(blob.size).toBe(3);
    const contents = await new Promise<ArrayBuffer>((resolve) => {
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result as ArrayBuffer);
      reader.readAsArrayBuffer(blob);
    });
    expect([...new Uint8Array(contents)]).toEqual([1, 2, 3]);
    expect(screen.getByRole('status')).toHaveTextContent('Loading audio');
    fireEvent.loadedMetadata(player);
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it.each(['audio/webm;codecs=opus', ' Audio/WAV ', 'audio/ogg; codecs="opus"'])(
    'accepts supported MIME spelling %s',
    (mime) => {
      renderAudio('AQID', mime);
      expect(screen.getByLabelText('Audio message')).toBeInTheDocument();
    }
  );

  it.each(['text/html', 'image/png', 'audio/pcm', 'audio/wav;evil=1', 'audio/wav\ntext/html'])(
    'rejects unsupported or malformed MIME %s before allocating a blob',
    (mime) => {
      renderAudio('AQID', mime);
      expect(screen.getByRole('alert')).toHaveTextContent('format is not supported');
      expect(screen.queryByLabelText('Audio message')).not.toBeInTheDocument();
      expect(createObjectURL).not.toHaveBeenCalled();
    }
  );

  it.each([
    '',
    '!',
    'A',
    'AQ=ID',
    'AQID===',
    'data:audio/wav;base64,AQID',
    'https://example.com/a.wav',
  ])('rejects empty, malformed, or URL payload %s', (data) => {
    renderAudio(data);
    expect(screen.getByRole('alert')).toHaveTextContent('empty or invalid');
    expect(createObjectURL).not.toHaveBeenCalled();
  });

  it('rejects oversized audio before decoding or creating a blob', () => {
    const decode = vi.spyOn(window, 'atob');
    renderAudio('A'.repeat(Math.ceil((32 * 1024 * 1024) / 3) * 4 + 4));
    expect(screen.getByRole('alert')).toHaveTextContent('32 MiB');
    expect(decode).not.toHaveBeenCalled();
    expect(createObjectURL).not.toHaveBeenCalled();
    decode.mockRestore();
  });

  it('reports decoder errors and resets error state when the clip changes', () => {
    const { rerender } = renderAudio();
    fireEvent.error(screen.getByLabelText('Audio message'));
    expect(screen.getByRole('alert')).toHaveTextContent('Unable to play this audio');
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:audio-test');
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    createObjectURL.mockReturnValue('blob:replacement');
    rerender(<AudioPreview data="BAUG" mimeType="audio/wav" />);
    expect(screen.getByLabelText('Audio message')).toHaveAttribute('src', 'blob:replacement');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:audio-test');
  });

  it('keeps a stable player through rerenders and revokes each replaced/unmounted URL', () => {
    const { rerender, unmount } = renderAudio();
    const player = screen.getByLabelText('Audio message');
    rerender(<AudioPreview data="AQID" mimeType="audio/wav" />);
    expect(screen.getByLabelText('Audio message')).toBe(player);
    expect(createObjectURL).toHaveBeenCalledTimes(1);
    createObjectURL.mockReturnValue('blob:replacement');
    rerender(<AudioPreview data="BAUG" mimeType="audio/wav" />);
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:audio-test');
    unmount();
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:replacement');
    expect(revokeObjectURL).toHaveBeenCalledTimes(2);
  });

  it('reports object URL allocation failure', () => {
    createObjectURL.mockImplementation(() => {
      throw new Error('unavailable');
    });
    renderAudio();
    expect(screen.getByRole('alert')).toHaveTextContent('Unable to play this audio');
  });
});
