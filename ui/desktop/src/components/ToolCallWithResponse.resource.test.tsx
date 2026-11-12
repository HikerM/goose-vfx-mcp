import type { ContentBlock } from '@agentclientprotocol/sdk';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import ToolCallWithResponse from './ToolCallWithResponse';
import { IntlTestWrapper } from '../i18n/test-utils';
import { createAcpSessionNotificationAdapter } from '../acp/sessionNotificationAdapter';
import { getToolRequests, getToolResponses } from '../types/message';

vi.mock('./McpApps/McpAppRenderer', () => ({ default: () => null }));
vi.mock('./ToolApprovalButtons', () => ({ default: () => null }));
vi.mock('./MarkdownContent', () => ({
  default: ({ content }: { content: string }) => <p>{content}</p>,
}));

beforeEach(() => {
  vi.stubGlobal(
    'URL',
    class extends URL {
      static createObjectURL = vi.fn(() => 'blob:resource-test-audio');
      static revokeObjectURL = vi.fn();
    }
  );
});
afterEach(() => vi.unstubAllGlobals());

const resource: ContentBlock = {
  type: 'resource_link',
  name: 'result.wav',
  title: 'Generated speech',
  uri: 'lumina-media://results/result-1',
  description: 'A spoken response.\nKeep the original file.',
  mimeType: 'audio/wav',
  size: 165006,
  _meta: { resultId: 'result-1' },
};

function renderResults(content: ContentBlock[], restore = false) {
  let adapter = createAcpSessionNotificationAdapter();
  adapter.apply({
    sessionId: 'resource-session',
    update: {
      sessionUpdate: 'tool_call',
      toolCallId: 'result-1',
      title: 'result',
      status: 'in_progress',
      rawInput: {},
    },
  });
  adapter.apply({
    sessionId: 'resource-session',
    update: {
      sessionUpdate: 'tool_call_update',
      toolCallId: 'result-1',
      status: 'completed',
      content: content.map((block) => ({ type: 'content', content: block })),
    },
  });
  if (restore)
    adapter = createAcpSessionNotificationAdapter(
      JSON.parse(JSON.stringify(adapter.getMessages()))
    );
  const messages = adapter.getMessages();
  const response = messages.flatMap(getToolResponses)[0];
  render(
    <ToolCallWithResponse
      toolRequest={messages.flatMap(getToolRequests)[0]}
      toolResponse={response}
      isCancelledMessage={false}
      isPendingApproval={false}
    />,
    { wrapper: IntlTestWrapper }
  );
  fireEvent.click(screen.getByRole('button', { name: 'Tool status: success Result' }));
  return { response, outputs: screen.getAllByRole('button', { name: 'Output' }) };
}

describe('ACP tool resource links', () => {
  it.each([false, true])(
    'shows actual fields after ACP conversion, history restore=%s',
    (restore) => {
      const { response, outputs } = renderResults([resource], restore);
      expect(response.toolResult).toMatchObject({ value: { content: [resource] } });
      expect(screen.queryByText('result.wav')).not.toBeInTheDocument();
      fireEvent.click(outputs[0]);
      const view = within(screen.getByRole('group', { name: 'result.wav' }));
      expect(view.getByText('result.wav')).toBeInTheDocument();
      expect(view.getByText('Generated speech')).toBeInTheDocument();
      expect(view.getByText(/A spoken response/)).toHaveTextContent('Keep the original file.');
      expect(view.getByText('audio/wav')).toBeInTheDocument();
      expect(view.getByText('165,006 B')).toBeInTheDocument();
      expect(view.getByText(resource.uri)).toBeInTheDocument();
      expect(view.queryByRole('link')).not.toBeInTheDocument();
      expect(view.queryByRole('button')).not.toBeInTheDocument();
      fireEvent.click(outputs[0]);
      expect(screen.queryByRole('group', { name: 'result.wav' })).not.toBeInTheDocument();
    }
  );

  it('renders minimal references without invented format, size or duplicate title', () => {
    const { outputs } = renderResults([
      { type: 'resource_link', name: 'resource', title: 'resource', uri: 'resource://item' },
    ]);
    fireEvent.click(outputs[0]);
    const view = within(screen.getByRole('group', { name: 'resource' }));
    expect(view.getAllByText('resource')).toHaveLength(1);
    expect(view.getByText('resource://item')).toBeInTheDocument();
    expect(view.queryByText(/\d.* B$/)).not.toBeInTheDocument();
  });

  it.each([undefined, -1, 1.5, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1])(
    'omits unavailable or nonrepresentable byte counts (%s)',
    (size) => {
      const { outputs } = renderResults([
        {
          type: 'resource_link',
          name: 'unknown-size',
          uri: 'resource://item',
          size,
          mimeType: '   ',
          description: '  ',
        },
      ]);
      fireEvent.click(outputs[0]);
      const view = screen.getByRole('group', { name: 'unknown-size' });
      expect(view).not.toHaveTextContent(/(?:undefined|Infinity|NaN| B)/);
      expect(view.textContent).toBe('unknown-sizeresource://item');
    }
  );

  it('preserves a real zero-byte size', () => {
    const { outputs } = renderResults([
      { type: 'resource_link', name: 'empty', uri: 'resource://empty', size: 0 },
    ]);
    fireEvent.click(outputs[0]);
    expect(screen.getByText('0 B')).toBeInTheDocument();
  });

  it.each([
    'file:///D:/private/output.wav',
    'javascript:alert(1)',
    'https://example.com/result.wav',
  ])('shows URI as text without turning it into a local path or active link: %s', (uri) => {
    const { outputs } = renderResults([
      {
        type: 'resource_link',
        name: '<img src=x onerror=alert(1)>',
        description: '[download](https://example.com)',
        uri,
      },
    ]);
    fireEvent.click(outputs[0]);
    const view = screen.getByRole('group', { name: '<img src=x onerror=alert(1)>' });
    expect(view).toHaveTextContent(uri);
    expect(view).toHaveTextContent('[download](https://example.com)');
    expect(view.querySelector('a, img, audio, video, button')).toBeNull();
  });

  it('preserves mixed text, link, audio, image and embedded resource order', () => {
    const { outputs } = renderResults([
      { type: 'text', text: 'First text' },
      resource,
      { type: 'audio', data: 'AQID', mimeType: 'audio/wav' },
      { type: 'image', data: 'image-data', mimeType: 'image/png' },
      {
        type: 'resource',
        resource: { uri: 'resource://embedded', text: 'Embedded text', mimeType: 'text/plain' },
      },
    ]);
    expect(outputs).toHaveLength(5);
    for (const button of outputs) fireEvent.click(button);
    const views = outputs.map((button) => within(button.parentElement!));
    expect(views[0].getByText('First text')).toBeInTheDocument();
    expect(views[1].getByRole('group', { name: 'result.wav' })).toBeInTheDocument();
    expect(views[2].getByLabelText('Audio message')).toHaveAttribute(
      'src',
      'blob:resource-test-audio'
    );
    expect(views[3].getByAltText('Tool result')).toHaveAttribute(
      'src',
      'data:image/png;base64,image-data'
    );
    expect(views[4].getByText(/resource:\/\/embedded/)).toHaveTextContent('Embedded text');
  });
});
