# Monodex — Consolidated Punch List

This document is a revised, post-review punch list focused on the remaining
functional and architectural issues that still matter for the next round of work.
Items that appear reasonably addressed have been removed to keep this document
actionable.

---

## Decisions (for future sessions)

1. **Priority order**: Work through categories A→B→C unless there is a concrete reason to reorder.

2. **Bar for inclusion here**: This list is intentionally limited to correctness bugs,
   failure-semantics gaps, and architectural mismatches likely to trip up future work.
   Trivial cleanup and polish items are omitted.

3. **Crawl config direction**: `crawl_config.rs` is the source of truth. Legacy wrappers
   in `config.rs` should not remain on the hot crawl path.

4. **Catalog type direction**: `monorepo` is the only implemented catalog type for now.
   Docs/schema were updated accordingly; runtime should enforce the same constraint.

---

## A. Crawl Correctness

These are the highest-priority items because they affect whether a crawl can be trusted.

### A.1 — Existing-file label-add failures still do not count as crawl failure ✅ FIXED

The shared crawl pipeline improved failure tracking, but there is still a correctness hole
for already-indexed files.

In both crawl entry points, the "existing file" path attempts `add_label_to_file_chunks()`
and logs failures, but those failures are not propagated into `CrawlFailures`. The final
`had_failures` check only looks at `pipeline_failures`, so the crawl can still proceed as
successful and mark `crawl_complete = true` even when label attachment failed for some
already-indexed files.

That is a real data-visibility bug: the crawl may claim success while some files were not
actually attached to the label.

- [x] Ensure failures from `add_label_to_file_chunks()` on already-indexed files are added to `CrawlFailures`
- [x] Make those failures participate in the same `had_failures` / partial-crawl logic as upload and embedding failures
- [x] Confirm the end-of-crawl summary reports these failures explicitly

### A.2 — Cleanup failure does not block `crawl_complete` ✅ FIXED

The current logic skips cleanup when earlier crawl failures happened, which is good.
However, if label reassignment cleanup itself fails (`remove_label_from_chunks()`), the
code only logs a warning and still sets `crawl_complete = true`.

This violates the intended semantics that label cleanup only completes after a fully
successful crawl transition. A failed cleanup means the label state may still be
inconsistent.

- [x] Treat label-cleanup failure as crawl failure for completion semantics
- [x] Do not set `crawl_complete = true` when cleanup fails
- [x] Report cleanup failure in the final crawl summary as a first-class failure mode

---

## B. Crawl Configuration Wiring

The crawl config system itself is sound, but there are still places where actual behavior
does not match the intended architecture.

### B.1 — Chunking strategy dispatch still ignores discovered crawl config ✅ FIXED

Both crawl paths now load `CompiledCrawlConfig` and use it for `should_crawl()` filtering.
But chunking still goes through `chunk_content()` → `get_chunk_strategy()` →
`load_compiled_crawl_config(None)`, which falls back to the embedded default config.

This means repo-local or user-global crawl config can decide whether a file is crawled,
but not reliably how it is chunked. For example, a repo can override a file type to use
`markdown`, but the actual chunking path may still use the default strategy.

This is the most important remaining config-wiring bug.

- [x] Pass `CompiledCrawlConfig` (or the resolved strategy) into `chunk_content()`
- [x] Remove strategy dispatch from the legacy default-config wrapper on the hot crawl path
- [x] Keep any default-config fallback only for commands that truly have no repo context (e.g. ad hoc tooling)
- [ ] Add a regression test proving that a repo-local `monodex-crawl.json` strategy override actually changes chunking behavior during a real crawl path

### B.2 — Working-directory enumeration still hard-filters hidden paths ✅ FIXED

The working-directory crawl now uses crawl config for file filtering, but filesystem
enumeration still excludes all dot-prefixed entries up front. That means hidden files and
directories never reach crawl-config evaluation, regardless of configured include/exclude
rules.

This is an architectural mismatch: crawl config is supposed to be the main policy surface,
but the enumerator still imposes hardcoded filtering before config gets a chance.

Also, some comments now claim only `.git` is excluded, which is no longer an accurate
description of the actual behavior.

- [x] Decide whether working-dir enumeration should exclude only `.git` by default, or whether broader hidden-path exclusion is intentional
  - **Decision**: Broader hidden-path exclusion is intentional. Hidden directories like `.cache`, `.temp`, `.idea`, `.vscode` typically contain build artifacts, editor configs, or temporary files that shouldn't be indexed.
- [x] Align the implementation with that decision
  - **Implementation**: Behavior unchanged, comments updated to accurately describe the actual behavior.
- [x] Update stale comments/docstrings so they describe the actual enumeration behavior

---

## C. Spec / Runtime Alignment

These issues matter because they create false confidence for the next round of work or
cause users/developers to copy invalid examples.

### C.1 — Runtime still does not enforce `monorepo` as the only supported catalog type ✅ FIXED

Docs/schema/examples were updated to remove `folder`, but runtime config loading still
accepts arbitrary catalog type strings and does not reject unsupported values. If config
validation is bypassed or drift occurs again, the binary will still accept unsupported
types without implementing distinct behavior.

At this point, runtime should enforce the same invariant the docs and schema already
communicate.

- [x] Validate catalog type during config loading / startup
- [x] Reject unsupported values with a clear error message
- [x] Remove or update any remaining runtime comments implying `"folder"` is supported

### C.2 — `DESIGN.md` still contains a stale strategy example ✅ FIXED

The strategy table was corrected, but an example config block in `DESIGN.md` still shows
a stale strategy value (`simpleLine`). Copying that example into a real crawl config will
fail validation because the implementation/schema only accept the currently supported
strategy names.

This is not a runtime bug, but it is still worth fixing because it will mislead the next
person using the design doc as a source of truth.

- [x] Update the stale `DESIGN.md` example config to use supported strategy names only
- [x] Remove any example entries for file types that are excluded by default and not meant to be configured in the example
- [x] Do a quick pass for any other copied strategy examples that still reference old names

---

## Notes

### What is intentionally not included here

The previous review found many checklist items that now look reasonably addressed, including:

- shared crawl pipeline extraction,
- worker distribution for parallel embedding,
- package-index nested path fix,
- markdown partitioner wiring,
- non-code output sanitization,
- dependency cleanup,
- package-name extraction deduplication.

Those are omitted from this revised list so the next round stays focused on the remaining
correctness and architecture gaps.

### Completion bar for this document

This document should be considered complete only when:

1. a crawl cannot be marked complete after any label-add or cleanup failure,
2. discovered crawl config actually controls chunking strategy during real crawls,
3. working-dir enumeration behavior is consciously aligned with crawl-config ownership,
4. runtime validation matches the documented/schematized catalog model,
5. design docs no longer contain invalid config examples.
