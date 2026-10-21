# 07 · Providers, routing and privacy

Five wire protocols are implemented: `openai_responses`, `openai_chat`, `anthropic`, `gemini` and `ollama`. Compatible services use their configured endpoint/model identifier. Each integration must still be qualified against its real service.

## Routing

The operator selects a primary model and an ordered fallback list. Only configured eligible models are used. Circuit breakers avoid recently failing providers; request generation, output limits and context fit are checked before dispatch. Transient failure can trigger an authorized fallback. An uncertain billed attempt is not guaranteed to be free.

## Credentials

Use `key_env` for environment references. Supported official OAuth public-client registrations can use PKCE/device flow, refresh and OS keyring storage. Tokens are not saved as plaintext configuration. Credentials are registered for exact-value redaction before provider request content is formatted. Authentication headers intentionally carry the credential to the configured provider; prompt content must not.

## Outbound policy

```toml
[privacy]
local_models_only = true
allowed_providers = ["local"]
```

An empty provider allowlist permits explicitly configured providers. A nonempty list restricts selection and fallback. Local-only mode accepts loopback model endpoints and rejects remote model requests. Disable or restrict browser, MCP, channels and tool networking separately when an entirely offline environment is required. Optional embedding artifacts must be preinstalled or embeddings left disabled to prevent model downloads.

## Native metadata

Responses encrypted reasoning, Anthropic signed thinking and Gemini thought signatures require exact replay. Plaintext duplicate text/arguments is sanitized. If detected sensitive data occurs inside an atomic signed unit, processing fails explicitly instead of silently corrupting its signature.

## Qualification

Fixtures cover streaming framing, fallback and native metadata. Real-provider testing requires credentials and may incur cost. Do not infer live certification from an adapter compiling.
