> 2026-10-17：現行保存形式はcheckpoint／journal v6、content hash v2。旧v1–5を移行。Plane／Sphere分割数を保持。[仕様](../subdivisions.md)。

# Nativeイベント配線とon-demand redraw

🟢 [Done] Tauri native windowのcallback登録、編集後の自動再描画、coalescing、backpressure保持と終了処理。
RustのDocument所有、永続ProtocolRequest、checkpoint／journal v4、ipc-sessions v1は維持する。

## 接続と所有

`attach_native_window_for(controller, Arc<Window>, id).await` と `attach_native_window` は、
surface準備後に `bind_native_window` を呼び、イベントcallbackとschedulerを自動登録する。
Window生成は従来どおりTauriイベントループ上で行い、準備中は非表示にする。
素のnative Windowを生成する実アプリはTauriの `wry`／`unstable` 機能を有効にする。
このcrateはgeneric Runtimeのまま、試験では `test`／`unstable` とMockRuntimeを使用する。

Rustの `bind_native_window(controller, handle, &window)` は既に接続したtargetにも利用できる。
backendの実サイズを初期値とし、callback登録の前後でWindowの最新physical sizeを読み、
GPU準備中のresizeを補完する。試験backendには `schedule_surface` で同じschedulerを接続できる。

callbackは弱参照のRedrawHandleだけを保持する。Document、Scene、AppHandleを所有せず、
ウインドウを閉じた後もRust workerの永続sessionを勝手に終了しない。
再接続は新しいgenerationに置き換え、旧callbackのイベントは新targetへ作用しない。
低水準の `try_surface_window_event` も互換APIとして残すが、自動配線済みのwindowには重ねて転送しない。

## イベントと操作

| 入力 | 処理 |
| --- | --- |
| Resized／ScaleFactorChanged | 最新physical sizeを保持し、必要なresizeと描画を予約 |
| Focused(true) | 再表示用に描画を予約 |
| Focused(false)／CloseRequested | 描画状態を変えない。閉鎖取消が可能な段階で破棄しない |
| Destroyed／RedrawHandle::close | Detachを優先し、queue満杯でも閉鎖意図を保持 |
| Rustのsuspend／resume | 休止中は描画を止め、再開時に最新サイズ・cameraで描画 |
| request_redraw／set_camera | 明示的に描画を予約。cameraは有限値の行列に限定 |
| 公開済みScene変更 | 登録済みtargetをdirtyにする |

Tauriの高水準WindowEventにないexpose通知やアプリのshow／hide／OS休止は、ホスト側の
該当操作からRedrawHandleのrequest_redraw／suspend／resumeへ接続する。
フォーカス喪失だけでは、表示中の別ウインドウを休止しない。

## Workerからの通知

JournalWorkerは初期SceneStampを記憶し、処理結果を公開するときに現在stampを比較する。
変更があれば小さいwake通知を出す。Sceneの複製、全Instance比較、IPC経由のframe送信は行わない。
commit／Undo／Redoはdurable publication後に通知する。group commitは同じ公開stampについて通知を集約する。
成功再送、no-op、永続編集エラー、Save、Inspect、描画処理ではSceneが変わらないため通知しない。
応答receiverを破棄しても、受理済み編集の永続化と通知はworker自身が完了する。

schedulerはRustの別スレッドで、小さいview intentだけを持つ。描画は従来どおりDocument workerで
Sceneを直接借用し、編集・保存と同じqueueの順序で処理する。schedulerへSceneを渡さない。
ウインドウごとのRendererの差分GPU uploadと独立stampを維持する。

## Coalescingと停止条件

- 最大16 target。それぞれ最新サイズ、camera、dirty、休止／閉鎖意図だけを保持する。イベントごとのqueueは作らない。
- 応答待ちは全targetを通じて1件。描画中に入った変更はdirtyとして残し、完了後に最新状態を描く。
- 閉鎖を優先し、通常のtargetはround robinで選ぶ。
- queue／pending上限で拒否された場合は8 ms後に再受付する。最新サイズと閉鎖意図は捨てない。
- Timeout／Reconfigureは1回のinvalidationについて最大3描画命令、16／32 msの間隔で再試行する。
  native backend側のLost／Outdated再取得は各命令で最大2回。無限再描画にしない。
- Device Lost／OOMなどのエラーはlast_errorへ記録し、交換まで対象の描画を止める。他targetと文書編集は継続する。
- ゼロサイズ／休止中はdirtyを保持して待機する。変更がないときはCondvarで待ち、idle timerや常時アニメーションを走らせない。

Rustの `RedrawHandle::status()` はbusy、実行可能なpending、presented、attempts、last_error、closedを返す。
attemptsにはqueueへの受付試行も含む。休止中のdirtyは実行可能なpendingには数えない。
Detach完了／世代交換後は旧handleのstatusはNoneになる。

shutdownは新規受付を止め、schedulerを停止・joinし、Document workerの受理済み命令をdrainして
surfaceを解放する。joinや応答待ちはasync executorをブロックしない経路へ分離する。
revokeはscheduler登録と一致するsurfaceを解除する。古いcallbackはhostやwriter lockを保持しない。

## IPCの互換範囲

既存Draw要求は同じdocument ID認可とcamera検証を使う。
schedulerへ登録されたtargetではcameraを保持して再描画を予約し、Frame status `scheduled` と小さい状態を返す。
これは予約の応答でありpresent成功を意味しない。未登録targetの低水準Drawは従来どおり同期的なFrame結果を返す。
UIが命令ごとにDocument／Sceneを持つ必要はなく、controllerとtargetの割当はRustだけが決定する。
codeはlocale非依存で、既存エラーDTOは英語・日本語・简体中文に翻訳する。

描画とイベントはProtocolRequestのsequenceを消費せず、文書revisionやfsync回数を変えない。
再起動時は描画状態を作り直す。Document ID／session IDと永続receiptは復旧し、同じ編集の再送を適用しない。
再送や失敗結果の再読込だけではSceneが変わらず、余分な自動再描画も発生しない。

## 検証

🟢 [Done] 追加6テスト：

- callbackへの1,000 resize入力の集約、queue満杯からの再開、idle停止、focus、ゼロサイズ、閉鎖意図の保持。
- commit後の自動描画、no-op／再送／永続エラーでの非描画、応答破棄後の編集通知、camera、保存・再起動後の成功／失敗再送。
- 描画中の100 camera更新と編集の保持、最新Sceneとcamera、workerスレッドでの同じScene借用。
- Timeoutの3回上限と再要求、別targetの進行、GPU失敗と世代交換、旧closeの拒否。
- Suspend／Resize／Resume、queue満杯中のshutdown、資源解放とwriter lock再取得。
- Tauri native Windowへの実callback登録コードをMockRuntimeで実行。

MockRuntimeはOSイベントcallbackを配送しないため、共通callback adapterには決定的な試験event sourceから
イベントを注入してscheduler・workerまで検証する。実invoke経路も使用する。

⭕️ [Pending] 実Wry／OSのイベント配送と表示、DPI／expose／hide-show、実GPU障害・ドッキングUI。
前回のMetal adapter試験はNotFound。模擬backendの結果を実機描画の成功とは扱わない。

English: Native callbacks record generation-checked view intent, while a sleeping on-demand scheduler
coalesces redraws and retains close/resize under backpressure. Durable worker publication wakes rendering;
replays and no-ops do not. Document ownership and restart request deduplication remain in Rust.

简体中文：原生窗口回调记录带代际验证的视图意图，按需调度器合并重绘并在队列繁忙时保留尺寸与关闭请求。
持久编辑发布后由Rust worker通知绘制，重复请求和无变化操作不触发重绘。保持Document所有权和重启后请求去重。
