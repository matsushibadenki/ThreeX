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
