import { useMemo, useRef, useState } from 'react';
import type { McpSourcesPolicyState } from '@hikerm/lumina-sdk';
import {
  confirmMcpSourceProvision,
  importGovernedMcpSource,
  prepareMcpSourceProvision,
  refreshMcpSource,
  toMcpRecoveryViewModel,
  type McpGovernedImportResult,
  type McpPlatformRecoveryViewModel,
  type McpSourceProvisionConfirmResult,
  type McpSourceProvisionPrepareResult,
  type McpSourceProvisionPreview,
} from '../../acp/mcp-platform';
import { useIntl } from '../../i18n';
import { Button } from '../ui/button';
import {
  DefinitionList,
  NoticeBanner,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
  formatMcpValue,
} from './McpCenterCommon';
import { mcpCenterMessages as messages } from './messages';

type ImportMode = 'local_manifest' | 'local_directory';

const importModes = [
  ['local_manifest', messages.localManifestImportTitle, messages.localManifestImportDescription],
  ['local_directory', messages.localDirectoryImportTitle, messages.localDirectoryImportDescription],
] as const;

export function SourcesPolicyTab({
  state,
  onImported,
  onOpenDiscover,
}: {
  state: McpSourcesPolicyState | null;
  onImported?: () => Promise<void> | void;
  onOpenDiscover?: () => void;
}) {
  const intl = useIntl();
  const [mode, setMode] = useState<ImportMode>('local_manifest');
  const importModeRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [selectedPath, setSelectedPath] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [pickerBusy, setPickerBusy] = useState(false);
  const [importResult, setImportResult] = useState<McpGovernedImportResult | null>(null);
  const [importError, setImportError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [provisionDirectory, setProvisionDirectory] = useState('');
  const [provisionPickerBusy, setProvisionPickerBusy] = useState(false);
  const [preparingProvision, setPreparingProvision] = useState(false);
  const [confirmingProvision, setConfirmingProvision] = useState(false);
  const [preparedProvision, setPreparedProvision] =
    useState<McpSourceProvisionPrepareResult | null>(null);
  const [provisionResult, setProvisionResult] = useState<McpSourceProvisionConfirmResult | null>(
    null
  );
  const [provisionError, setProvisionError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const confirmProvisionButtonRef = useRef<HTMLButtonElement | null>(null);
  const [refreshingSourceId, setRefreshingSourceId] = useState<string | null>(null);
  const [refreshTargetSourceId, setRefreshTargetSourceId] = useState<string | null>(null);
  const [refreshDocumentDigest, setRefreshDocumentDigest] = useState<string | null>(null);
  const [refreshError, setRefreshError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const importedSourceState = useMemo(
    () =>
      importResult
        ? state?.sources.find((source) => source.sourceId === importResult.source.sourceId)
        : null,
    [importResult, state]
  );
  const refreshableSources = useMemo(
    () => state?.sources.filter((source) => source.cache.refreshState !== 'local_only') ?? [],
    [state]
  );
  const counts = useMemo(() => summarizeImportEntries(importResult), [importResult]);
  const pickerLabel =
    mode === 'local_manifest'
      ? intl.formatMessage(messages.chooseManifestFile)
      : intl.formatMessage(messages.chooseSourceDirectory);
  const pickerHint =
    mode === 'local_manifest'
      ? intl.formatMessage(messages.localManifestImportDescription)
      : intl.formatMessage(messages.localDirectoryImportDescription);
  const provisionBusy = provisionPickerBusy || preparingProvision || confirmingProvision;

  const resetFlow = () => {
    setSelectedPath('');
    setImportResult(null);
    setImportError(null);
  };

  const selectImportMode = (nextMode: ImportMode) => {
    if (mode === nextMode) return;
    setMode(nextMode);
    resetFlow();
  };

  const resetProvisionFlow = () => {
    setProvisionDirectory('');
    setPreparedProvision(null);
    setProvisionResult(null);
    setProvisionError(null);
  };

  const chooseProvisionDirectory = async () => {
    if (provisionPickerBusy || preparingProvision || confirmingProvision) return;
    setProvisionPickerBusy(true);
    setProvisionError(null);
    try {
      const result = await window.electron.directoryChooser();
      if (!result.canceled && result.filePaths.length > 0) {
        setProvisionDirectory(result.filePaths[0]);
        setPreparedProvision(null);
        setProvisionResult(null);
      }
    } catch (cause) {
      setProvisionError(toMcpRecoveryViewModel(cause));
    } finally {
      setProvisionPickerBusy(false);
    }
  };

  const prepareProvision = async () => {
    if (!provisionDirectory || preparingProvision || confirmingProvision) return;
    setPreparingProvision(true);
    setPreparedProvision(null);
    setProvisionResult(null);
    setProvisionError(null);
    try {
      const nextPrepared = await prepareMcpSourceProvision(provisionDirectory);
      setPreparedProvision(nextPrepared);
      window.setTimeout(() => confirmProvisionButtonRef.current?.focus(), 0);
    } catch (cause) {
      setProvisionError(toMcpRecoveryViewModel(cause));
    } finally {
      setPreparingProvision(false);
    }
  };

  const confirmProvision = async () => {
    if (!preparedProvision || confirmingProvision || preparingProvision) return;
    setConfirmingProvision(true);
    setProvisionError(null);
    try {
      const confirmed = await confirmMcpSourceProvision({
        provisionId: preparedProvision.provisionId,
        confirmationToken: preparedProvision.confirmationToken,
      });
      if (!sameProvisionPreview(preparedProvision.preview, confirmed.preview)) {
        throw new Error('The MCP Platform returned a different source preview after confirmation.');
      }
      setProvisionResult(confirmed);
      setPreparedProvision(null);
      setProvisionDirectory('');
      await onImported?.();
    } catch (cause) {
      setPreparedProvision(null);
      setProvisionError(toMcpRecoveryViewModel(cause));
    } finally {
      setConfirmingProvision(false);
    }
  };

  const choosePath = async () => {
    setPickerBusy(true);
    setImportError(null);
    try {
      if (mode === 'local_manifest') {
        const nextPath = await window.electron.selectFileOrDirectory();
        if (nextPath) {
          setSelectedPath(nextPath);
          setImportResult(null);
        }
        return;
      }

      const result = await window.electron.directoryChooser();
      if (!result.canceled && result.filePaths.length > 0) {
        setSelectedPath(result.filePaths[0]);
        setImportResult(null);
      }
    } catch (cause) {
      setImportError(toMcpRecoveryViewModel(cause));
    } finally {
      setPickerBusy(false);
    }
  };

  const submitImport = async () => {
    if (!selectedPath || submitting) return;
    setSubmitting(true);
    setImportError(null);
    try {
      const nextResult = await importGovernedMcpSource(
        mode === 'local_manifest'
          ? { type: 'local_manifest', filePath: selectedPath }
          : { type: 'local_directory', directoryPath: selectedPath }
      );
      setImportResult(nextResult);
      await onImported?.();
    } catch (cause) {
      setImportError(toMcpRecoveryViewModel(cause));
    } finally {
      setSubmitting(false);
    }
  };

  const refreshSource = async (sourceId: string) => {
    if (refreshingSourceId) return;
    setRefreshingSourceId(sourceId);
    setRefreshTargetSourceId(sourceId);
    setRefreshDocumentDigest(null);
    setRefreshError(null);
    try {
      const result = await refreshMcpSource(sourceId);
      setRefreshDocumentDigest(result.documentDigest);
      await onImported?.();
    } catch (cause) {
      setRefreshError(toMcpRecoveryViewModel(cause));
      void onImported?.();
    } finally {
      setRefreshingSourceId(null);
    }
  };

  if (!state) {
    return (
      <StatePanel
        kind="loading"
        title={intl.formatMessage(messages.loadingPolicy)}
        description={intl.formatMessage(messages.readingPolicy)}
      />
    );
  }

  return (
    <div className="mx-auto max-w-6xl space-y-5">
      <section
        className="grid gap-4 min-[1200px]:grid-cols-[minmax(0,360px)_minmax(0,1fr)]"
        aria-labelledby="provision-trusted-source-heading"
      >
        <article className="rounded-xl border border-border-primary bg-background-primary p-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2
                id="provision-trusted-source-heading"
                className="text-lg font-medium text-text-primary"
              >
                {intl.formatMessage(messages.provisionTrustedSource)}
              </h2>
              <p className="mt-1 text-sm text-text-secondary">
                {intl.formatMessage(messages.provisionTrustedSourceDescription)}
              </p>
            </div>
            <StatusBadge tone="info">{intl.formatMessage(messages.reviewRequired)}</StatusBadge>
          </div>

          <div className="mt-4 rounded-xl border border-border-primary bg-background-secondary/20 p-4">
            <h3 className="font-medium text-text-primary">
              {intl.formatMessage(messages.provisionDirectoryLabel)}
            </h3>
            <p className="mt-1 text-sm text-text-secondary">
              {intl.formatMessage(messages.provisionDirectoryHint)}
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => void chooseProvisionDirectory()}
                disabled={provisionBusy}
              >
                {provisionPickerBusy
                  ? intl.formatMessage(messages.loading)
                  : intl.formatMessage(messages.chooseProvisionDirectory)}
              </Button>
              <Button
                type="button"
                onClick={() => void prepareProvision()}
                disabled={!provisionDirectory || provisionBusy}
              >
                {preparingProvision
                  ? intl.formatMessage(messages.previewingTrustedSource)
                  : intl.formatMessage(messages.previewTrustedSource)}
              </Button>
              {(provisionDirectory || preparedProvision || provisionResult || provisionError) && (
                <Button
                  type="button"
                  variant="ghost"
                  onClick={resetProvisionFlow}
                  disabled={provisionBusy}
                >
                  {intl.formatMessage(messages.cancel)}
                </Button>
              )}
            </div>
            <div className="mt-3 rounded-lg border border-dashed border-border-primary px-3 py-2">
              <div className="text-xs font-medium uppercase tracking-wide text-text-secondary">
                {intl.formatMessage(messages.selectedLocalPath)}
              </div>
              <p className="mt-1 break-all text-sm text-text-primary">
                {provisionDirectory || intl.formatMessage(messages.noPathSelected)}
              </p>
            </div>
            <p className="mt-3 text-xs text-text-secondary">
              {intl.formatMessage(messages.provisionPreviewOnlyHint)}
            </p>
          </div>
        </article>

        <article className="space-y-4">
          {provisionError && (
            <div className="space-y-2">
              <RecoveryPanel
                recovery={provisionError}
                onRetry={provisionDirectory ? () => void prepareProvision() : undefined}
              />
              {provisionDirectory && (
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => void prepareProvision()}
                  disabled={provisionBusy}
                >
                  {intl.formatMessage(messages.repreviewTrustedSource)}
                </Button>
              )}
            </div>
          )}
          {preparedProvision ? (
            <div
              className="rounded-xl border border-border-warning bg-background-warning/5 p-5"
              aria-live="polite"
            >
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h3 className="text-lg font-medium text-text-primary">
                    {intl.formatMessage(messages.reviewTrustedSource)}
                  </h3>
                  <p className="mt-1 text-sm text-text-secondary">
                    {intl.formatMessage(messages.reviewTrustedSourceDescription)}
                  </p>
                </div>
                <StatusBadge tone="warning">
                  {intl.formatMessage(messages.confirmationRequired)}
                </StatusBadge>
              </div>
              <div className="mt-4">
                <ProvisionPreviewDefinitionList
                  intl={intl}
                  preview={preparedProvision.preview}
                  timestamp={[
                    intl.formatMessage(messages.expiresAt),
                    preparedProvision.expiresAtMs,
                  ]}
                />
              </div>
              <ProvisionWarnings intl={intl} warnings={preparedProvision.preview.warnings} />
              <div className="mt-4 flex flex-wrap gap-2">
                <Button
                  ref={confirmProvisionButtonRef}
                  type="button"
                  onClick={() => void confirmProvision()}
                  disabled={provisionBusy}
                >
                  {confirmingProvision
                    ? intl.formatMessage(messages.confirmingTrustedSource)
                    : intl.formatMessage(messages.confirmTrustedSource)}
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  onClick={resetProvisionFlow}
                  disabled={provisionBusy}
                >
                  {intl.formatMessage(messages.cancelProvisioning)}
                </Button>
              </div>
            </div>
          ) : provisionResult ? (
            <div
              role="status"
              className="rounded-xl border border-border-success bg-background-success/5 p-5"
            >
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h3 className="text-lg font-medium text-text-primary">
                    {intl.formatMessage(messages.provisioningSucceededTitle)}
                  </h3>
                  <p className="mt-1 text-sm text-text-secondary">
                    {intl.formatMessage(messages.provisioningSucceededDescription)}
                  </p>
                </div>
                <div className="flex flex-wrap gap-2">
                  {onOpenDiscover && (
                    <Button type="button" variant="outline" onClick={onOpenDiscover}>
                      {intl.formatMessage(messages.openDiscover)}
                    </Button>
                  )}
                  <Button type="button" variant="ghost" onClick={resetProvisionFlow}>
                    {intl.formatMessage(messages.provisionAnotherSource)}
                  </Button>
                </div>
              </div>
              <div className="mt-4">
                <ProvisionPreviewDefinitionList
                  intl={intl}
                  preview={provisionResult.preview}
                  timestamp={[
                    intl.formatMessage(messages.confirmedAt),
                    provisionResult.confirmedAtMs,
                  ]}
                />
              </div>
              <ProvisionWarnings intl={intl} warnings={provisionResult.preview.warnings} />
            </div>
          ) : (
            <StatePanel
              kind="empty"
              title={intl.formatMessage(messages.provisionReadyTitle)}
              description={intl.formatMessage(messages.provisionReadyDescription)}
            />
          )}
        </article>
      </section>

      <section className="grid gap-4 xl:grid-cols-[minmax(0,360px)_minmax(0,1fr)]">
        <article className="rounded-xl border border-border-primary bg-background-primary p-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 className="text-lg font-medium text-text-primary">
                {intl.formatMessage(messages.importTrustedSource)}
              </h2>
              <p className="mt-1 text-sm text-text-secondary">
                {intl.formatMessage(messages.importTrustedSourceDescription)}
              </p>
            </div>
            <StatusBadge tone="info">{intl.formatMessage(messages.localOnlyPhase)}</StatusBadge>
          </div>

          <div
            className="mt-4 grid gap-2"
            role="radiogroup"
            aria-label={intl.formatMessage(messages.importMethod)}
          >
            {importModes.map(([value, title, description], index) => {
              const active = mode === value;
              return (
                <button
                  key={value}
                  ref={(element) => {
                    importModeRefs.current[index] = element;
                  }}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  tabIndex={active ? 0 : -1}
                  onClick={() => selectImportMode(value)}
                  onKeyDown={(event) => {
                    const currentIndex = importModes.findIndex(
                      ([candidate]) => candidate === value
                    );
                    let nextIndex: number | null = null;
                    if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
                      nextIndex = (currentIndex + 1) % importModes.length;
                    } else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
                      nextIndex = (currentIndex - 1 + importModes.length) % importModes.length;
                    } else if (event.key === 'Home') {
                      nextIndex = 0;
                    } else if (event.key === 'End') {
                      nextIndex = importModes.length - 1;
                    }
                    if (nextIndex === null) return;
                    event.preventDefault();
                    const nextMode = importModes[nextIndex][0];
                    selectImportMode(nextMode);
                    importModeRefs.current[nextIndex]?.focus();
                  }}
                  className={`rounded-xl border p-3 text-left transition ${
                    active
                      ? 'border-border-info bg-background-info/10'
                      : 'border-border-primary bg-background-secondary/20 hover:bg-background-secondary/35'
                  }`}
                >
                  <div className="font-medium text-text-primary">{intl.formatMessage(title)}</div>
                  <p className="mt-1 text-sm text-text-secondary">
                    {intl.formatMessage(description)}
                  </p>
                </button>
              );
            })}
          </div>

          <div className="mt-4 rounded-xl border border-border-primary bg-background-secondary/20 p-4">
            <h3 className="font-medium text-text-primary">{pickerLabel}</h3>
            <p className="mt-1 text-sm text-text-secondary">{pickerHint}</p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => void choosePath()}
                disabled={pickerBusy || submitting}
              >
                {pickerBusy ? intl.formatMessage(messages.loading) : pickerLabel}
              </Button>
              <Button
                type="button"
                onClick={() => void submitImport()}
                disabled={!selectedPath || submitting}
              >
                {submitting
                  ? intl.formatMessage(messages.importingSource)
                  : intl.formatMessage(messages.importSource)}
              </Button>
              {(selectedPath || importResult || importError) && (
                <Button type="button" variant="ghost" onClick={resetFlow} disabled={submitting}>
                  {intl.formatMessage(messages.cancel)}
                </Button>
              )}
            </div>
            <div className="mt-3 rounded-lg border border-dashed border-border-primary px-3 py-2">
              <div className="text-xs font-medium uppercase tracking-wide text-text-secondary">
                {intl.formatMessage(messages.selectedLocalPath)}
              </div>
              <p className="mt-1 break-all text-sm text-text-primary">
                {selectedPath || intl.formatMessage(messages.noPathSelected)}
              </p>
            </div>
            <p className="mt-3 text-xs text-text-secondary">
              {intl.formatMessage(messages.reimportSourceHint)}
            </p>
          </div>
        </article>

        <article className="space-y-4">
          <NoticeBanner
            tone="warning"
            title={intl.formatMessage(messages.httpsImportUnavailableTitle)}
            description={intl.formatMessage(messages.httpsImportUnavailableDescription)}
          />
          {importError && (
            <RecoveryPanel
              recovery={importError}
              onRetry={selectedPath ? () => void submitImport() : undefined}
            />
          )}
          {importResult ? (
            <div
              role="status"
              className="rounded-xl border border-border-success bg-background-success/5 p-5"
            >
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h3 className="text-lg font-medium text-text-primary">
                    {intl.formatMessage(messages.importSucceededTitle)}
                  </h3>
                  <p className="mt-1 text-sm text-text-secondary">
                    {intl.formatMessage(messages.importSucceededDescription)}
                  </p>
                </div>
                <div className="flex flex-wrap gap-2">
                  {onOpenDiscover && (
                    <Button type="button" variant="outline" onClick={onOpenDiscover}>
                      {intl.formatMessage(messages.openDiscover)}
                    </Button>
                  )}
                  <Button type="button" variant="ghost" onClick={resetFlow}>
                    {intl.formatMessage(messages.importAnotherSource)}
                  </Button>
                </div>
              </div>
              <div className="mt-4">
                <DefinitionList
                  items={[
                    [intl.formatMessage(messages.sourceName), importResult.source.displayName],
                    [intl.formatMessage(messages.sourceId), importResult.source.sourceId],
                    [
                      intl.formatMessage(messages.importKind),
                      formatMcpValue(intl, importResult.source.importKind),
                    ],
                    [
                      intl.formatMessage(messages.documentKind),
                      formatMcpValue(intl, importResult.document.documentKind),
                    ],
                    [intl.formatMessage(messages.documentId), importResult.document.documentId],
                    [
                      intl.formatMessage(messages.documentDigest),
                      importResult.document.documentDigest,
                    ],
                    [intl.formatMessage(messages.catalogEntries), counts.entryCount],
                    [intl.formatMessage(messages.compatible), counts.compatible],
                    [intl.formatMessage(messages.restricted), counts.restricted],
                    [intl.formatMessage(messages.denied), counts.denied],
                    [
                      intl.formatMessage(messages.cacheFreshness),
                      importedSourceState
                        ? formatMcpValue(intl, importedSourceState.cache.freshness)
                        : intl.formatMessage(messages.pendingRefresh),
                    ],
                    [
                      intl.formatMessage(messages.policyStatus),
                      importedSourceState
                        ? formatMcpValue(intl, importedSourceState.recovery)
                        : intl.formatMessage(messages.pendingRefresh),
                    ],
                  ]}
                />
              </div>
              {counts.trustTiers.length > 0 && (
                <div className="mt-4 flex flex-wrap gap-1.5">
                  {counts.trustTiers.map((tier) => (
                    <StatusBadge key={tier}>{formatMcpValue(intl, tier)}</StatusBadge>
                  ))}
                </div>
              )}
            </div>
          ) : (
            <StatePanel
              kind="empty"
              title={intl.formatMessage(messages.importReadyTitle)}
              description={intl.formatMessage(messages.importReadyDescription)}
            />
          )}
        </article>
      </section>

      <section className="rounded-xl border border-border-primary bg-background-primary p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 className="text-lg font-medium text-text-primary">
              {intl.formatMessage(messages.machinePolicy)}
            </h2>
            <p className="mt-1 text-sm text-text-secondary">
              {intl.formatMessage(messages.readOnlyPolicy)}
            </p>
            <p className="mt-2 text-sm text-text-secondary">
              {intl.formatMessage(messages.machinePolicyDescription)}
            </p>
          </div>
          <StatusBadge tone="info">{intl.formatMessage(messages.readOnly)}</StatusBadge>
        </div>
        <div className="mt-4">
          <DefinitionList
            items={[
              [intl.formatMessage(messages.targetPlatform), state.policy.targetPlatform],
              [intl.formatMessage(messages.architecture), state.policy.targetArchitecture],
              [
                intl.formatMessage(messages.developmentMode),
                intl.formatMessage(
                  state.policy.developmentMode ? messages.enabled : messages.disabled
                ),
              ],
              [
                intl.formatMessage(messages.dockerAllowed),
                intl.formatMessage(state.policy.dockerAllowed ? messages.yes : messages.no),
              ],
              [intl.formatMessage(messages.recovery), formatMcpValue(intl, state.policy.recovery)],
            ]}
          />
        </div>
      </section>

      <section>
        <h2 className="mb-3 text-lg font-medium text-text-primary">
          {intl.formatMessage(messages.sourcesCache)}
        </h2>
        <p className="mb-3 text-sm text-text-secondary">
          {intl.formatMessage(messages.sourcesCacheDescription)}
        </p>
        <NoticeBanner
          tone="info"
          title={intl.formatMessage(messages.refreshCatalogOnlyTitle)}
          description={intl.formatMessage(messages.refreshCatalogOnlyDescription)}
        />
        {refreshError && (
          <div className="mt-3">
            <RecoveryPanel
              recovery={refreshError}
              onRetry={
                refreshTargetSourceId ? () => void refreshSource(refreshTargetSourceId) : undefined
              }
            />
          </div>
        )}
        {refreshDocumentDigest && refreshTargetSourceId && (
          <div
            role="status"
            className="mt-3 rounded-xl border border-border-success bg-background-success/5 p-4"
          >
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div>
                <h3 className="font-medium text-text-primary">
                  {intl.formatMessage(messages.refreshSucceededTitle)}
                </h3>
                <p className="mt-1 text-sm text-text-secondary">
                  {intl.formatMessage(messages.refreshSucceededDescription)}
                </p>
              </div>
              {onOpenDiscover && (
                <Button type="button" variant="outline" onClick={onOpenDiscover}>
                  {intl.formatMessage(messages.openDiscover)}
                </Button>
              )}
            </div>
            <div className="mt-3">
              <DefinitionList
                items={[
                  [intl.formatMessage(messages.sourceId), refreshTargetSourceId],
                  [intl.formatMessage(messages.documentDigest), refreshDocumentDigest],
                ]}
              />
            </div>
          </div>
        )}
        {state.sources.length === 0 ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.noSources)}
            description={intl.formatMessage(messages.noSourcesDescription)}
          />
        ) : (
          <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,300px),1fr))] gap-3">
            {state.sources.map((source) => (
              <article
                key={source.sourceId}
                className="rounded-xl border border-border-primary bg-background-primary p-4"
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <h3 className="truncate font-medium text-text-primary">{source.displayName}</h3>
                    <p className="truncate text-xs text-text-secondary">{source.sourceId}</p>
                  </div>
                  <StatusBadge tone={source.cache.freshness === 'fresh' ? 'success' : 'warning'}>
                    {formatMcpValue(intl, source.cache.freshness)}
                  </StatusBadge>
                </div>
                <div className="mt-3">
                  <DefinitionList
                    items={[
                      [
                        intl.formatMessage(messages.importKind),
                        formatMcpValue(intl, source.importKind),
                      ],
                      [intl.formatMessage(messages.manifests), source.manifestCount],
                      [
                        intl.formatMessage(messages.refresh),
                        formatMcpValue(intl, source.cache.refreshState),
                      ],
                      [
                        intl.formatMessage(messages.lastRefreshed),
                        source.lastRefreshedAtMs ?? intl.formatMessage(messages.absent),
                      ],
                      [
                        intl.formatMessage(messages.documentDigest),
                        source.lastRefreshDocumentDigest ?? intl.formatMessage(messages.absent),
                      ],
                      [intl.formatMessage(messages.compatible), source.compatibility.compatible],
                      [intl.formatMessage(messages.restricted), source.compatibility.restricted],
                      [intl.formatMessage(messages.denied), source.compatibility.denied],
                      [
                        intl.formatMessage(messages.offline),
                        intl.formatMessage(source.cache.offline ? messages.yes : messages.no),
                      ],
                    ]}
                  />
                </div>
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {source.trustTiers.map((tier) => (
                    <StatusBadge key={tier}>{formatMcpValue(intl, tier)}</StatusBadge>
                  ))}
                </div>
                {source.cache.refreshState !== 'local_only' && (
                  <div className="mt-4">
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => void refreshSource(source.sourceId)}
                      disabled={refreshingSourceId !== null}
                    >
                      {refreshingSourceId === source.sourceId
                        ? intl.formatMessage(messages.refreshingSource)
                        : intl.formatMessage(messages.refreshSource)}
                    </Button>
                  </div>
                )}
              </article>
            ))}
          </div>
        )}
        {state.sources.length > 0 && refreshableSources.length === 0 && (
          <div className="mt-3">
            <StatePanel
              kind="empty"
              title={intl.formatMessage(messages.noRefreshableSources)}
              description={intl.formatMessage(messages.noRefreshableSourcesDescription)}
            />
          </div>
        )}
      </section>
    </div>
  );
}

function ProvisionPreviewDefinitionList({
  intl,
  preview,
  timestamp,
}: {
  intl: ReturnType<typeof useIntl>;
  preview: McpSourceProvisionPreview;
  timestamp: [string, number];
}) {
  return (
    <DefinitionList
      items={[
        [intl.formatMessage(messages.sourceName), preview.displayName],
        [intl.formatMessage(messages.sourceId), preview.sourceId],
        [intl.formatMessage(messages.sourceRootDigest), preview.rootDigest],
        [intl.formatMessage(messages.documentDigest), preview.documentDigest],
        [intl.formatMessage(messages.endpointHost), preview.endpointHost],
        [
          intl.formatMessage(messages.refreshTransport),
          formatMcpValue(intl, preview.refreshTransport),
        ],
        [intl.formatMessage(messages.manifests), preview.manifestCount],
        [intl.formatMessage(messages.trustBasis), formatMcpValue(intl, preview.trustBasis)],
        [timestamp[0], formatSourceProvisionTimestamp(intl, timestamp[1])],
      ]}
    />
  );
}

function ProvisionWarnings({
  intl,
  warnings,
}: {
  intl: ReturnType<typeof useIntl>;
  warnings: string[];
}) {
  if (warnings.length === 0) return null;
  return (
    <div className="mt-4 rounded-lg border border-border-warning bg-background-warning/5 p-3">
      <h4 className="font-medium text-text-primary">
        {intl.formatMessage(messages.provisionWarnings)}
      </h4>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-sm text-text-secondary">
        {warnings.map((warning, index) => (
          <li key={`${index}-${warning}`}>{warning}</li>
        ))}
      </ul>
    </div>
  );
}

function formatSourceProvisionTimestamp(intl: ReturnType<typeof useIntl>, value: number): string {
  return `${intl.formatDate(value, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })} ${intl.formatTime(value, {
    hour: 'numeric',
    minute: '2-digit',
  })}`;
}

function sameProvisionPreview(
  expected: McpSourceProvisionPreview,
  actual: McpSourceProvisionPreview
): boolean {
  return (
    expected.sourceId === actual.sourceId &&
    expected.displayName === actual.displayName &&
    expected.rootDigest === actual.rootDigest &&
    expected.documentDigest === actual.documentDigest &&
    expected.endpointHost === actual.endpointHost &&
    expected.refreshTransport === actual.refreshTransport &&
    expected.manifestCount === actual.manifestCount &&
    expected.trustBasis === actual.trustBasis &&
    expected.warnings.length === actual.warnings.length &&
    expected.warnings.every((warning, index) => warning === actual.warnings[index])
  );
}

function summarizeImportEntries(result: McpGovernedImportResult | null) {
  if (!result) {
    return {
      entryCount: 0,
      compatible: 0,
      restricted: 0,
      denied: 0,
      trustTiers: [] as string[],
    };
  }

  const trustTiers = [...new Set(result.entries.map((entry) => entry.trustTier))];
  const compatible = result.entries.filter((entry) => entry.compatibility === 'compatible').length;
  return {
    entryCount: result.entries.length,
    compatible,
    restricted: result.entries.filter((entry) => entry.eligibility.outcome === 'restricted').length,
    denied: result.entries.filter((entry) => entry.eligibility.outcome === 'denied').length,
    trustTiers,
  };
}
