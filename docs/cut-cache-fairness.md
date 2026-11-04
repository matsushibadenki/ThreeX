# Cut-cache admission / cut cacheの優先度と公平性 / cut缓存优先级与公平性

🟢 [Done] G03-C5

日本語：Rust Rendererの可視THREEXA2 instanceに対し、cache受付を描画前に計画する。
最大256 cut枠のうち128枠はsource順の巡回、残りは詳細要求と階層規模による優先度で割り当てる。
巡回位置は8回のrenderごとに進める。同じ可視候補なら連続描画で再利用でき、
低優先度の候補にも枠が回る。候補の追加・非表示は次のrenderで反映する。
枠を得られないinstanceも従来どおりLODを選択・描画し、訪問・描画budgetとfallbackを適用する。

English: Each render plans admission before selecting cuts. Of at most 256 slots,
128 follow a cyclic source order and the rest favor desired detail and hierarchy size.
The cyclic cursor advances every eight renders, allowing consecutive reuse without
permanent exclusion of low-priority sources. Bypassed instances still select/draw normally;
cache admission is not visibility or permission to bypass a budget.

简体中文：每次绘制前为可见THREEXA2实例规划最多256个cut缓存槽。
128个槽按source循环分配，其余优先分配给需要细化且层级较大的实例。
每8次绘制推进循环位置，兼顾连续命中与低优先级实例的机会。
未获缓存槽的实例仍正常选择LOD并绘制，继续受访问和绘制预算约束。

## Priority and fairness contract

Candidates are current visible hierarchical asset sources from the CPU LOD draw plan.
Priority is `(want_detail << 16) | page_count` (page count at most 4096). This is a
bounded proxy for useful traversal work saved, not a measurement of GPU time or exact
pixel benefit. Equal priorities use cyclic source distance, with no random seed or wall
clock. The remaining slots fill in priority order after deduplicating the fair slots.
At least 128 high-priority slots remain available when there are enough candidates.

For a stable candidate set, cyclic fair slots eventually include every candidate,
regardless of priority or numeric source ID. For 1000 candidates starting at cursor 0,
all are admitted within 64 renders. Candidates disappearing or layout changes can
change/reset that schedule. Sparse IDs and u32 wraparound are supported. Idle/on-demand
views do not request extra redraws merely to advance the cache schedule.

Admission is computed with bounded ordered sets: 128 fair candidates, 256 priority
candidates, and at most 256 admitted sources. The scan is O(n log 256) in visible
hierarchical instances and stores no full-scene candidate array or Instance snapshots.
The cursor stays fixed within an eight-render lease; priority and visibility still
refresh on each render. Stable candidates and priority therefore retain their slots.
Sequential bypass traffic cannot evict protected cuts during the same render.

This guarantees **cache opportunity**, not an equal share of detailed rendering, CPU
visits or triangles. The existing deterministic render order and shared allowances
still decide those. View-driven quality/streaming scheduling is separate work.

## Retired history and invalidation

A retired cut releases its selection vector and keeps its node hysteresis in a separate
history-only FIFO cache of at most 256 sources. Non-admitted selections update this
history cache too. Recent re-admission can recover deep-node hysteresis even when the
cut itself was evicted. Cut entries remain at most 256; total history owners are at
most 512 (256 active plus 256 retired), each bounded by the 4096-node manifest limit.
No page payload, GPU buffers or geometry arrays are retained by these caches.

Retired histories use the same immutable AssetRef/policy checks as active entries.
Scene identity/asset-layout changes clear active and retired histories and reset the
cursor. View/model/residency changes still invalidate cuts normally. Histories older
than the FIFO capacity may be lost and restart conservatively; unbounded retention is
not promised. Pool replacement still resets the whole cache explicitly.

`cut_cache_status()` reports per-render hits, misses, evictions, bypasses and history
restores, plus current active/history-only entry counts. History restore counts a valid
history-only entry recovered, even if no deep node had yet been evaluated. A miss means
cut recomputation, including a bypass; it need not consume visits when the allowance is zero.

## Verification

- Two independent planners choose identical 256-slot sets over 64 renders of 1000
  mixed-priority sources; all sources get an opportunity and high priority retains
  at least 128 slots. Candidate removal, empty views and sparse/wrapped IDs are covered.
- Actual page selection over 300 sources checks protected hits survive sequential
  bypass traffic, all sources eventually retain cuts, zero visit limits remain zero,
  and zero requested draw limits preserve the reported root coverage floor.
- Pressure eviction followed by bypass/re-admission preserves recent deep hysteresis
  in the dead band; history-only storage stays at most 256 under 1000 bypasses.
- Scene replacement clears both history populations. Existing page arrival/eviction,
  view/model invalidation, shared visits, draw-budget rejection, fallback, spatial
  repartitioning and sparse Instance/gather reuse tests remain enabled. The draw/gather
  integration test now uses the production admission path.

⭕️ [Pending] Real Metal rendering, LOD popping and MacBook Air timing require an adapter.
These deterministic cache/accounting tests do not establish GPU speedups.

🟢 [Done] View-driven streaming batches and bounded request budgets: [G04-A](view-streaming.md).

🟠 [Next] Resident-aware coalescing and cross-asset dispatch priority.

Validation: current debug suites pass 231 Rust tests (core 75, renderer 54,
storage 84, Tauri host 18; 9 GPU/performance cases ignored). Renderer was rerun after
the final pressure test was added. JavaScript compatibility passes 13 tests, and
workspace Clippy passes all targets/features with `-D warnings`.
