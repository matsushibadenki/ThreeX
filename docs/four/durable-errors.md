# エラー結果の永続化

2026-10-15 JST。`AutosaveDocument::request`とworkerの`Protocol`要求で、受理した編集の確定エラーを
成功結果と同じsession連番／receipt管理へ保存する。syncとmetadata適用を終えてからエラーを返す。

## 受理境界と返却

正しいprotocol・Document ID・session・次の連番で、既存command上限内の要求が対象。
revision競合、InvalidCommand（不正な長さも含む）、履歴容量不足、NothingToUndo／Redo、revision枯渇の
編集結果を保存する。error textはUTF-8で最大1024byte、error codeはversion付きbinaryの固定tag。
容量超過payload／未知actionは受理前拒否。その他のbounded error codecにも同じ上限を適用する。

Document不一致、0／欠落連番、終了済み／未知session、session上限、未知protocol、過去キーの内容不一致、
Expired、queue満杯、IO失敗は受理前・実行基盤のエラーであり、この永続結果の対象外。
その後状態が変わっても同じ結果を返す保証は、**永続化して受理した成功／編集エラー**に対するもの。

```rust
Err(StorageError::Rejected(ProtocolFailure {
    error: DocumentError::NothingToUndo,
    revision: 0,       // 失敗を確定した時点のrevision
    replayed: false,   // 再送ではtrue
}))
```

受理したエラーはsessionをActiveにし、連番とrequest metadata世代を進める。
Scene、Document revision、Undo／Redo、意味内容hashは変更しない。
失敗したEndSessionも連番を消費するがClosedにはしない。
同じキー／同じ内容の再送には保存済みのerror code・詳細・当時のrevisionを返す。
文書状態が変わり、その操作が現在は成功可能でも実行しない。
訂正した編集／再終了には次の連番と更新したexpected revisionを使う。同キーで内容を変えるとMismatch。

workerは候補の不正を検出したら先行正常prefixを確定し、不正候補を単独のrequest経路で処理する。
失敗を検出しただけで未同期の結果を返さない。
`WorkerFailure`は元のcodeと`current_revision`を保持する。この値は**失敗当時の観測revision**であり、
現在の文書状態が必要なら新transport IDでInspectする。外側transport cacheのreplayedとnative failureのreplayedは別。
workerのsync counterは確定エラーの新規書込も数え、失敗結果の再送では増やさない。

裸Commit／coreの直接commitは従来契約のまま。
明示`request_group`は成功Commitを全体検証するnative helperで、検証失敗時はIO前に全体を拒否する。
このhelperの未受理batch検証エラーを個別request結果として永続化しない。
個別エラー結果の確定／再送には`request`または`WorkerAction::Protocol`を使う。

## 保存・復旧・移行

checkpoint major=4。v3 receiptのkind直後へerror length（u32）とerror bytesを追加。
length=0は成功。固定tagは1=RevisionConflict（u64観測revision）、2=CommandTooLarge、3=HistoryCapacity、
4=RevisionExhausted、5=InvalidCommand（残りがUTF-8）、6=NothingToUndo、7=NothingToRedo。
不明tag、不正UTF-8／長さ、矛盾する観測revision／Close状態は拒否する。
旧v1／v2／v3を読込可能で、旧receiptは成功結果として復元する。

journal magicはTHREEXJ4。opcode=5が失敗結果を記録し、共通58byte request headerの後へ
error length（u32）・error bytes・元のcommandを保存する。元のexpected revisionと失敗観測revisionを区別する。
元要求と失敗結果・watermarkを同じCRC frameで復旧する。
writerは旧journalを復旧し、ロック下でv4 anchorへ原子的に切替えてから追記する。
旧binaryはv4を拒否し、未知majorは上書きしない。ID sidecarと意味内容hash v1は変更しない。

復旧では保存済み結果を採用する。現在のvalidatorで新しい結果へ置換せず、成功として実行しない。
header・session順序・bounds・error code／revision整合は検証する。
各完了frameまでを復旧するため、破損した未完末尾の失敗は未受理のまま再試行できる。
完全書込後・sync直後の終了では、応答前でも保存済み失敗として復旧され得る。
IOエラーではlive Documentの連番を進めず、同じownerの再試行は未確認suffixを置換する。
保存圧縮の中断でもmetadata世代を比較し、Scene revisionが同じでも新しい失敗結果を持つcheckpointを優先する。

最近64receipt／command payload 8 MiB、Active 64・session記録4096の上限を維持。
失敗もreceipt保持枠を使う。error textの追加領域は最大64×1024byteで有界。
結果淘汰後もsession watermarkを残し、古い連番をExpiredで拒否する。終了済みsessionの過去失敗も同じ扱い。
同じScene revisionの複数prepared planでもActive枠を超えないよう、単独結果の適用直前にも容量を確認する。
全世代消失／rollbackで失われた記録まで永久監査する保証はない。

## 検証・進捗

追加11テスト（core5、storage6）：

- 失敗のreceipt化、Scene／履歴不変、当時のerrorとrevisionの再送、checkpoint roundtrip。
- error codec全tag・UTF-8・長さ、v3終了記録／成功receiptのv4移行。
- validatorが現在なら成功と判定する場合でも保存済み失敗を復元すること。
- stale planとmetadata-onlyの同時準備でActive上限を超えないこと、失敗した終了後の状態。
- 状態変更・再起動・Undo・session終了・receipt淘汰後にも古い編集を再実行しないこと。
- 不正長commandの保存、受理前の連番欠落／容量超過が連番・ファイルを変更しないこと。
- workerの先行正常prefix・失敗連番消費・次の訂正要求、再起動後の同じWorkerFailure。
- 失敗frameの全切断位置／全byte破損、部分／全書込／sync直後のprocess exit。
- 各書込段階のIOエラー・再試行、圧縮失敗後の追記、新しい保存済みmetadataへのfallback。

core48＋storage58＋WGSL1＋JS4＝111件成功。core／storageはdebug・release、Clippy all-targets/all-features・fmtを確認。
実GPU試験1件は環境制約でignore。物理電源断・他OS durabilityは未検証。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

- 🟢 [Done] 受理済み編集エラーの永続化、元の結果／revisionの再送、失敗連番消費。
- 🟢 [Done] checkpoint／journal v4・v1〜3移行、process exit／保存中断・重複排除テスト。
- 🟢 [Done] Tauri async／IPC認可・3言語エラーDTO／Rust Renderer接続（[仕様](native-renderer.md)）。
- ⭕️ [Pending] 実UI・物理電源断・容量不足・他OS durability。

English: Admitted editing errors are durable request outcomes, consuming the session sequence
without editing the Scene. Retries return the original error and observed revision, even after
restart or state changes. Corrected requests use the next sequence. Admission/IO errors and atomic
batch preflight rejection are not durable outcomes. Checkpoint/journal v4 migrate v1–3 and preserve
valid-prefix recovery; evicted results remain protected by watermarks.

简体中文：已受理的编辑错误作为持久化请求结果保存，消耗会话序号但不改变场景。
重启或状态变化后重发仍返回原错误及当时版本；修正请求使用下一序号。受理前／IO 错误及整批预检拒绝
不属于持久化结果。v4 检查点／日志兼容迁移 v1–3，保持正常前缀恢复；结果淘汰后水位仍阻止重执行。
