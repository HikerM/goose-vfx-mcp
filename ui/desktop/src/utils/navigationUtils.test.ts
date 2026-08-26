import { describe, expect, it, vi } from 'vitest';
import {
  createNavigationHandler,
  isProjectWorkspacePath,
  projectTaskPath,
} from './navigationUtils';

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

describe('project task navigation', () => {
  it('recognizes only a concrete project workspace path', () => {
    expect(isProjectWorkspacePath('/projects/project-1')).toBe(true);
    expect(isProjectWorkspacePath('/projects/project-1/')).toBe(true);
    expect(isProjectWorkspacePath('/projects')).toBe(false);
    expect(isProjectWorkspacePath('/projects/project-1/settings')).toBe(false);
  });

  it('keeps the project and selected task in the URL', () => {
    expect(projectTaskPath('project/1', 'session?1')).toBe(
      '/projects/project%2F1?resumeSessionId=session%3F1'
    );
    expect(projectTaskPath('project-1')).toBe('/projects/project-1');
  });
});
