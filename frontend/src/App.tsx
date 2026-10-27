/**
 * Entry point of the shared LayerAgent frontend.
 */
import { AppProvider } from './state/AppContext';
import { AppShell } from './components/AppShell';
import type { ApiClient } from './api/client';

export function App({ client }: { client?: ApiClient } = {}) {
  return (
    <AppProvider client={client}>
      <AppShell />
    </AppProvider>
  );
}

export default App;
