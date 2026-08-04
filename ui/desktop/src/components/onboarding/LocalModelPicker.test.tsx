import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../i18n/test-utils';
import { getLocalModelDownloadProgress, listLocalModels } from '../../acp/local-inference';
import LocalModelPicker from './LocalModelPicker';

vi.mock('../../acp/local-inference', () => ({
  cancelLocalModelDownload: vi.fn(),
  downloadHfModel: vi.fn(),
  getLocalModelDownloadProgress: vi.fn(),
  listLocalModels: vi.fn(),
}));

vi.mock('../settings/localInference/HuggingFaceModelSearch', () => ({
  HuggingFaceModelSearch: ({
    onDownloadStarted,
  }: {
    onDownloadStarted: (modelId: string) => void;
  }) => (
    <button onClick={() => onDownloadStarted('Qwen/Qwen3-4B-GGUF:Q4_K_M')}>
      Choose a ModelScope model
    </button>
  ),
}));

describe('LocalModelPicker', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    vi.mocked(listLocalModels).mockResolvedValue([]);
    vi.mocked(getLocalModelDownloadProgress).mockResolvedValue({
      modelId: 'Qwen/Qwen3-4B-GGUF:Q4_K_M-model',
      status: 'completed',
      bytesDownloaded: 2_500_000_000,
      totalBytes: 2_500_000_000,
      progressPercent: 100,
      speedBps: null,
      etaSeconds: null,
      error: null,
      retryAttempt: 0,
      maxRetries: 10,
      taskExited: true,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('keeps ModelScope selection available when no models are installed', async () => {
    render(<LocalModelPicker onConfigured={vi.fn()} />, { wrapper: IntlTestWrapper });

    await act(async () => {
      await Promise.resolve();
    });

    expect(screen.getByRole('button', { name: 'Choose a ModelScope model' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Select a model' })).not.toBeInTheDocument();
  });

  it('automatically configures the selected model after the Rust download completes', async () => {
    const onConfigured = vi.fn().mockResolvedValue(undefined);
    render(<LocalModelPicker onConfigured={onConfigured} />, { wrapper: IntlTestWrapper });

    await act(async () => {
      await Promise.resolve();
    });
    fireEvent.click(screen.getByRole('button', { name: 'Choose a ModelScope model' }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(getLocalModelDownloadProgress).toHaveBeenCalledWith('Qwen/Qwen3-4B-GGUF:Q4_K_M');
    expect(onConfigured).toHaveBeenCalledWith('local', 'Qwen/Qwen3-4B-GGUF:Q4_K_M');
  });
});
