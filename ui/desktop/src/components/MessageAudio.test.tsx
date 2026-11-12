import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { IntlTestWrapper } from '../i18n/test-utils';
import { createAcpSessionNotificationAdapter } from '../acp/sessionNotificationAdapter';
import type { Message } from '../types/message';
import LuminaMessage from './LuminaMessage';
import UserMessage from './UserMessage';
import { SessionMessages } from './sessions/SessionViewComponents';

vi.mock('./MarkdownContent', () => ({
  default: ({ content }: { content: string }) => <p>{content}</p>,
}));
vi.mock('./ToolCallWithResponse', () => ({ default: () => null }));
vi.mock('./ToolCallConfirmation', () => ({ default: () => null }));
vi.mock('./ElicitationRequest', () => ({ default: () => null }));
vi.mock('./MessageCopyLink', () => ({ default: () => null }));
vi.mock('./MessageUsageStats', () => ({ default: () => null }));

beforeEach(() => {
  vi.stubGlobal(
    'URL',
    class extends URL {
      static createObjectURL = vi.fn(() => 'blob:message-audio');
      static revokeObjectURL = vi.fn();
    }
  );
});
afterEach(() => vi.unstubAllGlobals());

function streamedMessage(role: 'user' | 'assistant', mixed = false): Message {
  const adapter = createAcpSessionNotificationAdapter();
  const content: Message['content'] = [{ type: 'audio', data: 'AQID', mimeType: 'audio/wav' }];
  if (mixed) {
    content.push({ type: 'text', text: 'Speech transcript' });
    content.push({ type: 'image', data: 'AQID', mimeType: 'image/png' });
  }
  for (const block of content) {
    if (block.type === 'audio' || block.type === 'text' || block.type === 'image') {
      adapter.apply({
        sessionId: 'audio-session',
        update: {
          sessionUpdate: role === 'user' ? 'user_message_chunk' : 'agent_message_chunk',
          messageId: role,
          content: block,
        },
      });
    }
  }
  return adapter.getMessages()[0];
}

function assistant(message: Message, isStreaming: boolean) {
  return (
    <LuminaMessage
      sessionId="audio-session"
      message={message}
      messages={[message]}
      toolCallNotifications={new Map()}
      append={vi.fn()}
      isStreaming={isStreaming}
    />
  );
}

describe('chat and history audio rendering', () => {
  it('shows an audio-only assistant stream and keeps its player when text arrives', () => {
    const message = streamedMessage('assistant');
    const { rerender } = render(assistant(message, true), { wrapper: IntlTestWrapper });
    const player = screen.getByLabelText('Audio message');
    rerender(
      assistant(
        { ...message, content: [...message.content, { type: 'text', text: 'Later text' }] },
        false
      )
    );
    expect(screen.getByLabelText('Audio message')).toBe(player);
    expect(screen.getByText('Later text')).toBeInTheDocument();
  });

  it.each(['user', 'assistant'] as const)('retains text and images alongside %s audio', (role) => {
    const message = streamedMessage(role, true);
    render(role === 'user' ? <UserMessage message={message} /> : assistant(message, false), {
      wrapper: IntlTestWrapper,
    });
    expect(screen.getByLabelText('Audio message')).toHaveAttribute('controls');
    expect(screen.getByText('Speech transcript')).toBeInTheDocument();
    expect(screen.getByAltText('Lumina image')).toHaveAttribute(
      'src',
      'data:image/png;base64,AQID'
    );
  });

  it('renders user audio without text', () => {
    render(<UserMessage message={streamedMessage('user')} />, { wrapper: IntlTestWrapper });
    expect(screen.getByLabelText('Audio message')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Edit message:/ })).not.toBeInTheDocument();
  });

  it('keeps text/image editing available but prevents the text-only editor discarding audio', () => {
    const message = streamedMessage('user', true);
    const { rerender } = render(<UserMessage message={message} />, { wrapper: IntlTestWrapper });
    expect(screen.queryByRole('button', { name: /Edit message:/ })).not.toBeInTheDocument();
    rerender(
      <UserMessage
        message={{ ...message, content: message.content.filter((block) => block.type !== 'audio') }}
      />
    );
    expect(screen.getByRole('button', { name: /Edit message:/ })).toBeInTheDocument();
  });

  it('restores both roles in the session history detail view', () => {
    const messages = JSON.parse(
      JSON.stringify([streamedMessage('user'), streamedMessage('assistant', true)])
    );
    render(
      <SessionMessages messages={messages} isLoading={false} error={null} onRetry={vi.fn()} />,
      {
        wrapper: IntlTestWrapper,
      }
    );
    expect(screen.getAllByLabelText('Audio message')).toHaveLength(2);
    expect(screen.getByText('Speech transcript')).toBeInTheDocument();
    expect(screen.getByAltText('Lumina image')).toBeInTheDocument();
  });
});
