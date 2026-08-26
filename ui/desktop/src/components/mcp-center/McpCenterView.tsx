import { useCallback, useEffect, useState } from 'react';
import type { McpSourcesPolicyState, McpTaskRef } from '@hikerm/lumina-sdk';
import { Boxes, Compass, FileStack, ListChecks, Plus, ShieldCheck } from 'lucide-react';
import { MainPanelLayout } from '../Layout/MainPanelLayout';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../ui/tabs';
import { DiscoverTab } from './DiscoverTab';
import { ManagedTab } from './ManagedTab';
import { ManualTab } from './ManualTab';
import { ProfilesTab } from './ProfilesTab';
import { SourcesPolicyTab } from './SourcesPolicyTab';
import {
  getMcpSourcesPolicy,
  toMcpRecoveryViewModel,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import { formatMcpValue, RecoveryPanel } from './McpCenterCommon';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export default function McpCenterView() {
  const intl = useIntl();
  const [sourcesPolicy, setSourcesPolicy] = useState<McpSourcesPolicyState | null>(null);
  const [policyError, setPolicyError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [task, setTask] = useState<McpTaskRef | null>(null);
  const [tab, setTab] = useState('discover');
  const [discoverRefreshNonce, setDiscoverRefreshNonce] = useState(0);

  const loadPolicy = useCallback(async () => {
    setPolicyError(null);
    try {
      setSourcesPolicy(await getMcpSourcesPolicy());
    } catch (cause) {
      setPolicyError(toMcpRecoveryViewModel(cause));
    }
  }, []);

  useEffect(() => {
    void loadPolicy();
  }, [loadPolicy]);

  const handleTaskCreated = (nextTask: McpTaskRef) => {
    setTask(nextTask);
    setTab('managed');
  };

  const handleGovernedImportCompleted = useCallback(async () => {
    await loadPolicy();
    setDiscoverRefreshNonce((current) => current + 1);
  }, [loadPolicy]);

  return (
    <MainPanelLayout>
      <main className="flex h-full min-h-0 flex-col overflow-hidden">
        <header className="shrink-0 border-b border-border-primary px-4 pb-4 pt-5 sm:px-6 lg:px-8">
          <div className="mx-auto flex max-w-[1680px] flex-wrap items-end justify-between gap-3">
            <div>
              <div className="flex items-center gap-2 text-text-secondary">
                <Boxes className="h-5 w-5" />
                <span className="text-sm font-medium">
                  {intl.formatMessage(messages.managedPlatform)}
                </span>
              </div>
              <h1 className="mt-1 text-2xl font-medium text-text-primary">
                {intl.formatMessage(messages.title)}
              </h1>
              <p className="mt-1 max-w-3xl text-sm text-text-secondary">
                {intl.formatMessage(messages.subtitle)}
              </p>
            </div>
            {task && (
              <button
                onClick={() => setTab('managed')}
                className="rounded-full border border-border-primary px-3 py-1.5 text-sm text-text-primary hover:bg-background-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
              >
                {formatMcpValue(intl, task.operation)}: {formatMcpValue(intl, task.status)} ·{' '}
                {task.progress}%
              </button>
            )}
          </div>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-5 sm:px-6 lg:px-8">
          <div className="mx-auto max-w-[1680px]">
            {policyError && (
              <div className="mb-4">
                <RecoveryPanel recovery={policyError} onRetry={() => void loadPolicy()} />
              </div>
            )}
            <Tabs value={tab} onValueChange={setTab}>
              <TabsList
                className="mb-4 flex-wrap"
                aria-label={intl.formatMessage(messages.sectionsLabel)}
              >
                <TabsTrigger value="discover">
                  <Compass /> {intl.formatMessage(messages.discover)}
                </TabsTrigger>
                <TabsTrigger value="managed">
                  <ListChecks /> {intl.formatMessage(messages.myMcps)}
                </TabsTrigger>
                <TabsTrigger value="profiles">
                  <FileStack /> {intl.formatMessage(messages.profileTab)}
                </TabsTrigger>
                <TabsTrigger value="manual">
                  <Plus /> {intl.formatMessage(messages.manual)}
                </TabsTrigger>
                <TabsTrigger value="policy">
                  <ShieldCheck /> {intl.formatMessage(messages.sourcePolicy)}
                </TabsTrigger>
              </TabsList>
              <TabsContent value="discover">
                <DiscoverTab
                  sourcesPolicy={sourcesPolicy}
                  refreshNonce={discoverRefreshNonce}
                  onTaskCreated={handleTaskCreated}
                />
              </TabsContent>
              <TabsContent value="managed">
                <ManagedTab externalTask={task} onTaskChange={setTask} />
              </TabsContent>
              <TabsContent value="profiles">
                <ProfilesTab />
              </TabsContent>
              <TabsContent value="manual">
                <ManualTab onTaskCreated={handleTaskCreated} />
              </TabsContent>
              <TabsContent value="policy">
                <SourcesPolicyTab
                  state={sourcesPolicy}
                  onImported={handleGovernedImportCompleted}
                  onOpenDiscover={() => setTab('discover')}
                />
              </TabsContent>
            </Tabs>
          </div>
        </div>
      </main>
    </MainPanelLayout>
  );
}
