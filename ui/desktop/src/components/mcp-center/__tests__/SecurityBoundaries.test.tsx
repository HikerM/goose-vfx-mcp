import { IntlProvider } from 'react-intl';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpSourcesPolicyState } from '@aaif/goose-sdk';
import { ManualTab } from '../ManualTab';
import { SourcesPolicyTab } from '../SourcesPolicyTab';
import { listManualStdioSources } from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return { ...actual, listManualStdioSources: vi.fn() };
});

const policy: McpSourcesPolicyState = {
  policy: {
    targetPlatform: 'windows',
    targetArchitecture: 'x86_64',
    developmentMode: false,
    dockerAllowed: false,
    recovery: 'none',
  },
  sources: [],
};

function withIntl(node: React.ReactNode) {
  return render(<IntlProvider locale="en">{node}</IntlProvider>);
}

describe('MCP Center security boundaries', () => {
  beforeEach(() => {
    vi.mocked(listManualStdioSources).mockResolvedValue({
      provider: 'available',
      items: [
        {
          sourceId: 'approved-source',
          displayName: 'Approved source',
          publisherName: 'Publisher',
          trustTier: 'official',
          compatibility: 'compatible',
        },
      ],
    });
  });

  it('does not render HTTP token, password, or header secret inputs', async () => {
    withIntl(<ManualTab onTaskCreated={vi.fn()} />);
    await screen.findByRole('tab', { name: 'Remote HTTP' });

    expect(screen.queryByLabelText(/token|password|header/i)).not.toBeInTheDocument();
    expect(screen.getByLabelText('HTTPS endpoint')).toHaveAttribute('type', 'url');
    expect(screen.getByRole('combobox', { name: 'Authentication' })).toHaveValue('none');
  });

  it('offers approved stdio source selection without command or argument inputs', async () => {
    const user = userEvent.setup();
    withIntl(<ManualTab onTaskCreated={vi.fn()} />);
    await user.click(await screen.findByRole('tab', { name: 'Approved stdio provider' }));
    await screen.findByRole('radio', { name: /Approved source/i });

    expect(
      screen.queryByLabelText(/command|arguments|environment|working directory/i)
    ).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('renders Source & Policy as read-only data', () => {
    withIntl(<SourcesPolicyTab state={policy} />);

    expect(screen.getByText('Read only')).toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });
});
