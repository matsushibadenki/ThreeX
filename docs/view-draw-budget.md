# View draw budget / view描画budget / 视图绘制预算

🟢 [Done] G03-C4

日本語：view全体のasset cluster instance数と、通常形状を含むtriangle数に描画budgetを追加した。
Rustの`Renderer::set_view_draw_budget(ViewDrawBudget)`で設定し、既定は65,536 cluster instance／8,000,000 triangles。
全表示asset instanceのresident root（未常駐ならbounds proxy）と、通常形状の描画分を先に予約する。
詳細cutは、残るinstanceの予約を侵食せず両budgetに収まる時だけ丸ごと採用する。
収まらなければ、そのinstanceを丸ごとroot／proxyへ戻す。子の一部を省略しない。

**coverage floor：** 指定値が予約量より小さい場合、有効上限を予約量まで引き上げる。
0でも非表示にはしない。これはcoverage優先の方針であり、指定値の厳密なhard capではない。
`view_draw_status()`でrequested／minimum／effective／drawnを別々に公開する。
実際のdrawnはeffectiveを超えないが、minimumが大きければrequestedを超える。
rootを保持する方針のminimumであり、全assetをproxyへ置き換えた場合の数学的最小量ではない。

English: The view allowance limits asset cluster instances and all submitted triangles,
including primitives. Reserve root/proxy coverage for every visible asset instance and
all primitive triangles before admitting detail. Admit an entire cut or draw its root;
never truncate a coverage cut. Defaults are 65,536 cluster instances and 8 million
triangles. If mandatory coverage exceeds the requested allowance, the effective limit
is raised to that coverage floor and reported explicitly. Zero does not hide instances.

简体中文：视图预算统计资源cluster实例数量和全部提交的三角形（含普通几何体）。
先预留每个可见资源实例的常驻根节点或包围盒代理，以及普通几何体的绘制成本。
详细cut必须整体满足两项预算，否则整体退回根节点，不会只删除部分子节点。
若指定预算低于保留覆盖所需成本，有效上限提高至该成本，并分别报告请求值、最低值、
有效上限及实际绘制量。预算为0时仍保留覆盖，因此不是严格的请求值硬上限。

## Accounting and selection

`DrawCost` uses u64 counts. Triangle cost comes from actual selected GPU mesh index
counts divided by three, including geometry fallbacks and bounds proxies (12 triangles).
An instanced range multiplies triangles by instance count. Cluster cost counts one per
asset page **per instance**, including a root or proxy; a single instanced GPU command
may therefore count many clusters. Ordinary primitives contribute triangles, not asset
clusters. Counts describe submitted geometry, not clipped fragments or rasterized pixels.

The Renderer first charges already-submitted primitives and reserves all asset baselines.
Each instance releases its reservation, then admits the cached/selected cut only if
current drawn + future reservations + this cut fits both effective limits. V1 instanced
ranges are accepted/reverted as a whole range; V2 does this per source instance.
`fallback_instances` counts instances whose candidate was rejected, not all uses of
root/proxy due to residency, SSE or visit limits.

Selection order is the existing deterministic draw order. This is a conservative
coverage-first gate, not a global quality optimizer or partial parent coarsening pass.
Fine candidates are still computed under the existing visit budgets; rejected cuts may
consume CPU visits. Root/proxy reservation and draw planning remain O(visible instances).
No constant frame-time claim is made.

## Cache, history and upload contracts

Budget admission happens **after** cut-cache lookup on every render. A cached fine cut
cannot bypass a newly reduced draw budget. Rejection does not overwrite its cached cut
or deep-node history; increasing the allowance can immediately reuse a cached detail cut.
Effective traversal/draw-per-instance limits remain in the existing cut key. Residency,
view and model invalidation are unchanged. The shared actual-visit allowance remains
independent of the shared submitted-geometry allowance.

This stage adds small Rust configuration/status only. Document ownership, worker
protocols, instance buffers, batching and gather plan/index cache are unchanged.
No reference directory is required to build, run or test this implementation.

## Verification

CPU tests exhaust small combinations of both limits, check future-root reservation,
coverage floors below the minimum, rejection by either limit independently, primitive
triangle costs and whole legacy instanced ranges. An integration test uses generated
resident hierarchy pages, cache hits over repeated zero-budget views, recovery of fine
cuts after increasing budget, deep-node hysteresis after rejection, and proxy fallback.
The same test checks zero unchanged Instance transfer, unchanged bank revision and
resident gather plan reuse. Existing spatial visit/repartitioning and cache invalidation
regressions remain in the renderer suite.

⭕️ [Pending] Actual Metal rendering and MacBook Air timing require a usable adapter.
CPU selection/accounting tests are not GPU image or performance validation.

🟢 [Done] Cache priority/fairness under capacity pressure: [G03-C5](cut-cache-fairness.md).

🟢 [Done] View-driven streaming priority: [G04-A](view-streaming.md). Strict requested limits below the coverage floor would require a separately
specified aggregate representation or deliberate omission policy.

Validation: current debug Rust suite totals 228 tests (core 75, renderer 51,
storage 84, Tauri host 18; 9 GPU/performance cases ignored). JavaScript compatibility
suite passed 13 tests; workspace Clippy passed all targets/features with `-D warnings`.
