> 現行のsession上限・終了・保存形式v3は[session lifecycle](session-lifecycle.md)を参照。以下の性能／導入記録は計測当時の条件。

> 2026-10-15：worker候補prefix準備も差分化。[最新の候補CPU／実worker性能](worker-prefix-performance.md)。この文書の旧group同期比較は保持。

# ProtocolRequest group commit

2026-10-14 JST。ProtocolRequestのCommit envelopeを最大32件／command合計1 MiBでまとめ、
新しい編集＋receiptのレコードを追記してからjournalを1回syncする。全体の検証が成功して同期するまで
Rust Documentもsession watermarkも公開しない。空group／再送だけのgroupは追記・同期しない。

## APIと復旧境界

```rust
let receipts = owner.request_group(&[&first, &second, &retry])?;
```

返却順は入力順。各receiptに元のrevisionとreplayedを返す。バッチ内で同じ成功キーを再送しても
レコードを重複追加しない。no-op成功はrevisionを増やさないがreceiptを1レコードとして永続化する。
各編集のUndo境界、Document ID、sessionの連番、64session／64receipt／8 MiBの保持制限を維持。
バッチ途中のreceipt淘汰も逐次実行と同じ意味で検証する。未保持の過去連番はExpiredで拒否し、再適用しない。

coreは既存receipt payloadを借用し、小さなsession表と変更ID overlayで検証する。
全Document／履歴／既存receipt payloadを複製しない。不透明な準備結果をScene identity・revisionと
request watermark世代に結び付け、no-op後の古い準備結果も拒否する。
明示groupは不正なenvelopeを含めば全体をIO前に拒否する。journal v2の形式変更は不要。
Undo／Redoは単独requestで処理する境界であり、このCommit group APIには混在させない。

workerは時間／件数window内の連続したProtocol Commitを自動batchする。
同じenvelopeの再送も併合可能。互換性のない候補は次の処理へ回し、先行する正常requestを拒否しない。
transport IDの既存cache／順序制約を維持する。Save、Undo／Redo、read、裸actionは併合境界。
バッチの各応答は個別の適用revisionと同期済みバッチ末尾の現在stateを持つ。
workerの候補拡張は現在受理済みprefixを保持して新規候補だけを準備する。[差分準備](worker-prefix-performance.md)。

障害時は最後の完全で妥当なレコードまでを採用し、編集とreceiptを同時に復旧する。
group全体のdisk原子性は保証しない。途中の正常prefixが復旧されれば、そのenvelopeの再送は元のrevisionを返し、
残りだけを適用する。応答前に終了しても同じ境界を使う。同じownerで書込失敗後に再試行すると未確認suffixを置換する。
checkpoint保存／journal圧縮も既存のreceipt・watermark保存を維持し、no-op groupの圧縮中断を追加検証した。

## 同期回数と性能

同じnative Rust benchmark、release／thin LTO、aarch64 macOS 27.0.1、Rust 1.99.0。
既存100／10,000 Objectに64byteのCreate requestを1／8／32件追加する。
各caseは4warmup＋24sample、before→afterを2組測定。表は2runのmedianの中央値であり、
48sampleを合算した分布ではない。API呼出しからjournal追記・sync・Document適用までを含む。
初期配置、checkpoint、envelope生成、worker待ち時間、復旧は計測外。

| 既存Object | request数 | before ms | after ms | 比率 | journal sync呼出し |
|---:|---:|---:|---:|---:|---:|
| 100 | 1 | 4.406 | 4.758 | 0.93× | 1 → 1 |
| 100 | 8 | 37.984 | 4.837 | 7.85× | 8 → 1 |
| 100 | 32 | 151.227 | 5.032 | 30.05× | 32 → 1 |
| 10,000 | 1 | 4.861 | 4.852 | 1.00× | 1 → 1 |
| 10,000 | 8 | 37.355 | 4.491 | 8.32× | 8 → 1 |
| 10,000 | 32 | 153.305 | 4.350 | 35.24× | 32 → 1 |

同期直後のhookで単独経路1／8／32回→group 1回、再送のみ0回を別途テストした。
CSVのexpected_fsyncsはこのjournal file.sync_allの呼出し数を示し、OS全体のsyscallをトレースした値ではない。
8件は約8倍、32件は約30〜35倍。1件は同期回数が同じで、測定揺らぎと準備コストがあり改善を主張しない。
24sampleなのでp99は観測最大値。生データにp95／p99／maxを残している。
この結果からworkerのqueue遅延・大きいcommand・別filesystemの性能は推定しない。

- [実装前ソース](../benchmarks/protocol-group-before.zip)
- [before 1](../benchmarks/protocol-group-before-1.csv) / [after 1](../benchmarks/protocol-group-after-1.csv)
- [before 2](../benchmarks/protocol-group-before-2.csv) / [after 2](../benchmarks/protocol-group-after-2.csv)
- [比較](../benchmarks/protocol-group-comparison.csv) / [測定条件](../benchmarks/protocol-group-metadata.json)

baseline zipを一時directoryに展開し、下記の1行目をfeatureなしで実行すると単独requestのbeforeになる。
現行codeはfeature有無で同じrequest workloadの単独／group経路を切り替えられる。

```sh
cargo run --offline --release -p threex-storage --example protocol_group_bench
cargo run --offline --release -p threex-storage --example protocol_group_bench --features protocol_group
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

## 検証と次工程

追加12テスト：core3件＋storage9件。バッチ内再送・no-op・複数session・依存する親子編集、
逐次実行とのcheckpoint／Undo履歴一致、restart・保存／圧縮・Undo後の再送、receipt淘汰・session上限、
古い準備結果／別Scene拒否、不正groupのIO前拒否、全suffix切断・末尾レコード全byte破損、
書込段階ごとのエラーと同じownerの再試行、子プロセス終了とwriter解放、no-op圧縮失敗、
同期回数、workerの併合・無同期再送・不正な隣接requestの分離を検証した。

- 🟢 [Done] ProtocolRequest Commit group、worker自動batch、正常prefix復旧・再起動後の重複排除。
- 🟢 [Done] fsync呼出し検証、before／after 2組の実測・ソース保存。
- 🟢 [Done] Tauri async／認可接続（[仕様](tauri-ipc.md)）。
- ⭕️ [Pending] 実電源断、容量不足、Windows／Linux durability、Tauri実UIでの再送。

English: Protocol Commit requests share one journal sync while retaining individual edit/receipt,
revision and Undo boundaries. Replay-only groups perform no I/O. Restart recovers a valid prefix
and retries apply only missing requests. Undo/Redo are barriers; explicit groups reject invalid
inputs before I/O. The native benchmark improves 32-request groups by about 30–35×; worker
queue latency and physical power-loss durability are outside this measurement.

简体中文：协议 Commit 请求共用一次日志同步，同时保留各自的编辑／结果、版本及撤销边界。
仅含重发的分组不执行 IO。重启恢复正常前缀，重发只应用缺失请求。Undo／Redo 是分组边界；
显式分组在 IO 前拒绝无效输入。原生基准中 32 请求分组约提升 30–35 倍，未计入 worker 队列延迟；
实际断电、其他操作系统及 Tauri UI 验证仍为 Pending。
