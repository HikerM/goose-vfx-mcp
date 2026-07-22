import type { McpSourcesPolicyState } from '@aaif/goose-sdk';
import { DefinitionList, formatMcpValue, StatePanel, StatusBadge } from './McpCenterCommon';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export function SourcesPolicyTab({ state }: { state: McpSourcesPolicyState | null }) {
  const intl = useIntl();
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
    <div className="mx-auto max-w-5xl space-y-5">
      <section className="rounded-xl border border-border-primary bg-background-primary p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 className="text-lg font-medium text-text-primary">
              {intl.formatMessage(messages.machinePolicy)}
            </h2>
            <p className="mt-1 text-sm text-text-secondary">
              {intl.formatMessage(messages.readOnlyPolicy)}
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
                  <h3 className="truncate font-medium text-text-primary">{source.sourceId}</h3>
                  <StatusBadge tone={source.cache.freshness === 'fresh' ? 'success' : 'warning'}>
                    {formatMcpValue(intl, source.cache.freshness)}
                  </StatusBadge>
                </div>
                <div className="mt-3">
                  <DefinitionList
                    items={[
                      [intl.formatMessage(messages.manifests), source.manifestCount],
                      [
                        intl.formatMessage(messages.refresh),
                        formatMcpValue(intl, source.cache.refreshState),
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
              </article>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
