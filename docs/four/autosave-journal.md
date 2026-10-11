> 後続で明示group commitを実装。[最新の同期回数・性能・復旧境界](group-commit-performance.md)を参照。

# 自動保存journalと未保存変更の復旧

2026-10-12 JST。`threex-storage::AutosaveDocument`がRust Documentとwriter lockを所有し、
通常編集・Undo・Redoを自動でjournalへ追記する。既存Document単体は従来どおりメモリ内。
Tauri／WASMのUIへの接続は未実装。Universal-3DのRust所有・dirty維持を参照し、
相手ソースを変更せず独立実装。symlinkへのビルド・実行・テスト依存は追加していない。

## 使用方法

```rust
use threex_storage::{AutosaveDocument, DocumentStore};
let mut owner = AutosaveDocument::open(DocumentStore::new("project/document-store"))?;
let mut create = [0u8; 64];
create[..4].copy_from_slice(&1u32.to_le_bytes());
create[4..8].copy_from_slice(&42u32.to_le_bytes());
owner.commit(owner.document().revision(), &create)?;
owner.save()?; // 世代保存後、journalをcheckpoint anchorへ圧縮
```

抜粋コードの`?`にはResultを返すホスト関数が必要。`document()`は読取専用。
ホストのRustコマンド受付をowner経由にすることで、保存ボタンを押す前の編集も記録される。
変更を試験用Documentで検証し、journal write＋sync成功後に本Documentへ適用して公開する。
失敗・no-op・競合・無効コマンドは本Documentを変えず、no-opはjournalも増やさない。
本Sceneは差し替えないためRendererのSceneStampと差分履歴を維持する。

## 形式とcommit境界

`autosave.txj`はmagic `THREEXJ1`、最初にv1 Document checkpointのframeを持つ。
各frameはpayload長u64 little-endian、payload、CRC32 u32。
トランザクションpayloadはopcode u8（1=commit、2=undo、3=redo）、base_revision u64、
revision u64、commitのみ既存64byteコマンド列。通常はrevision=base+1。
Undo/Redoの履歴はanchorのcheckpointから復元し、以後の命令列で更新する。
CRC32は偶発破損検出で、認証ではない。

journal上限256 MiB、単一編集payload上限256 KiB＋header、anchorは既存checkpoint上限128 MiB。
満杯はJournalLimitで編集前に拒否する。`save()`による圧縮で再受付できる。
writer lockはownerの生存中保持し、プロセス終了時にOSが解放する。同じ保存先の別writerはBusy。
owner外の通常`DocumentStore::save`はJournalActiveで拒否し、古いsnapshotによるjournalの無効化を防ぐ。

## 復旧と中断

`DocumentStore::load()`はjournalがあればanchorから命令を順に再生する。
長さ、CRC、連続revision、命令の検証に失敗した時点で停止し、最後の完全transactionまで採用する。
途中の壊れたrecordを飛ばして後続recordを適用しない。`journal_transactions`と
`discarded_journal_bytes`、`recovered`で結果を通知する。読取loadはファイルを変更しない。
writerの`open()`は破損末尾を切り詰めsyncし、正常な境界から追記を再開する。

anchor自体が破損した場合は正常な保存世代へ戻り、writer再開時に壊れたjournalを
`autosave.damaged`へコピーして新anchorを作る。正常な保存世代もない場合は明示エラー。
未知journal version／checkpoint majorは暗黙変換・上書きしない。

`save()`は先に既存の不変世代保存を完成し、その後`autosave.tmp`に新anchorをwrite/sync、
renameとdirectory syncでjournalを置き換える。切替前は旧anchor＋全transaction、
切替後は新anchorが同じDocument内容を持つ。破損したjournalより保存世代が新しければ
保存世代を採用し、新anchorへリセットする。

追記失敗時は本Documentを公開せず、同じownerのretryは最後に確認した境界からsuffixを置き換える。
全recordがwrite済だがsync／応答が完了する前に終了した場合、そのrecordが復旧される可能性がある。
したがって「エラー＝記録なし」とは限らない。再起動後は復旧revisionを読んでUIを同期する。
request_idによる再送重複排除は未実装。

## 検証結果

- 未保存create、Undo/Redo、保存と圧縮後の履歴継続。
- 最終recordの全切断位置と全byte破損から、直前transactionまで復旧。
- 正常末尾にゴミを追加した場合も、最後の正常transactionを保持し追記再開。
- 正しいCRCでもrevision欠落・無効命令は採用しない。
- appendの部分write、全write、sync直後でIOエラーを注入。本Document不変とretry重複なし。
- appendとcompactionの計6段階で子プロセスをexitし、Dropなしで復旧・writer再取得・追記。
- anchor破損時の世代fallback、未知version拒否、writer競合、無効編集・no-op。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
node --test tests/compat/scene.test.js
```

macOSでのfilesystemテスト。実電源断・実disk-full・Windows／Linux durabilityはPending。
全write後のprocess exit試験は、電源断後に非sync recordが残る保証ではない。

現実装は変更IDのみをprepareし、journal同期後に検証済み差分を適用する。全Document複製は廃止。
構造変更の全体階層走査と、各編集のfsyncは残る。詳細と前後計測は
[journal-staging-performance.md](journal-staging-performance.md)。worker接続は次工程。
全DocumentをframeごとにIPC送信することはない。保存先は引き続き1文書専用。

- 🟢 [Done] Rust自動journal、最後の正常transaction復旧、切断・破損・process exitテスト。
- 🟠 [Next] group commitとTauri worker／IPC接続、request再送重複排除。
- ⭕️ [Pending] 実電源断・容量不足・他OS・実UIの復旧確認。

English: The Rust AutosaveDocument owner writes commits, undo and redo to a bounded,
checksummed transaction journal before exposing changes. Recovery stops at the first
incomplete, corrupt or invalid record. Checkpoint compaction preserves a complete old
or new log. Tauri wiring, group commit and request deduplication
remain future work; actual power loss and cross-platform durability remain Pending.

简体中文：Rust AutosaveDocument 在公开修改前将提交、撤销和重做写入有界校验事务日志。
恢复在首个不完整、损坏或无效记录处停止，保留最后完整事务。检查点压缩保留完整旧日志或新日志。
Tauri 接线、group commit 与请求去重留待后续；实际断电及跨平台持久性仍为 Pending。
