import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CheckCircle2, Circle, LoaderCircle, XCircle } from 'lucide-react';
import type { ProviderDetails } from '../../types/providers';
import { acpListProviderDetails } from '../../acp/providers';
import {
  testMcpProfileConnection,
  type McpConnectionTestPhase,
  type McpConnectionTestStatus,
  type McpProfileConnectionTestResult,
  type McpProfileSummary,
} from '../../acp/mcp-platform';
import { useIntl } from '../../i18n';
import { Button } from '../ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { StatusBadge } from './McpCenterCommon';
import { mcpCenterMessages as messages } from './messages';

const connectionTestPhases: McpConnectionTestPhase[] = [
  'eligibility',
  'mcp_connect_initialize',
  'tool_discovery',
  'cleanup',
  'model_request',
  'tool_visibility',
];

const safeConnectionTestFailureCodes = new Set([
  'connection_test_failed',
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
]);

type ProviderOption = {
  providerId: string;
  displayName: string;
  models: string[];
};

function configuredProviderOptions(providers: ProviderDetails[]): ProviderOption[] {
  return providers
    .filter((provider) => provider.is_configured && provider.metadata.known_models.length > 0)
    .map((provider) => ({
      providerId: provider.name,
      displayName: provider.metadata.display_name || provider.name,
      models: provider.metadata.known_models.map((model) => model.name),
    }));
}

function initialModel(
  provider: ProviderDetails | undefined,
  option: ProviderOption | undefined
): string {
  if (!provider || !option) return '';
  return option.models.includes(provider.metadata.default_model)
    ? provider.metadata.default_model
    : (option.models[0] ?? '');
}

function safeErrorCode(error: unknown): string {
  if (
    error &&
    typeof error === 'object' &&
    'envelope' in error &&
    error.envelope &&
    typeof error.envelope === 'object' &&
    'code' in error.envelope &&
    typeof error.envelope.code === 'string'
  ) {
    return safeConnectionTestFailureCodes.has(error.envelope.code)
      ? error.envelope.code
      : 'connection_test_failed';
  }
  return 'connection_test_failed';
}

function stageLabel(phase: McpConnectionTestPhase, intl: ReturnType<typeof useIntl>): string {
  const labels = {
    eligibility: messages.profileConnectionTestStageEligibility,
    mcp_connect_initialize: messages.profileConnectionTestStageMcpInitialize,
    tool_discovery: messages.profileConnectionTestStageToolDiscovery,
    cleanup: messages.profileConnectionTestStageCleanup,
    model_request: messages.profileConnectionTestStageModelRequest,
    tool_visibility: messages.profileConnectionTestStageToolVisibility,
  } satisfies Record<McpConnectionTestPhase, (typeof messages)[keyof typeof messages]>;
  return intl.formatMessage(labels[phase]);
}

function StageStatusIcon({ status }: { status: McpConnectionTestStatus | 'pending' | 'running' }) {
  if (status === 'passed') return <CheckCircle2 className="h-4 w-4 text-text-success" />;
  if (status === 'failed') return <XCircle className="h-4 w-4 text-text-danger" />;
  if (status === 'running') return <LoaderCircle className="h-4 w-4 animate-spin text-text-info" />;
  return <Circle className="h-4 w-4 text-text-tertiary" />;
}

function statusLabel(
  status: McpConnectionTestStatus | 'pending' | 'running',
  intl: ReturnType<typeof useIntl>
): string {
  if (status === 'pending') return intl.formatMessage(messages.profileConnectionTestPending);
  if (status === 'running') return intl.formatMessage(messages.profileConnectionTestRunningStage);
  if (status === 'skipped') return intl.formatMessage(messages.profileConnectionTestSkipped);
  return status;
}

function displayStageCode(code: string): string {
  return code === 'tool_visibility_validated'
    ? 'tool_definitions_withheld_safe_isolation'
    : code;
}

export function ProfileConnectionTestDialog({
  profile,
  open,
  onOpenChange,
}: {
  profile: McpProfileSummary;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const intl = useIntl();
  const [providers, setProviders] = useState<ProviderDetails[]>([]);
  const [providersLoading, setProvidersLoading] = useState(false);
  const [providersLoadFailed, setProvidersLoadFailed] = useState(false);
  const [providerId, setProviderId] = useState('');
  const [modelId, setModelId] = useState('');
  const [result, setResult] = useState<McpProfileConnectionTestResult | null>(null);
  const [runErrorCode, setRunErrorCode] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const providersRequestRef = useRef(0);
  const testRequestRef = useRef(0);
  const testScopeRef = useRef({ profileId: profile.profileId, open, providerId, modelId });
  testScopeRef.current = { profileId: profile.profileId, open, providerId, modelId };

  const invalidateTest = useCallback(() => {
    testRequestRef.current += 1;
    setTesting(false);
  }, []);

  const options = useMemo(() => configuredProviderOptions(providers), [providers]);
  const selectedOption = options.find((option) => option.providerId === providerId) ?? null;

  const loadProviders = useCallback(async () => {
    const requestId = ++providersRequestRef.current;
    setProvidersLoading(true);
    setProvidersLoadFailed(false);
    try {
      const nextProviders = await acpListProviderDetails();
      if (providersRequestRef.current !== requestId) return;
      const nextOptions = configuredProviderOptions(nextProviders);
      const firstProvider = nextProviders.find(
        (provider) => provider.name === nextOptions[0]?.providerId
      );
      setProviders(nextProviders);
      setProviderId(nextOptions[0]?.providerId ?? '');
      setModelId(initialModel(firstProvider, nextOptions[0]));
    } catch {
      if (providersRequestRef.current !== requestId) return;
      setProviders([]);
      setProviderId('');
      setModelId('');
      setProvidersLoadFailed(true);
    } finally {
      if (providersRequestRef.current === requestId) setProvidersLoading(false);
    }
  }, []);

  useEffect(() => {
    invalidateTest();
    setResult(null);
    setRunErrorCode(null);
    if (!open) return;
    void loadProviders();
    return () => {
      providersRequestRef.current += 1;
      invalidateTest();
    };
  }, [invalidateTest, loadProviders, open, profile.profileId]);

  const selectProvider = (nextProviderId: string) => {
    invalidateTest();
    const nextOption = options.find((option) => option.providerId === nextProviderId);
    const provider = providers.find((item) => item.name === nextProviderId);
    setProviderId(nextProviderId);
    setModelId(initialModel(provider, nextOption));
    setResult(null);
    setRunErrorCode(null);
  };

  const runTest = async () => {
    if (
      testing ||
      !selectedOption ||
      !selectedOption.models.includes(modelId) ||
      !options.some((option) => option.providerId === providerId)
    ) {
      return;
    }
    const requestId = ++testRequestRef.current;
    const requestProfileId = profile.profileId;
    const requestProviderId = providerId;
    const requestModelId = modelId;
    const isCurrentRequest = () => {
      const scope = testScopeRef.current;
      return (
        testRequestRef.current === requestId &&
        scope.open &&
        scope.profileId === requestProfileId &&
        scope.providerId === requestProviderId &&
        scope.modelId === requestModelId
      );
    };
    setTesting(true);
    setResult(null);
    setRunErrorCode(null);
    try {
      const refreshedProviders = await acpListProviderDetails();
      if (!isCurrentRequest()) return;
      const refreshedOptions = configuredProviderOptions(refreshedProviders);
      const refreshedOption = refreshedOptions.find(
        (option) => option.providerId === requestProviderId
      );
      setProviders(refreshedProviders);
      if (!refreshedOption || !refreshedOption.models.includes(requestModelId)) {
        const fallbackProvider = refreshedProviders.find(
          (provider) => provider.name === refreshedOptions[0]?.providerId
        );
        setProviderId(refreshedOptions[0]?.providerId ?? '');
        setModelId(initialModel(fallbackProvider, refreshedOptions[0]));
        setRunErrorCode('connection_test_failed');
        return;
      }
      const nextResult = await testMcpProfileConnection({
        profileId: requestProfileId,
        providerId: requestProviderId,
        modelId: requestModelId,
      });
      if (!isCurrentRequest()) return;
      setResult(nextResult);
    } catch (error) {
      if (!isCurrentRequest()) return;
      setRunErrorCode(safeErrorCode(error));
    } finally {
      if (isCurrentRequest()) setTesting(false);
    }
  };

  const stages: Array<{
    phase: McpConnectionTestPhase;
    status: McpConnectionTestStatus | 'pending' | 'running';
    code?: McpProfileConnectionTestResult['stages'][number]['code'];
  }> = result
    ? result.stages
    : connectionTestPhases.map((phase, index) => ({
        phase,
        status: testing && index === 0 ? 'running' : 'pending',
      }));
  const canRun =
    !providersLoading &&
    !providersLoadFailed &&
    !!selectedOption &&
    selectedOption.models.includes(modelId) &&
    !testing;

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) invalidateTest();
        onOpenChange(nextOpen);
      }}
    >
      <DialogContent className="max-h-[calc(100dvh-2rem)] max-w-2xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage(messages.profileConnectionTestTitle)}</DialogTitle>
          <DialogDescription>
            {intl.formatMessage(messages.profileConnectionTestDescription)}
          </DialogDescription>
        </DialogHeader>

        <p className="rounded-lg border border-border-info bg-background-info/5 p-3 text-sm text-text-secondary">
          {intl.formatMessage(messages.profileConnectionTestBoundary)}
        </p>

        {providersLoading ? (
          <p role="status" className="text-sm text-text-secondary">
            {intl.formatMessage(messages.profileConnectionTestLoadingProviders)}
          </p>
        ) : providersLoadFailed ? (
          <div
            role="alert"
            className="space-y-3 rounded-lg border border-border-danger bg-background-danger/5 p-4"
          >
            <p className="text-sm text-text-primary">
              {intl.formatMessage(messages.profileConnectionTestLoadFailed)}
            </p>
            <Button variant="outline" size="sm" onClick={() => void loadProviders()}>
              {intl.formatMessage(messages.profileConnectionTestRetryProviders)}
            </Button>
          </div>
        ) : options.length === 0 ? (
          <p
            role="status"
            className="rounded-lg border border-border-primary p-4 text-sm text-text-secondary"
          >
            {intl.formatMessage(messages.profileConnectionTestNoProviders)}
          </p>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-1.5">
              <label
                htmlFor="connection-test-provider"
                className="text-sm font-medium text-text-primary"
              >
                {intl.formatMessage(messages.profileConnectionTestProviderLabel)}
              </label>
              <select
                id="connection-test-provider"
                value={providerId}
                disabled={testing}
                onChange={(event) => selectProvider(event.target.value)}
                className="flex w-full rounded-md border border-border-primary bg-background-primary px-3 py-2 text-sm text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info disabled:cursor-not-allowed disabled:opacity-60"
              >
                {options.map((option) => (
                  <option key={option.providerId} value={option.providerId}>
                    {option.displayName}
                  </option>
                ))}
              </select>
            </div>
            <div className="space-y-1.5">
              <label
                htmlFor="connection-test-model"
                className="text-sm font-medium text-text-primary"
              >
                {intl.formatMessage(messages.profileConnectionTestModelLabel)}
              </label>
              <select
                id="connection-test-model"
                value={modelId}
                disabled={testing || !selectedOption}
                onChange={(event) => {
                  invalidateTest();
                  setModelId(event.target.value);
                  setResult(null);
                  setRunErrorCode(null);
                }}
                className="flex w-full rounded-md border border-border-primary bg-background-primary px-3 py-2 text-sm text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info disabled:cursor-not-allowed disabled:opacity-60"
              >
                {selectedOption?.models.map((model) => (
                  <option key={model} value={model}>
                    {model}
                  </option>
                ))}
              </select>
            </div>
          </div>
        )}

        {(testing || result || runErrorCode) && (
          <div aria-live="polite" className="space-y-3 rounded-lg border border-border-primary p-4">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="font-medium text-text-primary">
                {testing
                  ? intl.formatMessage(messages.profileConnectionTestRunning)
                  : result?.passed
                    ? intl.formatMessage(messages.profileConnectionTestPassed)
                    : intl.formatMessage(messages.profileConnectionTestFailed)}
              </h3>
              {result && (
                <StatusBadge tone={result.passed ? 'success' : 'danger'}>
                  {result.passed
                    ? intl.formatMessage(messages.profileConnectionTestPassed)
                    : intl.formatMessage(messages.profileConnectionTestFailed)}
                </StatusBadge>
              )}
            </div>
            <ol className="space-y-2">
              {stages.map((stage, index) => (
                <li
                  key={`${stage.phase}-${index}`}
                  className="flex items-center justify-between gap-3 text-sm"
                >
                  <span className="flex min-w-0 items-center gap-2 text-text-primary">
                    <StageStatusIcon status={stage.status} />
                    <span>{stageLabel(stage.phase, intl)}</span>
                  </span>
                  <span className="flex shrink-0 items-center gap-2">
                    <StatusBadge
                      tone={
                        stage.status === 'passed'
                          ? 'success'
                          : stage.status === 'failed'
                            ? 'danger'
                            : stage.status === 'running'
                              ? 'info'
                              : 'neutral'
                      }
                    >
                      {statusLabel(stage.status, intl)}
                    </StatusBadge>
                    {stage.code && (
                      <span className="font-mono text-xs text-text-tertiary">
                        {intl.formatMessage(messages.profileConnectionTestCode, {
                          code: displayStageCode(stage.code),
                        })}
                      </span>
                    )}
                  </span>
                </li>
              ))}
            </ol>
            {runErrorCode && (
              <div
                role="alert"
                className="rounded-md border border-border-danger bg-background-danger/5 p-3"
              >
                <p className="text-sm text-text-primary">
                  {intl.formatMessage(messages.profileConnectionTestSafeFailure)}
                </p>
                <p className="mt-1 font-mono text-xs text-text-tertiary">
                  {intl.formatMessage(messages.profileConnectionTestCode, { code: runErrorCode })}
                </p>
              </div>
            )}
          </div>
        )}

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => {
              invalidateTest();
              onOpenChange(false);
            }}
          >
            {intl.formatMessage(messages.cancel)}
          </Button>
          <Button disabled={!canRun} onClick={() => void runTest()}>
            {intl.formatMessage(
              testing
                ? messages.profileConnectionTestRunning
                : result || runErrorCode
                  ? messages.profileConnectionTestRetry
                  : messages.profileConnectionTestRun
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
