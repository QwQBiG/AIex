# Chroma v2 bridge

`ai-ex-migrate` exports a Chroma collection through its read-only v2 API into a portable JSON snapshot, then imports supported main memories into a new Rust JSONL store. It does not parse a raw SQLite file or reconstruct Chroma's HNSW files directly.

## Copy first, then export

Stop the source Chroma writer before making a complete directory copy, including SQLite journals and vector-index directories. Keep the stopped original untouched. Start a compatible Chroma service only against the working copy, with its database path, loopback host, and port explicitly specified. Opening a copy can update or migrate that copy's files; the bridge's read-only HTTP requests do not imply that the service performs no database writes.

For example, after preparing a separate working copy, run the native service in one terminal:

```powershell
& 'C:\path\to\verified\chroma-windows.exe' run --path 'C:\exports\chroma-working-copy' --host 127.0.0.1 --port 18000
```

Export in another terminal; choose new output files:

```powershell
cargo run -p ai-ex-migrate -- --chroma-export --url http://127.0.0.1:18000 --collection vtuber_memories --tenant default_tenant --database default_database --output 'C:\exports\chroma-snapshot.json' --entity-collection vtuber_entities
```

Omit `--entity-collection` when no entity archive is needed. The tenant, database, main collection, and output are explicit required options. A collection's complete descriptor and each record's ID, document, metadata, vector, and URI are retained in the snapshot. The exporter calls version, descriptor, count, and paginated `get` endpoints; it does not invoke embedding functions, models, `query_texts`, add/update/delete/reset operations, or a Python runtime.

Export uses 128-record pages, rejects duplicate IDs, and checks counts before and after reading a collection. These checks cannot detect concurrent edits that leave the record count unchanged. A quiescent working copy is still required.

## Offline import

The native service can be stopped before import. Import reads only the snapshot and writes a new JSONL file:

```powershell
cargo run -p ai-ex-migrate -- --chroma-snapshot --input 'C:\exports\chroma-snapshot.json' --output 'C:\exports\imported-memory.jsonl' --profile character-a --embedding-space 'your-explicit-embedding-space'
```

Use the original embedding space's explicit identifier when vectors are present. It is not inferred from dimensions or a model name, and no embeddings are generated. Omit the option only for a snapshot without nonempty main vectors. If legacy timestamps lack a timezone, also provide `--utc-offset-minutes MINUTES`; no local timezone is guessed. Zoned RFC 3339 timestamps need no offset option.

Main IDs must be UUIDs. Main records need documents and metadata with a valid timestamp and a supported legacy `memory_type`: `INTERACTION` or `SUMMARY` becomes a conversation; `EVENT` becomes a live event. When both `user_input` and `ai_response` exist, they populate the Rust conversation fields; otherwise the document is used as user text. Original metadata is retained as `legacy_metadata`, and available vectors retain their numeric values under the explicit embedding space. A main media URI is archived in the snapshot but is unsupported for JSONL import. Imported memories have the selected profile and remain `automatic` sources with revision 1.

An entity collection is **archived only**. Its non-UUID IDs, documents, metadata, vectors, and URIs can be preserved in the snapshot, but import does not convert those records into memories or restore the source entity graph. The CLI reports this limitation explicitly.

Existing output paths are refused. The source snapshot remains unchanged. Reopening the JSONL through `MemoryStore` requires selecting the imported profile; other profiles cannot recall its records.

## Supported bounds

- HTTP responses and the final snapshot are limited to 32 MiB. Imported JSONL is also bounded to 32 MiB.
- Main and entity collections together may contain at most 50,000 records.
- Tenant, database, and collection names are nonempty, at most 512 UTF-8 bytes, and cannot contain control characters or path separators. Record IDs are nonempty and at most 4,096 UTF-8 bytes without control characters.
- Vectors contain at most 8,192 finite `f32` values. Nonempty imported main vectors must have matching dimensions and at least one nonzero value.
- Combined imported user/assistant text is at most 128 KiB per main record. Profiles contain at most 128 characters; embedding-space identifiers contain at most 256 characters without control characters. An explicit UTC offset is between -1,439 and 1,439 minutes.

Malformed snapshots, duplicate JSON keys, unsupported schema versions, mismatched tenant/database descriptors, and invalid record/count shapes fail before publication.

## Opt-in native acceptance test

`crates/ai-ex-migrate/tests/chroma_native.rs` is ignored by default. It requires an explicitly supplied absolute `AIEX_CHROMA_NATIVE_BIN`; there is no PATH lookup, Python fallback, package installation, model download, or use of an existing database directory.

The pinned executable is the official [`cli-1.3.0` Windows native asset](https://github.com/chroma-core/chroma/releases/download/cli-1.3.0/chroma-windows.exe). Its SHA-256 is checked by the test itself:

```text
71884ca430ae3118d46d51a10e733f77100aff368cc0cd64363a602d1ecdeb35
```

The asset is listed on the [official release assets page](https://github.com/chroma-core/chroma/releases/expanded_assets/cli-1.3.0). The [`cli-1.3.0`](https://github.com/chroma-core/chroma/releases/tag/cli-1.3.0) and [`1.4.0`](https://github.com/chroma-core/chroma/releases/tag/1.4.0) releases identify commit `29f33c493fc7cc1a79cf4da5e6454113bed48817`; CLI and Python release numbers differ. At that commit, the Rust server's [`/api/v2/version` handler](https://github.com/chroma-core/chroma/blob/29f33c493fc7cc1a79cf4da5e6454113bed48817/rust/frontend/src/server.rs#L500-L513) reports the fixed value `1.0.0`. The snapshot records that reported value as provenance, rather than interpreting it as the CLI release number.

After independently obtaining and verifying that exact executable, run:

```powershell
$env:AIEX_CHROMA_NATIVE_BIN = 'C:\path\to\verified\chroma-windows.exe'
Get-FileHash -Algorithm SHA256 -LiteralPath $env:AIEX_CHROMA_NATIVE_BIN
cargo test -p ai-ex-migrate --test chroma_native -- --ignored --nocapture
```

The acceptance test creates a unique temporary fixture, inserts 129 main memories with explicit three-dimensional vectors, and inserts a non-UUID entity with a URI. It persists real SQLite/HNSW files, stops the source process, inventories every source file's bytes, copies the whole database directory, restarts only that copy, and invokes the AIex CLI to export it. It verifies complete descriptors, records, metadata, vectors, paging, and the raw entity archive. Both native processes are stopped before offline import and `MemoryStore` reopen/profile-isolation checks. The stopped original's file inventory and bytes, source JSON snapshot, and imported JSONL are checked for unintended changes.

Child processes run with `ANONYMIZED_TELEMETRY=FALSE` and `TOKIO_WORKER_THREADS=1`, bounded readiness/CLI waits, and fixture-local logs. Windows children use `CREATE_NO_WINDOW`. Readiness requires both a live child and HTTP 200 because the native CLI can report a startup error while exiting with status zero. Drop guards kill and reap children and remove only the validated temporary fixture tree; symlinks and Windows reparse points are rejected during copy/cleanup.

Adding this test does not establish a passing native acceptance result. Run the ignored target with the pinned executable to obtain that result; no real user database or device is part of the fixture.

## Recorded local verification (2026-10-20)

The migrate package passed 44 tests: 27 unit tests, 12 Chroma protocol tests, three legacy backup integration tests, and two multimodal integration tests. The 12 protocol cases perform 46 CLI invocations and exercise 257-record paging (offsets 0/128/256), exact read-only requests, archive/import/reopen, required URI columns, version validation, duplicate collection identities, failures, and no-clobber. Strict `cargo clippy -p ai-ex-migrate --all-targets --locked --offline -j1 -- -D warnings` passed.

The native acceptance target compiled and is the one ignored test in the default run. It was **not executed**: obtaining the pinned official binary failed with a WebRequest TLS EOF, a curl timeout with zero received bytes, and a subsequent release API timeout. No native Chroma process or real user database was opened. A successful ignored native run is still required before claiming SQLite/HNSW fixture compatibility; protocol mock success alone does not establish it.