import { useEffect, useState, useCallback, useMemo, useRef } from 'react';
import { Button } from '../../ui/button';
import { Download, Plus, Upload } from 'lucide-react';
import { GPSIcon } from '../../ui/icons';
import { useConfig, FixedExtensionEntry } from '../../ConfigContext';
import { defineMessages, useIntl } from '../../../i18n';
import ExtensionList from './subcomponents/ExtensionList';
import ExtensionModal from './modal/ExtensionModal';
import {
  createExtensionConfig,
  ExtensionFormData,
  extensionToFormData,
  getDefaultFormData,
  nameToKey,
} from './utils';

import { activateExtensionDefault, deleteExtension, toggleExtensionDefault } from './index';
import type { ExtensionConfig } from '../../../types/extensions';
import { listMcpAppTools } from '../../../acp/mcp-apps';
import type { ExtensionHealthCheckState } from './subcomponents/ExtensionItem';
import {
  createMcpConfigExport,
  mcpConfigNeedsSecrets,
  parseMcpConfigExport,
} from './mcp-config-transfer';
import { PRIMARY_EXTENSIONS_URL } from '../../../distribution-config';

const i18n = defineMessages({
  addCustomExtension: {
    id: 'extensionsSection.addCustomExtension',
    defaultMessage: 'Add custom extension',
  },
  browseExtensions: {
    id: 'extensionsSection.browseExtensions',
    defaultMessage: 'Browse extensions',
  },
  updateExtension: {
    id: 'extensionsSection.updateExtension',
    defaultMessage: 'Update Extension',
  },
  saveChanges: {
    id: 'extensionsSection.saveChanges',
    defaultMessage: 'Save Changes',
  },
  addExtension: {
    id: 'extensionsSection.addExtension',
    defaultMessage: 'Add Extension',
  },
  exportMcpConfigs: {
    id: 'extensionsSection.exportMcpConfigs',
    defaultMessage: 'Export custom MCPs',
  },
  importMcpConfigs: {
    id: 'extensionsSection.importMcpConfigs',
    defaultMessage: 'Import custom MCPs',
  },
  transferDescription: {
    id: 'extensionsSection.transferDescription',
    defaultMessage:
      'Exports connection details only. Environment values and HTTP header values are excluded for security and must be entered on the new computer.',
  },
  transferSuccess: {
    id: 'extensionsSection.transferSuccess',
    defaultMessage: 'Imported {count} custom MCP configuration(s).',
  },
  transferSuccessWithSecrets: {
    id: 'extensionsSection.transferSuccessWithSecrets',
    defaultMessage:
      'Imported {count} custom MCP configuration(s). {secretCount} remain disabled until their required secrets are entered.',
  },
  transferError: {
    id: 'extensionsSection.transferError',
    defaultMessage: 'MCP configuration transfer failed: {message}',
  },
});

interface ExtensionSectionProps {
  deepLinkConfig?: ExtensionConfig;
  showEnvVars?: boolean;
  hideButtons?: boolean;
  disableConfiguration?: boolean;
  customToggle?: (extension: FixedExtensionEntry) => Promise<boolean | void>;
  selectedExtensions?: string[]; // Add controlled state
  onModalClose?: (extensionName: string) => void;
  searchTerm?: string;
  healthCheckSessionId?: string;
  showTransferControls?: boolean;
}

export default function ExtensionsSection({
  deepLinkConfig,
  showEnvVars,
  hideButtons,
  disableConfiguration,
  customToggle,
  selectedExtensions = [],
  onModalClose,
  searchTerm = '',
  healthCheckSessionId,
  showTransferControls,
}: ExtensionSectionProps) {
  const intl = useIntl();
  const { getExtensions, addExtension, removeExtension, setExtensionEnabled, extensionsList } =
    useConfig();
  const [selectedExtension, setSelectedExtension] = useState<FixedExtensionEntry | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [isAddModalOpen, setIsAddModalOpen] = useState(false);
  const [deepLinkConfigStateVar, setDeepLinkConfigStateVar] = useState<
    ExtensionConfig | undefined | null
  >(deepLinkConfig);
  const [showEnvVarsStateVar, setShowEnvVarsStateVar] = useState<boolean | undefined | null>(
    showEnvVars
  );
  const [healthChecks, setHealthChecks] = useState<Record<string, ExtensionHealthCheckState>>({});
  const [transferStatus, setTransferStatus] = useState<
    { kind: 'success' | 'error'; message: string } | undefined
  >();
  const importInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setDeepLinkConfigStateVar(deepLinkConfig);
    setShowEnvVarsStateVar(showEnvVars);
  }, [deepLinkConfig, showEnvVars]);

  const extensions = useMemo(() => {
    if (extensionsList.length === 0) return [];

    return [...extensionsList]
      .sort((a, b) => {
        // First sort by builtin
        if (a.type === 'builtin' && b.type !== 'builtin') return -1;
        if (a.type !== 'builtin' && b.type === 'builtin') return 1;

        // Then sort by bundled (handle null/undefined cases)
        const aBundled = 'bundled' in a && a.bundled === true;
        const bBundled = 'bundled' in b && b.bundled === true;
        if (aBundled && !bBundled) return -1;
        if (!aBundled && bBundled) return 1;

        // Finally sort alphabetically within each group
        return a.name.localeCompare(b.name);
      })
      .map((ext) => ({
        ...ext,
        // Use selectedExtensions to determine enabled state in recipe editor
        enabled: disableConfiguration ? selectedExtensions.includes(ext.name) : ext.enabled,
      }));
  }, [extensionsList, disableConfiguration, selectedExtensions]);

  const fetchExtensions = useCallback(async () => {
    await getExtensions(true); // Force refresh - this will update the context
  }, [getExtensions]);

  const handleExtensionToggle = async (extensionConfig: FixedExtensionEntry) => {
    if (customToggle) {
      await customToggle(extensionConfig);
      return true;
    }

    const toggleDirection = extensionConfig.enabled ? 'toggleOff' : 'toggleOn';
    const configKey = extensionConfig.configKey ?? nameToKey(extensionConfig.name);

    await toggleExtensionDefault({
      toggle: toggleDirection,
      extensionConfig: extensionConfig,
      setEnabled: (enabled) => setExtensionEnabled(configKey, enabled),
    });

    await fetchExtensions();
    return true;
  };

  const handleConfigureClick = (extension: FixedExtensionEntry) => {
    setSelectedExtension(extension);
    setIsModalOpen(true);
  };

  const handleHealthCheck = useCallback(
    async (extension: FixedExtensionEntry) => {
      if (!healthCheckSessionId) return;

      const key = extension.configKey ?? nameToKey(extension.name);
      if (!extension.enabled) {
        setHealthChecks((current) => ({
          ...current,
          [key]: { status: 'error', message: 'Enable this MCP before checking it.' },
        }));
        return;
      }

      setHealthChecks((current) => ({ ...current, [key]: { status: 'checking' } }));
      try {
        const tools = await listMcpAppTools(healthCheckSessionId, extension.name);
        setHealthChecks((current) => ({
          ...current,
          [key]:
            tools.length > 0 ? { status: 'healthy', toolCount: tools.length } : { status: 'empty' },
        }));
      } catch (error) {
        setHealthChecks((current) => ({
          ...current,
          [key]: {
            status: 'error',
            message: error instanceof Error ? error.message : 'Unable to list MCP tools.',
          },
        }));
      }
    },
    [healthCheckSessionId]
  );

  const handleExportMcpConfigs = () => {
    const payload = createMcpConfigExport(extensions);
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = 'lumina-custom-mcps.json';
    anchor.click();
    URL.revokeObjectURL(url);
    setTransferStatus(undefined);
  };

  const handleImportMcpConfigs = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (!file) return;

    try {
      const payload = parseMcpConfigExport(await file.text());
      for (const { config, enabled } of payload.extensions) {
        await addExtension(config.name, config, enabled);
      }
      await fetchExtensions();
      const secretCount = payload.extensions.filter(({ config }) =>
        mcpConfigNeedsSecrets(config)
      ).length;
      setTransferStatus({
        kind: 'success',
        message: intl.formatMessage(
          secretCount > 0 ? i18n.transferSuccessWithSecrets : i18n.transferSuccess,
          { count: payload.extensions.length, secretCount }
        ),
      });
    } catch (error) {
      setTransferStatus({
        kind: 'error',
        message: intl.formatMessage(i18n.transferError, {
          message: error instanceof Error ? error.message : 'Unknown error',
        }),
      });
    }
  };

  const handleAddExtension = async (formData: ExtensionFormData) => {
    // Close the modal immediately
    handleModalClose();

    const extensionConfig = createExtensionConfig(formData);
    try {
      await activateExtensionDefault({
        addToConfig: addExtension,
        extensionConfig: extensionConfig,
      });
    } catch (error) {
      console.error('Failed to add extension:', error);
    } finally {
      await fetchExtensions();
      if (onModalClose) {
        setTimeout(() => {
          onModalClose(formData.name);
        }, 200);
      }
    }
  };

  const handleUpdateExtension = async (formData: ExtensionFormData) => {
    if (!selectedExtension) {
      console.error('No selected extension for update');
      return;
    }

    // Close the modal immediately
    handleModalClose();

    const extensionConfig = createExtensionConfig(formData);
    const originalName = selectedExtension.name;

    try {
      if (originalName !== extensionConfig.name) {
        await removeExtension(originalName);
      }
      await addExtension(extensionConfig.name, extensionConfig, formData.enabled);
    } catch (error) {
      console.error('Failed to update extension:', error);
    } finally {
      await fetchExtensions();
    }
  };

  const handleDeleteExtension = async (name: string) => {
    handleModalClose();

    try {
      await deleteExtension({
        name,
        removeFromConfig: removeExtension,
      });
    } catch (error) {
      console.error('Failed to delete extension:', error);
    } finally {
      await fetchExtensions();
    }
  };

  const handleModalClose = () => {
    setDeepLinkConfigStateVar(null);
    setShowEnvVarsStateVar(null);

    setIsModalOpen(false);
    setIsAddModalOpen(false);
    setSelectedExtension(null);

    // Clear any navigation state that might be cached
    if (window.history.state?.deepLinkConfig) {
      window.history.replaceState({}, '', window.location.hash);
    }
  };

  return (
    <section id="extensions">
      <div className="">
        <ExtensionList
          extensions={extensions}
          onToggle={handleExtensionToggle}
          onConfigure={handleConfigureClick}
          disableConfiguration={disableConfiguration}
          searchTerm={searchTerm}
          healthChecks={healthChecks}
          onHealthCheck={healthCheckSessionId ? handleHealthCheck : undefined}
        />

        {showTransferControls && (
          <div className="mt-4 rounded-lg border border-border-primary bg-background-secondary p-3">
            <p className="text-xs text-text-secondary">
              {intl.formatMessage(i18n.transferDescription)}
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={handleExportMcpConfigs}>
                <Download className="mr-1 h-3.5 w-3.5" />
                {intl.formatMessage(i18n.exportMcpConfigs)}
              </Button>
              <Button variant="outline" size="sm" onClick={() => importInputRef.current?.click()}>
                <Upload className="mr-1 h-3.5 w-3.5" />
                {intl.formatMessage(i18n.importMcpConfigs)}
              </Button>
              <input
                ref={importInputRef}
                className="hidden"
                type="file"
                accept="application/json,.json"
                onChange={(event) => void handleImportMcpConfigs(event)}
              />
            </div>
            {transferStatus && (
              <p
                role="status"
                className={`mt-3 text-xs ${
                  transferStatus.kind === 'success'
                    ? 'text-green-600 dark:text-green-400'
                    : 'text-red-600 dark:text-red-400'
                }`}
              >
                {transferStatus.message}
              </p>
            )}
          </div>
        )}

        {!hideButtons && (
          <div className="flex gap-4 pt-4 w-full">
            <Button
              className="flex items-center gap-2 justify-center"
              variant="default"
              onClick={() => setIsAddModalOpen(true)}
            >
              <Plus className="h-4 w-4" />
              {intl.formatMessage(i18n.addCustomExtension)}
            </Button>
            {PRIMARY_EXTENSIONS_URL && (
              <Button
                className="flex items-center gap-2 justify-center"
                variant="secondary"
                onClick={() => window.open(PRIMARY_EXTENSIONS_URL, '_blank')}
              >
                <GPSIcon size={12} />
                {intl.formatMessage(i18n.browseExtensions)}
              </Button>
            )}
          </div>
        )}

        {/* Modal for updating an existing extension */}
        {isModalOpen && selectedExtension && (
          <ExtensionModal
            title={intl.formatMessage(i18n.updateExtension)}
            initialData={extensionToFormData(selectedExtension)}
            onClose={handleModalClose}
            onSubmit={handleUpdateExtension}
            onDelete={handleDeleteExtension}
            submitLabel={intl.formatMessage(i18n.saveChanges)}
            modalType={'edit'}
          />
        )}

        {/* Modal for adding a new extension */}
        {isAddModalOpen && (
          <ExtensionModal
            title={intl.formatMessage(i18n.addCustomExtension)}
            initialData={getDefaultFormData()}
            onClose={handleModalClose}
            onSubmit={handleAddExtension}
            submitLabel={intl.formatMessage(i18n.addExtension)}
            modalType={'add'}
          />
        )}

        {/* Modal for adding extension from deeplink*/}
        {deepLinkConfigStateVar && showEnvVarsStateVar && (
          <ExtensionModal
            title={intl.formatMessage(i18n.addCustomExtension)}
            initialData={extensionToFormData({
              ...deepLinkConfig,
              enabled: true,
            } as FixedExtensionEntry)}
            onClose={handleModalClose}
            onSubmit={handleAddExtension}
            submitLabel={intl.formatMessage(i18n.addExtension)}
            modalType={'add'}
          />
        )}
      </div>
    </section>
  );
}
