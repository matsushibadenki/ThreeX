# G01-B：永続asset参照とworker job境界

🟢 [Done] AssetId／AssetRefをDocumentに接続し、checkpoint／journal v7・内容hash v3・workerのjob検証を実装。
🟢 [Done] [G02-A](asset-pages.md)：最小page形式・検証付き読み込み・粗いLOD・固定容量pool・Renderer API。
🟢 [Done] [G02-B](native-asset-loader.md)：native host loader dispatch・対象別redraw。
この文書はG01-Bの制御面を記録する。geometry bytesの検証付き読込とRenderer実装はG02-Aで追加済み。実GPU検証はPending。

## 永続参照と64-byte命令

`AssetId`は非zeroの16byte ID。32桁hexのDisplay／parseを持ち、DocumentId・SessionIdとは別の型。
`threex_storage::new_asset_id()`は対応native環境の乱数源からIDを生成する。
非Unix環境の自動生成は既存のidentity生成と同様にUnsupportedで、利用側が有効IDを供給する経路もある。

`AssetRef::new(id, revision, hash)`はrevision > 0を要求する。hashは32byteの期待する内容digest。
この参照はmetadataであり、実asset bytesとの一致は将来のloaderが検証する。
グローバルなasset catalogや、同一ID／revisionに別hashを割り当てる操作の一意性管理はまだ導入していない。
現在の照合はID・revision・hashの3要素すべてで行う。

`Object.asset: Option<AssetRef>`をauthoring stateに持ち、SoA側では参照のあるslotだけを疎な表に格納する。
rendererの一時handle／pool epoch／job ticketはDocumentへ保存しない。

| bytes | bind opcode 4 | clear opcode 5 |
| --- | --- | --- |
| 0–3 | opcode = 4 | opcode = 5 |
| 4–7 | entity ID、u32 LE | entity ID、u32 LE |
| 8–23 | AssetId、16 bytes | 0 |
| 24–31 | asset revision、u64 LE | 0 |
| 32–63 | 内容hash、32 bytes | 0 |

clearのreserved bytesが非zeroなら拒否。entity不在・ID zero・revision zeroも取引全体を拒否する。
既存opcode 1／2／3とrecord長・他fieldの位置は変えない。opcode 2はasset参照を維持し、明示的なclearはopcode 5を使う。

```rust
use threex_core::{AssetRef, asset_command};
let id = threex_storage::new_asset_id()?;
let reference = AssetRef::new(id, 1, [7; 32]).unwrap();
let commands = asset_command(entity_id, Some(reference));
// 既存ProtocolAction::Commitのcommandsへ入れる。expected revision・sequenceを通常どおり付与する。
```

Asset参照は既存のprepare/apply・group prefix overlay・historyに含まれる。
Undo／Redo、削除後の復元、failed candidate後のprefix維持を共通の取引境界で扱う。
asset参照だけの変更はDocument revisionを進めるが、Transform／RGB／primitive specを変更しないため、
local／world再計算・Instance書込・GPU転送rangeを発生させない。
asset本体の描画はG02で別途接続し、それまでは既存のprimitive描画を維持する。

## 保存・hash・旧binary

- checkpoint v7、journal magic `THREEXJ7`を出力し、v1〜6を読み込む。
- checkpointは既存のObject／history／identity／receipt領域の後へasset表を追加する。
- 現在参照のあるentity数（u64 LE）と、entity ID昇順のbind recordを保存する。参照がない場合は表自体を省略する。
- 表を格納する場合のcountは正数。件数・長さ・昇順・重複なし・entity存在・参照内容を読み込み時に検証する。
- historyにもbind／clearを格納し、読込時にはUndo／Redoを検証して現在内容へ戻ることを確認する。
- v6以前のcheckpointは新asset命令を成功history／receiptとして受け付けない。
- 旧journalはwriter開始時にv7へ原子的にre-anchorしてからasset命令を追記する。
- v7を知らない旧binaryはformat versionエラーで拒否し、新命令を壊れた末尾と誤認して切り捨てない。

内容hashはschema v3。従来のObject canonical fieldsの後へasset有無のu8を追加し、
参照ありならID16 bytes・revision u64 LE・内容hash32 bytesを追加する。
新schemaはassetなしの内容でもv2と異なるdigestになる。外部のhash利用側は必ずversionと組にして扱う。
checkpoint／journalへasset本体を埋め込まない。内容hashは読込後に再計算する。
request protocol v1／ipc-sessions v1、永続Document ID、既存session watermark／receiptを維持する。
同じrequestの再送は参照を再編集せず、記録済みrevision／結果を返す。

## workerの開始・取消・完了

Rust native workerへ次の小さいaction／outcomeを追加した。

| action | outcome | 意味 |
| --- | --- | --- |
| `BeginAssetJob { id }` | `AssetJob { ticket, reference, state }` | 現在参照のguardを固定し、一時poolでpendingを予約 |
| `FinishAssetJob { ticket, reference }` | `AssetFinished { accepted, reference, state }` | guard・結果参照を検証し、1回だけ受理 |
| `ReadObject { id }` | 既存の`Object` outcome | 復元したasset参照もRust Objectから取得可能 |

pending上限はworkerごとに128。同じentityへの新しいBeginは前の予約を取消す。
Begin時は古いDocument世代の予約も回収し、古いjobが容量を占有し続けないようにする。
`ticket=None, reference=None`は参照なし／entity不在、`ticket=None, reference=Some`は容量不足。
失敗時に生成jobを開始しない。終了・再起動時のticketは永続化せず、新しいpool epochで拒否する。

Document側の`asset_job_guard`はDocument ID・Scene identity／revision・entity・AssetRefを保持する。
完了時には現在のDocumentとguard、結果側のID／revision／hashがすべて一致する必要がある。
保存だけではScene revisionを変えず、in-flight jobの結果を受け付けられる。
置換・clear・削除・Undo／Redo・再起動を跨いだ結果は、同じ内容へ戻っていても拒否する。
今回のguardは保守的で、無関係なentityの編集でも拒否する。entity／asset単位の変更世代による最適化はLater。

完了の試行でticketは消費し、不一致結果の後に同じticketで再試行しない。再処理は新しいBeginで予約する。
別のworker request IDで二重完了してもaccepted=falseとなる。
同じtransport request IDの再送は既存cacheの応答を再送し、publishを再実行しない。
この一時結果受理は編集request／session receiptの永続的な重複排除とは別の契約である。

workerは今の段階ではloader threadを起動せず、完了結果も参照metadataだけを扱う。
実page bufferを受け渡す所有経路はG02以降で接続する。WebViewへのgeometry bytes転送は追加していない。
Begin／FinishはRust内部actionであり、Tauriに公開する新しい未認可commandは追加していない。

## 検証

- 🟢 [Done] core 57、renderer 32、storage 65、Tauri host 17、計171件のworkspace debug／releaseテスト。
- 🟢 [Done] JS 13件、全target／feature Clippy、fmt、diff whitespace検査。
- 🟢 [Done] asset参照のcheckpoint復元・hash一致、Undo／Redo／削除復元、group prefixのatomic性。
- 🟢 [Done] 不正ID／revision／reserved clear／未知entity、全checkpoint切断位置、CRCが正しい不正表・過大countの拒否。
- 🟢 [Done] v6 journalのv7 re-anchor、保存／再起動、古いCommitのreceipt再送でrevision・参照が変わらないこと。
- 🟢 [Done] group journalの追加区間を全位置で切断し、最後の正常編集とreceiptを一緒に復旧。破損末尾の修復・追記・再保存。
- 🟢 [Done] workerの保存中job受理、置換・Undo／RedoのABA・削除・再起動・新Begin・二重完了・異なるrevision／hashの拒否。
- 🟢 [Done] 128 pending上限と、取消・stale回収による容量再利用。
- 🟢 [Done] 参照だけの変更でTransform／Instance転送0、既存gather／mesh budget回帰テスト。
- 🟢 [Done] Python hashlibでv3の空／1Object golden vectorsを独立検算。
- 🟢 [Done] G02-Aでasset本体の読込・hash検算を追加。
- ⭕️ [Pending] 粗いLODの実GPU表示、WASM／他OSの実機確認。

## 関連プロジェクトと次の段階

Universal-3Dの`mesh::Source.revision`とGpuSceneのmesh mapを参照した。
ThreeXでは表示用source文字列とは分け、強い型の永続AssetId・整数revision・期待hashを保存する。
リンク先の型・保存形式は直接利用せず、シンボリックリンクへのbuild／実行時依存は追加していない。

🟢 [Done] G02-Aのmanifest／root LOD／固定容量page poolとRenderer API。
🟢 [Done] G02-Bのnative host loader dispatch。
🟠 [Next] G03-AのCPU可視判定・LOD選択。
🔴 [Later] asset catalogの不変性管理、entity別job世代、asset本体のGC・履歴参照pin。

English: G01-B persists typed asset IDs, revisions and expected content hashes through Document transactions,
Undo/Redo, checkpoint/journal v7 and content hash v3. Existing protocol receipts deduplicate edits after restart.
Bounded worker tickets reject stale, foreign and duplicate completion metadata; any Document edit conservatively
invalidates a guard. Actual asset loading, payload hashing and GPU LOD rendering remain for G02.

简体中文：G01-B将强类型asset ID、revision及预期内容hash接入Document事务、Undo／Redo、checkpoint／journal v7及hash v3。
重启后复用原有receipt防止重复编辑。有界worker ticket拒绝过期、其他worker和重复完成结果；任意Document编辑都会保守地使guard失效。
实际asset读取、内容hash校验和GPU LOD绘制属于后续G02，未添加符号链接依赖。
