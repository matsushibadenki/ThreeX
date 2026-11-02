# G03-B2b3：background builder／生成結果cache

🟢 [Done] Rustの`HierarchyBuilder`に、専用CPU lane、入力／受付／cache上限、生成結果のdisk LRU、検証済みTHREEXA2出力とguard付き反映を実装。

🟠 [Next] view全体の階層訪問budget・詳細LOD履歴／cut cache。永続cache indexやクラッシュ後のcache清掃は後続作業。

## 使用

アプリの共有Rust側で`HierarchyBuilder::start(cache_root, BuilderConfig)`を一度作る。windowごとに作らず、WebViewへ大きな入力・出力を渡さない。`BuildInput`はimmutableな`Arc`に包んだsource AssetRef、position、triangle、追加locksを持つ。元のRust importerが提供する入力であり、builder自体がglTF等を読み込むわけではない。

Documentを直接所有するRust hostは`try_submit_for(document, entity, input, settings, destination)`を使う。受付時のasset job guardを保持し、`BuildTicket::try_recv`で結果をpollする。成功結果の`bind_command(document)`はそのguardを再検証し、現在のDocument revisionを期待値とするdurable commitに渡す64-byte命令だけを生成する。background threadがDocumentを編集することはない。guard不一致、Undo ABA、削除、参照変更では`Stale`を返す。同じsourceを意図的に再要求した場合、cache hitにも新しいjobのguardが付く。

Documentが既存の`JournalWorker`内にある場合、信頼されたRust hostは次の順に処理する。

1. `BeginAssetJob`でowner側ticketとsource referenceを取得する。
2. 純粋な生成用の`try_submit`を呼ぶ。このAPIの結果には直接bind用guardがない。
3. 結果の`source_reference()`で`FinishAssetJob`を照合する。生成後のreferenceを照合値に使わない。
4. acceptedのときだけ、その応答のrevisionを期待値にして、生成referenceへのbind命令を既存`ProtocolRequest`でcommitする。照合後に別編集があればrevision競合となり、誤反映しない。

この経路で再起動後の同じProtocolRequest再送も、既存の永続receiptで重複排除する。出力pathはhostのasset catalogが管理し、Documentには従来どおりAssetRefのみを保持する。

## 専用laneと制限

重いgeometry hash・cluster分割・親生成・cache検証・file I/Oは1本の`threex-lod-builder` threadで行う。既存の2本のasset loader threadとDocument owner threadは別。単一laneなので同時に同じ入力を受付けても、先行結果がcacheに残っていれば後続jobは再生成しない。別の生成が同じCPU laneを使用中でも、ownerは編集を続けられる。

`try_submit`／`try_submit_for`はO(1)のadmissionとArc共有だけを行い、geometryをscan・copyしない。defaultは以下の上限。

| 項目 | default |
| --- | --- |
| queued／実行中／未受信の完了ticket | 合計4 |
| 予約された入力payload | 合計8 MiB |
| source vertices／triangles | 各16,384 |
| hierarchy nodes | 4,096 |
| 全nodeの保存triangle数 | 131,072 |
| 候補縮約評価数 | 全体512（nodeごとも同上限） |
| cache | 8 entry／64 MiB |

settingsの上限値もconfig内であることを確認する。入力bytesはBox sliceの長さから計算し、callerが別に保持するsourceやallocator overheadを含むRSS値ではない。生成中のmetadata・対応表・geometryはcore builderの個数上限で制限する。完了結果を読まずに溜めてもticketと入力の予約枠を保持するため、受付が無制限に増えない。

`cancel`／ticket dropは取消flagを立てる。queued jobは開始前、生成中jobはcore builderから戻った後、公開copyは64 KiBごとと公開直前に確認する。現在のcore生成やfsyncを途中で強制停止はしない。`wait`のtimeoutは`Timeout`となりticketを取り消す。UI／ownerではblocking waitではなくpollを使う。`shutdown`は受付を閉じ、受理済みjobをdrainしてjoinするため、active core生成やOS I/Oを待つ場合がある。

## Cache keyと整合性

keyはSHA-256で、次をすべて含む。

- source AssetRefのID／revision／hash。
- vertex・triangle・lock配列の長さと実際のbits。
- clusterとparent生成の全設定・予算。
- `PARENT_BUILDER_VERSION`。生成意味論を変えるときはこのversionも更新する。

geometryが違うのに同じAssetRefを渡してしまっても、cache keyを混同しない。Document ID、guard、出力pathはpure生成結果のkeyに含めず、jobごとに扱う。入力が宣言されたsourceに対応することは信頼されたRust importerの責務。

cacheはbuilder session専用directoryのTHREEXA2ファイルで、再利用前にmetadataと全pageを有界readで検証する。破損ならcacheを無効化して再生成する。hitはcore生成を省略するが、検証・必要な出力copyのI/Oは残る。entry数またはbytesが足りなければLRUを退避する。cache書き込み／清掃のI/O障害では不明な残存fileの予約を維持して以後のcache書き込みを止め、`Io`を返す。builderを終了して再作成することで再開できる。容量超過や不正入力はDocumentや既存assetを変更しない。

cacheは再起動を跨ぐindexを持たず、正常終了時は自分が作ったsession directoryだけを除去する。クラッシュで残った旧directoryは起動時に自動採用・削除しない。cache quotaは実行中sessionを対象とし、クラッシュ残骸の回収は未実装。

## 公開済みassetの保持

出力はcacheとは別のdestinationへstream copyし、全pageを検証してfileをfsyncしてから、同じdestination directory内の一時ファイルをhard linkで公開し、directoryをfsyncする。出力先directoryは事前に用意する。copy方式なのでsource cacheとdestinationのfilesystemが異なっても扱えるが、公開時のfilesystem機能は必要。

既存destinationが同じ検証済みreferenceなら成功として再利用し、違う／破損している場合は`OutputConflict`として上書きしない。cache退避・builder終了後も公開assetは残る。公開済みassetはcache容量に含まず、アプリのproject storage側で管理する。

取消やI/Oエラーが公開直前・直後に競合すると、完成した出力が残ることはある。これをDocumentへ自動適用せず、owner guardとdurable commitで適用可否を決める。公開後のエラーを「fileは存在しない」と解釈しない。

## 検証

- 同一入力の連続受付で生成1回＋cache hitとなり、settings・geometry・source revisionを変えるとkeyが分離すること。
- 専用threadでの実行、満杯時のBusy、取消、入力bytes上限、cache容量上限、未受信ticketがあってもshutdownをdrainできること。
- LRU退避、破損cacheの再生成、builder終了後も公開assetが有効であること。
- 既存destinationを破壊せず、cache書込障害で以後の書込を止めること。core panicを結果へ変換し、次のjobを処理できること。
- 開始時guardによるUndo ABA拒否と、新しい要求のcache hitによる再適用。
- actor内Documentへの反映、保存・再起動後の参照復元と同じrequestの再送重複排除。
- 既存のcoverage・fallback・差分転送・gather cacheは変更せず、workspace回帰テストを維持すること。

検証結果：Rust workspaceはdebug／releaseとも221 passed（core 75、renderer 44、storage 84、Tauri host 18）。JS 13 passed、Clippy all-targets／all-features、format、diff checkも通過。

⭕️ [Pending] MacBook Airの実負荷・温度・生成時間、実電源断、OS別durability。GPU描画の既存Pending gateも継続する。cache命中で生成呼び出しを省いた検証を、実機性能測定とは扱わない。

Universal-3DとのRust所有境界を維持し、MuTracerのコード・shader・テストを流用せず独立実装した。両参照ディレクトリへの依存は追加していない。

English: A bounded dedicated CPU lane generates immutable THREEXA2 assets and reuses a verified session-local disk LRU keyed by actual source data, settings and builder version. Durable outputs survive cache eviction/shutdown. Admission guards reject stale binding, and existing actor/protocol commits retain replay deduplication. Persistent cache indexing and global view/cut optimization remain future work; hardware performance and durability tests are Pending.

简体中文：已实现有界专用CPU lane和经过验证的session磁盘LRU，key包括真实输入、生成设置和builder version。公开asset不会随cache退避或builder关闭而删除。生成开始时的guard拒绝过期绑定，现有actor／protocol保持再送去重。跨重启cache index及view／cut优化仍待实现，实机性能和持久性验证保持Pending。
