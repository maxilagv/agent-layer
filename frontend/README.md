# LayerAgent frontend

Shared web/desktop frontend for LayerAgent. The same React application runs
in two hosts:

* the **Rust web server** (browser) — same-origin HTTP under `/api/...`;
* the **Tauri desktop shell** — IPC through `window.__TAURI__.core.invoke`.

Both transports implement the same `ApiClient` contract (`src/api/client.ts`),
so every view is host agnostic.

## Requirements

* Node 22 (`engines: { "node": ">=22 <23" }`, see `.nvmrc`).
  `.npmrc` sets `engine-strict=true`, so older Node versions fail fast.
* npm (no pnpm/yarn).

## Scripts

| command            | purpose                                              |
| ------------------ | ---------------------------------------------------- |
| `npm run dev`      | Vite dev server on port 5173, `/api` proxied to the daemon |
| `npm run build`    | `tsc -b` type check followed by a production build   |
| `npm run preview`  | serve the production build                           |
| `npm run typecheck`| type check only (`tsc -b --pretty`)                  |
| `npm test`         | unit tests (vitest + testing-library + jsdom)       |

The dev proxy target can be changed with `LAYERAGENT_API_TARGET`
(default `http://127.0.0.1:8080`).

## HTTP endpoints consumed by the frontend

All requests are relative to the current origin, carry `credentials: 'include'`
and — for every mutating request — the `X-LayerAgent-CSRF` header. The CSRF
token is read once from `GET /api/bootstrap` (`session.csrf_token`), kept in
memory only (never in `localStorage`/`sessionStorage`) and never placed in a
URL.

| method | path                                        | payload                                    |
| ------ | ------------------------------------------- | ------------------------------------------ |
| GET    | `/api/bootstrap`                            | `Bootstrap`                                |
| GET    | `/api/projects`                             | `Project[]`                                |
| GET    | `/api/sessions?project=<project>`           | `Session[]`                                |
| GET    | `/api/sessions/<id>/events?after=<seq>`     | `{ events, next }`                         |
| GET    | `/api/tasks?after=<seq>`                    | `{ tasks, next }`                          |
| POST   | `/api/tasks`                                | `Task` (body: `NewTaskRequest`)            |
| GET    | `/api/tasks/<id>`                           | `Task`                                     |
| GET    | `/api/tasks/<id>/events?after=<seq>`        | `{ events, next }`                         |
| POST   | `/api/tasks/<id>/cancel`                    | `Task`                                     |
| POST   | `/api/tasks/<id>/correct`                   | `CorrectionResult`                         |
| POST   | `/api/approvals/<id>`                       | `204 No Content` (body: `ApprovalDecision`)|
| GET    | `/api/artifacts/<hash>?offset&limit&session&task` | `{ offset, text }`                  |
| GET    | `/api/connections`                          | `Connection[]`                             |
| POST   | `/api/connections/<id>/action`              | `Connection` (body: `{ action }`)          |
| GET    | `/api/usage`                                | `{ totals, tasks }`                        |
| GET    | `/api/config`                               | `{ revision, config }`                     |
| GET    | `/api/models`                               | `unknown[]`                                |
| GET    | `/api/mcp/capabilities`                     | `{ resources, tools, prompts }`            |
| GET    | `/api/events?path=<stream-path>`            | Server-Sent Events (`TaskEvent` JSON)      |

`POST /api/tasks/<id>/correct` body: `{ instruction, expected_revision }`.
`POST /api/approvals/<id>` body: `{ expected_revision, arguments_hash, allow }`.
`POST /api/connections/<id>/action` body: `{ action }` where `action` is one of
`probe`, `enable`, `disable`, `revoke`.

### Stream paths

The `path` argument of `subscribe()` selects the event stream:

| path                          | content                                |
| ----------------------------- | -------------------------------------- |
| `tasks`                       | task events of every task              |
| `tasks?project=<project-id>`  | task events of one project             |
| `tasks/<task-id>`             | events of one task                     |
| `sessions/<session-id>`       | events of one session                  |

`after`/`next` are exclusive cursors: `next` returned by a page is sent back as
`after` to obtain the following page.

## Tauri bridge commands

`invoke('<command>', { <snake_case arguments> })`; the mapping is documented in
`src/api/index.ts`. Server-sent events arrive on the Tauri event channel
`layeragent://task-event` with the `TaskEvent` JSON as payload.

## Layout

```
src/
  api/          transport abstraction (types, client contract, http, tauri)
  components/   reusable presentation components (dialog, badges, timeline, ...)
  state/        hooks: snapshot-then-events feed, task list, app context
  utils/        formatting and id helpers
  views/        one component per navigation entry
  __tests__/    unit tests
```
