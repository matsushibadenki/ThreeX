> 現行はcheckpoint／journal v4。受理済み編集エラーも永続化して連番を消費する。[最新のエラー結果仕様](durable-errors.md)。

> 2026-10-15：現行保存形式はcheckpoint／journal v3。EndSession・終了記録とActive枠再利用を実装。[最新のsession lifecycle](session-lifecycle.md)。以下のv2記述は導入時の形式も含む。

# 永続Document IDとrequest/session protocol

2026-10-14 JST。Document checkpoint v2、journal v2、native request protocol v1を実装。
再起動後の重複排除には新しいProtocolRequest経路を使う。既存の裸Commit／Undo／Redoと
WorkerRequest.idのcacheは互換のため残し、そちらに永続重複排除の保証は追加しない。
Tauri／JSON IPC接続は未実装。大容量frame／InstanceのUI転送は追加していない。

## 識別子と移行

`DocumentId`と`SessionId`は0以外の128bit識別子。表示／parseは32文字hex、表示は小文字。
native hostがOS乱数から生成する。coreはOSに依存せず、draft／旧形式のDocumentはIDなしで開始できる。
`bind_document_id`は最初の割当だけを許し、別IDへの変更は拒否する。

保存先の`document.id`はmagic／16byte ID／CRC32を保持し、writer lock下でtmp→sync→rename→directory sync。
checkpoint v2とjournal anchorにもIDを保存する。IDファイル・文書IDの不一致は拒否し、別文書を暗黙採用しない。
Plain Documentの保存結果は`SaveReceipt.document_id`を返す。native AutosaveDocumentは開く時点でIDを持つ。
`SaveReceipt::is_current`はSceneStampに加えてIDとrequest metadata世代も確認する。

checkpoint v1とjournal v1を読み込める。IDなし旧文書はnative側で割当・永続化し、journalをv2へ
原子的に切り替える。編集属性、Object ID、revision、Undo/Redoを維持し、移行だけで編集を再実行しない。
未知majorは上書きしない。旧binaryはv2を認識できず拒否するため、旧版との交互書込は対象外。
プロジェクトを丸ごとコピーするとIDとrequest履歴もコピーされる。別文書としてforkするAPIは未実装。

## Envelopeとhandshake

```text
ProtocolRequest {
  protocol_version: 1,
  document_id: DocumentId,
  session_id: SessionId,
  sequence: u64,
  action: Commit { expected_revision, commands }
        | Undo { expected_revision }
        | Redo { expected_revision }
}
ProtocolReceipt { revision, replayed }
```

`new_session_id()`でsessionを作り、クライアントは未応答requestと一緒にIDを保持する。
再送には元のdocument_id・session_id・sequence・actionをそのまま使う。
連番は各sessionで1から開始し、成功時に増やす。欠落連番、0、別Document、未対応protocolは拒否。
受理済みの編集エラーは連番を消費して結果を永続化する。受理前の拒否／IO失敗は対象外。訂正要求には次の連番を使う。
no-op成功はrevisionを増やさないが、連番とreceiptを永続化する。

worker起動時の`initial_state.document_id`で対象を確認し、`InspectSession { session_id }`で
永続化された最終連番（未使用はNone）を取得できる。Inspect／session queryには新しいtransport IDを使う。
将来のJSON IPCではIDをhex文字列、sequence／revisionを十進文字列にしてJS整数精度を避ける。
現時点のAPIはtyped Rustで、JSON schema／認可／Tauri invokeは後続。

workerへの投入：

```rust
WorkerRequest {
    id: transport_id,
    action: WorkerAction::Protocol(envelope),
}
```

外側のtransport IDは従来のlive-worker cacheの識別子。再起動後も意味を持つ編集のキーは
内側のdocument/session/sequenceである。transport IDを新しくしても同じenvelopeは再適用しない。
`WorkerOutcome::Protocol`は元の適用revision、永続replayed、現在の小さいstateを返す。
外側WorkerReply.replayedはtransport cacheの再送であり、内側replayedとは区別する。

## 永続化と重複排除

protocol編集はjournalのopcode=4 recordにexpected／applied revision、Document ID、Session ID、
sequence、kind、commandsをまとめ、同じCRC32で保護する。単独requestまたはCommit groupをsyncしてから編集とreceiptを公開。
復旧も同じrecordから編集とreceiptを一緒に採用するため、receiptだけ／編集だけを採用しない。
原子性はtransaction単位。complete write後・応答前の終了ではreceiptと編集がともに復旧され得る。

同じキーの再送は内容をbyte単位で比較し、元のrevisionを返す。Undo後に元のCommitを再送しても
再作成せず、元のreceiptを返す。同じ成功キーで違う内容はMismatch。
checkpoint v2へsession watermarksと最近のreceiptsを保存し、世代保存・journal圧縮・再起動でも維持する。
no-opのためDocument revisionが同じ場合でもrequest metadata世代を比較し、古いanchorより
新しいcheckpointのreceiptを採用する。古いmetadataを持つ保存要求も拒否する。

保持上限：Active 64session、Active＋Closed記録はDocument lifetimeで4096、最近64receipt、command payload合計8 MiB。
古いreceiptは落とすが、sessionの最終連番は落とさない。結果がなくてもsequenceが過去ならExpiredで
拒否し、再実行しない。期限切れrequestを新しいsessionへコピーして再送してはいけない。
新規sessionは上限で明示拒否し、既存sessionは継続可能。明示終了・Active枠再利用は[実装済み](session-lifecycle.md)。Document forkは後続。
各commandは256 KiB、Document／checkpoint／journalの既存容量上限も維持。

## 実装範囲と保証

成功したCommit／Undo／Redo（no-op含む）の永続receiptを実装。Save／read／受理前の拒否の
永続結果cacheは対象外。認証・認可としてsession IDを使うものではなく、CRC32も改ざん防止ではない。
Protocol Commitは`request_group(&[&request, ...])`とworker自動batchに対応し、最大32件／1 MiBを1回syncする。
各requestのreceipt／Undo境界を維持し、バッチ内再送も重複追加しない。Undo／Redoは単独requestの境界。
[同期回数・性能・prefix復旧](protocol-group-performance.md)に、この経路の実測と制約を記載。

正常に保持／復旧された履歴について同じrequestを再適用しない保証。破損や世代rollbackで失われた
transactionまで永久監査できるものではない。その場合も編集とreceiptは同じ復旧境界で失われる。
実電源断・disk-full・Windows／Linux durabilityはPending。新しい乱数ID生成はUnix経路のみ実装し、
非Unixは明示Unsupported。Tauri実UI、他プロセス／ネットワークとの認可・JSON接続は未検証。

## テストと実行例

追加テストはcore1件＋storage6件：v1移行とID固定、restart／save／compaction後の再送、Undo後の再送、
payload不一致・別文書・未知protocol拒否、no-op、receipt evictionとwatermark、連番欠落、session上限、
worker再起動とsession handshake、record全切断位置とprocess exit、no-op保存中断からのreceipt復旧。
子プロセスをpartial write／full write／sync直後でexitし、再送が編集を二重に適用しないことを確認。

最新のgroup検証を含めcore48件＋storage58件＋WGSL1件＋JS4件が成功。core／storageはdebugとreleaseで確認。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
cargo run --offline --release -p threex-storage --example request_demo -- /tmp/new-threex-request-document
```

実行例は新しいdirectoryで編集後にworkerを終了し、同じenvelopeを再送する。
検証時の出力：初回revision=1/replayed=false → 再起動後revision=1/objects=1/replayed=true。

- 🟢 [Done] 永続Document ID、checkpoint/journal v2移行、request/session v1、再起動後の重複排除。
- 🟢 [Done] Protocol Commit group、worker自動batch、正常prefix復旧・fsync／性能比較。
- 🟢 [Done] Tauri IPC認可／async接続（[仕様](tauri-ipc.md)）。
- ⭕️ [Pending] 実電源断・他OS・実UIの再送復旧。

English: Document identity, session watermarks and recent successful request receipts
persist in checkpoint/journal v2. A request's edit and receipt share one record, so
restart retries return the original revision without reapplying. Evicted results are
rejected using retained watermarks. Protocol Commit grouping and worker batching now share one sync and retain per-request receipts. Durable failures and Tauri IPC remain future work.

简体中文：文档身份、会话水位及最近成功请求结果持久化于 v2 检查点／日志。
编辑与结果共享同一记录，重启后重发返回原版本而不再次执行。结果淘汰后通过保留的水位拒绝重执行。
协议 Commit 分组与 worker 批处理共用一次同步，保留每个请求的结果。持久化失败结果及 Tauri IPC 留待后续。

workerの候補prefixは差分準備され、受理済みopaque planを直接journalへ渡す。
[2026-10-15の候補CPU・実worker比較と追加検証](worker-prefix-performance.md)。

🟢 [Done] Tauri asyncホスト・IPC認可・3言語エラーDTOと再起動再送検証：[Tauri IPC](tauri-ipc.md)。実Wry／OS別表示検証は⭕️ [Pending]。
