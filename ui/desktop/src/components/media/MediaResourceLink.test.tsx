import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { ContentBlock } from '../../types/message';
import { IntlTestWrapper } from '../../i18n/test-utils';
import MediaResourceLink, { mediaResourceIdentity } from './MediaResourceLink';

vi.mock('./ConnectedMediaResultCard', () => ({
  default: ({
    sessionId,
    resultId,
    isActive,
  }: {
    sessionId: string;
    resultId: string;
    isActive: boolean;
  }) => (
    <div
      data-testid="connected-media"
      data-session={sessionId}
      data-result={resultId}
      data-active={isActive}
    />
  ),
}));

const resultId = '12345678-1234-8234-8234-123456789abc';
const resource: Extract<ContentBlock, { type: 'resource_link' }> = {
  type: 'resource_link',
  uri: `lumina-media://result/${resultId}`,
  name: 'output.wav',
  _meta: { 'lumina.mediaResult': { schemaVersion: 1, sessionId: 'session', resultId } },
};

describe('media resource binding', () => {
  it('passes only the matching session/result identity to the connected card', () => {
    render(<MediaResourceLink resource={resource} sessionId="session" isActive={false} />, {
      wrapper: IntlTestWrapper,
    });
    expect(screen.getByTestId('connected-media')).toHaveAttribute('data-result', resultId);
    expect(screen.getByTestId('connected-media')).toHaveAttribute('data-session', 'session');
    expect(screen.getByTestId('connected-media')).toHaveAttribute('data-active', 'false');
    expect(screen.queryByText(resource.uri)).not.toBeInTheDocument();
  });

  it.each([
    `${resource.uri}?path=D:/private/file`,
    `${resource.uri}#fragment`,
    `${resource.uri}/extra`,
    `lumina-media://result/${resultId.toUpperCase()}`,
    'file:///D:/private/file.wav',
    'https://example.com/output.wav',
    'lumina-media://result/../private',
  ])('treats a noncanonical or unregistered link as descriptive text: %s', (uri) => {
    expect(mediaResourceIdentity({ ...resource, uri }, 'session')).toBeNull();
  });

  it.each([undefined, 'other-session'])(
    'does not activate a card in a different or missing session',
    (sessionId) => {
      render(<MediaResourceLink resource={resource} sessionId={sessionId} />, {
        wrapper: IntlTestWrapper,
      });
      expect(screen.queryByTestId('connected-media')).not.toBeInTheDocument();
      expect(screen.getByText(resource.uri)).toBeInTheDocument();
      expect(screen.queryByRole('link')).not.toBeInTheDocument();
    }
  );

  it.each([
    undefined,
    null,
    [],
    { schemaVersion: 2, sessionId: 'session', resultId },
    { schemaVersion: 1, sessionId: 'session', resultId: 'other' },
  ])('rejects missing or inconsistent reference metadata', (value) => {
    expect(
      mediaResourceIdentity({ ...resource, _meta: { 'lumina.mediaResult': value } }, 'session')
    ).toBeNull();
  });
});
