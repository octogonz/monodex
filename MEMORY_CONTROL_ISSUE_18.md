## Fix: Deterministic Embedding Memory Control + Safer Incremental Relabeling

### Problem

Two practical issues need to be addressed:

1. **Embedding can consume too much RAM**
   - Parallel embedding creates multiple ONNX model instances
   - Current defaults are not driven by system memory
   - Users on memory-constrained machines can hit OOM failures

2. **Incremental relabeling does redundant work**
   - Existing files may already have the target label
   - Current behavior can still perform expensive per-chunk label updates

This proposal focuses on high-impact improvements with low regression risk.

---

## Part 1: Efficient Incremental Relabeling (Sentinel-Based)

### Goal

Avoid redundant per-chunk labeling work.

### Approach

Use the file sentinel as the authoritative indicator of file-level label presence.

If:

- sentinel exists, and
- `active_label_ids` contains the target label

then:

- skip `add_label_to_file_chunks`
- still mark file as touched

Otherwise:

- perform normal labeling

This optimization must be applied to **both** crawl entry points:

- [x] Update git-commit crawl path to skip relabeling when sentinel already has target label
- [x] Update working-directory crawl path to skip relabeling when sentinel already has target label

The two paths currently contain near-duplicate logic and both must be updated.

---

### Required Invariant

The sentinel must only be labeled **after all other chunks** of the file are labeled.

---

### Implementation Requirements

#### 1. Skip Decision Path: `FileSyncInfo`

The skip decision uses `get_file_sentinel`, whose return type is `FileSyncInfo`.

- [x] Extend `FileSyncInfo` struct to include `active_label_ids: Vec<String>`

This data is needed in the crawl paths to decide whether relabeling can be skipped.

This is a struct/plumbing change, not a new network request: `get_file_sentinel` already fetches enough payload to return this data.

- [x] Wire the updated `FileSyncInfo` through git-commit crawl path
- [x] Wire the updated `FileSyncInfo` through working-directory crawl path

---

#### 2. Explicit Reordering Required in `add_label_to_file_chunks`

Qdrant scroll returns chunks in **arbitrary internal order**.

The current implementation processes chunks in this order, which does **not** guarantee correct sentinel handling.

Therefore, `add_label_to_file_chunks` must:

- [x] Collect all chunks for a file
- [x] Partition them into:
  - non-sentinel chunks
  - sentinel chunk
- [x] Apply updates in two phases:
  1. label all non-sentinel chunks
  2. label the sentinel chunk last

This ordering is required for correctness.

---

#### 3. Sentinel Identification in Scroll Payload

The sentinel must be identified from the scroll payload used inside `add_label_to_file_chunks`.

Preferred signal:

- `file_complete == true`

Alternative (if needed):

- `chunk_ordinal == 1`

- [x] Update scroll payload in `add_label_to_file_chunks` to include `file_complete` (preferred) or `chunk_ordinal`

---

#### 4. Scroll Payload Struct Update (`LabelPayload`)

The current scroll payload struct used by `add_label_to_file_chunks` (`LabelPayload`) only includes:

- `active_label_ids`

- [x] Extend `LabelPayload` to include `file_complete: bool` (preferred) or `chunk_ordinal: usize`

so that the sentinel can be identified and updated last.

This is a separate change from the `FileSyncInfo` update above; both are required.

---

### Result

This ensures:

- successful labeling → sentinel implies full file labeled
- partial failure → sentinel remains unlabeled → safe retry on next run

---

## Part 2: Explicit Embedding Configuration in `config.json`

Add a required `embeddingModel` section to `config.json`:

```json
{
  "embeddingModel": {
    "modelInstances": "auto",
    "threadsPerInstance": "auto"
  }
}
```

Allowed values:

- `"auto"`
- integer >= 1

### Semantics

- `modelInstances`
  - Number of ONNX model instances (sessions)
  - **Primary driver of memory usage**

- `threadsPerInstance`
  - Threads per model instance
  - **CPU tuning only**

### Required Code / Schema Updates

This is a config format change. Update all of the following together:

- [x] Add `EmbeddingModelConfig` struct to `src/main.rs` with `model_instances` and `threads_per_instance` fields
- [x] Add `embedding_model` field to `Config` struct
- [x] Update `schemas/config.schema.json` with `embeddingModel` section
- [x] Update README.md config examples and option documentation
- [x] Add `#[serde(deny_unknown_fields)]` to new struct
- [x] Consider backward compatibility: either make fields optional with defaults, or require migration

---

## Part 3: Bounded Global Upload Accumulation

### Problem

Embedding results are accumulated before upload. Without a size-based limit, this accumulation can grow unnecessarily large in RAM.

---

### Separate Concepts

Two distinct limits must exist:

- `qdrant.maxUploadBytes`
  - external constraint
  - limits size of a single upload request
  - protects Qdrant / network layer

- `maxAccumulatedUploadBytes`
  - internal constraint
  - limits how much upload data Monodex holds in memory
  - protects Monodex process memory

- [x] Add `maxAccumulatedUploadBytes` constant/variable to uploader thread
- [x] Initially set `maxAccumulatedUploadBytes = qdrant.maxUploadBytes`

These must remain separate variables in code, even if they currently share the same value.

---

### Shared Measurement Model

Both limits use the **same unit and the same underlying calculation**:

- serialized JSON size of the upload request

This ensures:

- accumulation and upload operate in the same measurement space
- no mismatch between buffered data and sendable data

---

### Configuration Status

`maxAccumulatedUploadBytes` is:

- **not user-configurable in this change**
- an internal implementation detail

It may be exposed as a config setting in the future if needed.

---

### Architecture

- embedding workers produce results in parallel
- a single uploader thread owns the global accumulation buffer

This ensures the limit is **global**, not per worker.

---

### Flush Conditions

- [x] Flush when time threshold is reached (existing behavior)
- [x] Flush when estimated accumulated serialized bytes >= `maxAccumulatedUploadBytes`

---

### Estimation

- [x] Keep byte estimate in uploader thread alongside global accumulated queue
- [x] Update estimate as items are drained from embedding results channel
- [x] Estimate serialized size using same point/payload shape used for actual upload request
- [x] Estimate does not need to be exact

This estimate should be based on the actual upload structure built in `uploader.rs`, not on raw chunk size alone.

---

### Final Gate

`upload_batch()` remains the final enforcement point:

- constructs actual request
- enforces `qdrant.maxUploadBytes`
- splits if necessary

---

## Part 4: Deterministic `"auto"` Heuristic

`"auto"` must be deterministic for a given machine. It must **not** depend on current system load.

### New Dependency

- [x] Add `sysinfo` crate to `Cargo.toml`

This feature introduces a dependency on the `sysinfo` crate.

It is used to obtain:

- total system memory
- available system memory (for warnings)
- physical CPU core count
- Linux cgroup memory limits (when applicable)

This dependency should be reviewed carefully, especially for containerized/Linux environments.

---

### Inputs (via `sysinfo`)

Use a `System` instance for memory:

- `system.refresh_memory()`
- `system.total_memory()`
- `system.available_memory()` (warning only)

Use CPU count from `System`:

- `System::physical_core_count()` (fallback to logical cores if `None`)

On Linux, also use:

- `system.cgroup_limits()` when present

### Effective Total RAM

- On Linux:
  - `effective_total_ram = min(total_memory, cgroup memory limit)`
- Otherwise:
  - `effective_total_ram = total_memory`

### Constants

```text
PER_INSTANCE_RAM = 2.5 GiB
BASELINE_RESERVE = max(4 GiB, 25% of effective_total_ram)
```

### Sizing Logic

- [x] Implement `compute_auto_embedding_config() -> (u32, u32)` function returning `(model_instances, threads_per_instance)`
- [x] Use `sysinfo` crate to get total memory and physical core count
- [x] Implement Linux cgroup memory limit detection (via `cgroup_limits()`)
- [x] Implement effective_total_ram calculation
- [x] Implement the sizing formula:
  ```text
  usable_ram = effective_total_ram - BASELINE_RESERVE
  ram_limited_instances = floor(usable_ram / PER_INSTANCE_RAM)

  total_cpu_cores = physical_core_count_or_logical_fallback
  cpu_cap = min(4, total_cpu_cores)

  modelInstances_auto = clamp(ram_limited_instances, 1, cpu_cap)
  threadsPerInstance_auto = max(1, total_cpu_cores / modelInstances_auto)
  ```

### Properties

- Stable across runs
- Memory-aware first, CPU-aware second
- Avoids dynamic behavior based on transient conditions

---

## Part 5: Startup Memory Warning

Even with deterministic sizing, Monodex should warn if current conditions are risky.

### Estimate

```text
embeddingRamEstimate =
  (modelInstances × 2.5 GiB) + 0.5 GiB
```

### Output

- [x] Before embedding, print memory status:
  ```text
  Currently available system RAM: <X.X> GB
  Estimated embedding RAM usage: <Y.Y> GB
  ```

- [x] If `embeddingRamEstimate > available_memory`, print:
  ```text
  🚨 Warning: estimate exceeds available RAM by <Z>%.
  Consider adjusting "embeddingModel.modelInstances" or "embeddingModel.threadsPerInstance" in config.json
  ```

### Failure Messaging

- [ ] Where Monodex can detect a recoverable memory/allocation failure, the error message should point users to `embeddingModel.modelInstances` and `embeddingModel.threadsPerInstance` with a suggestion to start with `modelInstances = 1`

Do not assume Monodex can always print this message after a hard OS-level OOM kill.

---

## Part 6: Non-Goals

This change does not include:

- same-commit early-exit optimization
- GPU execution support
- embedding model variants
- CLI flags for embedding tuning

### Rationale

- reduces regression risk
- avoids complex invalidation logic
- keeps implementation easy to reason about

---

## Outcome

After this change:

- embedding memory usage is predictable and bounded
- configuration is explicit and machine-specific
- users receive clear warnings before OOM conditions
- incremental relabeling avoids unnecessary work
- upload buffering is globally bounded and consistent
- system behavior is easier to reason about and verify

---

## Testing Checklist

- [x] Test `"auto"` config on macOS (no cgroup)
  - Result: Auto-detected 4 instances × 3 threads on 12-core machine
  - Memory warning: 17.8 GB available, 10.5 GB estimated usage
- [ ] Test `"auto"` config on Linux with cgroup limits
- [x] Test explicit integer values for `modelInstances` and `threadsPerInstance`
  - Result: Correctly uses "Using explicit config: 2 instances × 4 threads/instance"
  - Estimated RAM: 5.5 GB (2 × 2.5 GiB + 0.5 GiB overhead)
- [x] Verify memory warning prints when estimate exceeds available
  - Result: Shows "🚨 Warning: estimate exceeds available RAM by 18%" when 8 instances configured
- [x] Verify incremental relabeling skips files that already have the label
  - Result: Shows "Existing files already labeled: 186 (skipping)"
- [x] Verify sentinel is labeled last in `add_label_to_file_chunks`
  - Verified in code: non-sentinel chunks updated first, sentinel updated last
- [ ] Verify upload flush triggers on accumulated bytes threshold
  - Would require large crawl to trigger; code path verified
- [x] End-to-end crawl with new config on sparo catalog
  - Completed successfully with auto-detection and memory warning
