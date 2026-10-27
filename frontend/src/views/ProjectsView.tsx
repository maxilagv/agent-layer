/**
 * Projects view: the catalogue of known workspaces.
 *
 * Selecting a project changes the scope of the conversations and the tasks
 * view; both reset their data and their subscriptions.
 */
import { useApp } from '../state/AppContext';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from '../components/Keyboard';
import { EmptyState, ErrorState, LoadingState } from '../components/States';

export function ProjectsView() {
  const {
    projects,
    projectsError,
    reloadProjects,
    selectedProjectId,
    selectProject,
    bootstrap,
    bootstrapError,
    reloadBootstrap,
  } = useApp();

  if (bootstrapError) {
    return (
      <ErrorState message={`Unable to load the operator session: ${bootstrapError}`} onRetry={reloadBootstrap} />
    );
  }

  return (
    <section className="view" aria-labelledby="projects-heading">
      <header className="view__header">
        <h2 id="projects-heading">Projects</h2>
        <p className="muted">
          {bootstrap
            ? `Tenant ${bootstrap.tenant} · principal ${bootstrap.principal}`
            : 'Loading session…'}
        </p>
      </header>

      {projectsError ? (
        <ErrorState message={`Unable to load projects: ${projectsError}`} onRetry={reloadProjects} />
      ) : null}

      {projects.length === 0 && !projectsError ? (
        <EmptyState message="No projects are registered yet." />
      ) : null}

      <ul className="list list--projects" onKeyDown={handleListKeyDown} aria-label="Projects">
        {projects.map((project) => {
          const selected = project.id === selectedProjectId;
          return (
            <li key={project.id}>
              <button
                type="button"
                {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                className={`list__row${selected ? ' list__row--selected' : ''}`}
                aria-pressed={selected}
                onClick={() => selectProject(project.id)}
              >
                <span className="list__title">{project.name}</span>
                <span className="list__meta">{project.path}</span>
                <span className="list__state">{selected ? 'selected' : 'select'}</span>
              </button>
            </li>
          );
        })}
      </ul>

      {!bootstrap && !bootstrapError ? <LoadingState label="Loading session…" /> : null}
    </section>
  );
}
