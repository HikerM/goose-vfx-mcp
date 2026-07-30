import { describe, expect, it, beforeEach } from 'vitest';
import { applyWindowsMcpEnvironment, setWindowsMcpEnvironment } from './extensions';

describe('Windows MCP environment', () => {
  beforeEach(() => setWindowsMcpEnvironment(undefined));

  it('adds only the main-process prepared runtime environment to stdio MCPs', () => {
    setWindowsMcpEnvironment({
      PATH: 'D:\\Goose\\bin;C:\\Windows\\System32',
      GOOSE_NODE_DIR: 'D:\\Goose\\runtime\\node',
      npm_config_cache: 'D:\\Goose\\runtime\\npm-cache',
      NPM_CONFIG_CACHE: 'D:\\Goose\\runtime\\npm-cache',
      TMP: 'D:\\Goose\\tmp',
      TEMP: 'D:\\Goose\\tmp',
      SECRET_FROM_RENDERER: 'must-not-be-forwarded',
    });
    const server = { name: 'npx-server', command: 'npx.cmd', args: ['--version'], env: [] };
    const extension = {
      type: 'mcp' as const,
      server,
    };

    expect(applyWindowsMcpEnvironment(extension)).toEqual({
      ...extension,
      server: {
        ...server,
        env: [
          { name: 'PATH', value: 'D:\\Goose\\bin;C:\\Windows\\System32' },
          { name: 'GOOSE_NODE_DIR', value: 'D:\\Goose\\runtime\\node' },
          { name: 'npm_config_cache', value: 'D:\\Goose\\runtime\\npm-cache' },
          { name: 'NPM_CONFIG_CACHE', value: 'D:\\Goose\\runtime\\npm-cache' },
          { name: 'TMP', value: 'D:\\Goose\\tmp' },
          { name: 'TEMP', value: 'D:\\Goose\\tmp' },
        ],
      },
    });
  });

  it('leaves non-stdio extensions unchanged', () => {
    setWindowsMcpEnvironment({ PATH: 'D:\\Goose\\bin' });
    const extension = {
      type: 'mcp' as const,
      server: { type: 'http' as const, name: 'remote', url: 'https://example.test', headers: [] },
    };
    expect(applyWindowsMcpEnvironment(extension)).toBe(extension);
  });

  it('preserves harmless configured stdio env and overrides controlled names', () => {
    setWindowsMcpEnvironment({ PATH: 'D:\\Goose\\bin', TMP: 'D:\\Goose\\tmp' });
    const extension = {
      type: 'mcp' as const,
      server: {
        name: 'server',
        command: 'npx.cmd',
        args: [],
        env: [
          { name: 'API_MODE', value: 'safe' },
          { name: 'PATH', value: 'C:\\attacker' },
          { name: 'temp', value: 'C:\\attacker' },
        ],
      },
    };
    const appliedExtension = applyWindowsMcpEnvironment(extension);
    if (appliedExtension.type !== 'mcp') {
      throw new Error('expected an MCP extension');
    }
    expect(appliedExtension.server).toEqual({
      ...extension.server,
      env: [
        { name: 'API_MODE', value: 'safe' },
        { name: 'PATH', value: 'D:\\Goose\\bin' },
        { name: 'TMP', value: 'D:\\Goose\\tmp' },
      ],
    });
  });
});
