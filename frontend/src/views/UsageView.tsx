/**
 * Usage view: aggregated token / cost totals and the per task breakdown.
 */
import { useApp } from '../state/AppContext';
import { useSnapshot } from '../state/useSnapshot';
import { EmptyState, ErrorState, LoadingState } from '../components/States';
import { TaskStateBadge } from '../components/Badge';
import { formatCost, formatNumber, formatTimestamp } from '../utils/format';

export function UsageView() {
  const { client } = useApp();
  const usage = useSnapshot(() => client.usage(), []);

  return (
    <section className="view" aria-labelledby="usage-heading">
      <header className="view__header">
        <h2 id="usage-heading">Usage</h2>
        <p className="muted">Tokens and cost recorded by the agent.</p>
      </header>

      {usage.loading ? <LoadingState label="Loading usage…" /> : null}
      {usage.error ? (
        <ErrorState message={`Unable to load usage: ${usage.error}`} onRetry={usage.reload} />
      ) : null}

      {usage.data ? (
        <>
          <dl className="totals">
            <div className="totals__item">
              <dt>Tasks</dt>
              <dd data-testid="usage-tasks">{formatNumber(usage.data.totals.tasks)}</dd>
            </div>
            <div className="totals__item">
              <dt>Input tokens</dt>
              <dd data-testid="usage-input-tokens">
                {formatNumber(usage.data.totals.input_tokens)}
              </dd>
            </div>
            <div className="totals__item">
              <dt>Output tokens</dt>
              <dd data-testid="usage-output-tokens">
                {formatNumber(usage.data.totals.output_tokens)}
              </dd>
            </div>
            <div className="totals__item">
              <dt>Cost (USD)</dt>
              <dd data-testid="usage-cost">{formatCost(usage.data.totals.cost_usd)}</dd>
            </div>
          </dl>

          {usage.data.tasks.length === 0 ? (
            <EmptyState message="No task has consumed resources yet." />
          ) : (
            <table className="table" aria-label="Usage per task">
              <thead>
                <tr>
                  <th scope="col">Task</th>
                  <th scope="col">State</th>
                  <th scope="col">Executor</th>
                  <th scope="col">Created</th>
                  <th scope="col">Usage</th>
                </tr>
              </thead>
              <tbody>
                {usage.data.tasks.map((task) => (
                  <tr key={task.id}>
                    <td>{task.id}</td>
                    <td>
                      <TaskStateBadge state={task.state} />
                    </td>
                    <td>{task.executor}</td>
                    <td>{formatTimestamp(task.created_at)}</td>
                    <td>
                      <code className="usage__raw">{JSON.stringify(task.usage)}</code>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      ) : null}
    </section>
  );
}
