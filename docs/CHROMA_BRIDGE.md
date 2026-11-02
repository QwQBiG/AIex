# Chroma v2 bridge

`ai-ex-migrate` exports Chroma collections through the read-only v2 API into a portable JSON snapshot, then imports supported main memories, independent entity candidates, and declared source associations into a new Rust JSONL store. It does not parse a raw SQLite file or reconstruct Chroma's HNSW files directly.

## Copy first, then export

Close the legacy application and its Chroma writer first. Provide a closed snapshot directory containing `chroma.sqlite3`, including its journals and vector-index directories. The source, working-copy directory, executable, checksum, collection names, and output are explicit; the tool never discovers an active personal database.

Obtain the fixed [official `cli-1.3.0` Windows native executable](https://github.com/chroma-core/chroma/releases/download/cli-1.3.0/chroma-windows.exe) and verify SHA-256 `71884ca430ae3118d46d51a10e733f77100aff368cc0cd64363a602d1ecdeb35`. Then run the complete copy-and-export operation; `C:\exports` must already exist and both destinations must be new:

```powershell
cargo run -p ai-ex-migrate -- --chroma-copy-export --source 'C:\exports\closed-original' --working-copy 'C:\exports\chroma-working-copy' --native-server 'C:\tools\chroma-windows.exe' --server-sha256 71884ca430ae3118d46d51a10e733f77100aff368cc0cd64363a602d1ecdeb35 --collection vtuber_memories --tenant default_tenant --database default_database --output 'C:\exports\chroma-snapshot.json' --entity-collection vtuber_entities
```

Source files remain under read-only leases that reject writers and deletion until export and owned-server shutdown finish. Links, reparse points, hard-linked source files, nested source/copy destinations, existing destinations, and a changing directory inventory are rejected. Every copied file is checksum-verified. An occupied file or inconsistent snapshot fails rather than publishing a ready snapshot. The tool retains the working copy on failure; it does not repair or delete the original. Only the process tree it launched is stopped.

The verified native server opens only the working copy and binds an explicitly selected loopback port. Readiness includes ownership of that port. Its private home/temp environment, fixed executable bytes, and database path are independent of installed Python or another Chroma service. The native database argument uses an ordinary absolute Windows path because HNSW file persistence does not accept a verbatim `\\?\` prefix; canonical paths still govern copy validation.

Opening a working copy can update or migrate its files. The bridge's read-only HTTP requests do not imply that the native service performs no database writes. Compatibility of an unknown old database remains a user acceptance item.

For an already prepared, quiescent working copy served separately, the existing explicit URL exporter remains available:

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

Entity records become independent `entity_candidate` rows, retaining their complete original ID, document, metadata, vector, and URI. They are never regenerated from main-memory text during startup. Supported name/type/value/confidence fields form a bounded display candidate; incomplete fields remain unparsed candidates with the original record intact. The original numeric confidence is retained alongside the display score.

Explicit `related_memories` values become `entity_source` rows: an imported UUID source is active, an absent UUID is missing, and an unresolvable identifier remains unresolved. Duplicate references are counted once. Unknown legacy fields such as `relationships` are retained without inventing source links. Explicit self/third-party subjects are preserved; legacy records without one remain `unknown`. Same-name candidates remain separate and unknown subjects never become confirmed personal facts.

Correcting or forgetting a source withdraws that source's contribution. Other active sources remain available; when all declared sources are corrected or forgotten, the candidate has no current value while its source history stays visible. Missing or unresolved links do not become active merely because a matching record appears after restart. This restores the declared source associations without adding a general graph database.

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

The acceptance test creates a unique temporary fixture, inserts 129 main memories with explicit three-dimensional vectors, and inserts a non-UUID entity with a URI. Fixture preparation uses an explicit-vector query to materialize the real SQLite/HNSW files; it does not run an embedding model. The test stops the source process and invokes the actual `--chroma-copy-export` command to copy the directory, open only the copy, export the snapshot, and stop its owned server. It verifies descriptors, complete records, metadata, vectors, paging, the independent entity candidate, and the original entity archive. Both native processes are stopped before offline import and `MemoryStore` reopen/profile-isolation checks. The stopped original's complete file inventory and bytes, source JSON snapshot, and imported JSONL are checked for unintended changes.

Child processes run with `ANONYMIZED_TELEMETRY=FALSE` and `TOKIO_WORKER_THREADS=1`, bounded readiness/CLI waits, and hidden Windows startup. Readiness requires a live process and HTTP 200 because the native CLI can report a startup error while exiting with status zero. Production copy/export additionally verifies that its owned job hosts the listener. Drop guards stop owned children and remove only the validated temporary fixture tree; symlinks and Windows reparse points are rejected during copy/cleanup.

The executable is supplied explicitly, so a default test run keeps this acceptance target ignored. No real user database or device is part of the fixture.

## Recorded local verification (2026-11-02)

The migrate package passed 48 default tests, including 13 Chroma protocol cases and the new format-upgrade CLI. They cover 257-record paging (offsets 0/128/256), exact read-only requests, archive/import/reopen, explicit entity subjects and source states, unchanged raw metadata, malformed input, and refusal to overwrite. Strict Clippy passed for the memory, migrate, and VTS packages with all targets and features under the shared Windows build environment.

The ignored native acceptance target was also explicitly executed with the pinned official executable and matching SHA-256: **1 passed, 0 failed**. The complete synthetic SQLite/HNSW copy/export/import/reopen flow passed, and the stopped original's bytes were unchanged. This establishes the tested synthetic fixture behavior, not compatibility with every historic Chroma format. Real private legacy-database acceptance is **`not_run`**; the user must test a closed copy of their own data separately before adopting the imported output.
