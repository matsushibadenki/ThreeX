> 2026-10-17：現行保存形式はcheckpoint／journal v6、content hash v2。旧v1–5を移行。Plane／Sphere分割数を保持。[仕様](../subdivisions.md)。

# Rust Renderer接続とnative surface lifecycle

🟢 [Done] native surfaceをRustのDocument workerへ接続する実装。
`threex-renderer::native` にwgpu backendと世代付きsurface管理、
`threex-tauri-host::surface` にTauri native Window接続・イベント受付を追加した。
実ウインドウを持つ製品UI、実GPU／OS固有の表示検証は別途⭕️ [Pending]。

## 所有と順序

JournalWorkerだけがAutosaveDocumentを所有する。workerのRender命令は、編集・保存と同じ有界queueへ入り、
group commitの境界になる。描画時にはその時点の `Document.scene()` を不変借用してRendererへ渡す。
全DocumentやSceneのsnapshot、全Instanceの比較・コピー、Rust↔JSのframe転送は追加していない。
既存Rendererのrevision-aware dirty rangesと借用Instance uploadをそのまま利用する。
ウインドウごとのRendererは独立したstamp、instance buffer、depth、surfaceを持ち、Documentを持たない。

NativeSurfacesはRustの共有registryで、最大16 target。SurfaceHandleはRustで与えたview IDと
単調増加generationから成る。attachは新候補のconfigureが成功してから旧targetを交換する。
失敗時は旧targetを保持し、交換後の古いresize／draw／detachはStaleで拒否する。
世代値は再利用せず、overflowは拒否する。resource handle全般の世代化を完了した意味ではない。
この情報は一時的な描画状態であり、Document checkpointやjournalには保存しない。

## Tauriホストからの接続

native WindowはWebViewパネルと分離して、Tauriイベントループ上で生成する。
準備中は非表示にし、接続とイベントcallbackの設定後に表示して初期resizeの取り逃しを避ける。
`attach_native_window(Arc<Window>, id).await` はそのWindowのラベルに接続する。
`attach_native_window_for("main", Arc<Window>, id).await` はRustで許可したmainパネルを
別native viewportのcontrollerにできる。controllerの指定やraw handleをIPC本文から受け付けない。

helperは `run_on_main_thread` で物理サイズを取得し、owned Arcからwgpu Surfaceを生成する。
Arcはsurfaceの寿命中Windowを保持する。adapter/deviceの非同期準備後、backendを登録する。
Rustの直接接続口 `attach_surface` は試験backendや既に準備したWgpuSurfaceにも利用できる。
Surface生成・候補準備・割当失敗でDocumentを変更しない。

実アプリのnativeイベントcallbackでは次の同期入口を使う：

```rust,ignore
// host is the managed IpcHost; controller and handle were retained by trusted Rust.
let accepted = host.try_surface_window_event(controller, handle, event)?;
if let Some(frame) = accepted {
    tauri::async_runtime::spawn(async move { frame.wait().await });
}
```

同期入口はイベント受信順にenqueueする。応答waitの開始順・完了順を入れ替えても受付順は変わらない。
`Resized` と `ScaleFactorChanged` はphysical pixelサイズへ、`Destroyed` はDetachへ変換する。
`CloseRequested` は閉鎖取消が可能なのでdetachしない。フォーカス喪失をSuspendとも解釈しない。
applicationの休止・再開はRustからSurfaceOp::Suspend／Resumeを明示的に送る。
`surface_window_event().await` と `surface_request().await` はasync呼出用の同等の入口。
イベントcallback自身をblock_onで待機させない。

QueueFull時は呼出側で最新サイズ／終了イベントを保持して再受付する。無視してイベントを捨てない。
受理済みQueuedFrameの応答を破棄しても処理は継続する。revoke_windowはtargetを破棄し、
shutdownは受理済み命令をdrainした後に全targetを破棄する。
native Window接続helperはイベントcallbackとon-demand schedulerを自動登録する。
低水準APIを使う独自ホストは明示配線できる。[現行scheduler仕様](native-redraw.md)。

## lifecycleと復旧

| 条件 | 動作 |
| --- | --- |
| 幅または高さ0 | configure／描画を行わずSuspended。最小化時もDocumentを保持 |
| 通常resize | configure成功後にサイズを公開。非対応サイズは旧サイズを保持 |
| Suspend中のresize | 最新サイズを保持、Resumeで再構成 |
| Lost／Outdated | 現サイズで再構成し1回だけ再取得。再発はReconfigureを返して次の明示frameへ |
| Timeout | Timeoutを返す。無限再試行・surface交換を行わない |
| suboptimal frame | present後に現設定で再構成 |
| Device Lost／OutOfMemory | targetを失敗状態で停止。明示的に新backendをprepare／attachして復旧 |
| Detach／Destroyed／revoke | 一致する世代のGPU資源とsurfaceを解放。古いイベントは新targetへ作用しない |

wgpu backendは互換adapterを選び、対応format／alpha mode／Fifoを使用する。
sRGB formatを優先し、render attachment能力、deviceのmax_texture_dimension_2d、ゼロサイズを検査する。
Device Lost callbackとuncaptured GPU errorを捕捉し、GPUエラーによる既定のpanicを避ける。deviceを新しくした場合、新Rendererのstamp／bufferから始まり
最初の描画は既存の全upload経路で再構築される。新deviceへの自動移行は未実装。
描画の停止は保存／編集／session receiptとは独立し、GPU障害でもDocumentは変更・破棄しない。

## IPC

既存document_ipcに小さいDraw要求を追加した：

```js
await invoke('document_ipc', { payload: {
  type: 'draw', document_id: hello.state.document_id,
  camera: [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1], locale: 'ja'
}});
```

cameraは有限f32のcolumn-major view-projection行列。surfaceは実callerのRust grantから解決する。
ReadOnlyでも自分に割り当てたtargetの描画は可能。別windowのtarget ID、保存先、surface生成情報を指定できない。
応答はFrameのstatusと小さいWorkerStateだけ。エラーは既存DTOのstable codeと3言語message。
Surface失敗は永続編集エラーと異なり、sequenceを消費せず、observed_revisionを持たない。
現在revisionはInspectで読む。native lifecycle命令も編集revision・dirty・fsync回数を変えない。

## 永続requestとの分離

描画命令のtransport IDもRustで単一採番する。再起動時にGPU状態を復元せず、targetは未接続から開始する。
Document ID・window session割当・ProtocolRequestの永続receiptは従来どおり復旧する。
旧requestを同じpayloadで再送すると元revision／結果を返し、Sceneへ再適用しない。
Surfaceの切断、再接続、GPU失敗、ウインドウ破棄はEndSessionではない。
保存形式はcheckpoint／journal v4、ipc-sessions v1のまま。

## 検証

🟢 [Done] deterministic backendによる5テスト：zero extent／Suspend／Resume／Detach、
候補失敗時の旧target保持と世代拒否、Lost／Outdatedの有界再試行とTimeout、
Device Lost／OOM停止と交換、target上限・不正サイズ・非有限camera。

🟢 [Done] Tauri実invoke経路の追加2テスト：
workerスレッド内で同じSceneを借用し複数targetが最新編集を描くこと、GPU障害後の編集・保存と
再起動再送の重複排除、再起動時target未接続、native resize／destroy世代認可と受付順、
surface操作がrevision／object数／fsyncを変更しないこと。
既存core／storage／IPC／JS回帰テストも実行する。

⭕️ [Pending] 実Wry WindowとMetal／Windows／Linux surfaceの表示・resize・DPI・device喪失、
GPU profile、実OSイベント配送・製品UI・ドッキング。テストbackendは実GPU／native表示の検証ではない。
WASM描画は既存経路を維持し、新native moduleはwasm32ではビルドしない。

English: The Rust worker borrows its Scene for ordered native rendering. Generation-tagged targets
support transactional replacement, resize, suspension and bounded surface recovery without changing
Document ownership or durable request deduplication. Real OS/GPU presentation remains pending.

简体中文：Rust worker借用自身Scene执行有序的原生绘制。带代际的表面支持安全替换、尺寸变化、
暂停及有界恢复，不改变Document所有权和持久请求去重。实际系统窗口与GPU显示验证仍待完成。

実行環境のGPU試験：`cargo test --offline -p threex-renderer -- --ignored` を再実行したが、
Metal adapterは `NotFound` で取得できなかった。実GPU描画を成功扱いにはしていない。
