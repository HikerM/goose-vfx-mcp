import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { IntlShape } from 'react-intl';
import type {
  McpCatalogCacheMetadata,
  McpCatalogDetail,
  McpCatalogSummary,
  McpPlanIntent,
  McpPlanReview,
  McpSourcesPolicyState,
  McpTaskRef,
  McpTrustTier,
} from '@hikerm/lumina-sdk';
import { Search } from 'lucide-react';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '../ui/sheet';
import BackButton from '../ui/BackButton';
import {
  createMcpPlan,
  getMcpCatalogDetail,
  listMcpCatalog,
  toMcpRecoveryViewModel,
  type McpCatalogRef,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import {
  DefinitionList,
  NoticeBanner,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
  formatMcpValue,
} from './McpCenterCommon';
import {
  catalogDetailMatchesTarget,
  catalogPlanTargetFromItem,
  catalogReviewMatchesTarget,
  catalogTargetKey,
} from './catalogIdentity';
import { PlanReviewDialog } from './PlanReviewDialog';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';
import { HttpsManifestProvision } from './HttpsManifestProvision';

type DiscoverLayoutMode = 'compact' | 'medium' | 'wide';

const compactBreakpointPx = 760;
const wideBreakpointPx = 1200;

export function DiscoverTab({
  sourcesPolicy,
  refreshNonce,
  onTaskCreated,
}: {
  sourcesPolicy: McpSourcesPolicyState | null;
  refreshNonce?: number;
  onTaskCreated: (task: McpTaskRef) => void;
}) {
  const intl = useIntl();
  const layoutMode = useDiscoverLayoutMode();
  const [query, setQuery] = useState('');
  const [trustTier, setTrustTier] = useState<McpTrustTier | ''>('');
  const [compatibility, setCompatibility] = useState<'' | 'compatible' | 'incompatible'>('');
  const [sourceId, setSourceId] = useState('');
  const [items, setItems] = useState<McpCatalogSummary[]>([]);
  const [nextCursor, setNextCursor] = useState<string>();
  const [pageCache, setPageCache] = useState<McpCatalogCacheMetadata | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [selected, setSelected] = useState<McpCatalogSummary | null>(null);
  const [detail, setDetail] = useState<McpCatalogDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [planning, setPlanning] = useState(false);
  const [planError, setPlanError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [plan, setPlan] = useState<McpPlanReview | null>(null);
  const [planOperation, setPlanOperation] = useState<McpTaskRef['operation'] | null>(null);
  const searchSequence = useRef(0);
  const detailRequestSequence = useRef(0);
  const planRequestSequence = useRef(0);
  const selectedCatalogKey = useRef<string | null>(null);
  const cardButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const detailCacheRef = useRef(new Map<string, McpCatalogDetail>());
  const hasFilters = !!(query.trim() || trustTier || compatibility || sourceId);
  const selectedTarget = selected ? catalogPlanTargetFromItem(selected) : null;
  const isCompactDetailView = layoutMode === 'compact' && !!selected;

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
        setPageCache(page.cache);
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
  }, [refreshNonce, search]);

  const closeSelection = useCallback(() => {
    const closingKey = selectedCatalogKey.current;
    selectedCatalogKey.current = null;
    setSelected(null);
    setDetail(null);
    setDetailError(null);
    setPlan(null);
    setPlanError(null);
    setPlanOperation(null);
    setDetailLoading(false);
    setPlanning(false);
    window.setTimeout(() => {
      if (!closingKey) return;
      cardButtonRefs.current.get(closingKey)?.focus();
    }, 0);
  }, []);

  const loadDetail = useCallback(
    async (item: McpCatalogSummary) => {
      const target = catalogPlanTargetFromItem(item);
      const targetKey = catalogTargetKey(target);
      const previousKey = selectedCatalogKey.current;
      const cachedDetail = detailCacheRef.current.get(targetKey) ?? null;
      const requestId = ++detailRequestSequence.current;

      selectedCatalogKey.current = targetKey;

      setSelected(item);
      setPlan(null);
      setPlanError(null);
      setPlanOperation(null);
      setDetailError(null);
      if (previousKey !== targetKey || !detail) {
        setDetail(cachedDetail);
      }
      setDetailLoading(true);

      try {
        const nextDetail = await getMcpCatalogDetail(catalogRefFromItem(item));
        if (
          detailRequestSequence.current !== requestId ||
          selectedCatalogKey.current !== targetKey
        ) {
          return;
        }
        if (!catalogDetailMatchesTarget(target, nextDetail)) {
          detailCacheRef.current.delete(targetKey);
          setDetail(null);
          setDetailError(
            createBoundaryRecovery(intl, 'detail', {
              retryable: true,
            })
          );
          return;
        }
        detailCacheRef.current.set(targetKey, nextDetail);
        setDetail(nextDetail);
        setDetailError(null);
      } catch (cause) {
        if (
          detailRequestSequence.current !== requestId ||
          selectedCatalogKey.current !== targetKey
        ) {
          return;
        }
        setDetailError(toMcpRecoveryViewModel(cause));
      } finally {
        if (
          detailRequestSequence.current === requestId &&
          selectedCatalogKey.current === targetKey
        ) {
          setDetailLoading(false);
        }
      }
    },
    [detail, intl]
  );

  const createPlan = useCallback(async () => {
    if (!selected || !selectedTarget) return;
    const requestId = ++planRequestSequence.current;
    const targetKey = catalogTargetKey(selectedTarget);
    const { intent, operation } = createCatalogPlanIntent(selected);

    setPlanning(true);
    setPlan(null);
    setPlanError(null);

    try {
      const nextPlan = await createMcpPlan(intent);
      if (
        planRequestSequence.current !== requestId ||
        selectedCatalogKey.current !== targetKey
      ) {
        return;
      }
      if (!catalogReviewMatchesTarget(selectedTarget, nextPlan)) {
        setPlan(null);
        setPlanOperation(null);
        setPlanError(createBoundaryRecovery(intl, 'plan', { retryable: true }));
        return;
      }
      setPlanOperation(operation);
      setPlan(nextPlan);
    } catch (cause) {
      if (
        planRequestSequence.current !== requestId ||
        selectedCatalogKey.current !== targetKey
      ) {
        return;
      }
      setPlanError(toMcpRecoveryViewModel(cause));
    } finally {
      if (
        planRequestSequence.current === requestId &&
        selectedCatalogKey.current === targetKey
      ) {
        setPlanning(false);
      }
    }
  }, [intl, selected, selectedTarget]);

  const sourceOptions = useMemo(() => sourcesPolicy?.sources ?? [], [sourcesPolicy]);
  const emptyState = useMemo(() => {
    if (hasFilters) {
      return {
        title: intl.formatMessage(messages.noMcpsFound),
        description: intl.formatMessage(messages.noCatalogMatchesDescription),
      };
    }

    if (sourcesPolicy?.sources.length === 0) {
      return {
        title: intl.formatMessage(messages.noLocalCatalogSources),
        description: intl.formatMessage(messages.noLocalCatalogSourcesDescription),
      };
    }

    const manifestCount =
      sourcesPolicy?.sources.reduce((total, source) => total + source.manifestCount, 0) ?? 0;
    if (manifestCount === 0) {
      return {
        title: intl.formatMessage(messages.localCatalogEmpty),
        description: intl.formatMessage(messages.localCatalogEmptyDescription),
      };
    }

    const compatibleCount =
      sourcesPolicy?.sources.reduce(
        (total, source) => total + source.compatibility.compatible,
        0
      ) ?? 0;
    if (sourcesPolicy && compatibleCount === 0) {
      return {
        title: intl.formatMessage(messages.catalogRestrictedByPolicy),
        description: intl.formatMessage(messages.catalogRestrictedByPolicyDescription),
      };
    }

    return {
      title: intl.formatMessage(messages.noMcpsFound),
      description: intl.formatMessage(messages.noMcpsFoundDescription),
    };
  }, [hasFilters, intl, sourcesPolicy]);

  const detailPanel = renderDiscoverDetail({
    intl,
    selected,
    detail,
    detailLoading,
    detailError,
    planError,
    planning,
    onRetryDetail:
      selected && selectedTarget
        ? () => void loadDetail(selected)
        : undefined,
    onCreatePlan: () => void createPlan(),
    createPlanLabel: selected && isRegistrationOnlyCatalogItem(selected)
      ? intl.formatMessage(messages.createRegistrationPlan)
      : intl.formatMessage(messages.createInstallPlan),
  });

  const catalogStatusBanner =
    !loading && pageCache ? (
      <CatalogStatusBanner
        cache={pageCache}
        error={items.length > 0 ? error : null}
        intl={intl}
        onRetry={() => void search()}
      />
    ) : null;

  return (
    <div className="grid min-h-0 gap-5 min-[1200px]:grid-cols-[minmax(0,1fr)_minmax(320px,400px)] min-[1600px]:grid-cols-[minmax(0,1fr)_440px]">
      {!isCompactDetailView && (
        <section className="min-w-0 space-y-4" aria-label={intl.formatMessage(messages.catalogLabel)}>
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border-primary bg-background-primary p-4">
            <p className="text-sm text-text-secondary">
              {intl.formatMessage(messages.catalogDescription)}
            </p>
            <HttpsManifestProvision onTaskCreated={onTaskCreated} />
          </div>
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
          ) : error && items.length === 0 ? (
            <RecoveryPanel recovery={error} onRetry={() => void search()} />
          ) : items.length === 0 ? (
            <StatePanel
              kind="empty"
              title={emptyState.title}
              description={emptyState.description}
            />
          ) : (
            <>
              {catalogStatusBanner}
              <div className="grid grid-cols-[repeat(auto-fill,minmax(min(100%,260px),1fr))] gap-3">
                {items.map((item) => {
                  const targetKey = catalogTargetKey(catalogPlanTargetFromItem(item));
                  const isSelected =
                    selectedTarget !== null && catalogTargetKey(selectedTarget) === targetKey;
                  return (
                    <button
                      key={targetKey}
                      onClick={() => void loadDetail(item)}
                      ref={(node) => {
                        if (node) {
                          cardButtonRefs.current.set(targetKey, node);
                        } else {
                          cardButtonRefs.current.delete(targetKey);
                        }
                      }}
                      className="min-w-0 rounded-xl border border-border-primary bg-background-primary p-4 text-left transition-colors hover:bg-background-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
                      aria-pressed={isSelected}
                      aria-haspopup={layoutMode === 'medium' ? 'dialog' : undefined}
                    >
                      <div className="flex items-start justify-between gap-3">
                        <h3 className="line-clamp-2 break-words font-medium text-text-primary">
                          {item.name}
                        </h3>
                        <div className="flex shrink-0 flex-wrap justify-end gap-1.5">
                          {isSelected && (
                            <StatusBadge tone="info">
                              {intl.formatMessage(messages.selected)}
                            </StatusBadge>
                          )}
                          <StatusBadge
                            tone={item.compatibility === 'compatible' ? 'success' : 'danger'}
                          >
                            {formatMcpValue(intl, item.compatibility)}
                          </StatusBadge>
                        </div>
                      </div>
                      <p className="mt-2 line-clamp-3 min-h-[3.75rem] text-sm text-text-secondary">
                        {item.description}
                      </p>
                      <div className="mt-3 flex flex-wrap gap-1.5">
                        <StatusBadge>{formatMcpValue(intl, item.trustTier)}</StatusBadge>
                        <StatusBadge>{formatMcpValue(intl, item.distribution)}</StatusBadge>
                        <StatusBadge>{item.version}</StatusBadge>
                      </div>
                      <div className="mt-3 space-y-1 text-xs text-text-tertiary">
                        <p className="break-words">
                          {intl.formatMessage(messages.source)}: {item.sourceId}
                        </p>
                        <p className="break-words">{item.publisherName}</p>
                      </div>
                    </button>
                  );
                })}
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
      )}

      {layoutMode === 'wide' && (
        <aside
          className="min-w-0 min-[1200px]:sticky min-[1200px]:top-0 min-[1200px]:max-h-[calc(100dvh-190px)] min-[1200px]:overflow-y-auto"
          aria-label={intl.formatMessage(messages.detailsLabel)}
        >
          {detailPanel}
        </aside>
      )}

      {layoutMode === 'medium' && (
        <Sheet
          open={!!selected}
          onOpenChange={(open) => {
            if (!open) {
              closeSelection();
            }
          }}
        >
          <SheetContent
            side="right"
            className="w-[min(92vw,420px)] max-w-none p-0 sm:max-w-[420px]"
          >
            <SheetHeader className="border-b border-border-primary pr-12">
              <SheetTitle>{intl.formatMessage(messages.detailsTitle)}</SheetTitle>
              <SheetDescription>
                {intl.formatMessage(messages.selectMcpDescription)}
              </SheetDescription>
            </SheetHeader>
            <div className="min-h-0 flex-1 overflow-y-auto p-4">{detailPanel}</div>
          </SheetContent>
        </Sheet>
      )}

      {isCompactDetailView && (
        <section
          className="min-w-0 space-y-4"
          aria-label={intl.formatMessage(messages.detailsLabel)}
        >
          <BackButton
            onClick={closeSelection}
            variant="secondary"
            className="px-4"
            aria-label={intl.formatMessage(messages.backToResults)}
          />
          {detailPanel}
        </section>
      )}

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

function renderDiscoverDetail({
  intl,
  selected,
  detail,
  detailLoading,
  detailError,
  planError,
  planning,
  onRetryDetail,
  onCreatePlan,
  createPlanLabel,
}: {
  intl: IntlShape;
  selected: McpCatalogSummary | null;
  detail: McpCatalogDetail | null;
  detailLoading: boolean;
  detailError: McpPlatformRecoveryViewModel | null;
  planError: McpPlatformRecoveryViewModel | null;
  planning: boolean;
  onRetryDetail?: () => void;
  onCreatePlan: () => void;
  createPlanLabel: string;
}) {
  if (!selected) {
    return (
      <StatePanel
        kind="empty"
        title={intl.formatMessage(messages.selectMcp)}
        description={intl.formatMessage(messages.selectMcpDescription)}
      />
    );
  }

  if (detailLoading && !detail) {
    return (
      <StatePanel
        kind="loading"
        title={intl.formatMessage(messages.loadingDetails)}
        description={intl.formatMessage(messages.readingManifest)}
      />
    );
  }

  if (detailError && !detail) {
    return <RecoveryPanel recovery={detailError} onRetry={onRetryDetail} />;
  }

  if (!detail) {
    return null;
  }

  return (
    <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
      {detailLoading && (
        <NoticeBanner
          tone="info"
          title={intl.formatMessage(messages.refreshingDetails)}
          description={intl.formatMessage(messages.refreshingDetailsDescription)}
        />
      )}
      {detailError && <RecoveryPanel recovery={detailError} onRetry={onRetryDetail} />}
      {planError && <RecoveryPanel recovery={planError} onRetry={onCreatePlan} />}

      <div>
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="break-words text-xl font-medium text-text-primary">{detail.name}</h2>
          <StatusBadge tone={detail.eligibility.outcome === 'denied' ? 'danger' : 'success'}>
            {formatMcpValue(intl, detail.eligibility.outcome)}
          </StatusBadge>
          <StatusBadge>{formatMcpValue(intl, detail.trustTier)}</StatusBadge>
        </div>
        <p className="mt-2 break-words text-sm text-text-secondary">{detail.description}</p>
      </div>

      <DefinitionList
        items={[
          [intl.formatMessage(messages.source), detail.sourceId],
          [intl.formatMessage(messages.publisher), detail.publisher.name],
          [intl.formatMessage(messages.version), detail.version],
          [intl.formatMessage(messages.trust), formatMcpValue(intl, detail.trustTier)],
          [
            intl.formatMessage(messages.verifiedAt),
            formatMcpTimestamp(detail.verifiedAtMs, intl),
          ],
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
          [intl.formatMessage(messages.transport), formatMcpValue(intl, detail.transport.type)],
          [
            intl.formatMessage(messages.authentication),
            formatMcpValue(intl, detail.auth.type),
          ],
          [intl.formatMessage(messages.healthContract), formatMcpValue(intl, detail.health.type)],
          [
            intl.formatMessage(messages.policyConclusion),
            formatMcpValue(intl, detail.eligibility.outcome),
          ],
          [
            intl.formatMessage(messages.policyReason),
            formatMcpValue(intl, detail.eligibility.reason),
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
                <div className="font-medium text-text-primary">
                  {formatMcpValue(intl, permission.kind)}
                </div>
                <div className="mt-1 break-words">{permission.reason}</div>
                {permission.scope && (
                  <div className="mt-1 break-all font-mono text-xs text-text-tertiary">
                    {permission.scope}
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>

      <Button
        className="w-full"
        disabled={planning || detail.eligibility.outcome === 'denied'}
        onClick={onCreatePlan}
      >
        {planning ? intl.formatMessage(messages.creatingPlan) : createPlanLabel}
      </Button>
    </div>
  );
}

function CatalogStatusBanner({
  cache,
  error,
  intl,
  onRetry,
}: {
  cache: McpCatalogCacheMetadata;
  error: McpPlatformRecoveryViewModel | null;
  intl: IntlShape;
  onRetry: () => void;
}) {
  let tone: 'neutral' | 'success' | 'warning' | 'danger' | 'info' = 'success';
  let title = intl.formatMessage(messages.catalogStatusFreshTitle);
  let description = intl.formatMessage(messages.catalogStatusFreshDescription);

  if (error) {
    tone = 'warning';
    title = intl.formatMessage(messages.catalogStatusRefreshFailedTitle);
    description = intl.formatMessage(messages.catalogStatusRefreshFailedDescription);
  } else if (cache.refreshState === 'refreshing') {
    tone = 'info';
    title = intl.formatMessage(messages.catalogStatusRefreshingTitle);
    description = intl.formatMessage(messages.catalogStatusRefreshingDescription);
  } else if (cache.offline || cache.freshness === 'offline_verified') {
    tone = 'warning';
    title = intl.formatMessage(messages.catalogStatusOfflineTitle);
    description = intl.formatMessage(messages.catalogStatusOfflineDescription);
  } else if (cache.freshness === 'stale' || cache.refreshState === 'failed') {
    tone = 'warning';
    title = intl.formatMessage(messages.catalogStatusStaleTitle);
    description = intl.formatMessage(messages.catalogStatusStaleDescription);
  }

  const lastVerified =
    cache.newestVerifiedAtMs !== undefined
      ? `${intl.formatMessage(messages.lastVerified)}: ${formatMcpTimestamp(
          cache.newestVerifiedAtMs,
          intl
        )}`
      : null;

  return (
    <NoticeBanner
      tone={tone}
      title={title}
      description={lastVerified ? `${description} ${lastVerified}` : description}
      action={
        error ? (
          <Button variant="outline" size="sm" onClick={onRetry}>
            {intl.formatMessage(messages.retry)}
          </Button>
        ) : undefined
      }
    />
  );
}

function createCatalogPlanIntent(
  item: Pick<
    McpCatalogSummary,
    'distribution' | 'sourceId' | 'mcpId' | 'version' | 'manifestDigest'
  >
): {
  intent: Extract<McpPlanIntent, { type: 'register_catalog' | 'install_catalog' }>;
  operation: McpTaskRef['operation'];
} {
  const catalog = catalogPlanTargetFromItem(item);
  if (isRegistrationOnlyCatalogItem(item)) {
    return {
      intent: {
        type: 'register_catalog',
        catalog,
        installation_scope: 'user',
      },
      operation: 'register',
    };
  }

  return {
    intent: {
      type: 'install_catalog',
      catalog,
    },
    operation: 'install',
  };
}

function createBoundaryRecovery(
  intl: IntlShape,
  kind: 'detail' | 'plan',
  options?: { retryable?: boolean }
): McpPlatformRecoveryViewModel {
  if (kind === 'detail') {
    return {
      title: intl.formatMessage(messages.detailIntegrityTitle),
      message: intl.formatMessage(messages.detailIntegrityDescription),
      nextStep: intl.formatMessage(messages.detailIntegrityNext),
      retryable: options?.retryable ?? false,
      kind: 'service',
    };
  }

  return {
    title: intl.formatMessage(messages.reviewIntegrityTitle),
    message: intl.formatMessage(messages.reviewIntegrityDescription),
    nextStep: intl.formatMessage(messages.reviewIntegrityNext),
    retryable: options?.retryable ?? false,
    kind: 'service',
  };
}

function isRegistrationOnlyCatalogItem(
  item: Pick<McpCatalogSummary, 'distribution'>
): boolean {
  return item.distribution === 'remote_http' || item.distribution === 'manual_stdio';
}

function catalogRefFromItem(
  item: Pick<McpCatalogSummary, 'sourceId' | 'mcpId' | 'version'>
): McpCatalogRef {
  return {
    sourceId: item.sourceId,
    mcpId: item.mcpId,
    version: item.version,
  };
}

function formatMcpTimestamp(value: number, intl: IntlShape): string {
  return `${intl.formatDate(value, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })} ${intl.formatTime(value, {
    hour: 'numeric',
    minute: '2-digit',
  })}`;
}

function useDiscoverLayoutMode(): DiscoverLayoutMode {
  const [layoutMode, setLayoutMode] = useState<DiscoverLayoutMode>(() =>
    resolveDiscoverLayoutMode(typeof window === 'undefined' ? wideBreakpointPx : window.innerWidth)
  );

  useEffect(() => {
    const handleResize = () => {
      setLayoutMode(resolveDiscoverLayoutMode(window.innerWidth));
    };

    handleResize();
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, []);

  return layoutMode;
}

function resolveDiscoverLayoutMode(width: number): DiscoverLayoutMode {
  if (width < compactBreakpointPx) {
    return 'compact';
  }
  if (width < wideBreakpointPx) {
    return 'medium';
  }
  return 'wide';
}
