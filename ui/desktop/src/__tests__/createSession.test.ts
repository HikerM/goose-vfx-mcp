import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSession } from '../sessions';
import type { ExtensionConfig } from '../types/extensions';
import type { Session } from '../types/session';
import type { FixedExtensionEntry } from '../components/ConfigContext';
import type { LuminaExtension, LuminaExtensionEntry } from '@hikerm/lumina-sdk';
import { getConfiguredLuminaExtensions } from '../acp/extensions';
import { acpChatSessionController } from '../acp/chatSessionController';

vi.mock('../acp/extensions', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../acp/extensions')>();
  return {
    ...actual,
    getConfiguredLuminaExtensions: vi.fn(),
  };
});

vi.mock('../acp/chatSessionController', () => ({
  acpChatSessionController: {
    createSession: vi.fn(),
  },
}));

const testSession: Session = {
  id: 'session-1',
  name: 'untitled',
  message_count: 0,
  created_at: '2026-06-19T00:00:00.000Z',
  updated_at: '2026-06-19T00:00:00.000Z',
  working_dir: '/tmp',
  extension_data: { active: [], installed: [] },
};

const extensionConfig = (name: string): ExtensionConfig => ({
  name,
  type: 'builtin',
  description: `${name} extension`,
});

const configuredExtension = (name: string, enabled: boolean): FixedExtensionEntry => ({
  ...extensionConfig(name),
  enabled,
});

const luminaExtension = (name: string): LuminaExtension => ({
  type: 'builtin',
  name,
  description: `${name} extension`,
});

const luminaExtensionEntry = (name: string): LuminaExtensionEntry => ({
  extension: luminaExtension(name),
  enabled: true,
});

const mockedGetConfiguredLuminaExtensions = vi.mocked(getConfiguredLuminaExtensions);
const mockedCreateAcpSession = vi.mocked(acpChatSessionController.createSession);

describe('createSession ACP session extensions', () => {
  beforeEach(() => {
    mockedGetConfiguredLuminaExtensions.mockReset();
    mockedGetConfiguredLuminaExtensions.mockResolvedValue([
      luminaExtensionEntry('developer'),
      luminaExtensionEntry('memory'),
    ]);
    mockedCreateAcpSession.mockReset();
    mockedCreateAcpSession.mockResolvedValue(testSession);
  });

  it('sends non-empty extension configs as ACP session extensions', async () => {
    await createSession('/tmp', {
      extensionConfigs: [extensionConfig('developer')],
    });

    expect(mockedGetConfiguredLuminaExtensions).toHaveBeenCalledOnce();
    expect(mockedCreateAcpSession).toHaveBeenCalledWith('/tmp', [luminaExtension('developer')], {
      profileApplicationToken: undefined,
      recipeDeeplink: undefined,
      recipeId: undefined,
    });
  });

  it('falls back to enabled configured extensions when extension configs are empty', async () => {
    await createSession('/tmp', {
      extensionConfigs: [],
      allExtensions: [configuredExtension('developer', true), configuredExtension('memory', false)],
    });

    expect(mockedGetConfiguredLuminaExtensions).toHaveBeenCalledOnce();
    expect(mockedCreateAcpSession).toHaveBeenCalledWith('/tmp', [luminaExtension('developer')], {
      profileApplicationToken: undefined,
      recipeDeeplink: undefined,
      recipeId: undefined,
    });
  });

  it('omits ACP session extensions when no configured extensions are enabled', async () => {
    await createSession('/tmp', {
      allExtensions: [configuredExtension('developer', false)],
    });

    expect(mockedGetConfiguredLuminaExtensions).not.toHaveBeenCalled();
    expect(mockedCreateAcpSession).toHaveBeenCalledWith('/tmp', [], {
      profileApplicationToken: undefined,
      recipeDeeplink: undefined,
      recipeId: undefined,
    });
  });

  it('passes a profile application token only through session creation metadata', async () => {
    await createSession('/tmp', {
      profileApplicationToken: 'profile-application-token',
    });

    expect(mockedCreateAcpSession).toHaveBeenCalledWith('/tmp', [], {
      profileApplicationToken: 'profile-application-token',
      recipeDeeplink: undefined,
      recipeId: undefined,
    });
  });
});
