import { getAcpClient } from './acpConnection';

export type WorkItemStatus =
  | 'draft'
  | 'planned'
  | 'running'
  | 'validating'
  | 'review'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'interrupted'
  | 'blocked';

export type ProjectRunStatus =
  | 'pending'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'interrupted';

export type ProjectRunPhase =
  | 'discovering'
  | 'planning'
  | 'awaiting_approval'
  | 'executing'
  | 'validating'
  | 'reviewing'
  | 'complete';

export interface ProjectProfile {
  manifests: string[];
  detectedStacks: string[];
  gitRepository: boolean;
}

export interface Project {
  id: string;
  name: string;
  rootPath: string;
  canonicalRoot: string;
  profile: ProjectProfile;
  createdAtMs: number;
  updatedAtMs: number;
  lastOpenedAtMs: number;
  archivedAtMs?: number | null;
}

export interface ProjectWorkItem {
  id: string;
  projectId: string;
  title: string;
  objective: string;
  acceptanceCriteria: string[];
  status: WorkItemStatus;
  createdAtMs: number;
  updatedAtMs: number;
  completedAtMs?: number | null;
}

export interface ProjectRun {
  id: string;
  workItemId: string;
  sessionId?: string | null;
  status: ProjectRunStatus;
  phase: ProjectRunPhase;
  startedAtMs?: number | null;
  updatedAtMs: number;
  finishedAtMs?: number | null;
  errorCode?: string | null;
  errorMessage?: string | null;
}

export interface ProjectRunStep {
  id: string;
  runId: string;
  sequence: number;
  kind: string;
  title: string;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
  detail: unknown;
  startedAtMs?: number | null;
  finishedAtMs?: number | null;
}

export interface ProjectSnapshot {
  project: Project;
  workItems: ProjectWorkItem[];
  runs: ProjectRun[];
  runSteps: ProjectRunStep[];
  checkpoints: ProjectCheckpoint[];
  interruptedRunCount: number;
}

export interface ProjectCheckpoint {
  id: string;
  runId: string;
  sequence: number;
  phase: ProjectRunPhase;
  state: unknown;
  safeToResume: boolean;
  createdAtMs: number;
}

export interface ProjectChangeSet {
  id: string;
  runId: string;
  baseRevision?: string | null;
  status: 'open' | 'conflict' | 'ready_for_review' | 'accepted' | 'reverted';
  summary: string;
  createdAtMs: number;
  updatedAtMs: number;
}

async function requestProject<T>(method: string, params: Record<string, unknown>): Promise<T> {
  const client = await getAcpClient();
  return (await client.extMethod(method, params)) as T;
}

export async function acpOpenProject(rootPath: string, name?: string): Promise<Project> {
  const response = await requestProject<{ project: Project }>('_lumina/project/open', {
    rootPath,
    ...(name?.trim() ? { name: name.trim() } : {}),
  });
  return response.project;
}

export async function acpListProjects(includeArchived = false): Promise<Project[]> {
  const response = await requestProject<{ projects: Project[] }>('_lumina/project/list', {
    includeArchived,
  });
  return response.projects;
}

export async function acpProjectSnapshot(projectId: string): Promise<ProjectSnapshot> {
  const response = await requestProject<{ snapshot: ProjectSnapshot }>('_lumina/project/snapshot', {
    projectId,
  });
  return response.snapshot;
}

export async function acpCreateProjectWorkItem(input: {
  projectId: string;
  title: string;
  objective: string;
  acceptanceCriteria: string[];
}): Promise<ProjectWorkItem> {
  const response = await requestProject<{ workItem: ProjectWorkItem }>(
    '_lumina/project/work-items/create',
    input
  );
  return response.workItem;
}

export async function acpCompleteProjectWorkItem(workItemId: string): Promise<ProjectWorkItem> {
  const response = await requestProject<{ workItem: ProjectWorkItem }>(
    '_lumina/project/work-items/complete',
    { workItemId }
  );
  return response.workItem;
}

export async function acpStartProjectRun(
  workItemId: string,
  sessionId?: string
): Promise<ProjectRun> {
  const response = await requestProject<{ run: ProjectRun }>('_lumina/project/runs/start', {
    workItemId,
    ...(sessionId ? { sessionId } : {}),
  });
  return response.run;
}

export async function acpUpdateProjectRunPhase(
  runId: string,
  phase: ProjectRunPhase
): Promise<ProjectRun> {
  const response = await requestProject<{ run: ProjectRun }>('_lumina/project/runs/phase', {
    runId,
    phase,
  });
  return response.run;
}

export async function acpFinishProjectRun(input: {
  runId: string;
  status: Exclude<ProjectRunStatus, 'pending' | 'running'>;
  errorCode?: string;
  errorMessage?: string;
}): Promise<ProjectRun> {
  const response = await requestProject<{ run: ProjectRun }>('_lumina/project/runs/finish', input);
  return response.run;
}

export async function acpCreateProjectCheckpoint(input: {
  runId: string;
  phase: ProjectRunPhase;
  state: unknown;
  safeToResume: boolean;
}): Promise<ProjectCheckpoint> {
  const response = await requestProject<{ checkpoint: ProjectCheckpoint }>(
    '_lumina/project/checkpoints/create',
    input
  );
  return response.checkpoint;
}

export async function acpGetProjectChangeSet(
  runId: string,
  baseRevision?: string
): Promise<ProjectChangeSet> {
  const response = await requestProject<{ changeSet: ProjectChangeSet }>(
    '_lumina/project/change-sets/get',
    { runId, ...(baseRevision ? { baseRevision } : {}) }
  );
  return response.changeSet;
}
