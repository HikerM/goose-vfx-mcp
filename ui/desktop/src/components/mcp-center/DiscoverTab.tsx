import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type {
  McpCatalogDetail,
  McpCatalogSummary,
  McpPlanReview,
  McpSourcesPolicyState,
  McpTaskRef,
  McpTrustTier,
} from '@aaif/goose-sdk';
import { Search } from 'lucide-react';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import {
  createMcpPlan,
  getMcpCatalogDetail,
  listMcpCatalog,
  toMcpRecoveryViewModel,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import {
  DefinitionList,
  formatMcpValue,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
} from './McpCenterCommon';
import { PlanReviewDialog } from './PlanReviewDialog';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export function DiscoverTab({
  sourcesPolicy,
  onTaskCreated,
}: {
  sourcesPolicy: McpSourcesPolicyState | null;
  onTaskCreated: (task: McpTaskRef) => void;
}) {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [trustTier, setTrustTier] = useState<McpTrustTier | ''>('');
  const [compatibility, setCompatibility] = useState<'' | 'compatible' | 'incompatible'>('');
  const [sourceId, setSourceId] = useState('');
  const [items, setItems] = useState<McpCatalogSummary[]>([]);
  const [nextCursor, setNextCursor] = useState<string>();
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [selected, setSelected] = useState<McpCatalogSummary | null>(null);
  const [detail, setDetail] = useState<McpCatalogDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [planning, setPlanning] = useState(false);
  const [plan, setPlan] = useState<McpPlanReview | null>(null);
  const [planOperation, setPlanOperation] = useState<McpTaskRef['operation'] | null>(null);
  const searchSequence = useRef(0);
  const selectionSequence = useRef(0);

  const search = useCallback(
    async (cursor?: string) => {
      const sequence = ++searchSequence.current;
      if (cursor) setLoadingMore(true);
      else setLoading(true);
      setError(null);
      try {
        const page = await listMcpCatalog({
          query: query.trim() || undefined,
          trustTiers: trustTier ? [trustTier] : undefined,
          compatibility: compatibility || undefined,
          sourceIds: sourceId ? [sourceId] : undefined,
          cursor,
          pageSize: 24,
        });
        if (searchSequence.current !== sequence) return;
        setItems((current) => (cursor ? [...current, ...page.items] : page.items));
        setNextCursor(page.nextCursor);
      } catch (cause) {
        if (searchSequence.current !== sequence) return;
        setError(toMcpRecoveryViewModel(cause));
      } finally {
        if (searchSequence.current === sequence) {
          setLoading(false);
          setLoadingMore(false);
        }
      }
    },
    [compatibility, query, sourceId, trustTier]
  );

  useEffect(() => {
    void search();
  }, [search]);

  const selectItem = async (item: McpCatalogSummary) => {
    const sequence = ++selectionSequence.current;
    setSelected(item);
    setDetail(null);
    setDetailError(null);
    setDetailLoading(true);
    try {
      const nextDetail = await getMcpCatalogDetail(item.manifestDigest);
      if (selectionSequence.current !== sequence) return;
      setDetail(nextDetail);
    } catch (cause) {
      if (selectionSequence.current !== sequence) return;
      setDetailError(toMcpRecoveryViewModel(cause));
    } finally {
      if (selectionSequence.current === sequence) setDetailLoading(false);
    }
  };

  const createPlan = async () => {
    if (!selected) return;
    const ownerDigest = selected.manifestDigest;
    const sequence = selectionSequence.current;
    setPlanning(true);
    setDetailError(null);
    try {
      const registrationOnly = ['remote_http', 'manual_stdio'].includes(selected.distribution);
      const operation = registrationOnly ? 'register' : 'install';
      const nextPlan = await createMcpPlan(
        registrationOnly
          ? {
              type: 'register',
              manifest_digest: selected.manifestDigest,
              installation_scope: 'user',
            }
          : { type: 'install', manifest_digest: selected.manifestDigest }
      );
      if (selectionSequence.current !== sequence || selected?.manifestDigest !== ownerDigest) {
        return;
      }
      setPlanOperation(operation);
      setPlan(nextPlan);
    } catch (cause) {
      if (selectionSequence.current !== sequence) return;
      setDetailError(toMcpRecoveryViewModel(cause));
    } finally {
      if (selectionSequence.current === sequence) setPlanning(false);
    }
  };

  const sourceOptions = useMemo(() => sourcesPolicy?.sources ?? [], [sourcesPolicy]);

  return (
    <div className="grid min-h-0 gap-5 xl:grid-cols-[minmax(0,1fr)_minmax(320px,0.42fr)]">
      <section className="min-w-0 space-y-4" aria-label={intl.formatMessage(messages.catalogLabel)}>
        <form
          className="grid gap-2 rounded-xl border border-border-primary bg-background-primary p-3 md:grid-cols-[minmax(220px,1fr)_repeat(3,minmax(140px,0.35fr))]"
          onSubmit={(event) => {
            event.preventDefault();
            void search();
          }}
        >
          <label className="relative">
            <span className="sr-only">{intl.formatMessage(messages.searchCatalog)}</span>
            <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-text-secondary" />
            <Input
              value={query}
              onChange={(event) => {
                searchSequence.current += 1;
                setQuery(event.target.value);
              }}
              placeholder={intl.formatMessage(messages.searchMcps)}
              className="pl-9"
            />
          </label>
          <label>
            <span className="sr-only">{intl.formatMessage(messages.trustLevel)}</span>
            <select
              className="h-9 w-full rounded-md border border-border-primary bg-background-primary px-3 text-sm"
              value={trustTier}
              onChange={(event) => {
                searchSequence.current += 1;
                setTrustTier(event.target.value as McpTrustTier | '');
              }}
            >
              <option value="">{intl.formatMessage(messages.allTrustLevels)}</option>
              <option value="official">{intl.formatMessage(messages.official)}</option>
              <option value="community">{intl.formatMessage(messages.community)}</option>
              <option value="local">{intl.formatMessage(messages.local)}</option>
            </select>
          </label>
          <label>
            <span className="sr-only">{intl.formatMessage(messages.compatibility)}</span>
            <select
              className="h-9 w-full rounded-md border border-border-primary bg-background-primary px-3 text-sm"
              value={compatibility}
              onChange={(event) => {
                searchSequence.current += 1;
                setCompatibility(event.target.value as '' | 'compatible' | 'incompatible');
              }}
            >
              <option value="">{intl.formatMessage(messages.allCompatibility)}</option>
              <option value="compatible">{intl.formatMessage(messages.compatible)}</option>
              <option value="incompatible">{intl.formatMessage(messages.incompatible)}</option>
            </select>
          </label>
          <label>
            <span className="sr-only">{intl.formatMessage(messages.catalogSource)}</span>
            <select
              className="h-9 w-full rounded-md border border-border-primary bg-background-primary px-3 text-sm"
              value={sourceId}
              onChange={(event) => {
                searchSequence.current += 1;
                setSourceId(event.target.value);
              }}
            >
              <option value="">{intl.formatMessage(messages.allSources)}</option>
              {sourceOptions.map((source) => (
                <option key={source.sourceId} value={source.sourceId}>
                  {source.sourceId}
                </option>
              ))}
            </select>
          </label>
        </form>

        {loading ? (
          <StatePanel
            kind="loading"
            title={intl.formatMessage(messages.loadingCatalog)}
            description={intl.formatMessage(messages.readingVerifiedSources)}
          />
        ) : error ? (
          <RecoveryPanel recovery={error} onRetry={() => void search()} />
        ) : items.length === 0 ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.noMcpsFound)}
            description={intl.formatMessage(messages.noMcpsFoundDescription)}
          />
        ) : (
          <>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(min(100%,260px),1fr))] gap-3">
              {items.map((item) => (
                <button
                  key={`${item.sourceId}:${item.manifestDigest}`}
                  onClick={() => void selectItem(item)}
                  className="min-w-0 rounded-xl border border-border-primary bg-background-primary p-4 text-left transition-colors hover:bg-background-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
                  aria-pressed={selected?.manifestDigest === item.manifestDigest}
                >
                  <div className="flex items-start justify-between gap-3">
                    <h3 className="truncate font-medium text-text-primary">{item.name}</h3>
                    <StatusBadge tone={item.compatibility === 'compatible' ? 'success' : 'danger'}>
                      {formatMcpValue(intl, item.compatibility)}
                    </StatusBadge>
                  </div>
                  <p className="mt-2 line-clamp-3 min-h-[3.75rem] text-sm text-text-secondary">
                    {item.description}
                  </p>
                  <div className="mt-3 flex flex-wrap gap-1.5">
                    <StatusBadge>{formatMcpValue(intl, item.trustTier)}</StatusBadge>
                    <StatusBadge>{formatMcpValue(intl, item.distribution)}</StatusBadge>
                    <StatusBadge>{item.version}</StatusBadge>
                  </div>
                  <p className="mt-3 truncate text-xs text-text-tertiary">{item.publisherName}</p>
                </button>
              ))}
            </div>
            {nextCursor && (
              <div className="flex justify-center">
                <Button
                  variant="outline"
                  disabled={loadingMore}
                  onClick={() => void search(nextCursor)}
                >
                  {intl.formatMessage(loadingMore ? messages.loading : messages.loadMore)}
                </Button>
              </div>
            )}
          </>
        )}
      </section>

      <aside
        className="min-w-0 xl:sticky xl:top-0 xl:max-h-[calc(100dvh-190px)] xl:overflow-y-auto"
        aria-label={intl.formatMessage(messages.detailsLabel)}
      >
        {!selected ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.selectMcp)}
            description={intl.formatMessage(messages.selectMcpDescription)}
          />
        ) : detailLoading ? (
          <StatePanel
            kind="loading"
            title={intl.formatMessage(messages.loadingDetails)}
            description={intl.formatMessage(messages.readingManifest)}
          />
        ) : detailError ? (
          <RecoveryPanel recovery={detailError} onRetry={() => void selectItem(selected)} />
        ) : detail ? (
          <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
            <div>
              <div className="flex flex-wrap items-center gap-2">
                <h2 className="text-xl font-medium text-text-primary">{detail.name}</h2>
                <StatusBadge tone={detail.eligibility.outcome === 'denied' ? 'danger' : 'success'}>
                  {formatMcpValue(intl, detail.eligibility.outcome)}
                </StatusBadge>
              </div>
              <p className="mt-2 text-sm text-text-secondary">{detail.description}</p>
            </div>
            <DefinitionList
              items={[
                [intl.formatMessage(messages.source), detail.sourceId],
                [intl.formatMessage(messages.publisher), detail.publisher.name],
                [intl.formatMessage(messages.version), detail.version],
                [intl.formatMessage(messages.trust), formatMcpValue(intl, detail.trustTier)],
                [
                  intl.formatMessage(messages.compatibility),
                  formatMcpValue(intl, detail.compatibility),
                ],
                [
                  intl.formatMessage(messages.distributionAdapter),
                  detail.distribution.type === 'git_dev'
                    ? detail.distribution.adapter
                    : formatMcpValue(intl, detail.distribution.type),
                ],
                [
                  intl.formatMessage(messages.transport),
                  formatMcpValue(intl, detail.transport.type),
                ],
                [
                  intl.formatMessage(messages.authentication),
                  formatMcpValue(intl, detail.auth.type),
                ],
                [
                  intl.formatMessage(messages.healthContract),
                  formatMcpValue(intl, detail.health.type),
                ],
              ]}
            />
            <div>
              <h3 className="mb-2 font-medium text-text-primary">
                {intl.formatMessage(messages.capabilities)}
              </h3>
              <div className="flex flex-wrap gap-1.5">
                {detail.capabilities.map((capability) => (
                  <StatusBadge key={capability}>{formatMcpValue(intl, capability)}</StatusBadge>
                ))}
              </div>
            </div>
            <div>
              <h3 className="mb-2 font-medium text-text-primary">
                {intl.formatMessage(messages.permissionsPolicy)}
              </h3>
              {detail.permissions.length === 0 ? (
                <p className="text-sm text-text-secondary">
                  {intl.formatMessage(messages.noPermissions)}
                </p>
              ) : (
                <ul className="space-y-2 text-sm text-text-secondary">
                  {detail.permissions.map((permission) => (
                    <li key={permission.id} className="rounded-lg bg-background-secondary p-3">
                      <span className="font-medium text-text-primary">
                        {formatMcpValue(intl, permission.kind)}
                      </span>{' '}
                      — {permission.reason}
                    </li>
                  ))}
                </ul>
              )}
              <p className="mt-2 text-sm text-text-secondary">
                {intl.formatMessage(messages.eligibility)}:{' '}
                {formatMcpValue(intl, detail.eligibility.reason)}
              </p>
            </div>
            <Button
              className="w-full"
              disabled={planning || detail.eligibility.outcome === 'denied'}
              onClick={() => void createPlan()}
            >
              {intl.formatMessage(planning ? messages.creatingPlan : messages.createInstallPlan)}
            </Button>
          </div>
        ) : null}
      </aside>

      <PlanReviewDialog
        plan={plan}
        operation={planOperation}
        onClose={() => {
          setPlan(null);
          setPlanOperation(null);
        }}
        onTaskCreated={onTaskCreated}
      />
    </div>
  );
}
