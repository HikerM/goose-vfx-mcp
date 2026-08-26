import type {
  DictationDownloadProgress,
  DictationLocalModelStatus,
  DictationProviderStatusEntry,
} from '@hikerm/lumina-sdk';
import { getAcpClient } from './acpConnection';

export type { DictationProviderStatusEntry };

export type DictationProviders = Record<string, DictationProviderStatusEntry>;
export type LocalDictationModel = DictationLocalModelStatus;
export type LocalDictationDownloadProgress = DictationDownloadProgress;

export async function getDictationConfig(): Promise<DictationProviders> {
  const client = await getAcpClient();
  const response = await client.lumina.dictationConfig_unstable({});
  return response.providers ?? {};
}

export async function transcribeDictation(
  audio: string,
  mimeType: string,
  provider: string
): Promise<string> {
  const client = await getAcpClient();
  const response = await client.lumina.dictationTranscribe_unstable({ audio, mimeType, provider });
  return response.text;
}

export async function listLocalDictationModels(): Promise<LocalDictationModel[]> {
  const client = await getAcpClient();
  const response = await client.lumina.dictationModelsList_unstable({});
  return response.models;
}

export async function downloadLocalDictationModel(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.dictationModelsDownload_unstable({ modelId });
}

export async function getLocalDictationModelDownloadProgress(
  modelId: string
): Promise<LocalDictationDownloadProgress | null> {
  const client = await getAcpClient();
  const response = await client.lumina.dictationModelsDownloadProgress_unstable({ modelId });
  return response.progress ?? null;
}

export async function cancelLocalDictationModelDownload(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.dictationModelsCancel_unstable({ modelId });
}

export async function deleteLocalDictationModel(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.dictationModelsDelete_unstable({ modelId });
}
