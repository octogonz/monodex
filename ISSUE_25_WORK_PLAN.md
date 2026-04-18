# Monodex Identifier Terminology & Validation Spec

> **Companion document:** the identifier & reference syntax proposal at https://github.com/microsoft/monodex/issues/25 (the "syntax spec") defines the grammar for catalog, label, and path references, including forms not yet implemented. That document is authoritative for grammar. This document is authoritative for what lands in the current implementation.

## 1. Problem

The codebase uses two forms for referring to a label and is inconsistent about which goes where:

- **`label_name`** — bare label identifier like `main` or `feature/login-flow`
- **`label_id`** — fully qualified storage key like `rushstack:main`

The distinction is sound but the boundaries have drifted:

- **CLI commands disagree on input shape.** `use` takes `--catalog` and `--label` as two bare flags. `search`, `view`, and `crawl` document `--label` as the colon-joined form `<catalog>:<label>`. `crawl`'s doc comments contradict themselves and its handler requires the colon form — so the bare-name usage the docstring implies actually fails validation.
- **The qualified form leaks into user-facing surfaces** (CLI flags, `Label ID:` printouts, error messages) where it doesn't belong. The code currently parses and prints IDs where it should parse and print names, which is precisely the confusion this spec exists to eliminate.
- **No identifier has defined character rules.** Any string is accepted today, including values that collide with characters the syntax spec reserves for future grammar.
- **The word `label_name` is itself a source of drift.** Every field named `label_name` is an opportunity for a future reader or author to conflate the name and the id, as has already happened.

## 2. Terminology

Apply consistently across every file. No mixing, no drift.

- **`catalog`** — bare catalog identifier, e.g. `my-repo`. User-facing.
- **`label`** — the semantic component stored inside a catalog, e.g. `main`, `feature/login-flow`. User-facing. This is the rename target for every current `label_name` usage.
- **`label_id`** — fully qualified storage key `<catalog>:<label>`. **Internal only.** Appears in the Qdrant `active_label_ids` payload, as a UUID seed for label-metadata points, and in internal log/debug output. A user inspecting a Qdrant collection sees the qualified form in raw data; that is the intended boundary.

Strict rule: a field, variable, parameter, log line, CLI flag, or doc section that refers to the user-typed short form is named `label` (or `catalog`). Anything named with an `_id` suffix must actually contain the qualified `<catalog>:<label>` form. Mismatches are bugs.

## 3. Scope: What Lands Now vs. What's Reserved

Today's commands (`use`, `crawl`, `search`, `view`) only need:

- A `--catalog <catalog>` flag accepting a bare catalog.
- A `--label <label>` flag accepting a bare label.
- Composition of `label_id = <catalog>:<label>` at the storage boundary.

The syntax spec describes a richer grammar — `@catalog:label` references, typed labels (`kind=payload`), path references (`label:path`, `@catalog:label:path`), and reserved characters for further extensions (`+`, `#`). **None of that grammar is parsed or accepted as input today.** What this spec implements from the syntax spec is just:

- The character rules for bare `catalog` and bare `label` identifiers, including allowing `=` inside labels (see §4 invariant 4 for the exact treatment).
- The reserved-character bans for `@`, `+`, `#`, `:`, whitespace, and control characters, enforced today so future grammar can land without breaking existing data.

The `@` prefix is reserved as the lead-in for the planned cross-catalog reference form (`@catalog:label`) and is forbidden in any bare catalog or label today. Reserving it now means the future grammar can be added as a pure extension.

## 4. Invariants

1. **Validation happens at boundaries.** Any catalog or label entering the system through a CLI flag, a config file, a stored context file, or a Qdrant payload read is validated before use. Composition and internal passing of already-validated identifiers does not re-validate.

2. **Users never type or see the qualified form directly.** The CLI accepts catalog and label only as two separate flags. The legacy bare colon-joined `--label catalog:label` form is rejected. User-facing output prints `Catalog:` and `Label:` as separate fields and never uses `Label ID:` or similar phrasing.

3. **CLI flags fall back to stored default context independently.** Missing `--catalog` → default catalog; missing `--label` → default label.

4. **Reserved characters are forbidden in bare identifiers.** These are forbidden in both `catalog` and `label` values today: `:`, `@`, `+`, `#`, whitespace, and control characters. Rationale: the syntax spec plans richer grammar using these characters, and reserving them now prevents breaking changes later.

   **Special case for `=`:** `=` is **allowed inside labels but reserved for future grammar and not interpreted today.** A user who types `--label branch=main` today gets a label literally named `branch=main`, which composes into `label_id = my-repo:branch=main`. Monodex does not parse, split, or assign meaning to the `=`. This permits users to adopt a `kind=payload` naming convention in their own automation (e.g. a cronjob crawling `branch=main`) and ensures the wire format will be compatible when Monodex later grows native typed-label support. `=` is forbidden in `catalog` identifiers.

5. **Catalog and label character sets and regexes:**
   - **`catalog`**: strict kebab-case.

     ```
     ^[a-z0-9]+(?:-[a-z0-9]+)*$
     ```

     Length 1–64. Lowercase ASCII alphanumeric words separated by single `-`. No leading, trailing, or consecutive `-`.

   - **`label`**: Git-like, with `=` permitted internally per invariant 4.
     ```
     ^[a-z0-9]+(?:[./=-][a-z0-9]+)*$
     ```
     Length 1–128. Lowercase ASCII alphanumeric words separated by single `.`, `/`, `-`, or `=`. No leading, trailing, or consecutive separators.

   These regexes are authoritative and must be implemented exactly as written. Error messages should still be produced by runtime code (see §5) rather than by schema pattern mismatch.

6. **Relative paths cannot contain reserved grammar characters.** A `relative_path` captured at crawl time must not contain `:`, `@`, or `=`. `:` would break breadcrumb parsing; `@` and `=` would break future reference round-tripping.

7. **All `label_id` construction goes through a single composition function.** No ad-hoc `format!("{}:{}", catalog, label)` anywhere in the codebase. The composition function is the only place that knows the separator is `:`, so that if the storage encoding ever changes it changes in exactly one place.

8. **No backward compatibility.** Existing Qdrant collections, context files, or configs containing identifiers that fail the new validation are broken by this change and will not load. A clear error pointing at the offending value is the migration path.

## 5. Error Reporting

Follow Rush Stack's conventions:

- **Runtime validation produces the useful errors; JSON schemas stay loose.** Schemas constrain to `type: string, minLength: 1, maxLength: N` and at most a trivial shape hint. Nontrivial rules are enforced in code because a schema pattern mismatch produces `does not match pattern` — unhelpful — whereas runtime code can produce `the catalog name "foo--bar" contains consecutive '-' characters`.
- **Every error names the field, quotes the full offending value, names the specific problem, and suggests a fix where one exists.** Target messages of this quality:
  - `the label "foo:bar" contains ':', which is reserved. If you meant to specify a catalog, use --catalog.`
  - `the catalog name "My-Repo" must be lowercase. Try "my-repo" instead.`
  - `the catalog name "foo--bar" contains consecutive '-' characters.`
  - `the label "feature_login" contains '_', which is not allowed. Use '-' or '/' as separators.`
  - `the catalog name is empty.`
  - `the label "..." exceeds the maximum length of 128 characters.`

The error API is the agent's call. Whatever form it takes (enum, struct, thiserror, plain strings), it must produce messages of this quality.

## 6. Where Validation Must Happen

- CLI argument parsing for `--catalog` and `--label` on every command that accepts them.
- Default context file load (`load_default_context` in `src/main.rs`).
- Catalog config validation (`CatalogConfig::validate` in `src/main.rs`).
- Qdrant label-metadata deserialization (`LabelMetadata` in `src/engine/uploader.rs`).
- Crawl-time path assertion (reject any relative path containing `:`, `@`, or `=`).
- JSON schemas — loose constraints only; authoritative validation is in code. Add a comment in each schema referencing the Rust validator.

## 7. Tasks

- [x] 1. **Centralize identifier validation and composition.** The current code has `compute_label_id` in `util.rs` and ad-hoc colon-splitting in `main.rs`. Move validation and composition into one module. Every `label_id` produced in the codebase flows through the single composition function defined here. Internal module layout, function signatures, and error types are the agent's call, subject to the invariants above.

- [x] 2. **Rewrite CLI flag handling in `src/main.rs`.** `--catalog` and `--label` each take a bare validated identifier. The legacy `--label catalog:label` form is rejected with a clear error. The existing `resolve_label_id` function should go away or be rewritten; its current contract (takes a colon-joined string) is wrong. Defaults fill in missing components.

- [x] 3. **Purge the qualified form from user-facing output.** The core confusion is that the tool today parses and prints IDs where it should parse and print names. Every `println!`, `eprintln!`, error string, and `format!` that emits `Label ID:` or `{catalog}:{label}` in user-facing output must be replaced with separate `Catalog:` / `Label:` fields. Known sites in `src/main.rs`: `run_use` around lines 682 and 711–712, `run_crawl_label` around line 1289, similar block around line 1671. Grep for more.

- [x] 4. **Rename `label_name` to `label` everywhere in the codebase.** Source files, struct fields, function parameters, log strings, doc comments, Qdrant payload field references. The only remaining uses of the phrase `label_name` after this change should be in migration notes in CHANGELOG.md. The word `label` must mean the name; the `_id` suffix must mean the qualified form; nothing blurs the two. Note: the Qdrant payload field `LabelMetadata.label_name` is itself renamed to `label` — no compatibility layer. This is a deliberate consequence of the no-backward-compatibility stance.

- [x] 5. **Apply validation at every boundary listed in §6.**

- [x] 6. **Update schemas.** Constrain catalog and label fields to `{ "type": "string", "minLength": 1, "maxLength": 128 }` with at most a trivial shape hint (e.g. `"pattern": "^[^\\s:@+#]+$"` to catch the most common confusions — note `=` is deliberately permitted here since it is allowed in labels). Add a comment in each schema pointing to the Rust validator.

- [x] 7. **Update DESIGN.md.** Revise the "Label-Based Indexing" section to use the strict terminology; `label_id` moves into a "Storage Format" subsection explaining it as internal. Every "Query Interface" example uses two separate flags. Add a concise "Identifier Syntax" subsection that:
   - Gives the regexes and length bounds for `catalog` and `label` as implemented today, matching §4 invariant 5 exactly.
   - Shows brief examples of valid and invalid values, including at least one example using `=` in a label to illustrate the reserved-but-not-interpreted treatment.
   - Documents the full envisioned syntax (typed labels `kind=payload`, `@catalog:label` references, `@catalog:label:path` path references, reserved `+` and `#`) as planned grammar, with a clear note that only bare catalog and label are parsed today.
   - References https://github.com/microsoft/monodex/issues/25 for the full spec.
     Justifies why `@`, `+`, `#` are forbidden in bare identifiers today, and why `=` is permitted but not interpreted: because they are reserved for future grammar described in the linked issue.

- [x] 8. **Update README.md.** Every example uses the two-flag form. No `Label ID` anywhere.

- [x] 9. **Update CHANGELOG.md.** One entry documenting the breaking change and noting that existing collections with non-conforming identifiers must be recreated, and that the `label_name` payload field has been renamed to `label`.

## 8. Acceptance Criteria

Review gate, not an implementation step.

- The string `label_name` appears nowhere in source code — it is fully renamed to `label`. Only CHANGELOG.md retains a reference, documenting the rename.
- `label_id`, `Label ID`, `catalog:label`, `<catalog>:<label>`, and `qualified` appear only in: the validation module, Qdrant storage code actually serializing to or deserializing from `active_label_ids`, or comments explicitly marking an internal-storage usage.
- No user-facing CLI help text, console output, error message, or public doc references the qualified form.
- All `label_id` construction in the codebase goes through the single composition function. No ad-hoc `format!` concatenations.
- `@`, `+`, `#`, `:`, whitespace, and control characters are rejected in both `--catalog` and `--label` input, with messages naming the specific character and, where applicable, pointing to the fix. `=` is rejected in `--catalog` but accepted in `--label` without any special interpretation.
- Every validation path in §6 fails cleanly on malformed input with a message of the quality shown in §5.
- DESIGN.md describes the full envisioned grammar from the linked issue but is clear about what is and isn't implemented today.