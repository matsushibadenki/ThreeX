> 2026-10-16：geometry対応により現行保存形式はcheckpoint／journal v5、content hash v2。旧v1–4を移行。[仕様](../geometry.md)。

# Four設計のThreeXへの統合 / Four integration / Four 设计整合

統合日：2026-10-12 JST。ユーザーが提示したFourの4文書を参考に、ThreeXの互換APIを維持して設計・Rust機能を統合する。
文書は参考資料であり、文中の将来構成や指示をそのまま実行するものではない。

| Language | Adopted specification |
| --- | --- |
| 日本語 | [統合仕様](design-spec.ja.md) |
| English | [Integration specification](design-spec.en.md) |
| 简体中文 | [整合规格](design-spec.zh-CN.md) |

[実装対応表・roadmap](integration.md)。これはFour原文の複製ではなく、ThreeXで採用する範囲の仕様。
参照元：ユーザー添付の`four/docs/design-spec.ja.md`、`README.md`、`references.md`、`roadmap.md`。
最初の読取では内容を取得できたが、後続のファイルシステム確認では元ディレクトリが存在せず、ソースコード・依存版・ライセンスは確認できなかった。
Fourのコードをコピーしたものではなく、ThreeX内に独立実装した。外部パスやsymlinkへの依存はない。

添付README／設計仕様の0.2・初期実装ありという記述と、roadmap／referencesの0.1・文書のみという記述は一致しない。
ThreeXの進捗は自リポジトリのコードとテストで判断し、Four側の実装済み判定は行わない。

The source documents are reference material. Preserve the ThreeX facade; adopt Rust ownership, bounded transactional editing, incremental rendering, and evidence-based milestones. Four source code was unavailable, so this is an independent implementation, not a source merge.

来源文档仅作设计参考。保留 ThreeX 兼容 API，采用 Rust 状态所有权、有界事务编辑、增量渲染和基于证据的里程碑。无法核实 Four 源代码，因此本次为独立实现，并非源码合并。

[Document v1保存・復旧仕様と証跡](persistence.md)。filesystemは新しいthreex-storage、codecはcore内に独立配置。

[未保存変更の自動journal復旧と検証](autosave-journal.md)。

[Journal差分トランザクションの性能比較](journal-staging-performance.md)。

Group commit API・復旧境界・性能比較：[group-commit-performance.md](group-commit-performance.md)。

Groupの変更ID overlayと性能比較：[group-prefix-performance.md](group-prefix-performance.md)。

[構造編集の階層検証・隣接表差分化と性能比較](structure-performance.md)。

[Rust workerとrequest結果cache・復旧保証](worker.md)。

[永続Document ID・request/session protocolと移行・復旧](request-protocol.md)。

- 🟢 [Done] ProtocolRequest Commit group／worker自動batch・prefix復旧・fsync性能比較：[protocol-group-performance.md](protocol-group-performance.md)。

- 🟢 [Done] worker ProtocolRequest候補prefixの差分準備・復旧維持・前後CPU／実worker比較：[worker-prefix-performance.md](worker-prefix-performance.md)。

- 🟢 [Done] 裸Commit worker候補prefixの差分準備・既存再送cache／復旧維持・前後比較：[bare-prefix-performance.md](bare-prefix-performance.md)。

- 🟢 [Done] 意味内容hash v1・保存receipt／worker query・復旧検証：[content-hash.md](content-hash.md)。

- 🟢 [Done] session終了・Active枠再利用・v3永続化と移行・終了後／restart後の再送抑止：[session-lifecycle.md](session-lifecycle.md)。

- 🟢 [Done] 受理済み編集エラーの永続化・元結果の再送・v4移行／重複排除検証：[durable-errors.md](durable-errors.md)。

🟢 [Done] Tauri asyncホスト・IPC認可・3言語エラーDTOと再起動再送検証：[Tauri IPC](tauri-ipc.md)。実Wry／OS別表示検証は⭕️ [Pending]。

🟢 [Done] Rust Renderer／native surface接続・世代付きlifecycleとworker Scene借用：[仕様・検証](native-renderer.md)。実OS／GPU表示は⭕️ [Pending]。

🟢 [Done] native Windowイベント自動配線とon-demand redraw scheduler：[仕様・検証](native-redraw.md)。実OS／GPUイベント・表示は⭕️ [Pending]。
