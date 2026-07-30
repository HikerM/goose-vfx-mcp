import type { ExtensionConfig, ExtensionEntry } from '../types/extensions';
import type { EnvVariable, GooseExtension, GooseExtensionEntry } from '@aaif/goose-sdk';
import { getAcpClient } from './acpConnection';

let windowsMcpEnvironment: NodeJS.ProcessEnv | undefined;

export function setWindowsMcpEnvironment(environment: NodeJS.ProcessEnv | undefined): void {
  windowsMcpEnvironment = environment ? { ...environment } : undefined;
}

function mcpEnvironmentEntries(): EnvVariable[] {
  if (!windowsMcpEnvironment) return [];
  const names = ['PATH', 'GOOSE_NODE_DIR', 'npm_config_cache', 'NPM_CONFIG_CACHE', 'TMP', 'TEMP'];
  return names.flatMap((name) => {
    const value = windowsMcpEnvironment?.[name];
    return value === undefined ? [] : [{ name, value }];
  });
}

const controlledWindowsEnvironmentNames = new Set([
  'PATH',
  'GOOSE_NODE_DIR',
  'NPM_CONFIG_CACHE',
  'NPM_CONFIG_USERCONFIG',
  'NPM_CONFIG_PREFIX',
  'NPM_CONFIG_TMP',
  'npm_config_cache',
  'npm_config_userconfig',
  'npm_config_prefix',
  'npm_config_tmp',
  'TMP',
  'TEMP',
]);

function mergeStdioEnvironment(existing: EnvVariable[]): EnvVariable[] {
  const controlled = new Set(controlledWindowsEnvironmentNames);
  return existing.filter(({ name }) => !controlled.has(name) && !controlled.has(name.toUpperCase()));
}

export function applyWindowsMcpEnvironment(extension: GooseExtension): GooseExtension {
  if (extension.type !== 'mcp' || !('command' in extension.server) || !windowsMcpEnvironment) {
    return extension;
  }
  return {
    ...extension,
    server: {
      ...extension.server,
      env: [...mergeStdioEnvironment(extension.server.env ?? []), ...mcpEnvironmentEntries()],
    },
  };
}

export type ConfiguredExtensionEntry = ExtensionEntry & { configKey?: string };

export interface ConfiguredExtensionsResponse {
  extensions: ConfiguredExtensionEntry[];
  warnings: string[];
}

export function gooseExtensionName(extension: GooseExtension): string {
  return extension.type === 'mcp' ? extension.server.name : extension.name;
}

function headersToRecord(headers: { name: string; value: string }[] = []) {
  return Object.fromEntries(headers.map(({ name, value }) => [name, value]));
}

function availableToolsOrUndefined(availableTools?: string[] | null): string[] | undefined {
  return availableTools?.length ? availableTools : undefined;
}

export function gooseExtensionToExtensionConfig(extension: GooseExtension): ExtensionConfig | null {
  switch (extension.type) {
    case 'builtin':
    case 'platform':
      return {
        ...extension,
        description: extension.description ?? '',
        available_tools: availableToolsOrUndefined(extension.available_tools),
      };
    case 'mcp': {
      const server = extension.server;
      if ('command' in server) {
        return {
          type: 'stdio',
          name: server.name,
          description: extension.description ?? '',
          cmd: server.command,
          args: server.args,
          env_keys: extension.envKeys ?? [],
          timeout: extension.timeout,
          bundled: extension.bundled,
          available_tools: availableToolsOrUndefined(extension.available_tools),
        };
      }
      if ('url' in server) {
        return {
          type: 'streamable_http',
          name: server.name,
          description: extension.description ?? '',
          uri: server.url,
          headers: headersToRecord(server.headers),
          env_keys: extension.envKeys ?? [],
          timeout: extension.timeout,
          socket: extension.socket,
          bundled: extension.bundled,
          available_tools: availableToolsOrUndefined(extension.available_tools),
        };
      }
      return null;
    }
  }
}

function gooseExtensionEntryToExtensionEntry(
  entry: GooseExtensionEntry
): ConfiguredExtensionEntry | null {
  const config = gooseExtensionToExtensionConfig(entry.extension);
  if (!config) {
    return null;
  }
  return { ...config, enabled: entry.enabled, configKey: entry.configKey ?? undefined };
}

export async function getConfiguredGooseExtensions(): Promise<GooseExtensionEntry[]> {
  const client = await getAcpClient();
  const response = await client.goose.configExtensionsList_unstable({});
  return response.extensions.map((entry) => ({
    ...entry,
    extension: applyWindowsMcpEnvironment(entry.extension),
  }));
}

export async function getConfiguredExtensions(): Promise<ConfiguredExtensionsResponse> {
  const client = await getAcpClient();
  const response = await client.goose.configExtensionsList_unstable({});
  return {
    extensions: response.extensions
      .map(gooseExtensionEntryToExtensionEntry)
      .filter((entry): entry is ConfiguredExtensionEntry => entry !== null),
    warnings: response.warnings ?? [],
  };
}

export function extensionConfigToGooseExtension(config: ExtensionConfig): GooseExtension | null {
  switch (config.type) {
    case 'builtin':
      return {
        type: 'builtin',
        name: config.name,
        description: config.description,
        display_name: config.display_name,
        timeout: config.timeout,
        bundled: config.bundled,
        available_tools: availableToolsOrUndefined(config.available_tools),
      };
    case 'platform':
      return {
        type: 'platform',
        name: config.name,
        description: config.description,
        display_name: config.display_name,
        bundled: config.bundled,
        available_tools: availableToolsOrUndefined(config.available_tools),
      };
    case 'stdio':
      return {
        type: 'mcp',
        server: { name: config.name, command: config.cmd, args: config.args ?? [], env: [] },
        envKeys: config.env_keys ?? [],
        description: config.description,
        timeout: config.timeout,
        bundled: config.bundled,
        available_tools: availableToolsOrUndefined(config.available_tools),
      };
    case 'streamable_http':
      return {
        type: 'mcp',
        server: {
          type: 'http',
          name: config.name,
          url: config.uri,
          headers: Object.entries(config.headers ?? {}).map(([name, value]) => ({ name, value })),
        },
        envKeys: config.env_keys ?? [],
        description: config.description,
        timeout: config.timeout,
        socket: config.socket,
        bundled: config.bundled,
        available_tools: availableToolsOrUndefined(config.available_tools),
      };
    case 'sse':
    case 'frontend':
    case 'inline_python':
      return null;
  }
}

export async function addConfigExtension(config: ExtensionConfig, enabled: boolean): Promise<void> {
  const extension = extensionConfigToGooseExtension(config);
  if (!extension) {
    throw new Error(`Unsupported extension type for ACP: ${config.type}`);
  }
  const client = await getAcpClient();
  await client.goose.configExtensionsAdd_unstable({ extension, enabled });
}

export async function removeConfigExtension(configKey: string): Promise<void> {
  const client = await getAcpClient();
  await client.goose.configExtensionsRemove_unstable({ configKey });
}

export async function setConfigExtensionEnabled(
  configKey: string,
  enabled: boolean
): Promise<void> {
  const client = await getAcpClient();
  await client.goose.configExtensionsSetEnabled_unstable({ configKey, enabled });
}
