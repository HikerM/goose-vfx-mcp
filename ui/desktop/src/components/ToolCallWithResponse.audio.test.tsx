import type { ContentBlock } from '@agentclientprotocol/sdk';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import ToolCallWithResponse from './ToolCallWithResponse';
import { IntlTestWrapper } from '../i18n/test-utils';
import { createAcpSessionNotificationAdapter } from '../acp/sessionNotificationAdapter';
import { getToolRequests, getToolResponses } from '../types/message';

vi.mock('./McpApps/McpAppRenderer', () => ({ default: () => null }));
vi.mock('./ToolApprovalButtons', () => ({ default: () => null }));
vi.mock('./MarkdownContent', () => ({
  default: ({ content }: { content: string }) => <p>{content}</p>,
}));

const createObjectURL = vi.fn();
const revokeObjectURL = vi.fn();

beforeEach(() => {
  createObjectURL.mockReset().mockReturnValue('blob:tool-audio');
  revokeObjectURL.mockReset();
  vi.stubGlobal(
    'URL',
    class extends URL {
      static createObjectURL = createObjectURL;
      static revokeObjectURL = revokeObjectURL;
    }
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderResult(content: ContentBlock[]) {
  const adapter = createAcpSessionNotificationAdapter();
  adapter.apply({
    sessionId: 'tool-audio',
    update: {
      sessionUpdate: 'tool_call',
      toolCallId: 'speak-1',
      title: 'speak',
      status: 'in_progress',
      rawInput: {},
    },
  });
  adapter.apply({
    sessionId: 'tool-audio',
    update: {
      sessionUpdate: 'tool_call_update',
      toolCallId: 'speak-1',
      status: 'completed',
      content: content.map((block) => ({ type: 'content', content: block })),
    },
  });
  const messages = adapter.getMessages();
  const request = messages.flatMap(getToolRequests)[0];
  const response = messages.flatMap(getToolResponses)[0];
  const rendered = render(
    <ToolCallWithResponse
      toolRequest={request}
      toolResponse={response}
      isCancelledMessage={false}
      isPendingApproval={false}
    />,
    { wrapper: IntlTestWrapper }
  );
  fireEvent.click(screen.getByRole('button', { name: 'Tool status: success Speak' }));
  return rendered;
}

describe('ACP tool-result audio', () => {
  it('renders converted audio when Output opens and releases it when Output collapses', () => {
    renderResult([{ type: 'audio', data: 'AQID', mimeType: 'audio/wav' }]);
    expect(screen.queryByLabelText('Audio message')).not.toBeInTheDocument();
    expect(createObjectURL).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Output' }));
    expect(screen.getByLabelText('Audio message')).toHaveAttribute('src', 'blob:tool-audio');
    expect(screen.getByLabelText('Audio message')).toHaveAttribute('controls');
    fireEvent.click(screen.getByRole('button', { name: 'Output' }));
    expect(screen.queryByLabelText('Audio message')).not.toBeInTheDocument();
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:tool-audio');
  });

  it('reports malformed tool audio data instead of an empty output', () => {
    renderResult([{ type: 'audio', data: 'not base64!', mimeType: 'audio/wav' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Output' }));
    expect(screen.getByRole('alert')).toHaveTextContent('empty or invalid');
    expect(createObjectURL).not.toHaveBeenCalled();
  });

  it('does not render an audio block claiming image MIME as an image', () => {
    renderResult([{ type: 'audio', data: 'AQID', mimeType: 'image/png' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Output' }));
    expect(screen.getByRole('alert')).toHaveTextContent('format is not supported');
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('retains text, image and embedded resource outputs alongside audio', () => {
    renderResult([
      { type: 'audio', data: 'AQID', mimeType: 'audio/wav' },
      { type: 'text', text: 'Tool transcript' },
      { type: 'image', data: 'image-data', mimeType: 'image/png' },
      {
        type: 'resource',
        resource: {
          uri: 'resource://transcript',
          text: 'Resource transcript',
          mimeType: 'text/plain',
        },
      },
    ]);
    for (const button of screen.getAllByRole('button', { name: 'Output' })) fireEvent.click(button);
    expect(screen.getByLabelText('Audio message')).toBeInTheDocument();
    expect(screen.getByText('Tool transcript')).toBeInTheDocument();
    expect(screen.getByAltText('Tool result')).toHaveAttribute(
      'src',
      'data:image/png;base64,image-data'
    );
    expect(screen.getByText(/resource:\/\/transcript/)).toHaveTextContent('Resource transcript');
  });
});
