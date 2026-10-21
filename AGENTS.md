# Working on LayerAgent

Keep target design and verified implementation coverage distinct. Update
`docs/10-implementation-status.md` when behavior or verification boundaries change.

Run formatting, warning-free Clippy and relevant tests before delivery. Runtime
changes require the loopback system/authentication smoke tests. Test doubles do
not certify confinement: run native tests explicitly on supported hosts. Never
add an unconfined production fallback or enable an unvalidated platform.

Keep credentials out of source, prompts, diagnostics and generated source bundles.
Preserve the operator's existing changes. Public documentation must be standalone,
use repository-relative links, and contain no private research or machine paths.

No Git initialization, remote upload, publication or release signing is implied
by preparing local code or a source archive. Record actual test outcomes, including
blocked and skipped checks, against the maintained source fingerprint.
