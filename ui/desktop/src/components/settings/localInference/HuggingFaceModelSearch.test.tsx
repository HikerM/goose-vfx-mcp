import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlTestWrapper } from '../../../i18n/test-utils';
import {
  downloadHfModel,
  getRepoFiles,
  searchHfModels,
  type HfModelInfo,
  type HfModelVariant,
} from '../../../acp/local-inference';
import { HuggingFaceModelSearch } from './HuggingFaceModelSearch';

vi.mock('../../../acp/local-inference', () => ({
  downloadHfModel: vi.fn(),
  getRepoFiles: vi.fn(),
  searchHfModels: vi.fn(),
}));

const model = {
  repoId: 'Qwen/Qwen3-4B-GGUF',
  author: 'Qwen',
  modelName: 'Qwen3-4B-GGUF',
  downloads: 42,
  ggufFiles: [],
  variants: [],
} as HfModelInfo;

const variant = {
  variantId: 'Q4_K_M',
  label: 'Q4_K_M',
  backendId: 'llamacpp',
  format: 'gguf',
  modelId: 'Qwen/Qwen3-4B-GGUF:Q4_K_M',
  downloadId: 'Qwen/Qwen3-4B-GGUF:Q4_K_M',
  sizeBytes: 2_500_000_000,
  description: 'Balanced',
  qualityRank: 45,
  sharded: false,
  supported: true,
} as HfModelVariant;

describe('HuggingFaceModelSearch', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(searchHfModels).mockResolvedValue([model]);
    vi.mocked(getRepoFiles).mockResolvedValue({
      variants: [variant],
      recommendedIndex: 0,
      availableMemoryBytes: 12_000_000_000,
      downloadedQuants: [],
      downloadedVariants: [],
    });
    vi.mocked(downloadHfModel).mockResolvedValue(variant.modelId);
  });

  it('downloads the user-selected ModelScope quantization through ACP', async () => {
    const user = userEvent.setup();
    const onDownloadStarted = vi.fn();
    render(<HuggingFaceModelSearch onDownloadStarted={onDownloadStarted} />, {
      wrapper: IntlTestWrapper,
    });

    await user.type(screen.getByPlaceholderText('Search for local models...'), 'qwen');
    await user.click(await screen.findByRole('button', { name: /Qwen\/Qwen3-4B-GGUF/ }));
    await user.click(screen.getByRole('button', { name: 'Download' }));

    await waitFor(() => {
      expect(downloadHfModel).toHaveBeenCalledWith({
        spec: model.repoId,
        backendId: 'llamacpp',
        variantId: 'Q4_K_M',
      });
    });
    expect(onDownloadStarted).toHaveBeenCalledWith(variant.modelId, {
      spec: model.repoId,
      backendId: 'llamacpp',
      variantId: 'Q4_K_M',
    });
  });

  it('shows a visible error when the Rust download cannot start', async () => {
    const user = userEvent.setup();
    vi.mocked(downloadHfModel).mockRejectedValue(new Error('backend unavailable'));
    render(<HuggingFaceModelSearch onDownloadStarted={vi.fn()} />, {
      wrapper: IntlTestWrapper,
    });

    await user.type(screen.getByPlaceholderText('Search for local models...'), 'qwen');
    await user.click(await screen.findByRole('button', { name: /Qwen\/Qwen3-4B-GGUF/ }));
    await user.click(screen.getByRole('button', { name: 'Download' }));

    expect(
      await screen.findByText('Failed to start download. Please try again.')
    ).toBeInTheDocument();
  });
});
