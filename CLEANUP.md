# Monodex — Consolidated Punch List

This document combines findings from multiple independent code reviews into a single
prioritized work plan. Items are grouped by theme, with explanation and motivation
preceding each batch of work items.

---

## Decisions (for future sessions)

These clarifications were made during the review process:

1. **Priority order**: Work through categories A→B→C→D→E→F unless there's a specific reason to reorder.

2. **A.2 Label metadata ID strategy**: Use UUID-derived point IDs consistently. Keep `string_to_uuid()` derivation for both `upsert_label_metadata()` and `get_label_metadata()`. DESIGN.md has been updated to reflect this.

3. **A.7 Package index bug**: This was elevated to Category A (done) because it impacts breadcrumb quality for all nested packages in a monorepo.

4. **D.4 "folder" catalog type**: Removed from docs/schema for now (not implemented). Can be re-added later if needed.

---

## A. Crawl Correctness

These are the highest-priority items. They affect whether the crawl produces correct,
trustworthy data — the core promise of the tool.

### A.1 — Crawl failure semantics

The DESIGN doc says label reassignment cleanup runs "ONLY after a fully successful crawl
completion" and that interrupted/failed crawls must NOT trigger cleanup. The implementation
does not enforce this. After the embed/upload phase, failures are counted and logged, but
cleanup still runs unconditionally, and `crawl_complete` is still set to `true`.

This can produce a state where some chunks failed to upload or failed to get labels
assigned, but old chunks are removed and the label is marked complete anyway — silently
losing previously-reachable content.

The working-directory crawl path has a second problem: it has no failure tracking at all
(the `Arc<Mutex<Vec<String>>>` pattern used in the commit-based path was not carried over).

- [x] Track upload, file-complete, and label-add failures in both `run_crawl_label()` and `run_crawl_working_dir()`
- [x] Skip label reassignment cleanup if any failures occurred during the crawl
- [x] Skip setting `crawl_complete = true` if any failures occurred
- [x] Add a summary line at the end reporting whether the crawl was fully successful or partial

### A.2 — Label metadata identity mismatch

`upsert_label_metadata()` stores label metadata under a UUID derived from `label_id`
via `string_to_uuid()`, but `get_label_metadata()` fetches `/points/{label_id}` using
the raw string. These will never match. Additionally, if label names contain `/`
(e.g., `feature/foo`), the raw string in a URL path is fragile.

The DESIGN doc says label IDs are used "directly as the point ID," but the implementation
diverges.

- [x] Choose one ID strategy: either use raw `label_id` strings consistently (and URL-encode when needed), or use `string_to_uuid()` consistently for both read and write
- [x] Update the DESIGN doc to match whichever strategy is chosen
- [x] Verify that `get_label_metadata()` actually works with a round-trip test

### A.3 — Existing-file label-add failures treated as "touched"

For already-indexed files, the sentinel check inserts the `file_id` into `existing_files`
before calling `add_label_to_file_chunks()`. If that call fails, the file is still in
`existing_files`, so cleanup skips it — but the label was never actually added. The file
becomes invisible to search under that label.

- [x] Only add to `existing_files` after `add_label_to_file_chunks()` succeeds, or track label-add failures separately and exclude failed files from the cleanup's "touched" set

### A.4 — Embedding failures are silently dropped

`all_chunks.into_par_iter().for_each()` swallows embedding errors — if `embedder.encode()`
fails, the chunk vanishes with no log entry, no error counter, and no indication in the
final summary. This applies to both crawl paths.

- [x] Log embedding failures with the file path / chunk info
- [x] Count embedding failures and include them in the crawl summary
- [x] Consider whether embedding failures should prevent `crawl_complete = true`

### A.5 — All parallel embeddings use worker_index=0

The `ParallelEmbedder` creates multiple ONNX sessions to parallelize embedding work, but
both crawl paths pass `worker_index = 0` for every chunk. Despite using `rayon::par_iter()`,
all chunks contend on a single `Mutex<(Session, Tokenizer)>`, partially negating the
benefit of the worker pool.

- [x] Pass the rayon thread index or chunk index modulo `num_workers` instead of hardcoded `0`

### A.6 — `--commit` and `--working-dir` mutual exclusion is not enforced

The doc comments say these flags are mutually exclusive, but clap is not configured to
enforce it. Since `--commit` has `default_value = "HEAD"`, passing `--working-dir` silently
ignores whatever commit was specified.

- [x] Add `#[arg(long, conflicts_with = "commit")]` to `working_dir`, or remove the `default_value` from `commit` and handle the default in code

### A.7 — Package index extracts only the leaf directory name, not the full path

`build_package_index_for_commit()` in `git_ops.rs` uses `rsplit('/').nth(1)` to extract
the directory from a `package.json` path. This returns only the immediate parent folder
name, not the full relative path. For example, `libraries/node-core-library/package.json`
is indexed under key `"node-core-library"` instead of `"libraries/node-core-library"`.

`find_package_name()` then walks full ancestor paths like `"libraries/node-core-library"`,
`"libraries"`, `""` — none of which match the truncated key. This causes package name
resolution to fail for essentially every nested package in a monorepo, falling back to
the catalog name instead of the correct package name. Since monorepo package attribution
is the core breadcrumb feature, this is a significant correctness bug.

- [x] Fix the directory extraction to produce the full relative path (strip only the trailing `/package.json` from the filepath)
- [x] Ensure `find_package_name()` and the index use the same path format (repo-relative, `/`-separated)
- [x] Add a test case with nested packages (e.g., `libraries/node-core-library/package.json`) to verify correct resolution

---

## B. Crawl Configuration Wiring

The crawl config system (Phase 8) was designed and built correctly — the schema, validation,
discovery precedence, and compilation logic in `crawl_config.rs` are all sound. But the
actual crawl path never uses it. Both `run_crawl_label()` and `run_crawl_working_dir()`
still filter files through compatibility wrappers that hardwire the embedded default
config, and add a second independent filter (`is_text_file()`) with its own extension
list.

This means repo-local `monodex-crawl.json` files and user-global `crawl.json` files are
completely ignored during actual crawls, and Phase 8 completion is overstated.

### B.1 — Wire crawl config into the crawl flow

- [x] Load `CompiledCrawlConfig` once at crawl start using `load_compiled_crawl_config(Some(repo_path))` and pass it through the crawl pipeline
- [x] Replace `should_skip_path()` calls with `compiled_config.should_crawl()` (done: both crawl paths now use crawl_config.should_crawl())
- [x] Replace `is_text_file()` with `compiled_config.matches_file_type()` — eliminated (file type matching is now handled by should_crawl())
- [x] Pass the compiled config to `chunk_content()` so strategy dispatch uses discovered config, not the embedded default
- [x] Remove (or deprecate behind a feature flag) the `should_skip_path()` and `get_chunk_strategy()` compatibility wrappers in `config.rs` (kept for backward compatibility with dump-chunks command)

### B.2 — Eliminate per-call config recompilation

- [x] This is automatically fixed by B.1 (load once, pass through) — verified no other call sites remain that load-and-compile per file

### B.3 — Wire crawl config into working-directory enumeration

- [x] Use the compiled crawl config for working-directory filtering instead of hardcoded directory lists
- [x] Fix the `.git` directory exclusion: now properly excludes `.git` (hidden directories are skipped during filesystem walk, then crawl config filters the results)

### B.4 — Fix `.gitignore` claim

- [x] Remove the `.gitignore` claim from the doc comment (the function now only excludes `.git` directories, all other filtering is delegated to crawl config)

---

## C. Code Deduplication and Cleanup

The codebase has accumulated migration debt from the Phase 1-8 progression. Several
subsystems exist in duplicate — an old version and a new version — with the old version
still on the hot path. Cleaning this up reduces the surface area for bugs and makes the
codebase easier to work in.

### C.1 — Extract shared crawl pipeline

`run_crawl_label()` and `run_crawl_working_dir()` are each ~500 lines with ~80% identical
code. The entire embed/upload/progress/uploader-thread pipeline is copy-pasted between
them. The working-dir version is already a stale fork (it lacks the failure tracking
added to the commit-based path).

- [ ] Extract the shared embed → upload → checkpoint → progress pipeline into a common function
- [ ] Both crawl paths should differ only in: file enumeration source, blob_id vs content_hash, and label metadata fields

### C.2 — Consolidate package-name extraction

Two independent implementations parse package.json by ad-hoc string search:
`extract_package_name_from_bytes()` in `git_ops.rs` and `extract_package_name()` in
`package_lookup.rs`. Both are fragile — they find the first textual `"name"` key, which
can misfire on package.json files where a nested object's `"name"` appears first.

`serde_json` is already a dependency.

- [ ] Replace both string-search implementations with a single JSON-parsing implementation (e.g., `serde_json::from_slice` with a struct that has `name: Option<String>`)
- [ ] Put the canonical implementation in one place (probably `git_ops.rs` since that's where both callers live)
- [ ] Either remove `package_lookup.rs` entirely (it appears unused by the crawl path) or clearly document its role as a filesystem-only convenience for `dump-chunks`

### C.3 — Consolidate SHA256 hashing — WONTFIX

Three places compute SHA256 with `sha256:` prefix independently: `util::compute_hash()`,
`git_ops::compute_content_hash()`, and inline in `markdown_partitioner.rs`. This is real
duplication but each is ~4 lines of identical logic that won't drift (SHA256 is SHA256).
Fix opportunistically when touching adjacent code; not worth a standalone work item.

- [x] WONTFIX — fix opportunistically when touching adjacent code

### C.4 — Remove unused dependencies

- [x] Remove `ndarray` from Cargo.toml (not imported anywhere)
- [x] Remove `uuid` from Cargo.toml (UUIDs are generated via custom `string_to_uuid()`, not the crate)
- [x] Remove `serde_bytes` from Cargo.toml (not imported anywhere)

### C.5 — Remove or mark legacy Qdrant methods

Several uploader methods are leftovers from pre-label API surface:

- `query()` — catalog-only search, superseded by `search_with_label()`
- `get_chunks_by_file_id()` — unfiltered by label, superseded by `get_chunks_by_file_id_with_label()`
- `get_point()` — explicitly marked legacy
- `delete_file()` — filters by `source_uri`, doesn't match the file-id-centric model
- `Shr` trait implementations on `QdrantId` — never used

- [x] Remove unused methods, or gate them behind `#[cfg(test)]` / `#[allow(dead_code)]` with a clear "legacy — remove after migration" comment
- [x] Remove the `Shr` trait implementations for `QdrantId`

### C.6 — Fix stale comments and doc strings — WONTFIX

Several comments now misdescribe behavior after migration steps. These should be fixed
opportunistically as adjacent code is touched, not as a dedicated pass.

- [x] WONTFIX — fix individually when touching the relevant file in other work items

---

## D. Spec/Implementation Alignment

The DESIGN doc, DEV_PLAN, README, and code have drifted apart in several places. This
creates false confidence for developers working with the plan and confusion for users
reading the README.

### D.1 — Fix strategy naming inconsistency

DESIGN.md lists valid strategies as: `typescript`, `javascript`, `markdown`, `json`,
`simpleLine`. The implementation (`crawl_config.rs:240`) only accepts: `typescript`,
`markdown`, `lineBased`. The JSON schema (`crawl.schema.json`) matches the code. The
README config table uses `lineBased`.

- [ ] Update DESIGN.md strategy table to match implementation: remove `javascript`, `json`, `simpleLine`; use `lineBased`
- [ ] Or, if `javascript`/`json` should be valid config values (even if they produce empty results), add them to `is_valid_strategy()`

### D.2 — Fix point ID format descriptions

Three sources disagree on point ID format:
- DESIGN says `hash(file_id + chunk_ordinal)`
- DEV_PLAN says `<file_id>:<ordinal>`
- Code returns a UUID-shaped deterministic hash string

- [ ] Update DESIGN.md and DEV_PLAN to describe the actual implementation: `string_to_uuid(format!("{}:{}", file_id, chunk_ordinal))`

### D.3 — Fix `deny_unknown_fields` completion claim — WONTFIX (roll into B.1)

DEV_PLAN Phase 8.4 says `deny_unknown_fields` was added to all config structs. Only
`CatalogConfig` and `CrawlConfig` actually have it. `Config`, `QdrantConfig`, and
`DefaultContext` do not. This is a three-line fix with near-zero blast radius — users
with config typos will notice immediately because the tool won't find their catalog.

- [x] WONTFIX as standalone item — add the annotations when touching config structs during B.1

### D.4 — Fix `folder` catalog type

The README, config schema, and example config all support `type: "folder"` and describe
distinct behavior (using parent folder name as package identifier). The implementation
never branches on catalog type — it always uses package.json-based resolution.

- [x] Either implement folder catalog semantics (use parent folder name when no package.json found), or remove `folder` from the docs/schema until implemented, and validate that `type` is `"monorepo"` in config loading

### D.5 — Fix tilde expansion for catalog paths

The example config uses `~/projects/...` for catalog paths, but the code passes the raw
string to `Path::new()` without shell expansion. Users who copy the example will get
"path not found" errors.

- [ ] Apply `shellexpand::tilde()` to `catalog_config.path` (it's already used for config and context file paths)

### D.6 — Fix `get_file_sentinel` signature mismatch

DEV_PLAN says `get_file_sentinel(file_id, label_id)` was added, but the implementation
has `get_file_sentinel(file_id)` with no label filtering on the sentinel check.

- [ ] Update DEV_PLAN to reflect the actual signature, or add label filtering if the design intent was to scope sentinel checks by label

### D.7 — Fix markdown feature description

Markdown support is currently described three different ways across the docs:
- README feature list says it's supported
- README config table says "TODO: currently line-based"
- DEV_PLAN Phase 9 says it's future work
- `markdown_partitioner.rs` exists with tests and snapshots
- `chunker.rs` still uses line-based splitting

- [ ] Wire `partition_markdown()` into `chunk_content()` for the `Markdown` strategy branch
- [ ] Update README to remove the "TODO" hedge
- [ ] Update DEV_PLAN Phase 9 to mark the basic implementation as complete, with remaining items for sub-chunking, front matter, etc.

### D.8 — Fix README architecture section

The README architecture file tree doesn't list `crawl_config.rs`, and describes `config.rs`
as "File exclusion rules" when it's now just a thin compatibility wrapper.

- [ ] Update the architecture file tree to include `crawl_config.rs`
- [ ] Update the `config.rs` description to "Legacy compatibility wrapper (delegates to crawl_config)"

### D.9 — Update `CHUNKER_ID` or document its scope — WONTFIX

`CHUNKER_ID` is `"typescript-partitioner:v1"` but it's used in file_id computation for
all file types. This is semantically misleading for markdown and line-based files, even
if functionally correct (changing it would invalidate all existing IDs, which is the
designed mechanism). A one-line comment is fine but doesn't need its own work item.

- [x] WONTFIX — add a clarifying comment when the markdown partitioner is wired in (D.7)

---

## E. Hardening

These items improve robustness and security without changing core functionality.

### E.1 — Sanitize non-code output fields

The README says code lines are prefixed with `>` to prevent injection. But breadcrumbs,
catalog names, relative paths, and source URIs are printed raw. A malicious repository
could embed terminal control sequences in file paths or headings.

- [ ] Sanitize or escape non-code fields in search and view output (strip control characters at minimum)

### E.2 — Fix `source_uri` format — WONTFIX

`source_uri` is built as `"{repo_path}:{relative_path}"`, which is not a URI and is
ambiguous on Windows (drive letters contain `:`). But the field is explicitly documented
as "best-effort display/debug locator, NOT a key." Nobody is parsing it. Making it a
proper `file://` URI adds complexity for zero user benefit.

- [x] WONTFIX — the field serves its purpose as-is

### E.3 — Fix `chrono_timestamp()` — WONTFIX

The function name implies it uses the chrono crate but it does manual UTC epoch math.
The displayed time is always UTC with no timezone indicator. This is purely cosmetic —
the timestamps work, the function is internal, and users aren't making decisions based
on the exact time shown in progress logs.

- [x] WONTFIX — rename opportunistically if the function is touched for other reasons

---

## F. Upcoming Feature Work

These items represent valuable capabilities for the next phase of development, informed by
the tool's stated goal of being the primary way an AI agent learns about a codebase.

### F.1 — MCP server

Monodex is explicitly designed for AI assistants, but agents currently have to shell out to
the CLI. A built-in MCP (Model Context Protocol) server would let Claude Code, Cursor, and
other agents discover and call search/view capabilities as native tools — reducing friction
from "agent-friendly" to "agent-native."

- [ ] Implement `monodex mcp-serve` command that exposes search and view as MCP tools
- [ ] Support single-project and workspace modes
- [ ] Include JSON-structured output for all tool responses

### F.2 — Watch mode

The DEV_PLAN lists this as Phase 11 but it's a high-value capability. Continuously
monitoring the working directory and updating the index as files change eliminates the
need for manual re-crawls and makes the tool useful as a live companion during development.

- [ ] Implement `monodex watch` command using filesystem notifications (e.g., `notify` crate)
- [ ] Debounce rapid changes to avoid redundant re-indexing
- [ ] Perform incremental updates (re-chunk and re-embed only changed files)
- [ ] Design integration with the label system (watch mode updates a specific working-dir label)

### F.3 — Hybrid search (vector + keyword)

Pure vector search can miss exact identifier matches — searching for `handleAuth` may not
find the function if the embedding doesn't preserve the exact token. Combining vector
similarity with keyword/text matching using Reciprocal Rank Fusion (RRF) addresses this.
Qdrant supports full-text search indexes natively, so this can be implemented server-side
rather than loading all chunks into memory.

- [ ] Add a full-text search index to the Qdrant collection for the `text` field
- [ ] Implement RRF fusion of vector and keyword results
- [ ] Make hybrid search configurable (enable/disable, tunable k parameter)

### F.4 — Search result boosting

Configurable score multipliers based on file path patterns would let users tune search
relevance — boosting source directories and penalizing test/mock/generated/vendor files.
Monodex's crawl config already excludes some file patterns entirely, but there are cases
where files should be indexed but ranked lower, not invisible.

- [ ] Add a `searchBoost` section to the crawl config with penalties and bonuses by path pattern
- [ ] Apply multipliers to search scores after Qdrant returns results
- [ ] Ship sensible defaults (boost `src/`, penalize `test/`, `mock/`, `generated/`, `.md`)

### F.5 — JSON output mode

For programmatic consumption (MCP, scripts, CI pipelines), structured JSON output is
essential. The current output format is human-readable with `>` prefixed lines, which
requires ad-hoc parsing.

- [ ] Add `--json` flag to `search` and `view` commands
- [ ] Output results as a JSON array with file_id, chunk_ordinal, score, breadcrumb, text, and metadata
- [ ] Add `--compact` flag for minimal JSON (omit text content, include only identifiers and scores)

### F.6 — Call graph tracing (future)

The tree-sitter infrastructure already parses TypeScript ASTs for chunking. Extending
this to extract cross-file symbol references would enable "who calls this function" and
"what does this function call" queries — a qualitatively different kind of code
understanding. This is a larger effort but builds on existing infrastructure.

- [ ] Design a symbol index format for storing caller/callee relationships
- [ ] Extract function definitions and call sites during the chunking pass
- [ ] Implement `monodex trace callers <symbol>` and `monodex trace callees <symbol>` commands
- [ ] Consider both regex-based (fast, multi-language) and AST-based (precise, TypeScript-first) extraction modes

### F.7 — Broader language support for AST chunking

Monodex currently does AST-aware chunking only for TypeScript/TSX. Other languages get
line-based chunking, which still produces useful search results (the embedding model
handles any language), but with lower chunk quality — no breadcrumbs, no symbol names,
no semantic boundaries.

- [ ] Prioritize JavaScript/JSX as the next AST-aware language (tree-sitter grammar already available, syntax is a subset of TypeScript)
- [ ] Consider Python, Go, and Rust as subsequent targets based on Rush Stack ecosystem needs
- [ ] Design the partitioner interface to make adding new languages straightforward

### F.8 — Offline garbage collection

The DESIGN doc describes an offline GC command for cleaning up orphaned chunks after
interrupted crawls. This is tracked in DEV_PLAN Phase 10 but not yet implemented.

- [ ] Implement `monodex gc --catalog <name>` to scan for chunks with empty `active_label_ids` and delete them
- [ ] Add `--dry-run` flag to show what would be deleted
- [ ] Report count and estimated storage recovered
