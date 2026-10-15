# Four設計とThreeX実装の対応

2026-10-12 JST。ThreeXの互換APIを維持するユーザー指定に基づく統合。

| Four要件 | ThreeXへの採用 | 状態 |
| --- | --- | --- |
| R-01 Rust独立コア | coreはwgpu／Tauri非依存 | 🟢 [Done] |
| R-02 Rust状態所有 | Document編集所有者と読取Scene境界 | 🟢 [Done] 初期Rustサービス。Tauri async／IPC接続済み。実Wry検証はPending |
| R-04 差分更新 | SoA、子孫Transform、membership、送信journal | 🟢 [Done] 既存実装を維持 |
| R-07 取引・履歴 | revision検証・Undo/Redo・有界履歴 | 🟢 [Done] メモリ内取引＋v1保存／読込・履歴復旧 |
| R-08 再現計測 | 既存CPUベンチとraw CSV | 🟢 [Done] CPUのみ。Four B-01〜06は未完了 |
| R-09 3言語 | 既存demoと今回のFX仕様3言語 | 🟢 [Done] サービスエラーのUI翻訳は未実装 |
| R-03/05/06/11 | GPU能力・surface・ウインドウ再生成・文書保護 | 🟠 [Next] ホスト接続の設計・実装 |
| R-12 オフライン | coreテストとDocumentサービス | 🟢 [Done] 保存機能は未実装 |

## 統合roadmap

- 🟢 [Done] FX-01〜05のRustサービス、既存Scene差分経路との接続。
- 🟢 [Done] 競合・無効編集・容量拒否・親子削除復元・no-op・分岐・安定IDのテスト。
- 🟢 [Done] version付き保存・読込、世代復旧、IO障害／プロセス終了テスト（persistence.md）。
- 🟢 [Done] 永続Document ID、v1移行、意味内容hash v1。
- 🟢 [Done] Tauriホストの単一書込経路、v1 IPC、request重複排除、3言語エラー翻訳。
- 🔴 [Later] WorkspaceStateレイアウト木、surface移動の取引、独立評価world。
- 🔴 [Later] メッシュ局所編集・GPU culling・streaming。既存roadmapと重複開発しない。
- ⭕️ [Pending] OS別surface、GPU喪失、再ドッキング、WASM／GPU実表示検証。

## API

`threex_core::{Document, DocumentError, DocumentSnapshot}`。
DocumentからSceneへの可変参照は公開しない。既存の64byteコマンドを`commit`へ渡す。
`scene()`の読取借用を既存Rust Rendererへ渡せる。WASM facadeは従来のSceneを引き続き使い、
JSにUndo/Redoを暗黙追加していない。Window再生成等の実機検証は行っていない。

```rust
use threex_core::Document;
let mut document = Document::default();
let mut create = [0u8; 64];
create[0..4].copy_from_slice(&1u32.to_le_bytes());
create[4..8].copy_from_slice(&42u32.to_le_bytes());
let revision = document.commit(0, &create).unwrap();
assert!(document.scene().object(42).is_some());
document.undo(revision).unwrap();
assert!(document.scene().object(42).is_none());
```

履歴は変化したbatchのforwardと、影響IDの編集前属性から作るinverseだけを保存し、Scene全量は複製しない。
読取snapshotは明示的に全量複製するため大規模Sceneでコストがある。履歴上限を超えた過去編集はundo不能。
Document単体はメモリ内。threex-storageによる明示保存で履歴を保持する。未保存変更は保持しない。

検証：`cargo test --offline --workspace`、`cargo test --offline --release -p threex-core`、
`cargo clippy --offline --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、
`node --test tests/compat/scene.test.js`。core 29件＋storage 30件、WGSL 1件、JS 4件が成功。80回の混在編集を全Undo／Redoし、snapshot内容とGPU転送計画の適用結果を検証。既存GPU試験は明示ignored。

保存形式・復旧保証・プラットフォーム制限：[persistence.md](persistence.md)。

- 🟢 [Done] 未保存変更の自動journalと正常transactionまでの復旧：[autosave-journal.md](autosave-journal.md)。
- 🟢 [Done] journalの差分prepare/applyと前後2組の性能比較。
- 🟢 [Done] group commit／障害復旧・fsync回数／性能比較。
- 🟢 [Done] worker接続／時間batching・永続request重複排除。

- 🟢 [Done] 累積prefix再検証を変更ID overlayへ差分化：[group-prefix-performance.md](group-prefix-performance.md)。

- 🟢 [Done] 構造検証・隣接表差分化と親削除＋付け替えの末尾復旧：[structure-performance.md](structure-performance.md)。

- 🟢 [Done] Rust worker／有界queue／session内request重複排除：[worker.md](worker.md)。
- 🟢 [Done] Tauri async境界、永続request/session。
- 🟢 [Done] native Renderer接続と世代付きsurface lifecycle。

- 🟢 [Done] 永続ID／request sessionと再起動後の再送抑止：[request-protocol.md](request-protocol.md)。

- 🟢 [Done] ProtocolRequest Commit group／worker自動batch・prefix復旧・fsync性能比較：[protocol-group-performance.md](protocol-group-performance.md)。

- 🟢 [Done] worker ProtocolRequest候補prefixの差分準備・復旧維持・前後CPU／実worker比較：[worker-prefix-performance.md](worker-prefix-performance.md)。

- 🟢 [Done] 裸Commit worker候補prefixの差分準備・既存再送cache／復旧維持・前後比較：[bare-prefix-performance.md](bare-prefix-performance.md)。

- 🟢 [Done] 意味内容hash v1・保存receipt／worker query・復旧検証：[content-hash.md](content-hash.md)。

- 🟢 [Done] session終了・Active枠再利用・v3永続化と移行・終了後／restart後の再送抑止：[session-lifecycle.md](session-lifecycle.md)。

- 🟢 [Done] 受理済み編集エラーの永続化・元結果の再送・v4移行／重複排除検証：[durable-errors.md](durable-errors.md)。

[IPCの認可・DTO・検証範囲](tauri-ipc.md)。

🟢 [Done] Rust Renderer／native surface接続・世代付きlifecycleとworker Scene借用：[仕様・検証](native-renderer.md)。実OS／GPU表示は⭕️ [Pending]。

🟢 [Done] native Windowイベント自動配線とon-demand redraw scheduler：[仕様・検証](native-redraw.md)。実OS／GPUイベント・表示は⭕️ [Pending]。
