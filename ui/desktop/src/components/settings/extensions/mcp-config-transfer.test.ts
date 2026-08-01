import { describe, expect, it } from 'vitest';
import type { FixedExtensionEntry } from '../../ConfigContext';
import { createMcpConfigExport, parseMcpConfigExport } from './mcp-config-transfer';

describe('MCP configuration transfer', () => {
  it('exports a custom stdio MCP without environment values', () => {
    const extension = {
      type: 'stdio',
      name: 'houdini',
      description: 'Houdini tools',
      cmd: 'fxhoudinimcp',
      args: ['--stdio'],
      env_keys: ['HOUDINI_PATH'],
      envs: { HOUDINI_PATH: 'C:\\Program Files\\Side Effects Software\\Houdini 20.5' },
      enabled: true,
    } as FixedExtensionEntry;

    const exported = createMcpConfigExport([extension]);

    expect(exported.extensions).toEqual([
      {
        config: {
          type: 'stdio',
          name: 'houdini',
          description: 'Houdini tools',
          cmd: 'fxhoudinimcp',
          args: ['--stdio'],
          cwd: undefined,
          env_keys: ['HOUDINI_PATH'],
          timeout: undefined,
          available_tools: undefined,
        },
        enabled: true,
      },
    ]);
    expect(JSON.stringify(exported)).not.toContain('Side Effects Software');
  });

  it('imports only supported custom MCP connection fields', () => {
    const imported = parseMcpConfigExport(
      JSON.stringify({
        version: 1,
        exportedAt: '2026-07-31T00:00:00.000Z',
        extensions: [
          {
            enabled: false,
            config: {
              type: 'stdio',
              name: 'houdini',
              cmd: 'fxhoudinimcp',
              args: ['--stdio'],
              envs: { SHOULD_NOT_IMPORT: 'secret' },
              headers: { Authorization: 'secret' },
            },
          },
        ],
      })
    );

    expect(imported.extensions).toEqual([
      {
        enabled: false,
        config: {
          type: 'stdio',
          name: 'houdini',
          description: undefined,
          cmd: 'fxhoudinimcp',
          args: ['--stdio'],
          cwd: undefined,
          env_keys: undefined,
          timeout: undefined,
          available_tools: undefined,
        },
      },
    ]);
  });

  it('keeps imported MCPs disabled until omitted secrets are configured', () => {
    const imported = parseMcpConfigExport(
      JSON.stringify({
        version: 1,
        extensions: [
          {
            enabled: true,
            config: {
              type: 'stdio',
              name: 'private-tools',
              cmd: 'private-tools-mcp',
              env_keys: ['PRIVATE_TOOLS_TOKEN'],
            },
          },
        ],
      })
    );

    expect(imported.extensions[0]).toMatchObject({
      enabled: false,
      config: { env_keys: ['PRIVATE_TOOLS_TOKEN'] },
    });
  });
});
