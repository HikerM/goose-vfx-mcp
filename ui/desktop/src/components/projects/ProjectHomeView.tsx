import { useCallback, useEffect, useState } from 'react';
import { FolderKanban, FolderOpen, GitBranch, Plus, RefreshCw } from 'lucide-react';
import { useNavigate } from 'react-router-dom';
import { toast } from 'react-toastify';
import { acpListProjects, acpOpenProject, type Project } from '../../acp/projects';
import { errorMessage } from '../../utils/conversionUtils';
import { Button } from '../ui/button';

function ProjectCard({ project, onOpen }: { project: Project; onOpen: () => void }) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group flex min-h-36 flex-col rounded-2xl border border-border-primary bg-background-primary p-5 text-left transition-colors hover:bg-background-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-border-primary"
    >
      <div className="flex w-full items-start justify-between gap-4">
        <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-background-tertiary">
          <FolderKanban className="h-5 w-5 text-text-primary" />
        </div>
        {project.profile.gitRepository && (
          <span className="inline-flex items-center gap-1 text-xs text-text-secondary">
            <GitBranch className="h-3.5 w-3.5" /> Git
          </span>
        )}
      </div>
      <div className="mt-5 w-full">
        <h2 className="truncate text-base font-semibold text-text-primary">{project.name}</h2>
        <p className="mt-1 truncate text-xs text-text-secondary" title={project.rootPath}>
          {project.rootPath}
        </p>
      </div>
      <div className="mt-auto flex flex-wrap gap-1.5 pt-4">
        {project.profile.detectedStacks.slice(0, 4).map((stack) => (
          <span
            key={stack}
            className="rounded-full bg-background-tertiary px-2 py-0.5 text-[11px] text-text-secondary"
          >
            {stack}
          </span>
        ))}
      </div>
    </button>
  );
}

export default function ProjectHomeView() {
  const navigate = useNavigate();
  const [projects, setProjects] = useState<Project[]>([]);
  const [loading, setLoading] = useState(true);
  const [opening, setOpening] = useState(false);

  const loadProjects = useCallback(async () => {
    setLoading(true);
    try {
      setProjects(await acpListProjects());
    } catch (error) {
      toast.error(`Unable to load projects: ${errorMessage(error)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);

  const chooseProject = async () => {
    if (opening) return;
    setOpening(true);
    try {
      const result = await window.electron.directoryChooser();
      const rootPath = result.filePaths[0];
      if (result.canceled || !rootPath) return;
      const project = await acpOpenProject(rootPath);
      navigate(`/projects/${encodeURIComponent(project.id)}`);
    } catch (error) {
      toast.error(`Unable to open project: ${errorMessage(error)}`);
    } finally {
      setOpening(false);
    }
  };

  return (
    <main className="h-full overflow-y-auto px-8 pb-10 pt-16">
      <div className="mx-auto w-full max-w-6xl">
        <header className="flex items-end justify-between gap-6 border-b border-border-secondary pb-6">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.18em] text-text-secondary">
              Development workspace
            </p>
            <h1 className="mt-2 text-3xl font-semibold tracking-tight text-text-primary">
              Projects
            </h1>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-text-secondary">
              Plan, execute, validate, and review repository-wide work with durable task history.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void loadProjects()}
              disabled={loading}
            >
              <RefreshCw className="mr-2 h-4 w-4" /> Refresh
            </Button>
            <Button size="sm" onClick={() => void chooseProject()} disabled={opening}>
              <Plus className="mr-2 h-4 w-4" /> {opening ? 'Opening…' : 'Open folder'}
            </Button>
          </div>
        </header>

        {loading ? (
          <div className="py-20 text-center text-sm text-text-secondary">Loading projects…</div>
        ) : projects.length === 0 ? (
          <section className="mt-12 flex min-h-80 flex-col items-center justify-center rounded-3xl border border-dashed border-border-primary bg-background-secondary px-6 text-center">
            <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-background-tertiary">
              <FolderOpen className="h-7 w-7 text-text-secondary" />
            </div>
            <h2 className="mt-5 text-lg font-semibold text-text-primary">Open a repository</h2>
            <p className="mt-2 max-w-md text-sm leading-6 text-text-secondary">
              Lumina will detect the stack and keep project tasks, runs, checkpoints, and review
              state across restarts.
            </p>
            <Button className="mt-6" onClick={() => void chooseProject()} disabled={opening}>
              <FolderOpen className="mr-2 h-4 w-4" /> Choose project folder
            </Button>
          </section>
        ) : (
          <section className="mt-8 grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {projects.map((project) => (
              <ProjectCard
                key={project.id}
                project={project}
                onOpen={() => navigate(`/projects/${encodeURIComponent(project.id)}`)}
              />
            ))}
          </section>
        )}
      </div>
    </main>
  );
}
