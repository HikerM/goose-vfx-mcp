import { IntlProvider } from 'react-intl';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProfileConnectionTestDialog } from '../ProfileConnectionTestDialog';
import { acpListProviderDetails } from '../../../acp/providers';
import {
  testMcpProfileConnection,
  type McpProfileConnectionTestResult,
} from '../../../acp/mcp-platform';

vi.mock('../../../acp/providers', () => ({
  acpListProviderDetails: vi.fn(),
}));

vi.mock('../../../acp/mcp-platform', () => ({
  testMcpProfileConnection: vi.fn(),
}));

const profile = {
  profileId: 'profile-a',
  name: 'Research setup',
  description: 'Browser and fetch MCPs',
  revision: 2,
  archived: false,
  entries: [{ managedMcpId: 'managed-a', ordinal: 0 }],
  createdAtMs: 100,
  updatedAtMs: 200,
};

const configuredProvider = {
  name: 'openai',
  is_configured: true,
  provider_type: 'Builtin' as const,
  metadata: {
    name: 'openai',
    display_name: 'OpenAI',
    description: '',
    default_model: 'gpt-5',
    model_doc_link: '',
    config_keys: [],
    known_models: [
      { name: 'gpt-5', context_limit: 1 },
      { name: 'gpt-5-mini', context_limit: 1 },
    ],
  },
};

const fallbackProvider = {
  name: 'azure',
  is_configured: true,
  provider_type: 'Builtin' as const,
  metadata: {
    name: 'azure',
    display_name: 'Azure',
    description: '',
    default_model: 'gpt-4',
    model_doc_link: '',
    config_keys: [],
    known_models: [{ name: 'gpt-4', context_limit: 1 }],
  },
};

function renderDialog() {
  return render(
    <IntlProvider locale="en">
      <ProfileConnectionTestDialog profile={profile} open onOpenChange={vi.fn()} />
    </IntlProvider>
  );
}

describe('ProfileConnectionTestDialog', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    vi.mocked(acpListProviderDetails).mockResolvedValue([configuredProvider]);
  });

  it('uses only configured provider and model choices, then renders safe stage results', async () => {
    vi.mocked(testMcpProfileConnection).mockResolvedValue({
      passed: true,
      stages: [
        { phase: 'eligibility', status: 'passed', code: 'eligible' },
        { phase: 'mcp_connect_initialize', status: 'passed', code: 'eligible' },
        { phase: 'tool_discovery', status: 'passed', code: 'eligible' },
        { phase: 'model_request', status: 'passed', code: 'eligible' },
        {
          phase: 'tool_visibility',
          status: 'passed',
          code: 'tool_visibility_validated',
        },
      ],
    });
    const user = userEvent.setup();

    renderDialog();

    expect(
      await screen.findByText(
        /discovers its tools, and verifies connectivity to the selected provider and model/
      )
    ).toBeInTheDocument();
    expect(
      screen.getByText(/discovered tool definitions are not sent to the model/)
    ).toBeInTheDocument();
    expect(screen.getByText(/not a tool capability or execution test/)).toBeInTheDocument();
    expect(screen.getByLabelText('Configured provider')).toHaveValue('openai');
    expect(screen.getByLabelText('Configured model')).toHaveValue('gpt-5');

    await user.click(screen.getByRole('button', { name: 'Run connection test' }));
    await waitFor(() =>
      expect(testMcpProfileConnection).toHaveBeenCalledWith({
        profileId: 'profile-a',
        providerId: 'openai',
        modelId: 'gpt-5',
      })
    );

    expect((await screen.findAllByText('Connection test passed')).length).toBeGreaterThan(0);
    expect(screen.getByText('MCP tools discovered')).toBeInTheDocument();
    expect(screen.getByText('Provider/model connectivity')).toBeInTheDocument();
    expect(
      screen.getByText('Tool definitions withheld from model (safe isolation)')
    ).toBeInTheDocument();
    expect(screen.getByText('Code: tool_definitions_withheld_safe_isolation')).toBeInTheDocument();
    expect(screen.queryByText('Code: tool_visibility_validated')).not.toBeInTheDocument();
  });

  it('disables repeat submission and never renders raw connection diagnostics', async () => {
    let rejectRequest!: (reason?: unknown) => void;
    vi.mocked(testMcpProfileConnection).mockImplementation(
      () =>
        new Promise((_, reject) => {
          rejectRequest = reject;
        })
    );
    const user = userEvent.setup();

    renderDialog();
    const run = await screen.findByRole('button', { name: 'Run connection test' });
    await user.click(run);
    await user.click(run);

    expect(testMcpProfileConnection).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Testing connection…' })).toBeDisabled();
    rejectRequest(new Error('Authorization: Bearer super-secret https://private.example.test'));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(
      'No diagnostic details, configuration, or credentials are shown.'
    );
    expect(alert).toHaveTextContent('Code: connection_test_failed');
    expect(alert).not.toHaveTextContent('super-secret');
    expect(alert).not.toHaveTextContent('private.example.test');
  });

  it('does not submit provider or model identifiers removed by the latest configured inventory', async () => {
    vi.mocked(acpListProviderDetails)
      .mockResolvedValueOnce([configuredProvider])
      .mockResolvedValueOnce([]);
    const user = userEvent.setup();

    renderDialog();
    await user.click(await screen.findByRole('button', { name: 'Run connection test' }));

    await waitFor(() => expect(testMcpProfileConnection).not.toHaveBeenCalled());
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'No diagnostic details, configuration, or credentials are shown.'
    );
    expect(screen.getByRole('button', { name: 'Retry connection test' })).toBeDisabled();
  });

  it('rejects DOM-injected provider and model identifiers before they can reach ACP', async () => {
    const user = userEvent.setup();
    renderDialog();
    const providerSelect = await screen.findByLabelText('Configured provider');
    const modelSelect = screen.getByLabelText('Configured model');
    const injectedModel = document.createElement('option');
    injectedModel.value = 'unconfigured-model';
    injectedModel.text = 'Injected model';
    modelSelect.append(injectedModel);
    fireEvent.change(modelSelect, { target: { value: 'unconfigured-model' } });
    expect(screen.getByRole('button', { name: 'Run connection test' })).toBeDisabled();

    const injectedProvider = document.createElement('option');
    injectedProvider.value = 'unconfigured-provider';
    injectedProvider.text = 'Injected provider';
    providerSelect.append(injectedProvider);
    fireEvent.change(providerSelect, { target: { value: 'unconfigured-provider' } });

    expect(screen.getByRole('button', { name: 'Run connection test' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Run connection test' }));
    expect(testMcpProfileConnection).not.toHaveBeenCalled();
  });

  it('renders every repeated phase and reports a later MCP failure', async () => {
    vi.mocked(testMcpProfileConnection).mockResolvedValue({
      passed: false,
      stages: [
        { phase: 'mcp_connect_initialize', status: 'passed', code: 'eligible' },
        {
          phase: 'mcp_connect_initialize',
          status: 'failed',
          code: 'runtime_activation_failed',
        },
        { phase: 'cleanup', status: 'failed', code: 'cleanup_failed' },
      ],
    });
    const user = userEvent.setup();

    renderDialog();
    await user.click(await screen.findByRole('button', { name: 'Run connection test' }));

    expect((await screen.findAllByText('MCP initialization')).length).toBe(2);
    expect(screen.getByText('Code: runtime_activation_failed')).toBeInTheDocument();
    expect(screen.getByText('Connection cleanup')).toBeInTheDocument();
    expect(screen.getByText('Code: cleanup_failed')).toBeInTheDocument();
    expect(screen.getAllByText('Connection test did not complete').length).toBeGreaterThan(0);
  });

  it('keeps a stable runtime activation failure code without rendering raw error text', async () => {
    vi.mocked(testMcpProfileConnection).mockRejectedValue({
      envelope: { code: 'runtime_activation_failed' },
      message: 'Authorization: Bearer secret-value',
    });
    const user = userEvent.setup();

    renderDialog();
    await user.click(await screen.findByRole('button', { name: 'Run connection test' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Code: runtime_activation_failed');
    expect(alert).not.toHaveTextContent('secret-value');
  });

  it('ignores an old connection-test completion after the profile changes', async () => {
    let resolveRequest!: (value: {
      passed: boolean;
      stages: Array<{ phase: 'eligibility'; status: 'passed'; code: 'eligible' }>;
    }) => void;
    vi.mocked(testMcpProfileConnection).mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveRequest = resolve;
        })
    );
    const user = userEvent.setup();
    const rendered = renderDialog();

    await user.click(await screen.findByRole('button', { name: 'Run connection test' }));
    await waitFor(() => expect(testMcpProfileConnection).toHaveBeenCalledTimes(1));
    rendered.rerender(
      <IntlProvider locale="en">
        <ProfileConnectionTestDialog
          profile={{ ...profile, profileId: 'profile-b', name: 'Different profile' }}
          open
          onOpenChange={vi.fn()}
        />
      </IntlProvider>
    );

    resolveRequest({
      passed: true,
      stages: [{ phase: 'eligibility', status: 'passed', code: 'eligible' }],
    });

    await waitFor(() => expect(screen.queryByText('Connection test passed')).not.toBeInTheDocument());
  });

  it('ignores an old connection-test completion after the dialog closes', async () => {
    let resolveRequest!: (value: {
      passed: boolean;
      stages: Array<{ phase: 'eligibility'; status: 'passed'; code: 'eligible' }>;
    }) => void;
    vi.mocked(testMcpProfileConnection).mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveRequest = resolve;
        })
    );
    const user = userEvent.setup();
    const rendered = renderDialog();

    await user.click(await screen.findByRole('button', { name: 'Run connection test' }));
    await waitFor(() => expect(testMcpProfileConnection).toHaveBeenCalledTimes(1));
    rendered.rerender(
      <IntlProvider locale="en">
        <ProfileConnectionTestDialog profile={profile} open={false} onOpenChange={vi.fn()} />
      </IntlProvider>
    );
    resolveRequest({
      passed: true,
      stages: [{ phase: 'eligibility', status: 'passed', code: 'eligible' }],
    });

    await waitFor(() => expect(screen.queryByText('Connection test passed')).not.toBeInTheDocument());
  });

  it('ignores old provider/model results when a newer request is started', async () => {
    const requestPromises: Array<(value: McpProfileConnectionTestResult) => void> = [];
    vi.mocked(testMcpProfileConnection).mockImplementation(() => {
      return new Promise((resolve) => {
        requestPromises.push(resolve);
      });
    });
    vi.mocked(acpListProviderDetails).mockResolvedValue([configuredProvider, fallbackProvider]);
    const user = userEvent.setup();

    renderDialog();
    const run = await screen.findByRole('button', { name: 'Run connection test' });
    await user.click(run);
    await waitFor(() => expect(testMcpProfileConnection).toHaveBeenCalledTimes(1));

    const providerSelect = screen.getByLabelText<HTMLSelectElement>('Configured provider');
    const modelSelect = screen.getByLabelText<HTMLSelectElement>('Configured model');
    providerSelect.disabled = false;
    modelSelect.disabled = false;
    fireEvent.change(providerSelect, { target: { value: 'azure' } });
    fireEvent.change(modelSelect, { target: { value: 'gpt-4' } });
    await user.click(screen.getByRole('button', { name: 'Run connection test' }));

    await waitFor(() => expect(testMcpProfileConnection).toHaveBeenCalledTimes(2));
    requestPromises[0]({
      passed: false,
      stages: [{ phase: 'eligibility', status: 'failed', code: 'runtime_activation_failed' }],
    });
    await waitFor(() => {
      expect(screen.queryByText('Connection test did not complete')).not.toBeInTheDocument();
      expect(screen.queryByText('Code: runtime_activation_failed')).not.toBeInTheDocument();
    });

    requestPromises[1]({
      passed: true,
      stages: [{ phase: 'eligibility', status: 'passed', code: 'eligible' }],
    });
    expect((await screen.findAllByRole('heading', { name: 'Connection test passed' })).length).toBe(1);
    expect(screen.getByText('Code: eligible')).toBeInTheDocument();
  });
});
