# 08 · Artifact analysis and evidence

LayerAgent includes an artifact-analysis capability as part of its engineering toolset. This guide documents the implementation and does not depend on private research materials.

## Built-in inspection

| Data | Current behavior |
| --- | --- |
| Identity | SHA-256, byte count and canonical source path |
| Native objects | Supported parser format/architecture, entry point, sections and symbols |
| Heuristics | Byte entropy and limited magic-based classification |
| Bounds | 64 MiB inspection limit, bounded section/symbol listings |
| Provenance | Observed/inferred/unknown evidence records |
| Integrity | Hash-linked ledger with optional signed checkpoints |

```bash
layeragent rea inspect /path/to/artifact
layeragent rea ledger
layeragent rea verify
```

External bridges invoke installed tools such as Ghidra Headless, JADX, ILSpy, binwalk, unblob, tshark and ASAR tooling inside the execution policy. Their availability, licensing, input support and output quality are separate prerequisites. Configured MCP servers can add other analyzers.

## Evidence interpretation

A hash proves content identity relative to the bytes hashed. A ledger verifies consistency relative to a trusted checkpoint. Neither proves that a conclusion is true or that an administrator cannot replace the files. Fixture reconstruction demonstrates behavior on those inputs, not universal equivalence.

Broad control-flow matching, source-map recovery and general reconstruction remain roadmap capabilities. Execute untrusted artifacts only under a verified confinement boundary.
