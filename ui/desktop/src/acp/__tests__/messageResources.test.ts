import type { ContentBlock, SessionNotification } from '@agentclientprotocol/sdk';
import { describe, expect, it } from 'vitest';
import { createAcpSessionNotificationAdapter } from '../sessionNotificationAdapter';
import { getResourceLinks, getToolResponses } from '../../types/message';

const resultId = '12345678-1234-8234-8234-123456789abc';
const reference: ContentBlock = {
  type: 'resource_link',
  uri: `lumina-media://result/${resultId}`,
  name: 'output.wav',
  title: 'Generated audio',
  mimeType: 'audio/wav',
  size: 64044,
  annotations: { audience: ['user'], priority: 0.7 },
  _meta: { 'lumina.mediaResult': { schemaVersion: 1, sessionId: 'session', resultId } },
};

function notification(
  content: ContentBlock,
  role: 'user' | 'assistant',
  messageId: string = role
): SessionNotification {
  return {
    sessionId: 'session',
    update: {
      sessionUpdate: role === 'user' ? 'user_message_chunk' : 'agent_message_chunk',
      messageId,
      content,
      _meta: { lumina: { created: 123 } },
    },
  };
}

describe('persistent media references through ACP messages', () => {
  it.each(['user', 'assistant'] as const)(
    'preserves %s resources and deduplicates replay without losing metadata',
    (role) => {
      const live = createAcpSessionNotificationAdapter();
      live.apply(notification({ type: 'text', text: 'Before' }, role));
      live.apply(notification(reference, role));
      live.apply(notification({ type: 'text', text: 'After' }, role));
      const persisted = JSON.parse(JSON.stringify(live.getMessages()));
      const restored = createAcpSessionNotificationAdapter(persisted);
      restored.apply(notification(reference, role));
      expect(restored.getMessages()).toEqual(persisted);
      expect(getResourceLinks(restored.getMessages()[0])).toEqual([reference]);
      expect(restored.getMessages()[0].content.map((block) => block.type)).toEqual([
        'text',
        'resource_link',
        'text',
      ]);
      restored.apply(notification(reference, role, 'another-message'));
      expect(getResourceLinks(restored.getMessages()[1])).toEqual([reference]);
    }
  );

  it('preserves tool resource annotations and the same durable identity', () => {
    const adapter = createAcpSessionNotificationAdapter();
    adapter.apply({
      sessionId: 'session',
      update: {
        sessionUpdate: 'tool_call_update',
        toolCallId: 'tool',
        status: 'completed',
        content: [{ type: 'content', content: reference }],
      },
    });
    const response = adapter.getMessages().flatMap(getToolResponses)[0];
    expect(response.toolResult).toMatchObject({
      status: 'success',
      value: { content: [reference] },
    });
  });
});
