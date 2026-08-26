import { IntlProvider } from 'react-intl';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router-dom';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpManagedSummary, McpPlatformErrorEnvelope } from '@hikerm/lumina-sdk';
import { ProfilesTab, redactProfileApplyReview } from '../ProfilesTab';
import {
  McpPlatformServiceError,
  archiveMcpProfile,
  confirmMcpProfileApply,
  createMcpProfile,
  createMcpProfileApplyPlan,
  createMcpProfileDraft,
  getMcpProfile,
  listManagedMcps,
  listMcpProfiles,
  mapMcpPlatformError,
  recommendMcpProfileModels,
  restoreMcpProfile,
  updateMcpProfile,
} from '../../../acp/mcp-platform';
import { acpChatSessionActions } from '../../../acp/chatSessionStore';
import { acpGetSessionListItem } from '../../../acp/sessions';
import { AppEvents } from '../../../constants/events';
import { createSession } from '../../../sessions';

const currentChatSession = vi.hoisted(() => ({ sessionId: 'session-current' }));

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    archiveMcpProfile: vi.fn(),
    confirmMcpProfileApply: vi.fn(),
    createMcpProfile: vi.fn(),
    createMcpProfileApplyPlan: vi.fn(),
    createMcpProfileDraft: vi.fn(),
    getMcpProfile: vi.fn(),
    listManagedMcps: vi.fn(),
    listMcpProfiles: vi.fn(),
    recommendMcpProfileModels: vi.fn(),
    restoreMcpProfile: vi.fn(),
    updateMcpProfile: vi.fn(),
  };
});

vi.mock('../../../sessions', () => ({
  createSession: vi.fn(),
}));

vi.mock('../../../acp/sessions', () => ({
  acpGetSessionListItem: vi.fn(),
}));

vi.mock('../../../contexts/ChatContext', () => ({
  useChatContext: () => ({
    chat: {
      sessionId: currentChatSession.sessionId,
      name: 'Current session',
      messages: [],
      recipe: null,
      recipeParameterValues: null,
    },
    setChat: vi.fn(),
    resetChat: vi.fn(),
    hasActiveSession: true,
    setRecipe: vi.fn(),
    clearRecipe: vi.fn(),
    contextKey: `pair-${currentChatSession.sessionId}`,
  }),
}));

vi.mock('../../ConfigContext', () => ({
  useConfig: () => ({
    extensionsList: [],
  }),
}));

const managed = (overrides: Partial<McpManagedSummary> = {}): McpManagedSummary => ({
  managedMcpId: 'managed-a',
  mcpId: 'Fetch',
  installationScope: 'user',
  registration: 'registered',
  installation: 'installed',
  runtime: 'running',
  health: 'healthy',
  defaultEnabled: false,
  revision: 1,
  updatedAtMs: 100,
  distributionAdapter: 'archive',
  activeVersion: null,
  availableVersion: null,
  availableManifestDigest: null,
  currentTask: null,
  recoveryRequired: false,
  eligibility: {
    update: false,
    repair: false,
    uninstall: false,
    reason: 'eligible',
    updateReason: 'eligible',
    repairReason: 'eligible',
    uninstallReason: 'eligible',
  },
  nextAction: 'none',
  phaseCapabilities: {
    sessionEnablement: 'not_available_in_this_phase',
    toolPolicy: 'not_available_in_this_phase',
    profiles: 'not_available_in_this_phase',
    modelSuggestions: 'not_available_in_this_phase',
  },
  ...overrides,
});

const profile = (overrides: Record<string, unknown> = {}) => ({
  profileId: 'profile-a',
  name: 'Research setup',
  description: 'Browser and fetch MCPs',
  revision: 2,
  archived: false,
  entries: [{ managedMcpId: 'managed-a', ordinal: 0 }],
  createdAtMs: 100,
  updatedAtMs: 200,
  ...overrides,
});

const profileDetail = (overrides: Record<string, unknown> = {}) => ({
  profile: profile(),
  history: [
    { revision: 2, operation: 'update', actor: 'user', createdAtMs: 200 },
    { revision: 1, operation: 'create', actor: 'user', createdAtMs: 100 },
  ],
  ...overrides,
});

const applyPlan = (overrides: Record<string, unknown> = {}) => ({
  planId: 'plan-profile-1',
  profileId: 'profile-a',
  profileRevision: 2,
  mergePolicy: 'replace_managed_only',
  entries: [
    {
      managedMcpId: 'managed-a',
      mcpId: 'fetch',
      name: 'Fetch',
      version: '1.0.0',
      health: 'healthy',
      authReady: true,
      policyReady: true,
      readiness: 'complete',
    },
  ],
  expiresAtMs: Date.now() + 60_000,
  confirmation: {
    confirmationToken: 'confirmation-token',
  },
  ...overrides,
});

const profileApplicationToken = (overrides: Record<string, unknown> = {}) => ({
  token: 'profile-application-token',
  planId: 'plan-profile-1',
  profileId: 'profile-a',
  profileRevision: 2,
  expiresAtMs: Date.now() + 60_000,
  ...overrides,
});

const profileDraft = {
  locale: 'en',
  name: 'Draft research setup',
  description: 'Drafted from natural language',
  entries: [{ managedMcpId: 'managed-a', ordinal: 0 }],
  candidates: [
    {
      managedMcpId: 'managed-a',
      mcpId: 'fetch',
      name: 'Fetch',
      description: 'HTTP MCP',
      confidence: 0.91,
      reasonCode: 'keyword_match',
    },
  ],
  unresolvedTerms: ['spreadsheet sync'],
  lowConfidence: true,
  persisted: false,
};

const credentialCases = [
  {
    label: 'ready',
    status: 'ready',
    badge: 'Authentication ready',
    badgeAria: 'Authentication is ready',
    title: 'Authentication is ready',
    detailRole: 'status',
    showsRecoveryCta: false,
  },
  {
    label: 'unconfigured',
    status: 'unconfigured',
    badge: 'Authentication required',
    badgeAria: 'Authentication must be completed',
    title: 'Authentication must be completed',
    detailRole: 'alert',
    showsRecoveryCta: true,
  },
  {
    label: 're_registration_required',
    status: 're_registration_required',
    badge: 'Re-authentication required',
    badgeAria: 'Authentication must be completed again',
    title: 'Authentication must be completed again',
    detailRole: 'alert',
    showsRecoveryCta: true,
  },
  {
    label: 'trusted_state_conflict',
    status: 'trusted_state_conflict',
    badge: 'Authentication conflict',
    badgeAria: 'Authentication state conflict requires attention',
    title: 'Authentication state conflict',
    detailRole: 'alert',
    showsRecoveryCta: true,
  },
  {
    label: 'temporarily_unavailable',
    status: 'temporarily_unavailable',
    badge: 'Authentication unavailable',
    badgeAria: 'Authentication is temporarily unavailable',
    title: 'Authentication is temporarily unavailable',
    detailRole: 'status',
    showsRecoveryCta: true,
  },
] as const;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

function phaseUnavailableError(operation: string) {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'not_implemented_for_phase',
    message: `${operation} is not available in this phase`,
    retryable: false,
    correlationId: `${operation}-ref`,
    details: { type: 'phase_unavailable', operation, phase: '4B' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function revisionConflictError() {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'revision_conflict',
    message: 'revision changed',
    retryable: true,
    correlationId: 'revision-ref',
    details: { type: 'revision_conflict' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function recordMissingError() {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'not_found',
    message: 'The requested MCP platform record was not found.',
    retryable: false,
    correlationId: 'record-missing-ref',
    details: { type: 'record_missing' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function repositoryUnavailableError() {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'repository_unavailable',
    message: 'The MCP platform repository is temporarily unavailable.',
    retryable: true,
    correlationId: 'repository-ref',
    details: { type: 'repository_temporarily_unavailable' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function planExpiredError() {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'plan_expired',
    message: 'plan expired',
    retryable: false,
    correlationId: 'plan-expired-ref',
    details: { type: 'plan_expired' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function requestRejectedError() {
  const envelope: McpPlatformErrorEnvelope = {
    code: 'invalid_request',
    message: 'The MCP platform request is invalid.',
    retryable: false,
    correlationId: 'request-rejected-ref',
    details: { type: 'request_rejected' } as const,
  };
  return new McpPlatformServiceError(envelope, mapMcpPlatformError(envelope));
}

function renderProfilesTab() {
  return render(
    <MemoryRouter>
      <IntlProvider locale="en">
        <ProfilesTab />
      </IntlProvider>
    </MemoryRouter>
  );
}

const currentSession = (overrides: Record<string, unknown> = {}) => ({
  id: currentChatSession.sessionId,
  name: 'Current session',
  message_count: 1,
  created_at: '2026-07-24T00:00:00.000Z',
  updated_at: '2026-07-24T00:00:00.000Z',
  working_dir: '/workspace/current',
  extension_data: { active: [], installed: [] },
  ...overrides,
});

const currentSessionListItem = (overrides: Record<string, unknown> = {}) => ({
  id: currentChatSession.sessionId,
  name: 'Current session',
  workingDir: '/workspace/current',
  updatedAt: '2026-07-24T00:00:00.000Z',
  messageCount: 1,
  createdAt: '2026-07-24T00:00:00.000Z',
  ...overrides,
});

describe('ProfilesTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    class ResizeObserverMock {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal('ResizeObserver', ResizeObserverMock);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [], nextCursor: null });
    acpChatSessionActions.deleteSnapshot(currentChatSession.sessionId);
    acpChatSessionActions.setSessionMetadata(
      currentChatSession.sessionId,
      currentSession() as never
    );
    vi.mocked(acpGetSessionListItem).mockResolvedValue(currentSessionListItem() as never);
    vi.mocked(createSession).mockResolvedValue({
      id: 'session-apply',
      name: 'New Chat',
      message_count: 0,
      created_at: '2026-07-24T00:00:00.000Z',
      updated_at: '2026-07-24T00:00:00.000Z',
      working_dir: '/tmp',
      extension_data: { active: [], installed: [] },
    } as never);
  });

  it.each(credentialCases)(
    'renders safe profile credential UI for $label',
    async ({ status, badge, badgeAria, title, detailRole, showsRecoveryCta }) => {
      vi.mocked(listManagedMcps).mockResolvedValue({
        items: [managed({ credentialStatus: status })],
        nextCursor: null,
      });
      vi.mocked(listMcpProfiles).mockResolvedValue({
        items: [profile({ credentialStatus: status })],
      });
      vi.mocked(getMcpProfile).mockResolvedValue(
        profileDetail({ profile: profile({ credentialStatus: status }) })
      );
      const user = userEvent.setup();

      renderProfilesTab();

      const profileButton = await screen.findByRole('button', { name: /Research setup/i });
      expect(profileButton).toHaveTextContent(badge);
      await user.click(profileButton);

      expect(await screen.findByText(title)).toBeInTheDocument();
      expect(screen.getByText('Authentication status')).toBeInTheDocument();
      expect(screen.getAllByLabelText(new RegExp(badgeAria, 'i')).length).toBeGreaterThan(0);
      expect(screen.getByText(title).closest(`[role="${detailRole}"]`)).not.toBeNull();
      if (showsRecoveryCta) {
        expect(screen.getByRole('button', { name: 'Refresh status' })).toBeInTheDocument();
        expect(
          screen.getByRole('button', { name: 'Authentication entry not yet available' })
        ).toBeDisabled();
      } else {
        expect(screen.queryByRole('button', { name: 'Refresh status' })).not.toBeInTheDocument();
        expect(
          screen.queryByRole('button', { name: 'Authentication entry not yet available' })
        ).not.toBeInTheDocument();
      }

      await user.click(screen.getByRole('button', { name: 'Edit' }));
      expect(await screen.findByText(title)).toBeInTheDocument();
      expect(
        screen.queryByRole('button', { name: 'Authentication entry not yet available' })
      ).not.toBeInTheDocument();
    }
  );

  it('fails closed when profile credential status is missing or unknown', async () => {
    const unknownProfile = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      credentialStatus: 'credential_leaked',
    });
    vi.mocked(listMcpProfiles).mockResolvedValue({
      items: [profile(), unknownProfile],
    });
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-b' ? profileDetail({ profile: unknownProfile }) : profileDetail()
    );
    const user = userEvent.setup();

    renderProfilesTab();

    expect(await screen.findByRole('button', { name: /Research setup/i })).toHaveTextContent(
      'Authentication unconfirmed'
    );
    const unknownButton = screen.getByRole('button', { name: /Browser setup/i });
    expect(unknownButton).toHaveTextContent('Authentication unconfirmed');
    expect(unknownButton).not.toHaveTextContent('Authentication ready');

    await user.click(unknownButton);

    expect(await screen.findByText('Authentication status not provided')).toBeInTheDocument();
    expect(screen.queryByText('Authentication is ready')).not.toBeInTheDocument();
  });

  it('renders revision history without exposing actor details', async () => {
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: /Research setup/i }));
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Research setup' })
    ).toBeInTheDocument();
    expect(screen.getByText('Revision history')).toBeInTheDocument();
    expect(screen.queryByText(/by user/i)).not.toBeInTheDocument();
    expect(screen.queryByTitle('user')).not.toBeInTheDocument();
  });

  it('treats empty inventory with a successful profile list as a normal empty state', async () => {
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [] });

    renderProfilesTab();

    expect(screen.getAllByText('Loading profiles').length).toBeGreaterThan(0);
    expect((await screen.findAllByText('No saved profiles')).length).toBeGreaterThan(0);
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
  });

  it('shows unavailable only when the profile RPC returns an explicit phase error', async () => {
    vi.mocked(listMcpProfiles).mockRejectedValue(
      phaseUnavailableError('lumina.mcpProfileList_unstable')
    );

    renderProfilesTab();

    expect(
      (await screen.findAllByText('Profiles are not available in this phase')).length
    ).toBeGreaterThan(0);
    expect(screen.getAllByText('lumina.mcpProfileList_unstable-ref').length).toBeGreaterThan(0);
  });

  it('shows a retryable error instead of unavailable for unknown profile list failures and recovers on retry', async () => {
    vi.mocked(listMcpProfiles)
      .mockRejectedValueOnce(new Error('socket down'))
      .mockResolvedValueOnce({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    const user = userEvent.setup();

    renderProfilesTab();

    expect((await screen.findAllByText('MCP Center is unavailable')).length).toBeGreaterThan(0);
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
    await user.click(screen.getAllByRole('button', { name: 'Retry' })[0]);

    const selectedProfileButton = await screen.findByRole('button', { name: /Research setup/i });
    await user.click(selectedProfileButton);
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Research setup' })
    ).toBeInTheDocument();
    expect(selectedProfileButton).toHaveAttribute('aria-pressed', 'true');
  });

  it('keeps repository unavailable in the retryable recovery path instead of surfacing phase unavailable', async () => {
    vi.mocked(listMcpProfiles).mockRejectedValue(repositoryUnavailableError());

    renderProfilesTab();

    expect(
      (await screen.findAllByText('MCP data is temporarily unavailable')).length
    ).toBeGreaterThan(0);
    expect(screen.getAllByRole('button', { name: 'Retry' }).length).toBeGreaterThan(0);
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
  });

  it('treats formal request-rejected profile list failures as blocking errors, not phase gating', async () => {
    vi.mocked(listMcpProfiles).mockRejectedValue(requestRejectedError());

    renderProfilesTab();

    expect(
      (await screen.findAllByText('MCP operation could not be completed')).length
    ).toBeGreaterThan(0);
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
    expect(screen.getAllByText('The MCP platform request is invalid.').length).toBeGreaterThan(0);
  });

  it('shows a generic safe error instead of phase unavailable when the boundary rejects an inherited attack', async () => {
    vi.mocked(listMcpProfiles).mockRejectedValue(
      new Error('The MCP Platform request could not be completed.')
    );

    renderProfilesTab();

    expect((await screen.findAllByText('MCP Center is unavailable')).length).toBeGreaterThan(0);
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
    expect(
      screen.getAllByText('The MCP Platform request could not be completed.').length
    ).toBeGreaterThan(0);
  });

  it('creates a profile after validation and reloads the saved detail', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles)
      .mockResolvedValueOnce({ items: [] })
      .mockResolvedValueOnce({ items: [profile()] });
    vi.mocked(createMcpProfile).mockResolvedValue(profile());
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findAllByRole('button', { name: 'New profile' });
    await user.click(screen.getAllByRole('button', { name: 'New profile' })[0]);
    await user.click(screen.getByRole('button', { name: 'Save profile' }));
    expect(await screen.findByText('Enter a profile name before saving.')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Profile name'), '  Research setup  ');
    await user.type(screen.getByLabelText('Description'), '  Browser and fetch MCPs  ');
    await user.click(screen.getByRole('checkbox', { name: /Fetch/i }));
    await user.click(screen.getByRole('button', { name: 'Save profile' }));

    await waitFor(() =>
      expect(createMcpProfile).toHaveBeenCalledWith({
        name: 'Research setup',
        description: 'Browser and fetch MCPs',
        managedMcpIds: ['managed-a'],
      })
    );
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Research setup' })
    ).toBeInTheDocument();
  });

  it('shows revision conflict recovery and confirms before discarding unsaved edits', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(updateMcpProfile).mockRejectedValue(revisionConflictError());
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Updated setup');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(await screen.findByText('MCP state changed')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(await screen.findByText('Discard unsaved profile changes?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Discard changes' }));

    await waitFor(() => expect(screen.queryByLabelText('Profile name')).not.toBeInTheDocument());
  });

  it('shows record-missing detail recovery without downgrading the page into phase unavailable', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockRejectedValue(recordMissingError());

    renderProfilesTab();

    expect(
      await screen.findByText('The requested MCP platform record was not found.')
    ).toBeInTheDocument();
    expect(screen.queryByText('Profiles are not available in this phase')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
  });

  it('ignores a stale save failure after a newer save for the same profile succeeds', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const updatedProfileA = profile({
      name: 'Research setup v2',
      revision: 3,
      updatedAtMs: 400,
    });
    const firstSave = deferred<ReturnType<typeof profile>>();
    let currentProfileA = profile();

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockImplementation(async () => ({
      items: [currentProfileA, profileB],
    }));
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail({ profile: currentProfileA })
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(updateMcpProfile)
      .mockImplementationOnce(() => firstSave.promise)
      .mockImplementationOnce(async () => {
        currentProfileA = updatedProfileA;
        return updatedProfileA;
      });
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Research setup v1');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    expect(await screen.findByText('Discard unsaved profile changes?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Discard changes' }));
    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });

    await user.click(screen.getByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Research setup v2');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(
      await screen.findByRole('heading', { level: 2, name: 'Research setup v2' })
    ).toBeInTheDocument();
    expect(updateMcpProfile).toHaveBeenCalledTimes(2);

    firstSave.reject(revisionConflictError());

    await waitFor(() =>
      expect(
        screen.getByRole('heading', { level: 2, name: 'Research setup v2' })
      ).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();
    expect(screen.queryByText('Profile action issues')).not.toBeInTheDocument();
  });

  it('scopes a late save failure to the original profile after the user switches away', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const saveResult = deferred<ReturnType<typeof profile>>();

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile(), profileB] });
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail()
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(updateMcpProfile).mockImplementation(() => saveResult.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Updated setup');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    expect(await screen.findByText('Discard unsaved profile changes?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Discard changes' }));
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();

    saveResult.reject(revisionConflictError());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    expect(await screen.findByText('MCP state changed')).toBeInTheDocument();
  });

  it('keeps a hidden profile failure diagnosable without restoring a ghost selection', async () => {
    const archivedProfileA = profile({
      profileId: 'profile-a',
      name: 'Archived setup',
      description: 'Hidden by default',
      archived: true,
    });
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const saveResult = deferred<ReturnType<typeof profile>>();

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockImplementation(async (includeArchived?: boolean) => ({
      items: includeArchived ? [archivedProfileA, profileB] : [profileB],
    }));
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail({
            profile: archivedProfileA,
            history: [{ revision: 2, operation: 'update', actor: 'user', createdAtMs: 200 }],
          })
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(updateMcpProfile).mockImplementation(() => saveResult.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });
    await user.click(screen.getByRole('checkbox', { name: 'Show archived' }));
    await user.click(await screen.findByRole('button', { name: /Archived setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Archived setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Archived setup updated');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    expect(await screen.findByText('Discard unsaved profile changes?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Discard changes' }));
    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });

    saveResult.reject(revisionConflictError());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    await user.click(screen.getByRole('checkbox', { name: 'Show archived' }));

    expect(await screen.findByText('Profile action issues')).toBeInTheDocument();
    expect(screen.getByText('Profile: Archived setup')).toBeInTheDocument();
    expect(screen.getByText('MCP state changed')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Archived setup/i })).not.toBeInTheDocument();
    expect(
      screen.queryByRole('heading', { level: 2, name: 'Archived setup' })
    ).not.toBeInTheDocument();
  });

  it('archives and restores through explicit confirmations', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(archiveMcpProfile).mockResolvedValue(profile({ archived: true }));
    vi.mocked(restoreMcpProfile).mockResolvedValue(profile());
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Archive' }));
    expect(await screen.findByText('Archive this profile?')).toBeInTheDocument();
    expect(
      screen.getByText(
        'This action will not uninstall, delete, enable, or apply MCPs to any session.'
      )
    ).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Archive profile' }));
    await waitFor(() =>
      expect(archiveMcpProfile).toHaveBeenCalledWith({
        profileId: 'profile-a',
        expectedRevision: 2,
      })
    );

    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    expect(await screen.findByText('Restore this profile revision?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await waitFor(() =>
      expect(restoreMcpProfile).toHaveBeenCalledWith({
        profileId: 'profile-a',
        sourceRevision: 1,
        expectedRevision: 2,
      })
    );
  });

  it('ignores a stale archive failure after a newer archive for the same profile succeeds', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const firstArchive = deferred<ReturnType<typeof profile>>();
    let archived = false;

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockImplementation(async () => ({
      items: archived ? [profileB] : [profile(), profileB],
    }));
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail()
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(archiveMcpProfile)
      .mockImplementationOnce(() => firstArchive.promise)
      .mockImplementationOnce(async () => {
        archived = true;
        return profile({ archived: true, revision: 3 });
      });
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Archive' }));
    await user.click(screen.getByRole('button', { name: 'Archive profile' }));
    await waitFor(() => expect(archiveMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });
    await user.click(screen.getByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Archive' }));
    await user.click(screen.getByRole('button', { name: 'Archive profile' }));

    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();
    expect(archiveMcpProfile).toHaveBeenCalledTimes(2);

    firstArchive.reject(revisionConflictError());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();
    expect(screen.queryByText('Profile action issues')).not.toBeInTheDocument();
  });

  it('keeps an archive failure scoped to the archived profile when another profile is selected', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const archiveResult = deferred<ReturnType<typeof profile>>();

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile(), profileB] });
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail()
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(archiveMcpProfile).mockImplementation(() => archiveResult.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Archive' }));
    await user.click(screen.getByRole('button', { name: 'Archive profile' }));
    await waitFor(() => expect(archiveMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();

    archiveResult.reject(revisionConflictError());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    expect(await screen.findByText('MCP state changed')).toBeInTheDocument();
  });

  it('keeps a restore failure scoped to the original profile when another profile is selected', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const restoreResult = deferred<ReturnType<typeof profile>>();

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile(), profileB] });
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail()
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(restoreMcpProfile).mockImplementation(() => restoreResult.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await waitFor(() => expect(restoreMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();

    restoreResult.reject(revisionConflictError());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Research setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    expect(await screen.findByText('MCP state changed')).toBeInTheDocument();
  });

  it('ignores a stale restore failure after a newer restore for the same profile succeeds', async () => {
    const archivedProfileA = profile({
      profileId: 'profile-a',
      name: 'Archived setup',
      description: 'Hidden by default',
      archived: true,
    });
    const restoredProfileA = profile({
      profileId: 'profile-a',
      name: 'Archived setup restored',
      description: 'Hidden by default',
      archived: false,
      revision: 3,
      updatedAtMs: 300,
    });
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const firstRestore = deferred<ReturnType<typeof profile>>();
    let currentProfileA = archivedProfileA;

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles).mockImplementation(async (includeArchived?: boolean) => ({
      items: includeArchived ? [currentProfileA, profileB] : [profileB],
    }));
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail({
            profile: currentProfileA,
            history: [
              {
                revision: currentProfileA.revision,
                operation: 'update',
                actor: 'user',
                createdAtMs: 200,
              },
              { revision: 1, operation: 'create', actor: 'user', createdAtMs: 100 },
            ],
          })
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(restoreMcpProfile)
      .mockImplementationOnce(() => firstRestore.promise)
      .mockImplementationOnce(async () => {
        currentProfileA = restoredProfileA;
        return restoredProfileA;
      });
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });
    await user.click(screen.getByRole('checkbox', { name: 'Show archived' }));
    await user.click(await screen.findByRole('button', { name: /Archived setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Archived setup' });
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await waitFor(() => expect(restoreMcpProfile).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /Browser setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Browser setup' });
    await user.click(screen.getByRole('button', { name: /Archived setup/i }));
    await screen.findByRole('heading', { level: 2, name: 'Archived setup' });
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));
    await user.click(screen.getByRole('button', { name: 'Restore revision' }));

    expect(
      await screen.findByRole('heading', { level: 2, name: 'Archived setup restored' })
    ).toBeInTheDocument();
    expect(restoreMcpProfile).toHaveBeenCalledTimes(2);

    firstRestore.reject(revisionConflictError());

    await waitFor(() =>
      expect(
        screen.getByRole('heading', { level: 2, name: 'Archived setup restored' })
      ).toBeInTheDocument()
    );
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();
    expect(screen.queryByText('Profile action issues')).not.toBeInTheDocument();
  });

  it('keeps the newer selection when a save-triggered reload for another profile resolves late', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const updatedProfileA = profile({ name: 'Research setup updated', revision: 3 });
    const reloadList = deferred<{ items: Array<ReturnType<typeof profile>> }>();
    const staleReloadDetail = deferred<ReturnType<typeof profileDetail>>();
    let profileADetailCalls = 0;

    vi.mocked(listManagedMcps).mockResolvedValue({
      items: [managed(), managed({ managedMcpId: 'managed-b', mcpId: 'Browser' })],
      nextCursor: null,
    });
    vi.mocked(listMcpProfiles)
      .mockResolvedValueOnce({ items: [profile(), profileB] })
      .mockImplementationOnce(() => reloadList.promise);
    vi.mocked(updateMcpProfile).mockResolvedValue(updatedProfileA);
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) => {
      if (profileId === 'profile-a') {
        profileADetailCalls += 1;
        return profileADetailCalls === 1 ? profileDetail() : staleReloadDetail.promise;
      }
      return profileDetail({
        profile: profileB,
        history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
      });
    });
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Profile name'));
    await user.type(screen.getByLabelText('Profile name'), 'Research setup updated');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMcpProfile).toHaveBeenCalledTimes(1));

    const browserButton = await screen.findByRole('button', { name: /Browser setup/i });
    await user.click(browserButton);
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();

    reloadList.resolve({ items: [updatedProfileA, profileB] });
    if (profileADetailCalls > 1) {
      staleReloadDetail.resolve(profileDetail({ profile: updatedProfileA }));
    }

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(browserButton).toHaveAttribute('aria-pressed', 'true');
    expect(
      screen.queryByRole('heading', { level: 2, name: 'Research setup updated' })
    ).not.toBeInTheDocument();
  });

  it('reselects a visible profile after archiving the selected item out of the filtered list', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const reloadList = deferred<{ items: Array<ReturnType<typeof profile>> }>();

    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles)
      .mockResolvedValueOnce({ items: [profile(), profileB] })
      .mockImplementationOnce(() => reloadList.promise);
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) =>
      profileId === 'profile-a'
        ? profileDetail()
        : profileDetail({
            profile: profileB,
            history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
          })
    );
    vi.mocked(archiveMcpProfile).mockResolvedValue(profile({ archived: true, revision: 3 }));
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findByRole('heading', { level: 2, name: 'Research setup' });
    await user.click(screen.getByRole('button', { name: 'Archive' }));
    await user.click(screen.getByRole('button', { name: 'Archive profile' }));
    await waitFor(() => expect(archiveMcpProfile).toHaveBeenCalledTimes(1));

    reloadList.resolve({ items: [profileB] });

    const browserHeading = await screen.findByRole('heading', {
      level: 2,
      name: 'Browser setup',
    });
    expect(browserHeading).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Browser setup/i })).toHaveAttribute(
      'aria-pressed',
      'true'
    );
    expect(screen.queryByRole('button', { name: /Research setup/i })).not.toBeInTheDocument();
    expect(
      screen.queryByRole('heading', { level: 2, name: 'Research setup' })
    ).not.toBeInTheDocument();
  });

  it('ignores a stale detail retry result after the user selects a different profile', async () => {
    const profileB = profile({
      profileId: 'profile-b',
      name: 'Browser setup',
      description: 'Browser-only MCPs',
      revision: 3,
      entries: [{ managedMcpId: 'managed-b', ordinal: 0 }],
      updatedAtMs: 300,
    });
    const retryDetail = deferred<ReturnType<typeof profileDetail>>();
    let profileADetailCalls = 0;

    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile(), profileB] });
    vi.mocked(getMcpProfile).mockImplementation(async (profileId: string) => {
      if (profileId === 'profile-a') {
        profileADetailCalls += 1;
        if (profileADetailCalls === 1) {
          throw new Error('socket down');
        }
        return retryDetail.promise;
      }
      return profileDetail({
        profile: profileB,
        history: [{ revision: 3, operation: 'create', actor: 'user', createdAtMs: 300 }],
      });
    });
    const user = userEvent.setup();

    renderProfilesTab();

    expect(await screen.findAllByText('MCP Center is unavailable')).not.toHaveLength(0);
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    await user.click(await screen.findByRole('button', { name: /Browser setup/i }));
    expect(
      await screen.findByRole('heading', { level: 2, name: 'Browser setup' })
    ).toBeInTheDocument();

    retryDetail.resolve(profileDetail());

    await waitFor(() =>
      expect(screen.getByRole('heading', { level: 2, name: 'Browser setup' })).toBeInTheDocument()
    );
    expect(
      screen.queryByRole('heading', { level: 2, name: 'Research setup' })
    ).not.toBeInTheDocument();
  });

  it('accepts only the latest archived-filter list response when returns arrive out of order', async () => {
    const archivedProfile = profile({
      profileId: 'profile-archived',
      name: 'Archived setup',
      archived: true,
      description: 'Hidden by default',
    });
    const firstList = deferred<{ items: Array<ReturnType<typeof profile>> }>();
    const secondList = deferred<{ items: Array<ReturnType<typeof profile>> }>();

    vi.mocked(listManagedMcps).mockResolvedValue({ items: [], nextCursor: null });
    vi.mocked(listMcpProfiles)
      .mockImplementationOnce(() => firstList.promise)
      .mockImplementationOnce(() => secondList.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(screen.getByRole('checkbox', { name: 'Show archived' }));
    secondList.resolve({ items: [archivedProfile] });
    expect(await screen.findByRole('button', { name: /Archived setup/i })).toBeInTheDocument();

    firstList.resolve({ items: [profile()] });

    await waitFor(() =>
      expect(screen.getByRole('button', { name: /Archived setup/i })).toBeInTheDocument()
    );
    expect(screen.queryByRole('button', { name: /Research setup/i })).not.toBeInTheDocument();
  });

  it('fills the form from a draft without saving and degrades model suggestions only after a real phase error', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [] });
    vi.mocked(createMcpProfileDraft).mockResolvedValue(profileDraft);
    vi.mocked(recommendMcpProfileModels).mockRejectedValue(
      phaseUnavailableError('lumina.mcpProfileModelRecommend_unstable')
    );
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findAllByRole('button', { name: 'New profile' });
    await user.type(
      screen.getByLabelText('Describe your work'),
      'Need browser automation and HTTP fetches'
    );
    await user.click(screen.getByRole('button', { name: 'Draft profile' }));
    expect(await screen.findByText('Draft research setup')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Fill form from draft' }));

    expect(await screen.findByDisplayValue('Draft research setup')).toBeInTheDocument();
    expect(createMcpProfile).not.toHaveBeenCalled();
    expect(updateMcpProfile).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Recommend models' }));
    expect(
      await screen.findByText('Model recommendations are not available in this phase')
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Recommend models' })).not.toBeInTheDocument();
  });

  it('sends provider filter values to model recommendations without trimming them', async () => {
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [] });
    vi.mocked(recommendMcpProfileModels).mockResolvedValue({
      candidates: [],
      inventoryOnly: false,
      mutated: false,
    });
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findAllByRole('button', { name: 'New profile' });
    await user.type(screen.getByLabelText('Describe your work'), 'Need MCP suggestions');
    await user.type(screen.getByLabelText('Provider IDs (optional)'), 'openai, openaі');
    await user.click(screen.getByRole('button', { name: 'Recommend models' }));

    await waitFor(() =>
      expect(recommendMcpProfileModels).toHaveBeenCalledWith('Need MCP suggestions', [
        'openai',
        ' openaі',
      ])
    );
  });

  it('sanitizes suggestion errors before rendering them', async () => {
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [] });
    vi.mocked(createMcpProfileDraft).mockRejectedValue(
      new Error('Authorization: Bearer top-secret at https://private.example.test path=/tmp')
    );
    const user = userEvent.setup();

    renderProfilesTab();

    await screen.findAllByRole('button', { name: 'New profile' });
    await user.type(screen.getByLabelText('Describe your work'), 'Need MCP suggestions');
    await user.click(screen.getByRole('button', { name: 'Draft profile' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The MCP Platform request could not be completed.');
    expect(alert).not.toHaveTextContent('top-secret');
    expect(alert).not.toHaveTextContent('private.example.test');
    expect(alert).not.toHaveTextContent('path=/tmp');
  });

  it('redacts profile apply reviews before they enter UI state', () => {
    const review = redactProfileApplyReview(
      applyPlan({
        confirmation: {
          confirmationToken: 'confirmation-token-should-not-persist',
        },
        digest: 'digest-should-not-persist',
        credentialRef: 'credential-ref-should-not-persist',
        provenance: 'source-fingerprint-should-not-persist',
      })
    );

    expect(review).toEqual({
      profileId: 'profile-a',
      profileRevision: 2,
      mergePolicy: 'replace_managed_only',
      entries: [
        {
          managedMcpId: 'managed-a',
          mcpId: 'fetch',
          name: 'Fetch',
          version: '1.0.0',
          health: 'healthy',
          authReady: true,
          policyReady: true,
          readiness: 'complete',
        },
      ],
      expiresAtMs: review.expiresAtMs,
    });
    expect(review).not.toHaveProperty('confirmation');
    expect(review).not.toHaveProperty('digest');
    expect(review).not.toHaveProperty('credentialRef');
    expect(review).not.toHaveProperty('provenance');
    expect(JSON.stringify(review)).not.toContain('confirmation-token-should-not-persist');
  });

  it('opens a safe apply review and closes without confirming', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));

    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    expect(screen.getByText('Review profile application')).toBeInTheDocument();
    expect(screen.getByText('replace managed only')).toBeInTheDocument();
    expect(screen.queryByText('confirmation-token')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(confirmMcpProfileApply).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
  });

  it('guards against double confirm and creates only a new session with the safe meta token', async () => {
    const confirmRequest = deferred<ReturnType<typeof profileApplicationToken>>();
    const dispatchSpy = vi.spyOn(window, 'dispatchEvent');
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(confirmMcpProfileApply).mockImplementation(() => confirmRequest.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    dispatchSpy.mockClear();
    const confirmButton = await screen.findByRole('button', {
      name: 'Confirm and create new session',
    });
    await user.click(confirmButton);
    await user.click(confirmButton);

    await waitFor(() => expect(confirmMcpProfileApply).toHaveBeenCalledTimes(1));

    confirmRequest.resolve(profileApplicationToken());

    await waitFor(() =>
      expect(createSession).toHaveBeenCalledWith('/workspace/current', {
        allExtensions: expect.any(Array),
        profileApplicationToken: 'profile-application-token',
      })
    );
    expect(confirmMcpProfileApply).toHaveBeenCalledWith({
      planId: 'plan-profile-1',
      confirmationToken: 'confirmation-token',
      confirm: true,
    });
    const dispatchedEvents = dispatchSpy.mock.calls.map(([event]) => event as CustomEvent);
    const sessionCreatedEvents = dispatchedEvents.filter(
      (event) => event.type === AppEvents.SESSION_CREATED
    );
    const addActiveEvents = dispatchedEvents.filter(
      (event) => event.type === AppEvents.ADD_ACTIVE_SESSION
    );
    expect(sessionCreatedEvents).toHaveLength(1);
    expect(addActiveEvents).toHaveLength(1);
    const sessionCreatedIndex = dispatchedEvents.indexOf(sessionCreatedEvents[0] as CustomEvent);
    const addActiveIndex = dispatchedEvents.indexOf(addActiveEvents[0] as CustomEvent);
    expect(sessionCreatedIndex).toBeGreaterThanOrEqual(0);
    expect(addActiveIndex).toBe(sessionCreatedIndex + 1);
    expect(sessionCreatedEvents[0]?.detail).toMatchObject({
      session: { id: 'session-apply' },
    });
    expect(addActiveEvents[0]?.detail).toEqual({
      sessionId: 'session-apply',
      initialMessage: undefined,
    });
    dispatchSpy.mockRestore();
  });

  it('blocks confirm when the current session folder cannot be resolved', async () => {
    acpChatSessionActions.deleteSnapshot(currentChatSession.sessionId);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(acpGetSessionListItem).mockRejectedValue(new Error('session metadata unavailable'));
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Current session folder unavailable');
    expect(alert).toHaveTextContent(
      'Lumina could not verify the current session folder, so this reviewed profile application was not confirmed.'
    );
    expect(screen.getByRole('button', { name: 'Create a new review' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Confirm and create new session' })).toBeDisabled();
    expect(confirmMcpProfileApply).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
  });

  it('resolves the current session folder before calling profile apply confirm', async () => {
    const sessionLookup = deferred<ReturnType<typeof currentSessionListItem>>();
    const confirmRequest = deferred<ReturnType<typeof profileApplicationToken>>();
    acpChatSessionActions.deleteSnapshot(currentChatSession.sessionId);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(acpGetSessionListItem).mockImplementation(() => sessionLookup.promise as never);
    vi.mocked(confirmMcpProfileApply).mockImplementation(() => confirmRequest.promise);
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));

    await waitFor(() =>
      expect(acpGetSessionListItem).toHaveBeenCalledWith(currentChatSession.sessionId)
    );
    expect(confirmMcpProfileApply).not.toHaveBeenCalled();

    sessionLookup.resolve(currentSessionListItem({ workingDir: '/workspace/from-session-info' }));

    await waitFor(() => expect(confirmMcpProfileApply).toHaveBeenCalledTimes(1));
    confirmRequest.resolve(profileApplicationToken());

    await waitFor(() =>
      expect(createSession).toHaveBeenCalledWith('/workspace/from-session-info', {
        allExtensions: expect.any(Array),
        profileApplicationToken: 'profile-application-token',
      })
    );
  });

  it('drops local apply state when the review is closed before current session folder resolution finishes', async () => {
    const sessionLookup = deferred<ReturnType<typeof currentSessionListItem>>();
    acpChatSessionActions.deleteSnapshot(currentChatSession.sessionId);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(acpGetSessionListItem).mockImplementation(() => sessionLookup.promise as never);
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));
    await waitFor(() =>
      expect(acpGetSessionListItem).toHaveBeenCalledWith(currentChatSession.sessionId)
    );

    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());

    sessionLookup.resolve(currentSessionListItem({ workingDir: '/workspace/from-session-info' }));
    await Promise.resolve();
    await Promise.resolve();

    expect(confirmMcpProfileApply).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
  });

  it('drops local apply state on unmount before current session folder resolution finishes', async () => {
    const sessionLookup = deferred<ReturnType<typeof currentSessionListItem>>();
    acpChatSessionActions.deleteSnapshot(currentChatSession.sessionId);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(acpGetSessionListItem).mockImplementation(() => sessionLookup.promise as never);
    const user = userEvent.setup();

    const view = renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));
    await waitFor(() =>
      expect(acpGetSessionListItem).toHaveBeenCalledWith(currentChatSession.sessionId)
    );

    view.unmount();
    sessionLookup.resolve(currentSessionListItem({ workingDir: '/workspace/from-session-info' }));
    await Promise.resolve();
    await Promise.resolve();

    expect(confirmMcpProfileApply).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
  });

  it('shows plan expiry recovery and recreates the review after a failed confirm', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan)
      .mockResolvedValueOnce(applyPlan())
      .mockResolvedValueOnce(applyPlan({ planId: 'plan-profile-2' }));
    vi.mocked(confirmMcpProfileApply).mockRejectedValue(planExpiredError());
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));

    expect(await screen.findByText('Plan expired')).toBeInTheDocument();
    expect(confirmMcpProfileApply).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Confirm and create new session' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Create a new review' }));

    await waitFor(() => expect(createMcpProfileApplyPlan).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    expect(screen.getByText('Review profile application')).toBeInTheDocument();
  });

  it('blocks confirm when the review token is missing and requires a new review', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(
      applyPlan({
        confirmation: {
          confirmationToken: '   ',
        },
      })
    );
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Profile application is no longer available');
    expect(screen.getByRole('button', { name: 'Create a new review' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Confirm and create new session' })).toBeDisabled();
    expect(confirmMcpProfileApply).not.toHaveBeenCalled();
    expect(createSession).not.toHaveBeenCalled();
    expect(screen.queryByText('confirmation-token')).not.toBeInTheDocument();
  });

  it('maps session creation failures to safe recovery copy without leaking raw codes', async () => {
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [managed()], nextCursor: null });
    vi.mocked(listMcpProfiles).mockResolvedValue({ items: [profile()] });
    vi.mocked(getMcpProfile).mockResolvedValue(profileDetail());
    vi.mocked(createMcpProfileApplyPlan).mockResolvedValue(applyPlan());
    vi.mocked(confirmMcpProfileApply).mockResolvedValue(profileApplicationToken());
    vi.mocked(createSession).mockRejectedValue({
      error: { data: 'profile application rejected: policy_denied' },
    });
    const user = userEvent.setup();

    renderProfilesTab();

    await user.click(await screen.findByRole('button', { name: 'Apply to new session' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and create new session' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Blocked by machine policy');
    expect(alert).toHaveTextContent(
      'Machine policy blocked this reviewed profile application from creating a new session.'
    );
    expect(alert).not.toHaveTextContent('policy_denied');
  });
});
