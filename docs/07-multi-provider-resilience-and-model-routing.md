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

## Prompt caching and cost

Agent turns resend their instructions, tools and conversation on every round,
so the provider's prompt cache decides much of their cost.

```toml
[models.claude]
provider = "anthropic"
id = "YOUR_ANTHROPIC_MODEL"
context_window = 200000
input_per_million = 3.0
output_per_million = 15.0
cache_read_per_million = 0.3    # cached input read back
cache_write_per_million = 3.75  # input written to the cache
prompt_cache = true             # Anthropic only: mark the cacheable prefix
```

- **Anthropic** caches only what a request marks. With `prompt_cache = true`
  each request marks the last tool definition, the instructions and the last
  block of the conversation (three of the four marks Anthropic allows), so the
  next round reads that prefix from the cache. `layeragent setup` turns it on
  for `https://api.anthropic.com/`; set it yourself for another endpoint only
  if that endpoint accepts Anthropic's `cache_control` blocks.
- **OpenAI, DeepSeek and Gemini** cache repeated prefixes on their own; nothing
  is marked. Streamed chat requests ask for usage
  (`stream_options.include_usage`), which is also how they report cached
  tokens.

Usage records every input token, with the cached ones counted apart:
`cache_read_tokens` (OpenAI `cached_tokens`, DeepSeek `prompt_cache_hit_tokens`,
Gemini `cachedContentTokenCount`, Anthropic `cache_read_input_tokens`) and
`cache_write_tokens` (Anthropic `cache_creation_input_tokens`). A cached token
costs its cache price, or the input price when none is configured. Cost limits
reserve a request at its upper bound: with `prompt_cache`, every input token at
the higher of the input and cache-write prices. Costs are estimates from your
configured prices, not provider invoices.

## Native metadata

Responses encrypted reasoning, Anthropic signed thinking and Gemini thought signatures require exact replay. Plaintext duplicate text/arguments is sanitized. If detected sensitive data occurs inside an atomic signed unit, processing fails explicitly instead of silently corrupting its signature.

## Qualification

Fixtures cover streaming framing, fallback and native metadata. Real-provider testing requires credentials and may incur cost. Do not infer live certification from an adapter compiling.
