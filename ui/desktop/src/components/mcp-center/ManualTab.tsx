import { useEffect, useState } from 'react';
import type { McpManualStdioSourcesPage, McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../ui/tabs';
import {
  createManualMcpPlan,
  listManualStdioSources,
  toMcpRecoveryViewModel,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import { RecoveryPanel, StatePanel, StatusBadge } from './McpCenterCommon';
import { PlanReviewDialog } from './PlanReviewDialog';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export function ManualTab({ onTaskCreated }: { onTaskCreated: (task: McpTaskRef) => void }) {
  const intl = useIntl();
  const [endpoint, setEndpoint] = useState('');
  const [authMode, setAuthMode] = useState<'none' | 'bearer_reference'>('none');
  const [authReference, setAuthReference] = useState('');
  const [stdio, setStdio] = useState<McpManualStdioSourcesPage | null>(null);
  const [selectedSource, setSelectedSource] = useState('');
  const [loadingStdio, setLoadingStdio] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [plan, setPlan] = useState<McpPlanReview | null>(null);

  const loadStdio = async () => {
    setLoadingStdio(true);
    setError(null);
    try {
      const page = await listManualStdioSources();
      setStdio(page);
      setSelectedSource((current) => current || page.items[0]?.sourceId || '');
    } catch (cause) {
      setError(toMcpRecoveryViewModel(cause));
    } finally {
      setLoadingStdio(false);
    }
  };

  useEffect(() => {
    void loadStdio();
  }, []);

  const submitRemote = async () => {
    setSubmitting(true);
    setError(null);
    try {
      setPlan(
        await createManualMcpPlan({
          type: 'remote_http',
          endpoint: endpoint.trim(),
          auth:
            authMode === 'none'
              ? { type: 'none' }
              : { type: 'bearer_reference', authReference: authReference.trim() },
        })
      );
    } catch (cause) {
      setError(toMcpRecoveryViewModel(cause));
    } finally {
      setSubmitting(false);
    }
  };

  const submitStdio = async () => {
    if (!selectedSource) return;
    setSubmitting(true);
    setError(null);
    try {
      setPlan(await createManualMcpPlan({ type: 'stdio_provider', sourceId: selectedSource }));
    } catch (cause) {
      setError(toMcpRecoveryViewModel(cause));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="mx-auto max-w-4xl space-y-5">
      {error && <RecoveryPanel recovery={error} />}
      <Tabs defaultValue="remote">
        <TabsList aria-label={intl.formatMessage(messages.manualTypeLabel)}>
          <TabsTrigger value="remote">{intl.formatMessage(messages.remoteHttp)}</TabsTrigger>
          <TabsTrigger value="stdio">{intl.formatMessage(messages.approvedStdio)}</TabsTrigger>
        </TabsList>

        <TabsContent value="remote">
          <form
            className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5"
            onSubmit={(event) => {
              event.preventDefault();
              void submitRemote();
            }}
          >
            <div>
              <h2 className="text-lg font-medium text-text-primary">
                {intl.formatMessage(messages.remoteConnection)}
              </h2>
              <p className="mt-1 text-sm text-text-secondary">
                {intl.formatMessage(messages.remoteConnectionDescription)}
              </p>
            </div>
            <label className="block space-y-1.5 text-sm text-text-primary">
              {intl.formatMessage(messages.httpsEndpoint)}
              <Input
                type="url"
                required
                value={endpoint}
                onChange={(event) => setEndpoint(event.target.value)}
                placeholder={intl.formatMessage(messages.endpointPlaceholder)}
              />
            </label>
            <label className="block space-y-1.5 text-sm text-text-primary">
              {intl.formatMessage(messages.authentication)}
              <select
                className="h-9 w-full rounded-md border border-border-primary bg-background-primary px-3 text-sm"
                value={authMode}
                onChange={(event) => setAuthMode(event.target.value as typeof authMode)}
              >
                <option value="none">{intl.formatMessage(messages.noAuthentication)}</option>
                <option value="bearer_reference">
                  {intl.formatMessage(messages.bearerReference)}
                </option>
              </select>
            </label>
            {authMode === 'bearer_reference' && (
              <label className="block space-y-1.5 text-sm text-text-primary">
                {intl.formatMessage(messages.credentialReference)}
                <Input
                  required
                  value={authReference}
                  onChange={(event) => setAuthReference(event.target.value)}
                  placeholder={intl.formatMessage(messages.existingCredentialReference)}
                  autoComplete="off"
                />
                <span className="block text-xs text-text-secondary">
                  {intl.formatMessage(messages.credentialReferenceHelp)}
                </span>
              </label>
            )}
            <Button disabled={submitting || !endpoint.trim()} type="submit">
              {intl.formatMessage(
                submitting ? messages.creatingPlan : messages.createConnectionPlan
              )}
            </Button>
          </form>
        </TabsContent>

        <TabsContent value="stdio">
          <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
            <div>
              <h2 className="text-lg font-medium text-text-primary">
                {intl.formatMessage(messages.approvedStdio)}
              </h2>
              <p className="mt-1 text-sm text-text-secondary">
                {intl.formatMessage(messages.approvedStdioDescription)}
              </p>
            </div>
            {loadingStdio ? (
              <StatePanel
                kind="loading"
                title={intl.formatMessage(messages.loadingApprovedProviders)}
                description={intl.formatMessage(messages.readingProviderPolicy)}
              />
            ) : !stdio || stdio.provider === 'operation_not_supported' ? (
              <StatePanel
                kind="empty"
                title={intl.formatMessage(messages.stdioUnavailable)}
                description={intl.formatMessage(messages.stdioUnavailableDescription)}
              />
            ) : stdio.items.length === 0 ? (
              <StatePanel
                kind="empty"
                title={intl.formatMessage(messages.noApprovedStdio)}
                description={intl.formatMessage(messages.noApprovedStdioDescription)}
              />
            ) : (
              <>
                <fieldset className="grid gap-2">
                  <legend className="mb-2 text-sm font-medium text-text-primary">
                    {intl.formatMessage(messages.providerSource)}
                  </legend>
                  {stdio.items.map((item) => (
                    <label
                      key={item.sourceId}
                      className="flex cursor-pointer items-center gap-3 rounded-lg border border-border-primary p-3 hover:bg-background-secondary"
                    >
                      <input
                        type="radio"
                        name="stdio-source"
                        value={item.sourceId}
                        checked={selectedSource === item.sourceId}
                        onChange={() => setSelectedSource(item.sourceId)}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium text-text-primary">
                          {item.displayName}
                        </span>
                        <span className="block truncate text-sm text-text-secondary">
                          {item.publisherName}
                        </span>
                      </span>
                      <StatusBadge
                        tone={item.compatibility === 'compatible' ? 'success' : 'danger'}
                      >
                        {intl.formatMessage(
                          item.compatibility === 'compatible'
                            ? messages.compatible
                            : messages.incompatible
                        )}
                      </StatusBadge>
                    </label>
                  ))}
                </fieldset>
                <Button disabled={submitting || !selectedSource} onClick={() => void submitStdio()}>
                  {intl.formatMessage(
                    submitting ? messages.creatingPlan : messages.createProviderPlan
                  )}
                </Button>
              </>
            )}
          </div>
        </TabsContent>
      </Tabs>

      <PlanReviewDialog
        plan={plan}
        operation="register"
        onClose={() => setPlan(null)}
        onTaskCreated={onTaskCreated}
      />
    </div>
  );
}
