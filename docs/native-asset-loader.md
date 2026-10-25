# G02-B：native有界asset loaderとredraw

2026-10-25 JST。🟢 [Done] 受理したasset要求の非同期read・surface配送・対象windowへのon-demand redraw。
実GPU／OS window検証はPending。参照からpathを探すasset catalog、優先streaming、自動retryは未実装。

## 利用境界

trusted Rustのfile選択・asset resolverが、永続AssetRefをDocumentへcommitした後に呼ぶ。
WebViewが任意pathを渡せる新しいJSON IPC commandは追加していない。

```rust
let status = host.load_asset("main", surface_handle, entity_id, selected_path, true).await?;
// Queuedは受理／集約の通知。GPU表示完了のreceiptではない。
```

`IpcHost::load_asset`はwindow grantとsurface handleを検証し、有界の通常worker ingressへ入れる。
返す`AssetLoadStatus`はQueued／Busy／MissingReference／StaleTarget。
`attach_native_window`はschedulerへ接続済み。独自backendを`attach_surface`で入れる場合は、
`schedule_surface`でそのviewを一度登録してon-demand描画を有効にする。
Queuedに対してI/O待ちをせず、読み込み・結果検証・redrawはRust内部で進む。
Busyは未受理なので、利用側が空きを待って新しい要求として再試行する。
起動後のpath対応付けはnative利用側の責務で、Sceneに参照を付けるだけの自動path探索は行わない。

直接workerを利用する場合は`WorkerAction::LoadAsset { target, entity, path, detail }`を送る。
`WorkerOutcome::AssetLoad`のticketは一時IDで、checkpoint／receiptへ保存しない。
pathは最大8192 encoded bytes。通常の編集requestと同様に、満杯時は元の要求とQueueFullを返す。
同じtransport request IDの再送は既存cacheの応答を返し、I/Oを再実行しない。

## threadと所有

| 処理 | 場所 | 保持するデータ |
| --- | --- | --- |
| 要求受理・guard固定 | Document worker | entity、AssetRef、Scene identity／revision、surface世代 |
| manifest／payloadのseek/read・検証 | 専用2 loader thread | path、期待AssetRef、root/detail、単一pageの入力 |
| 結果鮮度・surface確認、upload | Document worker | 借用Document、validated page、target handle |
| page buffer／proxy／root／detail | surfaceのRenderer | GPU resourceとresidency。Documentを複製しない |
| redraw意図 | 既存native scheduler | target、dirty状態。page bytesを持たない |

通常の編集・保存・inspect・frame要求は、file readを待たずに処理できる。
GPU upload自体は従来のsurface registry内で実行し、disk I/Oをそのlockの中で行わない。
実GPUのupload所要時間を保証するものではない。

内部EnvelopeはClient／Asset completion／Wakeを区別する。
Clientとcompletionを同じ有界channelで順序付け、completionを挟んだ先へ編集groupをまたがせない。
これにより、先に受理した編集を適用してから古い結果を検証する。
callbackが混ざるとgroup commitのまとまりが小さくなる場合があるが、正常prefix・fsync後公開・再送保証は維持する。
completionはclientの連番・transport結果cacheを消費せず、page bytesを結果cacheやIPCへ複製しない。

## 上限とroot→detail

- I/O threadは2。worker単位でlogical要求は最大8件（ASSET_LOAD_CAPACITY）。
- slot予約を取ってからjobを投入し、queued／read中／完了待ちをまとめて8件に抑える。
- 1 page上限1MiBのため、入力payload予約の上限は8MiB。manifest・path・thread stack等は別の有界overhead。
- 同じtarget／entity／guard／path／detail指定のin-flight要求は集約し、同じticketを返す。
- 同entity／targetへの新しい要求は以前のjobを取消済みにする。ただしI/O完了回収までは古いslotを解放しない。
- 取消済みreadの入力がまだ存在するのに、その分の容量を新しいjobへ二重に割り当てない。
- root完了後、detail希望・descriptor存在・root readyの場合だけdetailを自動投入する。
- rootとdetailは同じleaseを使い、root入力の消費／drop後にdetailを開始する。段階番号も照合する。
- この版では1要求あたり最大2回のpage read。自動retryや無制限の先読みは行わない。

native laneはまず独立したstaging leaseでmanifest＋pageを読み、Document workerでRendererのpage admissionを行う。
このためresidentやGPU budget超過でもreadする場合がある。
G02-Aの直接APIのような「manifest→resident判定→必要payloadだけ読む」分割最適化は今後の改善対象。
GPU page poolのbyte／件数上限は別途維持し、staging上限をprocess全体のメモリ保証とは扱わない。
複数Document／windowの合計budget・物理退避待ちbytesはG04の対象。

## 完了・失敗・古い結果

完了時に、Document guardのidentity／revision・現在参照と、targetのid／generationを照合する。
無関係なDocument編集でも保守的に失効させるG01-Bの規則を維持する。
取消・Undo／Redo ABA・削除・surface detach／交換後の結果はGPU allocation前に破棄する。
別段階の遅延completionや終了済みticketは、現在のleaseを消費しない。

`SurfaceBackend::upload_asset`を追加し、WgpuSurfaceはG02-AのRenderer APIへ渡す。
既存custom backendはdefaultでUnavailableを返し、従来のdraw実装だけでもsource互換を維持する。
DeviceLost／OutOfMemoryはtargetのterminal failureへ記録し、Documentの編集・保存を破壊しない。

| 状況 | 表示・通知 |
| --- | --- |
| root ready | root表示へ。detail要求なら次段階を投入 |
| detail ready | detailへ置換 |
| detail破損・短いread・容量超過 | rootを保持 |
| manifestは正常だがrootが入らない／読めない | 登録できたbounds proxyへ |
| manifest自体が読めない／hash不一致 | 既存表示を保持。不正boundsを信用しない |
| stale target／Document／取消 | uploadもredrawも行わない |
| 同じresidentを再利用 | 不要な再upload・redrawを行わない |

backendは`AssetDelivery { changed, ready, failure }`を返す。
changedの場合だけ、registry lockを解放した後にtarget別wakerを呼ぶ。
Tauri schedulerは一致するsurface世代のviewだけをdirtyにし、他windowを再描画しない。
休止・zero-size viewはdirtyを保持してresume後に描画し、閉じた／交換済みviewへの古いwakeは無視する。
asset用wakerを登録していない利用側は従来のscene wakerへfallbackする。

`InspectAssetLoads`はpending／reserved_payload_bytes／queued／completed／failed／stale／capacity_rejectionsを返す。
queuedは受理したlogical要求数、completedはready結果数（root・detail・resident再利用を含む）。
failedはmanifest読込失敗・readyにならなかった配達・backend失敗、staleは取消・Document／surface失効を数える。
既存residentを再利用できた場合は、不要なpayload readに失敗してもready結果として数える場合がある。
統計は一時的であり、保存済みDocument revisionや内容hashを変更しない。

## 終了

closeはingressを止め、Wakeをnonblockingで送る。満杯なら既存eventでworkerが起きるため、Dropでqueue空きを待たない。
受理済みclient要求とloader leaseをdrainしてから、loader senderを閉じて専用threadをjoinする。
idle時の周期pollは追加せず、通常はchannelで休止する。
明示shutdownはjoin完了を待ち、Dropは受理済み処理を非同期でdrainする既存契約を維持する。
OSのread自体の強制取消や時間上限はまだ設けず、明示shutdownは受理済みOS I/Oの完了を待つ。
job／path対応付けは再起動後にnative利用側が再要求する。永続編集receiptの再送重複排除とは独立している。

## 検証

- 🟢 [Done] workspace debug／release 184件：core 59、renderer 36、storage 71、Tauri host 18。
- 🟢 [Done] JS 13件、全target／feature Clippy、fmt、diff whitespace検査。
- 🟢 [Done] I/Oをgateで停止したままcommit／Undoをackし、遅い入力がDocument workerを止めないこと。
- 🟢 [Done] 2 threadの同時実行、8 lease／8MiB上限、9件目Busy、重複集約、容量の回収。
- 🟢 [Done] 8件を停止した状態からshutdownし、ingress拒否・解除後drain・全read回収・thread終了。
- 🟢 [Done] root→detailの自動継続、detail破損後のroot維持、page容量0のproxy、missing fileでも編集revision不変。
- 🟢 [Done] Undo／Redo ABA、detach＋同idのreplacement後の結果拒否、古いtargetの受理前拒否。
- 🟢 [Done] uploadはthreex-document、readはthreex-asset threadで実行すること。
- 🟢 [Done] Tauri MockRuntimeでgrant／target検証、対象windowだけのredraw、別window／Document revisionへの非干渉、idle停止。
- 🟢 [Done] 既存の1 slot差分・gather cache、保存／再起動・再送・journal障害復旧回帰。
- ⭕️ [Pending] native GPU smokeはMetal adapter NotFound。実Wry／OS配送、実GPU upload・表示・readback・性能は未確認。
- 🟠 [Next] G03-A：CPU bounds／frustum判定とscreen-space errorに基づくLOD選択。
- 🔴 [Later] 階層cluster builder、resident判定先行read、asset catalog／path resolver、G04の優先queue・先読み・合計budget。

Universal-3Dのrenderer共有meshとmain.rsの終了・非同期処理境界を参照した。
ThreeXは単一Document workerの永続取引を維持し、readを専用laneへ分離した。リンク先へのbuild／実行時依存はない。

English: G02-B connects a two-thread/eight-lease native loader to the Document worker, surface generation guards
and target-specific redraw. Root automatically precedes optional detail; failed detail preserves root. Reads do
not block edits, and cancelled input retains its staging lease until reclaimed. Only small admission/status data
leaves the worker; payloads remain in Rust/GPU ownership. MockRuntime and CPU tests pass; real GPU/Wry validation is Pending.

简体中文：G02-B将2个专用thread／8个lease的native loader接入Document worker、surface世代校验及目标window的redraw。
先加载root再按需加载detail，detail失败时保留root。读取不阻塞编辑，取消输入在回收前仍占用staging额度。
对外只返回小型受理／状态数据，payload保持在Rust／GPU侧。CPU及MockRuntime测试通过，实际GPU／Wry验证仍为Pending。
