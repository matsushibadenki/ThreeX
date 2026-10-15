> 2026-10-15: [Content hash v1 implemented / 意味内容hash実装 / 内容 hash 已实现](content-hash.md)。

# Adopted Four integration specification

FX-01 through FX-06 preserve the existing binary commands and JS facade.

- FX-01: Rust `Document` owns editing history and Scene. Renderers/windows borrow `scene()` read-only; standalone Scene remains supported.
- FX-02: `commit(expected_revision, commands)` atomically accepts only current revisions. Scene validates cycles, IDs and finite values. Failures preserve scene and history.
- FX-03: Undo/Redo creates new revisions and restores original entity IDs, attributes and hierarchy. New edits clear redo; no-ops and failures preserve it.
- FX-04: Default command limit is 256 KiB; history is 8 MiB including undo and redo. Reject edits whose forward plus inverse exceed the budget before mutation. Evict oldest undo entries when needed. These are Rust input limits, not an implemented IPC protocol.
- FX-05: Explicit `snapshot()` copies CPU values unaffected by subsequent edits. Do not call it per frame or send it wholesale over IPC. Rendering uses retained instances and revision ranges. Hosts translate typed errors.
- FX-06: Tauri surfaces, docking, GPU capabilities and request deduplication remain future work. Version 1 persistence is implemented within the scope of persistence.md. DocumentId, AssetId and generation handles are absent. Existing u32 object IDs are document-local. JS IDs never reuse; undo restores original IDs.

Four D-01 (no initial compatibility goal) is not adopted. The initial owner combines editable semantics and a derived Scene; separate EvaluationWorld/GPU Scene layers remain future work. Version 1 persistence, history restoration and checkpoint-generation crash recovery are implemented. Semantic hashing, stale asynchronous evaluation-job rejection and shared snapshot storage are absent. Performance targets and cross-engine comparisons are not achievements. Browser/GPU execution remains Pending.

2026-10-12 update: Versioned checkpoint persistence, history restoration and generation recovery are implemented; see [persistence](persistence.md). Migrations, permanent DocumentId and semantic hashing remain future work.

2026-10-12: Autosave journaling and unsaved-transaction recovery are implemented. Migrations and permanent IDs remain future work. [Journal](autosave-journal.md)。

Journal staging uses delta prepare/apply with no whole-document clone. [Performance](journal-staging-performance.md)。

Explicit commit groups synchronize once before publication and retain separate undo boundaries. [Group commit](group-commit-performance.md)。

Group prefix validation now uses a changed-ID overlay; recovery semantics are preserved. [Performance](group-prefix-performance.md)。

Structural validation and adjacency now update affected paths and edges only. [Performance](structure-performance.md)。

2026-10-14: Rust worker, bounded queue, time/count batching and live-session request deduplication are implemented. Tauri/renderer wiring and durable request/session identity remain future work. [Worker](worker.md)。

Persistent document identity, request/session v1, checkpoint/journal v2 migration and restart deduplication are implemented. Tauri IPC remains future work. [Protocol](request-protocol.md)。

Protocol Commit groups and worker batching now share one sync with individual receipts/Undo and durable valid-prefix recovery. [Performance and limits](protocol-group-performance.md).

Worker Protocol candidates retain validated deltas and journal their opaque prepared group directly; restart deduplication and recovery are preserved. [CPU and worker measurements](worker-prefix-performance.md).

Bare Commit worker candidates also retain validated deltas and journal prepared groups directly, preserving live-worker replay cache and recovery boundaries. [Performance](bare-prefix-performance.md).

Semantic content hash v1 is implemented for Document/snapshots, captured save receipts and explicit worker queries. Revision/request guards remain unchanged. [Specification](content-hash.md).

Session lifecycle now persists closure/tombstones in checkpoint/journal v3, reclaims active slots, migrates v1/v2 and prevents stale requests from reapplying after close/restart. [Specification](session-lifecycle.md).
