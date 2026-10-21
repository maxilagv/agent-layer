# Contributing to LayerAgent

LayerAgent is an MIT-licensed Rust workspace in development. The public repository is [maxilagv/agent-layer](https://github.com/maxilagv/agent-layer). Use issues for non-sensitive bug reports and feature discussions, and pull requests for proposed changes. Follow the private-disclosure guidance in [SECURITY.md](SECURITY.md) for security findings.

## Development environment

Install the toolchain pinned in `rust-toolchain.toml`, a C compiler and Python 3.11+. Linux native execution additionally needs Bubblewrap, namespaces and Landlock ABI 3. Run `layeragent doctor` before interpreting execution errors as model failures.

```bash
cargo install cargo-deny --version 0.20.2 --locked
cargo build --locked -p layeragent-cli
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 tests/verify.py
```

Read [architecture](docs/01-architecture-and-kernel-isolation.md), [configuration](docs/11-configuration-and-operations.md) and [security](SECURITY.md). Keep changes within the owning crate and avoid introducing a second orchestration layer in a client.

## Change expectations

- Reproduce the bug or establish a concrete missing behavior before editing.
- Exercise an ordinary successful case as well as the failure or adversarial case.
- Preserve permission checks, identity boundaries, uncertainty and cancellation semantics.
- Update user-facing examples and implementation status when contracts change.
- Record native tests separately from mocks, compilation and cross-compilation.
- Do not include paid-provider credentials, generated runtime state or personal paths.

## Security-sensitive work

Follow `SECURITY.md`. Never repair a failed confinement test by disabling the kernel or browser sandbox. Provider signatures must remain byte-exact; unsafe signed plaintext must be rejected as a whole unit. An operation with uncertain effects must not be retried automatically.

## Local release preparation

```bash
python3 scripts/prepare_release.py --output /tmp/layeragent-source.tar.gz
```

The command creates a reviewable source archive and dependency inventory. It does not create a Git repository, push code, publish packages or certify the build. Review the archive and its manifest before any future distribution.

Optional detached signing requires **OpenSSL 3** and an existing unencrypted Ed25519 PEM private key with private permissions (`0600` on POSIX). Keep the key and all outputs outside the source tree. The tool never creates a permanent signing key:

```bash
python3 scripts/sign_source.py sign \
  --archive /external/layeragent-source.tar.gz \
  --private-key /external/release-ed25519.pem \
  --signature /external/layeragent-source.sig \
  --public-key-output /external/release-public.pem \
  --metadata /external/source-signing.json
python3 scripts/sign_source.py verify \
  --archive /external/layeragent-source.tar.gz \
  --signature /external/layeragent-source.sig \
  --public-key /independently-trusted/release-public.pem
```

The signature authenticates exact archive bytes, including the embedded manifest. The JSON sidecar is unsigned descriptive metadata. Authenticate the public key through an independent trusted channel; never automatically trust an exported or archive-provided key. Existing outputs, symlink paths and archives larger than 512 MiB are refused. Encrypted or hardware-backed keys require a separate signing process. `python3 tests/source_signing.py` exercises disposable-key roundtrips, wrong keys and tampering; it does not sign a trusted release.
