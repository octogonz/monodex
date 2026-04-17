# Monodex Identifier & Reference Syntax Proposal

## 1. Scope

This document defines the syntax of identifiers and references used by Monodex at the CLI and in storage. It covers:

- **Catalogs**: names Monodex assigns to data sources it indexes.
- **Labels**: names Monodex assigns to versions/snapshots within a catalog.
- **References**: composite strings that locate content across catalog, label, and path.

It does **not** define what paths are allowed inside a catalog. Paths are bytes from external systems (Git trees, working directories, issue systems). Monodex does not own them and must not reject or silently drop them. See §8 for how paths are handled.

---

## 2. Terminology

- **Catalog** — a Monodex-assigned name for a data source. Few and stable. Chosen by the user.
- **Label** — a Monodex-assigned name for a version or snapshot of a catalog (branch, commit, tag, working-directory state, time snapshot). Many and diverse.
- **Path** — a location within a label. The identity of a path is determined by the underlying data source. Monodex does not assign or constrain path syntax.
- **Reference** — a composite string that locates content, composed of zero or more of: catalog, label, path.

Catalogs and labels are **identifiers Monodex owns**. Paths are **external data Monodex indexes**. This distinction is load-bearing; see §8.

---

## 3. Motivating Examples

### 3.1 Typical single-repo usage

```bash
--catalog my-repo
--label main
--path main:src/index.ts
```

### 3.2 Cross-catalog references

```bash
--path @frontend:main:src/app.ts
--path @backend:main:src/server.ts
```

### 3.3 Typed labels disambiguate user-chosen names

A user creates a branch literally named `commit`:

```bash
--path @my-repo:branch=commit:src/index.ts
--path @my-repo:commit=abc123:src/index.ts
```

The typed form makes these unambiguous. Other typed kinds include `tag=v1.2.3`, `local=working-dir`, `snapshot=2026-04-16T12-00`.

### 3.4 Non-Git data sources

```bash
--path @github-issues:snapshot=2026-04-16:issue/123
```

### 3.5 Paths with reserved characters

A user's repo contains a file literally named `weird:file.ts`:

```bash
--path @my-repo:main:src/weird%3Afile.ts
```

See §8 for the encoding rules.

---

## 4. Syntax Overview

### Base forms

```
path
label
catalog
```

### Composite reference forms

```
label:path
kind=payload:path
@catalog:path
@catalog:label
@catalog:kind=payload
@catalog:kind=payload:path
```

---

## 5. Typed Label Form

### Structure

```
<kind>=<payload>
```

Examples: `branch=main`, `commit=abc123`, `tag=v1.2.3`, `local=working-dir`, `snapshot=2026-04-16T12-00`.

`kind` is a reserved identifier; `payload` is opaque and may contain `/`. The typed form eliminates ambiguity with user-created branch names that coincide with reserved kinds.

### Status of `=` Today

The typed form is **reserved grammar**. In the current implementation, `=` is a permitted character in label identifiers but is not parsed or interpreted: `--label branch=main` today yields a label literally named `branch=main`. Users may adopt `kind=payload` as a naming convention in their own automation, and such names will remain valid when the typed form is parsed natively in the future.

---

## 6. Parsing Rules (Planned)

These rules describe the future parser. Today only the bare forms of `--catalog` and `--label` are parsed; composite forms are reserved.

1. If the string starts with `@`, parse catalog first:

   ```
   @catalog:...
   ```

2. Split the remaining string on `:` (left-to-right, respecting path encoding per §8):
   - 1 segment → label or path (based on context)
   - 2 segments → `label:path` or `kind=payload:path`
   - 3 segments → `@catalog:label:path` or `@catalog:kind=payload:path`

3. Within a label segment: if `=` present, parse as `kind=payload`; otherwise treat as opaque.

4. `/` is never parsed structurally; it is always part of a label or a decoded path.

5. Path segments are percent-decoded per §8 before use.

---

## 7. CLI Semantics

### `--catalog`

Accepts only the bare form:

```
catalog
```

### `--label`

Accepts only the bare form today:

```
label
```

Composite forms (`kind=payload`, `@catalog:label`, `@catalog:kind=payload`) are **not accepted** at the CLI yet. `=` is a valid character inside a bare label but is not interpreted.

### `--path`

Accepts only the bare path form today. Planned composite forms:

```
path
label:path
kind=payload:path
@catalog:path
@catalog:label:path
@catalog:kind=payload:path
```

---

## 8. Paths

### 8.1 Principle

Paths are facts about external systems. Monodex does not assign path syntax and must not refuse to index a file because its path contains a character that collides with Monodex's reference grammar.

This rules out two failure modes:

- **Rejection** — refusing to crawl a file because its path contains `:`, `@`, or `=`. Monodex does not control what filenames appear in a Git tree.
- **Silent omission** — skipping such files with a warning. The user gets a crawl reported as "successful" but search results are missing content they expect. Worse than rejection because the failure is invisible.

Both are forbidden.

### 8.2 Storage

Paths are stored verbatim. No normalization, rewriting, or character substitution. The path field round-trips bit-for-bit with what the data source reported.

### 8.3 Encoding at Reference Boundaries

When a path appears inside a reference string — any context where it is concatenated with grammar characters — it is **percent-encoded per RFC 3986**.

Characters that **must** be encoded in a path within a reference:

- Grammar-reserved: `:`, `@`, `=`, `+`, `#`
- The escape character itself: `%`
- Whitespace and control characters

`/` is **not** encoded. It is a legitimate path separator and does not collide with any reference-grammar character.

Decoding is the inverse: percent-sequences in the path segment of a reference are decoded before lookup. Storage still holds the decoded form.

Percent-encoding was chosen over backslash or quote-based escaping because it survives shells, JSON, and YAML without re-escaping, and it keeps ordinary paths mostly readable.

### 8.4 Examples

| Stored path         | In a reference                      |
| ------------------- | ----------------------------------- |
| `src/index.ts`      | `@my-repo:main:src/index.ts`        |
| `src/weird:file.ts` | `@my-repo:main:src/weird%3Afile.ts` |
| `50%off/notes.md`   | `@my-repo:main:50%25off/notes.md`   |

### 8.5 Breadcrumbs and Display

Breadcrumb rendering and other human-facing displays use the **stored (decoded)** form. Percent-encoding applies only at the boundary where paths are embedded in reference strings. A breadcrumb that says `my-repo / main / src/weird:file.ts` is correct; a breadcrumb that says `src/weird%3Afile.ts` is wrong.

---

## 9. Identifier Rules

### 9.1 Forbidden Characters

Forbidden in bare catalog and label identifiers:

```
:  @  +  #  whitespace  control characters
```

These are reserved for current or future reference grammar. `+` and `#` are not used by the grammar today but are reserved to keep future extensions non-breaking.

`=` is additionally forbidden in catalogs. `=` is permitted in labels but is not interpreted today (§5); a label containing `=` is an opaque identifier.

### 9.2 Catalog (kebab-case)

```
^[a-z0-9]+(?:-[a-z0-9]+)*$
```

- Length 1–64 characters.
- Lowercase ASCII alphanumeric words separated by single `-`.
- No leading, trailing, or consecutive `-`.

Examples: `my-repo`, `frontend`, `backend-api`.

### 9.3 Label (Git-like)

```
^[a-z0-9]+(?:[./=-][a-z0-9]+)*$
```

- Length 1–128 characters.
- Lowercase ASCII alphanumeric words separated by single `.`, `/`, `=`, or `-`.
- No leading, trailing, or consecutive separators.
- `=` is a permitted separator character but is not interpreted as a typed-form delimiter today.

Examples: `main`, `feature/x`, `release/v1.2.3`, `working-dir`, `branch=main`, `repo/sub/feature`.

### 9.4 Kind (typed prefix, planned)

```
^[a-z0-9]+$
```

Applies when the typed form `kind=payload` is parsed natively in a future release. Not enforced today.

Examples: `branch`, `commit`, `tag`, `local`, `snapshot`.

---

## 10. Key Principles

> Catalogs and labels are identifiers Monodex owns and may constrain. Paths are external data Monodex must represent faithfully.
>
> Labels remain human- and Git-like. Machine semantics are layered via `kind=payload` only when needed.
