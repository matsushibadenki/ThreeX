# Instance traversal budget and repartitioning / instance空間階層 / 实例空间层级

🟢 [Done] G03-C3

日本語：instance空間階層のqueryにviewごとの訪問budgetを追加した。
既定4096、Rustの`Renderer::set_instance_visit_budget(usize)`で変更でき、0も許容する。
budget到達後の未検査部分木は、格納している連続leaf範囲から全sourceを候補へ戻す。
未検査を不可視と扱わず、既存のinstanceごとのbounds／SSE判定へ渡すためcoverageを失わない。
asset内LODの訪問budgetとは独立している。

English: A per-query instance-tree allowance caps node bounds tests. Unvisited frontier
subtrees conservatively emit all source IDs through stored contiguous leaf spans.
Zero allowance emits every candidate without testing tree bounds. Existing per-instance
bounds/SSE checks still run, so this is not a total frame-time or candidate-count limit.

简体中文：每次查询默认最多访问4096个实例树节点。预算耗尽后，将未检查子树的全部
实例保守地保留为候选，继续进行原有bounds和SSE判定，不会因截断而漏绘。
此预算独立于资源内部LOD预算，不限制候选枚举、最终实例判定或总帧时间。

## Repartitioning and ownership

Ordinary bounds changes refit only the changed leaf and ancestors. Identical world
bounds, including color-only updates, do not accumulate movement. The index records
distinct moved sources in a set bounded by instance count. When the count reaches
`max(32, floor(instance_count / 4))`, the planner rebuilds a median spatial partition
from existing leaf bounds once at the end of the dirty batch. This restores useful
spatial grouping after unrelated instances cross the old partitions; tree height was
already balanced by construction.

The rebuild is synchronous and costs O(n log² n) with the current recursive sorting
builder, with O(n) index storage. It is thresholded, not a bounded-time incremental
rotation algorithm. Less than 32 instances do not automatically repartition. Repeated
movement of one instance does not force full rebuilds. Background or incremental
quality-based maintenance remains a later optimization if profiling requires it.

Sources, packed Instance slots, asset layout stamps, models, page residency and
Document state do not change during repartitioning. LOD histories and cut-cache keys
therefore remain valid for unchanged instances. A moved instance recomputes its cut
through the existing model key. Neither the GPU instance banks nor gather indices are
rebuilt by this maintenance operation.

Changing the visit allowance participates in the CPU planner's view key, so all
instance decisions are reassessed even with an unchanged camera and no dirty edits.
It does not reset root hysteresis or detailed-node histories. Renderer diagnostics
`cpu_lod_status()` report per-sync `spatial_nodes`, `spatial_deferred` (sources emitted
from untested subtrees), and `spatial_rebalanced`. These count different work and must
not be interpreted as GPU timing. When no query runs on an unchanged frame, the reported visit count is zero.

## Verification

- 1024 instances moved across old partitions: exact candidate set is preserved after
  repartitioning and the unrestricted query visits fewer nodes.
- Budgets 0 through 31 and larger values preserve every exact visible/unknown source
  before and after repartitioning, while visits never exceed the configured allowance.
- Unknown bounds, empty tree, identical refits, and no-op second rebalance are checked.
- Planner budget changes at an unchanged view force reassessment without resetting
  root hysteresis. Dirty-batch repartitioning preserves visible count and detail decisions.
- Spatial repartitioning with unchanged source/model/page identity retains a detailed
  cut-cache hit without a new bounds assessment. Existing cache invalidation, fallback,
  sparse upload, gather reuse, worker recovery and restart-dedup tests remain in the suite.

⭕️ [Pending] Metal rendering, LOD popping and MacBook Air frame-time measurements.
CPU traversal tests do not establish GPU performance.

🟢 [Done] View-wide cluster/triangle draw limits with a reported coverage floor: [G03-C4](view-draw-budget.md).

🟢 [Done] Cache priority/fairness under capacity pressure: [G03-C5](cut-cache-fairness.md).

Validation: `cargo test --workspace` passed 225 tests (9 GPU/performance cases ignored),
JavaScript compatibility tests passed 13 cases, and workspace Clippy passed all targets
and features with `-D warnings`. Current-stage tests were run in debug.
