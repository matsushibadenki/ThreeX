> 現行はv4。受理済みの失敗も連番を消費する。[エラー結果の最新仕様](durable-errors.md)。以下にv3導入時のencodingも記録する。

# 永続session lifecycle

2026-10-15 JST。native request/session protocolへ`EndSession`を追加し、session終了を永続化した。
DocumentはRustが所有し、終了も単一workerの順序に従う。window破棄・通信切断だけで自動終了しない。
クライアントが未応答requestの処理結果を確認してから明示終了する。

## 状態とAPI

sessionは未使用→Active→Closedの一方向。最初に永続化された編集結果（成功または確定エラー）でActiveになる。
明示的なopen要求は不要。未知sessionの終了はSessionUnknownで拒否し、記録を作らない。
終了済みIDは再使用・再開しない。新しい接続は新SessionIdを生成する。

```rust
ProtocolRequest {
    protocol_version: 1,
    document_id,
    session_id,
    sequence: last_sequence + 1,
    action: ProtocolAction::EndSession { expected_revision },
}
```

typed Rust protocol v1にaction kind=4を追加。まだJSON IPCのwire schemaはない。
EndSessionは現在revisionを検証し、編集をしない通常のprepared transactionと同時に終了receiptを確定する。
成功でsession sequence／request metadata世代が増えるが、Document revision・意味内容hash・Undoは変えない。
Commit groupへ混在させず、workerの併合境界として処理する。

`Document::session_status`とworkerの`InspectSession`は
`None`／`Active { sequence }`／`Closed { sequence }`を返す。従来のsequence値も維持する。
照会には新しいtransport IDを使う。同IDのcache再送は以前の照会結果を返す契約のまま。
SessionClosed／SessionUnknownは安定error codeと英語・日本語・简体中文messageを持つ。

## 再送と容量

終了後もsession IDと最終連番を削除しない。

- 保持中の過去成功request：内容をbyte単位で確認し、元のrevisionをreplayed=trueで返す。
- 同じ成功キーで異なる内容：Mismatch。
- 結果が淘汰された過去連番：Expired。編集を再実行しない。
- 終了済みsessionの新しい連番：SessionClosed。Commit／Undo／Redo／再終了を受理しない。

終了要求自身の応答が失われたら、元のdocument/session/sequence/expected revisionをそのまま再送する。
保持中なら元のreceiptを返し、再同期しない。結果が期限切れならInspectSessionでClosedを確認する。
終了要求の受理済み編集エラーは連番を消費するがClosedにはしない。受理前の拒否／IO失敗は消費しない。古いprepared request／groupはwatermark／metadata世代で拒否する。
候補builderも終了済みsessionからの新規編集を拒否し、受理済みprefixを変更しない。

同時Activeは64、Active＋Closedの記録数はDocument lifetimeで4096。
終了はActive枠を解放するが、終了記録は消さない。記録上限では新IDをSessionLimitで明示拒否する。
現在のActive sessionは継続・終了できる。無制限なID生成／自動tombstone GCは提供しない。
receiptは従来どおり最近64件／command合計8 MiB。

## checkpoint／journal v3と移行

checkpoint major=3。v2の各session `(ID16, highest u64)`の末尾へstatus byte（0=Active、1=Closed）を追加。
v1／v2は読込可能で、v2 sessionはActiveとして復元する。
v3は総記録上限4096、Active数64、状態byte、close receiptのkind／連番／Closed状態・不変revisionを検証する。
close receiptが淘汰されてもClosed記録は独立に残る。

journal magicは`THREEXJ3`。既存opcode=4のrequestレコードにkind=4／空commandを記録し、
同じCRC境界で終了receipt・watermark・Closed状態を復旧する。終了レコードのsync後に状態を公開する。
v1／v2 journalを読めるが、writerはロック下で復旧内容をcheckpoint v3 anchorとして原子的に再配置してから追記する。
v2 journal上の新kind=4は不正な末尾として拒否する。v2 binaryはv3を拒否するため、旧writerとの交互書込は対象外。
未知majorを上書きしない。Document ID sidecarの形式と意味内容hash v1は変更していない。

部分書込／破損した終了末尾は最後の正常prefixまで復旧する。その場合はActiveの最終連番を保持し、
同じ終了要求を再試行できる。完全書込後・同期直後に終了すると、未応答でもClosedとして復旧され得る。
同じownerでの書込失敗後の再試行は未確認suffixを置換する。
保存／圧縮中断後もmetadata世代を比較し、revisionが同じでも新しい終了記録を持つcheckpointを優先する。
復旧可能な有効履歴に対する重複排除であり、全世代の消失・rollbackで失われた取引を永久監査する保証ではない。

## テスト・進捗

追加10テスト（core4、storage6）：

- 終了後の保持済み再送・期限切れ再送・新規連番・payload不一致・未知session終了。
- 古いprepared plan、拒否された候補後のprefix、hash／revision不変。
- 64 Active枠の解放・新規利用、4096記録上限、checkpoint後も終了IDを忘れないこと。
- v2 sessionの移行、v3 status／close metadataの不正値拒否、v2 journalからv3へのwriter移行。
- 保存／圧縮・Undo・receipt淘汰後の再送拒否、worker再起動後のClosed照会。
- 終了レコードの全切断位置／全byte破損、部分／全書込／sync直後の子プロセスexit。
- 各段階の書込エラー・同じownerの再試行、圧縮失敗後の追記・再起動。

core43＋storage52＋WGSL1＋JS4＝100件成功。core／storageはdebug・releaseで検証。
Clippy all-targets/all-featuresとfmtを実施。実GPU試験1件は環境制約でignore。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

- 🟢 [Done] sessionの明示終了・Active枠再利用・有界終了記録、v3永続化／旧形式移行。
- 🟢 [Done] 終了後・再起動後・receipt淘汰後の再送抑止と障害prefix復旧。
- 🟢 [Done] Tauri async／IPC認可・Rust Renderer接続（[仕様](native-renderer.md)）。
- 🔴 [Later] Document forkと明示的な運用archive方針。
- ⭕️ [Pending] 実電源断・容量不足・他OS durability、実UIとの切断／終了連携。

English: Sessions implicitly start on their first successful edit and explicitly end via EndSession.
Closing releases one of 64 active slots, but retains a durable ID/watermark tombstone (4096 total
records). Cached old requests replay their original result; evicted old requests expire and new
requests on closed IDs are rejected. Checkpoint/journal v3 retain closure across restart and
compaction; v1/v2 migrate without reapplying edits. Physical power loss and GUI integration remain pending.

简体中文：首次成功编辑隐式启动会话，EndSession 显式关闭会话。关闭释放 64 个活动名额之一，
但保留持久化 ID／水位记录（总计最多 4096）。旧请求有缓存则返回原结果，淘汰后拒绝重执行；
已关闭 ID 的新请求被拒绝。v3 检查点／日志在重启、压缩后保留关闭状态，兼容迁移 v1／v2。
实际断电及 UI 联动仍为 Pending。
