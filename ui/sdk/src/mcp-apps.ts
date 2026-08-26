import { RESOURCE_MIME_TYPE } from "@modelcontextprotocol/ext-apps/app-bridge";
import type {
  McpUiAppResourceConfig,
  McpUiAppToolConfig,
} from "@modelcontextprotocol/ext-apps/server";
import type {
  BlobResourceContents,
  ReadResourceResult,
  TextResourceContents,
  Tool,
} from "@modelcontextprotocol/sdk/types.js";

export const LUMINA_MCP_UI_EXTENSION_ID = "io.modelcontextprotocol/ui" as const;

export interface LuminaMcpUiExtensionSettings {
  mimeTypes: string[];
}

export interface LuminaMcpHostCapabilities {
  extensions: Record<string, LuminaMcpUiExtensionSettings>;
}

export type LuminaToolUiMetadata = Extract<
  McpUiAppToolConfig["_meta"],
  { ui: unknown }
>["ui"];

export type LuminaToolMetadata = NonNullable<Tool["_meta"]> & {
  ui?: LuminaToolUiMetadata;
  lumina_extension?: string;
};

export type LuminaSessionTool = Tool & {
  meta?: LuminaToolMetadata;
  _meta?: LuminaToolMetadata;
};

export type LuminaTextResourceContents = TextResourceContents;

export type LuminaBlobResourceContents = BlobResourceContents;

export type LuminaResourceContents = TextResourceContents | BlobResourceContents;

export type LuminaReadResourceResult = ReadResourceResult;

export type LuminaResourceMetadata = NonNullable<
  Extract<NonNullable<McpUiAppResourceConfig["_meta"]>, { ui?: unknown }>["ui"]
>;

export interface LuminaMcpAppToolPayload {
  toolName: string;
  extensionName: string;
  resourceUri: string;
  toolMeta?: LuminaToolMetadata;
  resourceResult?: LuminaReadResourceResult | null;
  readError?: string;
}

export interface LuminaToolCallUpdateMeta {
  lumina?: {
    mcpApp?: LuminaMcpAppToolPayload;
    [key: string]: unknown;
  };
  [key: string]: unknown;
}

export const DEFAULT_LUMINA_MCP_HOST_CAPABILITIES: LuminaMcpHostCapabilities = {
  extensions: {
    [LUMINA_MCP_UI_EXTENSION_ID]: {
      mimeTypes: [RESOURCE_MIME_TYPE],
    },
  },
};
