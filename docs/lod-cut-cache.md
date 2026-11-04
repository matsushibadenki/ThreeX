# Detailed LOD history and cut cache / 詳細LOD履歴 / 详细LOD历史

🟢 [Done] G03-C2

日本語：Rust Rendererに詳細node単位のhysteresisと有界cut cacheを追加した。
rootは既存のCPU plannerの履歴を使い、詳細nodeは未細分化ならscreen-space errorが
設定閾値を超えた時に細分化する。細分化済みなら閾値×(1−hysteresis ratio)以下で戻る。
既定は2pxで細分化、1.6px以下で戻る。これは細分化の要求履歴であり、
子が未常駐なら従来どおり親を保持する。budget不足で訪問しなかったnodeの履歴は維持する。

English: Deep-node refinement uses a Schmitt trigger; root hysteresis stays in the
existing CPU planner. History records desired refinement, not successful residency.
Missing children and exhausted budgets retain atomic parent coverage. A cache hit
reuses the cut without traversal or bounds assessment and consumes zero actual visits.

简体中文：详细节点使用双阈值历史，默认误差超过2px时细化，降至1.6px以下时退回。
根节点保留现有历史。子页面缺失时保留父节点；缓存命中不遍历层级，不消耗访问预算。

## Identity and limits

Each Rust Renderer owns at most 256 source-slot entries. Each contains one cut
(at most the configured per-instance draw limit, no more than 4096) and a boolean
history map for evaluated deep nodes (at most the manifest's 4096 nodes).
No geometry, Instance snapshot, page payload or GPU index array is copied into this cache.
Only the 16 model-matrix values are stored as key bits; color is deliberately excluded.
[G03-C5](cut-cache-fairness.md) adds visible-source priority admission and cyclic fair slots.
A separate bounded cache retains up to 256 retired source histories. Older histories
may expire; total active plus retired history owners are bounded by 512.

The cut key includes immutable AssetRef, model and VP float bits, physical viewport,
error threshold, hysteresis ratio, root refinement request, effective visit/draw limits,
and PagePool residency revision. Transform/view changes recompute cuts while preserving
history. Asset identity or policy changes reset that entry's history. Scene identity or
asset layout changes clear all entries to prevent packed-slot reuse/undo ABA from
inheriting another instance's history. Asset layout reset is intentionally conservative.

Successful page publication, resident detail eviction, root release and pool clear advance
the residency revision. Pending/failed/cancelled loads cannot change a cut and do not
advance it. Pool replacement clears the cache explicitly. Page arrivals therefore replace
cached proxies/coarse cuts; evictions never reuse obsolete resource handles.
The revision is checked for overflow rather than silently wrapping.

The view allowance counts actual traversals. Cache hits use zero visits; misses use
`min(per-instance allowance, remaining view allowance)`. The effective allowance is part
of the cache key, so a fine cached cut cannot bypass a newly reduced limit. A zero-budget
entry contains only root/proxy coverage. Draw work and mandatory root lookups remain
O(visible instances); this does not bound total frame time or triangle count.

Instance uploads, primitive batching, gather plan/index cache, Document ownership and
worker protocols are unchanged. Nothing is transmitted through JavaScript for this feature.

## Verification

CPU tests cover deep refinement and coarsening through the dead band, unchanged-view
cache hits without assessment, camera/model/viewport/policy/root/draw-limit changes,
page publication and eviction invalidation, proxy fallback, scene identity reset,
256-entry capacity and repeated mixed cached/uncached views under one shared allowance.
The workspace suite also checks existing sparse uploads/gather reuse, partial-residency
coverage, stale completions, durable restart replay and failure recovery.

⭕️ [Pending] Actual Metal image validation, LOD popping and MacBook Air timing require
a usable adapter; CPU cache tests are not GPU measurements.

🟢 [Done] Instance spatial-tree traversal limits and repartitioning: [G03-C3](instance-spatial-budget.md).

🟢 [Done] Priority admission, cyclic fairness and retired history retention: [G03-C5](cut-cache-fairness.md).

🟢 [Done] View-driven streaming request priority: [G04-A](view-streaming.md).

Validation: workspace Rust tests passed 223 cases (9 GPU/performance cases ignored).
Clippy passed for all workspace targets/features with `-D warnings`.
