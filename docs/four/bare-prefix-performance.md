> 現行のsession上限・終了・保存形式v3は[session lifecycle](session-lifecycle.md)を参照。以下の性能／導入記録は計測当時の条件。

# 裸Commit worker候補prefixの差分準備

2026-10-15 JST。workerの裸`WorkerAction::Commit`候補を追加するたびに全prefixをprepareし、
journal送信前にもう一度prepareしていた処理を差分化した。
ProtocolRequestと同じScene差分準備を`CommitGroupBuilder`として共用し、受理済みoverlay・
revision・Undo用差分・command予算を保持する。追加候補のみを検証し、準備結果を直接journalへ渡す。
32件の新規編集のprepare回数は560回から32回になる。

## API・再送・復旧

```rust
let mut builder = document.commit_group_builder(expected)?;
builder.push(expected, first)?;
builder.push(next_expected, second)?;
let prepared = builder.finish();
```

不正候補を拒否しても、受理済みprefixと件数／byte予算を変更しない。
最大32件／1 MiB、command上限、履歴上限、階層・revisionの検証を維持。
Document全体／既存Undo履歴を複製しない。no-opも返却結果の1件として保持するが、
revision・Undo境界・journalレコードは増やさない。
workerはtransport ID順序・既存cacheを確認してから候補を追加し、
Save／Undo／Redo／read／Protocol action・再送・不正候補を境界として処理する。

journalはopaqueな準備結果に保持されたcommand・expected／applied revisionから既存opcode=1を生成する。
別のcommand配列を再解釈せず、Scene identity／revisionを**書込前**と適用時に確認する。
新しい編集を含むgroupは1sync、no-opだけなら0sync。同期するまでDocumentへ編集を公開しない。
途中終了・破損末尾は最後の正常レコードまでを復旧する。group全体のdisk原子性は保証しない。
同じownerの書込失敗後の再試行は、未確認suffixを置換して追記する。

裸Commitの再送保証は従来と同じlive-worker cacheの範囲。
同じtransport ID／同内容は以前の結果、同ID／別内容はMismatch、evicted IDはExpired。
再起動でこのcacheは失われる。古いexpected revisionは競合で拒否するが、no-op等を含む一般的な
再起動後のdurable deduplicationにはDocument／session／sequenceを持つProtocolRequestを使う。
ProtocolRequestの永続重複排除・checkpoint／journal形式は変更していない。

## 性能比較

before→afterを同じbenchmarkで2組実測し、実装前ソースsnapshotも保存した。
aarch64 macOS 27.0.1、Rust 1.99.0、release／thin LTO。
100／10,000既存Object、1／8／32request、64byte／request。
`create_each`は各requestで別Object作成、`create_update`は最初に作成したObjectを後続で更新する。
4warmup＋24sample、表は2runのmedianの中央値。合算sampleの分布ではない。

候補CPU測定は旧workerのprefix 1..N＋最後のprepareと、新builderのN回push＋finishをcore APIで再現する。
1sampleは32回の準備を行い、1group当たりに換算。transport cache／IDチェック・IOを除く。
実worker内だけの計測instrumentationではない。
32request、候補準備CPU（µs／group）：

| 既存Object | workload | before µs | after µs | 比率 |
|---:|---|---:|---:|---:|
| 100 | create_each | 130.227 | 12.136 | 10.73× |
| 100 | create_update | 114.879 | 7.353 | 15.62× |
| 10,000 | create_each | 185.642 | 13.577 | 13.67× |
| 10,000 | create_update | 113.581 | 7.416 | 15.32× |

実JournalWorkerへの投入から全応答まで（µs／group、32request）：

| 既存Object | workload | before µs | after µs | 比率 |
|---:|---|---:|---:|---:|
| 100 | create_each | 5515.750 | 4387.562 | 1.26× |
| 100 | create_update | 4758.375 | 4681.438 | 1.02× |
| 10,000 | create_each | 5261.104 | 5815.959 | 0.90× |
| 10,000 | create_update | 5349.876 | 4568.104 | 1.17× |

実workerは2ms window、queue／batch上限32で測定。32件は件数上限でwindow満了を待たず処理できる。
queue／window、journal書込・sync・適用を含み、request生成・初期配置・checkpoint・起動／終了・復旧を除く。
同期回数は前後とも1回。候補CPUは約10.7〜15.6倍短縮した。
実workerの結果は改善caseと悪化caseが混在し、sync・スケジューリングの揺らぎが大きいため
総応答時間の一律改善は主張しない。2回目のafterにはCPU／tail latencyの大きい揺らぎもあり、生データに残した。
24sampleのp99は観測最大値。深い階層・大容量command・別filesystemはこの数値の対象外。

- [実装前ソース](../benchmarks/bare-prefix-before.zip)
- [before 1](../benchmarks/bare-prefix-before-1.csv) / [after 1](../benchmarks/bare-prefix-after-1.csv)
- [before 2](../benchmarks/bare-prefix-before-2.csv) / [after 2](../benchmarks/bare-prefix-after-2.csv)
- [比較](../benchmarks/bare-prefix-comparison.csv) / [測定条件](../benchmarks/bare-prefix-metadata.json)

snapshotを一時directoryへ展開しfeatureなしで実行すると旧workerの測定になる。
現行codeのfeatureは候補CPUモデルを選ぶもの。現行codeのfeatureなし実行でもruntime workerは新実装。

```sh
cargo run --offline --release -p threex-storage --example bare_prefix_bench
cargo run --offline --release -p threex-storage --example bare_prefix_bench --features bare_prefix
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

## テスト・進捗

追加3テスト（core1、storage2）：no-op／依存する親子編集／不正親削除後のprefix維持、
逐次実行とのcheckpoint・Undo一致、失敗候補によるbudget／件数消費がないこと、
書込各段階のエラーと同じownerの再試行、古い準備結果のIO前拒否、
workerのno-op・同ID再送・payload不一致・不正候補分離・再起動時の旧expected拒否を検証。
既存の全suffix切断・破損・group append／compactionのprocess exitと復旧、
ProtocolRequestの永続重複排除・正常prefix復旧テストも新しい経路で維持した。

core35＋storage43＋WGSL1＋JS4＝83件成功。core／storageはdebug・releaseで検証。
実GPUテスト1件は環境制約でignore。

- 🟢 [Done] 裸Commit候補prefix差分準備、prepared group直接journal送信。
- 🟢 [Done] live-worker再送重複排除／障害復旧、候補CPU・実worker前後2組の測定。
- 🟠 [Next] 永続エラー結果、Tauri async／Renderer接続。
- ⭕️ [Pending] 実電源断・容量不足・他OS durability・実UI／GPU統合検証。

English: Bare Commit candidates now retain validated scene/history deltas and journal their
prepared group directly. Failed candidates preserve the accepted prefix; cache replay, no-op,
Undo and valid-prefix recovery semantics remain unchanged. The 32-request CPU model improves
about 10.7–15.6×; actual worker latency has mixed results dominated by sync/scheduling.
Bare transport cache remains live-worker only; durable restart deduplication uses ProtocolRequest.

简体中文：裸 Commit 候选保留已验证的场景／历史差分，并直接写入已准备的分组。
无效候选不改变已接受前缀；缓存重发、no-op、撤销及正常前缀恢复语义保持不变。
32 请求的 CPU 模型约提升 10.7–15.6 倍；实际 worker 延迟受同步／调度影响，结果并不一致。
裸请求缓存仅在当前 worker 有效；跨重启持久化去重使用 ProtocolRequest。
