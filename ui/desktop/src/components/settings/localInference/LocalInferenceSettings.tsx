import { useState, useEffect, useCallback, useRef } from 'react';
import {
  Download,
  Trash2,
  X,
  ChevronDown,
  ChevronUp,
  Settings2,
  Eye,
  RefreshCw,
  Cpu,
  KeyRound,
  PowerOff,
} from 'lucide-react';
import { Button } from '../../ui/button';
import { useModelAndProvider } from '../../ModelAndProviderContext';
import { defineMessages, useIntl } from '../../../i18n';
import {
  listLocalModels,
  downloadHfModel,
  getLocalModelDownloadProgress,
  cancelLocalModelDownload,
  deleteLocalModel,
  evictLocalModel,
  type DownloadProgress,
  type DownloadModelRequest,
  type LocalModelResponse,
} from '../../../acp/local-inference';
import { HuggingFaceModelSearch } from './HuggingFaceModelSearch';
import { ModelSettingsPanel } from './ModelSettingsPanel';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../../ui/dialog';
import { acpSaveDefaults } from '../../../acp/providers';
import { acpReadConfig, acpRemoveConfig, acpUpsertConfig } from '../../../acp/config';

const MODELSCOPE_TOKEN_SECRET_KEY = 'MODELSCOPE_TOKEN';

const i18n = defineMessages({
  title: {
    id: 'localInferenceSettings.title',
    defaultMessage: 'Local Inference Models',
  },
  description: {
    id: 'localInferenceSettings.description',
    defaultMessage:
      'Download and manage local GGUF models for inference without API keys. Search ModelScope or use the featured picks below.',
  },
  downloading: {
    id: 'localInferenceSettings.downloading',
    defaultMessage: 'Downloading',
  },
  downloadedModels: {
    id: 'localInferenceSettings.downloadedModels',
    defaultMessage: 'Downloaded Models',
  },
  featuredModels: {
    id: 'localInferenceSettings.featuredModels',
    defaultMessage: 'Featured Models',
  },
  recommended: {
    id: 'localInferenceSettings.recommended',
    defaultMessage: 'Recommended',
  },
  download: {
    id: 'localInferenceSettings.download',
    defaultMessage: 'Download',
  },
  showRecommendedOnly: {
    id: 'localInferenceSettings.showRecommendedOnly',
    defaultMessage: 'Show recommended only',
  },
  showAllFeatured: {
    id: 'localInferenceSettings.showAllFeatured',
    defaultMessage: 'Show all featured ({count} more)',
  },
  modelSettings: {
    id: 'localInferenceSettings.modelSettings',
    defaultMessage: 'Model Settings',
  },
  noModels: {
    id: 'localInferenceSettings.noModels',
    defaultMessage: 'No models available',
  },
  downloadProgress: {
    id: 'localInferenceSettings.downloadProgress',
    defaultMessage: '{downloaded} / {total} ({percent}%)',
  },
  remaining: {
    id: 'localInferenceSettings.remaining',
    defaultMessage: '{time} remaining',
  },
  downloadFailed: {
    id: 'localInferenceSettings.downloadFailed',
    defaultMessage: 'Download failed',
  },
  downloadCancelled: {
    id: 'localInferenceSettings.downloadCancelled',
    defaultMessage: 'Download cancelled',
  },
  automaticSetup: {
    id: 'localInferenceSettings.automaticSetup',
    defaultMessage:
      'Source: ModelScope · connection failures retry automatically · local GPU or CPU settings are applied when the download finishes.',
  },
  retryingConnection: {
    id: 'localInferenceSettings.retryingConnection',
    defaultMessage: 'Connection interrupted — retrying automatically ({attempt}/{maximum})',
  },
  retry: {
    id: 'localInferenceSettings.retry',
    defaultMessage: 'Retry',
  },
  dismiss: {
    id: 'localInferenceSettings.dismiss',
    defaultMessage: 'Dismiss',
  },
  deleteConfirm: {
    id: 'localInferenceSettings.deleteConfirm',
    defaultMessage: 'Delete this model? You can re-download it later.',
  },
  modelSettingsTitle: {
    id: 'localInferenceSettings.modelSettingsTitle',
    defaultMessage: 'Model settings',
  },
  loadedInMemory: {
    id: 'localInferenceSettings.loadedInMemory',
    defaultMessage: 'Loaded in memory',
  },
  evictFromMemory: {
    id: 'localInferenceSettings.evictFromMemory',
    defaultMessage: 'Evict from memory',
  },
  vision: {
    id: 'localInferenceSettings.vision',
    defaultMessage: 'Vision',
  },
  visionEncoderDownloading: {
    id: 'localInferenceSettings.visionEncoderDownloading',
    defaultMessage: 'Vision encoder downloading…',
  },
  visionEncoderNotDownloaded: {
    id: 'localInferenceSettings.visionEncoderNotDownloaded',
    defaultMessage: 'Vision encoder not downloaded',
  },
  modelScopeSourceNote: {
    id: 'localInferenceSettings.modelScopeSourceNote',
    defaultMessage:
      'Models are searched and downloaded from ModelScope. Public models do not require login; private or gated models require a ModelScope access token.',
  },
  modelScopeTokenTitle: {
    id: 'localInferenceSettings.modelScopeTokenTitle',
    defaultMessage: 'ModelScope access token',
  },
  modelScopeTokenDescription: {
    id: 'localInferenceSettings.modelScopeTokenDescription',
    defaultMessage:
      'Only needed for private or gated models. Saved in your operating system credential store and never added to Lumina configuration files.',
  },
  modelScopeTokenPlaceholder: {
    id: 'localInferenceSettings.modelScopeTokenPlaceholder',
    defaultMessage: 'Paste a new token to save or replace the current one',
  },
  saveToken: {
    id: 'localInferenceSettings.saveToken',
    defaultMessage: 'Save token',
  },
  clearToken: {
    id: 'localInferenceSettings.clearToken',
    defaultMessage: 'Clear token',
  },
  tokenConfigured: {
    id: 'localInferenceSettings.tokenConfigured',
    defaultMessage: 'A token is configured',
  },
});

const VisionBadge = ({
  model,
  intl,
}: {
  model: LocalModelResponse;
  intl: ReturnType<typeof useIntl>;
}) => {
  if (!model.visionCapable) return null;

  const mmproj = model.mmprojStatus;
  const isDownloaded = mmproj?.state === 'Downloaded';
  const isDownloading = mmproj?.state === 'Downloading';

  if (isDownloaded) {
    return (
      <span className="inline-flex items-center gap-1 text-xs text-green-400 bg-green-500/10 px-2 py-0.5 rounded">
        <Eye className="w-3 h-3" />
        {intl.formatMessage(i18n.vision)}
      </span>
    );
  }

  if (isDownloading) {
    const percent =
      mmproj && mmproj.progressPercent != null ? Math.round(mmproj.progressPercent) : null;
    return (
      <span className="inline-flex items-center gap-1 text-xs text-yellow-400 bg-yellow-500/10 px-2 py-0.5 rounded">
        <Eye className="w-3 h-3" />
        {intl.formatMessage(i18n.visionEncoderDownloading)}
        {percent != null && ` ${percent}%`}
      </span>
    );
  }

  return (
    <span className="inline-flex items-center gap-1 text-xs text-text-muted bg-background-subtle px-2 py-0.5 rounded">
      <Eye className="w-3 h-3" />
      {intl.formatMessage(i18n.vision)}
    </span>
  );
};

const formatBytes = (bytes: number): string => {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)}KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(0)}MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)}GB`;
};

export const LocalInferenceSettings = () => {
  const intl = useIntl();
  const [models, setModels] = useState<LocalModelResponse[]>([]);
  const [downloads, setDownloads] = useState<Map<string, DownloadProgress>>(new Map());
  const [downloadRequests, setDownloadRequests] = useState<Map<string, DownloadModelRequest>>(
    new Map()
  );
  const [evictingModelId, setEvictingModelId] = useState<string | null>(null);
  const [showAllFeatured, setShowAllFeatured] = useState(false);
  const [settingsOpenFor, setSettingsOpenFor] = useState<string | null>(null);
  const [modelScopeToken, setModelScopeToken] = useState('');
  const [modelScopeTokenConfigured, setModelScopeTokenConfigured] = useState(false);
  const [savingModelScopeToken, setSavingModelScopeToken] = useState(false);
  const { currentModel, currentProvider, refreshCurrentModelAndProvider } = useModelAndProvider();
  const downloadSectionRef = useRef<HTMLDivElement>(null);
  const activePolls = useRef(new Set<string>());
  const selectedModelId = currentProvider === 'local' ? currentModel : null;

  const loadModels = useCallback(async (): Promise<LocalModelResponse[] | undefined> => {
    try {
      const models = await listLocalModels();
      if (models) {
        setModels(models);
        const downloadedIds = new Set(
          models
            .filter((model) => model.status.state === 'Downloaded')
            .map((model) => model.id)
        );
        if (downloadedIds.size > 0) {
          setDownloads((prev) => {
            const next = new Map(prev);
            downloadedIds.forEach((modelId) => next.delete(modelId));
            return next;
          });
          setDownloadRequests((prev) => {
            const next = new Map(prev);
            downloadedIds.forEach((modelId) => next.delete(modelId));
            return next;
          });
        }
        models.forEach((model) => {
          if (model.status.state === 'Downloading') {
            pollDownloadProgress(model.id);
          }
        });

        return models;
      }
    } catch (error) {
      console.error('Failed to load models:', error);
    }
    return undefined;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    loadModels();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const loadModelScopeTokenStatus = async () => {
      try {
        const value = await acpReadConfig(MODELSCOPE_TOKEN_SECRET_KEY, true);
        setModelScopeTokenConfigured(value != null);
      } catch (error) {
        console.error('Failed to read ModelScope token status:', error);
      }
    };
    void loadModelScopeTokenStatus();
  }, []);

  const saveModelScopeToken = async () => {
    const token = modelScopeToken.trim();
    if (!token) return;
    setSavingModelScopeToken(true);
    try {
      await acpUpsertConfig(MODELSCOPE_TOKEN_SECRET_KEY, token, true);
      setModelScopeToken('');
      setModelScopeTokenConfigured(true);
    } catch (error) {
      console.error('Failed to save ModelScope token:', error);
    } finally {
      setSavingModelScopeToken(false);
    }
  };

  const clearModelScopeToken = async () => {
    setSavingModelScopeToken(true);
    try {
      await acpRemoveConfig(MODELSCOPE_TOKEN_SECRET_KEY, true);
      setModelScopeToken('');
      setModelScopeTokenConfigured(false);
    } catch (error) {
      console.error('Failed to clear ModelScope token:', error);
    } finally {
      setSavingModelScopeToken(false);
    }
  };

  // Poll model list while any vision encoder is downloading
  useEffect(() => {
    const hasDownloadingMmproj = models.some(
      (m) => m.visionCapable && m.mmprojStatus?.state === 'Downloading'
    );
    if (!hasDownloadingMmproj) return;

    const interval = setInterval(() => {
      loadModels();
    }, 2000);
    return () => clearInterval(interval);
  }, [models, loadModels]);

  const selectModel = async (modelId: string) => {
    try {
      await acpSaveDefaults('local', modelId);
      await refreshCurrentModelAndProvider();
    } catch (error) {
      console.error('Failed to select model:', error);
    }
  };

  const startFeaturedDownload = async (modelId: string) => {
    const model = models.find((m) => m.id === modelId);
    if (!model) return;
    const request = { spec: model.id };
    try {
      await downloadHfModel(request);
      setDownloadRequests((prev) => new Map(prev).set(modelId, request));
      pollDownloadProgress(modelId);
      scrollToDownloads();
    } catch (error) {
      console.error('Failed to start download:', error);
    }
  };

  const scrollToDownloads = useCallback(() => {
    requestAnimationFrame(() => {
      downloadSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    });
  }, []);

  const pollDownloadProgress = (modelId: string) => {
    if (activePolls.current.has(modelId)) return;
    activePolls.current.add(modelId);

    const stopPolling = (interval: ReturnType<typeof setInterval>) => {
      clearInterval(interval);
      activePolls.current.delete(modelId);
    };

    const interval = setInterval(async () => {
      try {
        const progress = await getLocalModelDownloadProgress(modelId);
        if (progress) {
          setDownloads((prev) => new Map(prev).set(modelId, progress));

          if (progress.status === 'completed') {
            stopPolling(interval);
            setDownloads((prev) => {
              const next = new Map(prev);
              next.delete(modelId);
              return next;
            });
            await loadModels();
            await selectModel(modelId);
          } else if (progress.status === 'failed' || progress.status === 'cancelled') {
            stopPolling(interval);
            await loadModels();
          }
        } else {
          stopPolling(interval);
        }
      } catch {
        stopPolling(interval);
      }
    }, 1000);
  };

  const cancelDownload = async (modelId: string) => {
    try {
      await cancelLocalModelDownload(modelId);
      setDownloads((prev) => {
        const next = new Map(prev);
        const progress = next.get(modelId);
        if (progress) {
          next.set(modelId, { ...progress, status: 'cancelled' });
        }
        return next;
      });
      await loadModels();
    } catch (error) {
      console.error('Failed to cancel download:', error);
    }
  };

  const dismissDownload = (modelId: string) => {
    setDownloads((prev) => {
      const next = new Map(prev);
      next.delete(modelId);
      return next;
    });
    setDownloadRequests((prev) => {
      const next = new Map(prev);
      next.delete(modelId);
      return next;
    });
  };

  const retryDownload = async (modelId: string) => {
    const request = downloadRequests.get(modelId) ?? { spec: modelId };
    dismissDownload(modelId);
    try {
      const nextModelId = await downloadHfModel(request);
      setDownloadRequests((prev) => new Map(prev).set(nextModelId, request));
      pollDownloadProgress(nextModelId);
      scrollToDownloads();
    } catch (error) {
      console.error('Failed to retry download:', error);
    }
  };

  const handleDeleteModel = async (modelId: string) => {
    if (!window.confirm(intl.formatMessage(i18n.deleteConfirm))) return;
    try {
      await deleteLocalModel(modelId);
      const updatedModels = await loadModels();

      if (selectedModelId === modelId && updatedModels) {
        const remainingDownloaded = updatedModels.filter(
          (m) => m.id !== modelId && m.status.state === 'Downloaded'
        );
        if (remainingDownloaded.length > 0) {
          selectModel(remainingDownloaded[0].id);
        }
      }
    } catch (error) {
      console.error('Failed to delete model:', error);
    }
  };

  const handleEvictModel = async (modelId: string) => {
    setEvictingModelId(modelId);
    try {
      await evictLocalModel(modelId);
      await loadModels();
    } catch (error) {
      console.error('Failed to evict model:', error);
    } finally {
      setEvictingModelId(null);
    }
  };

  const handleHfDownloadStarted = (modelId: string, request: DownloadModelRequest) => {
    setDownloadRequests((prev) => new Map(prev).set(modelId, request));
    pollDownloadProgress(modelId);
    loadModels();
    scrollToDownloads();
  };

  const isDownloaded = (model: LocalModelResponse) => model.status.state === 'Downloaded';
  const isNotDownloaded = (model: LocalModelResponse) =>
    model.status.state === 'NotDownloaded' && !downloads.has(model.id);

  const downloadedModels = models.filter(isDownloaded);
  const notDownloadedModels = models.filter(isNotDownloaded);
  const recommendedModels = notDownloadedModels.filter((m) => m.recommended);
  const displayedFeatured = showAllFeatured ? notDownloadedModels : recommendedModels;
  const showFeaturedToggle = notDownloadedModels.length > recommendedModels.length;
  const activeDownloadIds = new Set(
    Array.from(downloads.entries())
      .filter(([, progress]) => progress.status === 'downloading')
      .map(([modelId]) => modelId)
  );

  useEffect(() => {
    if (downloadedModels.length === 0) return;

    const interval = setInterval(() => {
      loadModels();
    }, 3000);
    return () => clearInterval(interval);
  }, [downloadedModels.length, loadModels]);

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-text-default font-medium">{intl.formatMessage(i18n.title)}</h3>
        <p className="text-xs text-text-muted max-w-2xl mt-1">
          {intl.formatMessage(i18n.description)}
        </p>
      </div>

      <p className="rounded-lg border border-border-subtle bg-background-default px-3 py-2 text-xs text-text-muted">
        {intl.formatMessage(i18n.modelScopeSourceNote)}
      </p>

      {/* Active Downloads */}
      {downloads.size > 0 && (
        <div ref={downloadSectionRef}>
          <h4 className="text-sm font-medium text-text-default mb-2">
            {intl.formatMessage(i18n.downloading)}
          </h4>
          <div className="space-y-2">
            {Array.from(downloads.entries()).map(([modelId, progress]) => {
              if (progress.status === 'completed') return null;
              return (
                <div
                  key={modelId}
                  className="border rounded-lg p-3 border-border-subtle bg-background-default"
                >
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-sm font-medium text-text-default truncate">
                      {modelId}
                    </span>
                    {progress.status === 'downloading' && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => cancelDownload(modelId)}
                        className="text-destructive hover:text-destructive"
                      >
                        <X className="w-4 h-4" />
                      </Button>
                    )}
                  </div>
                  <p className="mb-2 text-xs text-text-muted">
                    {intl.formatMessage(i18n.automaticSetup)}
                  </p>
                  {progress.status === 'downloading' && (
                    <div className="space-y-1">
                      <div className="w-full bg-gray-700 rounded-full h-2">
                        <div
                          className="bg-blue-500 h-2 rounded-full transition-all duration-300"
                          style={{ width: `${progress.progressPercent}%` }}
                        />
                      </div>
                      <div className="flex justify-between text-xs text-text-muted">
                        <span>
                          {intl.formatMessage(i18n.downloadProgress, {
                            downloaded: formatBytes(progress.bytesDownloaded),
                            total: formatBytes(progress.totalBytes),
                            percent: progress.progressPercent.toFixed(0),
                          })}
                        </span>
                        <span className="flex gap-2">
                          {progress.etaSeconds != null && progress.etaSeconds > 0 && (
                            <span>
                              {intl.formatMessage(i18n.remaining, {
                                time:
                                  progress.etaSeconds < 60
                                    ? `${Math.round(progress.etaSeconds)}s`
                                    : `${Math.round(progress.etaSeconds / 60)}m`,
                              })}
                            </span>
                          )}
                          {progress.speedBps != null && progress.speedBps > 0 && (
                            <span>{formatBytes(progress.speedBps)}/s</span>
                          )}
                        </span>
                      </div>
                      {(progress.retryAttempt ?? 0) > 0 && (
                        <p className="text-xs text-amber-600 dark:text-amber-400">
                          {intl.formatMessage(i18n.retryingConnection, {
                            attempt: progress.retryAttempt ?? 0,
                            maximum: progress.maxRetries ?? 10,
                          })}
                        </p>
                      )}
                    </div>
                  )}
                  {progress.status === 'failed' && (
                    <div className="space-y-2">
                      <p className="text-xs text-destructive">
                        {progress.error || intl.formatMessage(i18n.downloadFailed)}
                      </p>
                      <div className="flex gap-2">
                        <Button variant="outline" size="sm" onClick={() => retryDownload(modelId)}>
                          <RefreshCw className="w-3 h-3 mr-1" />
                          {intl.formatMessage(i18n.retry)}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => dismissDownload(modelId)}>
                          <X className="w-3 h-3 mr-1" />
                          {intl.formatMessage(i18n.dismiss)}
                        </Button>
                      </div>
                    </div>
                  )}
                  {progress.status === 'cancelled' && (
                    <div className="space-y-2">
                      <p className="text-xs text-text-muted">
                        {progress.error || intl.formatMessage(i18n.downloadCancelled)}
                      </p>
                      <div className="flex gap-2">
                        <Button variant="outline" size="sm" onClick={() => retryDownload(modelId)}>
                          <RefreshCw className="w-3 h-3 mr-1" />
                          {intl.formatMessage(i18n.retry)}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => dismissDownload(modelId)}>
                          <X className="w-3 h-3 mr-1" />
                          {intl.formatMessage(i18n.dismiss)}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Downloaded Models */}
      {downloadedModels.length > 0 && (
        <div>
          <h4 className="text-sm font-medium text-text-default mb-2">
            {intl.formatMessage(i18n.downloadedModels)}
          </h4>
          <div className="space-y-2">
            {downloadedModels.map((model) => {
              const isSelected = selectedModelId === model.id;
              return (
                <div
                  key={model.id}
                  className={`border rounded-lg p-3 transition-colors ${
                    isSelected
                      ? 'border-accent-primary bg-accent-primary/5'
                      : 'border-border-subtle bg-background-default hover:border-border-default'
                  }`}
                >
                  <div className="flex items-center justify-between gap-3">
                    <div className="flex min-w-0 items-center gap-2 flex-wrap">
                      <input
                        type="radio"
                        checked={isSelected}
                        onChange={() => selectModel(model.id)}
                        className="cursor-pointer"
                      />
                      <span className="text-sm font-medium text-text-default break-all">
                        {model.id}
                      </span>
                      <span className="text-xs text-text-muted">
                        {formatBytes(model.sizeBytes)}
                      </span>
                      {model.isLoaded && (
                        <span className="inline-flex items-center gap-1 text-xs text-green-400 bg-green-500/10 px-2 py-0.5 rounded">
                          <Cpu className="w-3 h-3" />
                          {intl.formatMessage(i18n.loadedInMemory)}
                        </span>
                      )}
                      {model.recommended && (
                        <span className="text-xs bg-blue-500 text-white px-2 py-0.5 rounded">
                          {intl.formatMessage(i18n.recommended)}
                        </span>
                      )}
                      <VisionBadge model={model} intl={intl} />
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setSettingsOpenFor(model.id)}
                        title={intl.formatMessage(i18n.modelSettingsTitle)}
                      >
                        <Settings2 className="w-4 h-4" />
                      </Button>
                      {model.isLoaded && (
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => handleEvictModel(model.id)}
                          disabled={evictingModelId === model.id}
                          title={intl.formatMessage(i18n.evictFromMemory)}
                        >
                          <PowerOff className="w-4 h-4" />
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleDeleteModel(model.id)}
                        className="text-destructive hover:text-destructive"
                      >
                        <Trash2 className="w-4 h-4" />
                      </Button>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* Featured Models (not yet downloaded) */}
      {displayedFeatured.length > 0 && (
        <div>
          <h4 className="text-sm font-medium text-text-default mb-2">
            {intl.formatMessage(i18n.featuredModels)}
          </h4>
          <div className="space-y-2">
            {displayedFeatured.map((model) => (
              <div
                key={model.id}
                className="border rounded-lg p-3 border-border-subtle bg-background-default hover:border-border-default"
              >
                <div className="flex items-center justify-between gap-3">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <h4 className="text-sm font-medium text-text-default">{model.id}</h4>
                      <span className="text-xs text-text-muted">
                        {formatBytes(model.sizeBytes)}
                      </span>
                      {model.recommended && (
                        <span className="text-xs bg-blue-500 text-white px-2 py-0.5 rounded">
                          {intl.formatMessage(i18n.recommended)}
                        </span>
                      )}
                      <VisionBadge model={model} intl={intl} />
                    </div>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => startFeaturedDownload(model.id)}
                  >
                    <Download className="w-4 h-4 mr-1" />
                    {intl.formatMessage(i18n.download)}
                  </Button>
                </div>
              </div>
            ))}
          </div>

          {showFeaturedToggle && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowAllFeatured(!showAllFeatured)}
              className="w-full text-text-muted hover:text-text-default mt-2"
            >
              {showAllFeatured ? (
                <>
                  <ChevronUp className="w-4 h-4 mr-1" />
                  {intl.formatMessage(i18n.showRecommendedOnly)}
                </>
              ) : (
                <>
                  <ChevronDown className="w-4 h-4 mr-1" />
                  {intl.formatMessage(i18n.showAllFeatured, {
                    count: notDownloadedModels.length - displayedFeatured.length,
                  })}
                </>
              )}
            </Button>
          )}
        </div>
      )}

      {/* ModelScope search */}
      <div className="border-t border-border-subtle pt-4">
        <div className="mb-4 rounded-lg border border-border-subtle bg-background-default p-3">
          <div className="flex items-start gap-2">
            <KeyRound className="mt-0.5 h-4 w-4 shrink-0 text-text-muted" />
            <div className="min-w-0 flex-1">
              <h4 className="text-sm font-medium text-text-default">
                {intl.formatMessage(i18n.modelScopeTokenTitle)}
              </h4>
              <p className="mt-1 text-xs text-text-muted">
                {intl.formatMessage(i18n.modelScopeTokenDescription)}
              </p>
              {modelScopeTokenConfigured && (
                <p className="mt-2 text-xs text-green-500">
                  {intl.formatMessage(i18n.tokenConfigured)}
                </p>
              )}
              <div className="mt-3 flex flex-col gap-2 sm:flex-row">
                <input
                  type="password"
                  autoComplete="off"
                  value={modelScopeToken}
                  onChange={(event) => setModelScopeToken(event.target.value)}
                  placeholder={intl.formatMessage(i18n.modelScopeTokenPlaceholder)}
                  className="min-w-0 flex-1 rounded-lg border border-border-subtle bg-background-subtle px-3 py-2 text-sm text-text-default placeholder:text-text-muted focus:outline-none focus:border-accent-primary"
                />
                <div className="flex gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={!modelScopeToken.trim() || savingModelScopeToken}
                    onClick={() => void saveModelScopeToken()}
                  >
                    {intl.formatMessage(i18n.saveToken)}
                  </Button>
                  {modelScopeTokenConfigured && (
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={savingModelScopeToken}
                      onClick={() => void clearModelScopeToken()}
                    >
                      {intl.formatMessage(i18n.clearToken)}
                    </Button>
                  )}
                </div>
              </div>
            </div>
          </div>
        </div>
        <HuggingFaceModelSearch
          onDownloadStarted={handleHfDownloadStarted}
          activeDownloadIds={activeDownloadIds}
          downloadedModelIds={
            new Set(models.filter((m) => m.status.state === 'Downloaded').map((m) => m.id))
          }
        />
      </div>

      {models.length === 0 && (
        <div className="text-center py-6 text-text-muted text-sm">
          {intl.formatMessage(i18n.noModels)}
        </div>
      )}

      <Dialog
        open={!!settingsOpenFor}
        onOpenChange={(open) => {
          if (!open) setSettingsOpenFor(null);
        }}
      >
        <DialogContent className="max-h-[80vh] overflow-y-auto sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage(i18n.modelSettings)}</DialogTitle>
            <p className="text-sm text-text-muted">{settingsOpenFor || ''}</p>
          </DialogHeader>
          {settingsOpenFor && <ModelSettingsPanel modelId={settingsOpenFor} />}
        </DialogContent>
      </Dialog>
    </div>
  );
};
