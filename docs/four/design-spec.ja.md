> 2026-10-15: [Content hash v1 implemented / 意味内容hash実装 / 内容 hash 已实现](content-hash.md)。

# Fourから採用する統合仕様

仕様ID：FX-01〜FX-06。ThreeXの既存バイナリコマンドとJS facadeを維持する。

- FX-01：Rust `Document`が編集履歴とSceneを所有する。Rendererとウインドウは編集正本を所有せず、`scene()`から読取専用借用を取得する。既存の単独Scene利用も可能。
- FX-02：`commit(expected_revision, commands)`はリビジョンが一致する編集だけを原子的に適用。循環・無効ID・NaN等はScene検証を利用。失敗時は履歴・Sceneを変更しない。
- FX-03：Undo/Redoは新しいrevisionとして適用し、元のEntity IDと属性・階層を復元する。新規編集はredoを破棄するが、no-opと失敗は保持する。
- FX-04：既定コマンド上限256 KiB、履歴上限8 MiB。単一編集のforward＋inverseが上限を超えると適用前に拒否。容量不足時は古いundoから破棄。履歴容量はundoとredo合計。これはRust入力上限であり、IPC実装を意味しない。
- FX-05：`snapshot()`は明示要求時にCPU上の不変読取用値を複製し、後続編集の影響を受けない。毎frameの呼出しや全量IPC送信は行わない。描画は既存の差分journalと借用Instanceを使う。型付きエラーはホストで翻訳する。
- FX-06：Tauri surface、ドッキング、GPU能力照会、再送重複排除は後続。v1永続化はpersistence.mdの範囲で実装。DocumentId・AssetId・世代ハンドルは未導入。Object IDは既存u32で文書内に限定。JS生成IDは再利用しないがUndoは元IDを復元する。

FourのD-01（互換APIを初期目標にしない）はThreeXへ適用しない。Documentは編集正本と派生Sceneを一つのRust所有者で扱う初期段階であり、独立EvaluationWorld／GPU Sceneの全面分離は後続。
v1保存・履歴復元・checkpoint世代のクラッシュ復旧は実装。内容hash・非同期評価jobの古い結果拒否・スナップショット共有方式は未実装。
性能目標や他エンジン比較を達成済みとは扱わない。ブラウザWASM／GPU実描画はPending。

2026-10-12更新：バージョン付きcheckpoint保存・履歴復元・世代復旧を実装。[保存仕様](persistence.md)。移行・永続DocumentId・意味内容hashは後続。

2026-10-12: 自動journalと未保存transactionの復旧を実装。移行・永続IDは後続。 [Journal](autosave-journal.md)。

journalは全Document複製なしの差分prepare/applyを採用。 [Performance](journal-staging-performance.md)。

通常編集の明示group commitを実装。個別Undo境界を維持し、一度のsync後に公開する。 [Group commit](group-commit-performance.md)。

Groupの累積prefix検証を変更ID overlayへ差分化し、復旧契約を維持。 [Performance](group-prefix-performance.md)。

構造編集の検証・隣接表を変更経路と辺の差分へ置換。 [Performance](structure-performance.md)。

2026-10-14：Rust workerと有界queue、時間／件数batching、session内request重複排除を実装。Tauri・Renderer接続と永続request/sessionは後続。 [Worker](worker.md)。

永続Document IDとrequest/session v1、checkpoint／journal v2移行、再起動後の再送抑止を実装。Tauri IPCは後続。 [Protocol](request-protocol.md)。

ProtocolRequest Commitを最大32件／1 MiBでgroup commitし、workerも自動併合する。個別receipt・Undo・正常prefix復旧・restart後の重複排除を維持。[同期回数・性能](protocol-group-performance.md)。

workerのProtocolRequest候補は受理済み差分を保持して追加候補のみを検証し、その準備結果をjournalへ直接渡す。[性能・復旧検証](worker-prefix-performance.md)。

裸Commitのworker候補も差分準備し、prepared groupを直接journalへ渡す。live-worker cache／復旧境界は維持。[性能・検証](bare-prefix-performance.md)。

意味内容hash v1を実装。Document／snapshot・保存receipt・worker明示queryに対応し、既存のrevision／request guardを維持。[仕様と検証](content-hash.md)。

session lifecycleを実装。明示終了・Active枠再利用と有界終了記録をcheckpoint／journal v3へ永続化し、旧v1／v2移行と終了後／restart後の再送抑止を検証。[仕様](session-lifecycle.md)。
