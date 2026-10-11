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
