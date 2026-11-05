# View-dependent streaming / view依存streaming / 视图相关流式请求

🟢 [Done] G04-A

日本語：Rust workerに`WorkerAction::StreamAssets`を追加した。対象surface、最大8件の
entity／asset path、camera／物理viewport／error閾値、共有request／訪問budgetを受け取る。
root要求を先に予約・投入し、metadata取得後はview依存の詳細要求を計画する。
frustum外の枝を除外し、親のscreen-space errorが大きい枝から、子page一式を要求する。
一式がrequest残量に収まらない場合は要求せず、residentな親／bounds proxyを維持する。
新しいbatchは同じtargetの旧streaming jobをcancel扱いにし、遅い旧結果の公開を防ぐ。

English: A trusted Rust `StreamAssets` batch supplies a surface, up to eight entity/path
pairs, camera/physical viewport/error threshold, and one shared request/assessment
allowance. Root requests are admitted first. Once metadata arrives, visible parents
with greater screen-space error get their complete child groups requested first.
Insufficient allowance keeps the parent/proxy. Replacing a target's batch invalidates
old streaming results without reclaiming in-flight loader leases prematurely.

简体中文：Rust worker接收surface、最多8组entity与路径、相机／物理视口／误差阈值，
以及共享请求数与访问次数预算。先提交根页面，再按屏幕误差优先请求可见父节点的完整子页面组。
不足以容纳整组时保留父节点或包围盒代理。新batch替换同target的旧streaming任务，
拒绝迟到结果，正在I/O中的lease在完成后才释放。

## Rust API and ownership

```rust,ignore
WorkerAction::StreamAssets {
    target,
    assets: vec![(entity, asset_path)],
    view: Box::new(threex_renderer::streaming::StreamingView {
        camera: camera_matrix.map(f32::to_bits),
        size: [physical_width, physical_height],
        error_pixels: 2.0_f32.to_bits(),
    }),
    budget: threex_renderer::streaming::RequestBudget {
        requests: 32,
        visits: 256,
    },
}
```

The outcome is `WorkerOutcome::StreamAssets { accepted, state }`, counting accepted
root jobs, not detail pages or guaranteed completion. This is a trusted native Rust
API; no new WebView path/geometry payload IPC is exposed. The host supplies its asset
path catalog and the same view used for drawing when it wants a new streaming plan.
It is not automatically invoked on every redraw. Resident state, Document, Renderer
and guards stay in Rust. Reusing the same worker request ID preserves the existing
worker reply-cache behavior; no Document edit or persistent protocol version changes.
The legacy `LoadAsset { detail }` breadth-first behavior stays available unchanged.

`StreamingView` carries float bits for exact envelope equality. The CPU planner uses
existing conservative `BoundsView` assessment and current Rust Document world matrices.
Invalid/singular views conservatively request detail within the limits. Invalid error
thresholds fall back to 2px. V1 root/detail and V2 hierarchies are both supported.

## Shared limits and priority

Each batch shares a request counter and a bounds-assessment counter across its jobs.
Both worker limits clamp to 4096; defaults are 32 requests and 256 visits. The public
pure planner consumes the caller's supplied limits directly. Root I/O consumes one
request per admitted entity. Duplicate entities in a batch are ignored. A zero request
budget admits no I/O and leaves existing coverage untouched. More than eight input
pairs or a path longer than 8192 encoded bytes is rejected before queue submission.

All root jobs are queued while processing the batch, before their completions can
schedule detail. Within each asset, a max-heap prioritizes parent screen-space error;
equal errors use node ID. Parents are requested before descendants, and child groups
are admitted atomically against the remaining request count. Unknown projection uses
infinite priority conservatively. Assessed nodes consume the shared visit count.
No frustum-culled subtree expands.

Across assets, metadata completion order currently determines which plan spends
remaining detail allowance. Root admission follows input order and existing free lease
capacity. This is not an optimal global cross-asset priority queue. Repeated submissions
of an unchanged view are not needed: each new batch cancels its old plan and may hit
Busy while cancelled I/O still occupies slots. The host should submit on meaningful
view/catalog changes and retry Busy cases deliberately, rather than each frame.

Detail slots are reserved when planning, not refunded on failure/cancellation. Actual
reads can therefore be fewer than reserved requests, never more. Each lease still holds
one page at a time; the existing 2 loader threads, 8 leases, 1MiB/page ceiling, metadata
bounds, hash validation and stale Document/surface checks remain in force. A batch may
reserve up to 4096 reads over time; it does not allocate 4096 payload buffers at once.
The planner uses authenticated manifest metadata only, with at most 4096 tree nodes.
No synchronous disk reads occur on the Document or Renderer lane.

`InspectAssetLoads` adds cumulative `streaming_batches`, `streaming_reserved_requests`
and `streaming_visits`. Reserved requests are planned upper bounds including roots,
not successful uploads. Existing completed/failed/stale/pending and reserved-byte
statistics continue to describe loader work and outstanding leases.

## Coverage, caches and failures

Budget exhaustion only suppresses new requests. It neither evicts resident parents
nor alters the current LOD history, cut cache or draw-budget coverage floor. Uploaded
pages invalidate existing cut keys through the established residency revision. Partial
child arrival keeps the parent until all required children are resident. A failed or
capacity-rejected upload stops that lease's remaining plan; it does not remove the
parent. Request planning does not force geometry/Instance/gather uploads.

A newer batch marks target-local old streaming jobs cancelled, including a new empty
batch. Their in-flight input reservations remain charged until completion; cancelled
results never reach the surface backend. Existing Document guard, Undo ABA, entity
removal, surface replacement and device-failure handling run on the same completion path.
Legacy jobs for explicitly replaced entities are cancelled; unrelated legacy loads are
not changed. No reference directory is used as a dependency or runtime asset path.

## Verification

CPU tests exhaust small request/visit combinations, check complete sibling groups,
shared exhaustion, invisible suppression, V1 behavior and invalid-view fallback. A
controlled hierarchy verifies the higher-SSE parent's children win a limited request
allowance. Worker tests read real THREEXA2 files with two entities under a four-request
shared cap and check root/parent coverage and unchanged Document revision. A gated I/O
test replaces the view while reading, checks the lease remains reserved, then verifies
the old result is rejected without a backend upload.

The existing draw-budget/cache/gather integration now also invokes request planning,
checking unchanged cache hits, hysteresis, root/proxy fallback, zero Instance transfer,
stable bank revisions and gather-plan reuse. Workspace regression tests include native
loader failures/capacity, restart replay deduplication and journal recovery.

⭕️ [Pending] Real Metal image/LOD popping checks and MacBook Air timing need an adapter.
CPU planning and mock-backend delivery do not establish GPU performance.

🟠 [Next] Resident-aware request coalescing and cross-asset priority dispatch, including
catalog/view scheduling and bounded retry. Separate aggregate CPU/GPU residency and
multi-window byte quotas remain later G04 work.

Validation: current debug workspace passes 235 Rust tests (core 75, renderer 56,
storage 86, Tauri host 18; 9 GPU/performance tests ignored). JavaScript compatibility
passes 13 tests; workspace Clippy passes all targets/features with `-D warnings`.

### G04-A1: resident detail filtering

🟢 [Done] Native surfaces expose a conservative, generation-checked residency query.
Streaming jobs remove already-resident detail pages before each subsequent dispatch.
Unknown backends, stale/suspended/failed targets report false; delivery guards remain
responsible for rejecting obsolete results. Legacy breadth-first loads are unchanged.
Requests already reserved by the planner are not refunded, so the batch ceiling remains
conservative. Root metadata is still read on each batch; pending I/O is not coalesced.

日本語：常駐済み詳細pageの再I/Oを省く。surface世代・休止・障害を確認し、
既存のrequest予約は返却しない。root再読込と実行中要求の統合は次工程。
简体中文：跳过已常驻详细页面的重复I/O，保留保守请求预算和surface世代检查。
根页面重读及进行中的请求合并仍是后续工作。

A real-file worker regression repeats the same three-request view: only the root is
read again; its two resident children are skipped. Existing coverage/cache/gather and
stale-view regressions remain enabled.

### G04-A2: root metadata reuse and in-flight batch joining

🟢 [Done] The worker retains up to eight authenticated manifests keyed by exact path
and immutable AssetRef. Root reads reuse this metadata through `open_known`; file length
and requested payload hash are still checked. Root payload I/O is not eliminated.
Eviction is deterministic by key; the cache is process-local and never persists paths.

An exact repeat of the latest streaming batch joins its pending jobs without cancelling
or submitting new reads, reserving additional requests, or resetting its shared budget.
Equality includes the target generation, ordered entity/path list, view bits, clamped
budget and Scene stamp. Changed input follows normal cancellation/guard rules. After
all jobs finish, a new submission is a new plan, using cached metadata and resident-page
filtering. Different targets or partially overlapping batches are not globally merged.
The small outcome reports currently pending joined jobs, not the original total.

日本語：最大8件の認証済みroot metadataをpath＋AssetRefで再利用する。同一の最新batchは
実行中jobへ合流し、request予約を二重消費しない。入力やSceneが変われば従来どおり旧結果を拒否する。
简体中文：按路径与AssetRef复用最多8份认证metadata；完全相同的最新batch复用进行中任务，
不重复占用请求预算。输入或Scene变化后仍拒绝旧结果。

Tests repeat a gated in-flight batch and verify only one read starts; replacing it
still rejects that completion. A completed repeated view verifies known metadata is
used for its root, resident children are skipped and coverage remains intact.

### G04-A3: overlapping view batches and cross-asset root dispatch

🟢 [Done] Compatible partial batches (same target generation, Scene stamp, view and
clamped original budget) retain matching entity/path jobs and share their remaining
allowance. Removed jobs are cancelled; added jobs consume that same allowance, so
repeated overlap cannot refill the request budget. No consumed reservations are refunded.
Incompatible batches still cancel prior work. Current target-local joining remains
separate from cross-surface sharing or deduplication across different entities.

New root admissions bootstrap unknown metadata first, then favor larger projected root
error from cached metadata; outside-frustum roots rank last and equal priorities use
entity ID. Ranking uses the authoritative Document world transforms. Existing queued
I/O is not preempted, and detailed page competition across already-running assets still
depends on completion order. A global reprioritizable detail queue remains unfinished.

日本語：互換な部分重複batchは重複jobと残りbudgetを共有する。追加rootは未知metadata、
投影誤差の順で投入する。投入済みI/Oや詳細pageの全体queueの並べ替えは未実装。
简体中文：兼容的部分重叠batch保留已有任务并共享剩余预算；新根请求优先未知metadata，
随后按投影误差排序。进行中的I/O不抢占，全局详细页面优先队列仍待实现。

The gated real-file test expands one pending entity to a reversed two-entity batch:
only the new root starts, both leases remain charged, and replacement rejects both
late results. Existing shared-budget, coverage, cache/history and gather tests remain.

### G04-A4: cross-asset waiting-page priority queue

🟢 [Done] The two native loader lanes now share a bounded priority queue (at most eight
waiting tasks). Each free lane chooses the highest current priority, preserving FIFO
for ties. Roots bootstrap at infinite priority; streaming detail uses the projected
error of the parent being refined, so exact zero-error leaf pages still inherit useful
priority. Legacy details retain priority zero. Each lease exposes only its next planned
page; this is head-of-plan arbitration, not reordering all undispatched descendants.

Pending tasks can be updated under the queue lock. Batch replacement demotes and marks
obsolete streaming tasks cancelled; when dequeued they return a stale completion without
payload I/O. Compatible retained tasks keep their priority and reservations. Changed
views invalidate old tasks rather than retargeting old guards. Running I/O is never
preempted. The queue does not allocate new requests or refill any shared budget.

日本語：2本のloaderが共有する最大8件の優先queueを追加。空いたlaneはasset横断で
親の投影誤差が大きい詳細pageを選ぶ。同順位はFIFO。旧viewの投入待ち要求は取消・降格し、
payload I/Oなしでstale完了へ返す。実行中I/Oやguardは差し替えない。
简体中文：两个loader共享最多8个等待任务，按待细化父节点的投影误差跨asset选择下一页面，
同优先级保持FIFO。旧视图等待任务取消后不读取payload，运行中的I/O不抢占。

Queue selection tests cover cross-asset priority changes and FIFO ties. Existing gated
batch tests cover retained overlap, cancellation, charged leases and stale rejection.
The complete Rust suite retains request/draw limits, parent/proxy coverage, LOD/cache,
Instance/gather reuse and durable recovery tests. Priority is not a starvation-free
I/O fairness guarantee; sustained root arrivals can delay detail. Aging, deeper
lookahead and independent byte/time budgets remain later work.
