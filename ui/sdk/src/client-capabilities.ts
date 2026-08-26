import type { LuminaMcpHostCapabilities } from "./mcp-apps.js";

export interface LuminaClientCapabilitiesMeta {
  lumina?: {
    mcpHostCapabilities?: LuminaMcpHostCapabilities;
    customNotifications?: boolean;
  };
}
