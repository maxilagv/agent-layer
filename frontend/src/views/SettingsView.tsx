/**
 * Settings view: the effective configuration, the available models and the
 * MCP capabilities of the host.
 */
import { useApp } from '../state/AppContext';
import { useSnapshot } from '../state/useSnapshot';
import { EmptyState, ErrorState, LoadingState } from '../components/States';
import { prettyJson } from '../utils/format';

export function SettingsView() {
  const { client, bootstrap, capabilities } = useApp();
  const config = useSnapshot(() => client.config(), []);
  const models = useSnapshot(() => client.models(), []);
  const mcp = useSnapshot(() => client.mcpCapabilities(), []);

  return (
    <section className="view" aria-labelledby="settings-heading">
      <header className="view__header">
        <h2 id="settings-heading">Settings</h2>
        <p className="muted">Read-only view of the effective configuration.</p>
      </header>

      <section className="panel" aria-label="Session">
        <h3>Session</h3>
        <dl className="details">
          <div className="details__row">
            <dt>Tenant</dt>
            <dd>{bootstrap?.tenant ?? '—'}</dd>
          </div>
          <div className="details__row">
            <dt>Principal</dt>
            <dd>{bootstrap?.principal ?? '—'}</dd>
          </div>
          <div className="details__row">
            <dt>Capabilities</dt>
            <dd>
              {Object.keys(capabilities).length === 0 ? (
                <span className="muted">none reported</span>
              ) : (
                <ul className="capabilities">
                  {Object.entries(capabilities).map(([name, enabled]) => (
                    <li key={name}>
                      {name}: {enabled ? 'yes' : 'no'}
                    </li>
                  ))}
                </ul>
              )}
            </dd>
          </div>
        </dl>
      </section>

      <section className="panel" aria-label="Configuration">
        <h3>Configuration</h3>
        {config.loading ? <LoadingState label="Loading configuration…" /> : null}
        {config.error ? (
          <ErrorState message={`Unable to load the configuration: ${config.error}`} onRetry={config.reload} />
        ) : null}
        {config.data ? (
          <>
            <p>
              Revision: <code data-testid="config-revision">{config.data.revision}</code>
            </p>
            <pre className="details__pre" data-testid="config-body" aria-label="Configuration">
              {prettyJson(config.data.config)}
            </pre>
          </>
        ) : null}
      </section>

      <section className="panel" aria-label="Models">
        <h3>Models</h3>
        {models.loading ? <LoadingState label="Loading models…" /> : null}
        {models.error ? (
          <ErrorState message={`Unable to load the models: ${models.error}`} onRetry={models.reload} />
        ) : null}
        {models.data && models.data.length === 0 ? (
          <EmptyState message="No model is configured." />
        ) : null}
        {models.data && models.data.length > 0 ? (
          <ul className="list list--plain" aria-label="Model list">
            {models.data.map((model, index) => (
              <li key={index}>
                <code>{typeof model === 'string' ? model : prettyJson(model)}</code>
              </li>
            ))}
          </ul>
        ) : null}
      </section>

      <section className="panel" aria-label="MCP capabilities">
        <h3>MCP capabilities</h3>
        {mcp.loading ? <LoadingState label="Loading MCP capabilities…" /> : null}
        {mcp.error ? (
          <ErrorState message={`Unable to load the MCP capabilities: ${mcp.error}`} onRetry={mcp.reload} />
        ) : null}
        {mcp.data ? (
          <ul className="capabilities">
            <li>
              resources: {mcp.data.resources ? 'disponible' : 'recursos no disponibles'}
            </li>
            <li>tools: {mcp.data.tools ? 'disponible' : 'no disponible'}</li>
            <li>prompts: {mcp.data.prompts ? 'disponible' : 'no disponible'}</li>
          </ul>
        ) : null}
      </section>
    </section>
  );
}
