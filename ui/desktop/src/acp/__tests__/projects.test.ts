import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getAcpClient } from '../acpConnection';
import {
  acpCreateProjectWorkItem,
  acpOpenProject,
  acpProjectSnapshot,
  acpStartProjectRun,
} from '../projects';

vi.mock('../acpConnection', () => ({
  getAcpClient: vi.fn(),
}));

describe('ACP project workspace', () => {
  const extMethod = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(getAcpClient).mockResolvedValue({
      extMethod,
    } as unknown as Awaited<ReturnType<typeof getAcpClient>>);
  });

  it('opens a whole project directory without sending an empty optional name', async () => {
    const project = { id: 'project-1' };
    extMethod.mockResolvedValue({ project });

    await expect(acpOpenProject('D:\\work\\sample', '   ')).resolves.toBe(project);
    expect(extMethod).toHaveBeenCalledWith('_lumina/project/open', {
      rootPath: 'D:\\work\\sample',
    });
  });

  it('uses stable project methods for persistent work and runs', async () => {
    const workItem = { id: 'work-1' };
    const run = { id: 'run-1' };
    extMethod.mockResolvedValueOnce({ workItem }).mockResolvedValueOnce({ run });

    await expect(
      acpCreateProjectWorkItem({
        projectId: 'project-1',
        title: 'Implement feature',
        objective: 'Change the project safely',
        acceptanceCriteria: ['Tests pass'],
      })
    ).resolves.toBe(workItem);
    await expect(acpStartProjectRun('work-1', 'session-1')).resolves.toBe(run);

    expect(extMethod).toHaveBeenNthCalledWith(1, '_lumina/project/work-items/create', {
      projectId: 'project-1',
      title: 'Implement feature',
      objective: 'Change the project safely',
      acceptanceCriteria: ['Tests pass'],
    });
    expect(extMethod).toHaveBeenNthCalledWith(2, '_lumina/project/runs/start', {
      workItemId: 'work-1',
      sessionId: 'session-1',
    });
  });

  it('returns a snapshot with execution history and resumable checkpoints', async () => {
    const snapshot = {
      project: { id: 'project-1' },
      workItems: [],
      runs: [],
      runSteps: [],
      checkpoints: [],
      interruptedRunCount: 0,
    };
    extMethod.mockResolvedValue({ snapshot });

    await expect(acpProjectSnapshot('project-1')).resolves.toBe(snapshot);
    expect(extMethod).toHaveBeenCalledWith('_lumina/project/snapshot', {
      projectId: 'project-1',
    });
  });
});
