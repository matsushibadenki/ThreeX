> 現行はhash v3／保存形式v7。asset参照もcanonical入力に含む。[G01-B仕様](../assets.md)。

# 意味内容hash v3

2026-10-15 JST。`Document::content_hash()`と`DocumentSnapshot::content_hash()`を実装。
`ContentHash`は32byteのSHA-256で、Displayは小文字64桁hex。
永続化／交換する側は`CONTENT_HASH_VERSION = 3`と組にして扱う。

## 正規化仕様

SHA-256への入力は以下の順序。数値はlittle endian。checkpointのbyte列そのものはhashしない。

1. ASCII `THREEX-CONTENT`＋NUL。
2. hash schema version（u32＝3）。
3. Object数（u64）。
4. Object ID昇順に、まず従来の各60byte：ID（u32）、position／rotation／scale／colorの順の12個のf32、flags（u32）、parent ID（u32）。
5. 各Objectの直後にasset有無u8。参照ありならAssetId16 bytes、revision u64 LE、期待内容hash32 bytes。

flagsはbit0＝visible、bit1＝mesh、bits2–3＝geometry（Box=0／Plane=1／Sphere=2）。
bits4–12／13–21はcustom分割数。既定値は両fieldを0へ正規化し、bits22–31は0。
f32はIEEE754 bitsを使用し、+0／-0のみ同じ0bitへ正規化する。
Documentは既存の編集検証で非有限数を拒否する。公開snapshotを外部で直接変更した場合、このAPIは再検証しない。
角度の周期・異なる親子構造・同じ見た目の別Object IDを同一化しない。描画結果ではなく編集元の意味内容を識別する。
非表示のObjectの属性も含む。

Document ID・revision・Undo／Redo・履歴上限・request／session receipts・renderer／SoA slot順序を除く。
同じ現在内容なら、別の作成順序・編集履歴・文書IDでも同じhashになる。
Undoで以前の内容へ戻れば同じhashになり、Redoで再編集内容のhashへ戻る。
canonical fieldやencodingが変わる場合はhash schema versionを更新する。
v3はasset参照fieldと有無markerを追加したため、assetなしでもv2とdigestが異なる。

## 接続

- core：Document／snapshotで同じ正規化とstreaming計算を使う。全DocumentやObject配列を複製しない。
- 保存：`PreparedSave::capture`でcheckpointと同じ現在内容のhashを固定し、`SaveReceipt.content_hash`へ返す。
- worker：`WorkerAction::InspectContentHash` → `WorkerOutcome::ContentHash { hash, state }`。明示的なread境界として実行し、queue順序に従う。

workerの通常state／edit応答では毎回hashしない。計算はO(Object数)、追加の作業メモリは一定。
明示queryと保存captureは全Objectを走査する。差分Merkle hashやhash cacheは今回導入していない。
checkpoint／journal formatは現在v7（hash導入時はv2）で、hashは保存receiptのメモリ上の情報。読込後は内容から再計算する。

**hash一致を編集鮮度・再送・保存の判定へ置換しない。**
同じ内容にUndoしてもrevision／receipt metadataが変わり得るため、`SaveReceipt::is_current`、
prepared groupのScene identity／revision、request metadata世代と永続watermarkの既存guardを維持する。
このhashは認証・文書IDやcheckpoint CRCの代替ではない。
asset結果にはDocument identity／revisionとAssetRefによるguardを実装済み。汎用評価WorldのguardはLater。

## 検証

追加7テスト（core4、storage3）：

- Python hashlibで独立に算出した空／1Objectのgolden vectors。
- 作成順序・履歴・Document ID・no-op receiptを変えても同じhash。
- 全Object属性／IDの変更、signed zeroの正規化、snapshot／checkpoint roundtrip、Undo／Redo。
- 保存capture後の編集でもreceiptは保存対象のhashを返し、内容が元へ戻っても古いrevisionのreceiptをcurrentとしない。
- journal未保存変更・破損末尾・修復追記・compaction後の内容hash。
- worker照会のqueue順序、readで同期が増えないこと、再起動後の一致。

hash初回導入時の記録：core39＋storage46＋WGSL1＋JS4＝90件成功。最新の全体検証は[ROADMAP](../roadmap.md)を参照。
既存の障害復旧・再送重複排除・group commitテストを維持。実GPUテスト1件は環境制約でignore。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
npm test
```

- 🟢 [Done] 意味内容hash v3、snapshot／保存receipt／worker明示query、正規化・保存／復旧テスト。
- 🟢 [Done] Tauri async／IPC認可・Rust Renderer接続（[仕様](native-renderer.md)）。
- 🔴 [Later] 評価World分離と非同期評価結果guard、必要に応じた差分hash。
- ⭕️ [Pending] 実UI・WASM／実GPU・他OS durability検証。

English: Versioned SHA-256 identifies current authoring content in object-ID order, excluding
identity, revisions, history and receipts. Signed zero is canonicalized. Save receipts carry the
captured content hash; explicit worker queries are ordered read barriers. Existing revision and
request guards remain authoritative. Hashing is streaming O(objects), not incremental.

简体中文：版本化 SHA-256 按对象 ID 顺序标识当前编辑内容，排除文档身份、版本、历史和请求结果。
正负零统一编码。保存结果携带捕获内容的 hash；worker 显式查询遵循读取顺序。
保留原有版本及请求校验。计算为 O(对象数) 的流式扫描，并非增量 hash。
