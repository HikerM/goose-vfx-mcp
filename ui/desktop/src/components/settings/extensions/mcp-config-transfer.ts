import type { FixedExtensionEntry } from '../../ConfigContext';
import type { ExtensionConfig } from '../../../types/extensions';

const MCP_CONFIG_EXPORT_VERSION = 1;

export type McpConfigExport = {
  version: number;
  exportedAt: string;
  extensions: Array<{
    config: ExtensionConfig;
    enabled: boolean;
  }>;
};

function isMcpConnection(config: ExtensionConfig): boolean {
  return config.type === 'stdio' || config.type === 'sse' || config.type === 'streamable_http';
}

export function mcpConfigNeedsSecrets(config: ExtensionConfig): boolean {
  return (
    (config.type === 'stdio' || config.type === 'streamable_http') &&
    (config.env_keys?.length ?? 0) > 0
  );
}

function copyStringArray(value: unknown): string[] | undefined {
  if (!Array.isArray(value) || !value.every((entry) => typeof entry === 'string')) return undefined;
  return [...value];
}

function safeMcpConfig(extension: FixedExtensionEntry): ExtensionConfig | null {
  if (!isMcpConnection(extension)) return null;

  const base = {
    name: extension.name,
    description: extension.description,
    timeout: 'timeout' in extension ? extension.timeout : undefined,
    available_tools:
      'available_tools' in extension ? copyStringArray(extension.available_tools) : undefined,
  };

  switch (extension.type) {
    case 'stdio':
      return {
        ...base,
        type: 'stdio',
        cmd: extension.cmd,
        args: copyStringArray(extension.args),
        cwd: extension.cwd,
        env_keys: copyStringArray(extension.env_keys),
      };
    case 'streamable_http':
      return {
        ...base,
        type: 'streamable_http',
        uri: extension.uri,
        env_keys: copyStringArray(extension.env_keys),
        socket: extension.socket,
      };
    case 'sse':
      return {
        ...base,
        type: 'sse',
        uri: extension.uri,
      };
    default:
      return null;
  }
}

export function createMcpConfigExport(extensions: FixedExtensionEntry[]): McpConfigExport {
  return {
    version: MCP_CONFIG_EXPORT_VERSION,
    exportedAt: new Date().toISOString(),
    extensions: extensions.flatMap((extension) => {
      const config = safeMcpConfig(extension);
      return config ? [{ config, enabled: extension.enabled }] : [];
    }),
  };
}

function stringField(value: Record<string, unknown>, key: string): string | undefined {
  const field = value[key];
  return typeof field === 'string' ? field : undefined;
}

function optionalStringArray(
  value: Record<string, unknown>,
  key: string
): string[] | undefined | null {
  const field = value[key];
  if (field == null) return undefined;
  return copyStringArray(field);
}

function parseMcpConfig(value: unknown): ExtensionConfig | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const type = stringField(record, 'type');
  const name = stringField(record, 'name')?.trim();
  if (!name) return null;

  const description = stringField(record, 'description');
  const timeout = typeof record.timeout === 'number' ? record.timeout : undefined;
  const availableTools = optionalStringArray(record, 'available_tools');
  if (availableTools === null) return null;

  if (type === 'stdio') {
    const cmd = stringField(record, 'cmd')?.trim();
    const args = optionalStringArray(record, 'args');
    const envKeys = optionalStringArray(record, 'env_keys');
    if (!cmd || args === null || envKeys === null) return null;
    return {
      type,
      name,
      description,
      cmd,
      args,
      cwd: stringField(record, 'cwd')?.trim() || undefined,
      env_keys: envKeys,
      timeout,
      available_tools: availableTools,
    };
  }

  if (type === 'streamable_http') {
    const uri = stringField(record, 'uri')?.trim();
    const envKeys = optionalStringArray(record, 'env_keys');
    if (!uri || envKeys === null) return null;
    return {
      type,
      name,
      description,
      uri,
      env_keys: envKeys,
      timeout,
      available_tools: availableTools,
      socket: stringField(record, 'socket'),
    };
  }

  if (type === 'sse') {
    const uri = stringField(record, 'uri')?.trim();
    if (!uri) return null;
    return { type, name, description, uri };
  }

  return null;
}

export function parseMcpConfigExport(text: string): McpConfigExport {
  const parsed: unknown = JSON.parse(text);
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error('The selected file is not an MCP configuration export.');
  }
  const record = parsed as Record<string, unknown>;
  if (record.version !== MCP_CONFIG_EXPORT_VERSION || !Array.isArray(record.extensions)) {
    throw new Error('The selected file uses an unsupported MCP configuration format.');
  }

  const extensions = record.extensions.flatMap((entry) => {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return [];
    const item = entry as Record<string, unknown>;
    const config = parseMcpConfig(item.config);
    if (!config) return [];
    return [{ config, enabled: item.enabled !== false && !mcpConfigNeedsSecrets(config) }];
  });
  if (extensions.length === 0) {
    throw new Error('The selected file does not contain a usable custom MCP configuration.');
  }

  return {
    version: MCP_CONFIG_EXPORT_VERSION,
    exportedAt: stringField(record, 'exportedAt') ?? '',
    extensions,
  };
}
