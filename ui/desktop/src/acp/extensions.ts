import type { ExtensionConfig, ExtensionEntry } from '../types/extensions';
import type { EnvVariable, LuminaExtension, LuminaExtensionEntry } from '@hikerm/lumina-sdk';
import { getAcpClient } from './acpConnection';

type ProcessEnvironment = typeof process.env;

let windowsMcpEnvironment: ProcessEnvironment | undefined;

export function setWindowsMcpEnvironment(environment: ProcessEnvironment | undefined): void {
  windowsMcpEnvironment = environment ? { ...environment } : undefined;
}

function mcpEnvironmentEntries(): EnvVariable[] {
  if (!windowsMcpEnvironment) return [];
  const names = ['PATH', 'LUMINA_NODE_DIR', 'npm_config_cache', 'NPM_CONFIG_CACHE', 'TMP', 'TEMP'];
  return names.flatMap((name) => {
    const value = windowsMcpEnvironment?.[name];
    return value === undefined ? [] : [{ name, value }];
  });
}

const controlledWindowsEnvironmentNames = new Set([
  'PATH',
  'LUMINA_NODE_DIR',
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
  return existing.filter(
    ({ name }) => !controlled.has(name) && !controlled.has(name.toUpperCase())
  );
}

export function applyWindowsMcpEnvironment(extension: LuminaExtension): LuminaExtension {
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

export function luminaExtensionName(extension: LuminaExtension): string {
  return extension.type === 'mcp' ? extension.server.name : extension.name;
}

function headersToRecord(headers: { name: string; value: string }[] = []) {
  return Object.fromEntries(headers.map(({ name, value }) => [name, value]));
}

function availableToolsOrUndefined(availableTools?: string[] | null): string[] | undefined {
  return availableTools?.length ? availableTools : undefined;
}

export function luminaExtensionToExtensionConfig(extension: LuminaExtension): ExtensionConfig | null {
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

function luminaExtensionEntryToExtensionEntry(
  entry: LuminaExtensionEntry
): ConfiguredExtensionEntry | null {
  const config = luminaExtensionToExtensionConfig(entry.extension);
  if (!config) {
    return null;
  }
  return { ...config, enabled: entry.enabled, configKey: entry.configKey ?? undefined };
}

export async function getConfiguredLuminaExtensions(): Promise<LuminaExtensionEntry[]> {
  const client = await getAcpClient();
  const response = await client.lumina.configExtensionsList_unstable({});
  return response.extensions.map((entry) => ({
    ...entry,
    extension: applyWindowsMcpEnvironment(entry.extension),
  }));
}

export async function getConfiguredExtensions(): Promise<ConfiguredExtensionsResponse> {
  const client = await getAcpClient();
  const response = await client.lumina.configExtensionsList_unstable({});
  return {
    extensions: response.extensions
      .map(luminaExtensionEntryToExtensionEntry)
      .filter((entry): entry is ConfiguredExtensionEntry => entry !== null),
    warnings: response.warnings ?? [],
  };
}

export function extensionConfigToLuminaExtension(config: ExtensionConfig): LuminaExtension | null {
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
  const extension = extensionConfigToLuminaExtension(config);
  if (!extension) {
    throw new Error(`Unsupported extension type for ACP: ${config.type}`);
  }
  const client = await getAcpClient();
  await client.lumina.configExtensionsAdd_unstable({ extension, enabled });
}

export async function removeConfigExtension(configKey: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.configExtensionsRemove_unstable({ configKey });
}

export async function setConfigExtensionEnabled(
  configKey: string,
  enabled: boolean
): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.configExtensionsSetEnabled_unstable({ configKey, enabled });
}
