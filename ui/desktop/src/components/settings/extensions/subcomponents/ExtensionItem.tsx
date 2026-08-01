import { useState, useEffect } from 'react';
import kebabCase from 'lodash/kebabCase';
import { CircleAlert, CircleCheck, LoaderCircle, Stethoscope } from 'lucide-react';
import { Switch } from '../../../ui/switch';
import { Gear } from '../../../icons';
import { Button } from '../../../ui/button';
import { FixedExtensionEntry } from '../../../ConfigContext';
import { getSubtitle, getFriendlyTitle } from './ExtensionList';
import { Card, CardHeader, CardTitle, CardContent, CardAction } from '../../../ui/card';
import { defineMessages, useIntl } from '../../../../i18n';

const i18n = defineMessages({
  configureExtension: {
    id: 'extensionItem.configureExtension',
    defaultMessage: 'Configure {name} Extension',
  },
  toggleExtension: {
    id: 'extensionItem.toggleExtension',
    defaultMessage: 'Toggle {name} extension On or Off',
  },
  checkExtension: {
    id: 'extensionItem.checkExtension',
    defaultMessage: 'Check {name} MCP connection',
  },
  checking: {
    id: 'extensionItem.checking',
    defaultMessage: 'Checking MCP tools…',
  },
  healthy: {
    id: 'extensionItem.healthy',
    defaultMessage: 'MCP ready · {count} tools found',
  },
  noTools: {
    id: 'extensionItem.noTools',
    defaultMessage: 'MCP connected, but no tools were found',
  },
  healthError: {
    id: 'extensionItem.healthError',
    defaultMessage: 'MCP check failed: {message}',
  },
});

export type ExtensionHealthCheckState =
  | { status: 'checking' }
  | { status: 'healthy'; toolCount: number }
  | { status: 'empty' }
  | { status: 'error'; message: string };

interface ExtensionItemProps {
  extension: FixedExtensionEntry;
  onToggle: (extension: FixedExtensionEntry) => Promise<boolean | void> | void;
  onConfigure?: (extension: FixedExtensionEntry) => void;
  isStatic?: boolean; // to not allow users to edit configuration
  healthCheck?: ExtensionHealthCheckState;
  onHealthCheck?: (extension: FixedExtensionEntry) => void;
}

export default function ExtensionItem({
  extension,
  onToggle,
  onConfigure,
  isStatic,
  healthCheck,
  onHealthCheck,
}: ExtensionItemProps) {
  const intl = useIntl();
  // Add local state to track the visual toggle state
  const [visuallyEnabled, setVisuallyEnabled] = useState(extension.enabled);
  // Track if we're in the process of toggling
  const [isToggling, setIsToggling] = useState(false);

  const handleToggle = async (ext: FixedExtensionEntry) => {
    // Prevent multiple toggles while one is in progress
    if (isToggling) return;

    setIsToggling(true);

    // Immediately update visual state
    const newState = !ext.enabled;
    setVisuallyEnabled(newState);

    try {
      // Call the actual toggle function that performs the async operation
      await onToggle(ext);
      // Success case is handled by the useEffect below when extension.enabled changes
    } catch {
      // If there was an error, revert the visual state
      setVisuallyEnabled(!newState);
    } finally {
      setIsToggling(false);
    }
  };

  // Update visual state when the actual extension state changes
  useEffect(() => {
    if (!isToggling) {
      setVisuallyEnabled(extension.enabled);
    }
  }, [extension.enabled, isToggling]);

  const renderSubtitle = () => {
    const { description, command } = getSubtitle(extension);
    return (
      <>
        {description && <span>{description}</span>}
        {description && command && <br />}
        {command && <span className="font-mono text-xs">{command}</span>}
      </>
    );
  };

  // Bundled extensions and builtins are not editable
  // Over time we can take the first part of the conditional away as people have bundled: true in their config.yaml entries

  // allow configuration editing if extension is not a builtin/bundled extension AND isStatic = false
  const editable =
    !(extension.type === 'builtin' || ('bundled' in extension && extension.bundled)) && !isStatic;

  return (
    <Card
      id={`extension-${kebabCase(extension.name)}`}
      className="transition-all duration-200 min-h-[120px] overflow-hidden"
    >
      <CardHeader>
        <CardTitle>{getFriendlyTitle(extension)}</CardTitle>

        <CardAction>
          <div className="flex items-center justify-end gap-2">
            {editable && (
              <button
                className="text-text-secondary hover:text-text-primary"
                aria-label={intl.formatMessage(i18n.configureExtension, {
                  name: getFriendlyTitle(extension),
                })}
                onClick={() => onConfigure?.(extension)}
              >
                <Gear className="w-4 h-4" />
              </button>
            )}
            {onHealthCheck && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                className="h-7 w-7"
                disabled={!extension.enabled || healthCheck?.status === 'checking'}
                aria-label={intl.formatMessage(i18n.checkExtension, {
                  name: getFriendlyTitle(extension),
                })}
                title={intl.formatMessage(i18n.checkExtension, {
                  name: getFriendlyTitle(extension),
                })}
                onClick={() => onHealthCheck(extension)}
              >
                {healthCheck?.status === 'checking' ? (
                  <LoaderCircle className="h-4 w-4 animate-spin" />
                ) : (
                  <Stethoscope className="h-4 w-4" />
                )}
              </Button>
            )}
            <Switch
              checked={visuallyEnabled}
              onCheckedChange={() => handleToggle(extension)}
              disabled={isToggling}
              variant="mono"
              aria-label={intl.formatMessage(i18n.toggleExtension, {
                name: getFriendlyTitle(extension),
              })}
            />
          </div>
        </CardAction>
      </CardHeader>
      <CardContent className="px-4 overflow-hidden text-sm break-words text-text-secondary">
        {renderSubtitle()}
        {healthCheck && (
          <div
            role="status"
            className={`mt-3 flex items-start gap-1.5 text-xs ${
              healthCheck.status === 'healthy'
                ? 'text-green-600 dark:text-green-400'
                : healthCheck.status === 'error'
                  ? 'text-red-600 dark:text-red-400'
                  : 'text-text-secondary'
            }`}
          >
            {healthCheck.status === 'healthy' && (
              <CircleCheck className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            )}
            {healthCheck.status === 'empty' && (
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            )}
            {healthCheck.status === 'error' && (
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            )}
            <span>
              {healthCheck.status === 'checking' && intl.formatMessage(i18n.checking)}
              {healthCheck.status === 'healthy' &&
                intl.formatMessage(i18n.healthy, { count: healthCheck.toolCount })}
              {healthCheck.status === 'empty' && intl.formatMessage(i18n.noTools)}
              {healthCheck.status === 'error' &&
                intl.formatMessage(i18n.healthError, { message: healthCheck.message })}
            </span>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
