# Change Log - monodex

This log was last updated on Mon, 14 Apr 2026 15:16:00 GMT.

<!--
## Unreleased

(Add change entries here during pull request development.)

When ready to publish:
1. Choose an appropriate version number (semver)
2. Update `version` in Cargo.toml
3. Move entries from this comment block below the header as "## X.Y.Z"
4. Update the "last generated" date above
5. Remove this comment block (it will be re-added by the next PR)

### Breaking changes

- **crawl command now requires explicit source**: Must specify `--label` AND either `--working-dir` OR `--commit`
  - Previously: `monodex crawl --catalog myrepo --label main` (defaulted to HEAD)
  - Now: `monodex crawl --catalog myrepo --label main --commit HEAD`
  - This prevents accidental overwrites of labels and makes crawl intent explicit
  - CLI now shows proper usage: `monodex crawl --label <LABEL> <--commit <COMMIT>|--working-dir>`

### Bug fixes

- Document working directory hash incompatibility: `--working-dir` uses SHA-256 while `--commit` uses Git blob SHA-1, preventing incremental skipping between modes (fix pending)

## X.Y.Z
-->

## 0.2.0

### Updates

- Add `--debug` CLI flag for verbose network request logging
- Add `maxUploadBytes` config setting for Qdrant payload limit (default 30MB)
- Implement rewind upload algorithm for large batch splitting to avoid Qdrant payload limits
- Improve upload error handling: preserve chunks on failure, report clear error messages

## 0.1.0

### Minor changes

- Add JSON schemas for `config.json`, `monodex-crawl.json`, and `context.json` for IDE autocomplete
- Add user-configurable crawl settings via `monodex-crawl.json` (file types, exclusions, keep patterns)
- Add `--working-dir` flag to index uncommitted changes from the filesystem
- Add label-based indexing: maintain multiple queryable snapshots (branches, commits) within a catalog
- Add `use` command to set default catalog/label context for subsequent commands
- Add Git-based crawling: reads from Git objects, not working tree (deterministic, reproducible)
- Switch to jina-embeddings-v2-base-code model (768 dimensions, 8192 token limit)
- Increase chunk target size from 1800 to 6000 characters

### Patches

- Fix crawl error handling: track and report upload failures, label assignment failures
- Fix `source_uri` path separator on Windows
- Fix catalog validation in `use` command
- Fix race condition in crawl checkpointing
- Increase HTTP timeout for wait=true operations

## 0.0.1

- Initial release
