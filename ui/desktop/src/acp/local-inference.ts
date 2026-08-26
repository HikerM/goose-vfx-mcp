import type {
  LocalInferenceDownloadProgressDto,
  LocalInferenceHfModelInfoDto,
  LocalInferenceHfModelVariantDto,
  LocalInferenceModelDownloadRequest_unstable,
  LocalInferenceModelDto,
  LocalInferenceModelSettingsDto,
} from '@hikerm/lumina-sdk';
import { getAcpClient } from './acpConnection';

export type LocalModelResponse = LocalInferenceModelDto;
export type DownloadProgress = LocalInferenceDownloadProgressDto & {
  retryAttempt?: number;
  maxRetries?: number;
};
export type DownloadModelRequest = LocalInferenceModelDownloadRequest_unstable;
export type HfModelInfo = LocalInferenceHfModelInfoDto;
export type HfModelVariant = LocalInferenceHfModelVariantDto;
export type ModelSettings = LocalInferenceModelSettingsDto;
export type SamplingConfig = NonNullable<LocalInferenceModelSettingsDto['sampling']>;
export type ToolCallingMode = NonNullable<LocalInferenceModelSettingsDto['toolCalling']>;
export type ChatTemplate = NonNullable<LocalInferenceModelSettingsDto['chatTemplate']>;

export type RepoVariantsResponse = {
  variants: HfModelVariant[];
  recommendedIndex: number | null;
  availableMemoryBytes: number;
  downloadedQuants: string[];
  downloadedVariants: string[];
};

export async function listLocalModels(): Promise<LocalModelResponse[]> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceModelsList_unstable({});
  return response.models;
}

export async function downloadHfModel(request: DownloadModelRequest): Promise<string> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceModelsDownload_unstable(request);
  return response.modelId;
}

export async function getLocalModelDownloadProgress(
  modelId: string
): Promise<DownloadProgress | null> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceModelsDownloadProgress_unstable({ modelId });
  return response.progress ?? null;
}

export async function cancelLocalModelDownload(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.localInferenceModelsDownloadCancel_unstable({ modelId });
}

export async function deleteLocalModel(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.localInferenceModelsDelete_unstable({ modelId });
}

export async function evictLocalModel(modelId: string): Promise<void> {
  const client = await getAcpClient();
  await client.lumina.localInferenceModelsEvict_unstable({ modelId });
}

export async function getModelSettings(modelId: string): Promise<ModelSettings> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceModelsSettingsRead_unstable({ modelId });
  return response.settings;
}

export async function updateModelSettings(
  modelId: string,
  settings: ModelSettings
): Promise<ModelSettings> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceModelsSettingsUpdate_unstable({
    modelId,
    settings,
  });
  return response.settings;
}

export async function searchHfModels(query: string, limit?: number): Promise<HfModelInfo[]> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceHuggingfaceSearch_unstable({ query, limit });
  return response.models;
}

export async function getRepoFiles(repoId: string): Promise<RepoVariantsResponse> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceHuggingfaceRepoVariants_unstable({ repoId });
  return {
    variants: response.variants,
    recommendedIndex: response.recommendedIndex ?? null,
    availableMemoryBytes: response.availableMemoryBytes,
    downloadedQuants: response.downloadedQuants,
    downloadedVariants: response.downloadedVariants,
  };
}

export async function listBuiltinChatTemplates(): Promise<string[]> {
  const client = await getAcpClient();
  const response = await client.lumina.localInferenceChatTemplatesBuiltinList_unstable({});
  return response.templates;
}
