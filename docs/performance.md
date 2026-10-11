# Incremental command and GPU upload work — 2026-10-11

Historical report for the transactional-overlay milestone. Full world rebuilding
and recursive evaluation described below were subsequently replaced by SoA and
incremental evaluation; current results are in `soa-performance.md`.

Problem: Empty batches and single-object updates copied the complete scene and
validated every hierarchy. Every changed scene uploaded all packed instances.

Root cause: Full map clone in Scene::apply; unconditional full instance write.

Evidence: Same host, release build, 40 sequential samples per operation at
100/1k/10k/100k objects. Raw median/p95/p99/max CSV files are committed in docs.
Synthetic flat cube data, no GPU. No explicit warmup; results are an initial
CPU baseline, not end-to-end frame or production performance measurements.

Changed files: core/src/lib.rs, renderer/src/lib.rs, core/examples/scene_bench.rs.

Change: Stage only touched entities, validate the complete batch before commit,
validate graph only when structure changes. Empty batches return immediately.
Retain the last packed instances and coalesce adjacent changed ranges for writes.

Why it should improve performance: Work for a nonstructural command depends on
changed entities rather than the complete object count. Unchanged instance bytes
are not submitted to the queue.

| 100k objects / p95 ms | Before | After | Difference |
| --- | ---: | ---: | ---: |
| Empty apply | 6.602125 | 0.000042 | -6.602083 |
| One transform apply | 4.318750 | 0.000084 | -4.318666 |
| Full instance generation | 19.957208 | 19.789166 | -0.168042 |

Tiny post-change timings approach timer resolution. Do not infer a precise speedup
ratio. Full instance generation is unchanged and its difference is measurement
variation; it remains the next CPU bottleneck.

Before: 100k packed instances submitted on one transform change = 8,000,000 bytes.
After: If packing order is stable, one changed instance = 80 bytes; verified by
upload-range unit test. This is a transfer plan, not measured hardware GPU bandwidth.
Visibility/structure changes may shift packed indices and affect many ranges.

CPU impact: Applied transform cost reduced; instance generation still O(N).
GPU impact: Less queue write data planned; GPU timing unmeasured.
Memory impact: An additional 80 bytes per uploaded instance retained (8MB at 100k).
Batch staging now stores only touched objects instead of a full scene clone.
I/O impact: No network or disk operations in the hot path.
Energy impact: Unmeasured; no power reduction claim.

Correctness verification: Atomic invalid batches, cycle/orphan rejection, parent
propagation, delete/reparent in the same batch, idle revision, dirty upload merging,
reallocation/full upload, shrinking/empty scene, shared material updates and
Universal-3D transform conversion tests pass. Native GPU test remains ignored
because the environment has no accessible adapter. WASM compilation is pending.

Regression risk: Compact instance ordering can still force large writes. CPU
comparison and retained snapshot cost must be measured on a GPU-enabled machine.
Deep hierarchies still use recursive world computation; SoA and iterative updates
are not implemented. Structural validation can be expensive for deep hierarchies.

Keep / Revert: Keep transactional overlay based on measured CPU reduction. Keep
range upload as a tested transfer reduction; hardware frame-time/energy benefit
remains Pending and should be reassessed with real GPU profiling.

Reproduce CPU baseline (current implementation):

```sh
cargo run --offline --release -p threex-core --example scene_bench
```
