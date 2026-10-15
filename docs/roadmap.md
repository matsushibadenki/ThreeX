# ThreeX implementation roadmap

- 🟢 [Done] Cargo workspace: core / renderer / wasm; JavaScript facade.
- 🟢 [Done] Scene hierarchy, XYZ transforms, inherited visibility, entity deletion.
- 🟢 [Done] Binary batched commands, complete validation before mutation.
- 🟢 [Done] wgpu cube pipeline, depth testing, shared geometry, instanced draw.
- 🟢 [Done] Retained GPU buffer capacity; unchanged scene skips instance upload.
- 🟢 [Done] On-demand example; English / 日本語 / 简体中文 text.
- 🟢 [Done] Rust and JS contract regression tests.
- 🟢 [Done] Transactional changed-entity validation; empty batch performs no scene work.
- 🟢 [Done] Coalesced dirty-range GPU writes, including buffer reallocation handling.
- 🟢 [Done] SoA transform/visibility/material arrays with private reusable slots.
- 🟢 [Done] Cached local/world transforms; affected subtree updates and parent-first iterative evaluation.
- 🟢 [Done] Retained packed instances, in-place transform/color edits, no-op batches skip work.
- 🟢 [Done] Iterative cycle validation and deep hierarchy regression tests.
- 🟢 [Done] SoA before/after CPU benchmark, startup/all-dynamic costs, process peak RSS.
- 🟠 [Next] Generation resource handles.
- 🟢 [Done] Revision-aware GPU upload ranges; removed full Instance comparison/copy.
- 🟢 [Done] Bounded dirty journal, skipped-update merging, independent renderer stamps, safe full-upload fallback.
- 🟢 [Done] Upload-preparation before/after release benchmark and repeated all-dynamic comparisons.
- 🟢 [Done] Incremental membership packing: append/swap-remove, entity-index lookup, shrink-safe dirty ranges.
- 🟢 [Done] Visibility/Mesh/entity lifecycle before/after CPU and planned-upload benchmarks.
- 🟢 [Done] Incremental structural validation and adjacency for create/delete/reparent.
- 🟢 [Done] Shared material RGB changes and material replacement mark affected meshes dirty.
- 🟠 [Next] Full camera behavior, Plane/Sphere geometry.
- 🟢 [Done] Release CPU scene benchmark at 100 / 1k / 10k / 100k objects, before/after CSV.
- 🟠 [Next] GPU Scene / compute culling and four-way rendered 100k-cube comparison.
- 🔴 [Later] Indirect draws, LOD, Hi-Z, Forward+, render graph, PBR.
- 🔴 [Later] glTF/GLB, Meshopt/KTX2, GPU animation and particles, streaming.
- 🔴 [Later] Worker/OffscreenCanvas, native/Tauri 2 docking and floating panels.
- ⭕️ [Pending] Browser WASM/WebGPU visual verification and Apple GPU profiling.

Historical CPU results are in benchmark-before.csv / benchmark-after.csv.
Current SoA measurements are in benchmarks/soa-_.csv and benchmarks/soa-memory-_.json.
Historical transactional-overlay results are in performance.md; current SoA results
and limitations are in soa-performance.md. GPU/power measurements remain pending.

## Verification results

- Native workspace compilation, formatting and Clippy: passed.
- Rust core: 43 passed; storage: 52 passed in debug and release. WGSL validation: 1 passed.
- JS compatibility tests: 4 passed.
- Native GPU test: attempted; Metal adapter NotFound. Explicitly ignored in normal tests.
- WASM: target install failed; wasm-pack not installed. Browser path remains unverified.

## 関連プロジェクトとの協調

- 🟢 [Done] Universal-3Dとの役割・データ境界・互換性の差異をcollaboration.mdに記録。
- 🟢 [Done] Surface Lost/Outdatedへの復旧処理をWASM Runtimeに追加。
- ⭕️ [Pending] Surface復旧経路のWASMビルドと実機確認。

- 🟢 [Done] Universal-3D conversion and world mirror verified through the new incremental SoA path.

Current upload-preparation comparisons and limits: upload-performance.md; raw data: benchmarks/upload-\*.csv.

Current membership packing measurements and limitations: packing-performance.md; raw data: benchmarks/packing-\*.csv.

## Four設計の統合

- 🟢 [Done] ThreeX互換APIを維持してFourのRust所有・取引編集・有界履歴を採用。
- 🟢 [Done] Documentサービス：revision競合検出、Undo/Redo、安定ID復元、明示読取snapshot。
- 🟢 [Done] FX-01〜06の日本語・英語・简体中文仕様と実装対応表（four/README.md）。
- 🟢 [Done] version付きDocument checkpoint、Undo/Redo永続化、世代復旧、障害・プロセス終了テスト（storage 30件）。
- 🟢 [Done] 意味内容hash v1、snapshot／保存receipt／worker明示query、正規化と保存・復旧検証（four/content-hash.md）。
- 🟠 [Next] Tauriホスト接続・IPC認可／エラー翻訳。
- 🔴 [Later] WorkspaceStateと評価World分離、取引的surface再接続。
- ⭕️ [Pending] OS別native surface・ドッキング・GPU喪失の実機検証。

- ⭕️ [Pending] 保存の実電源断・容量不足・Windows／Linux durability検証。

- 🟢 [Done] AutosaveDocument：commit／Undo／Redoの自動journal、末尾の最後の正常transaction復旧。
- 🟢 [Done] journal append／圧縮の中断、全切断位置・破損、process exit・追記再開のテスト。
- 🟢 [Done] journal差分transaction prepare/apply、全Document複製廃止、CPU／fsync込み前後比較。
- 🟢 [Done] bounded group commit、1syncで複数revisionを公開、正常prefix復旧・前後性能比較。
- 🟠 [Next] Tauri async接続。
  最新仕様と制約：four/autosave-journal.md。

最新の差分journal性能と制約：four/journal-staging-performance.md。

Group commit仕様・計測・制約：four/group-commit-performance.md。

- 🟢 [Done] group累積prefixの再検証・再適用を変更ID overlayへ差分化、障害復旧維持、前後性能比較。
- 🟢 [Done] 構造編集の階層検証／隣接表を変更経路・辺の差分へ置換、復旧と性能比較。
  最新のgroup準備仕様・計測：four/group-prefix-performance.md。

最新仕様・性能・残るコスト：four/structure-performance.md。

- 🟢 [Done] Rust Document worker、有界queue、時間／件数batching、session内request結果cache・再送拒否。
- 🟢 [Done] worker7テスト、3言語error、headless実行例。
- 🟠 [Next] Tauri async／Renderer接続。
仕様と保証範囲：four/worker.md。

- 🟢 [Done] 永続Document ID、checkpoint／journal v2とv1移行、request/session protocol v1。
- 🟢 [Done] 編集＋receiptの原子的記録、restart／Undo／圧縮後の再送拒否、watermarkと期限切れ。
- 🟠 [Next] 永続エラー結果、Tauri IPC接続。
仕様・保証範囲：four/request-protocol.md。

- 🟢 [Done] ProtocolRequest Commitのgroup commit、worker自動併合、1syncで編集＋receiptを公開。
- 🟢 [Done] restart／正常prefix復旧・再送抑止、no-op圧縮中断、fsync回数と前後2組の性能比較。
仕様・実測・制約：[four/protocol-group-performance.md](four/protocol-group-performance.md)。

- 🟢 [Done] worker ProtocolRequest候補prefixの差分検証、受理済み準備結果の直接journal送信。
- 🟢 [Done] 不正候補後のprefix維持・restart重複排除・障害復旧、候補CPU／実workerの前後比較。
最新の仕様・性能：[four/worker-prefix-performance.md](four/worker-prefix-performance.md)。

- 🟢 [Done] 裸Commit worker候補prefixの差分検証、prepared group直接journal送信。
- 🟢 [Done] no-op／transport再送cache／障害復旧維持、候補CPU・実worker前後比較。
最新仕様・実測：[four/bare-prefix-performance.md](four/bare-prefix-performance.md)。

- 🟢 [Done] session lifecycle：明示終了・Active枠再利用・永続終了記録、checkpoint／journal v3とv1／v2移行。
- 🟢 [Done] 終了後／restart／receipt淘汰後の再送抑止、書込／圧縮中断・process exit復旧。
仕様・上限・検証：[four/session-lifecycle.md](four/session-lifecycle.md)。
