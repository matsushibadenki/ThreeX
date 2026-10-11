# ThreeX implementation roadmap

🟢 [Done] Cargo workspace: core / renderer / wasm; JavaScript facade.
🟢 [Done] Scene hierarchy, XYZ transforms, inherited visibility, entity deletion.
🟢 [Done] Binary batched commands, complete validation before mutation.
🟢 [Done] wgpu cube pipeline, depth testing, shared geometry, instanced draw.
🟢 [Done] Retained GPU buffer capacity; unchanged scene skips instance upload.
🟢 [Done] On-demand example; English / 日本語 / 简体中文 text.
🟢 [Done] Rust and JS contract regression tests.
🟢 [Done] Transactional changed-entity validation; empty batch performs no scene work.
🟢 [Done] Coalesced dirty-range GPU writes, including buffer reallocation handling.
🟢 [Done] SoA transform/visibility/material arrays with private reusable slots.
🟢 [Done] Cached local/world transforms; affected subtree updates and parent-first iterative evaluation.
🟢 [Done] Retained packed instances, in-place transform/color edits, no-op batches skip work.
🟢 [Done] Iterative cycle validation and deep hierarchy regression tests.
🟢 [Done] SoA before/after CPU benchmark, startup/all-dynamic costs, process peak RSS.
🟠 [Next] Generation resource handles.
🟢 [Done] Revision-aware GPU upload ranges; removed full Instance comparison/copy.
🟢 [Done] Bounded dirty journal, skipped-update merging, independent renderer stamps, safe full-upload fallback.
🟢 [Done] Upload-preparation before/after release benchmark and repeated all-dynamic comparisons.
🟢 [Done] Incremental membership packing: append/swap-remove, entity-index lookup, shrink-safe dirty ranges.
🟢 [Done] Visibility/Mesh/entity lifecycle before/after CPU and planned-upload benchmarks.
🟠 [Next] Incremental structural validation/adjacency for entity creation/deletion (currently full hierarchy work).
🟢 [Done] Shared material RGB changes and material replacement mark affected meshes dirty.
🟠 [Next] Full camera behavior, Plane/Sphere geometry.
🟢 [Done] Release CPU scene benchmark at 100 / 1k / 10k / 100k objects, before/after CSV.
🟠 [Next] GPU Scene / compute culling and four-way rendered 100k-cube comparison.
🔴 [Later] Indirect draws, LOD, Hi-Z, Forward+, render graph, PBR.
🔴 [Later] glTF/GLB, Meshopt/KTX2, GPU animation and particles, streaming.
🔴 [Later] Worker/OffscreenCanvas, native/Tauri 2 docking and floating panels.
⭕️ [Pending] Browser WASM/WebGPU visual verification and Apple GPU profiling.

Historical CPU results are in benchmark-before.csv / benchmark-after.csv.
Current SoA measurements are in benchmarks/soa-_.csv and benchmarks/soa-memory-_.json.
Historical transactional-overlay results are in performance.md; current SoA results
and limitations are in soa-performance.md. GPU/power measurements remain pending.

## Verification results

- Native workspace compilation, formatting and Clippy: passed.
- Rust scene/transform/journal tests: 17 passed in debug and release. WGSL validation: 1 passed.
- JS compatibility tests: 4 passed.
- Native GPU test: attempted; Metal adapter NotFound. Explicitly ignored in normal tests.
- WASM: target install failed; wasm-pack not installed. Browser path remains unverified.

## 関連プロジェクトとの協調

- 🟢 [Done] Universal-3Dとの役割・データ境界・互換性の差異をcollaboration.mdに記録。
- 🟢 [Done] Surface Lost/Outdatedへの復旧処理をWASM Runtimeに追加。
- ⭕️ [Pending] Surface復旧経路のWASMビルドと実機確認。

- 🟢 [Done] Universal-3D conversion and world mirror verified through the new incremental SoA path.

Current upload-preparation comparisons and limits: upload-performance.md; raw data: benchmarks/upload-*.csv.

Current membership packing measurements and limitations: packing-performance.md; raw data: benchmarks/packing-*.csv.
