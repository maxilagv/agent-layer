# 03 · Gateway, API, messaging and MCP

The gateway normalizes ingress into scoped sessions. Private IPC and authenticated HTTP reuse the same engine. Public HTTP defaults to `127.0.0.1:9473` and needs the configured owner token.

## API

| Method/path | Purpose |
| --- | --- |
| `GET /health` | Authenticated service/protocol identity |
| `GET/POST /v1/sessions` | List/create authorized sessions |
| `POST /v1/sessions/{id}/turns` | Execute and stream events through SSE |
| `GET /v1/sessions/{id}/events` | Read persisted events after a cursor |
| `GET /v1/sessions/{id}/ws` | Observe via WebSocket |
| `POST /v1/jobs/{id}/cancel` | Cancel an owned job |
| `POST /v1/approvals/{id}` | Resolve an owned approval |
| `GET /v1/models` | Configured model descriptors |
| `GET /v1/schema` | Protocol schema |
| `GET /v1/config` | Owner-only redacted configuration |
| `POST /v1/config/reload` | Owner-only reload |
| `POST /v1/shutdown` | Owner-only shutdown |
| `/mcp` | Owner-only MCP over Streamable HTTP |
| `/webhooks/{channel}` | Channel-specific signature/subscription validation |

The actual owner is identified independently of tenant/principal strings. Delegated entries cannot claim the reserved local owner scope. Same-identity bindings must grant identical workspace/read permissions. Ambiguous token values are rejected.

## Admission and streaming

The gateway bounds request bodies, global/per-principal admission, request rates, concurrent jobs and stream connections. WebSocket frames and lifetime are bounded; slow request handling has a deadline. Capacities remain occupied while response bodies are alive. Origin checks reject mismatched browser origins. Exact constants live in `layeragent-gateway/src/admission.rs`.

## Channels

| Channel | Implementation | Additional requirements |
| --- | --- | --- |
| Telegram | Native connector and signed webhook mode | Bot token and sender grant |
| Discord | Gateway connector | Token/intents and sender grant |
| Slack | Signed webhooks | App signing secret and token |
| WhatsApp | Signed webhooks/subscription challenge | Separate verification token and app credentials |
| Matrix | Connector | Encrypted rooms need a decrypting bridge |
| Signal | Connector | Compatible `signal-cli` service |

Live platform qualification requires separate accounts and credentials. Durable inbox/outbox queues retain uncertain delivery outcomes rather than blindly resending them. Attachments and richer approval UX remain product work.

## MCP

Client transports: confined stdio subprocesses and configured HTTP endpoints. Discovery/calls are approval gated. HTTP redirects are disabled, responses and discovery pagination are bounded, and calls are cancellable with deadlines.

Server tools currently expose `layeragent_run` and `layeragent_sessions`. HTTP server access is owner-only; it is not a general delegated remote-execution endpoint. Richer authentication and integration administration remain roadmap work.
