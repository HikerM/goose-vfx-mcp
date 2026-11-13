import type { NativeTaskDescriptionDto } from '@hikerm/lumina-sdk';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { describeInstalledTask, listInstalledTasks } from '../../../acp/local-inference';
import { IntlTestWrapper } from '../../../i18n/test-utils';
import { InstalledTaskParameters } from './InstalledTaskParameters';

vi.mock('../../../acp/local-inference', () => ({
  listInstalledTasks: vi.fn(),
  describeInstalledTask: vi.fn(),
}));
vi.mock('../../../acp/chatSessionStore', () => ({
  acpChatSessionStore: { getSnapshot: () => undefined },
}));

function checkpoint(name: string, maximum: number): NativeTaskDescriptionDto {
  return {
    identity: {
      installationId: name,
      taskVariant: 'image',
      versionId: 'v1',
      parameterSchemaId: name + '-schema',
      manifestDigest: name + '-manifest',
      runtimeDigest: 'same-engine',
      componentBindingDigest: name + '-weights',
    },
    intent: 'text_to_image',
    descriptor: {
      family: 'generation',
      parameters: {
        schemaVersion: 1,
        profile: name,
        executionStatus: 'unknown',
        evidence: '',
        limitations: [],
        fields: {
          steps: {
            support: 'native_supported',
            valueType: 'integer',
            minimum: 1,
            maximum,
            default: 2,
            enumValues: [],
            constraints: [],
          },
        },
      },
    },
  };
}

describe('InstalledTaskParameters', () => {
  beforeEach(() => vi.resetAllMocks());

  it('describes the selected installation and ignores a late response for another weight', async () => {
    const first = checkpoint('A', 8),
      second = checkpoint('B', 30);
    let finish: (value: { status: 'found'; task: NativeTaskDescriptionDto }) => void = () => {};
    vi.mocked(listInstalledTasks).mockResolvedValue([first, second]);
    vi.mocked(describeInstalledTask).mockImplementation(async (_, identity) =>
      identity.installationId === 'A'
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : { status: 'found', task: second }
    );
    render(<InstalledTaskParameters sessionIds={['chat']} />, { wrapper: IntlTestWrapper });
    await waitFor(() => expect(describeInstalledTask).toHaveBeenCalledWith('chat', first.identity));
    await userEvent.selectOptions(
      screen.getByLabelText('Installed model and variant'),
      JSON.stringify(second.identity)
    );
    expect(await screen.findByText(/Maximum: 30/)).toBeDefined();
    expect(describeInstalledTask).toHaveBeenCalledWith('chat', second.identity);
    await act(async () => finish({ status: 'found', task: first }));
    expect(screen.queryByText(/Maximum: 8/)).toBeNull();
  });

  it('lists only for an open session, clears stale data and handles a failed request on refresh', async () => {
    vi.mocked(listInstalledTasks)
      .mockRejectedValueOnce(new Error('session closed'))
      .mockResolvedValue([]);
    const view = render(<InstalledTaskParameters sessionIds={[]} />, { wrapper: IntlTestWrapper });
    expect(screen.getByText(/Open a chat/)).toBeDefined();
    expect(listInstalledTasks).not.toHaveBeenCalled();
    view.rerender(<InstalledTaskParameters sessionIds={['chat-1', 'chat-2']} />);
    expect(await screen.findByRole('alert')).toHaveTextContent('session closed');
    await userEvent.click(screen.getByRole('button', { name: 'Refresh model list' }));
    expect(await screen.findByText('No installed model offers this task.')).toBeDefined();
    await userEvent.selectOptions(screen.getByLabelText('Chat session'), 'chat-2');
    await userEvent.selectOptions(screen.getByLabelText('Task'), 'speech_synthesis');
    expect(listInstalledTasks).toHaveBeenLastCalledWith('chat-2', 'speech_synthesis');
    expect(describeInstalledTask).not.toHaveBeenCalled();
  });
});
