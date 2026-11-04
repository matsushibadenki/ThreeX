# View-wide asset hierarchy budget / view全体の階層budget / 视图层级预算

🟢 [Done] G03-C1

日本語：Rust Rendererの各renderで、THREEXA2の全表示instanceが同じ訪問budgetを共有する。
既定4096、`set_hierarchy_view_visits`で変更でき、0はrootのみ。
instanceごとの上限（既定64訪問／32draw）と共有残量の小さい方で選択し、
`PagePool::select_cut_counted`の実訪問数だけ消費する。
`hierarchy_last_visits`は直近の描画で消費した訪問数を返す。
残量0では詳細判定を呼ばず、resident rootまたはbounds proxyを定数時間で選ぶ。
子のatomic coverage置換、page欠落時の親維持は変更しない。

English: Each render shares a default 4096 asset-tree visit allowance across visible
THREEXA2 instances. Actual visits are charged, with the existing per-instance limits
still applied. Zero remaining visits selects the resident root or bounds proxy without
calling refinement. The last render's visit count is observable through the Rust API.

简体中文：每次绘制的全部可见THREEXA2实例共享默认4096次层级访问预算，
并保留单实例限制。仅扣除实际访问次数，耗尽后使用常驻根节点或包围盒代理，
不再进行细化判定，保持完整覆盖。

This is a traversal-work bound, not a total frame-time, triangle or draw-call bound.
Mandatory root/proxy lookup and drawing remain O(visible instances). Allocation of
root fallback selections remains on cache misses; [G03-C2](lod-cut-cache.md) adds a bounded cut cache. Selection order follows
the existing deterministic draw order: early instances can consume the allowance.
No new camera priority/fairness policy or asynchronous page request is introduced.
V1 selection, Document ownership, instance uploads and gather planning are unchanged.

CPU regression exercises 100 selections with each shared allowance from 0 through 23,
checks actual visits never exceed it, checks zero-budget refinement is never called,
and checks root coverage after exhaustion with fully resident details. The existing
partial-residency, eviction, stale completion, sparse updates and gather-cache assertions
run in the same test. No GPU is needed for these coverage/work-bound checks.

🟢 [Done] Deep-node hysteresis and bounded cut cache: [G03-C2](lod-cut-cache.md).

🟢 [Done] Instance spatial-tree visit limits and repartitioning: [G03-C3](instance-spatial-budget.md).

🟠 [Next] Prioritization/fairness under repeated budget exhaustion.

⭕️ [Pending] Actual Metal rendering and MacBook Air frame-time measurements require
a usable GPU adapter. CPU tests do not establish GPU performance.

Validation: `cargo test --workspace` passed 221 tests (9 ignored GPU/performance
cases); workspace Clippy with all targets/features and `-D warnings` passed.
