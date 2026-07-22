import { describe, expect, it, vi } from 'vitest';
import { createNavigationHandler } from './navigationUtils';

describe('MCP Center navigation', () => {
  it('keeps the legacy Extensions route and the MCP Center route distinct', () => {
    const navigate = vi.fn();
    const handleNavigation = createNavigationHandler(navigate);

    handleNavigation('extensions');
    handleNavigation('mcpCenter');

    expect(navigate).toHaveBeenNthCalledWith(1, '/extensions', { state: undefined });
    expect(navigate).toHaveBeenNthCalledWith(2, '/mcp-center', { state: undefined });
  });
});
