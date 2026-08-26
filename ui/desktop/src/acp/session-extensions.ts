import type { ExtensionConfig } from '../types/extensions';
import { getAcpClient } from './acpConnection';
import { extensionConfigToLuminaExtension, luminaExtensionToExtensionConfig } from './extensions';

export async function getSessionExtensions(sessionId: string): Promise<ExtensionConfig[]> {
  const client = await getAcpClient();
  const response = await client.lumina.sessionExtensionsList_unstable({ sessionId });
  return response.extensions
    .map(luminaExtensionToExtensionConfig)
    .filter((config): config is ExtensionConfig => config !== null);
}

export async function addSessionExtension(
  sessionId: string,
  config: ExtensionConfig
): Promise<void> {
  const extension = extensionConfigToLuminaExtension(config);
  if (!extension) {
    throw new Error(`Unsupported extension type for ACP: ${config.type}`);
  }
  const client = await getAcpClient();
  await client.lumina.sessionExtensionsAdd_unstable({ sessionId, extension });
}

export async function removeSessionExtension(sessionId: string, name: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.sessionExtensionsRemove_unstable({ sessionId, name });
}
