/**
 * Application wide context: bootstrap information, the project catalogue and
 * the currently selected project.
 *
 * The selected project defines the "scope" of every data view; changing it
 * resets the conversations and the tasks view (their subscriptions are
 * disposed and their data cleared before the new scope is loaded).
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { getClient } from '../api';
import type { ApiClient, Bootstrap } from '../api/client';
import { errorMessage, isUnauthenticated } from '../api/client';
import type { Project } from '../api/types';

export interface AppContextValue {
  client: ApiClient;
  bootstrap: Bootstrap | null;
  bootstrapError: string | null;
  /** True while the web surface still requires the local access code. */
  needsAccessCode: boolean;
  /** Redeems the single-use access code and reloads the bootstrap. */
  signIn: (code: string) => Promise<void>;
  signOut: () => Promise<void>;
  projects: Project[];
  projectsError: string | null;
  capabilities: Record<string, boolean>;
  selectedProjectId: string | null;
  selectedProject: Project | null;
  /** Changes whenever the selected project changes; views use it as scope key. */
  scopeKey: string;
  selectProject: (id: string | null) => void;
  reloadBootstrap: () => void;
  reloadProjects: () => void;
}

const AppContext = createContext<AppContextValue | null>(null);

export function AppProvider({
  children,
  client: injectedClient,
}: {
  children: ReactNode;
  client?: ApiClient;
}) {
  const client = useMemo(() => injectedClient ?? getClient(), [injectedClient]);
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null);
  const [bootstrapError, setBootstrapError] = useState<string | null>(null);
  const [needsAccessCode, setNeedsAccessCode] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [bootstrapToken, setBootstrapToken] = useState(0);
  const [projectsToken, setProjectsToken] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setBootstrapError(null);
    client
      .bootstrap()
      .then((value) => {
        if (cancelled) return;
        setBootstrap(value);
        setNeedsAccessCode(false);
        setProjects(value.projects ?? []);
        setSelectedProjectId((current) => current ?? value.projects?.[0]?.id ?? null);
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        if (isUnauthenticated(cause)) {
          // The web surface asks for the single-use code first.
          setNeedsAccessCode(true);
          setBootstrapError(null);
          return;
        }
        setBootstrapError(errorMessage(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [client, bootstrapToken]);

  useEffect(() => {
    let cancelled = false;
    setProjectsError(null);
    client
      .projects()
      .then((value) => {
        if (cancelled) return;
        setProjects(value);
        setSelectedProjectId((current) => {
          if (current && value.some((project) => project.id === current)) return current;
          // An empty catalogue must not clear a selection that the bootstrap
          // already established.
          if (value.length === 0) return current;
          return value[0]?.id ?? null;
        });
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setProjectsError(errorMessage(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [client, projectsToken]);

  const selectProject = useCallback((id: string | null) => {
    setSelectedProjectId(id);
  }, []);

  const reloadBootstrap = useCallback(() => setBootstrapToken((value) => value + 1), []);
  const reloadProjects = useCallback(() => setProjectsToken((value) => value + 1), []);

  const signIn = useCallback(
    async (code: string) => {
      await client.login(code);
      setBootstrapToken((value) => value + 1);
      // The catalogue fetch runs before the code is redeemed and fails with
      // 401. Retry it now that the session exists, or the sign-in view keeps a
      // stale "Unauthorized" banner.
      setProjectsToken((value) => value + 1);
    },
    [client],
  );

  const signOut = useCallback(async () => {
    await client.logout();
    setBootstrap(null);
    setProjects([]);
    setSelectedProjectId(null);
    setNeedsAccessCode(true);
  }, [client]);

  const selectedProject = useMemo(
    () => projects.find((project) => project.id === selectedProjectId) ?? null,
    [projects, selectedProjectId],
  );

  const value = useMemo<AppContextValue>(
    () => ({
      client,
      bootstrap,
      bootstrapError,
      needsAccessCode,
      signIn,
      signOut,
      projects,
      projectsError,
      capabilities: bootstrap?.capabilities ?? {},
      selectedProjectId,
      selectedProject,
      scopeKey: selectedProjectId ?? '(no-project)',
      selectProject,
      reloadBootstrap,
      reloadProjects,
    }),
    [
      client,
      bootstrap,
      bootstrapError,
      needsAccessCode,
      signIn,
      signOut,
      projects,
      projectsError,
      selectedProjectId,
      selectedProject,
      selectProject,
      reloadBootstrap,
      reloadProjects,
    ],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppContextValue {
  const value = useContext(AppContext);
  if (!value) throw new Error('useApp must be used inside <AppProvider>');
  return value;
}
