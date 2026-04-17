# Monodex Identifier & Reference Syntax Proposal

## 1. Motivating Examples (Realistic, Exhaustive)

### 1.1 Single Repo, Typical Usage

```bash
--catalog my-repo
--label main
--path main:src/index.ts
--path release/v1.2.3:src/index.ts
```

Labels:

```
main
feature/login-flow
release/v1.2.3
```

---

### 1.2 Multiple Repos (Cross-Catalog)

```bash
--path @frontend:main:src/app.ts
--path @backend:main:src/server.ts
```

---

### 1.3 Commit-Specific Queries

```bash
--path @my-repo:commit=abc123:src/index.ts
--path @my-repo:commit=def456:src/index.ts
```

---

### 1.4 Working Directory / Local State

```bash
--path @my-repo:local=working-dir:src/index.ts
```

---

### 1.5 Time-Based Snapshots (Automation)

```bash
--path @my-repo:snapshot=2026-04-16T12-00:src/index.ts
```

---

### 1.6 Release Tags

```bash
--path @my-repo:tag=v1.2.3:src/index.ts
```

---

### 1.7 Branch Names with Slashes (Git-like)

```bash
--path @my-repo:branch=feature/login-flow:src/index.ts
--path @my-repo:branch=release/v1.2.3:src/index.ts
```

---

### 1.8 Avoiding Collisions

User creates a branch literally named `commit`:

```bash
--path @my-repo:branch=commit:src/index.ts
--path @my-repo:commit=abc123:src/index.ts
```

These remain unambiguous.

---

### 1.9 Omitted Catalog (Contextual Default)

```bash
--path branch=main:src/index.ts
--path release/v1.2.3:src/index.ts
```

---

### 1.10 Non-Git Data Sources

```bash
--path @github-issues:snapshot=2026-04-16:issue/123
```

---

## 2. Core Model

### Catalog

- Identifies a data source (e.g. repo, issue system)
- Few and stable

### Label

- Identifies a version/snapshot of a catalog
- Many and diverse
- May correspond to:
  - Git branch
  - Git commit
  - Git tag
  - working directory
  - time snapshot
  - external system state

### Path

- Identifies a location within a label

---

## 3. Syntax Overview

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

## 4. Typed Label Form (Key Innovation)

### Structure

```
<kind>=<payload>
```

Examples:

```
branch=main
branch=feature/x
commit=abc123
tag=v1.2.3
local=working-dir
snapshot=2026-04-16T12-00
```

### Properties

- `kind` is a reserved identifier
- `payload` is opaque and may contain `/`
- `=` is the delimiter between machine type and payload
- eliminates ambiguity with user-created branch names

---

## 5. Full Reference Grammar

### Without catalog

```
path
label:path
kind=payload:path
```

### With catalog

```
@catalog:path
@catalog:label
@catalog:kind=payload
@catalog:label:path
@catalog:kind=payload:path
```

---

## 6. Parsing Rules

1. If string starts with `@`, parse catalog first:

   ```
   @catalog:...
   ```

2. Split remaining string on `:` (left-to-right):
   - 1 segment → label or path (based on context)
   - 2 segments → label:path or kind=payload:path
   - 3 segments → catalog:label:path or catalog:kind=payload:path

3. Within a label segment:
   - if `=` present → parse as `kind=payload`
   - else → treat as opaque label

4. `/` is never parsed structurally by Monodex:
   - always part of label or path

---

## 7. CLI Semantics

### `--catalog`

```
catalog
```

### `--label`

```
label
kind=payload
@catalog:label
@catalog:kind=payload
```

### `--path`

```
path
label:path
kind=payload:path
@catalog:path
@catalog:label:path
@catalog:kind=payload:path
```

---

## 8. Identifier Rules

### Forbidden characters (global)

```
: @ = + whitespace control-chars
```

### Allowed characters (recommended)

```
[a-z0-9._/-]
```

---

### Catalog (kebab-case)

```
^[a-z0-9]+(?:-[a-z0-9]+)*$
```

Examples:

```
my-repo
frontend
backend-api
```

---

### Label payload (Git-like)

```
^[a-z0-9]+(?:[./-][a-z0-9]+)*$
```

Examples:

```
main
feature/x
release/v1.2.3
working-dir
repo/sub/feature
```

---

### Kind (typed prefix)

```
^[a-z0-9]+$
```

Examples:

```
branch
commit
tag
local
snapshot
```

---

## 9. Reserved Syntax Budget

Reserved globally:

```
: @ =
```

Reserved for future:

```
+
#
```

---

## 10. Design Outcomes

- No ambiguity between branch names and machine labels
- `/` remains fully usable in labels
- Git naming patterns preserved
- CLI remains shell-safe without quoting
- Future extensions possible without breaking syntax

---

## 11. Key Principle

> Labels remain human/Git-like; machine semantics are layered via `kind=payload` only when needed.
