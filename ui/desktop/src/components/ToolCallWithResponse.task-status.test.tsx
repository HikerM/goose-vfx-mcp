import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import ToolCallWithResponse from './ToolCallWithResponse';
import { IntlTestWrapper } from '../i18n/test-utils';
import { createAcpSessionNotificationAdapter } from '../acp/sessionNotificationAdapter';
import { getToolRequests, getToolResponses } from '../types/message';

vi.mock('./McpApps/McpAppRenderer', () => ({ default: () => null }));
vi.mock('./ToolApprovalButtons', () => ({ default: () => null }));
vi.mock('./MarkdownContent', () => ({
  default: ({ content }: { content: string }) => <p>{content}</p>,
}));

function renderNativeTaskResult(text: string) {
  const adapter = createAcpSessionNotificationAdapter();
  adapter.apply({
    sessionId: 'native-task-session',
    update: {
      sessionUpdate: 'tool_call',
      toolCallId: 'native-task-call',
      title: 'native task',
      status: 'in_progress',
      rawInput: {},
    },
  });
  adapter.apply({
    sessionId: 'native-task-session',
    update: {
      sessionUpdate: 'tool_call_update',
      toolCallId: 'native-task-call',
      status: 'completed',
      content: [{ type: 'content', content: { type: 'text', text } }],
    },
  });
  const messages = adapter.getMessages();
  render(
    <ToolCallWithResponse
      toolRequest={messages.flatMap(getToolRequests)[0]}
      toolResponse={messages.flatMap(getToolResponses)[0]}
      isCancelledMessage={false}
      isPendingApproval={false}
    />,
    { wrapper: IntlTestWrapper }
  );
}

describe('native task status display', () => {
  it('shows the durable job state returned by a native task', () => {
    renderNativeTaskResult('{"status":"completed","jobId":"native:session:job"}');

    expect(
      screen.getByRole('status', { name: 'Task status: completed (native:session:job)' })
    ).toBeInTheDocument();
  });

  it('does not treat arbitrary JSON with a job-looking field as a native status', () => {
    renderNativeTaskResult('{"status":"completed","jobId":"other:session:job"}');

    expect(screen.queryByRole('status', { name: /Task status:/ })).not.toBeInTheDocument();
  });
});
