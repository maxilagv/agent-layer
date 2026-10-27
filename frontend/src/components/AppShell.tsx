/**
 * Application shell: skip link, left navigation and the active view.
 *
 * The navigation is a real list of buttons; ArrowUp / ArrowDown move the
 * focus between the entries and Enter activates them.
 */
import { useState } from 'react';
import { useApp } from '../state/AppContext';
import { handleListKeyDown, LIST_ITEM_ATTRIBUTE } from './Keyboard';
import { LoginView } from '../views/LoginView';
import { ProjectsView } from '../views/ProjectsView';
import { ConversationsView } from '../views/ConversationsView';
import { TasksView } from '../views/TasksView';
import { ApprovalsView } from '../views/ApprovalsView';
import { ConnectionsView } from '../views/ConnectionsView';
import { UsageView } from '../views/UsageView';
import { SettingsView } from '../views/SettingsView';

const VIEWS = [
  { id: 'projects', label: 'Projects', render: () => <ProjectsView /> },
  { id: 'conversations', label: 'Conversations', render: () => <ConversationsView /> },
  { id: 'tasks', label: 'Tasks', render: () => <TasksView /> },
  { id: 'approvals', label: 'Approvals', render: () => <ApprovalsView /> },
  { id: 'connections', label: 'Connections', render: () => <ConnectionsView /> },
  { id: 'usage', label: 'Usage', render: () => <UsageView /> },
  { id: 'settings', label: 'Settings', render: () => <SettingsView /> },
] as const;

type ViewId = (typeof VIEWS)[number]['id'];

export function AppShell() {
  const { bootstrap, selectedProject, needsAccessCode, signIn, bootstrapError } = useApp();
  const [active, setActive] = useState<ViewId>('projects');
  const current = VIEWS.find((view) => view.id === active) ?? VIEWS[0];

  if (needsAccessCode) {
    return (
      <div className="shell">
        <a className="skip-link" href="#main-content">
          Skip to main content
        </a>
        <LoginView onSubmit={signIn} error={bootstrapError} />
      </div>
    );
  }

  return (
    <div className="shell">
      <a className="skip-link" href="#main-content">
        Skip to main content
      </a>

      <header className="shell__header">
        <span className="shell__brand">LayerAgent</span>
        <span className="shell__context">
          {bootstrap ? `${bootstrap.principal} · ${bootstrap.tenant}` : 'connecting…'}
          {selectedProject ? ` · ${selectedProject.name}` : ''}
        </span>
      </header>

      <div className="shell__body">
        <nav className="shell__nav" aria-label="Sections">
          <ul className="nav" onKeyDown={handleListKeyDown}>
            {VIEWS.map((view) => (
              <li key={view.id}>
                <button
                  type="button"
                  {...{ [LIST_ITEM_ATTRIBUTE]: 'true' }}
                  className={`nav__item${view.id === active ? ' nav__item--active' : ''}`}
                  aria-current={view.id === active ? 'page' : undefined}
                  onClick={() => setActive(view.id)}
                >
                  {view.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <main className="shell__main" id="main-content" tabIndex={-1}>
          {current.render()}
        </main>
      </div>
    </div>
  );
}
