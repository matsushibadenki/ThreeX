> 現行のsession上限・終了・保存形式v3は[session lifecycle](session-lifecycle.md)を参照。以下の性能／導入記録は計測当時の条件。

> 後続で裸Commit候補も差分化。[最新の裸Commit性能・復旧検証](bare-prefix-performance.md)。この文書はProtocolRequest候補の測定を記録する。

# worker ProtocolRequest候補prefixの差分準備

2026-10-15 JST。workerが候補を追加するたびに累積prefix全件をprepareし、さらにjournal送信前に
全件prepareしていた経路を置き換えた。`RequestGroupBuilder`がsession watermarks・最近のreceipt参照・
変更ID overlay・Undo用差分・個別revisionを保持し、新しい候補だけを検証する。
32件の新規requestではScene編集の準備が560回から32回になる。再送はreceipt照合だけでScene準備を行わない。

## 所有と失敗境界

```rust
let mut builder = document.request_group_builder()?;
builder.push(&first)?;
// 不正な候補を拒否しても、受理済みprefixは保持される。
builder.push(&second)?;
let prepared = builder.finish();
```

Documentはprepare中に変更しない。既存Document・Undo履歴・receipt payloadを複製しない。
新規編集・receiptのみを保持し、virtual receipt淘汰も逐次実行と同じ意味にする。
候補のrevision不一致、階層cycle／orphan、payload不一致、容量／session上限を検証する。
受理済みoverlay・連番・receipt・件数／byte予算を、不正なpushで消費しない。

workerは準備結果をそのままjournalへ渡す。journalは準備結果が所有する新規requestからレコードを生成し、
別のenvelope配列を再解釈しない。Scene identity／revision・Document ID・request metadata世代を
**書込前**と適用時に確認し、no-op receiptで古くなった準備結果もIO前に拒否する。
各編集＋receiptのopcode=4 CRCレコード、checkpoint／journal v2、1group 1sync、Undo境界を維持。
途中の正常prefixを復旧し、再送は復旧済みrequestを飛ばす。group全体のdisk原子性は保証しない。

最大32envelope／1 MiB、64session／64receipt／8 MiBの既存制限を維持。
transport cache／ID順序、Undo／Redo・保存・read・裸actionの境界も維持する。
明示`request_group`も同じbuilderを使う。不正な明示groupは全体を拒否するが、workerは先行正常prefixを確定して
不正候補を別処理する。裸Commit workerはこの測定時点の対象外だった。現在は[差分化済み](bare-prefix-performance.md)。

## 性能

同じbenchmarkと実装前ソースsnapshotでbefore→afterを2組測定。
aarch64 macOS 27.0.1、Rust 1.99.0、release／thin LTO。初期100／10,000 Objectに1／8／32requestを追加。
`create_each`は各requestで別Objectを作成、`create_update`は最初に作成したObjectを後続requestで更新する。
4warmup＋24sample。表は2runのmedianの中央値であり、sampleの合算ではない。

候補CPU測定はcore APIでworkerの旧候補選択アルゴリズム（prefix 1..N＋最後の再prepare）と
新builder（N回push＋finish）を再現する。1sampleで32回繰り返し1group当たりへ換算。
Scene／protocol差分準備・allocationを含み、workerのtransport cache／version変換・queue・IOは含まない。
実際のworker内だけを計測するinstrumentationではない。request生成・初期配置・checkpoint・復旧は計測外。

32request、候補準備CPU（µs／group）：

| 既存Object | workload | before µs | after µs | 比率 |
|---:|---|---:|---:|---:|
| 100 | create_each | 156.133 | 20.029 | 7.80× |
| 100 | create_update | 107.650 | 11.214 | 9.60× |
| 10,000 | create_each | 170.711 | 11.035 | 15.47× |
| 10,000 | create_update | 109.974 | 6.162 | 17.85× |

実際のJournalWorkerへの投入から全応答まで（µs／group、32request）：

| 既存Object | workload | before µs | after µs | 比率 |
|---:|---|---:|---:|---:|
| 100 | create_each | 5153.500 | 4811.354 | 1.07× |
| 100 | create_update | 4985.688 | 4374.271 | 1.14× |
| 10,000 | create_each | 4089.645 | 4340.916 | 0.94× |
| 10,000 | create_update | 3981.375 | 4671.271 | 0.85× |

worker測定は2ms window、有界queueとbatchを32に設定し、queue／window・書込・sync・適用を含む。
32件は件数上限到達でwindowの満了を待たず処理できる。同期回数は前後とも1回。
候補CPUは約7.8〜17.9倍短縮した。worker全体はsync／スケジューリングの揺らぎが支配し、
今回の測定には遅くなったcaseもあるため、総応答時間の一律改善は主張しない。
24sampleでp99は観測最大値。1／8request、p95／p99を含む全caseは生CSVを参照。
深い階層の変更経路検証やbyte比較のコスト自体は残る。大きいcommandや他filesystemへの外挿はしない。

- [実装前ソース](../benchmarks/worker-prefix-before.zip)
- [before 1](../benchmarks/worker-prefix-before-1.csv) / [after 1](../benchmarks/worker-prefix-after-1.csv)
- [before 2](../benchmarks/worker-prefix-before-2.csv) / [after 2](../benchmarks/worker-prefix-after-2.csv)
- [比較](../benchmarks/worker-prefix-comparison.csv) / [測定条件](../benchmarks/worker-prefix-metadata.json)

snapshotを一時directoryへ展開しfeatureなしで実行すると旧worker／旧CPU候補経路になる。
現行codeのfeatureはbenchmarkのCPU経路を選ぶもの。runtimeのworkerは常に新実装なので、
現行codeのfeatureなし実行を旧worker全体の測定として扱わない。

```sh
cargo run --offline --release -p threex-storage --example worker_prefix_bench
cargo run --offline --release -p threex-storage --example worker_prefix_bench --features worker_prefix
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

## 検証・進捗

追加4テスト（core2、storage2）：拒否された候補の後に正常候補を追加し、階層・revision・receipt・
Undo履歴が逐次実行と一致すること、拒否によるbudget／件数消費がないこと、古い準備結果のIO前拒否、
workerのバッチ内再送と再起動後の無同期再送を検証。既存の親子テストもCreateが属性を読まない仕様に合わせ、
Create＋Updateで実際に親子依存を作るfixtureへ修正した。
既存の全末尾切断位置・破損・各書込段階のprocess exit／再試行・no-op圧縮中断・fsync数のテストも維持。

core34＋storage41＋WGSL1＋JS4＝80件成功。core／storageはdebug・releaseで検証。
実GPUテスト1件は環境制約でignoreのまま。

- 🟢 [Done] worker ProtocolRequest候補prefixの差分検証・準備結果直接journal送信。
- 🟢 [Done] restart重複排除・障害prefix復旧、before／after CPUと実worker応答比較。
- 🟢 [Done] Tauri async／Rust Renderer接続・native surface lifecycle（[仕様](native-renderer.md)）。
- ⭕️ [Pending] 実電源断・容量不足・他OS durability・実UI／GPUの統合検証。

English: The worker retains validated scene/session/receipt deltas and validates only each added
Protocol Commit candidate. It journals the retained preparation without a final prefix rebuild.
Rejected candidates leave the accepted prefix intact; restart deduplication and valid-prefix
recovery remain unchanged. The 32-request CPU model improves about 7.8–17.9×; actual worker
latency remains dominated by sync/scheduling and does not consistently improve in these runs.

简体中文：worker 保留已验证的场景／会话／请求结果差分，仅验证新增的协议 Commit 候选，
直接写入已准备的请求，不再重建整个前缀。无效候选不改变已接受前缀；重启去重及正常前缀恢复保持不变。
32 请求的 CPU 模型约提升 7.8–17.9 倍，实际 worker 延迟受同步／调度影响，本次测量未显示一致改善。
