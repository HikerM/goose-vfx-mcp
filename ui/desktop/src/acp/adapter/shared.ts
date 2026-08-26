import type { ToolCall, ToolCallUpdate } from '@agentclientprotocol/sdk';
import type { TokenState } from '../../types/chat';
import type { Message, NotificationEvent } from '../../types/message';

export type AcpChatStateChange =
  | { type: 'messages'; messages: Message[] }
  | { type: 'tokenState'; tokenState: Partial<TokenState> }
  | { type: 'progressMessage'; message: string | undefined }
  | {
      type: 'sessionInfo';
      name?: string;
      activeRunId?: string | null;
    }
  | { type: 'localSteerConfirmed'; messageId: string }
  | { type: 'notification'; notification: NotificationEvent };

export interface AdapterState {
  messages: Message[];
  localSteerTextByMessageId: Map<string, string>;
}

export interface LuminaMessageMeta {
  messageId?: string;
  created?: number;
  steer?: boolean;
}

export interface ToolIdentity {
  toolName?: string;
  extensionName?: string;
}

export const DEFAULT_VISIBLE_MESSAGE_METADATA: Message['metadata'] = {
  userVisible: true,
  agentVisible: true,
};

export function messagesChange(state: AdapterState): AcpChatStateChange[] {
  return [{ type: 'messages', messages: state.messages.map(cloneMessage) }];
}

export function cloneMessage(message: Message): Message {
  return {
    ...message,
    content: message.content.map((content) => ({ ...content })),
    metadata: { ...message.metadata },
  };
}

export function getLuminaMessageMeta(update: { _meta?: unknown }): LuminaMessageMeta {
  if (!isRecord(update._meta)) {
    return {};
  }

  const lumina = update._meta.lumina;
  if (!isRecord(lumina)) {
    return {};
  }

  return {
    created: typeof lumina.created === 'number' ? lumina.created : undefined,
    messageId: typeof lumina.messageId === 'string' ? lumina.messageId : undefined,
    steer: lumina.steer === true ? true : undefined,
  };
}

export function getLuminaActiveRunId(update: { _meta?: unknown }): string | null | undefined {
  if (!isRecord(update._meta)) {
    return undefined;
  }

  const lumina = update._meta.lumina;
  if (!isRecord(lumina) || !('activeRunId' in lumina)) {
    return undefined;
  }

  return typeof lumina.activeRunId === 'string' || lumina.activeRunId === null
    ? lumina.activeRunId
    : undefined;
}

export function rawInputToArguments(rawInput: unknown): Record<string, unknown> {
  return isRecord(rawInput) ? rawInput : {};
}

export function toolIdentity(update: ToolCall | ToolCallUpdate): ToolIdentity {
  if (!isRecord(update._meta)) {
    return {};
  }

  const lumina = update._meta.lumina;
  if (!isRecord(lumina) || !isRecord(lumina.toolCall)) {
    return {};
  }

  return {
    toolName: typeof lumina.toolCall.toolName === 'string' ? lumina.toolCall.toolName : undefined,
    extensionName:
      typeof lumina.toolCall.extensionName === 'string' ? lumina.toolCall.extensionName : undefined,
  };
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}
