import type { ContentBlock, SessionNotification } from '@agentclientprotocol/sdk';
import { describe, expect, it } from 'vitest';
import { getAudioContent, getTextAndImageContent } from '../../types/message';
import { createAcpSessionNotificationAdapter } from '../sessionNotificationAdapter';

const audio: ContentBlock = {
  type: 'audio',
  data: 'AQID',
  mimeType: 'audio/wav',
  annotations: { audience: ['user'], priority: 0.5 },
  _meta: { origin: 'test' },
};

function chunk(
  content: ContentBlock,
  messageId = 'assistant-1',
  role: 'user' | 'assistant' = 'assistant'
): SessionNotification {
  return {
    sessionId: 'audio-session',
    update: {
      sessionUpdate: role === 'user' ? 'user_message_chunk' : 'agent_message_chunk',
      content,
      messageId,
      _meta: { lumina: { created: 123 } },
    },
  };
}

describe('ACP audio messages', () => {
  it('keeps complete audio blocks and metadata while merging neighboring text', () => {
    const adapter = createAcpSessionNotificationAdapter();
    adapter.apply(chunk({ type: 'text', text: 'Hello ' }));
    adapter.apply(chunk({ type: 'text', text: 'world' }));
    const changes = adapter.apply(chunk(audio));
    adapter.apply(chunk({ type: 'text', text: ' transcript' }));
    adapter.apply(chunk({ type: 'image', data: 'image', mimeType: 'image/png' }));
    adapter.apply(chunk({ type: 'image', data: 'image', mimeType: 'image/png' }));

    expect(changes[0]).toMatchObject({ type: 'messages' });
    const [message] = adapter.getMessages();
    expect(message.content).toEqual([
      { type: 'text', text: 'Hello world' },
      audio,
      { type: 'text', text: ' transcript' },
      { type: 'image', data: 'image', mimeType: 'image/png' },
    ]);
    expect(getAudioContent(message)).toEqual([audio]);
    expect(getTextAndImageContent(message)).toEqual({
      textContent: 'Hello world transcript',
      imagePaths: ['data:image/png;base64,image'],
    });
  });

  it('deduplicates the same clip only inside the same message, without concatenating audio data', () => {
    const adapter = createAcpSessionNotificationAdapter();
    adapter.apply(chunk(audio));
    adapter.apply(chunk({ type: 'text', text: 'caption' }));
    adapter.apply(chunk(audio));
    adapter.apply(chunk({ ...audio, data: 'BAUG' }));
    adapter.apply(chunk({ ...audio, mimeType: 'audio/ogg' }));
    adapter.apply(chunk(audio, 'assistant-2'));
    adapter.apply(chunk(audio, 'user-1', 'user'));
    const messages = adapter.getMessages();
    expect(messages).toHaveLength(3);
    expect(getAudioContent(messages[0]).map(({ data, mimeType }) => ({ data, mimeType }))).toEqual([
      { data: 'AQID', mimeType: 'audio/wav' },
      { data: 'BAUG', mimeType: 'audio/wav' },
      { data: 'AQID', mimeType: 'audio/ogg' },
    ]);
    expect(getAudioContent(messages[1])).toEqual([audio]);
    expect(messages[2]).toMatchObject({ role: 'user', content: [audio] });
  });

  it('restores serialized history and reconciles replayed audio by message id', () => {
    const original = createAcpSessionNotificationAdapter();
    original.apply(chunk(audio));
    original.apply(chunk({ type: 'text', text: 'caption' }));
    original.apply(chunk(audio, 'user-1', 'user'));
    const history = JSON.parse(JSON.stringify(original.getMessages()));
    const restored = createAcpSessionNotificationAdapter(history);
    restored.apply(chunk(audio));
    restored.apply(chunk(audio, 'user-1', 'user'));
    expect(restored.getMessages()).toEqual(history);
    expect(restored.getMessages()[0].content[0]).not.toBe(history[0].content[0]);

    const replayed = createAcpSessionNotificationAdapter();
    replayed.apply(chunk(audio));
    replayed.apply(chunk({ type: 'text', text: 'caption' }));
    replayed.apply(chunk(audio, 'user-1', 'user'));
    expect(replayed.getMessages()).toEqual(history);
  });
});
