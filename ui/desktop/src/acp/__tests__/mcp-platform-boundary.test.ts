import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  McpCatalogDetail,
  McpManagedSummary,
  McpPlanReview,
  McpTaskRef,
} from '@aaif/goose-sdk';
import { McpPlatformClient } from '@aaif/goose-sdk';
import {
  archiveMcpProfile,
  cancelMcpTask,
  confirmMcpProfileApply,
  confirmMcpPlan,
  confirmMcpSourceProvision,
  confirmHttpsMcpManifest,
  createMcpProfile,
  createMcpProfileApplyPlan,
  createMcpProfileDraft,
  createMcpPlan,
  getMcpCatalogDetail,
  getMcpSourcesPolicy,
  importGovernedMcpSource,
  getPublicMcpTask,
  getManagedMcp,
  getMcpHealth,
  getMcpProfile,
  refreshMcpSource,
  isMcpPhaseUnavailableError,
  listManagedMcps,
  listMcpCatalog,
  listMcpProfiles,
  mapPublicMcpTask,
  McpPlatformServiceError,
  prepareMcpSourceProvision,
  prepareHttpsMcpManifest,
  recommendMcpProfileModels,
  restoreMcpProfile,
  retryMcpTask,
  setMcpDefaultEnabled,
  testMcpProfileConnection,
  updateMcpProfile,
} from '../mcp-platform';

const boundary = vi.hoisted(() => ({
  client: null as {
    extMethod: (
      method: string,
      params: Record<string, unknown>
    ) => Promise<Record<string, unknown>>;
    mcpPlatform: McpPlatformClient;
  } | null,
}));

vi.mock('../acpConnection', () => ({
  getAcpClient: async () => {
    if (!boundary.client) throw new Error('boundary client is not configured');
    return boundary.client;
  },
}));

const task: McpTaskRef = {
  taskId: 'task-managed-a',
  operation: 'update',
  status: 'queued',
  progress: 0,
  cancellable: true,
  revision: 8,
  updatedAtMs: 8,
  outcome: {
    state: 'pending',
    rollback: 'not_required',
    finalization: 'pending',
    remainingEffects: [],
    nextAction: 'wait',
  },
};

const plan: McpPlanReview = {
  planId: 'plan-managed-a',
  planDigest: 'digest-managed-a',
  expiresAtMs: Date.now() + 60_000,
  sourceId: 'catalog',
  proof: { type: 'local_bytes' },
  trustTier: 'official',
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  mcpId: 'managed-a',
  name: 'Managed A',
  version: '2.0.0',
  selectedManifestDigest: 'manifest-managed-a',
  immutableEvidence: { type: 'artifact', sha256: 'sha256:test' },
  permissions: [],
  networkOrigins: [],
  fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
  hostEffects: { registrationIds: [] },
  processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
  reversibility: { reversible: true, strategy: { type: 'remove_connection_registration' } },
  policy: { outcome: 'allow', reasons: [] },
  warnings: [],
  requiredConfirmations: [],
  defaultDisabled: true,
  recovery: 'none',
};

const catalogDetail: McpCatalogDetail = {
  sourceId: 'catalog-a',
  mcpId: 'managed-a',
  version: '2.0.0',
  manifestDigest: 'manifest-managed-a',
  name: 'Managed A',
  description: 'Managed A description',
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  proof: { type: 'local_bytes' },
  trustTier: 'official',
  distribution: { type: 'remote_http' },
  transport: {
    type: 'streamable_http',
    endpoint: 'https://example.test/mcp',
    allowed_redirect_origins: [],
  },
  auth: { type: 'none' },
  health: { type: 'http', relative_endpoint: '/health', expected_status: 200, timeout_seconds: 5 },
  capabilities: ['tools'],
  permissions: [],
  compatibility: 'compatible',
  verifiedAtMs: 1,
  eligibility: { outcome: 'allowed', reason: 'eligible', recovery: 'none' },
};

const catalogPage = {
  items: [
    {
      sourceId: 'catalog-a',
      mcpId: 'managed-a',
      version: '2.0.0',
      manifestDigest: 'manifest-managed-a',
      name: 'Managed A',
      description: 'Managed A description',
      publisherId: 'publisher',
      publisherName: 'Publisher',
      trustTier: 'official',
      proof: { type: 'local_bytes' },
      compatibility: 'compatible',
      distribution: 'remote_http',
      verifiedAtMs: 1,
      eligibility: { outcome: 'allowed', reason: 'eligible', recovery: 'none' },
    },
  ],
  cache: {
    offline: true,
    localPersistenceOnly: true,
    newestVerifiedAtMs: 1,
    freshness: 'offline_verified',
    refreshState: 'local_only',
    recovery: 'none',
  },
};

const sourcesPolicy = {
  sources: [
    {
      sourceId: 'catalog-a',
      displayName: 'Catalog A',
      importKind: 'local_persistence',
      trustTiers: ['local'],
      manifestCount: 1,
      lastImportedAtMs: 1,
      cache: catalogPage.cache,
      compatibility: { compatible: 1, restricted: 0, denied: 0 },
      recovery: 'none',
    },
  ],
  policy: {
    targetPlatform: 'windows',
    targetArchitecture: 'x86_64',
    developmentMode: false,
    dockerAllowed: false,
    recovery: 'none',
  },
};

const summary: McpManagedSummary = {
  managedMcpId: 'managed-a',
  mcpId: 'managed-a',
  installationScope: 'user',
  registration: 'registered',
  installation: 'installed',
  runtime: 'stopped',
  health: 'healthy',
  defaultEnabled: true,
  revision: 18,
  updatedAtMs: 18,
  distributionAdapter: 'archive',
  activeVersion: null,
  availableVersion: null,
  availableManifestDigest: null,
  currentTask: null,
  recoveryRequired: false,
  eligibility: {
    update: true,
    repair: true,
    uninstall: true,
    reason: 'eligible',
    updateReason: 'eligible',
    repairReason: 'eligible',
    uninstallReason: 'eligible',
  },
  nextAction: 'none',
  phaseCapabilities: {
    sessionEnablement: 'available',
    toolPolicy: 'not_available_in_this_phase',
    profiles: 'available',
    modelSuggestions: 'available',
  },
};

const profileSummary = {
  profileId: 'profile-a',
  name: 'Team MCPs',
  description: 'Saved MCP set',
  revision: 4,
  archived: false,
  entries: [
    { managedMcpId: 'managed-a', ordinal: 0 },
    { managedMcpId: 'managed-b', ordinal: 1 },
  ],
  createdAtMs: 10,
  updatedAtMs: 11,
};

const profileCredentialStatuses = [
  'unconfigured',
  're_registration_required',
  'trusted_state_conflict',
  'temporarily_unavailable',
  'ready',
] as const;

const profileDetail = {
  profile: profileSummary,
  history: [
    { revision: 4, operation: 'update', actor: 'user', createdAtMs: 11 },
    { revision: 3, operation: 'create', actor: 'user', createdAtMs: 10 },
  ],
};

const profileApplyPlan = {
  planId: 'profile-apply-plan-1',
  profileId: 'profile-a',
  profileRevision: 4,
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
    confirmationToken: 'profile-confirmation-token',
  },
};

const profileApplicationToken = {
  token: 'profile-application-token',
  planId: 'profile-apply-plan-1',
  profileId: 'profile-a',
  profileRevision: 4,
  expiresAtMs: Date.now() + 60_000,
};

const profileConnectionTestResult = {
  passed: true,
  stages: [
    {
      phase: 'eligibility',
      status: 'passed',
      code: 'eligible',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
    {
      phase: 'cleanup',
      status: 'passed',
      code: 'eligible',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
    {
      phase: 'mcp_connect_initialize',
      status: 'passed',
      code: 'eligible',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
    {
      phase: 'tool_discovery',
      status: 'passed',
      code: 'eligible',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
    {
      phase: 'model_request',
      status: 'passed',
      code: 'eligible',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
    {
      phase: 'tool_visibility',
      status: 'passed',
      code: 'tool_visibility_validated',
      diagnostic: 'hidden',
      repairSuggestion: '',
      durationMs: 1,
    },
  ],
};

const governedImportResult = {
  source: {
    sourceId: 'verified_source_catalog_source-alpha',
    importKind: 'verified_source_catalog',
    displayName: 'Alpha Source',
  },
  document: {
    sourceId: 'verified_source_catalog_source-alpha',
    documentId: 'directory-deadbeef',
    documentDigest: 'deadbeef',
    documentKind: 'directory',
  },
  entries: [
    {
      sourceId: 'verified_source_catalog_source-alpha',
      mcpId: 'pkg.alpha',
      version: '1.0.0',
      manifestDigest: 'manifest-alpha',
      name: 'Alpha',
      description: 'Alpha description',
      publisherId: 'publisher',
      publisherName: 'Publisher',
      trustTier: 'community',
      proof: { type: 'local_bytes' as const },
      compatibility: 'compatible' as const,
      distribution: 'remote_http' as const,
      verifiedAtMs: 1,
      eligibility: {
        outcome: 'allowed' as const,
        reason: 'eligible' as const,
        recovery: 'none' as const,
      },
    },
  ],
};

const sourceRefreshResult = {
  sourceId: 'verified_source_catalog_source-alpha',
  displayName: 'Alpha Source',
  documentDigest: 'document-alpha',
  manifestCount: 1,
  refreshedAtMs: 2,
};

const sourceProvisionPreview = {
  sourceId: 'verified_source_catalog_source-alpha',
  displayName: 'Alpha Source',
  rootDigest: 'root-alpha',
  documentDigest: 'document-alpha',
  endpointHost: 'catalog.example.test',
  refreshTransport: 'verified_source_bundle_v1',
  manifestCount: 1,
  warnings: [],
  trustBasis: 'user_pin',
};

const sourceProvisionPrepareResult = {
  provisionId: 'source_provisioning_alpha',
  confirmationToken: 'source_provisioning_confirmation_alpha',
  expiresAtMs: Date.now() + 60_000,
  preview: sourceProvisionPreview,
};

const sourceProvisionConfirmResult = {
  provisionId: 'source_provisioning_alpha',
  confirmedAtMs: 2,
  preview: sourceProvisionPreview,
};

const profileDraft = {
  locale: 'en',
  name: 'Draft profile',
  description: 'Draft description',
  entries: [{ managedMcpId: 'managed-a', ordinal: 0 }],
  candidates: [
    {
      managedMcpId: 'managed-a',
      mcpId: 'fetch',
      name: 'Fetch',
      description: 'HTTP MCP',
      confidence: 0.9,
      reasonCode: 'keyword_match',
    },
  ],
  unresolvedTerms: ['drive sync'],
  lowConfidence: true,
  persisted: false,
};

const modelRecommendation = {
  candidates: [
    {
      providerId: 'openai',
      modelId: 'gpt-5',
      confidence: 0.88,
      reasonCodes: ['tool_coverage'],
      caveatCodes: ['inventory_only'],
    },
  ],
  inventoryOnly: true,
  mutated: false,
};

describe('Desktop adapter to formal ACP MCP Platform boundary', () => {
  const calls: Array<{ method: string; params: Record<string, unknown> }> = [];
  const profileCalls = [
    { label: 'list', call: () => listMcpProfiles(true) },
    { label: 'get', call: () => getMcpProfile('profile-a') },
    {
      label: 'create',
      call: () =>
        createMcpProfile({
          name: 'Team MCPs',
          description: 'Saved MCP set',
          managedMcpIds: ['managed-a'],
        }),
    },
    {
      label: 'update',
      call: () =>
        updateMcpProfile({
          profileId: 'profile-a',
          expectedRevision: 4,
          name: 'Team MCPs',
          description: 'Updated',
          managedMcpIds: ['managed-a'],
        }),
    },
    {
      label: 'restore',
      call: () =>
        restoreMcpProfile({
          profileId: 'profile-a',
          sourceRevision: 3,
          expectedRevision: 4,
        }),
    },
    {
      label: 'archive',
      call: () => archiveMcpProfile({ profileId: 'profile-a', expectedRevision: 4 }),
    },
    {
      label: 'draft',
      call: () => createMcpProfileDraft('Need research and browser tools', 'en'),
    },
    {
      label: 'recommend',
      call: () => recommendMcpProfileModels('Need research and browser tools', ['openai']),
    },
  ];

  function setBoundaryClient(
    extMethod: (method: string, params: Record<string, unknown>) => Promise<Record<string, unknown>>
  ) {
    boundary.client = {
      extMethod,
      mcpPlatform: new McpPlatformClient({ extMethod }),
    };
  }

  beforeEach(() => {
    calls.length = 0;
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      const value =
        method === 'goose.mcpCatalogDetail_unstable'
          ? catalogDetail
          : method === 'goose.mcpCatalogList_unstable'
            ? catalogPage
            : method === 'goose.mcpSourcesPolicyGet_unstable'
              ? sourcesPolicy
              : method === 'goose.mcpGovernedImport_unstable'
                ? governedImportResult
                : method === 'goose.mcpSourceRefresh_unstable'
                  ? sourceRefreshResult
                  : method === 'goose.mcpSourceProvisionPrepare_unstable'
                    ? sourceProvisionPrepareResult
                    : method === 'goose.mcpSourceProvisionConfirm_unstable'
                      ? sourceProvisionConfirmResult
                      : method === 'goose.mcpPlanCreate_unstable'
                        ? plan
                        : method === 'goose.mcpProfileApplyPlanCreate_unstable'
                          ? profileApplyPlan
                          : method === 'goose.mcpProfileApplyConfirm_unstable'
                            ? profileApplicationToken
                            : method === 'goose.mcpProfileConnectionTest_unstable'
                              ? profileConnectionTestResult
                              : method === 'goose.mcpProfileList_unstable'
                                ? { items: [profileSummary] }
                                : method === 'goose.mcpProfileGet_unstable'
                                  ? profileDetail
                                  : method === 'goose.mcpProfileDraftCreate_unstable'
                                    ? profileDraft
                                    : method === 'goose.mcpProfileModelRecommend_unstable'
                                      ? modelRecommendation
                                      : method === 'goose.mcpSetDefaultEnabled_unstable'
                                        ? summary
                                        : method === 'goose.mcpProfileCreate_unstable' ||
                                            method === 'goose.mcpProfileUpdate_unstable' ||
                                            method === 'goose.mcpProfileRestore_unstable' ||
                                            method === 'goose.mcpProfileArchive_unstable'
                                          ? profileSummary
                                          : task;
      return { outcome: { status: 'success', value } };
    };
    boundary.client = {
      mcpPlatform: new McpPlatformClient({
        extMethod,
      }),
      extMethod,
    };
  });

  it('carries plan owner, confirmation identity, and expected revisions through both real layers', async () => {
    const review = await createMcpPlan({
      type: 'update',
      managed_mcp_id: 'managed-a',
      target_version: '2.0.0',
    });
    await confirmMcpPlan(review, 'confirm');
    await setMcpDefaultEnabled({ managedMcpId: 'managed-a', revision: 17 }, true);
    await cancelMcpTask({ taskId: 'task-managed-a', revision: 8 });
    await retryMcpTask({ taskId: 'task-managed-a', revision: 9 });
    await listMcpProfiles(true);
    await getMcpProfile('profile-a');
    await createMcpProfile({
      name: 'Team MCPs',
      description: 'Saved MCP set',
      managedMcpIds: ['managed-a', 'managed-b'],
    });
    await updateMcpProfile({
      profileId: 'profile-a',
      expectedRevision: 4,
      name: 'Team MCPs',
      description: 'Updated',
      managedMcpIds: ['managed-a'],
    });
    await restoreMcpProfile({
      profileId: 'profile-a',
      sourceRevision: 3,
      expectedRevision: 4,
    });
    await archiveMcpProfile({ profileId: 'profile-a', expectedRevision: 4 });
    await createMcpProfileDraft('Need research and browser tools', 'en');
    await recommendMcpProfileModels('Need research and browser tools', ['openai']);

    expect(calls.map(({ method }) => method)).toEqual([
      'goose.mcpPlanCreate_unstable',
      'goose.mcpInstallConfirm_unstable',
      'goose.mcpSetDefaultEnabled_unstable',
      'goose.mcpTaskCancel_unstable',
      'goose.mcpTaskRetry_unstable',
      'goose.mcpProfileList_unstable',
      'goose.mcpProfileGet_unstable',
      'goose.mcpProfileCreate_unstable',
      'goose.mcpProfileUpdate_unstable',
      'goose.mcpProfileRestore_unstable',
      'goose.mcpProfileArchive_unstable',
      'goose.mcpProfileDraftCreate_unstable',
      'goose.mcpProfileModelRecommend_unstable',
    ]);
    expect(calls[0].params).toMatchObject({
      intent: { type: 'update', managed_mcp_id: 'managed-a', target_version: '2.0.0' },
    });
    expect(calls[1].params).toMatchObject({
      planId: 'plan-managed-a',
      planDigest: 'digest-managed-a',
      userDecision: 'confirm',
    });
    expect(calls[2].params).toEqual({
      managedMcpId: 'managed-a',
      enabled: true,
      expectedRevision: 17,
    });
    expect(calls[3].params).toEqual({ taskId: 'task-managed-a', expectedRevision: 8 });
    expect(calls[4].params).toMatchObject({
      taskId: 'task-managed-a',
      expectedRevision: 9,
    });
    expect(calls[5].params).toEqual({ includeArchived: true });
    expect(calls[6].params).toEqual({ profileId: 'profile-a' });
    expect(calls[7].params).toMatchObject({
      name: 'Team MCPs',
      description: 'Saved MCP set',
      managedMcpIds: ['managed-a', 'managed-b'],
    });
    expect(calls[8].params).toMatchObject({
      profileId: 'profile-a',
      expectedRevision: 4,
      name: 'Team MCPs',
      description: 'Updated',
      managedMcpIds: ['managed-a'],
    });
    expect(calls[9].params).toMatchObject({
      profileId: 'profile-a',
      sourceRevision: 3,
      expectedRevision: 4,
    });
    expect(calls[10].params).toMatchObject({
      profileId: 'profile-a',
      expectedRevision: 4,
    });
    expect(calls[11].params).toEqual({
      text: 'Need research and browser tools',
      locale: 'en',
    });
    expect(calls[12].params).toEqual({
      text: 'Need research and browser tools',
      providerIds: ['openai'],
    });
    expect(calls.some(({ method }) => method.includes('mcpProfileApply'))).toBe(false);
  });

  it('sends only saved profile and configured provider/model identifiers for a connection test', async () => {
    await expect(
      testMcpProfileConnection({
        profileId: 'profile-a',
        providerId: 'openai',
        modelId: 'gpt-5',
      })
    ).resolves.toEqual({
      passed: true,
      stages: profileConnectionTestResult.stages.map(({ phase, status, code }) => ({
        phase,
        status,
        code,
      })),
    });

    expect(calls[0]).toEqual({
      method: 'goose.mcpProfileConnectionTest_unstable',
      params: { profileId: 'profile-a', providerId: 'openai', modelId: 'gpt-5' },
    });
    expect(JSON.stringify(calls[0].params)).not.toMatch(/credential|token|endpoint|command/i);
  });

  it('fails closed when a connection-test response has an unknown stage code', async () => {
    setBoundaryClient(async () => ({
      outcome: {
        status: 'success',
        value: {
          passed: false,
          stages: [
            {
              phase: 'eligibility',
              status: 'failed',
              code: 'raw_diagnostic_leak',
              diagnostic: 'Authorization: Bearer secret-value',
              repairSuggestion: '',
              durationMs: 1,
            },
          ],
        },
      },
    }));

    await expect(
      testMcpProfileConnection({
        profileId: 'profile-a',
        providerId: 'openai',
        modelId: 'gpt-5',
      })
    ).rejects.toThrow('The MCP Platform request could not be completed.');
  });

  it('accepts every published connection-test phase and code while dropping runtime details', async () => {
    const phases = [
      'eligibility',
      'mcp_connect_initialize',
      'tool_discovery',
      'cleanup',
      'model_request',
      'tool_visibility',
    ];
    const codes = [
      'eligible',
      'profile_not_ready',
      'policy_denied',
      'mcp_connect_failed',
      'mcp_timeout',
      'tool_discovery_failed',
      'no_tools_exposed',
      'provider_not_configured',
      'model_not_configured',
      'provider_initialization_failed',
      'model_request_failed',
      'runtime_activation_failed',
      'resource_limit_exceeded',
      'prerequisites_failed',
      'cleanup_failed',
      'tool_visibility_validated',
      'tool_visibility_skipped',
    ];
    const stages = codes.map((code, index) => ({
      phase: phases[index % phases.length],
      status: index === 0 ? 'failed' : 'skipped',
      code,
      diagnostic: { raw: 'must-not-cross-the-ui-boundary' },
      repairSuggestion: ['must-not-cross-the-ui-boundary'],
      durationMs: { raw: 1 },
      managedMcpId: { raw: 'managed-secret' },
    }));
    setBoundaryClient(async () => ({
      outcome: { status: 'success', value: { passed: false, stages } },
    }));

    await expect(
      testMcpProfileConnection({ profileId: 'profile-a', providerId: 'openai', modelId: 'gpt-5' })
    ).resolves.toEqual({
      passed: false,
      stages: stages.map(({ phase, status, code }) => ({ phase, status, code })),
    });
  });

  it('fails closed on empty, contradictory, or unknown connection-test stages', async () => {
    const invalidValues = [
      { passed: true, stages: [] },
      {
        passed: true,
        stages: [{ phase: 'cleanup', status: 'failed', code: 'cleanup_failed' }],
      },
      {
        passed: false,
        stages: [{ phase: 'cleanup', status: 'passed', code: 'eligible' }],
      },
      {
        passed: false,
        stages: [{ phase: 'future_phase', status: 'failed', code: 'cleanup_failed' }],
      },
      {
        passed: false,
        stages: [{ phase: 'cleanup', status: 'failed', code: 'future_code' }],
      },
    ];

    for (const value of invalidValues) {
      setBoundaryClient(async () => ({ outcome: { status: 'success', value } }));
      await expect(
        testMcpProfileConnection({ profileId: 'profile-a', providerId: 'openai', modelId: 'gpt-5' })
      ).rejects.toThrow('The MCP Platform request could not be completed.');
    }
  });

  it('uses catalog_ref locators for source-aware detail lookups while keeping digest compatibility', async () => {
    await getMcpCatalogDetail({
      sourceId: 'catalog-a',
      mcpId: 'managed-a',
      version: '2.0.0',
    });
    await getMcpCatalogDetail('manifest-managed-a');

    expect(calls[0]).toEqual({
      method: 'goose.mcpCatalogDetail_unstable',
      params: {
        catalog: {
          type: 'catalog_ref',
          source_id: 'catalog-a',
          mcp_id: 'managed-a',
          version: '2.0.0',
        },
      },
    });
    expect(calls[1]).toEqual({
      method: 'goose.mcpCatalogDetail_unstable',
      params: {
        catalog: {
          type: 'manifest_digest',
          manifest_digest: 'manifest-managed-a',
        },
      },
    });
  });

  it('strictly parses catalog, detail, and source policy responses before exposing them to the desktop', async () => {
    await expect(listMcpCatalog({ pageSize: 24 })).resolves.toEqual(catalogPage);
    await expect(getMcpCatalogDetail('manifest-managed-a')).resolves.toEqual(catalogDetail);
    await expect(getMcpSourcesPolicy()).resolves.toEqual(sourcesPolicy);

    expect(calls.map(({ method }) => method)).toEqual([
      'goose.mcpCatalogList_unstable',
      'goose.mcpCatalogDetail_unstable',
      'goose.mcpSourcesPolicyGet_unstable',
    ]);
  });

  it('accepts every supported catalog detail contract variant through the strict parser', async () => {
    const variants = [
      {
        label: 'manual stdio distribution',
        value: {
          ...catalogDetail,
          distribution: { type: 'manual_stdio', platforms: ['windows', 'linux'] },
        },
      },
      {
        label: 'npm distribution',
        value: {
          ...catalogDetail,
          distribution: {
            type: 'npm',
            package: '@example/mcp',
            package_version: '1.0.0',
            artifacts: [
              {
                platform: 'windows',
                architecture: 'x86_64',
                origin: 'https://catalog.example.test/mcp.tgz',
                digest: 'sha256:npm',
                sizeBytes: 42,
              },
            ],
          },
        },
      },
      {
        label: 'python wheel distribution',
        value: {
          ...catalogDetail,
          distribution: {
            type: 'python_wheel',
            package: 'example-mcp',
            package_version: '1.0.0',
            python: '>=3.11',
            artifacts: [
              {
                platform: 'linux',
                architecture: 'x86_64',
                origin: 'https://catalog.example.test/mcp.whl',
                digest: 'sha256:wheel',
              },
            ],
          },
        },
      },
      {
        label: 'binary archive distribution',
        value: {
          ...catalogDetail,
          distribution: {
            type: 'binary_archive',
            archive_format: 'zip',
            artifacts: [
              {
                platform: 'windows',
                architecture: 'arm64',
                origin: 'https://catalog.example.test/mcp.zip',
                digest: 'sha256:archive',
              },
            ],
          },
        },
      },
      {
        label: 'docker distribution',
        value: {
          ...catalogDetail,
          distribution: {
            type: 'docker',
            image: 'registry.example.test/mcp',
            digest: 'sha256:docker',
          },
        },
      },
      {
        label: 'git development distribution',
        value: {
          ...catalogDetail,
          distribution: {
            type: 'git_dev',
            repository: 'https://git.example.test/mcp.git',
            commit: '0123456789abcdef',
            adapter: 'goose-dev',
          },
        },
      },
      {
        label: 'stdio transport',
        value: {
          ...catalogDetail,
          transport: { type: 'stdio', startup_timeout_seconds: 30 },
        },
      },
      {
        label: 'API key authentication',
        value: {
          ...catalogDetail,
          auth: {
            type: 'api_key_header',
            credential_required: true,
            prefix_required: false,
          },
        },
      },
      {
        label: 'environment authentication',
        value: {
          ...catalogDetail,
          auth: { type: 'environment', credential_required: true },
        },
      },
      {
        label: 'OAuth authentication',
        value: {
          ...catalogDetail,
          auth: {
            type: 'oauth2',
            authorization_endpoint: 'https://auth.example.test/authorize',
            token_endpoint: 'https://auth.example.test/token',
            client_registration: 'dynamic',
            scopes: ['mcp.read'],
          },
        },
      },
      {
        label: 'initialize health check',
        value: {
          ...catalogDetail,
          health: { type: 'mcp_initialize', timeout_seconds: 10 },
        },
      },
      {
        label: 'list tools health check',
        value: {
          ...catalogDetail,
          health: { type: 'mcp_list_tools', timeout_seconds: 10 },
        },
      },
    ] as const;

    for (const variant of variants) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return { outcome: { status: 'success', value: variant.value } };
      });
      await expect(getMcpCatalogDetail('manifest-managed-a'), variant.label).resolves.toEqual(
        variant.value
      );
    }
  });

  it('fails closed on malformed catalog, detail, and source policy success payloads', async () => {
    const malformedResponses = [
      {
        method: 'goose.mcpCatalogList_unstable',
        value: {
          ...catalogPage,
          cache: { ...catalogPage.cache, refreshState: 'network_refresh' },
        },
        call: () => listMcpCatalog({ pageSize: 24 }),
      },
      {
        method: 'goose.mcpCatalogDetail_unstable',
        value: {
          ...catalogDetail,
          transport: {
            ...catalogDetail.transport,
            untrusted: 'raw-secret',
          },
        },
        call: () => getMcpCatalogDetail('manifest-managed-a'),
      },
      {
        method: 'goose.mcpSourcesPolicyGet_unstable',
        value: {
          ...sourcesPolicy,
          sources: [{ ...sourcesPolicy.sources[0], trustTiers: ['untrusted'] }],
        },
        call: () => getMcpSourcesPolicy(),
      },
      {
        method: 'goose.mcpCatalogDetail_unstable',
        value: { ...catalogDetail, sourceId: 'catalog-b' },
        call: () =>
          getMcpCatalogDetail({
            sourceId: 'catalog-a',
            mcpId: 'managed-a',
            version: '2.0.0',
          }),
      },
    ] as const;

    for (const invalid of malformedResponses) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return {
          outcome: {
            status: 'success',
            value: method === invalid.method ? invalid.value : catalogPage,
          },
        };
      });
      await expect(invalid.call()).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('routes local governed imports through the formal ACP boundary without URL fields', async () => {
    await expect(
      importGovernedMcpSource({
        type: 'local_manifest',
        filePath: 'D:/catalogs/manifest.json',
      })
    ).resolves.toEqual(governedImportResult);

    await expect(
      importGovernedMcpSource({
        type: 'local_directory',
        directoryPath: 'D:/catalogs/source-alpha',
      })
    ).resolves.toEqual(governedImportResult);

    expect(calls[0]).toEqual({
      method: 'goose.mcpGovernedImport_unstable',
      params: {
        source: {
          type: 'local_manifest',
          filePath: 'D:/catalogs/manifest.json',
        },
      },
    });
    expect(calls[1]).toEqual({
      method: 'goose.mcpGovernedImport_unstable',
      params: {
        source: {
          type: 'local_directory',
          directoryPath: 'D:/catalogs/source-alpha',
        },
      },
    });
    expect(calls[0].params.source).not.toHaveProperty('url');
    expect(calls[0].params.source).not.toHaveProperty('command');
  });

  it('refreshes governed sources only by registered source id through the formal ACP boundary', async () => {
    await expect(refreshMcpSource('verified_source_catalog_source-alpha')).resolves.toEqual(
      sourceRefreshResult
    );

    expect(calls[0]).toEqual({
      method: 'goose.mcpSourceRefresh_unstable',
      params: {
        sourceId: 'verified_source_catalog_source-alpha',
      },
    });
    expect(calls[0].params).not.toHaveProperty('url');
    expect(calls[0].params).not.toHaveProperty('command');
    expect(calls[0].params).not.toHaveProperty('env');
    expect(calls[0].params).not.toHaveProperty('secret');
  });

  it('uses the two-step local source provisioning contract without URL, command, env, or secret fields', async () => {
    const prepared = await prepareMcpSourceProvision('D:/catalogs/source-alpha');
    const confirmed = await confirmMcpSourceProvision(prepared);

    expect(prepared).toEqual(sourceProvisionPrepareResult);
    expect(confirmed).toEqual(sourceProvisionConfirmResult);
    expect(calls).toEqual([
      {
        method: 'goose.mcpSourceProvisionPrepare_unstable',
        params: { localDirectory: 'D:/catalogs/source-alpha' },
      },
      {
        method: 'goose.mcpSourceProvisionConfirm_unstable',
        params: {
          provisionId: 'source_provisioning_alpha',
          confirmationToken: 'source_provisioning_confirmation_alpha',
          confirm: true,
        },
      },
    ]);
    for (const { params } of calls) {
      expect(params).not.toHaveProperty('url');
      expect(params).not.toHaveProperty('command');
      expect(params).not.toHaveProperty('argv');
      expect(params).not.toHaveProperty('env');
      expect(params).not.toHaveProperty('secret');
    }
  });

  it('fails closed on malformed source provisioning responses and mismatched refresh source identities', async () => {
    const malformedResponses = [
      {
        method: 'goose.mcpSourceProvisionPrepare_unstable',
        value: {
          ...sourceProvisionPrepareResult,
          preview: { ...sourceProvisionPreview, refreshTransport: 'untrusted_transport' },
        },
        call: () => prepareMcpSourceProvision('D:/catalogs/source-alpha'),
      },
      {
        method: 'goose.mcpSourceProvisionConfirm_unstable',
        value: { ...sourceProvisionConfirmResult, provisionId: 'source_provisioning_other' },
        call: () =>
          confirmMcpSourceProvision({
            provisionId: 'source_provisioning_alpha',
            confirmationToken: 'source_provisioning_confirmation_alpha',
          }),
      },
      {
        method: 'goose.mcpSourceRefresh_unstable',
        value: { ...sourceRefreshResult, sourceId: 'verified_source_catalog_other' },
        call: () => refreshMcpSource('verified_source_catalog_source-alpha'),
      },
    ] as const;

    for (const invalid of malformedResponses) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return {
          outcome: {
            status: 'success',
            value: method === invalid.method ? invalid.value : sourceProvisionPrepareResult,
          },
        };
      });
      await expect(invalid.call()).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('uses the formal F4 profile apply contract without legacy digest fields', async () => {
    const review = await createMcpProfileApplyPlan({
      profileId: 'profile-a',
      profileRevision: 4,
    });
    const application = await confirmMcpProfileApply({
      planId: review.planId,
      confirmationToken: review.confirmation.confirmationToken,
      confirm: true,
    });

    expect(review).toEqual(profileApplyPlan);
    expect(application).toEqual(profileApplicationToken);
    expect(calls.map(({ method }) => method)).toEqual([
      'goose.mcpProfileApplyPlanCreate_unstable',
      'goose.mcpProfileApplyConfirm_unstable',
    ]);
    expect(calls[0].params).toEqual({
      profileId: 'profile-a',
      profileRevision: 4,
      idempotencyKey: expect.any(String),
    });
    expect(calls[1].params).toEqual({
      planId: 'profile-apply-plan-1',
      confirmationToken: 'profile-confirmation-token',
      confirm: true,
    });
    expect(calls[1].params).not.toHaveProperty('planDigest');
    expect(calls[1].params).not.toHaveProperty('userDecision');
  });

  it('preserves a formal deny envelope until the desktop recovery boundary', async () => {
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'error',
          error: {
            code: 'policy_denied',
            message: 'denied',
            retryable: false,
            correlationId: 'deny-correlation',
            details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
          },
        },
      };
    };
    boundary.client = {
      extMethod,
      mcpPlatform: new McpPlatformClient({ extMethod }),
    };

    await expect(
      createMcpPlan({ type: 'repair', managed_mcp_id: 'managed-a' })
    ).rejects.toMatchObject({
      envelope: {
        code: 'policy_denied',
        details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
      },
      recovery: { code: 'policy_denied', retryable: false },
    });
    expect(calls[0].method).toBe('goose.mcpPlanCreate_unstable');
  });

  it('recognizes only the formal 4B profile phase envelope after wire parsing', async () => {
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'error',
          error: {
            code: 'not_implemented_for_phase',
            message: 'profile list unavailable in phase 4B',
            retryable: false,
            correlationId: 'phase-4b',
            details: {
              type: 'phase_unavailable',
              phase: '4B',
              operation: 'goose.mcpProfileList_unstable',
            },
          },
        },
      };
    });

    let thrown: unknown;
    try {
      await listMcpProfiles(true);
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeInstanceOf(McpPlatformServiceError);
    expect(isMcpPhaseUnavailableError(thrown, 'profiles')).toBe(true);
    expect(isMcpPhaseUnavailableError(thrown, 'modelSuggestions')).toBe(false);
  });

  it('does not downgrade operation_not_supported profile wires to a phase gate', async () => {
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'error',
          error: {
            code: 'operation_not_supported',
            message: 'closed adapter contract',
            retryable: false,
            correlationId: 'op-unsupported',
            details: {
              type: 'phase_unavailable',
              phase: '3C',
              operation: 'lifecycle',
            },
          },
        },
      };
    });

    let thrown: unknown;
    try {
      await listMcpProfiles(true);
    } catch (error) {
      thrown = error;
    }

    expect(thrown).toBeInstanceOf(McpPlatformServiceError);
    expect(isMcpPhaseUnavailableError(thrown, 'profiles')).toBe(false);
  });

  it('preserves the formal profile service-detail union through wire parsing', async () => {
    const cases = [
      {
        label: 'revision_conflict',
        call: () =>
          updateMcpProfile({
            profileId: 'profile-a',
            expectedRevision: 4,
            name: 'Team MCPs',
            description: 'Updated',
            managedMcpIds: ['managed-a'],
          }),
        error: {
          code: 'revision_conflict',
          message: 'reload before retrying',
          retryable: true,
          correlationId: 'revision-conflict-ref',
          details: { type: 'revision_conflict' },
        },
      },
      {
        label: 'record_missing',
        call: () => getMcpProfile('profile-a'),
        error: {
          code: 'not_found',
          message: 'missing profile',
          retryable: false,
          correlationId: 'record-missing-ref',
          details: { type: 'record_missing' },
        },
      },
      {
        label: 'repository_temporarily_unavailable',
        call: () => listMcpProfiles(true),
        error: {
          code: 'repository_unavailable',
          message: 'repository is temporarily unavailable',
          retryable: true,
          correlationId: 'repository-ref',
          details: { type: 'repository_temporarily_unavailable' },
        },
      },
      {
        label: 'request_rejected',
        call: () =>
          createMcpProfile({
            name: 'Team MCPs',
            description: 'Saved MCP set',
            managedMcpIds: ['managed-a'],
          }),
        error: {
          code: 'invalid_request',
          message: 'request rejected',
          retryable: false,
          correlationId: 'request-rejected-ref',
          details: { type: 'request_rejected' },
        },
      },
    ] as const;

    for (const testCase of cases) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return { outcome: { status: 'error', error: testCase.error } };
      });

      await expect(testCase.call(), testCase.label).rejects.toMatchObject({
        envelope: {
          code: testCase.error.code,
          details: testCase.error.details,
        },
        recovery: {
          code: testCase.error.code,
          retryable: testCase.error.retryable,
        },
      });
    }
  });

  it('preserves available phase capability values from managed summary success payloads', async () => {
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'success',
          value: {
            items: [summary],
            nextCursor: null,
          },
        },
      };
    });

    await expect(listManagedMcps()).resolves.toEqual({ items: [summary], nextCursor: null });
  });

  it('preserves valid managed null wire forms across the desktop wrappers', async () => {
    const nullableSummary = {
      ...summary,
      activeVersion: null,
      availableVersion: null,
      availableManifestDigest: null,
      currentTask: null,
    };
    const detail = {
      summary: nullableSummary,
      distributionAdapter: 'archive',
      activeManifestDigest: 'sha256:active',
      activeVersion: '2.0.0',
      extensionConfigKey: 'managed-a',
      projectionDigest: 'projection-managed-a',
      latestHealth: null,
      registrationTask: null,
    };
    const health = {
      managedMcpId: 'managed-a',
      state: 'healthy',
      latest: {
        taskId: 'task-health-a',
        mode: 'runtime',
        result: 'healthy',
        latencyMs: 321,
        capabilitiesDigest: null,
        toolsDigest: null,
        checkedAtMs: 1_720_000_000_321,
        detailCode: 'mcp_list_tools_succeeded',
      },
    };

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      const payload =
        method === 'goose.mcpList_unstable'
          ? {
              outcome: {
                status: 'success',
                value: {
                  items: [nullableSummary],
                  nextCursor: null,
                },
              },
            }
          : method === 'goose.mcpGet_unstable'
            ? {
                outcome: {
                  status: 'success',
                  value: detail,
                },
              }
            : method === 'goose.mcpHealthGet_unstable'
              ? {
                  outcome: {
                    status: 'success',
                    value: health,
                  },
                }
              : {
                  outcome: {
                    status: 'success',
                    value: nullableSummary,
                  },
                };
      return JSON.parse(JSON.stringify(payload)) as Record<string, unknown>;
    });

    await expect(listManagedMcps()).resolves.toEqual({
      items: [nullableSummary],
      nextCursor: null,
    });
    await expect(getManagedMcp('managed-a')).resolves.toEqual(detail);
    await expect(getMcpHealth('managed-a')).resolves.toEqual(health);
    await expect(
      setMcpDefaultEnabled({ managedMcpId: 'managed-a', revision: 17 }, true)
    ).resolves.toEqual(nullableSummary);
  });

  it('fails closed on malformed managed success payloads across the desktop wrappers', async () => {
    const malformedResponses: Record<string, Record<string, unknown>> = {
      'goose.mcpList_unstable': {
        outcome: {
          status: 'success',
          value: {
            items: [{ managedMcpId: 'managed-a' }],
          },
        },
      },
      'goose.mcpGet_unstable': {
        outcome: {
          status: 'success',
          value: {
            summary,
            distributionAdapter: 'archive',
            activeManifestDigest: 'sha256:active',
            extensionConfigKey: 'managed-a',
            projectionDigest: 'projection-managed-a',
          },
        },
      },
      'goose.mcpSetDefaultEnabled_unstable': {
        outcome: {
          status: 'success',
          value: {
            ...summary,
            extra: true,
          },
        },
      },
      'goose.mcpHealthGet_unstable': {
        outcome: {
          status: 'success',
          value: {
            managedMcpId: 'managed-a',
            state: 'healthy',
          },
        },
      },
    };

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return malformedResponses[method] ?? { outcome: { status: 'success', value: task } };
    });

    await expect(listManagedMcps()).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    await expect(getManagedMcp('managed-a')).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    await expect(getMcpHealth('managed-a')).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    await expect(
      setMcpDefaultEnabled({ managedMcpId: 'managed-a', revision: 17 }, true)
    ).rejects.toThrow('The MCP Platform request could not be completed.');
  });

  it('fails closed across every profile wrapper when the extMethod response omits outcome', async () => {
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      return {} as Record<string, unknown>;
    };
    setBoundaryClient(extMethod);

    for (const profileCall of profileCalls) {
      await expect(profileCall.call(), profileCall.label).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('fails closed on malformed success payloads for every profile wrapper', async () => {
    const malformedResponseByMethod: Record<string, Record<string, unknown>> = {
      'goose.mcpProfileList_unstable': {
        outcome: { status: 'success', value: { items: 'nope' } },
      },
      'goose.mcpProfileGet_unstable': {
        outcome: { status: 'success', value: { profile: profileSummary, history: 'nope' } },
      },
      'goose.mcpProfileCreate_unstable': {
        outcome: {
          status: 'success',
          value: { ...profileSummary, revision: '4' },
        },
      },
      'goose.mcpProfileUpdate_unstable': {
        outcome: {
          status: 'success',
          value: { ...profileSummary, archived: 'false' },
        },
      },
      'goose.mcpProfileRestore_unstable': {
        outcome: {
          status: 'success',
          value: { ...profileSummary, entries: { managedMcpId: 'managed-a', ordinal: 0 } },
        },
      },
      'goose.mcpProfileArchive_unstable': {
        outcome: {
          status: 'success',
          value: { ...profileSummary, updatedAtMs: '11' },
        },
      },
      'goose.mcpProfileDraftCreate_unstable': {
        outcome: {
          status: 'success',
          value: { ...profileDraft, persisted: 'false' },
        },
      },
      'goose.mcpProfileModelRecommend_unstable': {
        outcome: {
          status: 'success',
          value: {
            ...modelRecommendation,
            candidates: [{ ...modelRecommendation.candidates[0], reasonCodes: {} }],
          },
        },
      },
    };
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      return (
        malformedResponseByMethod[method] ??
        ({ outcome: { status: 'success', value: task } } as Record<string, unknown>)
      );
    };
    setBoundaryClient(extMethod);

    for (const profileCall of profileCalls) {
      await expect(profileCall.call(), profileCall.label).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('accepts profile responses that omit credentialStatus for backward compatibility', async () => {
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      const value =
        method === 'goose.mcpProfileList_unstable'
          ? { items: [profileSummary] }
          : method === 'goose.mcpProfileGet_unstable'
            ? profileDetail
            : profileSummary;
      return { outcome: { status: 'success', value } };
    };
    setBoundaryClient(extMethod);

    await expect(listMcpProfiles(true)).resolves.toEqual({ items: [profileSummary] });
    await expect(getMcpProfile('profile-a')).resolves.toEqual(profileDetail);
    await expect(
      createMcpProfile({
        name: 'Team MCPs',
        description: 'Saved MCP set',
        managedMcpIds: ['managed-a'],
      })
    ).resolves.toEqual(profileSummary);
  });

  it('accepts all five credentialStatus wire values in profile summaries', async () => {
    for (const credentialStatus of profileCredentialStatuses) {
      const extMethod = async (method: string, params: Record<string, unknown>) => {
        calls.push({ method, params });
        const value =
          method === 'goose.mcpProfileGet_unstable'
            ? {
                profile: { ...profileSummary, credentialStatus },
                history: profileDetail.history,
              }
            : { items: [{ ...profileSummary, credentialStatus }] };
        return { outcome: { status: 'success', value } };
      };
      setBoundaryClient(extMethod);

      await expect(listMcpProfiles(true)).resolves.toEqual({
        items: [{ ...profileSummary, credentialStatus }],
      });
      await expect(getMcpProfile('profile-a')).resolves.toEqual({
        profile: { ...profileSummary, credentialStatus },
        history: profileDetail.history,
      });
    }
  });

  it('fails closed on unknown credentialStatus values in profile summaries and details', async () => {
    for (const method of [
      'goose.mcpProfileList_unstable',
      'goose.mcpProfileGet_unstable',
    ] as const) {
      const extMethod = async (actualMethod: string, params: Record<string, unknown>) => {
        calls.push({ method: actualMethod, params });
        const value =
          method === 'goose.mcpProfileGet_unstable'
            ? {
                profile: { ...profileSummary, credentialStatus: 'credential_leaked' },
                history: profileDetail.history,
              }
            : { items: [{ ...profileSummary, credentialStatus: 'credential_leaked' }] };
        return { outcome: { status: 'success', value } };
      };
      setBoundaryClient(extMethod);

      await expect(
        method === 'goose.mcpProfileGet_unstable'
          ? getMcpProfile('profile-a')
          : listMcpProfiles(true)
      ).rejects.toThrow('The MCP Platform request could not be completed.');
    }
  });

  it('preserves credentialStatus across every profile success wrapper', async () => {
    const credentialStatusByMethod = {
      'goose.mcpProfileList_unstable': 'unconfigured',
      'goose.mcpProfileGet_unstable': 're_registration_required',
      'goose.mcpProfileCreate_unstable': 'trusted_state_conflict',
      'goose.mcpProfileUpdate_unstable': 'temporarily_unavailable',
      'goose.mcpProfileRestore_unstable': 'ready',
      'goose.mcpProfileArchive_unstable': 'unconfigured',
    } as const;
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      const credentialStatus =
        credentialStatusByMethod[method as keyof typeof credentialStatusByMethod];
      const value =
        method === 'goose.mcpProfileList_unstable'
          ? { items: [{ ...profileSummary, credentialStatus }] }
          : method === 'goose.mcpProfileGet_unstable'
            ? {
                profile: { ...profileSummary, credentialStatus },
                history: profileDetail.history,
              }
            : { ...profileSummary, credentialStatus };
      return { outcome: { status: 'success', value } };
    };
    setBoundaryClient(extMethod);

    await expect(listMcpProfiles(true)).resolves.toEqual({
      items: [{ ...profileSummary, credentialStatus: 'unconfigured' }],
    });
    await expect(getMcpProfile('profile-a')).resolves.toEqual({
      profile: { ...profileSummary, credentialStatus: 're_registration_required' },
      history: profileDetail.history,
    });
    await expect(
      createMcpProfile({
        name: 'Team MCPs',
        description: 'Saved MCP set',
        managedMcpIds: ['managed-a'],
      })
    ).resolves.toEqual({ ...profileSummary, credentialStatus: 'trusted_state_conflict' });
    await expect(
      updateMcpProfile({
        profileId: 'profile-a',
        expectedRevision: 4,
        name: 'Team MCPs',
        description: 'Updated',
        managedMcpIds: ['managed-a'],
      })
    ).resolves.toEqual({ ...profileSummary, credentialStatus: 'temporarily_unavailable' });
    await expect(
      restoreMcpProfile({
        profileId: 'profile-a',
        sourceRevision: 3,
        expectedRevision: 4,
      })
    ).resolves.toEqual({ ...profileSummary, credentialStatus: 'ready' });
    await expect(
      archiveMcpProfile({ profileId: 'profile-a', expectedRevision: 4 })
    ).resolves.toEqual({
      ...profileSummary,
      credentialStatus: 'unconfigured',
    });
  });

  it('fails closed on malformed profile error envelopes instead of surfacing unavailable', async () => {
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'error',
          error: {
            code: 'not_implemented_for_phase',
            message: ['not a string'],
            retryable: false,
          },
        },
      } as Record<string, unknown>;
    };
    setBoundaryClient(extMethod);

    await expect(listMcpProfiles(true)).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
  });

  it('fails closed on extra phase-unavailable detail keys instead of surfacing unavailable', async () => {
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'error',
          error: {
            code: 'not_implemented_for_phase',
            message: 'profile list unavailable in phase 4B',
            retryable: false,
            correlationId: 'phase-4b-extra',
            details: {
              type: 'phase_unavailable',
              phase: '4B',
              operation: 'goose.mcpProfileList_unstable',
              extra: 'unexpected',
            },
          },
        },
      } as Record<string, unknown>;
    });

    await expect(listMcpProfiles(true)).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
  });

  it('fails closed on wrong-code, unknown-type, and malformed formal detail payloads', async () => {
    const invalidCases = [
      {
        label: 'wrong code/detail pairing',
        error: {
          code: 'revision_conflict',
          message: 'Authorization: Bearer top-secret',
          retryable: true,
          correlationId: 'wrong-code-ref',
          details: { type: 'record_missing' },
        },
      },
      {
        label: 'unknown detail type',
        error: {
          code: 'revision_conflict',
          message: 'Authorization: Bearer top-secret',
          retryable: true,
          correlationId: 'unknown-detail-ref',
          details: { type: 'future_detail' },
        },
      },
      {
        label: 'malformed detail field',
        error: {
          code: 'invalid_request',
          message: 'Authorization: Bearer top-secret',
          retryable: false,
          correlationId: 'malformed-detail-ref',
          details: { type: 'request_rejected', extra: 'unexpected' },
        },
      },
    ] as const;

    for (const testCase of invalidCases) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return { outcome: { status: 'error', error: testCase.error } } as Record<string, unknown>;
      });

      await expect(getMcpProfile('profile-a'), testCase.label).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
      await getMcpProfile('profile-a').catch((error) => {
        expect((error as Error).message).toBe('The MCP Platform request could not be completed.');
        expect((error as Error).message).not.toContain('top-secret');
      });
    }
  });

  it('fails closed on inherited outcome/error/details and dangerous own keys without surfacing phase unavailable', async () => {
    const inheritedOutcome = Object.create({
      outcome: {
        status: 'success',
        value: { items: [profileSummary] },
      },
    });
    const inheritedErrorDetails = {
      outcome: {
        status: 'error',
        error: {
          code: 'not_implemented_for_phase',
          message: 'profile list unavailable in phase 4B',
          retryable: false,
          correlationId: 'phase-4b-inherited',
          details: Object.assign(Object.create({ type: 'phase_unavailable' }), {
            phase: '4B',
            operation: 'goose.mcpProfileList_unstable',
          }),
        },
      },
    };
    const protoKeyResponse = JSON.parse(
      '{"outcome":{"status":"success","value":{"items":[{"profileId":"profile-a","name":"Team MCPs","description":"Saved MCP set","revision":4,"archived":false,"entries":[{"managedMcpId":"managed-a","ordinal":0},{"managedMcpId":"managed-b","ordinal":1}],"createdAtMs":10,"updatedAtMs":11}]}},"__proto__":{}}'
    ) as Record<string, unknown>;
    const nullPrototypeResponse = Object.assign(Object.create(null), {
      outcome: {
        status: 'success',
        value: { items: [profileSummary] },
      },
    });

    for (const [label, response] of [
      ['inherited outcome', inheritedOutcome],
      ['inherited details', inheritedErrorDetails],
      ['__proto__ own key', protoKeyResponse],
      ['null prototype', nullPrototypeResponse],
    ] as const) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return response;
      });

      await expect(listMcpProfiles(true), label).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
      await listMcpProfiles(true).catch((error) => {
        expect(isMcpPhaseUnavailableError(error, 'profiles')).toBe(false);
      });
    }
  });

  it('rejects accessor and non-enumerable required fields without invoking getters', async () => {
    let getterCalls = 0;
    const accessorResponse: Record<string, unknown> = {};
    Object.defineProperty(accessorResponse, 'outcome', {
      enumerable: true,
      get() {
        getterCalls += 1;
        throw new Error('getter must not be invoked');
      },
    });

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return accessorResponse;
    });

    await expect(listMcpProfiles(true)).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    expect(getterCalls).toBe(0);

    const nonEnumerableProfile = { ...profileSummary };
    Object.defineProperty(nonEnumerableProfile, 'profileId', {
      enumerable: false,
      value: profileSummary.profileId,
    });
    const accessorHistoryEntry = { ...profileDetail.history[0] };
    let nestedGetterCalls = 0;
    Object.defineProperty(accessorHistoryEntry, 'actor', {
      enumerable: true,
      get() {
        nestedGetterCalls += 1;
        throw new Error('nested getter must not be invoked');
      },
    });

    const malformedDetail = {
      outcome: {
        status: 'success',
        value: {
          profile: nonEnumerableProfile,
          history: [accessorHistoryEntry],
        },
      },
    };

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return malformedDetail;
    });

    await expect(getMcpProfile('profile-a')).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    expect(nestedGetterCalls).toBe(0);
  });

  it('rejects accessor array elements in success payloads without invoking getters', async () => {
    let getterCalls = 0;
    const accessorItems: unknown[] = [];
    Object.defineProperty(accessorItems, '0', {
      enumerable: true,
      get() {
        getterCalls += 1;
        throw new Error('array getter must not be invoked');
      },
    });
    accessorItems.length = 1;

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'success',
          value: {
            items: accessorItems,
          },
        },
      };
    });

    await expect(listMcpProfiles(true)).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
    expect(getterCalls).toBe(0);
  });

  it('rejects arrays, dates, and class instances for profile and model payloads', async () => {
    class ProfileSummaryRecord {
      profileId = 'profile-a';
      name = 'Research setup';
      description = 'Browser and fetch MCPs';
      revision = 4;
      archived = false;
      entries = [{ managedMcpId: 'managed-a', ordinal: 0 }];
      createdAtMs = 10;
      updatedAtMs = 11;
    }

    const invalidValues = [
      {
        label: 'array profile',
        method: 'goose.mcpProfileList_unstable',
        value: { items: [[profileSummary]] },
      },
      {
        label: 'date profile',
        method: 'goose.mcpProfileGet_unstable',
        value: { profile: new Date('2026-07-23T00:00:00.000Z'), history: profileDetail.history },
      },
      {
        label: 'class profile',
        method: 'goose.mcpProfileCreate_unstable',
        value: new ProfileSummaryRecord(),
      },
      {
        label: 'date model candidate',
        method: 'goose.mcpProfileModelRecommend_unstable',
        value: { ...modelRecommendation, candidates: [new Date('2026-07-23T00:00:00.000Z')] },
      },
    ] as const;

    for (const invalid of invalidValues) {
      setBoundaryClient(async (method, params) => {
        calls.push({ method, params });
        return method === invalid.method
          ? { outcome: { status: 'success', value: invalid.value } }
          : { outcome: { status: 'success', value: task } };
      });

      const call =
        invalid.method === 'goose.mcpProfileList_unstable'
          ? () => listMcpProfiles(true)
          : invalid.method === 'goose.mcpProfileGet_unstable'
            ? () => getMcpProfile('profile-a')
            : invalid.method === 'goose.mcpProfileCreate_unstable'
              ? () =>
                  createMcpProfile({
                    name: 'Team MCPs',
                    description: 'Saved MCP set',
                    managedMcpIds: ['managed-a'],
                  })
              : () => recommendMcpProfileModels('Need research and browser tools', ['openai']);

      await expect(call(), invalid.label).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('accepts plain JSON parsed responses for formal details and both phase capability values', async () => {
    const parsedCapabilities = {
      sessionEnablement: 'available',
      toolPolicy: 'not_available_in_this_phase',
      profiles: 'available',
      modelSuggestions: 'not_available_in_this_phase',
    } as const;

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      const payload =
        method === 'goose.mcpList_unstable'
          ? {
              outcome: {
                status: 'success',
                value: {
                  items: [{ ...summary, phaseCapabilities: parsedCapabilities }],
                  nextCursor: null,
                },
              },
            }
          : method === 'goose.mcpSetDefaultEnabled_unstable'
            ? {
                outcome: {
                  status: 'success',
                  value: { ...summary, phaseCapabilities: parsedCapabilities },
                },
              }
            : {
                outcome: {
                  status: 'error',
                  error: {
                    code: 'not_implemented_for_phase',
                    message: 'profile list unavailable in phase 4B',
                    retryable: false,
                    correlationId: 'phase-4b-json',
                    details: {
                      type: 'phase_unavailable',
                      phase: '4B',
                      operation: 'goose.mcpProfileList_unstable',
                    },
                  },
                },
              };
      return JSON.parse(JSON.stringify(payload)) as Record<string, unknown>;
    });

    await expect(listManagedMcps()).resolves.toEqual({
      items: [{ ...summary, phaseCapabilities: parsedCapabilities }],
      nextCursor: null,
    });
    await expect(
      setMcpDefaultEnabled({ managedMcpId: 'managed-a', revision: 17 }, true)
    ).resolves.toEqual({ ...summary, phaseCapabilities: parsedCapabilities });

    await expect(listMcpProfiles(true)).rejects.toMatchObject({
      envelope: {
        code: 'not_implemented_for_phase',
        details: {
          type: 'phase_unavailable',
          phase: '4B',
          operation: 'goose.mcpProfileList_unstable',
        },
      },
    });
  });

  it('preserves opaque identifiers exactly across responses and request params', async () => {
    const preservedProfileId = 'profile-a\t';
    const distinctProfileId = 'profile-a\r';
    const preservedManagedId = 'managed-\u03b1\n';
    const confusableProviderId = 'opena\u0456';
    const preservedModelId = 'model-\u03b1';
    const extMethod = async (method: string, params: Record<string, unknown>) => {
      calls.push({ method, params });
      const value =
        method === 'goose.mcpProfileGet_unstable'
          ? {
              profile: {
                ...profileSummary,
                profileId: preservedProfileId,
                entries: [{ managedMcpId: preservedManagedId, ordinal: 0 }],
                name: 'Team\u0000 MCPs',
                description: 'Saved\u0007 MCP set',
              },
              history: profileDetail.history,
            }
          : method === 'goose.mcpProfileModelRecommend_unstable'
            ? {
                ...modelRecommendation,
                candidates: [
                  {
                    ...modelRecommendation.candidates[0],
                    providerId: confusableProviderId,
                    modelId: preservedModelId,
                    reasonCodes: ['inventory_reasoning_match'],
                    caveatCodes: ['context_limit_unknown'],
                  },
                ],
              }
            : method === 'goose.mcpProfileList_unstable'
              ? {
                  items: [
                    { ...profileSummary, profileId: preservedProfileId, name: 'Team\u0000 MCPs' },
                    {
                      ...profileSummary,
                      profileId: distinctProfileId,
                      name: 'Second\u0007 profile',
                    },
                  ],
                }
              : { ...profileSummary, name: 'Team\u0000 MCPs' };
      return { outcome: { status: 'success', value } };
    };
    setBoundaryClient(extMethod);

    await expect(listMcpProfiles(true)).resolves.toEqual({
      items: [
        { ...profileSummary, profileId: preservedProfileId, name: 'Team MCPs' },
        { ...profileSummary, profileId: distinctProfileId, name: 'Second profile' },
      ],
    });
    await expect(getMcpProfile(preservedProfileId)).resolves.toMatchObject({
      profile: {
        profileId: preservedProfileId,
        name: 'Team MCPs',
        description: 'Saved MCP set',
        entries: [{ managedMcpId: preservedManagedId, ordinal: 0 }],
      },
    });
    await expect(
      recommendMcpProfileModels('Need research and browser tools', [confusableProviderId])
    ).resolves.toMatchObject({
      candidates: [
        {
          providerId: confusableProviderId,
          modelId: preservedModelId,
          reasonCodes: ['inventory_reasoning_match'],
          caveatCodes: ['context_limit_unknown'],
        },
      ],
    });

    await updateMcpProfile({
      profileId: preservedProfileId,
      expectedRevision: 4,
      name: 'Team MCPs',
      description: 'Updated',
      managedMcpIds: [preservedManagedId],
    });
    await archiveMcpProfile({ profileId: preservedProfileId, expectedRevision: 4 });

    expect(calls.find(({ method }) => method === 'goose.mcpProfileGet_unstable')?.params).toEqual({
      profileId: preservedProfileId,
    });
    expect(
      calls.find(({ method }) => method === 'goose.mcpProfileModelRecommend_unstable')?.params
    ).toEqual({
      text: 'Need research and browser tools',
      providerIds: [confusableProviderId],
    });
    expect(calls[calls.length - 2]?.params).toMatchObject({
      profileId: preservedProfileId,
      managedMcpIds: [preservedManagedId],
    });
    expect(calls[calls.length - 1]?.params).toMatchObject({
      profileId: preservedProfileId,
    });
  });

  it('fails closed when profile identifiers violate the backend profile grammar', async () => {
    for (const invalidProfileId of ['\t\r\n', 'profile-a\u0000', 'p'.repeat(257)]) {
      const extMethod = async (method: string, params: Record<string, unknown>) => {
        calls.push({ method, params });
        const value =
          method === 'goose.mcpProfileGet_unstable'
            ? {
                profile: { ...profileSummary, profileId: invalidProfileId },
                history: profileDetail.history,
              }
            : { items: [{ ...profileSummary, profileId: invalidProfileId }] };
        return { outcome: { status: 'success', value } };
      };
      setBoundaryClient(extMethod);

      await expect(listMcpProfiles(true)).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
      await expect(getMcpProfile('profile-a')).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
  });

  it('fails closed when provider or model identifiers violate their stricter backend grammar', async () => {
    for (const candidate of [
      { providerId: 'openai\t', modelId: 'gpt-5' },
      { providerId: 'openai', modelId: 'gpt-5\r' },
    ]) {
      const extMethod = async (method: string, params: Record<string, unknown>) => {
        calls.push({ method, params });
        const value =
          method === 'goose.mcpProfileModelRecommend_unstable'
            ? {
                ...modelRecommendation,
                candidates: [{ ...modelRecommendation.candidates[0], ...candidate }],
              }
            : { items: [profileSummary] };
        return { outcome: { status: 'success', value } };
      };
      setBoundaryClient(extMethod);

      await expect(
        recommendMcpProfileModels('Need research and browser tools', ['openai'])
      ).rejects.toThrow('The MCP Platform request could not be completed.');
    }
  });

  it('keeps public task facade support refs opaque when unknown failures lack correlation ids', async () => {
    const publicTask = mapPublicMcpTask(task);
    const rawMessage =
      'Authorization: Bearer top-secret token=abc123 path=/tmp/private ' +
      'C:\\\\sensitive\\\\token.txt https://private.example.test/query?token=abc123';

    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      throw new Error(rawMessage);
    });

    await expect(getPublicMcpTask(publicTask.reference)).rejects.toMatchObject({
      code: 'unknown',
      retryable: false,
      userCopyKey: 'mcpCenter.safeError.generic',
      supportRef: expect.stringMatching(/^MCP-(?:[0-9A-F]{32}|UNAVAILABLE)$/),
    });

    await getPublicMcpTask(publicTask.reference).catch((error) => {
      const safeError = error as { supportRef: string };
      expect(safeError.supportRef).not.toContain('top-secret');
      expect(safeError.supportRef).not.toContain('abc123');
      expect(safeError.supportRef).not.toContain('/tmp/private');
      expect(safeError.supportRef).not.toContain('sensitive');
    });
  });

  it('uses the HTTPS manifest ACP methods and never returns the confirmation token from confirm', async () => {
    const token = 'manifest-confirmation-secret';
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      const preview = {
        manifestId: 'manifest-1', version: '1', redactedOrigin: 'https://example.test',
        rawDigest: 'raw-1', parsedDigest: 'parsed-1', redirectChainDigest: 'redirect-1',
        dnsEvidenceDigest: 'dns-1', warnings: [],
      };
      return method === 'goose.mcpHttpsManifestPrepare_unstable'
        ? { outcome: { status: 'success', value: { provisionId: 'provision-1', confirmationToken: token, expiresAtMs: 10, preview } } }
        : { outcome: { status: 'success', value: { manifestDigest: 'manifest-digest-1', preview } } };
    });

    await expect(prepareHttpsMcpManifest('https://example.test/manifest')).resolves.toMatchObject({
      provisionId: 'provision-1', confirmationToken: token,
    });
    await expect(confirmHttpsMcpManifest({ provisionId: 'provision-1', confirmationToken: token, confirm: true }))
      .resolves.toMatchObject({ manifestDigest: 'manifest-digest-1' });
    expect(calls[0]).toEqual({ method: 'goose.mcpHttpsManifestPrepare_unstable', params: { url: 'https://example.test/manifest' } });
    expect(calls[1]).toEqual({ method: 'goose.mcpHttpsManifestConfirm_unstable', params: { provisionId: 'provision-1', confirmationToken: token, confirm: true } });
  });

  it('strips runtime extension fields before sending HTTPS manifest confirm params', async () => {
    const token = 'runtime-confirmation-secret';
    setBoundaryClient(async (method, params) => {
      calls.push({ method, params });
      return {
        outcome: {
          status: 'success',
          value: {
            manifestDigest: 'manifest-digest-1',
            preview: {
              manifestId: 'manifest-1', version: '1', redactedOrigin: 'https://example.test',
              rawDigest: 'raw-1', parsedDigest: 'parsed-1', redirectChainDigest: 'redirect-1',
              dnsEvidenceDigest: 'dns-1',
            },
          },
        },
      };
    });

    const input = {
      provisionId: 'provision-1',
      confirmationToken: token,
      confirm: true,
      idempotencyKey: 'must-not-cross-boundary',
      endpoint: 'https://attacker.example.test',
      secret: 'must-not-cross-boundary',
    } as any;

    await expect(confirmHttpsMcpManifest(input)).resolves.toEqual({
      manifestDigest: 'manifest-digest-1',
      preview: {
        manifestId: 'manifest-1', version: '1', redactedOrigin: 'https://example.test',
        rawDigest: 'raw-1', parsedDigest: 'parsed-1', redirectChainDigest: 'redirect-1',
        dnsEvidenceDigest: 'dns-1', warnings: [],
      },
    });
    expect(calls[0]).toEqual({
      method: 'goose.mcpHttpsManifestConfirm_unstable',
      params: { provisionId: 'provision-1', confirmationToken: token, confirm: true },
    });
    expect(JSON.stringify(calls[0]?.params)).not.toContain('must-not-cross-boundary');
    expect(JSON.stringify(calls[0]?.params)).toContain(token);
  });

  it('rejects unknown fields, empty digests, and errors containing the token', async () => {
    const token = 'token-that-must-not-leak';
    for (const value of [
      { manifestDigest: '', preview: {} },
      { manifestDigest: 'digest-1', preview: { manifestId: 'm', version: '1', redactedOrigin: 'https://e.test', rawDigest: 'r', parsedDigest: 'p', redirectChainDigest: 'c', dnsEvidenceDigest: 'd', sensitive: token } },
    ]) {
      setBoundaryClient(async () => ({ outcome: { status: 'success', value } }));
      await expect(confirmHttpsMcpManifest({ provisionId: 'p', confirmationToken: token, confirm: true })).rejects.toThrow(
        'The MCP Platform request could not be completed.'
      );
    }
    setBoundaryClient(async () => ({ outcome: { status: 'error', error: { code: 'invalid_request', message: token, retryable: false } } }));
    await expect(confirmHttpsMcpManifest({ provisionId: 'p', confirmationToken: token, confirm: false })).rejects.toThrow(
      'The MCP Platform request could not be completed.'
    );
  });
});
