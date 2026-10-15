> EndSessionは併合境界。InspectSessionはActive／Closedと最終連番を返す。[最新lifecycle／v3仕様](session-lifecycle.md)。

> 2026-10-15：ProtocolRequestの候補prefix準備を差分化し、journalへ準備結果を直接渡す。[最新の性能・検証](worker-prefix-performance.md)。裸Commit候補も[差分化済み](bare-prefix-performance.md)。

> Protocol Commitの自動batchも実装。[group API・同期回数・復旧境界と実測](protocol-group-performance.md)。Undo／Redo、Save、read、裸actionは併合境界。
> 後続で永続Document IDとProtocolRequestを実装。[再起動後の再送保証](request-protocol.md)を参照。従来の裸actionとtransport IDはlive-session契約のまま。

# Rust Document workerとrequest重複排除

2026-10-14 JST。`threex-storage::JournalWorker`を追加し、これまでの差分編集・autosave journalを
独立したRust threadで実行する。アプリのRust hostがworkerを所有し、各ウインドウは
`WorkerClient`のcloneだけを保持する。Document正本とwriter lockはworker内のAutosaveDocumentが所有する。
これはnative headless hostサービス。Tauri command／UI／Renderer接続は未実装。
既存のWASM facadeも変更していない。

## APIとキュー

- `JournalWorker::start(store, config)`：初期復旧とworker起動、initial_stateを返す。
- `client.try_submit(WorkerRequest { id, action })`：キューへ投入しcompletion receiverを返す。
- action：Commit、Undo、Redo、Save、Inspect、ReadObject。
- reply：request ID、replayed、結果またはcode＋current_revision。
- 結果：適用revision、最新revision／object数／dirty／saved_revision、保存世代、要求した1Object。

デフォルトはqueue 32件、batch最大32件、収集window 2 ms、結果cache 64件。
設定はqueue 1〜64件、batch 1〜32件、window 0〜50 ms、cache 1〜256件に制限。
各command長256 KiB、group合計1 MiBは既存の制限を維持。queue満杯はQueueFullと元requestを返し、
状態を変更せず受け付けない。submitはdisk IOとqueue空きを待たないが、入口の短いmutex取得はある。
結果待ち・start・shutdownはblocking Rust APIなので、TauriのUI threadでは待たずhostのasync境界へ接続する。

実装コード例は`crates/storage/examples/worker_demo.rs`。

```sh
cargo run --offline --release -p threex-storage --example worker_demo -- /tmp/new-threex-worker-document
```

新しいディレクトリで実行する。例は8編集を投入し、revision・object数・同期group数のみを出力し、
保存してshutdownする。GPU frame・頂点・Instance全量・Document snapshotはメッセージへ載せない。
Rust Rendererへのsnapshot／共有契約は次工程。

## 順序とgroup commit

到着順で処理する。隣接Commitのexpected_revisionが先行編集のprospective revisionに一致し、
容量内なら同じgroupへまとめる。no-opはrevisionを増やさない。
Save／Undo／Redo／読取、重複request、revision不一致はgroupの境界。
独立requestの1件が無効でも先行する正常requestは公開し、後続を再検証する。
workerがrevision不一致を自動的に書き換えて編集することはない。

各groupが同期した後にcompletionを返す。group内のreplyの適用revisionは個別だが、
stateは公開済みgroup全体の最終状態。`successful_sync_groups`はこのworker起動後に成功した
journal編集sync group数（Undo／Redoを含む）で、checkpoint syncや失敗したsync試行、物理IO回数ではない。
workerの候補group組み立ては複数回prepare_groupを呼ぶため、追加の準備コストがある。
全Document複製は行わないが、この収集経路のCPU性能比較・incremental builder最適化は次工程。

## request IDと再送

request IDはこのworker session内で0以外のu64。新規requestを入口の到着順で単調増加させる。
複数ウインドウはRust hostで共通採番し、同じ操作の再送だけが同じIDと同じ内容を使用する。
新規の小さいIDや到着順を逆転させたIDは安全側にExpiredとして拒否する。
将来のJSON IPCではu64 ID／revisionを文字列にするが、そのprotocolは未実装。

結果cacheに同じID・同じactionがあれば元の結果を返す。途中の重複も先行request完了後にcacheから返し、
追加のrevision／Undo／journal transactionを作らない。同じIDで違う内容ならRequestMismatch。
保存・読取・エラー結果もcache対象。replayのstate／error revisionは元の値であり、最新状態ではない。
最新値は新しいIDのInspectで取得する。

cacheは件数で有界。cacheから消えたIDは再実行せずExpired。再同期して新しいrequestを作る。
結果cacheとID watermarkはメモリ内だけで、再起動をまたぐexactly-once保証ではない。
worker再起動後はDocument revisionを再取得し、前sessionのrequestを新しいworkerへ盲目に再送しない。
永続DocumentId／session protocolは[request-protocol.md](request-protocol.md)を参照。裸actionのtransport cacheは上記のlive-session契約。

## 障害・終了

journalエラー時もDocumentを変更せずcompletionでStorageFailure等を返す。
ただしwrite済・応答前の終了では未応答transactionが復旧され得るため、エラーを未記録と同一視しない。
worker再起動は既存の正常prefix復旧を利用し、discarded_journal_bytesをinitial_stateへ返す。
Saveは既存世代保存＋journal圧縮の境界で、後続編集はdirtyのまま。

completion receiverを破棄しても、既に受け付けた編集をキャンセルしない。
`shutdown()`は全clientの入口を閉じ、受け付け済みqueueを処理してthreadをjoinする。
保持されたclientも以後Stoppedとなる。Dropは入口を閉じ、プロセスが生きている間は非同期に処理を続ける。
アプリ終了前は明示shutdownを完了させる必要がある。ウインドウを閉じるだけではアプリ所有workerを停止しない。

## 検証

7つのworkerテストを追加：2clientの編集／save境界、8編集の1sync、pending duplicate、no-op、
request内容不一致、revision競合、期限切れ、queue飽和と元request返却、上限拒否、shutdown drain、
無効編集の後の正常編集、writer lock競合、IO failureの結果cache、破損末尾からのworker再起動。
英語・日本語・简体中文の既定error messageと英語fallbackも確認。
既存journalの中断・破損・process exit／圧縮試験は維持。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
node --test tests/compat/scene.test.js
```

- 🟢 [Done] Rust worker、bounded queue、時間／件数batching、session内request重複排除・既定3言語error。
- 🟠 [Next] Tauri async command／Renderer接続。
- ⭕️ [Pending] native surface／実UI、実電源断・他OS durability。

English: A native Rust worker owns the document and journal; window clients submit
bounded requests and receive small results. Adjacent compatible edits group behind
one sync. Request results are cached only within the live session; expired IDs are
rejected, and restart requires resynchronization. Tauri and renderer wiring remain future work.

简体中文：原生 Rust worker 拥有文档与日志，窗口客户端提交有界请求并接收小型结果。
兼容的相邻编辑在一次同步后发布。请求结果仅在当前会话缓存，过期 ID 被拒绝，
重启后须重新同步。Tauri 与渲染器接线留待后续。

裸Commitも`CommitGroupBuilder`の受理済み差分を直接journalへ渡す。
[2026-10-15のCPU／実worker性能と復旧検証](bare-prefix-performance.md)。
