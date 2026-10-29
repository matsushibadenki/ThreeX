# ThreeX

Rust + WASM + wgpu prototype of the Three.js-compatible engine described in docs.
This is an incremental rendering prototype, not the complete engine.

## Run

Install Rust's `wasm32-unknown-unknown` target and `wasm-pack`, then:

```sh
npm run build:wasm
npm run dev
```

Open `http://localhost:8080/apps/examples/` in a browser with WebGPU enabled.
The demo renders on resize or interaction; there is no idle animation loop.

```sh
cargo test --offline
npm test
```

## Implemented contract

Scene, Group, Object3D, Mesh, BoxGeometry, PlaneGeometry, SphereGeometry, MeshBasicMaterial,
PerspectiveCamera (rotation, parent TRS, zoom and world-space lookAt), WebGPURenderer.
Transforms use XYZ Euler radians. Box dimensions are combined with object scale.
Call `render()` after changing transforms or visibility. Structural changes reconcile
entities; transform changes send only dirty objects. Runtime state and GPU buffers
live in Rust. Geometry is shared; cubes render in one instanced indexed draw.

Binary records are 64 bytes, little endian: opcode u32, entity u32, twelve f32
(position, rotation, scale, RGB), flags u32 (visible=1, mesh=2, geometry=bits 2–3), parent u32.
Opcodes: create=1, update=2, delete=3. Entity zero is reserved. JS IDs never reuse.
Batches are validated atomically; hierarchy cycles and missing parents are rejected.

## Current limits

Material RGB color changes and material replacement are tracked, including shared
materials. Geometry is immutable; replacing mesh.geometry marks only that mesh dirty. Lighting/PBR, GPU culling, resource pools,
asset loading and the complete native application UI/docking are not implemented.
The Rust scene stores transforms and render attributes in SoA arrays, with a
transactional changed-entity overlay and cached world matrices. Changes evaluate
only the affected subtree. Color changes do not evaluate matrices. Cached packed
instances are updated in place; visibility/mesh-membership changes append or swap-remove
only affected entries. Dense render indices are transient: use `instance_index(id)`
to resolve an entity, and never persist a packed index or assume ID-sorted draws.
Structural batches validate changed ancestor paths and deleted-parent references.
Adjacency attaches/detaches only changed edges using child-position reverse indices.
The renderer requests revision-aware dirty ranges and borrows only their Instance
slices for queue writes; it does not compare or retain a full Instance snapshot.
A bounded journal supports skipped updates and independent renderers, including
shrinking membership. Expired history, scene switching, buffer reallocation, and
excessively fragmented changes require a full upload.
Internal slots are reused but never exposed. Generation-tagged resource handles
are not implemented; JS entity IDs still never reuse.
Surface Lost/Outdated is retried once; GPU device loss recovery and large scene
capacity checks are future work.

## Verification in this development environment

Rust scene/transform/journal tests (16), WGSL validation (1), JS contract tests (4)
and workspace Clippy with warnings denied passed. The Metal adapter was unavailable
in the execution environment. The GPU integration test is explicitly ignored by
default; run `cargo test -p threex-renderer -- --ignored` on a GPU-enabled host.
WASM target installation failed and wasm-pack is unavailable, so browser compilation
and rendered output are pending. Native `cargo check` does not check the wasm-only module.

## Rust data access and performance

`Scene::instances()` now returns a borrowed `&[Instance]` from the retained cache,
not a newly allocated `Vec`. Copy with `.to_vec()` only when independent ownership
is necessary. `object()` and `world_transform()` return read-only value snapshots.
`update_stats()` exposes work counts for the last successful batch. No-op batches
do not increment scene revision. Invalid batches leave state and stats unchanged.

CPU before/after comparisons, startup/all-dynamic costs, memory measurements,
and reproduction steps are documented in `docs/soa-performance.md`.

```sh
cargo run --offline --release -p threex-core --example transform_bench
cargo run --offline --release -p threex-core --example transform_cost_bench
```

GPU-upload CPU preparation results are documented in `docs/upload-performance.md`.
Incremental membership packing results and order contract: `docs/packing-performance.md`.
`Scene::instance_updates_since(previous_stamp, force_full)` returns a new stamp
and coalesced ranges. Enqueue all ranges from `Scene::instances()` before retaining
the new stamp; failed attempts can retry from the same old stamp. Each renderer
keeps its own stamp. A fresh/recreated GPU buffer must pass `force_full = true`.
The former renderer `upload_ranges` byte-comparison helper was removed.

```sh
cargo run --offline --release -p threex-core --example upload_bench
```

## Four design integration

The existing JS facade remains compatible. Optional Rust `Document` owns editing
history above Scene, checks expected revisions, and supports bounded Undo/Redo
with stable entity IDs. Renderers borrow `document.scene()`; windows do not own
the document. Versioned persistence and recovery are available in `threex-storage`; Tauri host
wiring remains future work. See [storage guarantees and tests](docs/four/persistence.md).

Adopted scope, three-language specifications and verification:
[Four integration](docs/four/README.md). No external Four or Universal-3D path
dependency is required. This is an independent implementation from supplied design
documents; Four source code was unavailable.

Rust `threex_storage::AutosaveDocument` journals accepted commits, Undo and Redo
automatically and recovers the last complete transaction from an interrupted or
corrupt suffix. Use this owner for durable edits; standalone Document remains
in-memory. See [autosave guarantees and API](docs/four/autosave-journal.md).
Tauri/WASM host wiring is not implemented.

Journal preparation now retains only changed objects and validates before IO.
[Before/after CPU and sync measurements](docs/four/journal-staging-performance.md).

Group commit API・復旧境界・性能比較：[docs/four/group-commit-performance.md](docs/four/group-commit-performance.md)。

Groupの変更ID overlayと性能比較：[docs/four/group-prefix-performance.md](docs/four/group-prefix-performance.md)。

[Incremental structural validation and adjacency performance](docs/four/structure-performance.md).

Rust `threex_storage::JournalWorker` owns the document/journal on a dedicated
thread. Window-independent clients submit bounded requests; compatible edits
are grouped, and duplicate IDs return cached results within the live session.
See [worker API and limits](docs/four/worker.md). Tauri/renderer wiring remains future work.

Durable `ProtocolRequest` uses persistent document/session identity and sequence
receipts. Restart retries return the original revision without reapplying; expired
receipts are rejected using persistent watermarks. Legacy bare worker actions keep
their live-session contract. [Protocol and migration guarantees](docs/four/request-protocol.md).

Protocol Commit envelopes support `AutosaveDocument::request_group` and automatic
worker batching: one journal sync, separate receipts/Undo boundaries, valid-prefix
recovery and durable restart deduplication. [Benchmarks and limits](docs/four/protocol-group-performance.md).

- 🟢 [Done] worker ProtocolRequest候補prefixの差分準備・復旧維持・前後CPU／実worker比較：[docs/four/worker-prefix-performance.md](docs/four/worker-prefix-performance.md)。

- 🟢 [Done] 裸Commit worker候補prefixの差分準備・既存再送cache／復旧維持・前後比較：[docs/four/bare-prefix-performance.md](docs/four/bare-prefix-performance.md)。

- 🟢 [Done] 意味内容hash v1・保存receipt／worker query・復旧検証：[docs/four/content-hash.md](docs/four/content-hash.md)。

- 🟢 [Done] session終了・Active枠再利用・v3永続化と移行・終了後／restart後の再送抑止：[docs/four/session-lifecycle.md](docs/four/session-lifecycle.md)。

- 🟢 [Done] 受理済み編集エラーの永続化・元結果の再送・v4移行／重複排除検証：[docs/four/durable-errors.md](docs/four/durable-errors.md)。

🟢 [Done] Tauri asyncホスト・IPC認可・3言語エラーDTOと再起動再送検証：[Tauri IPC](docs/four/tauri-ipc.md)。実Wry／OS別表示検証は⭕️ [Pending]。

🟢 [Done] Rust Renderer／native surface接続・世代付きlifecycleとworker Scene借用：[仕様・検証](docs/four/native-renderer.md)。実OS／GPU表示は⭕️ [Pending]。

🟢 [Done] native Windowイベント自動配線とon-demand redraw scheduler：[仕様・検証](docs/four/native-redraw.md)。実OS／GPUイベント・表示は⭕️ [Pending]。

Camera contracts, Rust computation and pending WASM verification: [docs/camera.md](docs/camera.md).

[Primitive geometry, incremental mixed draws and subdivisions](docs/geometry.md).
[Persistent asset references and worker guards](docs/assets.md): checkpoint/journal v7 (reads v1–6), content hash schema v3.

[Minimal asset pages, coarse LOD and bounded residency](docs/asset-pages.md): verified Rust seek/read and Renderer APIs; [native loader dispatch and target-specific redraw](docs/native-asset-loader.md) are implemented, GPU display verification is Pending.

[CPU bounds/frustum and screen-space LOD selection](docs/cpu-lod.md): per-instance hysteresis, root/proxy fallback, sparse bounds updates and resident gather index reuse are implemented. Hierarchical traversal follows in G03-B; actual GPU verification remains Pending.

[Independent asset and instance hierarchies](docs/hierarchies.md): exact cluster partitioning, atomic coverage cuts and sparse instance bounds refits; [boundary-preserving parent generation](docs/parent-simplification.md) is implemented. Multilevel page persistence and background generation remain G03-B2b.
