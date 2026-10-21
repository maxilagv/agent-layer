# Changelog

## 0.1.0 — development, P0 hardening

- Added credential registration, typed plaintext sanitization and bounded stream redaction.
- Preserve original tool execution arguments while storing a digest and sanitized replay copy.
- Added outbound model privacy controls and broader authority-change cancellation.
- Restricted global administration and owner MCP to the actual owner identity.
- Bounded gateway admission, rates, streaming connections, jobs and remote MCP responses.
- Added concrete external-action approvals and browser target drift checks.
- Hardened process termination, proxy parsing and browser execution boundaries.
- Disabled unvalidated production execution paths rather than falling back unconfined.
- Added encrypted offline backups, manifests, explicit restore and retention commands.
- Added repeatable source fingerprints, source-only packaging and dependency checks.
- Replaced design comparisons with standalone public documentation.

See [implementation status](docs/10-implementation-status.md) for verification and remaining gates. This changelog describes code changes, not a certification.
